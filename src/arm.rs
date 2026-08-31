//! Azure Resource Manager: the subscription the registry and vault tabs read,
//! the tokens each of their planes wants, and the client that carries them.
//!
//! None of this belongs in [`crate::azure`]. Azure DevOps is addressed by
//! organization and project and signed with a token minted for its own
//! audience; a container registry and a key vault live under a subscription,
//! are signed with a token minted for `management.azure.com`, and each has a
//! data plane on its own host that wants a third token besides. The one thing
//! they share is the Azure CLI's login, and even that differs: a personal
//! access token opens Azure DevOps and nothing else, so a PAT-only run is
//! ARM-offline and every refusal here says `az login`.
//!
//! Nothing here touches SQLite. What a subscription holds is read live, the
//! way a cluster's pods are: it is not the project's business, and it changes
//! without anyone editing a work item.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use time::format_description::FormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, PrimitiveDateTime};

use crate::timestamp::Timestamp;

/// The audience an ARM token is minted for. The trailing slash is part of it.
const ARM_RESOURCE: &str = "https://management.azure.com/";
/// The audience a Key Vault data-plane token is minted for.
const VAULT_RESOURCE: &str = "https://vault.azure.net";
/// Everything a subscription holds is read through one query rather than one
/// list call per provider, which is the whole reason Resource Graph exists.
const RESOURCE_GRAPH_URL: &str = "https://management.azure.com/providers/Microsoft.ResourceGraph/resources?api-version=2021-03-01";
/// The two resource types the tabs know, with the fields each needs projected
/// under one name. `sku` sits in different places for the two providers and
/// `coalesce` picks whichever is there.
const INVENTORY_QUERY: &str = r#"Resources | where type in ("microsoft.containerregistry/registries", "microsoft.keyvault/vaults") | project id, name, type, resourceGroup, location, sku = tostring(coalesce(sku.name, properties.sku.name)), loginServer = tostring(properties.loginServer), vaultUri = tostring(properties.vaultUri)"#;
const REGISTRY_TYPE: &str = "microsoft.containerregistry/registries";
const VAULT_TYPE: &str = "microsoft.keyvault/vaults";
/// The Key Vault data-plane version every listing is asked at.
const VAULT_API_VERSION: &str = "7.4";
/// The scope a catalog listing is signed for.
const CATALOG_SCOPE: &str = "registry:catalog:*";
/// How many entries a paged data-plane listing asks for at once. A short page
/// is how the end of the listing announces itself.
const PAGE_SIZE: usize = 100;
/// How long a throttled request waits when ARM refuses one without saying how
/// long to leave it.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(30);
/// The longest wait one header may ask for, so a date read out of a clock that
/// disagrees with ours cannot park a tab for days.
const MAX_THROTTLE_PAUSE: Duration = Duration::from_secs(3600);
/// The shortest wait worth reporting: a header that works out at nothing is
/// still a refusal, and asking again in the same breath is what makes it worse.
const MIN_THROTTLE_PAUSE: Duration = Duration::from_secs(1);
/// The statuses ARM and its data planes shed load with.
const THROTTLED_STATUSES: [u16; 2] = [429, 503];
const BODY_LIMIT: u64 = 32 * 1024 * 1024;
/// `Retry-After` in its other form: an IMF-fixdate, always in GMT.
const HTTP_DATE: &[FormatItem<'static>] = format_description!(
    "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
);

/// Which subscription the registry and vault tabs read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArmConfig {
    /// The subscription id, as a GUID.
    pub subscription: String,
}

impl ArmConfig {
    /// The subscription from the flag, then from `TICKET_TUI_SUBSCRIPTION` —
    /// which the caller reads and hands over — then from whichever one the
    /// Azure CLI is set to.
    pub fn resolve(flag: Option<String>, env: Option<String>) -> Result<Self> {
        Self::resolve_with(flag, env, az_subscription)
    }

    /// [`ArmConfig::resolve`] with the CLI step handed in, so the order can be
    /// tested without a shell.
    pub fn resolve_with(
        flag: Option<String>,
        env: Option<String>,
        az: impl FnOnce() -> Result<String>,
    ) -> Result<Self> {
        let subscription = named(flag)
            .or_else(|| named(env))
            .or_else(|| az().ok().and_then(|found| named(Some(found))));
        match subscription {
            Some(subscription) => Ok(Self { subscription }),
            None => bail!(
                "no Azure subscription: pass --subscription, set TICKET_TUI_SUBSCRIPTION, or run `az account set`"
            ),
        }
    }
}

/// One setting that was actually set: trimmed, and blank is not an answer.
fn named(raw: Option<String>) -> Option<String> {
    raw.map(|held| held.trim().to_owned())
        .filter(|held| !held.is_empty())
}

/// The subscription the Azure CLI is currently set to.
fn az_subscription() -> Result<String> {
    az(&["account", "show", "--query", "id", "-o", "tsv"])
}

/// A token for one audience, borrowed from the Azure CLI's login. There is no
/// personal-access-token path: a PAT is an Azure DevOps credential and ARM has
/// never accepted one.
pub fn arm_token() -> Result<String> {
    token_for(ARM_RESOURCE)
}

/// A token for the Key Vault data plane, which has an audience of its own.
pub fn vault_token() -> Result<String> {
    token_for(VAULT_RESOURCE)
}

fn token_for(resource: &str) -> Result<String> {
    az(&[
        "account",
        "get-access-token",
        "--resource",
        resource,
        "--query",
        "accessToken",
        "-o",
        "tsv",
    ])
}

/// One `az` call, run without a terminal to talk to, and whatever single value
/// it printed. A failure says to sign in, because that is what it nearly
/// always is.
fn az(arguments: &[&str]) -> Result<String> {
    let output = Command::new("az")
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .context("failed to run `az`; install the Azure CLI and run `az login`")?;
    if !output.status.success() {
        bail!(
            "`az {}` failed; run `az login`: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if value.is_empty() {
        bail!(
            "`az {}` answered with nothing; run `az login`",
            arguments.join(" ")
        );
    }
    Ok(value)
}

/// A registry's data-plane token, which ARM does not mint: the registry itself
/// trades an ARM token for a refresh token, and the refresh token for an
/// access token scoped to the one thing about to be read.
pub fn acr_token(client: &ArmClient, login_server: &str, scope: &str) -> Result<String> {
    let arm = client.arm_bearer()?;
    let exchanged = client.request(
        &format!("https://{login_server}/oauth2/exchange"),
        None,
        Some(&Body::Form(vec![
            ("grant_type".to_owned(), "access_token".to_owned()),
            ("service".to_owned(), login_server.to_owned()),
            ("access_token".to_owned(), arm),
        ])),
    )?;
    let refresh = text(&exchanged["refresh_token"])
        .with_context(|| format!("{login_server} did not answer the exchange with a token"))?;
    let issued = client.request(
        &format!("https://{login_server}/oauth2/token"),
        None,
        Some(&Body::Form(vec![
            ("grant_type".to_owned(), "refresh_token".to_owned()),
            ("service".to_owned(), login_server.to_owned()),
            ("scope".to_owned(), scope.to_owned()),
            ("refresh_token".to_owned(), refresh),
        ])),
    )?;
    text(&issued["access_token"])
        .with_context(|| format!("{login_server} did not answer with an access token for {scope}"))
}

/// What one repository's own calls are signed for.
fn metadata_scope(repository: &str) -> String {
    format!("repository:{repository}:metadata_read")
}

/// One container registry, as the subscription lists it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Registry {
    /// The full ARM resource id, which is also what the portal link is built
    /// from.
    pub id: String,
    pub name: String,
    pub resource_group: String,
    pub location: String,
    pub sku: String,
    /// The data-plane host, `myacr.azurecr.io`.
    pub login_server: String,
}

/// One repository in a registry. The counts and the stamp are `None` until the
/// attributes call fills them in: a catalog listing is names and nothing else.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Repository {
    pub name: String,
    pub tags: Option<u64>,
    pub manifests: Option<u64>,
    pub updated: Option<Timestamp>,
}

/// One tag, and the manifest it points at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tag {
    pub name: String,
    pub digest: String,
    pub created: Option<Timestamp>,
    pub updated: Option<Timestamp>,
}

/// One manifest, by what it weighs and what it runs on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Manifest {
    pub digest: String,
    /// Bytes, as the registry counts them.
    pub size: Option<u64>,
    pub created: Option<Timestamp>,
    pub architecture: String,
    pub os: String,
}

/// One key vault, as the subscription lists it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Vault {
    pub id: String,
    pub name: String,
    pub resource_group: String,
    pub location: String,
    pub sku: String,
    /// The data-plane host, `https://myvault.vault.azure.net/`.
    pub uri: String,
}

/// The three things a vault holds, which are listed the same way and shown in
/// one list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemKind {
    Secret,
    Key,
    Certificate,
}

impl ItemKind {
    /// Every kind, in the order a vault's items are read and shown.
    pub const ALL: [Self; 3] = [Self::Secret, Self::Key, Self::Certificate];

    /// What a row calls it, and what a filter matches on.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Key => "key",
            Self::Certificate => "cert",
        }
    }

    /// The path segment the data plane lists it under.
    const fn listing(self) -> &'static str {
        match self {
            Self::Secret => "secrets",
            Self::Key => "keys",
            Self::Certificate => "certificates",
        }
    }
}

/// One thing a vault holds, as its listing describes it. A listing never
/// carries the value, which is the point of listing one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultItem {
    pub kind: ItemKind,
    pub name: String,
    pub enabled: bool,
    pub created: Option<Timestamp>,
    pub updated: Option<Timestamp>,
    pub expires: Option<Timestamp>,
    pub content_type: Option<String>,
    pub recovery_level: Option<String>,
}

/// A secret's value, which is read out of a vault only when someone asks for
/// it in as many words.
///
/// Neither `Debug` nor `Display` will print it, so it cannot reach a log line,
/// an error, or a panic message by accident; [`Secret::expose`] is the one way
/// to read it, and it is meant to be conspicuous at the call site.
#[derive(Clone, Eq, PartialEq)]
pub struct Secret(String);

impl Secret {
    /// The value itself, for the one place that is about to show or copy it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

/// Everything one subscription holds that these tabs know about.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Inventory {
    pub registries: Vec<Registry>,
    pub vaults: Vec<Vault>,
}

/// Where the portal shows one resource, for the key that opens it in a
/// browser.
#[must_use]
pub fn portal_url(id: &str) -> String {
    format!("https://portal.azure.com/#resource{id}")
}

/// One request on its way out. Everything ARM and its data planes are asked
/// here is either a GET or a POST carrying one body, so the body is what says
/// which.
#[derive(Clone, Debug)]
pub struct PreparedRequest {
    pub url: String,
    /// The token to sign with, when the call is signed at all: the two token
    /// endpoints are how a token is got, so they carry none.
    pub bearer: Option<String>,
    pub body: Option<Body>,
}

/// What a POST carries: a document, or the form fields a token endpoint wants.
#[derive(Clone, Debug)]
pub enum Body {
    Json(Value),
    Form(Vec<(String, String)>),
}

/// What came back, in the only three parts this client reads. `Retry-After` is
/// the one header worth carrying: nothing here looks at another.
#[derive(Clone, Debug)]
pub struct TransportResponse {
    pub status: u16,
    pub retry_after: Option<String>,
    pub body: String,
}

/// How the client reaches the outside world: the network, and the CLI it
/// borrows tokens from. One seam, so a test drives the client with canned
/// answers and canned tokens rather than a socket and a shell.
pub trait Transport: Send {
    fn send(&self, request: PreparedRequest) -> Result<TransportResponse>;

    /// A freshly minted ARM token.
    fn arm_token(&self) -> Result<String> {
        arm_token()
    }

    /// A freshly minted Key Vault data-plane token.
    fn vault_token(&self) -> Result<String> {
        vault_token()
    }
}

/// The real thing: one agent, configured the way the Azure DevOps one is.
/// Redirects are not followed — a hop would drop the `Authorization` header
/// and trade a status this code can read for a page of markup it cannot.
struct Https {
    agent: ureq::Agent,
}

impl Https {
    fn new() -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .max_redirects(0)
                .timeout_global(Some(Duration::from_secs(90)))
                .build()
                .into(),
        }
    }
}

impl Transport for Https {
    fn send(&self, request: PreparedRequest) -> Result<TransportResponse> {
        let bearer = request
            .bearer
            .as_ref()
            .map(|token| format!("Bearer {token}"));
        let mut response = match &request.body {
            None => {
                let mut builder = self.agent.get(&request.url);
                if let Some(bearer) = &bearer {
                    builder = builder.header("Authorization", bearer);
                }
                builder.call()
            }
            Some(Body::Json(document)) => {
                let mut builder = self.agent.post(&request.url);
                if let Some(bearer) = &bearer {
                    builder = builder.header("Authorization", bearer);
                }
                builder.send_json(document)
            }
            Some(Body::Form(fields)) => {
                let mut builder = self
                    .agent
                    .post(&request.url)
                    .header("Content-Type", "application/x-www-form-urlencoded");
                if let Some(bearer) = &bearer {
                    builder = builder.header("Authorization", bearer);
                }
                builder.send(query(fields))
            }
        }
        .with_context(|| format!("the request to {} failed", request.url))?;
        let status = response.status().as_u16();
        // Read before the body, which takes the response apart.
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response
            .body_mut()
            .with_config()
            .limit(BODY_LIMIT)
            .read_to_string()
            .with_context(|| format!("failed to read the response body from {}", request.url))?;
        Ok(TransportResponse {
            status,
            retry_after,
            body,
        })
    }
}

/// The credentials are spent rather than the request being wrong, which is
/// worth one fresh token before it is worth reporting.
#[derive(Debug)]
struct SignedOut(String);

impl fmt::Display for SignedOut {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SignedOut {}

/// A client for one subscription: the agent every call goes out on, and the
/// ARM token they are all signed with.
pub struct ArmClient {
    transport: Box<dyn Transport>,
    subscription: String,
    /// Minted on the first call that needs it and re-minted once when ARM says
    /// it is spent: a CLI token lasts about an hour and a running TUI outlives
    /// that.
    token: RefCell<Option<String>>,
    /// How long the last refusal asked to be left alone, until something reads
    /// it.
    throttled: Cell<Option<Duration>>,
}

impl ArmClient {
    /// A client over the real network. Nothing is minted until something is
    /// read, so building one cannot fail and an unopened tab costs no `az`.
    #[must_use]
    pub fn new(config: ArmConfig) -> Self {
        Self::with_transport(config, Box::new(Https::new()))
    }

    #[must_use]
    pub fn with_transport(config: ArmConfig, transport: Box<dyn Transport>) -> Self {
        Self {
            transport,
            subscription: config.subscription,
            token: RefCell::new(None),
            throttled: Cell::new(None),
        }
    }

    #[must_use]
    pub fn subscription(&self) -> &str {
        &self.subscription
    }

    /// How long the refusals since the last time this was asked want to be
    /// left alone. Reading it clears it, so one refusal delays one read rather
    /// than every read after it.
    pub fn throttled_for(&self) -> Option<Duration> {
        self.throttled.take()
    }

    /// The ARM token, minted the first time one is wanted.
    fn arm_bearer(&self) -> Result<String> {
        if let Some(held) = self.token.borrow().clone() {
            return Ok(held);
        }
        let minted = self.transport.arm_token()?;
        *self.token.borrow_mut() = Some(minted.clone());
        Ok(minted)
    }

    /// One ARM call, retried once with a freshly minted token when ARM says
    /// the current one is spent. A failed mint reports the original refusal,
    /// which is the one that says to sign in.
    fn arm_request(&self, url: &str, body: Option<&Body>) -> Result<Value> {
        let bearer = self.arm_bearer()?;
        match self.request(url, Some(&bearer), body) {
            Err(error) if error.downcast_ref::<SignedOut>().is_some() => {
                match self.transport.arm_token() {
                    Ok(minted) => {
                        *self.token.borrow_mut() = Some(minted.clone());
                        self.request(url, Some(&minted), body)
                    }
                    Err(_) => Err(error),
                }
            }
            result => result,
        }
    }

    /// One call, and what it answered with. Throttling first: it is the one
    /// refusal that is nobody's fault and only worth waiting out.
    fn request(&self, url: &str, bearer: Option<&str>, body: Option<&Body>) -> Result<Value> {
        let response = self.transport.send(PreparedRequest {
            url: url.to_owned(),
            bearer: bearer.map(str::to_owned),
            body: body.cloned(),
        })?;
        let status = response.status;
        if THROTTLED_STATUSES.contains(&status) {
            let wait = retry_after(response.retry_after.as_deref(), OffsetDateTime::now_utc());
            self.note_throttle(wait);
            bail!(
                "Azure is throttling requests (HTTP {status}) for {url}; try again in {}s: {}",
                wait.as_secs(),
                failure_message(&response.body)
            );
        }
        if status == 401 || status == 403 {
            return Err(anyhow::Error::new(SignedOut(format!(
                "Azure rejected the credentials ({status}) for {url}; run `az login` and retry: {}",
                failure_message(&response.body)
            ))));
        }
        if !(200..300).contains(&status) {
            bail!(
                "{url} answered {status}: {}",
                failure_message(&response.body)
            );
        }
        if response.body.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&response.body)
            .with_context(|| format!("{url} answered with something other than JSON"))
    }

    /// One read makes several calls; the longest wait any of them asked for is
    /// the one that has to be honoured.
    fn note_throttle(&self, wait: Duration) {
        if self.throttled.get().is_none_or(|held| wait > held) {
            self.throttled.set(Some(wait));
        }
    }

    /// One registry data-plane call, signed for the one thing it reads.
    fn acr(&self, registry: &Registry, scope: &str, path: &str) -> Result<Value> {
        let bearer = acr_token(self, &registry.login_server, scope)?;
        let url = format!("https://{}/acr/v1/{path}", registry.login_server);
        self.request(&url, Some(&bearer), None)
    }
}

/// What a registry or a vault answers with when it refuses: ARM writes the
/// reason under `error.message`, a registry under `errors[0].message`, and
/// anything else is worth the front of its body rather than nothing.
fn failure_message(text: &str) -> String {
    let parsed = serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
    for candidate in [
        &parsed["error"]["message"],
        &parsed["errors"][0]["message"],
        &parsed["message"],
    ] {
        if let Some(said) = candidate.as_str() {
            return said.to_owned();
        }
    }
    text.trim().chars().take(200).collect()
}

/// The wait a throttled response asks for: `Retry-After` as whole seconds, or
/// as a date to count forward to. Never less than a second, never more than
/// [`MAX_THROTTLE_PAUSE`], and [`DEFAULT_RETRY_AFTER`] when the header is
/// absent or is something this cannot read.
fn retry_after(header: Option<&str>, now: OffsetDateTime) -> Duration {
    let Some(raw) = header.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return DEFAULT_RETRY_AFTER;
    };
    let seconds = match raw.parse::<f64>() {
        Ok(seconds) => seconds,
        Err(_) => match PrimitiveDateTime::parse(raw, HTTP_DATE) {
            Ok(when) => (when.assume_utc() - now).as_seconds_f64(),
            Err(_) => return DEFAULT_RETRY_AFTER,
        },
    };
    Duration::from_secs_f64(seconds.clamp(
        MIN_THROTTLE_PAUSE.as_secs_f64(),
        MAX_THROTTLE_PAUSE.as_secs_f64(),
    ))
}

/// A query string, or the body of a form post: the same encoding either way.
fn query(pairs: &[(String, String)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

/// One string that was actually there: trimmed, and blank counts as absent.
fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|held| !held.is_empty())
        .map(str::to_owned)
}

fn string(value: &Value) -> String {
    text(value).unwrap_or_default()
}

/// A count, whichever way it was written: registries send these as numbers,
/// but a string of digits is the same answer.
fn count(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}

/// An RFC 3339 stamp, as the registry data plane writes them.
fn stamp(value: &Value) -> Option<Timestamp> {
    Timestamp::parse(&text(value)?).ok()
}

/// A unix second, as a vault writes its attributes.
fn unix(value: &Value) -> Option<Timestamp> {
    OffsetDateTime::from_unix_timestamp(value.as_i64()?)
        .ok()
        .map(Timestamp::from_offset_date_time)
}

fn registry(row: &Value) -> Registry {
    Registry {
        id: string(&row["id"]),
        name: string(&row["name"]),
        resource_group: string(&row["resourceGroup"]),
        location: string(&row["location"]),
        sku: string(&row["sku"]),
        login_server: string(&row["loginServer"]),
    }
}

fn vault(row: &Value) -> Vault {
    Vault {
        id: string(&row["id"]),
        name: string(&row["name"]),
        resource_group: string(&row["resourceGroup"]),
        location: string(&row["location"]),
        sku: string(&row["sku"]),
        uri: string(&row["vaultUri"]),
    }
}

fn tag(entry: &Value) -> Option<Tag> {
    Some(Tag {
        name: text(&entry["name"])?,
        digest: string(&entry["digest"]),
        created: stamp(&entry["createdTime"]),
        updated: stamp(&entry["lastUpdateTime"]),
    })
}

/// One listed vault item. The name is the last segment of its id, which is the
/// only place a listing puts it.
fn vault_item(kind: ItemKind, entry: &Value) -> Option<VaultItem> {
    let id = text(&entry["id"])?;
    let name = id.rsplit('/').next().filter(|name| !name.is_empty())?;
    let attributes = &entry["attributes"];
    Some(VaultItem {
        kind,
        name: name.to_owned(),
        // A vault only says so when an item is disabled, and an item with no
        // attributes at all is a usable one.
        enabled: attributes["enabled"].as_bool().unwrap_or(true),
        created: unix(&attributes["created"]),
        updated: unix(&attributes["updated"]),
        expires: unix(&attributes["exp"]),
        content_type: text(&entry["contentType"]),
        recovery_level: text(&attributes["recoveryLevel"]),
    })
}

/// What the registry and vault tabs read. Read-only, like the watcher's
/// source: nothing here changes anything in Azure, and the one call that reads
/// a secret's value is driven by a keystroke rather than by a poll.
pub trait ArmSource: Send {
    /// Every registry and vault the subscription holds.
    fn inventory(&self) -> Result<Inventory>;

    /// Every repository in one registry, by name.
    fn repositories(&self, _registry: &Registry) -> Result<Vec<Repository>> {
        Ok(Vec::new())
    }

    /// One repository's counts and stamps, which the catalog listing does not
    /// carry.
    fn repository(&self, _registry: &Registry, name: &str) -> Result<Repository> {
        Ok(Repository {
            name: name.to_owned(),
            tags: None,
            manifests: None,
            updated: None,
        })
    }

    /// One repository's tags, newest first.
    fn tags(&self, _registry: &Registry, _repo: &str) -> Result<Vec<Tag>> {
        Ok(Vec::new())
    }

    /// What one tag points at.
    fn manifest(&self, _registry: &Registry, _repo: &str, digest: &str) -> Result<Manifest> {
        Ok(Manifest {
            digest: digest.to_owned(),
            size: None,
            created: None,
            architecture: String::new(),
            os: String::new(),
        })
    }

    /// Everything one vault holds: its secrets, its keys and its certificates,
    /// in that order.
    fn items(&self, _vault: &Vault) -> Result<Vec<VaultItem>> {
        Ok(Vec::new())
    }

    /// One secret's value. Only ever called for an explicit reveal — never by
    /// a listing, never by a poll — because reading one is audited and is the
    /// one call here that hands back something worth hiding.
    fn secret_value(&self, _vault: &Vault, _name: &str) -> Result<Secret> {
        Ok(Secret(String::new()))
    }

    /// How long the reads since this was last asked want to be left alone.
    /// Reading it clears it.
    fn throttled_for(&self) -> Option<Duration> {
        None
    }
}

impl ArmSource for ArmClient {
    fn inventory(&self) -> Result<Inventory> {
        let mut inventory = Inventory::default();
        let mut skip: Option<String> = None;
        loop {
            let mut body = json!({
                "subscriptions": [self.subscription],
                "query": INVENTORY_QUERY,
            });
            if let Some(token) = &skip {
                body["options"] = json!({ "$skipToken": token });
            }
            let answer = self.arm_request(RESOURCE_GRAPH_URL, Some(&Body::Json(body)))?;
            for row in answer["data"].as_array().into_iter().flatten() {
                match string(&row["type"]).to_ascii_lowercase().as_str() {
                    REGISTRY_TYPE => inventory.registries.push(registry(row)),
                    VAULT_TYPE => inventory.vaults.push(vault(row)),
                    _ => {}
                }
            }
            let next = text(&answer["$skipToken"]);
            // A repeated token would page for ever; absent or unchanged is the
            // end of the answer either way.
            if next.is_none() || next == skip {
                return Ok(inventory);
            }
            skip = next;
        }
    }

    fn repositories(&self, registry: &Registry) -> Result<Vec<Repository>> {
        let bearer = acr_token(self, &registry.login_server, CATALOG_SCOPE)?;
        let mut repositories = Vec::new();
        let mut last: Option<String> = None;
        loop {
            let mut pairs = vec![("n".to_owned(), PAGE_SIZE.to_string())];
            if let Some(held) = &last {
                pairs.push(("last".to_owned(), held.clone()));
            }
            let url = format!(
                "https://{}/acr/v1/_catalog?{}",
                registry.login_server,
                query(&pairs)
            );
            let page = self.request(&url, Some(&bearer), None)?;
            let listed = page["repositories"].as_array().cloned().unwrap_or_default();
            let asked_for_more = listed.len() >= PAGE_SIZE;
            let previous = last.clone();
            for name in listed.iter().filter_map(text) {
                last = Some(name.clone());
                repositories.push(Repository {
                    name,
                    tags: None,
                    manifests: None,
                    updated: None,
                });
            }
            // A page that did not move the cursor on would be asked for again
            // for ever, whatever it claimed to be full of.
            if !asked_for_more || last == previous {
                return Ok(repositories);
            }
        }
    }

    fn repository(&self, registry: &Registry, name: &str) -> Result<Repository> {
        let answer = self.acr(registry, &metadata_scope(name), name)?;
        Ok(Repository {
            name: text(&answer["imageName"]).unwrap_or_else(|| name.to_owned()),
            tags: count(&answer["tagCount"]),
            manifests: count(&answer["manifestCount"]),
            updated: stamp(&answer["lastUpdateTime"]),
        })
    }

    fn tags(&self, registry: &Registry, repo: &str) -> Result<Vec<Tag>> {
        let bearer = acr_token(self, &registry.login_server, &metadata_scope(repo))?;
        let mut tags: Vec<Tag> = Vec::new();
        let mut last: Option<String> = None;
        loop {
            let mut pairs = vec![
                ("n".to_owned(), PAGE_SIZE.to_string()),
                ("orderby".to_owned(), "timedesc".to_owned()),
            ];
            if let Some(held) = &last {
                pairs.push(("last".to_owned(), held.clone()));
            }
            let url = format!(
                "https://{}/acr/v1/{repo}/_tags?{}",
                registry.login_server,
                query(&pairs)
            );
            let page = self.request(&url, Some(&bearer), None)?;
            let listed = page["tags"].as_array().cloned().unwrap_or_default();
            let asked_for_more = listed.len() >= PAGE_SIZE;
            let previous = last.clone();
            tags.extend(listed.iter().filter_map(tag));
            last = tags.last().map(|held| held.name.clone());
            // As above: a page that did not move the cursor on ends the
            // listing, whatever it claimed to be full of.
            if !asked_for_more || last == previous {
                return Ok(tags);
            }
        }
    }

    fn manifest(&self, registry: &Registry, repo: &str, digest: &str) -> Result<Manifest> {
        let answer = self.acr(
            registry,
            &metadata_scope(repo),
            &format!("{repo}/_manifests/{digest}"),
        )?;
        // A registry wraps the manifest in a `manifest` object; the same
        // fields sit at the top level when it does not.
        let held = if answer["manifest"].is_object() {
            &answer["manifest"]
        } else {
            &answer
        };
        Ok(Manifest {
            digest: text(&held["digest"]).unwrap_or_else(|| digest.to_owned()),
            size: count(&held["imageSize"]),
            created: stamp(&held["createdTime"]),
            architecture: string(&held["architecture"]),
            os: string(&held["os"]),
        })
    }

    fn items(&self, vault: &Vault) -> Result<Vec<VaultItem>> {
        let bearer = self.transport.vault_token()?;
        let mut items = Vec::new();
        for kind in ItemKind::ALL {
            let mut url = Some(format!(
                "{}/{}?api-version={VAULT_API_VERSION}",
                vault.uri.trim_end_matches('/'),
                kind.listing()
            ));
            // The listing hands back the next page's whole address, so it is
            // followed rather than built.
            while let Some(next) = url {
                let page = self.request(&next, Some(&bearer), None)?;
                items.extend(
                    page["value"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|entry| vault_item(kind, entry)),
                );
                url = text(&page["nextLink"]);
            }
        }
        Ok(items)
    }

    fn secret_value(&self, vault: &Vault, name: &str) -> Result<Secret> {
        let bearer = self.transport.vault_token()?;
        let url = format!(
            "{}/secrets/{name}?api-version={VAULT_API_VERSION}",
            vault.uri.trim_end_matches('/')
        );
        let answer = self.request(&url, Some(&bearer), None)?;
        Ok(Secret(
            answer["value"]
                .as_str()
                .with_context(|| format!("{} did not answer with a value for {name}", vault.name))?
                .to_owned(),
        ))
    }

    fn throttled_for(&self) -> Option<Duration> {
        Self::throttled_for(self)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use anyhow::anyhow;
    use time::macros::datetime;

    use super::*;
    use crate::timestamp::ts;

    /// One canned answer: a status, a `Retry-After`, and a body.
    type Answer = (u16, Option<&'static str>, String);

    /// A transport over canned answers, counting the tokens it minted and
    /// keeping every request it was handed.
    #[derive(Clone, Default)]
    struct FakeHttp {
        answers: Arc<Mutex<VecDeque<Answer>>>,
        sent: Arc<Mutex<Vec<PreparedRequest>>>,
        mints: Arc<Mutex<usize>>,
    }

    impl FakeHttp {
        fn answering(answers: &[Answer]) -> Self {
            Self {
                answers: Arc::new(Mutex::new(answers.iter().cloned().collect())),
                ..Self::default()
            }
        }

        fn ok(bodies: &[Value]) -> Self {
            let answers: Vec<Answer> = bodies
                .iter()
                .map(|body| (200, None, body.to_string()))
                .collect();
            Self::answering(&answers)
        }

        fn sent(&self) -> Vec<PreparedRequest> {
            self.sent.lock().unwrap().clone()
        }

        fn urls(&self) -> Vec<String> {
            self.sent().into_iter().map(|held| held.url).collect()
        }

        fn mints(&self) -> usize {
            *self.mints.lock().unwrap()
        }
    }

    impl Transport for FakeHttp {
        fn send(&self, request: PreparedRequest) -> Result<TransportResponse> {
            self.sent.lock().unwrap().push(request.clone());
            let answer = self.answers.lock().unwrap().pop_front();
            let (status, retry_after, body) =
                answer.ok_or_else(|| anyhow!("nothing canned for {}", request.url))?;
            Ok(TransportResponse {
                status,
                retry_after: retry_after.map(str::to_owned),
                body,
            })
        }

        fn arm_token(&self) -> Result<String> {
            let mut mints = self.mints.lock().unwrap();
            *mints += 1;
            Ok(format!("arm-token-{mints}"))
        }

        fn vault_token(&self) -> Result<String> {
            Ok("vault-token".to_owned())
        }
    }

    fn client(transport: &FakeHttp) -> ArmClient {
        ArmClient::with_transport(
            ArmConfig {
                subscription: "sub-1".to_owned(),
            },
            Box::new(transport.clone()),
        )
    }

    /// The two token answers every registry call starts with.
    fn acr_handshake() -> Vec<Answer> {
        vec![
            (
                200,
                None,
                json!({ "refresh_token": "refresh-1" }).to_string(),
            ),
            (200, None, json!({ "access_token": "acr-1" }).to_string()),
        ]
    }

    fn registry_of(login_server: &str) -> Registry {
        Registry {
            id: "/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.ContainerRegistry/registries/acr".to_owned(),
            name: "acr".to_owned(),
            resource_group: "rg".to_owned(),
            location: "westeurope".to_owned(),
            sku: "Premium".to_owned(),
            login_server: login_server.to_owned(),
        }
    }

    fn vault_of(uri: &str) -> Vault {
        Vault {
            id: "/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.KeyVault/vaults/kv"
                .to_owned(),
            name: "kv".to_owned(),
            resource_group: "rg".to_owned(),
            location: "westeurope".to_owned(),
            sku: "standard".to_owned(),
            uri: uri.to_owned(),
        }
    }

    /// The form fields one request carried, for a request that carried a form.
    fn form(request: &PreparedRequest) -> Vec<(String, String)> {
        match &request.body {
            Some(Body::Form(fields)) => fields.clone(),
            other => panic!("expected a form body, got {other:?}"),
        }
    }

    #[test]
    fn the_subscription_comes_from_the_flag_then_the_variable_then_az_and_the_refusal_names_all_three()
     {
        let flagged = ArmConfig::resolve_with(
            Some("  from-flag  ".to_owned()),
            Some("from-env".to_owned()),
            || Ok("from-az".to_owned()),
        )
        .unwrap();
        assert_eq!(flagged.subscription, "from-flag");

        let from_env = ArmConfig::resolve_with(None, Some("from-env".to_owned()), || {
            Ok("from-az".to_owned())
        })
        .unwrap();
        assert_eq!(from_env.subscription, "from-env");

        // A flag or a variable set to nothing is not an answer.
        let from_az = ArmConfig::resolve_with(Some("  ".to_owned()), Some(String::new()), || {
            Ok(" from-az\n".to_owned())
        })
        .unwrap();
        assert_eq!(from_az.subscription, "from-az");

        let refused = ArmConfig::resolve_with(None, None, || Err(anyhow!("az is not signed in")))
            .unwrap_err()
            .to_string();
        assert!(refused.contains("--subscription"), "{refused}");
        assert!(refused.contains("TICKET_TUI_SUBSCRIPTION"), "{refused}");
        assert!(refused.contains("az account set"), "{refused}");
    }

    #[test]
    fn one_resource_graph_answer_fills_registries_and_vaults_and_a_skip_token_is_followed() {
        let transport = FakeHttp::ok(&[
            json!({
                "data": [{
                    "id": "/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.ContainerRegistry/registries/acr",
                    "name": "acr",
                    "type": "microsoft.containerregistry/registries",
                    "resourceGroup": "rg",
                    "location": "westeurope",
                    "sku": "Premium",
                    "loginServer": "acr.azurecr.io",
                    "vaultUri": "",
                }],
                "$skipToken": "page-2",
            }),
            json!({
                "data": [{
                    "id": "/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.KeyVault/vaults/kv",
                    "name": "kv",
                    "type": "microsoft.keyvault/vaults",
                    "resourceGroup": "rg",
                    "location": "northeurope",
                    "sku": "standard",
                    "loginServer": "",
                    "vaultUri": "https://kv.vault.azure.net/",
                }],
            }),
        ]);
        let inventory = client(&transport).inventory().unwrap();

        assert_eq!(
            inventory.registries,
            vec![registry_of("acr.azurecr.io")],
            "the registry row reads its fields from the projection"
        );
        assert_eq!(inventory.vaults.len(), 1);
        let vault = &inventory.vaults[0];
        assert_eq!(vault.name, "kv");
        assert_eq!(vault.location, "northeurope");
        assert_eq!(vault.uri, "https://kv.vault.azure.net/");
        assert_eq!(
            portal_url(&vault.id),
            format!("https://portal.azure.com/#resource{}", vault.id)
        );

        let sent = transport.sent();
        assert_eq!(sent.len(), 2, "the skip token asked for a second page");
        let Some(Body::Json(second)) = &sent[1].body else {
            panic!("the second page was not a JSON post");
        };
        assert_eq!(second["options"]["$skipToken"], json!("page-2"));
        assert_eq!(second["subscriptions"], json!(["sub-1"]));
        assert_eq!(transport.mints(), 1, "one token signed both pages");
    }

    #[test]
    fn the_acr_token_is_exchanged_from_the_arm_token_in_two_steps() {
        let transport = FakeHttp::answering(&acr_handshake());
        let token = acr_token(&client(&transport), "acr.azurecr.io", CATALOG_SCOPE).unwrap();
        assert_eq!(token, "acr-1");

        let sent = transport.sent();
        assert_eq!(
            transport.urls(),
            vec![
                "https://acr.azurecr.io/oauth2/exchange".to_owned(),
                "https://acr.azurecr.io/oauth2/token".to_owned(),
            ]
        );
        assert!(
            sent.iter().all(|held| held.bearer.is_none()),
            "the token endpoints are how a token is got, so they carry none"
        );
        assert_eq!(
            form(&sent[0]),
            vec![
                ("grant_type".to_owned(), "access_token".to_owned()),
                ("service".to_owned(), "acr.azurecr.io".to_owned()),
                ("access_token".to_owned(), "arm-token-1".to_owned()),
            ]
        );
        assert_eq!(
            form(&sent[1]),
            vec![
                ("grant_type".to_owned(), "refresh_token".to_owned()),
                ("service".to_owned(), "acr.azurecr.io".to_owned()),
                ("scope".to_owned(), "registry:catalog:*".to_owned()),
                ("refresh_token".to_owned(), "refresh-1".to_owned()),
            ]
        );
    }

    #[test]
    fn a_429_with_retry_after_is_reported_as_the_wait_to_hold_off() {
        let transport = FakeHttp::answering(&[(
            429,
            Some(" 45 "),
            json!({ "error": { "message": "Too many requests" } }).to_string(),
        )]);
        let arm = client(&transport);
        let refused = arm.inventory().unwrap_err().to_string();
        assert!(refused.contains("45s"), "{refused}");
        assert!(refused.contains("Too many requests"), "{refused}");
        assert_eq!(arm.throttled_for(), Some(Duration::from_secs(45)));
        assert_eq!(arm.throttled_for(), None, "reading it clears it");

        // A service that cannot say how long asks for the default wait.
        let transport = FakeHttp::answering(&[(
            503,
            None,
            json!({ "error": { "message": "Unable to serve the request" } }).to_string(),
        )]);
        let arm = client(&transport);
        let refused = arm.inventory().unwrap_err().to_string();
        assert!(refused.contains("Unable to serve the request"), "{refused}");
        assert_eq!(arm.throttled_for(), Some(DEFAULT_RETRY_AFTER));

        // The other form of the header is a date to count forward to.
        let now = datetime!(2026-10-21 07:27:00 UTC);
        assert_eq!(
            retry_after(Some("Wed, 21 Oct 2026 07:28:00 GMT"), now),
            Duration::from_secs(60)
        );
        assert_eq!(
            retry_after(Some("Wed, 21 Oct 2026 07:00:00 GMT"), now),
            MIN_THROTTLE_PAUSE,
            "a date already past is still a refusal"
        );
        assert_eq!(retry_after(Some("soon"), now), DEFAULT_RETRY_AFTER);
    }

    #[test]
    fn vault_items_read_their_kind_name_enabled_and_expiry_from_the_listing_and_follow_next_link() {
        let vault = vault_of("https://kv.vault.azure.net/");
        let transport = FakeHttp::ok(&[
            json!({
                "value": [{
                    "id": "https://kv.vault.azure.net/secrets/db-password",
                    "contentType": "text/plain",
                    "attributes": {
                        "enabled": true,
                        "created": 1_756_000_000,
                        "updated": 1_756_100_000,
                        "exp": 1_800_000_000,
                        "recoveryLevel": "Recoverable+Purgeable",
                    },
                }],
                "nextLink": "https://kv.vault.azure.net/secrets?api-version=7.4&$skiptoken=next",
            }),
            json!({
                "value": [{
                    "id": "https://kv.vault.azure.net/secrets/retired",
                    "attributes": { "enabled": false },
                }],
                "nextLink": Value::Null,
            }),
            json!({
                "value": [{
                    "id": "https://kv.vault.azure.net/keys/signing",
                    "attributes": { "enabled": true },
                }],
            }),
            json!({
                "value": [{
                    "id": "https://kv.vault.azure.net/certificates/wildcard",
                    "attributes": { "enabled": true, "exp": 1_800_000_000 },
                }],
            }),
        ]);
        let items = client(&transport).items(&vault).unwrap();

        let read: Vec<(&str, &str, bool)> = items
            .iter()
            .map(|item| (item.kind.as_str(), item.name.as_str(), item.enabled))
            .collect();
        assert_eq!(
            read,
            vec![
                ("secret", "db-password", true),
                ("secret", "retired", false),
                ("key", "signing", true),
                ("cert", "wildcard", true),
            ]
        );
        assert_eq!(items[0].content_type.as_deref(), Some("text/plain"));
        assert_eq!(
            items[0].recovery_level.as_deref(),
            Some("Recoverable+Purgeable")
        );
        assert_eq!(items[0].created, unix(&json!(1_756_000_000)));
        assert_eq!(items[0].updated, unix(&json!(1_756_100_000)));
        assert_eq!(items[0].expires, unix(&json!(1_800_000_000)));
        assert_eq!(items[1].expires, None);

        assert_eq!(
            transport.urls(),
            vec![
                "https://kv.vault.azure.net/secrets?api-version=7.4".to_owned(),
                "https://kv.vault.azure.net/secrets?api-version=7.4&$skiptoken=next".to_owned(),
                "https://kv.vault.azure.net/keys?api-version=7.4".to_owned(),
                "https://kv.vault.azure.net/certificates?api-version=7.4".to_owned(),
            ]
        );
        assert_eq!(transport.mints(), 0, "the vault plane wants its own token");
    }

    #[test]
    fn tags_and_manifests_read_their_digest_created_and_size() {
        let registry = registry_of("acr.azurecr.io");
        let mut answers = acr_handshake();
        answers.push((
            200,
            None,
            json!({
                "tags": [
                    {
                        "name": "2026.8.29",
                        "digest": "sha256:aaa",
                        "createdTime": "2026-08-29T09:00:00.0000000Z",
                        "lastUpdateTime": "2026-08-29T09:05:00.0000000Z",
                    },
                    { "name": "latest", "digest": "sha256:bbb" },
                ],
            })
            .to_string(),
        ));
        let transport = FakeHttp::answering(&answers);
        let tags = client(&transport).tags(&registry, "team/api").unwrap();

        assert_eq!(
            tags,
            vec![
                Tag {
                    name: "2026.8.29".to_owned(),
                    digest: "sha256:aaa".to_owned(),
                    created: Some(ts("2026-08-29T09:00:00Z")),
                    updated: Some(ts("2026-08-29T09:05:00Z")),
                },
                Tag {
                    name: "latest".to_owned(),
                    digest: "sha256:bbb".to_owned(),
                    created: None,
                    updated: None,
                },
            ]
        );
        assert_eq!(
            transport.urls()[2],
            "https://acr.azurecr.io/acr/v1/team/api/_tags?n=100&orderby=timedesc"
        );
        assert_eq!(
            form(&transport.sent()[1])[2],
            (
                "scope".to_owned(),
                "repository:team/api:metadata_read".to_owned()
            )
        );

        let mut answers = acr_handshake();
        answers.push((
            200,
            None,
            json!({
                "manifest": {
                    "digest": "sha256:aaa",
                    "imageSize": 41_234_567_u64,
                    "createdTime": "2026-08-29T09:00:00.0000000Z",
                    "architecture": "amd64",
                    "os": "linux",
                },
            })
            .to_string(),
        ));
        let transport = FakeHttp::answering(&answers);
        let manifest = client(&transport)
            .manifest(&registry, "team/api", "sha256:aaa")
            .unwrap();
        assert_eq!(
            manifest,
            Manifest {
                digest: "sha256:aaa".to_owned(),
                size: Some(41_234_567),
                created: Some(ts("2026-08-29T09:00:00Z")),
                architecture: "amd64".to_owned(),
                os: "linux".to_owned(),
            }
        );

        // The same fields at the top level are the same answer.
        let mut answers = acr_handshake();
        answers.push((
            200,
            None,
            json!({ "digest": "sha256:ccc", "imageSize": "17", "os": "windows" }).to_string(),
        ));
        let transport = FakeHttp::answering(&answers);
        let flat = client(&transport)
            .manifest(&registry, "team/api", "sha256:ccc")
            .unwrap();
        assert_eq!(flat.size, Some(17));
        assert_eq!(flat.os, "windows");
    }

    #[test]
    fn a_secret_never_prints_itself() {
        let vault = vault_of("https://kv.vault.azure.net/");
        let transport = FakeHttp::ok(&[json!({ "value": "hunter2" })]);
        let secret = client(&transport)
            .secret_value(&vault, "db-password")
            .unwrap();

        assert_eq!(format!("{secret:?}"), "[redacted]");
        assert_eq!(format!("{secret}"), "[redacted]");
        assert_eq!(secret.expose(), "hunter2");
        assert_eq!(
            transport.urls(),
            vec!["https://kv.vault.azure.net/secrets/db-password?api-version=7.4".to_owned()]
        );
    }

    #[test]
    fn a_signed_out_answer_mints_the_token_once_more() {
        let transport = FakeHttp::answering(&[
            (
                401,
                None,
                json!({ "error": { "message": "The access token expiry is in the past" } })
                    .to_string(),
            ),
            (200, None, json!({ "data": [] }).to_string()),
        ]);
        let arm = client(&transport);
        assert_eq!(arm.inventory().unwrap(), Inventory::default());
        assert_eq!(
            transport.mints(),
            2,
            "the rejection was worth one new token"
        );
        assert_eq!(
            transport
                .sent()
                .iter()
                .map(|held| held.bearer.clone().unwrap_or_default())
                .collect::<Vec<_>>(),
            vec!["arm-token-1".to_owned(), "arm-token-2".to_owned()]
        );
    }

    #[test]
    fn the_fake_source_answers_its_canned_inventory_and_holds_one_throttle() {
        let fake = FakeArm::default();
        *fake.registries.lock().unwrap() = vec![registry_of("acr.azurecr.io")];
        *fake.vaults.lock().unwrap() = vec![vault_of("https://kv.vault.azure.net/")];
        *fake.secret.lock().unwrap() = "hunter2".to_owned();
        *fake.throttle.lock().unwrap() = Some(Duration::from_secs(45));

        let inventory = fake.inventory().unwrap();
        assert_eq!(inventory.registries.len(), 1);
        assert_eq!(inventory.vaults.len(), 1);
        assert_eq!(
            fake.secret_value(&inventory.vaults[0], "db-password")
                .unwrap()
                .expose(),
            "hunter2"
        );
        assert_eq!(fake.throttled_for(), Some(Duration::from_secs(45)));
        assert_eq!(fake.throttled_for(), None);
        assert_eq!(
            *fake.reads.lock().unwrap(),
            vec![
                "inventory".to_owned(),
                "secret_value db-password".to_owned()
            ]
        );
    }

    /// A source over canned data, recording what it was asked. The later tabs'
    /// tests drive their panes with one of these rather than a transport.
    #[derive(Clone, Default)]
    pub(crate) struct FakeArm {
        pub registries: Arc<Mutex<Vec<Registry>>>,
        pub vaults: Arc<Mutex<Vec<Vault>>>,
        pub repositories: Arc<Mutex<Vec<Repository>>>,
        pub tags: Arc<Mutex<Vec<Tag>>>,
        pub items: Arc<Mutex<Vec<VaultItem>>>,
        pub secret: Arc<Mutex<String>>,
        /// The wait the next read reports having been asked for.
        pub throttle: Arc<Mutex<Option<Duration>>>,
        /// What the next read fails with, instead of answering.
        pub failure: Arc<Mutex<Option<String>>>,
        /// Every call made, in order.
        pub reads: Arc<Mutex<Vec<String>>>,
    }

    impl FakeArm {
        fn read<T>(&self, what: &str, answer: T) -> Result<T> {
            self.reads.lock().unwrap().push(what.to_owned());
            match self.failure.lock().unwrap().clone() {
                Some(message) => Err(anyhow!(message)),
                None => Ok(answer),
            }
        }
    }

    impl ArmSource for FakeArm {
        fn inventory(&self) -> Result<Inventory> {
            let inventory = Inventory {
                registries: self.registries.lock().unwrap().clone(),
                vaults: self.vaults.lock().unwrap().clone(),
            };
            self.read("inventory", inventory)
        }

        fn repositories(&self, _registry: &Registry) -> Result<Vec<Repository>> {
            let repositories = self.repositories.lock().unwrap().clone();
            self.read("repositories", repositories)
        }

        fn repository(&self, _registry: &Registry, name: &str) -> Result<Repository> {
            let held = self
                .repositories
                .lock()
                .unwrap()
                .iter()
                .find(|held| held.name == name)
                .cloned()
                .unwrap_or(Repository {
                    name: name.to_owned(),
                    tags: None,
                    manifests: None,
                    updated: None,
                });
            self.read(&format!("repository {name}"), held)
        }

        fn tags(&self, _registry: &Registry, repo: &str) -> Result<Vec<Tag>> {
            let tags = self.tags.lock().unwrap().clone();
            self.read(&format!("tags {repo}"), tags)
        }

        fn items(&self, _vault: &Vault) -> Result<Vec<VaultItem>> {
            let items = self.items.lock().unwrap().clone();
            self.read("items", items)
        }

        fn secret_value(&self, _vault: &Vault, name: &str) -> Result<Secret> {
            let secret = Secret(self.secret.lock().unwrap().clone());
            self.read(&format!("secret_value {name}"), secret)
        }

        fn throttled_for(&self) -> Option<Duration> {
            self.throttle.lock().unwrap().take()
        }
    }
}
