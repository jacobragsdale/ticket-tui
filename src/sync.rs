//! Background sync with Azure DevOps: a worker thread pulls work items on
//! request, writes them to SQLite, and hands the reloaded rows back to the
//! main thread, which owns the timer that decides when to ask.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::slice;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::app::PreparedTickets;
use crate::azure::{self, AzureClient, AzureConfig, SyncBatch};
use crate::classification::ClassificationNode;
use crate::db::{self, SqliteTicketRepository};
use crate::edit::{EditApplied, EditRejection, EditRequest};
use crate::model::{
    DetailsUpdate, Identity, RelationRecord, StateOption, Ticket, TicketGraph, TicketKey,
    WorkItemDetails,
};
use crate::timestamp::Timestamp;

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
    /// Read one work item's comments and revision history, for a work item
    /// whose stored details are behind the revision on screen.
    Details(TicketKey),
    /// Read the project's team members, for the assignee picker. Asked for once
    /// a session, the first time that picker opens.
    Identities,
    /// Read the project's iteration and area trees, for the two node pickers.
    /// Asked for once a session, the first time either one opens on a cache
    /// that is empty or over an hour old.
    ClassificationNodes,
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
    /// One work item's comments and revision history were read, or could not
    /// be.
    Details(Box<DetailsOutcome>),
    /// The project's team members, already stored. Empty when they could not be
    /// read, which is not worth reporting: the assignee picker already offers
    /// everybody the database has seen.
    Identities(Vec<Identity>),
    /// The project's iteration and area trees, flattened and already stored.
    /// Empty when they could not be read, which is not worth reporting: both
    /// pickers already offer every path the database has seen.
    ClassificationNodes(Vec<ClassificationNode>),
    /// The worker thread is gone and no further events will arrive.
    Stopped,
}

/// How one details request ended. A failure names the work item it was for, so
/// the main thread can stop asking about that one rather than about all of
/// them.
#[derive(Debug)]
pub enum DetailsOutcome {
    Fetched(DetailsUpdate),
    Failed { key: TicketKey, message: String },
}

/// How much of the project one pull asked Azure DevOps for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncMode {
    /// Every work item, replacing the stored rows wholesale. Used when there is
    /// no watermark to start from: a fresh file, a rebuilt schema, or
    /// `ticket-tui --sync`.
    Full,
    /// Only the work items edited since the stored watermark, plus whatever the
    /// project no longer lists.
    Incremental,
}

/// How one request ended.
#[derive(Debug)]
pub enum SyncOutcome {
    /// Work items already written to SQLite and read back from it, so memory
    /// and the database hold the same rows.
    Pulled {
        prepared: PreparedTickets,
        mode: SyncMode,
        /// Work items stored, for a full pull; work items changed or removed,
        /// for an incremental one.
        count: usize,
    },
    /// Azure DevOps was reached and had nothing new: the database was not
    /// written, so the rows already in memory still match it and nothing is
    /// reloaded. The pull still happened, so the last-synced time moves.
    Unchanged,
    Failed(String),
}

/// The greatest `System.ChangedDate` in a batch, which is where the next
/// incremental pull starts. Taking it from the work items rather than from the
/// clock is the whole point: a client whose clock runs fast would otherwise
/// skip past edits it never saw.
#[must_use]
pub fn watermark_of(tickets: &[Ticket]) -> Option<Timestamp> {
    tickets.iter().map(|ticket| ticket.changed_at).max()
}

/// Where work items come from. `AzureClient` implements it; tests use a fake so
/// the worker can be exercised without a network.
pub trait WorkItemSource {
    /// Every work item in the project.
    fn pull(&self) -> Result<SyncBatch>;
    /// Only the work items edited at or after `watermark`.
    fn pull_changed_since(&self, watermark: Timestamp) -> Result<SyncBatch>;
    /// Every work item id the project still has, which is what tells a pull
    /// which stored rows have been deleted.
    fn list_ids(&self) -> Result<Vec<i64>>;
    /// Display name of the signed-in user, used to mark their own work items.
    fn display_name(&self) -> Result<Option<String>>;
    /// Write one work item with a JSON Patch document, answering with the copy
    /// the server stored.
    fn patch_work_item(&self, id: i64, patch: &[Value]) -> Result<(Ticket, Vec<RelationRecord>)>;
    /// The states one work item type allows, which is what the state picker
    /// offers once a pull has cached them.
    fn work_item_type_states(&self, work_item_type: &str) -> Result<Vec<StateOption>>;
    /// One work item's comments and revision history. A source that cannot
    /// read them answers with none, which leaves the details pane showing what
    /// the work item itself says and nothing more.
    fn fetch_details(&self, _id: i64) -> Result<WorkItemDetails> {
        Ok(WorkItemDetails::default())
    }
    /// Everybody on the project's teams, which the assignee picker offers
    /// alongside the people already assigned work. A source that cannot list
    /// them answers with nobody, and the picker is none the worse for it.
    fn team_members(&self) -> Result<Vec<Identity>> {
        Ok(Vec::new())
    }
    /// The project's iteration and area trees, flattened, which the two node
    /// pickers offer alongside the paths the work items already carry. A source
    /// that cannot read them answers with nothing, and the pickers fall back.
    fn classification_nodes(&self) -> Result<Vec<ClassificationNode>> {
        Ok(Vec::new())
    }
}

impl WorkItemSource for AzureClient {
    fn pull(&self) -> Result<SyncBatch> {
        self.fetch_all_work_items()
    }

    fn pull_changed_since(&self, watermark: Timestamp) -> Result<SyncBatch> {
        self.fetch_changed_work_items(watermark)
    }

    fn list_ids(&self) -> Result<Vec<i64>> {
        self.query_ids()
    }

    fn display_name(&self) -> Result<Option<String>> {
        self.current_user_display_name()
    }

    fn patch_work_item(&self, id: i64, patch: &[Value]) -> Result<(Ticket, Vec<RelationRecord>)> {
        self.update_work_item(id, patch)
    }

    fn work_item_type_states(&self, work_item_type: &str) -> Result<Vec<StateOption>> {
        self.fetch_work_item_type_states(work_item_type)
    }

    fn fetch_details(&self, id: i64) -> Result<WorkItemDetails> {
        self.fetch_work_item_details(id)
    }

    fn team_members(&self) -> Result<Vec<Identity>> {
        self.fetch_team_members()
    }

    fn classification_nodes(&self) -> Result<Vec<ClassificationNode>> {
        self.fetch_classification_nodes()
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
        typed_states: HashSet::new(),
        typed_states_seeded: false,
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
            SyncRequest::Details(key) => SyncEvent::Details(Box::new(worker.details(key, events))),
            SyncRequest::Identities => SyncEvent::Identities(worker.identities(events)),
            SyncRequest::ClassificationNodes => {
                SyncEvent::ClassificationNodes(worker.classification_nodes(events))
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
    /// Work item types whose states are already cached, so the states endpoint
    /// is asked once per type per run rather than once per pull.
    typed_states: HashSet<String>,
    /// Whether the types the database already knows about have been folded into
    /// `typed_states`, which happens on the first pull of the run.
    typed_states_seeded: bool,
}

impl Worker {
    fn pull(&mut self, events: &Sender<SyncEvent>) -> SyncOutcome {
        match self.try_pull(events) {
            Ok(outcome) => outcome,
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

    /// Reads one work item's comments and revision history and stores them
    /// against the revision the database currently holds for it. A failure
    /// names the work item rather than the whole sync: nothing else is wrong.
    fn details(&mut self, key: TicketKey, events: &Sender<SyncEvent>) -> DetailsOutcome {
        match self.try_details(&key, events) {
            Ok(update) => DetailsOutcome::Fetched(update),
            Err(error) => DetailsOutcome::Failed {
                key,
                message: format!("{error:#}"),
            },
        }
    }

    fn try_details(
        &mut self,
        key: &TicketKey,
        events: &Sender<SyncEvent>,
    ) -> Result<DetailsUpdate> {
        // The revision is read before the fetch, so details stored against it
        // can only ever be treated as older than the work item, never newer.
        let revision = self
            .repository()?
            .revision_of(key)?
            .with_context(|| format!("work item {} is no longer in the database", key.id))?;
        let details = self.source(events)?.fetch_details(key.id)?;
        let update = DetailsUpdate {
            key: key.clone(),
            revision,
            details,
        };
        self.repository()?
            .replace_details(slice::from_ref(&update))?;
        Ok(update)
    }

    /// Reads the comments and revision history of every work item a pull
    /// brought back. A work item whose details could not be read is left
    /// without any, so its `details_rev` stays behind and the selection
    /// landing on it asks again; the pull itself still lands.
    fn details_for(
        &mut self,
        tickets: &[Ticket],
        events: &Sender<SyncEvent>,
    ) -> Result<Vec<DetailsUpdate>> {
        let mut updates = Vec::with_capacity(tickets.len());
        for ticket in tickets {
            let Ok(details) = self.source(events)?.fetch_details(ticket.key.id) else {
                continue;
            };
            updates.push(DetailsUpdate {
                key: ticket.key.clone(),
                revision: ticket.revision,
                details,
            });
        }
        Ok(updates)
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

    /// Reads the project's team members and stores them for the next session,
    /// answering with what was stored. A failure answers with nobody and says
    /// nothing: the assignee picker already offers everybody the database has
    /// ever seen a work item assigned to, which is the whole team in a small
    /// project, and a toast about an endpoint nobody asked for would only be
    /// noise.
    fn identities(&mut self, events: &Sender<SyncEvent>) -> Vec<Identity> {
        let members = match self.source(events) {
            Ok(source) => source.team_members().unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        if members.is_empty() {
            return members;
        }
        if let Ok(repository) = self.repository() {
            drop(repository.replace_identities(&members));
        }
        members
    }

    /// Reads both classification trees and stores them for the next session,
    /// answering with what was stored. Like the team members, a failure answers
    /// with nothing and says nothing: the pickers already offer every iteration
    /// and area the database has a work item in, which is enough to move work
    /// between the sprints that are actually in use.
    fn classification_nodes(&mut self, events: &Sender<SyncEvent>) -> Vec<ClassificationNode> {
        let nodes = match self.source(events) {
            Ok(source) => source.classification_nodes().unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        if nodes.is_empty() {
            return nodes;
        }
        if let Ok(repository) = self.repository()
            && repository.replace_classification_nodes(&nodes).is_ok()
        {
            drop(repository.set_meta(
                db::CLASSIFICATION_FETCHED_KEY,
                &Timestamp::now().to_rfc3339(),
            ));
        }
        nodes
    }

    /// A pull starts from the watermark the last one left behind. Without one —
    /// a fresh file, a schema the current build rebuilt, or a value written by
    /// something that can no longer be read — there is no safe starting point,
    /// so everything comes down once and leaves a watermark for next time.
    fn try_pull(&mut self, events: &Sender<SyncEvent>) -> Result<SyncOutcome> {
        match self.watermark()? {
            Some(watermark) => self.pull_changed(watermark, events),
            None => self.pull_everything(events),
        }
    }

    fn watermark(&mut self) -> Result<Option<Timestamp>> {
        let Some(stored) = self.repository()?.meta(db::WATERMARK_KEY)? else {
            return Ok(None);
        };
        Ok(Timestamp::parse(&stored).ok())
    }

    /// Replaces every stored work item with a fresh copy of the project.
    fn pull_everything(&mut self, events: &Sender<SyncEvent>) -> Result<SyncOutcome> {
        let batch = self.source(events)?.pull()?;
        let graph = TicketGraph {
            relations: batch.relations,
            ..TicketGraph::default()
        };
        let types = self.uncached_types(&batch.tickets)?;
        let watermark = watermark_of(&batch.tickets);
        let repository = self.repository()?;
        let count = repository.replace_all(&batch.tickets, &graph)?;
        if let Some(watermark) = watermark {
            repository.set_meta(db::WATERMARK_KEY, &watermark.to_rfc3339())?;
        }
        self.cache_type_states(&types, events)?;
        Ok(SyncOutcome::Pulled {
            prepared: self.reload()?,
            mode: SyncMode::Full,
            count,
        })
    }

    /// Reads only what changed since `watermark`, then reconciles deletions
    /// against the project's own id list. When neither turns anything up the
    /// database is not touched at all: no write means no new data signature,
    /// so no other ticket-tui or agent reading the file reloads for nothing.
    fn pull_changed(
        &mut self,
        watermark: Timestamp,
        events: &Sender<SyncEvent>,
    ) -> Result<SyncOutcome> {
        let batch = self.source(events)?.pull_changed_since(watermark)?;
        let live_ids = self.source(events)?.list_ids()?;
        let types = self.uncached_types(&batch.tickets)?;
        // Only ever forward: the query is inclusive and rounded down to the
        // second, so a boundary work item can come back reading a shade older
        // than the watermark that asked for it.
        let next = watermark_of(&batch.tickets).filter(|next| *next > watermark);

        // A work item that moved is a work item somebody is about to look at,
        // so its comments and history come down with it and land in the same
        // transaction. A full pull does not do this: two more requests per
        // work item is a price only a handful of changes can pay.
        let details = self.details_for(&batch.tickets, events)?;

        let repository = self.repository()?;
        if !batch.tickets.is_empty() {
            repository.upsert_all(&batch.tickets, &batch.relations, &details)?;
        }
        let removed = repository.delete_missing(&live_ids)?;
        let count = batch.tickets.len() + removed;
        if count == 0 {
            return Ok(SyncOutcome::Unchanged);
        }
        if let Some(next) = next {
            repository.set_meta(db::WATERMARK_KEY, &next.to_rfc3339())?;
        }
        self.cache_type_states(&types, events)?;
        Ok(SyncOutcome::Pulled {
            prepared: self.reload()?,
            mode: SyncMode::Incremental,
            count,
        })
    }

    /// The rows, their graph, and the states they allow, all out of the same
    /// read, so what the main thread shows is what the database holds.
    fn reload(&mut self) -> Result<PreparedTickets> {
        let repository = self.repository()?;
        let tickets = repository.load_all()?;
        let graph = repository.load_graph()?;
        let states = repository.load_type_states()?;
        Ok(PreparedTickets::with_graph(tickets, graph).with_states(states))
    }

    /// The work item types in a batch whose states nobody has read yet, in the
    /// order they first appear.
    fn uncached_types(&mut self, tickets: &[Ticket]) -> Result<Vec<String>> {
        self.seed_typed_states()?;
        let mut types: Vec<String> = Vec::new();
        for ticket in tickets {
            if !self.typed_states.contains(&ticket.work_item_type)
                && !types.contains(&ticket.work_item_type)
            {
                types.push(ticket.work_item_type.clone());
            }
        }
        Ok(types)
    }

    /// Counts every type the database already holds states for as cached. A run
    /// that opens a filled database therefore asks the states endpoint for
    /// nothing at all, which is what keeps an idle incremental pull down to its
    /// two queries.
    fn seed_typed_states(&mut self) -> Result<()> {
        if self.typed_states_seeded {
            return Ok(());
        }
        self.typed_states_seeded = true;
        for work_item_type in self.repository()?.cached_state_types()? {
            self.typed_states.insert(work_item_type);
        }
        Ok(())
    }

    /// Reads the states of every work item type this pull saw for the first
    /// time and stores them. A type whose states could not be read is left
    /// uncached rather than failing the pull: the picker falls back to the
    /// states already in the database, and the next pull asks again.
    fn cache_type_states(&mut self, types: &[String], events: &Sender<SyncEvent>) -> Result<()> {
        for work_item_type in types {
            let Ok(states) = self.source(events)?.work_item_type_states(work_item_type) else {
                continue;
            };
            if states.is_empty() {
                continue;
            }
            self.repository()?
                .replace_type_states(work_item_type, &states)?;
            self.typed_states.insert(work_item_type.clone());
        }
        Ok(())
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
    use crate::classification::NodeKind;
    use crate::edit::FieldEdit;
    use crate::model::{
        CommentRecord, HistoryRecord, RelationKind, RelationRecord, StateCategory, Ticket,
        TicketKey,
    };
    use crate::timestamp::ts;
    use serde_json::json;
    use std::collections::HashMap;
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
            details_rev: 0,
        }
    }

    /// One write the worker made: the work item id, and the document it sent.
    type SentPatch = (i64, Vec<Value>);

    /// A scripted stand-in for Azure DevOps: each pull takes the next result, and
    /// a write answers with the stored copy or with the refusal it was given.
    #[derive(Clone, Default)]
    struct FakeSource {
        results: Arc<Mutex<Vec<Result<SyncBatch, String>>>>,
        /// The ids the project still lists. Unset means every id the fake has
        /// ever handed out, so nothing looks deleted.
        live_ids: Arc<Mutex<Option<Vec<i64>>>>,
        /// Every id handed out, which is what an unset `live_ids` answers with.
        handed_out: Arc<Mutex<Vec<i64>>>,
        /// The watermark each incremental pull was asked to start from.
        watermarks: Arc<Mutex<Vec<Timestamp>>>,
        /// Requests that would have gone over the wire, one per query and one
        /// per batch of work items read.
        requests: Arc<Mutex<usize>>,
        /// The copy a write answers with, and the links that come with it.
        stored: Option<(Ticket, Vec<RelationRecord>)>,
        /// The status and message a write is refused with instead.
        refusal: Option<(u16, String)>,
        /// Every patch document the worker sent.
        patches: Arc<Mutex<Vec<SentPatch>>>,
        display_name: Option<String>,
        /// The states each work item type answers with.
        type_states: Arc<Mutex<HashMap<String, Vec<StateOption>>>>,
        /// Every work item type whose states were asked for.
        asked_types: Arc<Mutex<Vec<String>>>,
        /// The comments and history each work item answers with.
        details: Arc<Mutex<HashMap<i64, WorkItemDetails>>>,
        /// Every work item whose details were read, in order.
        detailed: Arc<Mutex<Vec<i64>>>,
        /// The project's team members, or `None` for a source that cannot list
        /// them, which is what the trait's default leaves behind.
        team_members: Option<Vec<Identity>>,
        /// The project's classification trees, or `None` for a source that
        /// cannot read them.
        classification_nodes: Option<Vec<ClassificationNode>>,
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

        fn with_states(self, work_item_type: &str, states: Vec<StateOption>) -> Self {
            self.type_states
                .lock()
                .unwrap()
                .insert(work_item_type.to_owned(), states);
            self
        }

        /// What one work item's comments and history endpoints answer with.
        fn with_details(self, id: i64, details: WorkItemDetails) -> Self {
            self.details.lock().unwrap().insert(id, details);
            self
        }

        fn with_team(self, members: Vec<Identity>) -> Self {
            Self {
                team_members: Some(members),
                ..self
            }
        }

        fn with_nodes(self, nodes: Vec<ClassificationNode>) -> Self {
            Self {
                classification_nodes: Some(nodes),
                ..self
            }
        }

        /// The ids the project still lists, whatever the pulls returned.
        fn listing(self, ids: Vec<i64>) -> Self {
            *self.live_ids.lock().unwrap() = Some(ids);
            self
        }

        /// One query, plus one read per batch of work items it named.
        fn take_next_batch(&self) -> Result<SyncBatch> {
            *self.requests.lock().unwrap() += 1;
            let batch = match self.results.lock().unwrap().remove(0) {
                Ok(batch) => batch,
                Err(message) => return Err(anyhow::anyhow!(message)),
            };
            if !batch.tickets.is_empty() {
                *self.requests.lock().unwrap() += 1;
            }
            let mut handed_out = self.handed_out.lock().unwrap();
            for ticket in &batch.tickets {
                if !handed_out.contains(&ticket.key.id) {
                    handed_out.push(ticket.key.id);
                }
            }
            Ok(batch)
        }
    }

    impl WorkItemSource for FakeSource {
        fn pull(&self) -> Result<SyncBatch> {
            self.take_next_batch()
        }

        fn pull_changed_since(&self, watermark: Timestamp) -> Result<SyncBatch> {
            self.watermarks.lock().unwrap().push(watermark);
            self.take_next_batch()
        }

        fn list_ids(&self) -> Result<Vec<i64>> {
            *self.requests.lock().unwrap() += 1;
            Ok(self
                .live_ids
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| self.handed_out.lock().unwrap().clone()))
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

        fn team_members(&self) -> Result<Vec<Identity>> {
            self.team_members
                .clone()
                .context("the fake source cannot list teams")
        }

        fn classification_nodes(&self) -> Result<Vec<ClassificationNode>> {
            self.classification_nodes
                .clone()
                .context("the fake source cannot read classification nodes")
        }

        fn work_item_type_states(&self, work_item_type: &str) -> Result<Vec<StateOption>> {
            *self.requests.lock().unwrap() += 1;
            self.asked_types
                .lock()
                .unwrap()
                .push(work_item_type.to_owned());
            self.type_states
                .lock()
                .unwrap()
                .get(work_item_type)
                .cloned()
                .with_context(|| format!("no states for {work_item_type}"))
        }

        /// Two requests over the wire: one page of comments and one of updates.
        fn fetch_details(&self, id: i64) -> Result<WorkItemDetails> {
            *self.requests.lock().unwrap() += 2;
            self.detailed.lock().unwrap().push(id);
            Ok(self
                .details
                .lock()
                .unwrap()
                .get(&id)
                .cloned()
                .unwrap_or_default())
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

    /// A database a previous pull already left a watermark in, which is what
    /// makes the next pull incremental.
    fn watermarked_database(
        directory: &TempDir,
        tickets: &[Ticket],
        graph: &TicketGraph,
        watermark: &str,
    ) -> PathBuf {
        let path = directory.path().join("tickets.sqlite3");
        let mut repository = SqliteTicketRepository::open(&path).unwrap();
        repository.replace_all(tickets, graph).unwrap();
        repository.set_meta(db::WATERMARK_KEY, watermark).unwrap();
        path
    }

    fn stored_watermark(path: &PathBuf) -> Option<String> {
        SqliteTicketRepository::open_existing(path)
            .unwrap()
            .meta(db::WATERMARK_KEY)
            .unwrap()
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

    /// The outcome of the next pull, past the display name the first connect
    /// reports.
    fn pulled(handle: &SyncHandle) -> SyncOutcome {
        loop {
            match next_event(handle) {
                SyncEvent::Finished { outcome, .. } => return outcome,
                SyncEvent::DisplayName(_) => continue,
                other => panic!("expected a finished pull, got {other:?}"),
            }
        }
    }

    /// How the next details request ended, past the display name the first
    /// connect reports.
    fn detailed(handle: &SyncHandle) -> DetailsOutcome {
        loop {
            match next_event(handle) {
                SyncEvent::Details(outcome) => return *outcome,
                SyncEvent::DisplayName(_) => continue,
                other => panic!("expected a details fetch to finish, got {other:?}"),
            }
        }
    }

    /// One work item's comments and history, as a fake source hands them over.
    fn details_of(key: &TicketKey) -> WorkItemDetails {
        WorkItemDetails {
            comments: vec![CommentRecord {
                ticket: key.clone(),
                comment_id: 5,
                created_at: ts("2026-03-04T00:00:00Z"),
                author: Some("Avery Chen".into()),
                text: "Looks good".into(),
            }],
            history: vec![HistoryRecord {
                ticket: key.clone(),
                revision: 4,
                changed_at: ts("2026-03-05T10:00:00Z"),
                changed_by: Some("Jacob Ragsdale".into()),
                field_name: "State".into(),
                old_value: Some("To Do".into()),
                new_value: Some("Doing".into()),
            }],
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
        let SyncOutcome::Pulled {
            prepared,
            mode,
            count,
        } = outcome
        else {
            panic!("expected a successful pull");
        };
        assert_eq!(
            mode,
            SyncMode::Full,
            "a database with no watermark is filled whole"
        );
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
    fn a_pull_caches_the_states_of_every_work_item_type_it_saw_once() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let mut bug = ticket(9, "A bug");
        bug.work_item_type = "Bug".into();
        let batch = || SyncBatch {
            tickets: vec![ticket(7, "Pulled"), ticket(8, "Also pulled"), bug.clone()],
            relations: Vec::new(),
        };
        let task_states = vec![
            StateOption::new("To Do", StateCategory::Proposed),
            StateOption::new("Doing", StateCategory::InProgress),
            StateOption::new("Done", StateCategory::Completed),
        ];
        let source = FakeSource::with(vec![Ok(batch()), Ok(batch())])
            .with_states("Task", task_states.clone());
        let asked = Arc::clone(&source.asked_types);
        let handle = SyncHandle::spawn(path.clone(), Box::new(source)).unwrap();

        handle.send(SyncRequest::Pull(PullOrigin::Timer)).unwrap();
        let SyncOutcome::Pulled { prepared, .. } = pulled(&handle) else {
            panic!("expected a successful pull");
        };
        assert_eq!(
            prepared.states().states_for("Task"),
            task_states,
            "the rows and the states they allow come out of the same read"
        );
        assert_eq!(
            SqliteTicketRepository::open_existing(&path)
                .unwrap()
                .load_type_states()
                .unwrap()
                .states_for("Task"),
            task_states,
            "the worker wrote them where the picker reads them"
        );

        handle.send(SyncRequest::Pull(PullOrigin::User)).unwrap();
        pulled(&handle);
        assert_eq!(
            *asked.lock().unwrap(),
            ["Task", "Bug", "Bug"],
            "a cached type is asked once; a type whose states failed is asked again"
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
    fn an_incremental_pull_writes_only_what_changed_and_advances_the_watermark() {
        let directory = tempdir().unwrap();
        let path = watermarked_database(
            &directory,
            &[ticket(1, "One"), ticket(2, "Two"), ticket(3, "Three")],
            &TicketGraph::default(),
            "2026-02-01T00:00:00Z",
        );
        let mut changed = ticket(2, "Changed in Azure DevOps");
        changed.state = "Done".into();
        changed.changed_at = ts("2026-03-05T10:00:00Z");
        let source = FakeSource::with(vec![Ok(SyncBatch {
            relations: vec![RelationRecord {
                from: changed.key.clone(),
                to: ticket(1, "One").key,
                kind: RelationKind::Parent,
            }],
            tickets: vec![changed.clone()],
        })])
        .listing(vec![1, 2, 3]);
        let watermarks = Arc::clone(&source.watermarks);
        let handle = SyncHandle::spawn(path.clone(), Box::new(source)).unwrap();

        handle.send(SyncRequest::Pull(PullOrigin::User)).unwrap();
        let SyncOutcome::Pulled { mode, count, .. } = pulled(&handle) else {
            panic!("expected a successful pull");
        };
        assert_eq!(mode, SyncMode::Incremental);
        assert_eq!(count, 1, "one work item moved, so one was transferred");
        assert_eq!(
            *watermarks.lock().unwrap(),
            [ts("2026-02-01T00:00:00Z")],
            "the pull asked from where the last one stopped"
        );

        let stored = SqliteTicketRepository::open_existing(&path).unwrap();
        let mut rows = stored.load_all().unwrap();
        rows.sort_by_key(|ticket| ticket.key.id);
        // The pull read the changed work item's details as well, so its row
        // says which revision they belong to.
        let mut expected = changed.clone();
        expected.details_rev = changed.revision;
        assert_eq!(
            rows,
            vec![ticket(1, "One"), expected, ticket(3, "Three")],
            "the rows nobody touched are left exactly as they were"
        );
        assert_eq!(stored.load_graph().unwrap().relations.len(), 1);
        assert_eq!(
            stored_watermark(&path),
            Some(changed.changed_at.to_rfc3339()),
            "the watermark is the batch's own greatest ChangedDate, never the clock"
        );
    }

    #[test]
    fn an_incremental_pull_reads_the_details_of_what_moved_and_of_nothing_else() {
        let directory = tempdir().unwrap();
        let path = watermarked_database(
            &directory,
            &[ticket(1, "One"), ticket(2, "Two")],
            &TicketGraph::default(),
            "2026-02-01T00:00:00Z",
        );
        let mut changed = ticket(2, "Changed in Azure DevOps");
        changed.revision = 4;
        changed.changed_at = ts("2026-03-05T10:00:00Z");
        let details = details_of(&changed.key);
        let source = FakeSource::with(vec![Ok(SyncBatch {
            tickets: vec![changed.clone()],
            relations: Vec::new(),
        })])
        .listing(vec![1, 2])
        .with_details(2, details.clone());
        let read = Arc::clone(&source.detailed);
        let handle = SyncHandle::spawn(path.clone(), Box::new(source)).unwrap();

        handle.send(SyncRequest::Pull(PullOrigin::User)).unwrap();
        assert!(matches!(pulled(&handle), SyncOutcome::Pulled { .. }));

        assert_eq!(
            *read.lock().unwrap(),
            [2],
            "the work item that moved is read; the one that did not is not"
        );
        let stored = SqliteTicketRepository::open_existing(&path).unwrap();
        let graph = stored.load_graph().unwrap();
        assert_eq!(graph.comments, details.comments);
        assert_eq!(graph.history, details.history);
        let mut rows = stored.load_all().unwrap();
        rows.sort_by_key(|ticket| ticket.key.id);
        assert_eq!(
            rows.iter()
                .map(|ticket| ticket.details_rev)
                .collect::<Vec<_>>(),
            [0, 4],
            "the row that moved records the revision its details came from"
        );
    }

    #[test]
    fn a_details_request_reads_one_work_item_and_stores_it_against_that_revision() {
        let directory = tempdir().unwrap();
        let mut resting = ticket(1, "Resting");
        resting.revision = 6;
        let path = seeded_database_of(&directory, &[resting.clone(), ticket(2, "Elsewhere")]);
        let details = details_of(&resting.key);
        let source = FakeSource::with(Vec::new()).with_details(1, details.clone());
        let handle = SyncHandle::spawn(path.clone(), Box::new(source)).unwrap();

        handle
            .send(SyncRequest::Details(resting.key.clone()))
            .unwrap();
        let DetailsOutcome::Fetched(update) = detailed(&handle) else {
            panic!("expected the details to be read");
        };
        assert_eq!(update.key, resting.key);
        assert_eq!(
            update.revision, 6,
            "the details belong to the revision the database held when they were read"
        );
        assert_eq!(update.details, details);

        let stored = SqliteTicketRepository::open_existing(&path).unwrap();
        assert_eq!(stored.load_graph().unwrap().comments, details.comments);
        let mut rows = stored.load_all().unwrap();
        rows.sort_by_key(|ticket| ticket.key.id);
        assert_eq!(
            rows.iter()
                .map(|ticket| ticket.details_rev)
                .collect::<Vec<_>>(),
            [6, 0],
            "one row moved; the work item nobody looked at is untouched"
        );

        // A work item the database no longer holds fails by name, so the main
        // thread can stop asking about that one rather than about all of them.
        let gone = TicketKey {
            organization: "demo".into(),
            id: 404,
        };
        handle.send(SyncRequest::Details(gone.clone())).unwrap();
        let DetailsOutcome::Failed { key, message } = detailed(&handle) else {
            panic!("expected the fetch to fail");
        };
        assert_eq!(key, gone);
        assert!(message.contains("404"), "{message}");
    }

    #[test]
    fn an_idle_pull_asks_twice_writes_nothing_and_reports_that_nothing_changed() {
        let directory = tempdir().unwrap();
        let path = watermarked_database(
            &directory,
            &[ticket(1, "One"), ticket(2, "Two")],
            &TicketGraph::default(),
            "2026-02-01T00:00:00Z",
        );
        // The states the picker offers are already stored, so a pull has no
        // reason to ask Azure DevOps for them a second time. This handle stays
        // open for the length of the test the way the TUI's own does, so the
        // write-ahead log is not created and torn down under the measurement.
        let mut reader = SqliteTicketRepository::open_existing(&path).unwrap();
        reader
            .replace_type_states(
                "Task",
                &[StateOption::new("Active", StateCategory::InProgress)],
            )
            .unwrap();
        let source = FakeSource::with(vec![Ok(SyncBatch::default())]).listing(vec![1, 2]);
        let requests = Arc::clone(&source.requests);
        let asked = Arc::clone(&source.asked_types);
        let handle = SyncHandle::spawn(path.clone(), Box::new(source)).unwrap();
        let before = crate::db::data_signature(&path);

        handle.send(SyncRequest::Pull(PullOrigin::Timer)).unwrap();
        assert!(
            matches!(pulled(&handle), SyncOutcome::Unchanged),
            "an unchanged project reports itself rather than a pull of nothing"
        );

        assert_eq!(
            crate::db::data_signature(&path),
            before,
            "nothing was written, so nobody watching the file reloads"
        );
        assert_eq!(
            *requests.lock().unwrap(),
            2,
            "one changed-since query and one id query, and no work items read"
        );
        assert!(
            asked.lock().unwrap().is_empty(),
            "the states already in the database are not fetched again"
        );
        assert_eq!(
            reader.load_all().unwrap().len(),
            2,
            "the rows the pull found nothing to say about are still there"
        );
    }

    #[test]
    fn a_work_item_the_project_stopped_listing_is_deleted_with_everything_on_it() {
        let directory = tempdir().unwrap();
        let doomed = ticket(2, "Two").key;
        let graph = TicketGraph {
            relations: vec![RelationRecord {
                from: doomed.clone(),
                to: ticket(1, "One").key,
                kind: RelationKind::Parent,
            }],
            comments: vec![CommentRecord {
                ticket: doomed.clone(),
                comment_id: 5,
                created_at: ts("2026-02-02T00:00:00Z"),
                author: Some("Avery Chen".into()),
                text: "Looks good".into(),
            }],
            history: vec![HistoryRecord {
                ticket: doomed,
                revision: 1,
                changed_at: ts("2026-02-02T00:00:00Z"),
                changed_by: None,
                field_name: "State".into(),
                old_value: None,
                new_value: Some("Active".into()),
            }],
        };
        let path = watermarked_database(
            &directory,
            &[ticket(1, "One"), ticket(2, "Two")],
            &graph,
            "2026-02-01T00:00:00Z",
        );
        let handle = SyncHandle::spawn(
            path.clone(),
            Box::new(FakeSource::with(vec![Ok(SyncBatch::default())]).listing(vec![1])),
        )
        .unwrap();

        handle.send(SyncRequest::Pull(PullOrigin::User)).unwrap();
        let SyncOutcome::Pulled {
            prepared,
            mode,
            count,
        } = pulled(&handle)
        else {
            panic!("expected a successful pull");
        };
        assert_eq!(mode, SyncMode::Incremental);
        assert_eq!(count, 1, "a work item that vanished is a change");
        assert_eq!(prepared.ticket_count(), 1);

        let stored = SqliteTicketRepository::open_existing(&path).unwrap();
        assert_eq!(
            stored
                .load_all()
                .unwrap()
                .iter()
                .map(|ticket| ticket.key.id)
                .collect::<Vec<_>>(),
            [1],
            "the recycled work item is gone from the table"
        );
        assert_eq!(
            stored.load_graph().unwrap(),
            TicketGraph::default(),
            "its links, comments, and history went with it"
        );
        assert_eq!(
            stored_watermark(&path).as_deref(),
            Some("2026-02-01T00:00:00Z"),
            "a deletion moves no watermark: no changed date came back"
        );
    }

    #[test]
    fn a_database_with_no_watermark_is_pulled_whole_and_left_with_one() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let mut newest = ticket(7, "Pulled");
        newest.changed_at = ts("2026-04-01T12:00:00Z");
        let source = FakeSource::with(vec![Ok(SyncBatch {
            tickets: vec![ticket(8, "Older"), newest.clone()],
            relations: Vec::new(),
        })]);
        let watermarks = Arc::clone(&source.watermarks);
        let handle = SyncHandle::spawn(path.clone(), Box::new(source)).unwrap();

        handle.send(SyncRequest::Pull(PullOrigin::User)).unwrap();
        let SyncOutcome::Pulled { mode, count, .. } = pulled(&handle) else {
            panic!("expected a successful pull");
        };
        assert_eq!(
            mode,
            SyncMode::Full,
            "with nowhere to start from, everything comes down"
        );
        assert_eq!(count, 2);
        assert!(
            watermarks.lock().unwrap().is_empty(),
            "no changed-since query goes out without a watermark"
        );
        assert_eq!(
            stored_watermark(&path),
            Some(newest.changed_at.to_rfc3339()),
            "the full pull leaves the watermark the next pull starts from"
        );
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
    fn the_team_members_a_source_lists_are_stored_and_handed_to_the_picker() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let team = vec![
            Identity::new("Avery Chen", Some("avery@example.com".into())),
            Identity::new("Dana Okafor", None),
        ];
        let handle = SyncHandle::spawn(
            path.clone(),
            Box::new(FakeSource::with(vec![]).with_team(team.clone())),
        )
        .unwrap();

        handle.send(SyncRequest::Identities).unwrap();
        let found = loop {
            match next_event(&handle) {
                SyncEvent::Identities(identities) => break identities,
                SyncEvent::DisplayName(_) => continue,
                other => panic!("expected the team members, got {other:?}"),
            }
        };
        assert_eq!(found, team, "the worker hands back what it stored");
        assert_eq!(
            SqliteTicketRepository::open_existing(&path)
                .unwrap()
                .load_identities()
                .unwrap(),
            team,
            "the next session's picker is complete without asking again"
        );
    }

    #[test]
    fn a_source_that_cannot_list_teams_says_nothing_and_stores_nothing() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let handle = SyncHandle::spawn(path.clone(), Box::new(FakeSource::with(vec![]))).unwrap();

        handle.send(SyncRequest::Identities).unwrap();
        let found = loop {
            match next_event(&handle) {
                SyncEvent::Identities(identities) => break identities,
                SyncEvent::DisplayName(_) => continue,
                other => panic!("expected the team members, got {other:?}"),
            }
        };
        assert!(found.is_empty(), "a failure is answered with nobody");
        assert!(
            SqliteTicketRepository::open_existing(&path)
                .unwrap()
                .load_identities()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_classification_trees_a_source_reads_are_stored_and_handed_to_the_pickers() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let trees = vec![
            ClassificationNode::new(NodeKind::Area, "atlas", 0),
            ClassificationNode {
                start_date: Some(ts("2026-08-25T00:00:00Z")),
                finish_date: Some(ts("2026-09-05T00:00:00Z")),
                ..ClassificationNode::new(NodeKind::Iteration, "atlas\\Sprint 1", 1)
            },
        ];
        let handle = SyncHandle::spawn(
            path.clone(),
            Box::new(FakeSource::with(vec![]).with_nodes(trees.clone())),
        )
        .unwrap();

        handle.send(SyncRequest::ClassificationNodes).unwrap();
        let found = loop {
            match next_event(&handle) {
                SyncEvent::ClassificationNodes(nodes) => break nodes,
                SyncEvent::DisplayName(_) => continue,
                other => panic!("expected the classification trees, got {other:?}"),
            }
        };
        assert_eq!(found, trees, "the worker hands back what it stored");

        let stored = SqliteTicketRepository::open_existing(&path).unwrap();
        assert_eq!(
            stored.load_classification_nodes().unwrap(),
            trees,
            "the next session's pickers open on them without asking again"
        );
        assert!(
            stored
                .meta(db::CLASSIFICATION_FETCHED_KEY)
                .unwrap()
                .is_some(),
            "and the fetch is dated, so a fresh cache is left alone"
        );
    }

    #[test]
    fn a_source_that_cannot_read_classification_nodes_stores_nothing() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let handle = SyncHandle::spawn(path.clone(), Box::new(FakeSource::with(vec![]))).unwrap();

        handle.send(SyncRequest::ClassificationNodes).unwrap();
        let found = loop {
            match next_event(&handle) {
                SyncEvent::ClassificationNodes(nodes) => break nodes,
                SyncEvent::DisplayName(_) => continue,
                other => panic!("expected the classification trees, got {other:?}"),
            }
        };
        assert!(found.is_empty(), "a failure is answered with nothing");
        let stored = SqliteTicketRepository::open_existing(&path).unwrap();
        assert!(stored.load_classification_nodes().unwrap().is_empty());
        assert_eq!(
            stored.meta(db::CLASSIFICATION_FETCHED_KEY).unwrap(),
            None,
            "nothing was fetched, so nothing is dated and the next open asks again"
        );
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
                SyncEvent::DisplayName(_)
                | SyncEvent::Details(_)
                | SyncEvent::Identities(_)
                | SyncEvent::ClassificationNodes(_) => continue,
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
