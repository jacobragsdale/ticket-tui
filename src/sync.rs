//! Background sync with Azure DevOps: a worker thread pulls work items on
//! request, writes them to SQLite, and hands the reloaded rows back to the
//! main thread, which owns the timer that decides when to ask.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::app::PreparedTickets;
use crate::azure::{self, AzureClient, AzureConfig, SyncBatch};
use crate::db::SqliteTicketRepository;
use crate::edit::{EditApplied, EditRejection, EditRequest};
use crate::model::{RelationRecord, Ticket, TicketGraph};

/// What asked for a pull. A timer pull is silent unless it fails; a keypress
/// reports itself either way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PullOrigin {
    Timer,
    User,
}

/// Work for the sync thread, done in the order it arrives: an edit queued
/// before a pull is written before that pull reads.
#[derive(Clone, Debug)]
pub enum SyncRequest {
    /// Replace every local work item with a fresh copy from Azure DevOps.
    Pull(PullOrigin),
    /// Write one field of one work item back to Azure DevOps.
    Edit(EditRequest),
}

/// What the sync thread sends back.
#[derive(Debug)]
pub enum SyncEvent {
    /// The signed-in display name, sent once after the first connect.
    DisplayName(String),
    /// A request finished, successfully or not.
    Finished {
        origin: PullOrigin,
        outcome: SyncOutcome,
    },
    /// One edit landed, or was refused and changes nothing.
    Edited(Box<Result<EditApplied, EditRejection>>),
    /// The worker thread is gone and no further events will arrive.
    Stopped,
}

/// How one request ended.
#[derive(Debug)]
pub enum SyncOutcome {
    /// Work items already written to SQLite and read back from it, so memory
    /// and the database hold the same rows.
    Pulled {
        prepared: PreparedTickets,
        count: usize,
    },
    Failed(String),
}

/// Where work items come from. `AzureClient` implements it; tests use a fake so
/// the worker can be exercised without a network.
pub trait WorkItemSource {
    fn pull(&self) -> Result<SyncBatch>;
    /// Display name of the signed-in user, used to mark their own work items.
    fn display_name(&self) -> Result<Option<String>>;
    /// Write one work item with a JSON Patch document, answering with the copy
    /// the server stored.
    fn patch_work_item(&self, id: i64, patch: &[Value]) -> Result<(Ticket, Vec<RelationRecord>)>;
}

impl WorkItemSource for AzureClient {
    fn pull(&self) -> Result<SyncBatch> {
        self.fetch_all_work_items()
    }

    fn display_name(&self) -> Result<Option<String>> {
        self.current_user_display_name()
    }

    fn patch_work_item(&self, id: i64, patch: &[Value]) -> Result<(Ticket, Vec<RelationRecord>)> {
        self.update_work_item(id, patch)
    }
}

/// Opens a source the first time one is needed, so the TUI never waits for an
/// access token before it draws.
pub trait SourceConnector: Send {
    fn connect(&mut self) -> Result<Box<dyn WorkItemSource>>;
}

/// Connects to the configured Azure DevOps project.
#[derive(Clone, Debug)]
pub struct AzureConnector {
    config: AzureConfig,
}

impl AzureConnector {
    #[must_use]
    pub const fn new(config: AzureConfig) -> Self {
        Self { config }
    }
}

impl SourceConnector for AzureConnector {
    fn connect(&mut self) -> Result<Box<dyn WorkItemSource>> {
        Ok(Box::new(AzureClient::connect(self.config.clone())?))
    }
}

/// The main thread's end of the sync worker.
#[derive(Debug)]
pub struct SyncHandle {
    requests: Sender<SyncRequest>,
    events: Receiver<SyncEvent>,
    stopped: Cell<bool>,
}

impl SyncHandle {
    /// Starts the worker. It keeps running until the handle is dropped, then
    /// ends once the request in flight finishes.
    pub fn spawn(database: PathBuf, connector: Box<dyn SourceConnector>) -> Result<Self> {
        let (request_sender, request_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("ticket-sync".into())
            .spawn(move || work(database, connector, &request_receiver, &event_sender))
            .context("failed to start the Azure DevOps sync worker")?;
        Ok(Self {
            requests: request_sender,
            events: event_receiver,
            stopped: Cell::new(false),
        })
    }

    /// Queues one request. Fails only when the worker thread is gone.
    pub fn send(&self, request: SyncRequest) -> Result<()> {
        self.requests
            .send(request)
            .context("the Azure DevOps sync worker stopped")
    }

    /// The next finished event, if one is waiting.
    pub fn try_event(&self) -> Option<SyncEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                (!self.stopped.replace(true)).then_some(SyncEvent::Stopped)
            }
        }
    }
}

fn work(
    database: PathBuf,
    connector: Box<dyn SourceConnector>,
    requests: &Receiver<SyncRequest>,
    events: &Sender<SyncEvent>,
) {
    let mut worker = Worker {
        database,
        connector,
        source: None,
        repository: None,
    };
    while let Ok(request) = requests.recv() {
        let event = match request {
            SyncRequest::Pull(origin) => SyncEvent::Finished {
                origin,
                outcome: worker.pull(events),
            },
            SyncRequest::Edit(request) => {
                SyncEvent::Edited(Box::new(worker.edit(&request, events)))
            }
        };
        if events.send(event).is_err() {
            break;
        }
    }
}

/// The sync thread's own state: its work-item source and its own connection to
/// the database, both opened on first use and kept for the process's life.
struct Worker {
    database: PathBuf,
    connector: Box<dyn SourceConnector>,
    source: Option<Box<dyn WorkItemSource>>,
    repository: Option<SqliteTicketRepository>,
}

impl Worker {
    fn pull(&mut self, events: &Sender<SyncEvent>) -> SyncOutcome {
        match self.try_pull(events) {
            Ok((prepared, count)) => SyncOutcome::Pulled { prepared, count },
            Err(error) => SyncOutcome::Failed(format!("{error:#}")),
        }
    }

    /// Writes one field back to Azure DevOps. A refusal is reported as itself
    /// rather than as a failed sync, because the row it belongs to has to be
    /// put back on the main thread.
    fn edit(
        &mut self,
        request: &EditRequest,
        events: &Sender<SyncEvent>,
    ) -> Result<EditApplied, EditRejection> {
        self.try_edit(request, events)
            .map_err(|error| EditRejection {
                key: request.key.clone(),
                label: request.edit.label().to_owned(),
                conflict: azure::is_write_conflict(&error),
                message: format!("{error:#}"),
            })
    }

    fn try_edit(
        &mut self,
        request: &EditRequest,
        events: &Sender<SyncEvent>,
    ) -> Result<EditApplied> {
        let (ticket, relations) = self
            .source(events)?
            .patch_work_item(request.key.id, &request.document())?;
        self.repository()?.upsert(&ticket, &relations)?;
        Ok(EditApplied {
            ticket,
            relations,
            edit: request.edit.clone(),
        })
    }

    fn try_pull(&mut self, events: &Sender<SyncEvent>) -> Result<(PreparedTickets, usize)> {
        let batch = self.source(events)?.pull()?;
        let graph = TicketGraph {
            relations: batch.relations,
            ..TicketGraph::default()
        };
        let repository = self.repository()?;
        let count = repository.replace_all(&batch.tickets, &graph)?;
        let tickets = repository.load_all()?;
        let graph = repository.load_graph()?;
        Ok((PreparedTickets::with_graph(tickets, graph), count))
    }

    /// The work-item source, connecting on first use. The signed-in display
    /// name goes out once, right after that first connect.
    fn source(&mut self, events: &Sender<SyncEvent>) -> Result<&dyn WorkItemSource> {
        if self.source.is_none() {
            let source = self.connector.connect()?;
            if let Ok(Some(name)) = source.display_name() {
                let _ = events.send(SyncEvent::DisplayName(name));
            }
            self.source = Some(source);
        }
        Ok(self
            .source
            .as_deref()
            .expect("the source was just connected"))
    }

    /// The worker's own database handle. It never rebuilds the schema: the
    /// process that opened the database owns that decision.
    fn repository(&mut self) -> Result<&mut SqliteTicketRepository> {
        if self.repository.is_none() {
            self.repository = Some(SqliteTicketRepository::open_existing(&self.database)?);
        }
        Ok(self
            .repository
            .as_mut()
            .expect("the database was just opened"))
    }
}

/// When the next background pull is due, and whether one is already running.
/// This lives on the main thread so the timer needs no threads to test.
#[derive(Clone, Copy, Debug)]
pub struct SyncScheduler {
    interval: Option<Duration>,
    next_due: Option<Instant>,
    in_flight: bool,
}

impl SyncScheduler {
    /// `None` disables the timer, leaving only pulls the user asks for.
    #[must_use]
    pub const fn new(interval: Option<Duration>) -> Self {
        Self {
            interval,
            next_due: None,
            in_flight: false,
        }
    }

    #[must_use]
    pub const fn in_flight(&self) -> bool {
        self.in_flight
    }

    /// Ask for a pull at the next turn of the event loop.
    pub const fn schedule_now(&mut self, now: Instant) {
        self.next_due = Some(now);
    }

    /// Ask for a pull one interval from now, or for none at all when the timer
    /// is disabled or `--refresh` names an interval no clock can reach.
    pub fn schedule_next(&mut self, now: Instant) {
        self.next_due = self.interval.and_then(|interval| now.checked_add(interval));
    }

    /// Whether the event loop should send a pull. Never true while one is in
    /// flight, so pulls can not pile up behind a slow network.
    #[must_use]
    pub fn due(&self, now: Instant) -> bool {
        !self.in_flight && self.next_due.is_some_and(|due| now >= due)
    }

    /// Marks a pull as started, whatever asked for it.
    pub const fn start(&mut self) {
        self.in_flight = true;
        self.next_due = None;
    }

    /// A keypress asking for a pull: `false` when one is already running, so
    /// the press is reported rather than queued.
    pub const fn request_user_pull(&mut self) -> bool {
        if self.in_flight {
            return false;
        }
        self.start();
        true
    }

    /// Marks the pull in flight as finished and books the next timer pull.
    pub fn finish(&mut self, now: Instant) {
        self.in_flight = false;
        self.schedule_next(now);
    }

    /// Gives up on the timer entirely, for when the worker is gone.
    pub const fn stop(&mut self) {
        self.interval = None;
        self.next_due = None;
        self.in_flight = false;
    }

    /// How long the event loop may sleep before the next pull is due.
    #[must_use]
    pub fn time_until_due(&self, now: Instant) -> Option<Duration> {
        if self.in_flight {
            return None;
        }
        self.next_due.map(|due| due.saturating_duration_since(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::azure::RequestRejected;
    use crate::edit::FieldEdit;
    use crate::model::{RelationKind, RelationRecord, Ticket, TicketKey};
    use crate::timestamp::ts;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tempfile::{TempDir, tempdir};

    fn ticket(id: i64, title: &str) -> Ticket {
        Ticket {
            key: TicketKey {
                organization: "demo".into(),
                id,
            },
            project: "atlas".into(),
            revision: 1,
            work_item_type: "Task".into(),
            title: title.into(),
            state: "Active".into(),
            reason: None,
            assigned_to: Some("Avery Chen".into()),
            priority: Some(2),
            area_path: "Atlas".into(),
            iteration_path: "Atlas\\Sprint 1".into(),
            tags: Vec::new(),
            description: String::new(),
            created_at: ts("2026-01-01T00:00:00Z"),
            changed_at: ts("2026-02-01T00:00:00Z"),
            web_url: format!("https://dev.azure.com/demo/atlas/_workitems/edit/{id}"),
        }
    }

    /// One write the worker made: the work item id, and the document it sent.
    type SentPatch = (i64, Vec<Value>);

    /// A scripted stand-in for Azure DevOps: each pull takes the next result, and
    /// a write answers with the stored copy or with the refusal it was given.
    #[derive(Clone, Default)]
    struct FakeSource {
        results: Arc<Mutex<Vec<Result<SyncBatch, String>>>>,
        /// The copy a write answers with, and the links that come with it.
        stored: Option<(Ticket, Vec<RelationRecord>)>,
        /// The status and message a write is refused with instead.
        refusal: Option<(u16, String)>,
        /// Every patch document the worker sent.
        patches: Arc<Mutex<Vec<SentPatch>>>,
        display_name: Option<String>,
    }

    impl FakeSource {
        fn with(results: Vec<Result<SyncBatch, String>>) -> Self {
            Self {
                results: Arc::new(Mutex::new(results)),
                display_name: Some("Jacob Ragsdale".into()),
                ..Self::default()
            }
        }

        fn storing(ticket: Ticket, relations: Vec<RelationRecord>) -> Self {
            Self {
                stored: Some((ticket, relations)),
                ..Self::with(vec![Ok(SyncBatch::default())])
            }
        }

        fn refusing(status: u16, message: &str) -> Self {
            Self {
                refusal: Some((status, message.to_owned())),
                ..Self::with(vec![Ok(SyncBatch::default())])
            }
        }
    }

    impl WorkItemSource for FakeSource {
        fn pull(&self) -> Result<SyncBatch> {
            let mut results = self.results.lock().unwrap();
            match results.remove(0) {
                Ok(batch) => Ok(batch),
                Err(message) => Err(anyhow::anyhow!(message)),
            }
        }

        fn display_name(&self) -> Result<Option<String>> {
            Ok(self.display_name.clone())
        }

        fn patch_work_item(
            &self,
            id: i64,
            patch: &[Value],
        ) -> Result<(Ticket, Vec<RelationRecord>)> {
            self.patches.lock().unwrap().push((id, patch.to_vec()));
            if let Some((status, message)) = &self.refusal {
                return Err(anyhow::Error::new(RequestRejected::new(
                    *status,
                    format!("https://dev.azure.com/demo/_apis/wit/workitems/{id}"),
                    message.clone(),
                )));
            }
            self.stored
                .clone()
                .context("the fake source was not given a stored copy")
        }
    }

    impl SourceConnector for FakeSource {
        fn connect(&mut self) -> Result<Box<dyn WorkItemSource>> {
            Ok(Box::new(self.clone()))
        }
    }

    /// A database holding one ticket, plus the worker reading and writing it.
    fn seeded_database(directory: &TempDir) -> PathBuf {
        seeded_database_of(directory, &[ticket(1, "Existing")])
    }

    fn seeded_database_of(directory: &TempDir, tickets: &[Ticket]) -> PathBuf {
        let path = directory.path().join("tickets.sqlite3");
        let mut repository = SqliteTicketRepository::open(&path).unwrap();
        repository
            .replace_all(tickets, &TicketGraph::default())
            .unwrap();
        path
    }

    fn edited(handle: &SyncHandle) -> Result<EditApplied, EditRejection> {
        loop {
            match next_event(handle) {
                SyncEvent::Edited(result) => return *result,
                SyncEvent::DisplayName(_) => continue,
                other => panic!("expected an edit to finish, got {other:?}"),
            }
        }
    }

    fn edit_request(id: i64, state: &str, expected_revision: i64) -> EditRequest {
        EditRequest {
            key: TicketKey {
                organization: "demo".into(),
                id,
            },
            expected_revision,
            edit: FieldEdit::state(state),
        }
    }

    fn next_event(handle: &SyncHandle) -> SyncEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(event) = handle.try_event() {
                return event;
            }
            assert!(Instant::now() < deadline, "the sync worker timed out");
            thread::yield_now();
        }
    }

    fn finished(handle: &SyncHandle) -> (PullOrigin, SyncOutcome) {
        match next_event(handle) {
            SyncEvent::Finished { origin, outcome } => (origin, outcome),
            other => panic!("expected a finished pull, got {other:?}"),
        }
    }

    #[test]
    fn a_pull_replaces_the_database_and_hands_back_what_it_wrote() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let batch = SyncBatch {
            tickets: vec![ticket(7, "Pulled"), ticket(8, "Also pulled")],
            relations: vec![RelationRecord {
                from: ticket(8, "Also pulled").key,
                to: ticket(7, "Pulled").key,
                kind: RelationKind::Parent,
            }],
        };
        let handle = SyncHandle::spawn(
            path.clone(),
            Box::new(FakeSource::with(vec![Ok(batch), Ok(SyncBatch::default())])),
        )
        .unwrap();

        handle.send(SyncRequest::Pull(PullOrigin::Timer)).unwrap();
        assert!(
            matches!(next_event(&handle), SyncEvent::DisplayName(name) if name == "Jacob Ragsdale"),
            "the first connect reports who is signed in"
        );
        let (origin, outcome) = finished(&handle);
        assert_eq!(origin, PullOrigin::Timer);
        let SyncOutcome::Pulled { prepared, count } = outcome else {
            panic!("expected a successful pull");
        };
        assert_eq!(count, 2);
        assert_eq!(prepared.ticket_count(), 2);

        let stored = SqliteTicketRepository::open_existing(&path).unwrap();
        let mut titles: Vec<String> = stored
            .load_all()
            .unwrap()
            .into_iter()
            .map(|ticket| ticket.title)
            .collect();
        titles.sort();
        assert_eq!(titles, ["Also pulled", "Pulled"]);
        assert_eq!(stored.load_graph().unwrap().relations.len(), 1);

        handle.send(SyncRequest::Pull(PullOrigin::User)).unwrap();
        let (origin, _) = finished(&handle);
        assert_eq!(
            origin,
            PullOrigin::User,
            "the display name is reported once, not on every pull"
        );
    }

    #[test]
    fn a_failed_pull_reports_the_error_and_leaves_the_rows_alone() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let handle = SyncHandle::spawn(
            path.clone(),
            Box::new(FakeSource::with(vec![Err("network unreachable".into())])),
        )
        .unwrap();

        handle.send(SyncRequest::Pull(PullOrigin::Timer)).unwrap();
        let outcome = loop {
            match next_event(&handle) {
                SyncEvent::Finished { outcome, .. } => break outcome,
                SyncEvent::DisplayName(_) => continue,
                other => panic!("expected a finished pull, got {other:?}"),
            }
        };
        let SyncOutcome::Failed(error) = outcome else {
            panic!("expected a failed pull");
        };
        assert!(error.contains("network unreachable"), "{error}");

        let stored = SqliteTicketRepository::open_existing(&path).unwrap();
        assert_eq!(stored.load_all().unwrap()[0].title, "Existing");
    }

    #[test]
    fn an_edit_writes_one_row_and_hands_back_the_copy_azure_devops_stored() {
        let directory = tempdir().unwrap();
        let path = seeded_database_of(&directory, &[ticket(1, "Existing"), ticket(2, "Untouched")]);
        let mut stored = ticket(1, "Existing");
        stored.state = "Done".into();
        stored.revision = 7;
        let relation = RelationRecord {
            from: stored.key.clone(),
            to: ticket(2, "Untouched").key,
            kind: RelationKind::Parent,
        };
        let source = FakeSource::storing(stored.clone(), vec![relation.clone()]);
        let patches = Arc::clone(&source.patches);
        let handle = SyncHandle::spawn(path.clone(), Box::new(source)).unwrap();

        handle
            .send(SyncRequest::Edit(edit_request(1, "Done", 1)))
            .unwrap();
        let applied = edited(&handle).expect("the write was accepted");

        assert_eq!(applied.ticket, stored);
        assert_eq!(applied.relations, vec![relation.clone()]);
        assert_eq!(applied.edit.summary(), "State → Done");
        assert_eq!(
            *patches.lock().unwrap(),
            vec![(
                1,
                vec![
                    json!({"op": "test", "path": "/rev", "value": 1}),
                    json!({"op": "add", "path": "/fields/System.State", "value": "Done"}),
                ]
            )],
            "the revision test leads the document"
        );

        let database = SqliteTicketRepository::open_existing(&path).unwrap();
        let mut rows = database.load_all().unwrap();
        rows.sort_by_key(|ticket| ticket.key.id);
        assert_eq!(
            rows,
            vec![stored, ticket(2, "Untouched")],
            "an edit writes its own row and leaves the others alone"
        );
        assert_eq!(database.load_graph().unwrap().relations, vec![relation]);
    }

    #[test]
    fn a_refused_edit_reports_a_conflict_only_when_the_work_item_moved_on() {
        let directory = tempdir().unwrap();
        let conflict = |status, message: &str| {
            let path = seeded_database(&directory);
            let handle = SyncHandle::spawn(
                path.clone(),
                Box::new(FakeSource::refusing(status, message)),
            )
            .unwrap();
            handle
                .send(SyncRequest::Edit(edit_request(1, "Done", 1)))
                .unwrap();
            let rejection = edited(&handle).expect_err("the write was refused");
            assert_eq!(rejection.key.id, 1);
            assert_eq!(rejection.label, "State");
            assert!(rejection.message.contains(message), "{}", rejection.message);
            assert_eq!(
                SqliteTicketRepository::open_existing(&path)
                    .unwrap()
                    .load_all()
                    .unwrap()[0]
                    .state,
                "Active",
                "a refused write never reaches the database"
            );
            rejection.conflict
        };

        assert!(conflict(409, "the work item has been changed"));
        assert!(conflict(
            400,
            r#"The "test" operation for path "/rev" failed"#
        ));
        assert!(!conflict(403, "field is read only"));
    }

    #[test]
    fn an_edit_queued_before_a_pull_is_written_before_that_pull_reads() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let mut stored = ticket(1, "Existing");
        stored.state = "Done".into();
        let handle =
            SyncHandle::spawn(path, Box::new(FakeSource::storing(stored, vec![]))).unwrap();

        handle
            .send(SyncRequest::Edit(edit_request(1, "Done", 1)))
            .unwrap();
        handle.send(SyncRequest::Pull(PullOrigin::User)).unwrap();

        let mut seen = Vec::new();
        while seen.len() < 2 {
            match next_event(&handle) {
                SyncEvent::Edited(result) => {
                    result.expect("the write was accepted");
                    seen.push("edit");
                }
                SyncEvent::Finished { .. } => seen.push("pull"),
                SyncEvent::DisplayName(_) => continue,
                SyncEvent::Stopped => panic!("the worker stopped early"),
            }
        }
        assert_eq!(seen, ["edit", "pull"], "requests are answered in order");
    }

    #[test]
    fn the_timer_books_one_pull_at_a_time() {
        let start = Instant::now();
        let mut scheduler = SyncScheduler::new(Some(Duration::from_secs(60)));
        assert!(
            !scheduler.due(start),
            "nothing is due until one is scheduled"
        );

        scheduler.schedule_now(start);
        assert!(scheduler.due(start));
        assert_eq!(scheduler.time_until_due(start), Some(Duration::ZERO));

        scheduler.start();
        assert!(scheduler.in_flight());
        assert!(
            !scheduler.due(start + Duration::from_secs(600)),
            "a pull in flight blocks the next one"
        );
        assert_eq!(scheduler.time_until_due(start), None);

        scheduler.finish(start);
        assert!(!scheduler.due(start + Duration::from_secs(59)));
        assert_eq!(
            scheduler.time_until_due(start + Duration::from_secs(59)),
            Some(Duration::from_secs(1))
        );
        assert!(scheduler.due(start + Duration::from_secs(60)));
    }

    #[test]
    fn a_second_request_while_one_runs_is_refused_rather_than_queued() {
        let mut scheduler = SyncScheduler::new(Some(Duration::from_secs(60)));
        assert!(scheduler.request_user_pull());
        assert!(
            !scheduler.request_user_pull(),
            "the second press reports the pull already running"
        );
        scheduler.finish(Instant::now());
        assert!(scheduler.request_user_pull());
    }

    #[test]
    fn a_disabled_timer_still_allows_a_pull_that_was_asked_for() {
        let start = Instant::now();
        let mut scheduler = SyncScheduler::new(None);

        scheduler.schedule_now(start);
        assert!(
            scheduler.due(start),
            "a rebuilt schema books one pull even with the timer off"
        );
        scheduler.start();
        scheduler.finish(start);
        assert!(!scheduler.due(start + Duration::from_secs(86_400)));
        assert_eq!(scheduler.time_until_due(start), None);

        assert!(
            scheduler.request_user_pull(),
            "the sync keypress works whatever the interval is"
        );
    }
}
