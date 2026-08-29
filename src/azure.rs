//! Azure DevOps work-item sync: authenticate with the Azure CLI, pull the
//! project's work items over REST, and map them onto the local ticket model.

use std::cell::RefCell;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use url::Url;

use crate::classification::{self, ClassificationNode};
use crate::model::{
    Identity, RelationKind, RelationRecord, StateCategory, StateOption, Ticket, TicketKey,
};
use crate::timestamp::Timestamp;

/// Azure DevOps resource id accepted by `az account get-access-token`.
const ADO_RESOURCE: &str = "499b84ac-1321-427f-aa17-267ca6975798";
const API_VERSION: &str = "7.1";
/// Largest id batch the work items endpoint accepts.
const BATCH_SIZE: usize = 200;
const BODY_LIMIT: u64 = 64 * 1024 * 1024;
/// Profiles live on the identity host, not on `dev.azure.com/{org}`.
const PROFILE_URL: &str =
    "https://app.vssps.visualstudio.com/_apis/profile/profiles/me?api-version=7.1";
/// Every work item in the project, which both pulls narrow in their own way.
const PROJECT_IDS_WIQL: &str =
    "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AzureConfig {
    /// Organization slug, e.g. `jacobragsdale` for `https://dev.azure.com/jacobragsdale`.
    pub organization: String,
    pub project: String,
}

impl AzureConfig {
    /// Resolve organization and project from explicit values, then the
    /// `TICKET_TUI_ORG` / `TICKET_TUI_PROJECT` environment, then the
    /// `az devops configure` defaults file.
    pub fn resolve(organization: Option<String>, project: Option<String>) -> Result<Self> {
        let defaults = az_devops_defaults();
        let organization = organization
            .or_else(|| std::env::var("TICKET_TUI_ORG").ok())
            .or_else(|| defaults.0.clone())
            .context(
                "no Azure DevOps organization; pass --org, set TICKET_TUI_ORG, or run `az devops configure --defaults organization=...`",
            )?;
        let project = project
            .or_else(|| std::env::var("TICKET_TUI_PROJECT").ok())
            .or_else(|| defaults.1.clone())
            .context(
                "no Azure DevOps project; pass --project, set TICKET_TUI_PROJECT, or run `az devops configure --defaults project=...`",
            )?;
        Ok(Self {
            organization: organization_slug(&organization),
            project,
        })
    }

    #[must_use]
    pub fn base_url(&self) -> String {
        format!("https://dev.azure.com/{}", self.organization)
    }

    #[must_use]
    pub fn work_item_url(&self, id: i64) -> String {
        format!("{}/{}/_workitems/edit/{id}", self.base_url(), self.project)
    }
}

/// Accept `https://dev.azure.com/slug`, `https://slug.visualstudio.com`, or a bare slug.
fn organization_slug(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix("https://dev.azure.com/") {
        return rest.split('/').next().unwrap_or(rest).to_owned();
    }
    if let Some(slug) = trimmed
        .strip_prefix("https://")
        .and_then(|rest| rest.strip_suffix(".visualstudio.com"))
    {
        return slug.to_owned();
    }
    trimmed.to_owned()
}

fn az_devops_defaults() -> (Option<String>, Option<String>) {
    let Some(path) = az_config_path() else {
        return (None, None);
    };
    let Ok(raw) = fs::read_to_string(path) else {
        return (None, None);
    };
    let mut organization = None;
    let mut project = None;
    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "organization" => organization = Some(value.trim().to_owned()),
            "project" => project = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    (organization, project)
}

fn az_config_path() -> Option<PathBuf> {
    let config_dir = std::env::var_os("AZURE_CONFIG_DIR").map_or_else(
        || std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".azure")),
        |dir| Some(PathBuf::from(dir)),
    )?;
    Some(config_dir.join("azuredevops").join("config"))
}

pub struct AzureClient {
    agent: ureq::Agent,
    config: AzureConfig,
    /// Refreshed in place when Azure DevOps rejects it: CLI access tokens
    /// expire in about an hour and the TUI outlives that.
    authorization: RefCell<String>,
}

/// Everything one pull produces.
#[derive(Debug, Default)]
pub struct SyncBatch {
    pub tickets: Vec<Ticket>,
    pub relations: Vec<RelationRecord>,
}

impl AzureClient {
    pub fn connect(config: AzureConfig) -> Result<Self> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(90)))
            .build()
            .into();
        Ok(Self {
            agent,
            config,
            authorization: RefCell::new(authorization_header()?),
        })
    }

    #[must_use]
    pub fn config(&self) -> &AzureConfig {
        &self.config
    }

    /// Pull every work item in the configured project.
    pub fn fetch_all_work_items(&self) -> Result<SyncBatch> {
        self.fetch_work_items(&self.query_ids()?)
    }

    /// Pull only the work items edited at or after `watermark`. The comparison
    /// is inclusive, so the work item the watermark came from is read once
    /// more; that costs one row and is what keeps two edits made in the same
    /// second from hiding behind each other.
    pub fn fetch_changed_work_items(&self, watermark: Timestamp) -> Result<SyncBatch> {
        let wiql = format!(
            "{PROJECT_IDS_WIQL} AND [System.ChangedDate] >= '{}' ORDER BY [System.Id]",
            watermark.to_iso8601_utc()
        );
        self.fetch_work_items(&self.query_work_item_ids(&wiql)?)
    }

    /// Every work item id the project still has. A pull compares this against
    /// the ids it already holds, because a deleted work item is not reported as
    /// changed — it simply stops being listed.
    pub fn query_ids(&self) -> Result<Vec<i64>> {
        self.query_work_item_ids(&format!("{PROJECT_IDS_WIQL} ORDER BY [System.Id]"))
    }

    /// Read the named work items, relations and all, in batches the endpoint
    /// accepts. An empty id list makes no request at all.
    fn fetch_work_items(&self, ids: &[i64]) -> Result<SyncBatch> {
        let mut batch = SyncBatch::default();
        for chunk in ids.chunks(BATCH_SIZE) {
            let joined = chunk
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let url = format!(
                "{}/_apis/wit/workitems?ids={joined}&$expand=relations&api-version={API_VERSION}",
                self.config.base_url()
            );
            let response = self.get(&url)?;
            let items = response
                .get("value")
                .and_then(Value::as_array)
                .context("work item batch response has no value array")?;
            for item in items {
                let (ticket, relations) = parse_work_item(item, &self.config)?;
                batch.tickets.push(ticket);
                batch.relations.extend(relations);
            }
        }
        Ok(batch)
    }

    /// Write one work item's fields with a JSON Patch document, returning Azure
    /// DevOps's own copy of what it stored. The document decides whether the
    /// write is safe: [`crate::edit::EditRequest`] leads with a revision test,
    /// so a work item that moved on is refused rather than overwritten.
    pub fn update_work_item(
        &self,
        id: i64,
        patch: &[Value],
    ) -> Result<(Ticket, Vec<RelationRecord>)> {
        // Without `$expand=relations` the answer carries no links at all, and
        // the row's relations would be replaced with nothing.
        let url = format!(
            "{}/_apis/wit/workitems/{id}?$expand=relations&api-version={API_VERSION}",
            self.config.base_url()
        );
        let item = self.send(&url, Request::Patch(patch))?;
        parse_work_item(&item, &self.config)
    }

    /// The states one work item type allows, in the order the process template
    /// lists them, which is the order the state picker offers. A state carries
    /// the category Azure DevOps assigned it rather than one guessed from its
    /// name, so a custom state is still coloured correctly.
    pub fn fetch_work_item_type_states(&self, work_item_type: &str) -> Result<Vec<StateOption>> {
        let url = self.work_item_type_states_url(work_item_type)?;
        let response = self.get(&url)?;
        let states = response
            .get("value")
            .and_then(Value::as_array)
            .with_context(|| format!("{work_item_type} states response has no value array"))?;
        Ok(states
            .iter()
            .filter_map(|state| {
                let name = state.get("name").and_then(Value::as_str)?.trim();
                if name.is_empty() {
                    return None;
                }
                let category = state
                    .get("category")
                    .and_then(Value::as_str)
                    .map_or(StateCategory::of(name), StateCategory::parse);
                Some(StateOption::new(name, category))
            })
            .collect())
    }

    /// Everybody on the project's teams, so the assignee picker can offer
    /// somebody who has no work item in the database yet. Azure DevOps lists the
    /// teams first and the members of each separately, so this is one request
    /// per team plus one. Somebody on two teams is listed once.
    pub fn fetch_team_members(&self) -> Result<Vec<Identity>> {
        let teams = self.get(&self.teams_url(&[])?)?;
        let teams = teams
            .get("value")
            .and_then(Value::as_array)
            .context("teams response has no value array")?;
        let mut found: Vec<Identity> = Vec::new();
        for team in teams {
            let Some(id) = team.get("id").and_then(Value::as_str) else {
                continue;
            };
            let members = self.get(&self.teams_url(&[id, "members"])?)?;
            collect_team_members(&members, &mut found);
        }
        Ok(found)
    }

    /// Both classification trees — areas and iterations — in one request, deep
    /// enough for any hierarchy a project is likely to have. The field path a
    /// work item carries is not the `path` each node reports, so it is rebuilt
    /// from the names on the way down; see [`crate::classification`].
    pub fn fetch_classification_nodes(&self) -> Result<Vec<ClassificationNode>> {
        let response = self.get(&self.classification_nodes_url()?)?;
        Ok(classification::parse_classification_nodes(&response))
    }

    /// A project name is a path segment and may have spaces in it, so this URL
    /// is assembled rather than formatted, like the teams one below.
    fn classification_nodes_url(&self) -> Result<String> {
        let mut url = Url::parse(&self.config.base_url())
            .with_context(|| format!("invalid Azure DevOps URL {}", self.config.base_url()))?;
        url.path_segments_mut()
            .map_err(|()| anyhow!("Azure DevOps URL cannot carry a path"))?
            .extend([
                self.config.project.as_str(),
                "_apis",
                "wit",
                "classificationnodes",
            ]);
        url.set_query(Some(&format!("$depth=10&api-version={API_VERSION}")));
        Ok(url.into())
    }

    /// A project name is a path segment and may have spaces in it, so the URL is
    /// assembled rather than formatted. `tail` is whatever follows `teams`.
    fn teams_url(&self, tail: &[&str]) -> Result<String> {
        let mut url = Url::parse(&self.config.base_url())
            .with_context(|| format!("invalid Azure DevOps URL {}", self.config.base_url()))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|()| anyhow!("Azure DevOps URL cannot carry a path"))?;
            segments.extend(["_apis", "projects", self.config.project.as_str(), "teams"]);
            segments.extend(tail);
        }
        url.set_query(Some(&format!("api-version={API_VERSION}")));
        Ok(url.into())
    }

    /// A work item type is a path segment and its name has spaces in it — `User
    /// Story`, `Product Backlog Item` — so the URL is assembled rather than
    /// formatted.
    fn work_item_type_states_url(&self, work_item_type: &str) -> Result<String> {
        let mut url = Url::parse(&self.config.base_url())
            .with_context(|| format!("invalid Azure DevOps URL {}", self.config.base_url()))?;
        url.path_segments_mut()
            .map_err(|()| anyhow!("Azure DevOps URL cannot carry a path"))?
            .extend([
                self.config.project.as_str(),
                "_apis",
                "wit",
                "workitemtypes",
                work_item_type,
                "states",
            ]);
        url.set_query(Some(&format!("api-version={API_VERSION}")));
        Ok(url.into())
    }

    /// Display name of the signed-in user, used to mark their own work items.
    /// The profile host is separate from the work-item host and may be blocked
    /// or unavailable, so a failure yields `None` rather than sinking the sync.
    pub fn current_user_display_name(&self) -> Result<Option<String>> {
        Ok(self
            .get(PROFILE_URL)
            .ok()
            .as_ref()
            .and_then(profile_display_name))
    }

    fn query_work_item_ids(&self, wiql: &str) -> Result<Vec<i64>> {
        let url = format!(
            "{}/{}/_apis/wit/wiql?api-version={API_VERSION}",
            self.config.base_url(),
            self.config.project
        );
        let response = self.post(&url, &json!({ "query": wiql }))?;
        response
            .get("workItems")
            .and_then(Value::as_array)
            .context("WIQL response has no workItems array")?
            .iter()
            .map(|item| {
                item.get("id")
                    .and_then(Value::as_i64)
                    .context("WIQL result without an id")
            })
            .collect()
    }

    fn get(&self, url: &str) -> Result<Value> {
        self.send(url, Request::Get)
    }

    fn post(&self, url: &str, body: &Value) -> Result<Value> {
        self.send(url, Request::Post(body))
    }

    /// One request, retried once with a freshly minted token when Azure DevOps
    /// rejects the current one, because an access token expires long before a
    /// running TUI does. A failed refresh reports the original rejection, which
    /// carries the advice to sign in again.
    fn send(&self, url: &str, request: Request<'_>) -> Result<Value> {
        match self.attempt(url, request) {
            Err(error) if rejected_credentials(&error) => match authorization_header() {
                Ok(refreshed) => {
                    *self.authorization.borrow_mut() = refreshed;
                    self.attempt(url, request)
                }
                Err(_) => Err(error),
            },
            result => result,
        }
    }

    fn attempt(&self, url: &str, request: Request<'_>) -> Result<Value> {
        let authorization = self.authorization.borrow().clone();
        let response = match request {
            Request::Get => authorized(self.agent.get(url), &authorization)
                .call()
                .with_context(|| format!("GET {url} failed"))?,
            Request::Post(body) => authorized(self.agent.post(url), &authorization)
                .send_json(body)
                .with_context(|| format!("POST {url} failed"))?,
            Request::Patch(patch) => authorized(self.agent.patch(url), &authorization)
                // Azure DevOps refuses a patch document sent as plain JSON.
                .header("Content-Type", "application/json-patch+json")
                .send_json(patch)
                .with_context(|| format!("PATCH {url} failed"))?,
        };
        read_json(response, url)
    }
}

/// One request the client makes, and the body that goes with it.
#[derive(Clone, Copy)]
enum Request<'a> {
    Get,
    Post(&'a Value),
    /// A JSON Patch document, which Azure DevOps takes only under its own
    /// media type.
    Patch(&'a [Value]),
}

/// The headers every Azure DevOps request carries, whatever its method.
fn authorized<Body>(
    builder: ureq::RequestBuilder<Body>,
    authorization: &str,
) -> ureq::RequestBuilder<Body> {
    builder
        .header("Authorization", authorization)
        .header("X-VSS-ForceMsaPassThrough", "true")
        .header("Accept", "application/json")
}

/// Azure DevOps refused the credentials. Carried as its own error type so a
/// request can tell an expired token apart from every other failure.
#[derive(Debug)]
struct RejectedCredentials(String);

impl fmt::Display for RejectedCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RejectedCredentials {}

fn rejected_credentials(error: &anyhow::Error) -> bool {
    error.downcast_ref::<RejectedCredentials>().is_some()
}

/// Azure DevOps refused a request and said why. Carried as its own error type
/// so a write can tell a work item that moved on apart from every other
/// failure; the display text is the same either way.
#[derive(Debug)]
pub struct RequestRejected {
    status: u16,
    url: String,
    message: String,
}

impl RequestRejected {
    #[must_use]
    pub fn new(status: u16, url: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            url: url.into(),
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Whether the refusal means the work item changed after it was read: an
    /// explicit conflict status, or the `test` operation on `/rev` failing,
    /// which Azure DevOps reports as an ordinary 4xx that talks about the
    /// revision or the test.
    #[must_use]
    pub fn is_conflict(&self) -> bool {
        if matches!(self.status, 409 | 412) {
            return true;
        }
        if !(400..500).contains(&self.status) {
            return false;
        }
        let message = self.message.to_ascii_lowercase();
        [
            "/rev",
            "revision",
            "test operation",
            "test op",
            "'test'",
            "\"test\"",
            "concurren",
            "changed by another",
            "modified by another",
        ]
        .iter()
        .any(|needle| message.contains(needle))
    }
}

impl fmt::Display for RequestRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Azure DevOps returned HTTP {} for {}: {}",
            self.status, self.url, self.message
        )
    }
}

impl std::error::Error for RequestRejected {}

/// Whether a failed write means the work item moved on under us, which a fresh
/// pull fixes. Anything Azure DevOps did not refuse outright — a dead network,
/// an unreadable body — is not a conflict.
#[must_use]
pub fn is_write_conflict(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RequestRejected>()
        .is_some_and(RequestRejected::is_conflict)
}

/// Pull `displayName` out of a `/_apis/profile/profiles/me` document.
fn profile_display_name(profile: &Value) -> Option<String> {
    profile
        .get("displayName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

fn read_json(mut response: ureq::http::Response<ureq::Body>, url: &str) -> Result<Value> {
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .with_config()
        .limit(BODY_LIMIT)
        .read_to_string()
        .with_context(|| format!("failed to read the response body from {url}"))?;
    if !(200..300).contains(&status) {
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| text.chars().take(200).collect());
        if status == 401 || status == 302 {
            return Err(anyhow::Error::new(RejectedCredentials(format!(
                "Azure DevOps rejected the credentials ({status}); run `az login` and retry: {message}"
            ))));
        }
        return Err(anyhow::Error::new(RequestRejected::new(
            status, url, message,
        )));
    }
    serde_json::from_str(&text)
        .with_context(|| format!("Azure DevOps returned invalid JSON from {url}"))
}

/// Prefer a personal access token from `AZURE_DEVOPS_EXT_PAT`, otherwise
/// borrow the Azure CLI's login.
fn authorization_header() -> Result<String> {
    if let Ok(pat) = std::env::var("AZURE_DEVOPS_EXT_PAT")
        && !pat.trim().is_empty()
    {
        return Ok(format!(
            "Basic {}",
            base64(format!(":{}", pat.trim()).as_bytes())
        ));
    }
    let output = Command::new("az")
        .args([
            "account",
            "get-access-token",
            "--resource",
            ADO_RESOURCE,
            "--query",
            "accessToken",
            "-o",
            "tsv",
        ])
        .output()
        .context("failed to run `az`; install the Azure CLI or set AZURE_DEVOPS_EXT_PAT")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "`az account get-access-token` failed; run `az login` or set AZURE_DEVOPS_EXT_PAT: {}",
            stderr.trim()
        );
    }
    let token = String::from_utf8(output.stdout)
        .context("az returned a non-UTF-8 token")?
        .trim()
        .to_owned();
    if token.is_empty() {
        bail!("`az account get-access-token` returned an empty token; run `az login`");
    }
    Ok(format!("Bearer {token}"))
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut buffer = [0_u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let bits = u32::from(buffer[0]) << 16 | u32::from(buffer[1]) << 8 | u32::from(buffer[2]);
        for index in 0..4 {
            if index <= chunk.len() {
                let value = (bits >> (18 - 6 * index)) & 0x3f;
                output.push(char::from(TABLE[value as usize]));
            } else {
                output.push('=');
            }
        }
    }
    output
}

/// Map one `/_apis/wit/workitems` entry onto a ticket and its relations.
pub fn parse_work_item(
    item: &Value,
    config: &AzureConfig,
) -> Result<(Ticket, Vec<RelationRecord>)> {
    let id = item
        .get("id")
        .and_then(Value::as_i64)
        .context("work item without an id")?;
    let fields = item
        .get("fields")
        .and_then(Value::as_object)
        .with_context(|| format!("work item {id} has no fields"))?;
    let text = |name: &str| {
        fields
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let timestamp = |name: &str| -> Result<Timestamp> {
        let raw = text(name).with_context(|| format!("work item {id} is missing {name}"))?;
        Timestamp::parse(&raw).with_context(|| format!("work item {id} has an invalid {name}"))
    };
    let key = TicketKey {
        organization: config.organization.clone(),
        id,
    };
    let ticket = Ticket {
        key: key.clone(),
        project: text("System.TeamProject").unwrap_or_else(|| config.project.clone()),
        revision: item.get("rev").and_then(Value::as_i64).unwrap_or(1),
        work_item_type: text("System.WorkItemType").unwrap_or_else(|| "Work Item".into()),
        title: text("System.Title").unwrap_or_else(|| format!("Work item {id}")),
        state: text("System.State").unwrap_or_else(|| "New".into()),
        reason: text("System.Reason"),
        assigned_to: fields.get("System.AssignedTo").and_then(identity_name),
        priority: fields
            .get("Microsoft.VSTS.Common.Priority")
            .and_then(Value::as_i64),
        area_path: text("System.AreaPath").unwrap_or_default(),
        iteration_path: text("System.IterationPath").unwrap_or_default(),
        tags: text("System.Tags")
            .map(|tags| {
                tags.split(';')
                    .map(str::trim)
                    .filter(|tag| !tag.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        description: text("System.Description")
            .map(|html| html_to_text(&html))
            .unwrap_or_default(),
        created_at: timestamp("System.CreatedDate")?,
        changed_at: timestamp("System.ChangedDate")?,
        web_url: config.work_item_url(id),
    };
    let relations = item
        .get("relations")
        .and_then(Value::as_array)
        .map(|relations| {
            relations
                .iter()
                .filter_map(|relation| {
                    let kind = relation_kind(relation.get("rel")?.as_str()?)?;
                    let target = relation.get("url")?.as_str()?;
                    let to_id = target.rsplit('/').next()?.parse::<i64>().ok()?;
                    Some(RelationRecord {
                        from: key.clone(),
                        to: TicketKey {
                            organization: config.organization.clone(),
                            id: to_id,
                        },
                        kind,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((ticket, relations))
}

fn identity_name(value: &Value) -> Option<String> {
    match value {
        Value::String(name) => Some(name.clone()),
        Value::Object(identity) => identity
            .get("displayName")
            .or_else(|| identity.get("uniqueName"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

/// Adds one team's members to `found`, skipping anybody already listed: a
/// project's teams overlap, and the picker offers each person once. Each member
/// nests the person under `identity`; a member without a display name is no use
/// to a picker, so it is dropped.
fn collect_team_members(members: &Value, found: &mut Vec<Identity>) {
    let Some(list) = members.get("value").and_then(Value::as_array) else {
        return;
    };
    for member in list {
        let identity = member.get("identity").unwrap_or(member);
        let Some(display_name) = trimmed(identity, "displayName") else {
            continue;
        };
        if found
            .iter()
            .any(|known| known.display_name.eq_ignore_ascii_case(&display_name))
        {
            continue;
        }
        found.push(Identity::new(display_name, trimmed(identity, "uniqueName")));
    }
}

/// One string field of an identity, with the surrounding space gone and an
/// empty value read as absent.
fn trimmed(identity: &Value, field: &str) -> Option<String> {
    identity
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn relation_kind(rel: &str) -> Option<RelationKind> {
    Some(match rel {
        "System.LinkTypes.Hierarchy-Reverse" => RelationKind::Parent,
        "System.LinkTypes.Hierarchy-Forward" => RelationKind::Child,
        "System.LinkTypes.Related" => RelationKind::Related,
        "System.LinkTypes.Dependency-Forward" => RelationKind::Successor,
        "System.LinkTypes.Dependency-Reverse" => RelationKind::Predecessor,
        "System.LinkTypes.Duplicate-Forward" | "System.LinkTypes.Duplicate-Reverse" => {
            RelationKind::Duplicate
        }
        _ => return None,
    })
}

/// Reduce Azure DevOps rich text to readable terminal text. Block elements
/// become line breaks, list items get a bullet, every other tag is dropped,
/// and the common entities are decoded.
#[must_use]
pub fn html_to_text(html: &str) -> String {
    let mut output = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        output.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            output.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let tag = rest[start + 1..start + end]
            .trim()
            .trim_start_matches('/')
            .split(|character: char| character.is_whitespace() || character == '/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let closing = rest[start + 1..].starts_with('/');
        match tag.as_str() {
            "br" => output.push('\n'),
            "li" if !closing => {
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str("• ");
            }
            "p" | "div" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "ul" | "ol"
            | "blockquote" | "pre" | "table"
                if closing =>
            {
                output.push('\n');
            }
            _ => {}
        }
        rest = &rest[start + end + 1..];
    }
    output.push_str(rest);
    let decoded = output
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let mut lines: Vec<&str> = decoded.lines().map(str::trim_end).collect();
    lines.dedup_by(|current, previous| current.is_empty() && previous.is_empty());
    lines.join("\n").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AzureConfig {
        AzureConfig {
            organization: "demo".into(),
            project: "development".into(),
        }
    }

    #[test]
    fn organization_slugs_accept_urls_and_bare_names() {
        assert_eq!(organization_slug("https://dev.azure.com/demo/"), "demo");
        assert_eq!(organization_slug("https://demo.visualstudio.com"), "demo");
        assert_eq!(organization_slug(" demo "), "demo");
    }

    #[test]
    fn work_items_map_fields_and_hierarchy_relations() {
        let item = json!({
            "id": 12,
            "rev": 2,
            "fields": {
                "System.TeamProject": "development",
                "System.WorkItemType": "Task",
                "System.Title": "Initialize workspace",
                "System.State": "Done",
                "System.Reason": "Completed",
                "System.AssignedTo": {"displayName": "Jacob Ragsdale", "uniqueName": "jacob@example.com"},
                "Microsoft.VSTS.Common.Priority": 2,
                "System.AreaPath": "development",
                "System.IterationPath": "development\\Sprint 1",
                "System.Tags": "tech-debt; azure",
                "System.Description": "<p>First&nbsp;line</p><ul><li>one</li><li>two</li></ul>",
                "System.CreatedDate": "2026-05-16T20:16:20.853Z",
                "System.ChangedDate": "2026-05-16T20:19:36.133Z"
            },
            "relations": [
                {"rel": "System.LinkTypes.Hierarchy-Reverse", "url": "https://dev.azure.com/demo/x/_apis/wit/workItems/11"},
                {"rel": "System.LinkTypes.Related", "url": "https://dev.azure.com/demo/x/_apis/wit/workItems/40"},
                {"rel": "AttachedFile", "url": "https://dev.azure.com/demo/x/_apis/wit/attachments/abc"}
            ]
        });
        let (ticket, relations) = parse_work_item(&item, &config()).unwrap();
        assert_eq!(ticket.key.id, 12);
        assert_eq!(ticket.key.organization, "demo");
        assert_eq!(ticket.assigned_to.as_deref(), Some("Jacob Ragsdale"));
        assert_eq!(ticket.priority, Some(2));
        assert_eq!(ticket.tags, vec!["tech-debt", "azure"]);
        assert_eq!(ticket.description, "First line\n• one\n• two");
        assert_eq!(
            ticket.changed_at,
            crate::timestamp::ts("2026-05-16T20:19:36.133Z")
        );
        assert_eq!(
            ticket.web_url,
            "https://dev.azure.com/demo/development/_workitems/edit/12"
        );
        assert_eq!(relations.len(), 2);
        assert_eq!(relations[0].kind, RelationKind::Parent);
        assert_eq!(relations[0].to.id, 11);
        assert_eq!(relations[1].kind, RelationKind::Related);
    }

    #[test]
    fn profile_documents_yield_a_display_name_when_one_is_present() {
        assert_eq!(
            profile_display_name(&json!({
                "displayName": "Jacob Ragsdale",
                "emailAddress": "jacob@example.com"
            }))
            .as_deref(),
            Some("Jacob Ragsdale")
        );
        assert_eq!(profile_display_name(&json!({"displayName": "  "})), None);
        assert_eq!(profile_display_name(&json!({"id": "abc"})), None);
        assert_eq!(profile_display_name(&json!("Jacob")), None);
    }

    #[test]
    fn team_member_documents_yield_names_and_addresses_once_each() {
        let mut found = Vec::new();
        collect_team_members(
            &json!({"value": [
                {"identity": {"displayName": "Avery Chen", "uniqueName": "avery@example.com"}},
                {"identity": {"displayName": "Dana Okafor"}},
                {"identity": {"displayName": "  ", "uniqueName": "blank@example.com"}},
                {"identity": {"uniqueName": "nameless@example.com"}},
            ]}),
            &mut found,
        );
        assert_eq!(
            found,
            vec![
                Identity::new("Avery Chen", Some("avery@example.com".into())),
                Identity::new("Dana Okafor", None),
            ],
            "somebody without a display name is no use to a picker"
        );

        // The second team the project has, with one person on both.
        collect_team_members(
            &json!({"value": [
                {"identity": {"displayName": "avery chen", "uniqueName": "other@example.com"}},
                {"identity": {"displayName": "Priya Nair", "uniqueName": "priya@example.com"}},
            ]}),
            &mut found,
        );
        assert_eq!(
            found.len(),
            3,
            "somebody on two teams is listed once, however it is spelled"
        );
        assert_eq!(found[2].display_name, "Priya Nair");

        let mut none = Vec::new();
        collect_team_members(&json!({"count": 0}), &mut none);
        assert!(none.is_empty(), "a response without members adds nobody");
    }

    fn response(status: u16, body: &str) -> ureq::http::Response<ureq::Body> {
        ureq::http::Response::builder()
            .status(status)
            .body(ureq::Body::builder().data(body.as_bytes().to_vec()))
            .unwrap()
    }

    #[test]
    fn only_a_refused_token_is_worth_retrying_with_a_fresh_one() {
        let url = "https://dev.azure.com/demo/_apis/wit/workitems";
        for status in [401, 302] {
            let error = read_json(response(status, r#"{"message":"token expired"}"#), url)
                .expect_err("a refused token is an error");
            assert!(
                rejected_credentials(&error),
                "HTTP {status} must be retryable: {error:#}"
            );
            assert!(format!("{error:#}").contains("token expired"), "{error:#}");
        }

        let error = read_json(response(500, r#"{"message":"boom"}"#), url).unwrap_err();
        assert!(
            !rejected_credentials(&error),
            "a server fault is not a credential problem: {error:#}"
        );
        let error = read_json(response(200, "not json"), url).unwrap_err();
        assert!(!rejected_credentials(&error), "{error:#}");

        assert_eq!(
            read_json(response(200, r#"{"count":1}"#), url).unwrap(),
            json!({"count": 1})
        );
    }

    #[test]
    fn a_refused_write_is_a_conflict_only_when_the_work_item_moved_on() {
        let url = "https://dev.azure.com/demo/_apis/wit/workitems/613";
        let conflict = |status: u16, body: &str| {
            let error = read_json(response(status, body), url).unwrap_err();
            is_write_conflict(&error)
        };

        assert!(conflict(409, r#"{"message":"conflict"}"#));
        assert!(conflict(412, r#"{"message":"precondition failed"}"#));
        assert!(
            conflict(
                400,
                r#"{"message":"The \"test\" operation for path \"/rev\" failed."}"#
            ),
            "Azure DevOps reports a failed revision test as a plain 400"
        );
        assert!(conflict(
            400,
            r#"{"message":"Work item 613 has been changed by another client."}"#
        ));
        assert!(conflict(
            400,
            r#"{"message":"The revision 4 does not match the current revision 6."}"#
        ));

        assert!(
            !conflict(
                400,
                r#"{"message":"TF401320: Rule Error for field State. Value 'Testing' is not allowed."}"#
            ),
            "a rule error is the user's problem, not a stale copy"
        );
        assert!(!conflict(403, r#"{"message":"read only field"}"#));
        assert!(
            !conflict(500, r#"{"message":"/rev"}"#),
            "a fault is a fault"
        );

        let error = read_json(response(404, r#"{"message":"does not exist"}"#), url).unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            format!("Azure DevOps returned HTTP 404 for {url}: does not exist"),
            "typing the refusal keeps the message it always had"
        );
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b":pat"), "OnBhdA==");
    }
}
