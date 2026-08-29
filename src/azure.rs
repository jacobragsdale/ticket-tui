//! Azure DevOps work-item sync: authenticate with the Azure CLI, pull the
//! project's work items over REST, and map them onto the local ticket model.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use ureq::http::HeaderMap;
use url::Url;

use crate::classification::{self, ClassificationNode};
use crate::html::html_to_text;
use crate::model::{
    CommentRecord, HistoryRecord, Identity, Issue, Pipeline, RelationKind, RelationRecord, Repo,
    Run, RunResult, RunStatus, StateCategory, StateOption, Ticket, TicketKey, TimelineKind,
    TimelineRecord, WorkItemDetails,
};
use crate::timestamp::Timestamp;

/// Azure DevOps resource id accepted by `az account get-access-token`.
const ADO_RESOURCE: &str = "499b84ac-1321-427f-aa17-267ca6975798";
const API_VERSION: &str = "7.1";
/// Comments are still behind a preview flag on every 7.x API version.
const COMMENTS_API_VERSION: &str = "7.1-preview.4";
/// Largest id batch the work items endpoint accepts.
const BATCH_SIZE: usize = 200;
/// Revisions read per updates request, and the page size that endpoint takes.
const UPDATES_PAGE: usize = 200;
/// How many pages of one work item's comments or updates are read before the
/// client stops asking. A work item with more revisions than this is a bot's,
/// and the details pane is no place to render forty thousand rows.
const MAX_DETAIL_PAGES: usize = 50;
/// Azure DevOps stamps the newest revision's `revisedDate` with a date in this
/// year — `9999-01-01T00:00:00Z` — because nothing has revised that revision
/// yet. It is a sentinel, not an instant, and never reaches the database.
const UNREVISED_YEAR: &str = "9999-";

/// The work item fields whose changes are worth showing, in the order the
/// details pane renders them when one revision touched several. Everything
/// else Azure DevOps reports on an update — the revision number, the changed
/// date, the comment count, the watermark — is bookkeeping about the change
/// rather than the change itself.
const TRACKED_FIELDS: [(&str, &str); 8] = [
    ("System.State", "State"),
    ("System.AssignedTo", "Assigned to"),
    ("System.Title", "Title"),
    ("System.IterationPath", "Iteration"),
    ("System.AreaPath", "Area"),
    ("Microsoft.VSTS.Common.Priority", "Priority"),
    ("System.Tags", "Tags"),
    ("System.Reason", "Reason"),
];
const BODY_LIMIT: u64 = 64 * 1024 * 1024;
/// How long a throttled request waits when Azure DevOps refuses one without
/// saying how long to leave it. Its own guidance is to back off for a good
/// while before asking again, and half a minute is the shortest wait worth
/// calling one.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(30);
/// The longest wait any one header may ask for. A reset stamp read out of a
/// clock that disagrees with ours could otherwise park the timer for days.
const MAX_THROTTLE_PAUSE: Duration = Duration::from_secs(3600);
/// The statuses Azure DevOps sheds load with: too many requests, and the
/// service telling the client to come back later.
const THROTTLED_STATUSES: [u16; 2] = [429, 503];
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
    /// An extra WIQL condition ANDed into both pulls, narrowing a project too
    /// large to hold whole — `[System.ChangedDate] > @today-180`, say. `None`
    /// pulls the project entire.
    pub scope: Option<String>,
}

impl AzureConfig {
    /// Resolve organization, project, and sync scope from explicit values, then
    /// the `TICKET_TUI_ORG` / `TICKET_TUI_PROJECT` / `TICKET_TUI_QUERY`
    /// environment, then the `az devops configure` defaults file.
    pub fn resolve(
        organization: Option<String>,
        project: Option<String>,
        scope: Option<String>,
    ) -> Result<Self> {
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
            scope: sync_scope(scope.or_else(|| std::env::var("TICKET_TUI_QUERY").ok())),
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

/// A sync scope is whatever the user wrote, trimmed; a blank one is no scope at
/// all. The condition itself is never inspected here: WIQL is Azure DevOps's
/// dialect to parse, and a mistake in it comes back as a failed sync rather
/// than as a local guess about what is legal.
fn sync_scope(raw: Option<String>) -> Option<String> {
    raw.map(|scope| scope.trim().to_owned())
        .filter(|scope| !scope.is_empty())
}

/// Every work item in the project the configured scope still lets through. The
/// condition goes through in parentheses so its own `OR` cannot swallow the
/// clauses around it.
fn scoped_project_wiql(scope: Option<&str>) -> String {
    scope.map_or_else(
        || PROJECT_IDS_WIQL.to_owned(),
        |scope| format!("{PROJECT_IDS_WIQL} AND ({scope})"),
    )
}

/// The query behind a full pull: every work item the scope allows.
fn all_ids_wiql(scope: Option<&str>) -> String {
    format!("{} ORDER BY [System.Id]", scoped_project_wiql(scope))
}

/// The query behind an incremental pull: what the scope allows and the
/// watermark says has moved.
fn changed_ids_wiql(scope: Option<&str>, watermark: Timestamp) -> String {
    format!(
        "{} AND [System.ChangedDate] >= '{}' ORDER BY [System.Id]",
        scoped_project_wiql(scope),
        watermark.to_iso8601_utc()
    )
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
    /// When the rate-limit budget the last responses reported comes back, for
    /// a budget that is already spent. `None` while there is room to spare.
    throttled_until: Cell<Option<Instant>>,
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
            throttled_until: Cell::new(None),
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
        let wiql = changed_ids_wiql(self.config.scope.as_deref(), watermark);
        self.fetch_work_items(&self.query_work_item_ids(&wiql)?)
    }

    /// Every work item id the project still has within the configured scope. A
    /// pull compares this against the ids it already holds, because a deleted
    /// work item is not reported as changed — it simply stops being listed, and
    /// so does one an edit has moved outside the scope.
    pub fn query_ids(&self) -> Result<Vec<i64>> {
        self.query_work_item_ids(&all_ids_wiql(self.config.scope.as_deref()))
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

    /// Move one work item under a different parent, or out from under the one
    /// it has when `new_parent` is `None`, answering with Azure DevOps's own
    /// copy of what it stored.
    ///
    /// A parent is not a field but an entry in the work item's relations array,
    /// and taking one off means naming the index it sits at — which is only
    /// knowable from a copy read now. So the work item is fetched with its
    /// links, the document is built against exactly what came back, and the
    /// `test` on `/rev` at the head of it refuses the write if anything moved
    /// in between. One request writes both halves of the move, so there is no
    /// state in which the work item has been detached but not re-filed.
    pub fn reparent_work_item(
        &self,
        id: i64,
        new_parent: Option<i64>,
    ) -> Result<(Ticket, Vec<RelationRecord>)> {
        let url = format!(
            "{}/_apis/wit/workitems/{id}?$expand=relations&api-version={API_VERSION}",
            self.config.base_url()
        );
        let current = self.get(&url)?;
        let document = reparent_document(&current, new_parent, &self.config)?;
        let item = self.send(&url, Request::Patch(&document))?;
        parse_work_item(&item, &self.config)
    }

    /// Add a work item to the project, answering with Azure DevOps's own copy
    /// of what it stored. `fields` are the operations that set its fields —
    /// [`crate::edit::set_field`] builds one each — and `parent` is the work
    /// item it hangs under, which goes out as a link rather than as a field.
    ///
    /// A creation is a `POST` carrying a JSON Patch document: there is no
    /// revision to test, because there is nothing there yet.
    pub fn create_work_item(
        &self,
        work_item_type: &str,
        fields: &[Value],
        parent: Option<i64>,
    ) -> Result<(Ticket, Vec<RelationRecord>)> {
        let document = create_document(fields, parent, &self.config);
        let url = self.create_work_item_url(work_item_type)?;
        let item = self.send(&url, Request::PostPatch(&document))?;
        parse_work_item(&item, &self.config)
    }

    /// Where a new work item is posted: the type is a path segment with a
    /// literal `$` in front of it. Without `$expand=relations` the answer
    /// carries no links at all, so a work item created under a parent would
    /// be stored without one.
    fn create_work_item_url(&self, work_item_type: &str) -> Result<String> {
        self.wit_url(
            &["workitems", &format!("${work_item_type}")],
            &format!("$expand=relations&api-version={API_VERSION}"),
        )
    }

    /// Move one work item to the project's recycle bin.
    ///
    /// The URL carries no `destroy` parameter, so the delete is the soft one:
    /// Azure DevOps takes the work item out of every query and every board and
    /// keeps it, and somebody who deleted the wrong thing restores it from the
    /// recycle bin. Nothing comes back worth reading — the answer is the work
    /// item as it was, which is of no use to a row about to be dropped — so the
    /// body is discarded and only the refusal matters.
    pub fn delete_work_item(&self, id: i64) -> Result<()> {
        let url = format!(
            "{}/_apis/wit/workitems/{id}?api-version={API_VERSION}",
            self.config.base_url()
        );
        self.send(&url, Request::Delete)?;
        Ok(())
    }

    /// The states one work item type allows, in the order the process template
    /// lists them, which is the order the state picker offers. A state carries
    /// the category Azure DevOps assigned it rather than one guessed from its
    /// name, so a custom state is still coloured correctly.
    pub fn fetch_work_item_type_states(&self, work_item_type: &str) -> Result<Vec<StateOption>> {
        let url = self.wit_url(
            &["workitemtypes", work_item_type, "states"],
            &version_query(),
        )?;
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

    /// Every work item type the project's process offers, in the order the
    /// process lists them, which is the order the new-work-item form offers
    /// them. The types nobody files by hand are left out: one the process has
    /// disabled, and one it keeps in its hidden category — the code review and
    /// feedback requests Azure DevOps files for itself. A hidden category that
    /// cannot be read excludes nothing rather than sinking the fetch, because
    /// a list with a few oddities in it is better than no list at all.
    pub fn fetch_work_item_types(&self) -> Result<Vec<String>> {
        let url = self.wit_url(&["workitemtypes"], &version_query())?;
        let response = self.get(&url)?;
        let hidden = self.hidden_work_item_types().unwrap_or_default();
        work_item_type_names(&response, &hidden)
    }

    /// The types the process template keeps out of the way, which is a category
    /// of its own rather than a flag on each type.
    fn hidden_work_item_types(&self) -> Result<Vec<String>> {
        let url = self.wit_url(
            &["workitemtypecategories", "Microsoft.HiddenCategory"],
            &version_query(),
        )?;
        Ok(hidden_type_names(&self.get(&url)?))
    }

    /// One work item's discussion and the revisions behind it, which is what
    /// the details pane shows under its planning fields. Two requests for a
    /// work item of ordinary length, more when either list pages.
    pub fn fetch_work_item_details(&self, id: i64) -> Result<WorkItemDetails> {
        let key = TicketKey {
            organization: self.config.organization.clone(),
            id,
        };
        Ok(WorkItemDetails {
            comments: self.fetch_comments(&key)?,
            history: self.fetch_updates(&key)?,
        })
    }

    /// Leaves one comment on a work item, answering with the record Azure
    /// DevOps stored — its id, its date, and its author as the server settled
    /// them. `html` is the comment body as rich text, which
    /// [`comment_html`] makes out of what was typed; a comment is not a field,
    /// so this is a plain `POST` rather than a JSON Patch, and carries no
    /// revision test.
    pub fn post_comment(&self, id: i64, html: &str) -> Result<CommentRecord> {
        let key = TicketKey {
            organization: self.config.organization.clone(),
            id,
        };
        let posted = self.post(&self.comments_url(id, None)?, &json!({ "text": html }))?;
        parse_comment(&posted, &key)
            .with_context(|| format!("Azure DevOps stored no readable comment on work item {id}"))
    }

    /// Every comment on one work item, following the continuation token the
    /// endpoint answers with while there is another page.
    fn fetch_comments(&self, key: &TicketKey) -> Result<Vec<CommentRecord>> {
        let mut comments = Vec::new();
        let mut continuation: Option<String> = None;
        for _ in 0..MAX_DETAIL_PAGES {
            let page = self.get(&self.comments_url(key.id, continuation.as_deref())?)?;
            comments.extend(parse_comments(&page, key));
            continuation = page
                .get("continuationToken")
                .and_then(Value::as_str)
                .filter(|token| !token.is_empty())
                .map(str::to_owned);
            if continuation.is_none() {
                break;
            }
        }
        Ok(comments)
    }

    /// Every revision of one work item, read a page at a time and mapped onto
    /// history records in one pass: the newest revision's date is resolved
    /// against the revision before it, which can sit on the previous page.
    fn fetch_updates(&self, key: &TicketKey) -> Result<Vec<HistoryRecord>> {
        let mut updates: Vec<Value> = Vec::new();
        for page in 0..MAX_DETAIL_PAGES {
            let response = self.get(&self.updates_url(key.id, page * UPDATES_PAGE)?)?;
            let Some(items) = response.get("value").and_then(Value::as_array) else {
                break;
            };
            let read = items.len();
            updates.extend(items.iter().cloned());
            if read < UPDATES_PAGE {
                break;
            }
        }
        Ok(parse_updates(&updates, key))
    }

    /// Comments hang off the project, and a continuation token is opaque, so
    /// both are escaped rather than pasted into a format string.
    fn comments_url(&self, id: i64, continuation: Option<&str>) -> Result<String> {
        let mut url = self.api_url(&[
            self.config.project.as_str(),
            "_apis",
            "wit",
            "workItems",
            &id.to_string(),
            "comments",
        ])?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("api-version", COMMENTS_API_VERSION);
            if let Some(continuation) = continuation {
                query.append_pair("continuationToken", continuation);
            }
        }
        Ok(url.into())
    }

    /// Updates hang off the organization rather than the project, and page by
    /// `$top`/`$skip` rather than by token.
    fn updates_url(&self, id: i64, skip: usize) -> Result<String> {
        let mut url = self.api_url(&["_apis", "wit", "workItems", &id.to_string(), "updates"])?;
        url.query_pairs_mut()
            .append_pair("api-version", API_VERSION)
            .append_pair("$top", &UPDATES_PAGE.to_string())
            .append_pair("$skip", &skip.to_string());
        Ok(url.into())
    }

    /// A URL under the organization with every path segment escaped, because a
    /// project name or a work item type can carry spaces.
    fn api_url(&self, segments: &[&str]) -> Result<Url> {
        let mut url = Url::parse(&self.config.base_url())
            .with_context(|| format!("invalid Azure DevOps URL {}", self.config.base_url()))?;
        url.path_segments_mut()
            .map_err(|()| anyhow!("Azure DevOps URL cannot carry a path"))?
            .extend(segments);
        Ok(url)
    }

    /// A `{project}/_apis/wit/...` URL with `query` after it as written, so
    /// the `$` in `$expand` and `$depth` survives the way the endpoints spell
    /// their options.
    fn wit_url(&self, tail: &[&str], query: &str) -> Result<String> {
        let mut segments = vec![self.config.project.as_str(), "_apis", "wit"];
        segments.extend_from_slice(tail);
        let mut url = self.api_url(&segments)?;
        url.set_query(Some(query));
        Ok(url.into())
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
        let url = self.wit_url(
            &["classificationnodes"],
            &format!("$depth=10&api-version={API_VERSION}"),
        )?;
        let response = self.get(&url)?;
        Ok(classification::parse_classification_nodes(&response))
    }

    /// The project's Git repositories, and the project's own GUID, which comes
    /// back on each of them and is what the pull request and artifact-link
    /// endpoints ask for. One cheap request, made on every pull.
    pub fn fetch_repositories(&self) -> Result<(Vec<Repo>, Option<String>)> {
        let segments = [self.config.project.as_str(), "_apis", "git", "repositories"];
        let mut url = self.api_url(&segments)?;
        url.set_query(Some(&version_query()));
        let response = self.get(url.as_str())?;
        Ok(parse_repositories(&response, &self.config.project))
    }

    /// The project's build definitions. `includeAllProperties` is what carries
    /// the repository and the default branch, which the Pipelines tab needs to
    /// say what a pipeline builds.
    pub fn fetch_pipelines(&self) -> Result<Vec<Pipeline>> {
        let segments = [
            self.config.project.as_str(),
            "_apis",
            "build",
            "definitions",
        ];
        let mut url = self.api_url(&segments)?;
        url.set_query(Some(&format!(
            "includeAllProperties=true&api-version={API_VERSION}"
        )));
        Ok(parse_pipelines(&self.get(url.as_str())?))
    }

    /// The newest runs in the project, whatever pipeline they belong to. One
    /// window, newest first, which is also what prunes the stored table.
    pub fn fetch_runs(&self) -> Result<Vec<Run>> {
        let segments = [self.config.project.as_str(), "_apis", "build", "builds"];
        let mut url = self.api_url(&segments)?;
        url.set_query(Some(&format!(
            "$top={RUN_WINDOW}&queryOrder=queueTimeDescending&api-version={API_VERSION}"
        )));
        Ok(parse_runs(&self.get(url.as_str())?))
    }

    /// Every run that is queued, going, or being cancelled. This is the one
    /// the watcher polls, so it asks for as little as it can: the statuses
    /// that mean "still happening", and a small window of them.
    pub fn fetch_live_runs(&self) -> Result<Vec<Run>> {
        let segments = [self.config.project.as_str(), "_apis", "build", "builds"];
        let mut url = self.api_url(&segments)?;
        url.set_query(Some(&format!(
            "statusFilter=inProgress,notStarted,cancelling&$top=50\
             &queryOrder=queueTimeDescending&api-version={API_VERSION}"
        )));
        Ok(parse_runs(&self.get(url.as_str())?))
    }

    /// One run's timeline: its stages, jobs and tasks, and what each is
    /// doing. This is what the watcher reads every five seconds while a run is
    /// on screen and going.
    pub fn fetch_timeline(&self, run_id: i64) -> Result<Vec<TimelineRecord>> {
        let id = run_id.to_string();
        let segments = [
            self.config.project.as_str(),
            "_apis",
            "build",
            "builds",
            id.as_str(),
            "timeline",
        ];
        let mut url = self.api_url(&segments)?;
        url.set_query(Some(&version_query()));
        Ok(parse_timeline(&self.get(url.as_str())?))
    }

    /// The teams hang off `_apis/projects` rather than off the project. `tail`
    /// is whatever follows `teams`.
    fn teams_url(&self, tail: &[&str]) -> Result<String> {
        let mut segments = vec!["_apis", "projects", self.config.project.as_str(), "teams"];
        segments.extend_from_slice(tail);
        let mut url = self.api_url(&segments)?;
        url.set_query(Some(&version_query()));
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
        // `timePrecision` is what lets a date in the query carry a time as
        // well. Without it Azure DevOps refuses the incremental pull outright:
        // the watermark names the second an edit landed, and a query read at
        // date precision may not mention one.
        let url = format!(
            "{}/{}/_apis/wit/wiql?timePrecision=true&api-version={API_VERSION}",
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
            Request::PostPatch(document) => authorized(self.agent.post(url), &authorization)
                .header("Content-Type", "application/json-patch+json")
                .send_json(document)
                .with_context(|| format!("POST {url} failed"))?,
            Request::Delete => authorized(self.agent.delete(url), &authorization)
                .call()
                .with_context(|| format!("DELETE {url} failed"))?,
        };
        self.note_rate_limit(response.headers());
        read_json(response, url)
    }

    /// Records how long the last response asked to be left alone. Azure DevOps
    /// reports the budget left on ordinary successes, well before it starts
    /// refusing requests outright, so a spent budget is a chance to hold off
    /// rather than something to find out about from the next 429.
    fn note_rate_limit(&self, headers: &HeaderMap) {
        let Some(until) = rate_limit_pause(headers, unix_now())
            .and_then(|delay| Instant::now().checked_add(delay))
        else {
            return;
        };
        // One pull makes several requests; the longest wait any of them asked
        // for is the one that has to be honoured.
        if self.throttled_until.get().is_none_or(|held| until > held) {
            self.throttled_until.set(Some(until));
        }
    }

    /// How long the responses since the last time this was asked want to be
    /// left alone. Reading it clears it, so one spent budget delays one pull
    /// rather than every pull after it.
    pub fn throttled_for(&self) -> Option<Duration> {
        let until = self.throttled_until.take()?;
        let left = until.saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left)
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
    /// The same document posted rather than patched, which is how a work item
    /// is created: there is no work item yet to patch.
    PostPatch(&'a [Value]),
    /// Move a work item to the recycle bin. Nothing goes out with it and
    /// nothing worth reading comes back.
    Delete,
}

/// The query every plain endpoint takes: the API version and nothing else.
/// How many runs one pull brings back. The stored table is exactly this
/// window, so it never grows.
const RUN_WINDOW: usize = 200;

fn parse_pipelines(response: &Value) -> Vec<Pipeline> {
    response["value"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| {
            Some(Pipeline {
                id: entry["id"].as_i64()?,
                name: entry["name"].as_str()?.to_owned(),
                folder: entry["path"].as_str().unwrap_or("\\").to_owned(),
                repo_id: entry["repository"]["id"].as_str().map(str::to_owned),
                default_branch: entry["repository"]["defaultBranch"]
                    .as_str()
                    .map(str::to_owned),
                url: entry["_links"]["web"]["href"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                queue_status: entry["queueStatus"]
                    .as_str()
                    .unwrap_or("enabled")
                    .to_owned(),
            })
        })
        .collect()
}

fn parse_runs(response: &Value) -> Vec<Run> {
    response["value"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| {
            let time = |key: &str| {
                entry[key]
                    .as_str()
                    .and_then(|raw| Timestamp::parse(raw).ok())
            };
            Some(Run {
                id: entry["id"].as_i64()?,
                pipeline_id: entry["definition"]["id"].as_i64()?,
                build_number: entry["buildNumber"].as_str().unwrap_or_default().to_owned(),
                status: RunStatus::parse(entry["status"].as_str().unwrap_or_default()),
                result: entry["result"].as_str().and_then(RunResult::parse),
                source_branch: entry["sourceBranch"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                source_version: entry["sourceVersion"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                requested_for: entry["requestedFor"]["displayName"]
                    .as_str()
                    .map(str::to_owned),
                reason: entry["reason"].as_str().unwrap_or_default().to_owned(),
                // Azure DevOps reports the pull request number as a string.
                pr_id: entry["triggerInfo"]["pr.number"]
                    .as_str()
                    .and_then(|raw| raw.parse().ok()),
                queue_time: time("queueTime"),
                start_time: time("startTime"),
                finish_time: time("finishTime"),
                url: entry["_links"]["web"]["href"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect()
}

/// The records in a timeline response. Phase records are dropped and their
/// children re-parented onto the stage above them, which is the tree the epic
/// draws: stages, jobs, tasks.
fn parse_timeline(response: &Value) -> Vec<TimelineRecord> {
    let entries = response["records"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    // Every phase, and the stage it hangs under, so its jobs can be lifted.
    let phases: HashMap<&str, Option<String>> = entries
        .iter()
        .filter(|entry| {
            entry["type"]
                .as_str()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("phase"))
        })
        .filter_map(|entry| {
            Some((
                entry["id"].as_str()?,
                entry["parentId"].as_str().map(str::to_owned),
            ))
        })
        .collect();
    let mut records: Vec<TimelineRecord> = entries
        .iter()
        .filter_map(|entry| {
            let kind = TimelineKind::parse(entry["type"].as_str()?)?;
            let parent = entry["parentId"].as_str().map(str::to_owned);
            let parent = parent.map(|parent| {
                phases
                    .get(parent.as_str())
                    .and_then(Clone::clone)
                    .unwrap_or(parent)
            });
            let time = |key: &str| {
                entry[key]
                    .as_str()
                    .and_then(|raw| Timestamp::parse(raw).ok())
            };
            Some(TimelineRecord {
                id: entry["id"].as_str()?.to_owned(),
                parent_id: parent,
                kind,
                name: entry["name"].as_str().unwrap_or_default().to_owned(),
                state: RunStatus::parse(entry["state"].as_str().unwrap_or_default()),
                result: entry["result"].as_str().and_then(RunResult::parse),
                start: time("startTime"),
                finish: time("finishTime"),
                percent_complete: entry["percentComplete"].as_i64(),
                log_id: entry["log"]["id"].as_i64(),
                order: entry["order"].as_i64().unwrap_or_default(),
                issues: entry["issues"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|issue| {
                        Some(Issue {
                            kind: issue["type"].as_str()?.to_owned(),
                            message: issue["message"].as_str().unwrap_or_default().to_owned(),
                        })
                    })
                    .collect(),
            })
        })
        .collect();
    records.sort_by_key(|record| record.order);
    records
}

/// The repositories in a `GET .../_apis/git/repositories` response, and the
/// project GUID they all carry. A repository the response cannot be read as is
/// left out rather than sinking the pull.
fn parse_repositories(response: &Value, project: &str) -> (Vec<Repo>, Option<String>) {
    let mut project_id = None;
    let mut repos: Vec<Repo> = response["value"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| {
            let id = entry["id"].as_str()?.to_owned();
            let name = entry["name"].as_str()?.to_owned();
            if project_id.is_none() {
                project_id = entry["project"]["id"].as_str().map(str::to_owned);
            }
            Some(Repo {
                id,
                name,
                project: entry["project"]["name"]
                    .as_str()
                    .unwrap_or(project)
                    .to_owned(),
                default_branch: entry["defaultBranch"].as_str().map(str::to_owned),
                remote_url: entry["remoteUrl"].as_str().unwrap_or_default().to_owned(),
                ssh_url: entry["sshUrl"].as_str().unwrap_or_default().to_owned(),
                web_url: entry["webUrl"].as_str().unwrap_or_default().to_owned(),
                is_disabled: entry["isDisabled"].as_bool().unwrap_or_default(),
                size: entry["size"].as_i64(),
            })
        })
        .collect();
    repos.sort_by_key(|repo| repo.name.to_lowercase());
    (repos, project_id)
}

fn version_query() -> String {
    format!("api-version={API_VERSION}")
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

/// Azure DevOps turned a request away to shed load, and said how long to leave
/// it. Carried as its own error type because it is not a failure to report and
/// forget: the timer that asked has to hold off, and asking again straight away
/// is what makes the throttling worse.
#[derive(Debug)]
pub struct Throttled {
    retry_after: Duration,
    status: u16,
    url: String,
    message: String,
}

impl Throttled {
    #[must_use]
    pub fn new(
        retry_after: Duration,
        status: u16,
        url: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            retry_after,
            status,
            url: url.into(),
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn retry_after(&self) -> Duration {
        self.retry_after
    }
}

impl fmt::Display for Throttled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Azure DevOps is throttling requests (HTTP {}) for {}; try again in {}s: {}",
            self.status,
            self.url,
            self.retry_after.as_secs(),
            self.message
        )
    }
}

impl std::error::Error for Throttled {}

/// How long a failure asks the caller to wait before trying again, for a
/// request Azure DevOps turned away to shed load. `None` for every other
/// failure, which is what tells a pull to report itself as failed rather than
/// as paused.
#[must_use]
pub fn throttle_delay(error: &anyhow::Error) -> Option<Duration> {
    error
        .downcast_ref::<Throttled>()
        .map(Throttled::retry_after)
}

/// The wait a throttled response asks for: `Retry-After` read as whole seconds,
/// falling back to [`DEFAULT_RETRY_AFTER`] when the header is absent or is
/// something this client cannot read, such as the HTTP-date form.
fn retry_after_delay(headers: &HeaderMap) -> Duration {
    header_number(headers, "retry-after")
        .filter(|seconds| *seconds >= 0.0)
        .map_or(DEFAULT_RETRY_AFTER, |seconds| {
            Duration::from_secs_f64(seconds.min(MAX_THROTTLE_PAUSE.as_secs_f64()))
        })
}

/// How long a response that still carried its data asks to be left alone:
/// `Some` only when `X-RateLimit-Remaining` says the budget is spent, and then
/// until the `X-RateLimit-Reset` epoch it names. Azure DevOps sends these on
/// ordinary successes, ahead of the 429 a spent budget turns into.
fn rate_limit_pause(headers: &HeaderMap, now: f64) -> Option<Duration> {
    if header_number(headers, "x-ratelimit-remaining")? > 0.0 {
        return None;
    }
    let delay = header_number(headers, "x-ratelimit-reset")
        .map_or(DEFAULT_RETRY_AFTER.as_secs_f64(), |reset| reset - now);
    // A reset already in the past is a budget already back: nothing to wait for.
    (delay >= 1.0).then(|| Duration::from_secs_f64(delay.min(MAX_THROTTLE_PAUSE.as_secs_f64())))
}

/// One header read as a number. Azure DevOps writes these as integers, but the
/// rate-limit counters are documented as usage units and read as decimals just
/// as happily.
fn header_number(headers: &HeaderMap, name: &str) -> Option<f64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

/// Seconds since the Unix epoch, for reading a rate-limit reset stamp. A clock
/// somehow set before the epoch reads as the epoch, which can only ask for a
/// longer wait than the header meant.
fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
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
    // Read before the body, which takes the response apart.
    let retry_after = retry_after_delay(response.headers());
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
        // Throttling first: it is the one refusal that is neither the caller's
        // fault nor worth reporting, only worth waiting out.
        if THROTTLED_STATUSES.contains(&status) {
            return Err(anyhow::Error::new(Throttled::new(
                retry_after,
                status,
                url,
                message,
            )));
        }
        if status == 401 || status == 302 {
            return Err(anyhow::Error::new(RejectedCredentials(format!(
                "Azure DevOps rejected the credentials ({status}); run `az login` and retry: {message}"
            ))));
        }
        return Err(anyhow::Error::new(RequestRejected::new(
            status, url, message,
        )));
    }
    // A success with nothing in it is an answer rather than a broken one: a
    // delete replies `204 No Content`, and the caller has no document to read
    // either way.
    if text.trim().is_empty() {
        return Ok(Value::Null);
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

/// The link type a parent is held under, on the child's side of it.
const PARENT_REL: &str = "System.LinkTypes.Hierarchy-Reverse";

/// The JSON Patch operation that files a work item under `parent`. A parent is
/// not a field, so it cannot be written like one: it is a relation appended to
/// the work item's own list, naming the link type and the parent's API URL.
/// The URL hangs off the organization rather than the project, which is the
/// form [`parse_work_item`] reads back.
fn parent_link(parent: i64, config: &AzureConfig) -> Value {
    json!({
        "op": "add",
        "path": "/relations/-",
        "value": {
            "rel": PARENT_REL,
            "url": format!("{}/_apis/wit/workItems/{parent}", config.base_url()),
        },
    })
}

/// The JSON Patch document that creates a work item: the operations setting
/// its fields, then the link to its parent when it has one.
#[must_use]
pub fn create_document(fields: &[Value], parent: Option<i64>, config: &AzureConfig) -> Vec<Value> {
    let mut document = fields.to_vec();
    if let Some(parent) = parent {
        document.push(parent_link(parent, config));
    }
    document
}

/// The JSON Patch document that moves one work item between parents, built
/// against `item` — the copy of the work item just read, links and all.
///
/// The three kinds of operation go in one document so the move is one write:
///
/// * `test /rev` leads, so a work item somebody else changed between the read
///   and this write is refused rather than patched against stale indices.
/// * `remove /relations/{index}` takes the parent link off. A relation can only
///   be removed by its position in the array, so the index is the one it holds
///   in `item` as it was just read. Any further ones are removed in descending
///   order, because each removal shifts everything after it down by one.
/// * `add /relations/-` appends the new parent, which is why it comes last: an
///   append cannot move an index the removes above it still need.
///
/// A document that would neither remove nor add anything is not a move at all,
/// and is refused here rather than sent.
fn reparent_document(
    item: &Value,
    new_parent: Option<i64>,
    config: &AzureConfig,
) -> Result<Vec<Value>> {
    let id = item.get("id").and_then(Value::as_i64);
    let revision = item
        .get("rev")
        .and_then(Value::as_i64)
        .with_context(|| format!("work item {id:?} came back without a revision to test"))?;
    let held: Vec<usize> = item
        .get("relations")
        .and_then(Value::as_array)
        .map(|relations| {
            relations
                .iter()
                .enumerate()
                .filter(|(_, relation)| {
                    relation.get("rel").and_then(Value::as_str) == Some(PARENT_REL)
                })
                .map(|(index, _)| index)
                .collect()
        })
        .unwrap_or_default();
    if held.is_empty() && new_parent.is_none() {
        bail!(
            "work item {} has no parent to remove",
            id.unwrap_or_default()
        );
    }
    let mut document = vec![crate::edit::revision_test(revision)];
    for index in held.into_iter().rev() {
        document.push(json!({"op": "remove", "path": format!("/relations/{index}")}));
    }
    if let Some(parent) = new_parent {
        document.push(parent_link(parent, config));
    }
    Ok(document)
}

/// The offerable types in one `/_apis/wit/workitemtypes` response: everything
/// the process has not disabled, less the ones `hidden` names.
fn work_item_type_names(response: &Value, hidden: &[String]) -> Result<Vec<String>> {
    let types = response
        .get("value")
        .and_then(Value::as_array)
        .context("work item types response has no value array")?;
    Ok(types
        .iter()
        .filter(|item| item.get("isDisabled").and_then(Value::as_bool) != Some(true))
        .filter_map(|item| {
            let name = item.get("name").and_then(Value::as_str)?.trim();
            (!name.is_empty() && !hidden.iter().any(|skip| skip == name)).then(|| name.to_owned())
        })
        .collect())
}

/// The types one `/_apis/wit/workitemtypecategories/...` response holds. A
/// response that names none excludes none.
fn hidden_type_names(response: &Value) -> Vec<String> {
    response
        .get("workItemTypes")
        .and_then(Value::as_array)
        .map(|types| {
            types
                .iter()
                .filter_map(|item| Some(item.get("name")?.as_str()?.trim().to_owned()))
                .collect()
        })
        .unwrap_or_default()
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
        description_html: text("System.Description").unwrap_or_default(),
        created_at: timestamp("System.CreatedDate")?,
        changed_at: timestamp("System.ChangedDate")?,
        web_url: config.work_item_url(id),
        // A work item read from the list endpoint carries no comments or
        // history: those are two more requests, made only when this revision
        // is the one being looked at. An edit lands the same way, which is
        // what makes the details pane read a work item again after a write.
        details_rev: 0,
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

/// The rich text one typed comment is posted as. Azure DevOps stores a comment
/// body as HTML, so the three characters that would otherwise be read as markup
/// are escaped — `&` first, or it would escape its own replacements — and the
/// result is wrapped in a paragraph, which is what the browser writes and what
/// [`html_to_text`] reads back as the line that was typed.
#[must_use]
pub fn comment_html(text: &str) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!("<p>{escaped}</p>")
}

/// Map one `/_apis/wit/workItems/{id}/comments` page onto comment records.
/// A comment whose body flattens to nothing is dropped: an author line with
/// no text under it says less than the space it takes.
#[must_use]
pub fn parse_comments(page: &Value, key: &TicketKey) -> Vec<CommentRecord> {
    page.get("comments")
        .or_else(|| page.get("value"))
        .and_then(Value::as_array)
        .map(|comments| {
            comments
                .iter()
                .filter_map(|comment| parse_comment(comment, key))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_comment(comment: &Value, key: &TicketKey) -> Option<CommentRecord> {
    let text = html_to_text(
        comment
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    if text.is_empty() {
        return None;
    }
    Some(CommentRecord {
        ticket: key.clone(),
        comment_id: comment.get("id").and_then(Value::as_i64)?,
        created_at: comment
            .get("createdDate")
            .and_then(Value::as_str)
            .and_then(|raw| Timestamp::parse(raw).ok())?,
        author: comment.get("createdBy").and_then(identity_name),
        text,
    })
}

/// Map `/_apis/wit/workItems/{id}/updates` entries onto history records,
/// oldest revision first.
///
/// Only [`TRACKED_FIELDS`] survive: a revision that moved nothing else — a
/// description reworded, a comment counted, a watermark advanced — is not a
/// change anyone reads a history for, and is dropped whole. The newest
/// revision's `revisedDate` is Azure DevOps's [`UNREVISED_YEAR`] sentinel
/// rather than an instant, so its own `System.ChangedDate` stands in, and
/// failing that the revision before it does.
#[must_use]
pub fn parse_updates(updates: &[Value], key: &TicketKey) -> Vec<HistoryRecord> {
    let mut records = Vec::new();
    let mut previous: Option<Timestamp> = None;
    for update in updates {
        let fields = update.get("fields").and_then(Value::as_object);
        let Some(changed_at) = update_timestamp(update, fields, previous) else {
            continue;
        };
        previous = Some(changed_at);
        let (Some(revision), Some(fields)) = (update.get("rev").and_then(Value::as_i64), fields)
        else {
            continue;
        };
        let changed_by = update.get("revisedBy").and_then(identity_name);
        for (name, label) in TRACKED_FIELDS {
            let Some(change) = fields.get(name) else {
                continue;
            };
            let old_value = field_value(change.get("oldValue"));
            let new_value = field_value(change.get("newValue"));
            if old_value == new_value {
                continue;
            }
            records.push(HistoryRecord {
                ticket: key.clone(),
                revision,
                changed_at,
                changed_by: changed_by.clone(),
                field_name: (*label).to_owned(),
                old_value,
                new_value,
            });
        }
    }
    records
}

/// When one revision landed. The sentinel the newest revision carries is not a
/// date, so what the revision said about `System.ChangedDate` is used instead,
/// and failing that the date of the revision before it.
fn update_timestamp(
    update: &Value,
    fields: Option<&serde_json::Map<String, Value>>,
    previous: Option<Timestamp>,
) -> Option<Timestamp> {
    let revised = update
        .get("revisedDate")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if !revised.is_empty() && !revised.starts_with(UNREVISED_YEAR) {
        return Timestamp::parse(revised).ok();
    }
    fields
        .and_then(|fields| fields.get("System.ChangedDate"))
        .and_then(|change| change.get("newValue"))
        .and_then(Value::as_str)
        .and_then(|raw| Timestamp::parse(raw).ok())
        .or(previous)
}

/// One side of a field change as text: an identity keeps its display name, a
/// number or a flag its literal, and a blank string is no value at all — which
/// is how a field being set for the first time reads.
fn field_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(raw) => {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        }
        object @ Value::Object(_) => identity_name(object),
        other => Some(other.to_string()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AzureConfig {
        AzureConfig {
            organization: "demo".into(),
            project: "development".into(),
            scope: None,
        }
    }

    /// A client that never reaches the network, for the URLs it builds.
    fn client(config: AzureConfig) -> AzureClient {
        AzureClient {
            agent: ureq::Agent::new_with_defaults(),
            config,
            authorization: RefCell::new("Bearer test".into()),
            throttled_until: Cell::new(None),
        }
    }

    #[test]
    fn the_offerable_work_item_types_leave_out_the_disabled_and_the_hidden_ones() {
        let response = json!({
            "value": [
                {"name": "Epic"},
                {"name": "Issue"},
                {"name": "Task"},
                {"name": "Code Review Request"},
                {"name": "Retired", "isDisabled": true},
                {"name": "  "},
            ],
        });
        let hidden = hidden_type_names(&json!({
            "workItemTypes": [{"name": "Code Review Request"}, {"name": "Feedback Request"}],
        }));

        assert_eq!(hidden, ["Code Review Request", "Feedback Request"]);
        assert_eq!(
            work_item_type_names(&response, &hidden).unwrap(),
            ["Epic", "Issue", "Task"],
            "the order the process listed them, less what nobody files by hand"
        );
        assert!(
            work_item_type_names(&response, &hidden_type_names(&json!({})))
                .unwrap()
                .contains(&"Code Review Request".to_owned()),
            "a hidden category that could not be read excludes nothing"
        );
    }

    #[test]
    fn a_sync_scope_narrows_both_pulls_and_travels_verbatim_in_parentheses() {
        assert_eq!(
            all_ids_wiql(None),
            "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project \
             ORDER BY [System.Id]"
        );
        assert_eq!(
            changed_ids_wiql(None, crate::timestamp::ts("2026-08-28T20:15:03Z")),
            "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project \
             AND [System.ChangedDate] >= '2026-08-28T20:15:03Z' ORDER BY [System.Id]"
        );

        let scope = Some("[System.WorkItemType] <> 'Test Case' OR [System.Id] = 7");
        assert_eq!(
            all_ids_wiql(scope),
            "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project \
             AND ([System.WorkItemType] <> 'Test Case' OR [System.Id] = 7) ORDER BY [System.Id]",
            "the condition is parenthesised so its own OR cannot swallow the project clause"
        );
        assert_eq!(
            changed_ids_wiql(scope, crate::timestamp::ts("2026-08-28T20:15:03Z")),
            "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project \
             AND ([System.WorkItemType] <> 'Test Case' OR [System.Id] = 7) \
             AND [System.ChangedDate] >= '2026-08-28T20:15:03Z' ORDER BY [System.Id]"
        );

        assert_eq!(
            sync_scope(Some("  [System.ChangedDate] > @today-180 ".into())).as_deref(),
            Some("[System.ChangedDate] > @today-180")
        );
        assert_eq!(
            sync_scope(Some("   ".into())),
            None,
            "a blank scope is none"
        );
        assert_eq!(sync_scope(None), None);
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
        assert_eq!(ticket.description, "First line\n\n• one\n• two");
        assert_eq!(
            ticket.description_html, "<p>First&nbsp;line</p><ul><li>one</li><li>two</li></ul>",
            "the editor gets the document Azure DevOps stored, not the reading of it"
        );
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

    fn key(id: i64) -> TicketKey {
        TicketKey {
            organization: "demo".into(),
            id,
        }
    }

    #[test]
    fn updates_keep_the_fields_a_person_reads_a_history_for_and_drop_the_bookkeeping() {
        let updates = [
            json!({
                "id": 1, "rev": 1,
                "revisedBy": {"displayName": "Jacob Ragsdale"},
                "revisedDate": "2026-08-20T10:00:00Z",
                "fields": {
                    "System.Id": {"newValue": 613},
                    "System.Rev": {"newValue": 1},
                    "System.Watermark": {"newValue": 4},
                    "System.State": {"newValue": "To Do"},
                    "System.ChangedDate": {"newValue": "2026-08-20T10:00:00Z"}
                }
            }),
            json!({
                "id": 2, "rev": 2,
                "revisedBy": {"displayName": "Avery Chen"},
                "revisedDate": "2026-08-21T09:30:00Z",
                "fields": {
                    "System.Rev": {"oldValue": 1, "newValue": 2},
                    "System.Watermark": {"oldValue": 4, "newValue": 5},
                    "System.CommentCount": {"oldValue": 0, "newValue": 1},
                    "System.ChangedDate": {
                        "oldValue": "2026-08-20T10:00:00Z",
                        "newValue": "2026-08-21T09:30:00Z"
                    }
                }
            }),
            json!({
                "id": 3, "rev": 3,
                "revisedBy": {"displayName": "Jacob Ragsdale"},
                // The newest revision has not been revised, so Azure DevOps
                // stamps it with a sentinel rather than a date.
                "revisedDate": "9999-01-01T00:00:00Z",
                "fields": {
                    "System.Rev": {"oldValue": 2, "newValue": 3},
                    "System.State": {"oldValue": "To Do", "newValue": "Doing"},
                    "System.AssignedTo": {
                        "oldValue": {"displayName": "Avery Chen", "uniqueName": "avery@example.com"},
                        "newValue": {"displayName": "Jacob Ragsdale", "uniqueName": "jacob@example.com"}
                    },
                    "Microsoft.VSTS.Common.Priority": {"oldValue": 2, "newValue": 1},
                    "System.ChangedDate": {
                        "oldValue": "2026-08-21T09:30:00Z",
                        "newValue": "2026-08-22T16:45:00Z"
                    }
                }
            }),
        ];

        let history = parse_updates(&updates, &key(613));
        let rendered: Vec<String> = history
            .iter()
            .map(|entry| {
                format!(
                    "r{} {} {}: {} → {}",
                    entry.revision,
                    entry.changed_at.exact_utc(),
                    entry.field_name,
                    entry.old_value.as_deref().unwrap_or("—"),
                    entry.new_value.as_deref().unwrap_or("—"),
                )
            })
            .collect();
        assert_eq!(
            rendered,
            [
                "r1 2026-08-20 10:00:00 UTC State: — → To Do",
                "r3 2026-08-22 16:45:00 UTC State: To Do → Doing",
                "r3 2026-08-22 16:45:00 UTC Assigned to: Avery Chen → Jacob Ragsdale",
                "r3 2026-08-22 16:45:00 UTC Priority: 2 → 1",
            ],
            "revision 2 moved only bookkeeping, so it is not a change at all"
        );
        assert!(
            history
                .iter()
                .all(|entry| entry.ticket == key(613)
                    && !entry.changed_at.to_rfc3339().contains("9999")),
            "the sentinel never reaches the database"
        );
        assert_eq!(
            history[1].changed_by.as_deref(),
            Some("Jacob Ragsdale"),
            "an identity is stored as the name it displays under"
        );
    }

    #[test]
    fn a_sentinel_date_with_nothing_to_replace_it_falls_back_to_the_revision_before() {
        let updates = [
            json!({
                "rev": 1,
                "revisedDate": "2026-08-20T10:00:00Z",
                "fields": {"System.State": {"newValue": "To Do"}}
            }),
            json!({
                "rev": 2,
                "revisedDate": "9999-01-01T00:00:00Z",
                "fields": {"System.State": {"oldValue": "To Do", "newValue": "Done"}}
            }),
        ];

        let history = parse_updates(&updates, &key(613));
        assert_eq!(history.len(), 2);
        assert_eq!(
            history[1].changed_at, history[0].changed_at,
            "with no changed date of its own, the newest revision is dated by the one before it"
        );

        assert!(
            parse_updates(&[json!({"rev": 1, "fields": {}})], &key(613)).is_empty(),
            "the very first revision, undatable and touching nothing, is no history at all"
        );
    }

    #[test]
    fn comments_are_flattened_to_text_and_the_empty_ones_dropped() {
        let page = json!({
            "totalCount": 3,
            "count": 3,
            "comments": [
                {
                    "id": 5,
                    "text": "<div>Looks&nbsp;good</div><div>Shipping it</div>",
                    "createdBy": {"displayName": "Avery Chen", "uniqueName": "avery@example.com"},
                    "createdDate": "2026-08-21T09:30:00Z"
                },
                {
                    "id": 6,
                    "text": "<div>   </div>",
                    "createdBy": {"displayName": "A Bot"},
                    "createdDate": "2026-08-21T09:31:00Z"
                },
                {
                    "id": 7,
                    "text": "No author here",
                    "createdDate": "2026-08-21T09:32:00Z"
                }
            ],
            "continuationToken": "next"
        });

        let comments = parse_comments(&page, &key(613));
        assert_eq!(
            comments
                .iter()
                .map(|comment| (
                    comment.comment_id,
                    comment.author.as_deref(),
                    comment.text.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![
                (5, Some("Avery Chen"), "Looks good\nShipping it"),
                (7, None, "No author here"),
            ],
            "a comment that flattens to nothing is not worth a line"
        );
        assert_eq!(comments[0].ticket, key(613));
        assert_eq!(
            comments[0].created_at,
            crate::timestamp::ts("2026-08-21T09:30:00Z")
        );
        assert!(parse_comments(&json!({"count": 0}), &key(613)).is_empty());
    }

    #[test]
    fn a_typed_comment_is_escaped_into_a_paragraph_and_reads_back_as_itself() {
        assert_eq!(comment_html("<b>&"), "<p>&lt;b&gt;&amp;</p>");
        assert_eq!(
            html_to_text(&comment_html("<b>&")),
            "<b>&",
            "what was typed is what the details pane shows again"
        );
        assert_eq!(comment_html("Merged into main"), "<p>Merged into main</p>");

        let posted = json!({
            "id": 42,
            "createdDate": "2026-03-04T09:15:00Z",
            "createdBy": {"displayName": "Jacob Ragsdale"},
            "text": comment_html("blocked on <auth>"),
        });
        let stored = parse_comment(&posted, &key(613)).expect("the stored comment reads back");
        assert_eq!(stored.comment_id, 42);
        assert_eq!(stored.text, "blocked on <auth>");
        assert_eq!(stored.author.as_deref(), Some("Jacob Ragsdale"));
        assert_eq!(stored.ticket, key(613));
    }

    #[test]
    fn detail_urls_escape_the_project_and_the_continuation_token() {
        let client = client(AzureConfig {
            project: "my project".into(),
            ..config()
        });

        assert_eq!(
            client.comments_url(613, None).unwrap(),
            "https://dev.azure.com/demo/my%20project/_apis/wit/workItems/613/comments\
             ?api-version=7.1-preview.4"
        );
        assert_eq!(
            client.comments_url(613, Some("a b/c")).unwrap(),
            "https://dev.azure.com/demo/my%20project/_apis/wit/workItems/613/comments\
             ?api-version=7.1-preview.4&continuationToken=a+b%2Fc"
        );
        assert_eq!(
            client.updates_url(613, 200).unwrap(),
            "https://dev.azure.com/demo/_apis/wit/workItems/613/updates\
             ?api-version=7.1&%24top=200&%24skip=200",
            "updates hang off the organization, not the project"
        );
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
        response_with(status, body, &[])
    }

    /// A synthetic response carrying the headers a throttled Azure DevOps sends.
    fn response_with(
        status: u16,
        body: &str,
        headers: &[(&str, &str)],
    ) -> ureq::http::Response<ureq::Body> {
        let mut builder = ureq::http::Response::builder().status(status);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder
            .body(ureq::Body::builder().data(body.as_bytes().to_vec()))
            .unwrap()
    }

    fn headers_of(pairs: &[(&str, &str)]) -> HeaderMap {
        response_with(200, "{}", pairs).headers().clone()
    }

    #[test]
    fn a_throttled_response_carries_the_wait_it_asked_for() {
        let url = "https://dev.azure.com/demo/_apis/wit/wiql";
        let throttled = |status: u16, headers: &[(&str, &str)]| {
            read_json(
                response_with(status, r#"{"message":"too many requests"}"#, headers),
                url,
            )
            .expect_err("a throttled request is an error")
        };

        let error = throttled(429, &[("Retry-After", "45")]);
        assert_eq!(throttle_delay(&error), Some(Duration::from_secs(45)));
        assert!(
            format!("{error:#}").contains("try again in 45s"),
            "{error:#}"
        );
        assert!(
            format!("{error:#}").contains("too many requests"),
            "{error:#}"
        );

        assert_eq!(
            throttle_delay(&throttled(503, &[("Retry-After", " 5 ")])),
            Some(Duration::from_secs(5)),
            "503 is the other way Azure DevOps sheds load"
        );

        for headers in [
            &[][..],
            &[("Retry-After", "Wed, 21 Oct 2026 07:28:00 GMT")],
            &[("Retry-After", "-1")],
        ] {
            assert_eq!(
                throttle_delay(&throttled(429, headers)),
                Some(DEFAULT_RETRY_AFTER),
                "a wait this client cannot read is still a wait: {headers:?}"
            );
        }
        assert_eq!(
            throttle_delay(&throttled(429, &[("Retry-After", "999999999")])),
            Some(MAX_THROTTLE_PAUSE),
            "no header parks the timer for a day"
        );

        let refused = read_json(response(500, r#"{"message":"boom"}"#), url).unwrap_err();
        assert_eq!(
            throttle_delay(&refused),
            None,
            "a fault is a failure, not a pause: {refused:#}"
        );
        assert!(!rejected_credentials(&refused));
    }

    #[test]
    fn a_spent_rate_limit_budget_pauses_the_next_pull_and_keeps_this_one() {
        let now = 1_800_000_000.0;

        assert_eq!(
            rate_limit_pause(
                &headers_of(&[
                    ("X-RateLimit-Remaining", "0"),
                    ("X-RateLimit-Reset", "1800000090"),
                ]),
                now,
            ),
            Some(Duration::from_secs(90)),
            "a spent budget waits for the reset it names"
        );
        assert_eq!(
            rate_limit_pause(
                &headers_of(&[
                    ("X-RateLimit-Remaining", "180"),
                    ("X-RateLimit-Reset", "1800000090"),
                ]),
                now,
            ),
            None,
            "a budget with room to spare asks for nothing"
        );
        assert_eq!(
            rate_limit_pause(&headers_of(&[]), now),
            None,
            "a response that reports no budget asks for nothing"
        );
        assert_eq!(
            rate_limit_pause(
                &headers_of(&[
                    ("X-RateLimit-Remaining", "0"),
                    ("X-RateLimit-Reset", "1799999990"),
                ]),
                now,
            ),
            None,
            "a reset already behind us is a budget already back"
        );
        assert_eq!(
            rate_limit_pause(&headers_of(&[("X-RateLimit-Remaining", "0")]), now),
            Some(DEFAULT_RETRY_AFTER),
            "spent, with no reset named, is the default wait"
        );

        assert_eq!(
            read_json(
                response_with(200, r#"{"count":1}"#, &[("X-RateLimit-Remaining", "0")]),
                "https://dev.azure.com/demo/_apis/wit/wiql",
            )
            .unwrap(),
            json!({"count": 1}),
            "the data still applies; only the next pull is held back"
        );
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
    fn creating_a_work_item_posts_its_fields_and_hangs_it_under_its_parent() {
        let fields = [
            crate::edit::set_field(crate::edit::TITLE_FIELD, "Edit dispatcher"),
            crate::edit::set_field(crate::edit::PRIORITY_FIELD, 2),
        ];

        let alone = create_document(&fields, None, &config());
        assert_eq!(
            alone,
            fields.to_vec(),
            "a work item with no parent is its fields and nothing else"
        );

        let parented = create_document(&fields, Some(613), &config());
        assert_eq!(&parented[..2], &fields[..], "the fields lead, in order");
        assert_eq!(
            parented[2],
            json!({
                "op": "add",
                "path": "/relations/-",
                "value": {
                    "rel": "System.LinkTypes.Hierarchy-Reverse",
                    "url": "https://dev.azure.com/demo/_apis/wit/workItems/613",
                },
            }),
            "the parent travels as a link on the organization, not as a field"
        );
        let (_, relations) = parse_work_item(
            &json!({
                "id": 700,
                "rev": 1,
                "fields": {
                    "System.Title": "Edit dispatcher",
                    "System.CreatedDate": "2026-08-29T09:00:00Z",
                    "System.ChangedDate": "2026-08-29T09:00:00Z",
                },
                "relations": [parented[2].get("value").unwrap().clone()],
            }),
            &config(),
        )
        .unwrap();
        assert_eq!(
            relations[0].to.id, 613,
            "the link reads back as the parent it named"
        );
    }

    /// A work item as `$expand=relations` reports it, with the links given.
    fn with_relations(revision: i64, relations: Vec<Value>) -> Value {
        json!({"id": 613, "rev": revision, "fields": {}, "relations": relations})
    }

    fn parent_relation(id: i64) -> Value {
        json!({
            "rel": "System.LinkTypes.Hierarchy-Reverse",
            "url": format!("https://dev.azure.com/demo/_apis/wit/workItems/{id}"),
        })
    }

    fn related_relation(id: i64) -> Value {
        json!({
            "rel": "System.LinkTypes.Related",
            "url": format!("https://dev.azure.com/demo/_apis/wit/workItems/{id}"),
        })
    }

    #[test]
    fn filing_a_work_item_under_its_first_parent_only_appends_a_link() {
        let item = with_relations(4, vec![related_relation(700)]);

        let document = reparent_document(&item, Some(11), &config()).unwrap();

        assert_eq!(
            document,
            vec![
                json!({"op": "test", "path": "/rev", "value": 4}),
                json!({
                    "op": "add",
                    "path": "/relations/-",
                    "value": {
                        "rel": "System.LinkTypes.Hierarchy-Reverse",
                        "url": "https://dev.azure.com/demo/_apis/wit/workItems/11",
                    },
                }),
            ],
            "with no parent held there is nothing to remove, and the related link is left alone"
        );
    }

    #[test]
    fn moving_a_work_item_between_parents_removes_by_index_then_appends_in_one_document() {
        let item = with_relations(
            9,
            vec![
                related_relation(700),
                parent_relation(11),
                related_relation(800),
            ],
        );

        let document = reparent_document(&item, Some(22), &config()).unwrap();

        assert_eq!(
            document,
            vec![
                json!({"op": "test", "path": "/rev", "value": 9}),
                json!({"op": "remove", "path": "/relations/1"}),
                json!({
                    "op": "add",
                    "path": "/relations/-",
                    "value": {
                        "rel": "System.LinkTypes.Hierarchy-Reverse",
                        "url": "https://dev.azure.com/demo/_apis/wit/workItems/22",
                    },
                }),
            ],
            "the revision test leads, the removal names the index the link sits at in the copy \
             just read, and the append comes last so it cannot shift that index"
        );
    }

    #[test]
    fn detaching_a_work_item_removes_its_parent_link_and_adds_nothing() {
        let item = with_relations(2, vec![parent_relation(11), related_relation(700)]);

        let document = reparent_document(&item, None, &config()).unwrap();

        assert_eq!(
            document,
            vec![
                json!({"op": "test", "path": "/rev", "value": 2}),
                json!({"op": "remove", "path": "/relations/0"}),
            ],
            "nothing is appended for a work item that is to hang under nothing"
        );
    }

    #[test]
    fn a_work_item_with_no_parent_cannot_have_one_removed() {
        let item = with_relations(1, vec![related_relation(700)]);

        let error =
            reparent_document(&item, None, &config()).expect_err("there is no link to take off");

        assert!(
            format!("{error:#}").contains("no parent to remove"),
            "{error:#}"
        );
    }

    #[test]
    fn several_parent_links_are_removed_from_the_last_one_back() {
        // Azure DevOps allows a work item one parent, so this only happens to
        // data that is already wrong; removing back to front is what keeps the
        // second index from meaning something else once the first is gone.
        let item = with_relations(
            3,
            vec![
                parent_relation(11),
                related_relation(700),
                parent_relation(12),
            ],
        );

        let document = reparent_document(&item, None, &config()).unwrap();

        assert_eq!(
            document,
            vec![
                json!({"op": "test", "path": "/rev", "value": 3}),
                json!({"op": "remove", "path": "/relations/2"}),
                json!({"op": "remove", "path": "/relations/0"}),
            ]
        );
    }

    #[test]
    fn the_create_url_keeps_the_dollar_before_an_escaped_work_item_type() {
        assert_eq!(
            client(config()).create_work_item_url("Issue").unwrap(),
            "https://dev.azure.com/demo/development/_apis/wit/workitems/$Issue\
             ?$expand=relations&api-version=7.1"
        );
        let spaced = client(AzureConfig {
            project: "Fabrikam Fiber".into(),
            ..config()
        });
        assert_eq!(
            spaced.create_work_item_url("User Story").unwrap(),
            "https://dev.azure.com/demo/Fabrikam%20Fiber/_apis/wit/workitems/$User%20Story\
             ?$expand=relations&api-version=7.1",
            "a space is escaped either side of the type; the dollar is not"
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
