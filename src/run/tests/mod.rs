//! Tests for the interactive run, split the way the module is. The fake
//! Azure DevOps and the helpers that drive it live here; each submodule
//! covers one file's worth of behaviour.

mod desktop;
mod details;
mod editor;
mod edits;
mod startup;
mod sync;

use std::time::Instant;

use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use tempfile::tempdir;
use ticket_tui::app::{FormFieldId, NotificationLevel, WorkItemMode};
use ticket_tui::azure::{RequestRejected, SyncBatch, Throttled};
use ticket_tui::edit::FieldEdit;
use ticket_tui::model::{
    CommentRecord, HistoryRecord, RelationKind, RelationRecord, StateOption, StoredWorkItem,
    Ticket, TicketGraph, TicketKey, WorkItemDetails,
};
use ticket_tui::sync::{PulledExtras, SourceConnector, WorkItemSource};
use ticket_tui::timestamp::Timestamp;

/// A launcher that never opens anything, for the tests that only need
/// the refusal.
fn failing_opener(_: &Url) -> Result<()> {
    bail!("launcher unavailable")
}

/// Azure DevOps stood in for: every pull returns the same tickets, or the
/// same failure, and a write answers with a stored copy or a refusal.
#[derive(Clone)]
struct FakeAzure {
    tickets: Vec<Ticket>,
    failure: Option<String>,
    /// The copies a write is answered with, looked up by work item id, so
    /// a bulk change can be answered one work item at a time.
    stored: Vec<Ticket>,
    refusal: Option<(u16, String)>,
    /// Whether a changed-since query comes back empty: the project still
    /// lists every one of `tickets`, but none of them has moved.
    quiet: bool,
    /// The one work item whose comments and history can be read, and what
    /// they say.
    details: Option<(i64, WorkItemDetails)>,
    /// The comment a post answers with, if this fake takes comments at all.
    comment: Option<CommentRecord>,
    /// The work item a create answers with, and the links that come with
    /// it, if this fake creates work items at all.
    creation: Option<StoredWorkItem>,
    /// The wait every pull is turned away with, for a project Azure DevOps
    /// is shedding load from.
    throttle: Option<Duration>,
}

impl FakeAzure {
    fn returning(tickets: Vec<Ticket>) -> Self {
        Self {
            tickets,
            failure: None,
            stored: Vec::new(),
            refusal: None,
            quiet: false,
            details: None,
            comment: None,
            creation: None,
            throttle: None,
        }
    }

    /// Turns every pull away for throttling, naming the same wait each time.
    fn throttling(retry_after: Duration) -> Self {
        Self {
            throttle: Some(retry_after),
            ..Self::returning(Vec::new())
        }
    }

    /// Still holds `tickets`, but nothing in it has changed.
    fn quiet(tickets: Vec<Ticket>) -> Self {
        Self {
            quiet: true,
            ..Self::returning(tickets)
        }
    }

    fn failing(message: &str) -> Self {
        Self {
            failure: Some(message.to_owned()),
            ..Self::returning(Vec::new())
        }
    }

    /// Accepts the next write and answers with `stored`.
    fn storing(stored: Ticket) -> Self {
        Self::storing_each(vec![stored])
    }

    /// Accepts a write of any of these work items and answers with the
    /// copy it holds for that one.
    fn storing_each(stored: Vec<Ticket>) -> Self {
        Self {
            stored,
            ..Self::returning(Vec::new())
        }
    }

    /// Refuses the next write, then answers pulls with `tickets`.
    fn refusing(status: u16, message: &str, tickets: Vec<Ticket>) -> Self {
        Self {
            refusal: Some((status, message.to_owned())),
            ..Self::returning(tickets)
        }
    }

    /// Accepts the next comment and answers with `comment`.
    fn commenting(comment: CommentRecord) -> Self {
        Self {
            comment: Some(comment),
            ..Self::returning(Vec::new())
        }
    }

    /// Accepts the next create and answers with `ticket` and its links.
    fn creating(ticket: Ticket, relations: Vec<RelationRecord>) -> Self {
        Self {
            creation: Some((ticket, relations, Vec::new())),
            ..Self::returning(Vec::new())
        }
    }

    /// Answers with `details` when that one work item is read.
    fn detailing(id: i64, details: WorkItemDetails) -> Self {
        Self {
            details: Some((id, details)),
            ..Self::returning(Vec::new())
        }
    }
}

impl WorkItemSource for FakeAzure {
    fn pull(&self) -> Result<SyncBatch> {
        if let Some(retry_after) = self.throttle {
            return Err(anyhow::Error::new(Throttled::new(
                retry_after,
                429,
                "https://dev.azure.com/example-org/_apis/wit/wiql",
                "too many requests",
            )));
        }
        match &self.failure {
            Some(message) => bail!("{message}"),
            None => Ok(SyncBatch {
                tickets: self.tickets.clone(),
                relations: Vec::new(),
                artifacts: Vec::new(),
            }),
        }
    }

    /// The same work items whichever way they are asked for, unless the
    /// fake was told the project has gone quiet.
    fn pull_changed_since(&self, _watermark: Timestamp) -> Result<SyncBatch> {
        if self.quiet {
            return Ok(SyncBatch::default());
        }
        self.pull()
    }

    fn list_ids(&self) -> Result<Vec<i64>> {
        match &self.failure {
            Some(message) => bail!("{message}"),
            None => Ok(self.tickets.iter().map(|ticket| ticket.key.id).collect()),
        }
    }

    fn display_name(&self) -> Result<Option<String>> {
        Ok(None)
    }

    fn patch_work_item(&self, id: i64, _patch: &[Value]) -> Result<StoredWorkItem> {
        if let Some((status, message)) = &self.refusal {
            return Err(anyhow::Error::new(RequestRejected::new(
                *status,
                format!("https://dev.azure.com/example-org/_apis/wit/workitems/{id}"),
                message.clone(),
            )));
        }
        match self.stored.iter().find(|ticket| ticket.key.id == id) {
            Some(ticket) => Ok((ticket.clone(), Vec::new(), Vec::new())),
            None => bail!("the fake source was not given a stored copy of #{id}"),
        }
    }

    fn create_work_item(
        &self,
        _work_item_type: &str,
        _fields: &[Value],
        _parent: Option<i64>,
    ) -> Result<StoredWorkItem> {
        if let Some((status, message)) = &self.refusal {
            return Err(anyhow::Error::new(RequestRejected::new(
                *status,
                "https://dev.azure.com/example-org/atlas/_apis/wit/workitems/$Issue".to_owned(),
                message.clone(),
            )));
        }
        self.creation
            .clone()
            .context("the fake source was not given a work item to create")
    }

    /// The state picker is fed from the database, so these tests never need
    /// the endpoint; answering with nothing keeps a pull from asking twice.
    fn work_item_type_states(&self, _work_item_type: &str) -> Result<Vec<StateOption>> {
        Ok(Vec::new())
    }

    fn fetch_details(&self, id: i64) -> Result<WorkItemDetails> {
        match &self.details {
            Some((detailed, details)) if *detailed == id => Ok(details.clone()),
            _ => Ok(WorkItemDetails::default()),
        }
    }

    fn post_comment(&self, _id: i64, _html: &str) -> Result<CommentRecord> {
        match self.comment.clone() {
            Some(comment) => Ok(comment),
            None => bail!("HTTP 403: the work item is read only"),
        }
    }
}

impl SourceConnector for FakeAzure {
    fn connect(&mut self) -> Result<Box<dyn WorkItemSource>> {
        Ok(Box::new(self.clone()))
    }
}

/// An app and a runtime wired to a fake Azure DevOps over a seeded database.
fn synced_app(path: &Path, source: FakeAzure) -> (App, SqliteTicketRepository, SyncRuntime) {
    let repository = seeded_repository(path);
    let mut app = App::new(repository.load_all().unwrap());
    app.shell
        .configure_database(path.to_path_buf(), db::data_signature(path));
    app.shell.enable_sync();
    let runtime = SyncRuntime {
        worker: Some(SyncHandle::spawn(path.to_path_buf(), Box::new(source)).unwrap()),
        scheduler: SyncScheduler::new(Some(Duration::from_secs(60))),
        config: Some(AzureConfig {
            organization: "example-org".into(),
            project: "atlas".into(),
            scope: None,
        }),
        offline_reason: None,
        details: DetailsEngine::default(),
        pipelines: None,
        watching_tab: false,
        watching_run: (None, None),
        watched_runs: Vec::new(),
        local: LocalRuntime::default(),
    };
    (app, repository, runtime)
}

/// Pumps the event loop's sync polling until the pull in flight lands.
fn await_sync(app: &mut App, repository: &mut SqliteTicketRepository, runtime: &mut SyncRuntime) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.shell.sync_pending {
        poll_sync(app, repository, runtime);
        assert!(Instant::now() < deadline, "the sync worker timed out");
        thread::yield_now();
    }
}

/// Pumps the event loop's sync polling until the edit in flight answers.
fn await_edit(app: &mut App, repository: &mut SqliteTicketRepository, runtime: &mut SyncRuntime) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.work_items.edits_pending() {
        poll_sync(app, repository, runtime);
        assert!(Instant::now() < deadline, "the sync worker timed out");
        thread::yield_now();
    }
}

/// Pumps the event loop's sync polling until the comment in flight answers.
fn await_comment(
    app: &mut App,
    repository: &mut SqliteTicketRepository,
    runtime: &mut SyncRuntime,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.work_items.comments_pending() {
        poll_sync(app, repository, runtime);
        assert!(Instant::now() < deadline, "the sync worker timed out");
        thread::yield_now();
    }
}

/// Pumps the event loop's sync polling until the create in flight answers.
fn await_create(app: &mut App, repository: &mut SqliteTicketRepository, runtime: &mut SyncRuntime) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.work_items.creates_pending() {
        poll_sync(app, repository, runtime);
        assert!(Instant::now() < deadline, "the sync worker timed out");
        thread::yield_now();
    }
}

fn ticket(id: i64) -> Ticket {
    Ticket {
        key: TicketKey {
            organization: "example-org".into(),
            id,
        },
        project: "atlas".into(),
        revision: 1,
        work_item_type: "Task".into(),
        title: format!("Ticket {id}"),
        state: "Active".into(),
        reason: None,
        assigned_to: Some("Avery Chen".into()),
        priority: Some(2),
        area_path: "Atlas".into(),
        iteration_path: "Atlas\\Sprint 1".into(),
        tags: Vec::new(),
        description: String::new(),
        description_html: String::new(),
        created_at: Timestamp::parse("2026-01-01T00:00:00Z").unwrap(),
        changed_at: Timestamp::parse(&format!("2026-0{id}-01T00:00:00Z")).unwrap(),
        web_url: format!("https://dev.azure.com/example-org/atlas/_workitems/edit/{id}"),
        details_rev: 0,
    }
}

/// The work items on the table, in the order it holds them.
fn visible_ids(app: &App) -> Vec<i64> {
    app.work_items
        .visible_tickets()
        .map(|ticket| ticket.key.id)
        .collect()
}

fn seeded_repository(path: &Path) -> SqliteTicketRepository {
    let mut repository = SqliteTicketRepository::open(path).unwrap();
    let tickets: Vec<Ticket> = (1..=3).map(ticket).collect();
    repository
        .replace_all(&tickets, &TicketGraph::default())
        .unwrap();
    repository
}
