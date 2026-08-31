use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::model::{RowDensity, SearchOrder, SortDirection, SortField};

pub const SCHEMA_VERSION: u8 = 3;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AgentContext {
    pub database_path: String,
    /// Display name of the signed-in Azure DevOps user, or `null` when the
    /// last sync could not read a profile.
    pub me: Option<String>,
    /// How fresh the rows are: where they are pulled from, whether a pull is
    /// running, and how the last one went.
    pub sync: SyncContext,
    /// Edits sent to Azure DevOps and not answered yet. The rows already show
    /// them, so a value named here is optimistic until the edit leaves this
    /// list.
    pub pending_edits: Vec<PendingEditContext>,
    /// Which tab is showing: `work_items`, `repos`, `pull_requests`,
    /// `pipelines`, `aks`, `acr` or `key_vault`. Every tab is described
    /// whether or not it is the one on screen, so an agent can read the whole
    /// workspace; this says where the user actually is.
    pub active_tab: String,
    pub work_items: WorkItemsContext,
    pub repos: ReposContext,
    pub pull_requests: PullRequestsContext,
    pub pipelines: PipelinesContext,
    pub aks: AksContext,
    pub acr: AcrContext,
    pub key_vault: KeyVaultContext,
    /// What the ARM tabs can reach: the subscription they read, and why they
    /// read nothing when they cannot.
    pub arm: ArmContext,
}

/// The ACR tab. A placeholder while the tab is a shell: B3 fills it in.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct AcrContext {
    pub level: String,
    pub visible_rows: usize,
}

/// The Key Vault tab, the same way round: C1 fills it in.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct KeyVaultContext {
    pub level: String,
    pub visible_rows: usize,
}

/// Whether the ACR and Key Vault tabs have a subscription to read at all. An
/// offline run shows both tabs and neither reads anything; `last_error` is the
/// one line that says why.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ArmContext {
    pub subscription: Option<String>,
    pub offline: bool,
    pub last_error: Option<String>,
}

/// The AKS tab: the clusters `config.toml` names, the pod under the cursor,
/// and whatever could not be read.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct AksContext {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub clusters: Vec<String>,
    pub selected: Option<PodContext>,
    pub visible_rows: usize,
    /// How many of the pods on the table are in trouble.
    pub unhealthy: usize,
    /// The log the text pane is tailing, when it is on one.
    pub following_log: Option<FollowingPodLogContext>,
    /// One line per `(cluster, namespace)` that could not be read.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// One pod, as an agent reads it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PodContext {
    pub cluster: String,
    pub namespace: String,
    pub name: String,
    pub status: String,
    /// Containers ready over containers in the spec: `1/2`.
    pub ready: String,
    pub restarts: u32,
    pub node: String,
    /// What made it: `Deployment/orders-api`, or `null` for a bare pod.
    pub owner: Option<String>,
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub containers: Vec<ContainerContext>,
    /// The repository on file its image or app label names, when one does.
    pub repo: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerContext {
    pub name: String,
    pub image: String,
    pub state: String,
    pub restarts: u32,
}

/// The work items tab, which is everything schema 2 described at the top
/// level, moved under a name of its own and otherwise unchanged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkItemsContext {
    pub mode: String,
    pub focus: String,
    pub screen: String,
    pub active_view: Option<String>,
    pub search: SearchContext,
    pub sort: SortContext,
    pub tickets: TicketsContext,
    pub selected_ticket: Option<TicketContext>,
    pub checked_tickets: Vec<TicketContext>,
    pub family_cursor: Option<TicketReference>,
    pub details_scroll_line: u16,
}

/// The Repos tab: the project's repositories and which of them are on this
/// machine.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ReposContext {
    pub selected: Option<RepoContext>,
    pub visible_rows: Vec<RepoContext>,
    /// Where clones are looked for and made, or `null` when there is nowhere.
    pub workspace: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RepoContext {
    pub id: String,
    pub name: String,
    pub default_branch: String,
    pub is_disabled: bool,
    /// How many active pull requests and pipelines name it.
    pub pull_requests: usize,
    pub pipelines: usize,
    pub web_url: String,
    /// The clone on this machine, or `null` for a repository that is not here.
    pub local: Option<LocalRepoContext>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LocalRepoContext {
    pub path: String,
    pub branch: String,
    pub dirty: bool,
    pub ahead: u32,
    pub behind: u32,
    /// What git is doing to it right now — `cloning`, `fetching`, `pulling` —
    /// or `null` when nothing is.
    pub busy: Option<String>,
}

/// The Pull requests tab: the review queue and whichever request is selected.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct PullRequestsContext {
    pub selected: Option<PullRequestContext>,
    pub visible_rows: Vec<PullRequestRowContext>,
    /// How many are waiting on the signed-in user's vote.
    pub to_review_count: usize,
    /// Whether closed pull requests are on the table.
    pub closed_shown: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PullRequestRowContext {
    pub id: i64,
    pub repo: String,
    pub title: String,
    pub author: String,
    pub status: String,
    pub is_draft: bool,
    pub source_branch: String,
    pub target_branch: String,
    /// `succeeded`, `conflicts`, `queued` — what Azure DevOps says a merge
    /// would do.
    pub merge_status: String,
    /// The signed-in user's own vote, on the API's scale: 10 approved, 5
    /// approved with suggestions, 0 no vote, -5 waiting, -10 rejected.
    pub my_vote: i8,
    pub web_url: String,
}

/// The selected pull request, which carries what the details pane draws.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PullRequestContext {
    #[serde(flatten)]
    pub row: PullRequestRowContext,
    pub reviewers: Vec<ReviewerContext>,
    /// The work items it carries, by id.
    pub work_items: Vec<i64>,
    pub build: Option<PrBuildContext>,
    pub auto_complete: bool,
    /// How many comment threads it has, and how many are unresolved.
    pub thread_count: usize,
    pub unresolved_threads: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReviewerContext {
    pub name: String,
    pub vote: i8,
    pub is_required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PrBuildContext {
    pub status: String,
    pub run_id: Option<i64>,
}

/// The Pipelines tab: which level it is on, what is selected, and what the
/// watcher is following.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct PipelinesContext {
    /// `pipelines` or `runs`.
    pub level: String,
    pub selected_pipeline: Option<PipelineContext>,
    pub selected_run: Option<RunContext>,
    /// The log the details pane is tailing, or `null`.
    pub following_log: Option<FollowingLogContext>,
    /// How many runs are going right now.
    pub running: usize,
    /// The runs `w` is following, by id.
    pub watched: Vec<i64>,
    pub pending_approvals: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PipelineContext {
    pub id: i64,
    pub name: String,
    pub folder: String,
    pub repo: Option<String>,
    pub web_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RunContext {
    pub id: i64,
    pub pipeline_id: i64,
    pub build_number: String,
    pub status: String,
    pub result: Option<String>,
    pub branch: String,
    pub requested_for: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub web_url: String,
    /// The run's stages and how each is going, top level of the timeline only:
    /// the whole tree would be longer than the rest of the document.
    pub stages: Vec<StageContext>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StageContext {
    pub name: String,
    pub state: String,
    pub result: Option<String>,
}

/// The pod log the AKS text pane is tailing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FollowingPodLogContext {
    pub pod: String,
    /// The container chosen, or nothing while the pane follows the first.
    pub container: Option<String>,
    /// Whether it is the run before the last restart.
    pub previous: bool,
    pub line_count: usize,
    /// Whether the pane is pinned to the tail rather than scrolled back.
    pub following: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FollowingLogContext {
    pub run_id: i64,
    pub log_id: i64,
    pub node: String,
    pub line_count: usize,
    /// Whether the pane is pinned to the tail rather than scrolled back.
    pub following: bool,
}

/// What the run knows about its own sync, so an agent can tell data that is a
/// minute old from data that stopped arriving an hour ago.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SyncContext {
    /// The Azure DevOps organization and project the rows are pulled from, or
    /// `null` on a run with no project resolved.
    pub organization: Option<String>,
    pub project: Option<String>,
    /// Seconds between timer pulls, `0` when the timer is off and the sync key
    /// is the only thing that pulls.
    pub refresh_seconds: u64,
    /// Whether a pull is in flight right now.
    pub in_progress: bool,
    /// When the last pull that reached Azure DevOps finished, RFC 3339, or
    /// `null` when none has this run. A pull that found nothing new still
    /// moves this: it says when the rows were last confirmed, not when they
    /// last changed.
    pub last_success_at: Option<String>,
    /// What the last failed pull said, cleared by the next one that succeeds.
    pub last_error: Option<String>,
    /// Whether the run has an Azure DevOps project to pull from at all. An
    /// offline run browses whatever the database already holds and never
    /// refreshes it. A run whose worker died later stays `false` and says so
    /// through `last_error` instead.
    pub offline: bool,
}

/// One edit the table is already showing and Azure DevOps has not answered yet.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PendingEditContext {
    pub id: i64,
    /// The field as the Actions menu names it, such as `State` or `Tags`.
    pub field: String,
    /// The value being written, as a notification spells it; a cleared field
    /// reads `(none)`.
    pub value: String,
    /// When the edit was sent, RFC 3339.
    pub since: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SearchContext {
    pub query: String,
    pub fuzzy_text: String,
    pub filters: Vec<String>,
    pub pending: bool,
    pub order: SearchOrder,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SortContext {
    pub field: SortField,
    pub direction: SortDirection,
    pub row_density: RowDensity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TicketsContext {
    pub total_count: usize,
    pub matching_count: usize,
    /// Whether the table is leaving finished work out, so the rows counted
    /// here are the open backlog rather than everything the query matches.
    /// The details pane and the family tree still reach a hidden work item.
    pub finished_hidden: bool,
    pub viewport_start: usize,
    pub viewport_size: usize,
    pub visible_rows: Vec<TicketContext>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TicketContext {
    pub organization: String,
    pub project: String,
    pub id: i64,
    pub work_item_type: String,
    pub title: String,
    pub state: String,
    pub assigned_to: Option<String>,
    pub priority: Option<i64>,
    pub tags: Vec<String>,
    pub web_url: String,
    pub bookmarked: bool,
    pub checked: bool,
    /// What the work item was worked on with: its pull requests, the commits
    /// that named it, the builds it went out in. Carried on the selected work
    /// item only — the checked ones are a list, not a reading — and left out
    /// of the document entirely when there is nothing to say.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<ArtifactContext>,
}

/// One artifact link, as an agent reads it. `target` is the pull request or
/// build number, or the commit sha; `in_database` says whether this app can
/// show it, which is what tells an agent whether a `ticket-tui` command will
/// find it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ArtifactContext {
    pub kind: String,
    pub name: String,
    pub repo: Option<String>,
    pub target: String,
    pub in_database: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TicketReference {
    pub organization: String,
    pub id: i64,
}

#[derive(Serialize)]
struct ContextDocument<'a> {
    schema_version: u8,
    process_id: u32,
    updated_at: String,
    #[serde(flatten)]
    context: &'a AgentContext,
}

#[must_use]
pub fn path_for(database: &Path) -> PathBuf {
    let mut file_name = database
        .file_stem()
        .map_or_else(|| "tickets".into(), |stem| stem.to_os_string());
    file_name.push(".context.json");
    database.with_file_name(file_name)
}

pub fn save(path: &Path, context: &AgentContext) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let document = ContextDocument {
        schema_version: SCHEMA_VERSION,
        process_id: std::process::id(),
        updated_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .context("failed to format agent context time")?,
        context,
    };
    let mut raw =
        serde_json::to_string_pretty(&document).context("failed to serialize agent context")?;
    raw.push('\n');
    let temporary = temporary_path(path);
    fs::write(&temporary, raw)
        .with_context(|| format!("failed to write agent context {}", temporary.display()))?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error)
            .with_context(|| format!("failed to publish agent context {}", path.display()));
    }
    Ok(())
}

pub fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to remove agent context {}", path.display()))
        }
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map_or_else(|| "tickets.context.json".into(), |name| name.to_os_string());
    file_name.push(format!(".tmp.{}", std::process::id()));
    path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn context(database_path: String) -> AgentContext {
        AgentContext {
            database_path,
            me: Some("Jacob Ragsdale".into()),
            sync: SyncContext {
                organization: Some("example-org".into()),
                project: Some("atlas".into()),
                refresh_seconds: 60,
                in_progress: false,
                last_success_at: Some("2026-08-29T12:00:00Z".into()),
                last_error: None,
                offline: false,
            },
            pending_edits: Vec::new(),
            active_tab: "work_items".into(),
            repos: ReposContext::default(),
            pull_requests: PullRequestsContext::default(),
            pipelines: PipelinesContext::default(),
            aks: AksContext::default(),
            acr: AcrContext::default(),
            key_vault: KeyVaultContext::default(),
            arm: ArmContext::default(),
            work_items: WorkItemsContext {
                mode: "browse".into(),
                focus: "tickets".into(),
                screen: "workspace".into(),
                active_view: None,
                search: SearchContext {
                    query: String::new(),
                    fuzzy_text: String::new(),
                    filters: Vec::new(),
                    pending: false,
                    order: SearchOrder::Relevance,
                },
                sort: SortContext {
                    field: SortField::Changed,
                    direction: SortDirection::Descending,
                    row_density: RowDensity::Compact,
                },
                tickets: TicketsContext {
                    total_count: 0,
                    matching_count: 0,
                    finished_hidden: true,
                    viewport_start: 0,
                    viewport_size: 0,
                    visible_rows: Vec::new(),
                },
                selected_ticket: None,
                checked_tickets: Vec::new(),
                family_cursor: None,
                details_scroll_line: 0,
            },
        }
    }

    #[test]
    fn save_replaces_a_complete_json_document_and_remove_is_idempotent() {
        let directory = tempdir().unwrap();
        let path = path_for(&directory.path().join("tickets.sqlite3"));
        assert_eq!(path, directory.path().join("tickets.context.json"));
        let mut first = context("first.sqlite3".into());
        first.pending_edits.push(PendingEditContext {
            id: 625,
            field: "State".into(),
            value: "Doing".into(),
            since: "2026-08-29T12:00:01Z".into(),
        });
        save(&path, &first).unwrap();
        let first_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(first_json["schema_version"], SCHEMA_VERSION);
        assert_eq!(first_json["database_path"], "first.sqlite3");
        assert_eq!(first_json["me"], "Jacob Ragsdale");
        assert_eq!(first_json["active_tab"], "work_items");
        assert_eq!(first_json["work_items"]["search"]["order"], "relevance");
        assert_eq!(first_json["work_items"]["sort"]["field"], "changed");
        assert_eq!(first_json["work_items"]["sort"]["direction"], "desc");
        assert_eq!(first_json["work_items"]["sort"]["row_density"], "compact");
        assert_eq!(first_json["work_items"]["tickets"]["finished_hidden"], true);
        // Every tab is in the document whether or not it is the one showing.
        assert!(first_json["repos"]["visible_rows"].is_array());
        assert!(first_json["pull_requests"]["visible_rows"].is_array());
        assert_eq!(first_json["pipelines"]["level"], "");
        assert_eq!(first_json["sync"]["organization"], "example-org");
        assert_eq!(first_json["sync"]["project"], "atlas");
        assert_eq!(first_json["sync"]["refresh_seconds"], 60);
        assert_eq!(first_json["sync"]["in_progress"], false);
        assert_eq!(
            first_json["sync"]["last_success_at"],
            "2026-08-29T12:00:00Z"
        );
        assert!(first_json["sync"]["last_error"].is_null());
        assert_eq!(first_json["sync"]["offline"], false);
        assert_eq!(first_json["pending_edits"][0]["id"], 625);
        assert_eq!(first_json["pending_edits"][0]["field"], "State");
        assert_eq!(first_json["pending_edits"][0]["value"], "Doing");
        assert_eq!(
            first_json["pending_edits"][0]["since"],
            "2026-08-29T12:00:01Z"
        );
        assert!(first_json["process_id"].as_u64().is_some());
        assert!(first_json["updated_at"].as_str().is_some());

        let mut second = context("second.sqlite3".into());
        second.me = None;
        save(&path, &second).unwrap();
        let second_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(second_json["database_path"], "second.sqlite3");
        assert!(second_json["me"].is_null());
        assert!(second_json["pending_edits"].as_array().unwrap().is_empty());
        assert!(!temporary_path(&path).exists());

        remove(&path).unwrap();
        remove(&path).unwrap();
        assert!(!path.exists());
    }
}
