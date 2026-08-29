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

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::app::PreparedTickets;
use crate::azure::{self, AzureClient, AzureConfig, SyncBatch};
use crate::classification::ClassificationNode;
use crate::db::{self, SqliteTicketRepository};
use crate::edit::{EditApplied, EditRejection, EditRequest};
use crate::model::{
    CommentRecord, DetailsUpdate, Identity, RelationRecord, StateOption, Ticket, TicketGraph,
    TicketKey, WorkItemDetails,
};
use crate::timestamp::Timestamp;

/// How long the worker will wait out a throttled request the user is waiting
/// on before trying it again. A longer wait than this is not worth freezing the
/// queue for: the request is refused, and says when to try again.
const MAX_THROTTLE_WAIT: Duration = Duration::from_secs(60);

/// How far consecutive throttles may push the timer out. Ten minutes is long
/// enough to be out of a throttling window and short enough that a project that
/// came back is picked up while somebody is still looking at it.
const MAX_BACKOFF: Duration = Duration::from_secs(600);

/// What a request refused twice for throttling says. It names the wait rather
/// than the status code, because waiting is the only thing to do about it.
fn throttled_message(delay: Duration) -> String {
    format!(
        "Azure DevOps is throttling requests; try again in {}s",
        delay.as_secs()
    )
}

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
    /// Leave one comment on one work item. `text` is what was typed, as plain
    /// text; the worker turns it into the rich text Azure DevOps stores.
    Comment { key: TicketKey, text: String },
    /// Read the work item types the project's process offers, for the Type
    /// field of the new-work-item form. Asked for once a session, the first
    /// time that form opens.
    WorkItemTypes,
    /// Add one work item to the project. `patch` sets its fields and nothing
    /// else: the parent travels as a link, which the client appends.
    Create {
        work_item_type: String,
        patch: Vec<Value>,
        parent: Option<i64>,
    },
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
        /// How long Azure DevOps asked to be left alone before the next pull,
        /// for a pull that still landed its data: the rate-limit budget ran out
        /// on the way through, or a request inside the pull was turned away
        /// while the pull as a whole went on. `None` when nothing throttled.
        pause: Option<Duration>,
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
    /// One comment landed and is already written to SQLite, or was refused and
    /// nothing was written at all.
    Commented(Box<Result<CommentRecord, CommentRejection>>),
    /// The work item types the project's process offers, already stored. Empty
    /// when they could not be read, which is not worth reporting: the type
    /// picker already offers every type the database has seen.
    WorkItemTypes(Vec<String>),
    /// One work item was created and is already written to SQLite, or was
    /// refused and nothing was written at all.
    Created(Box<Result<CreatedWorkItem, CreateRejection>>),
    /// The worker thread is gone and no further events will arrive.
    Stopped,
}

/// A work item Azure DevOps stored, as the answer to a create came back: the
/// row and the links it carries, both already written to SQLite.
#[derive(Clone, Debug)]
pub struct CreatedWorkItem {
    pub ticket: Ticket,
    pub relations: Vec<RelationRecord>,
}

/// A work item that was never created. It names nothing, because there is no
/// id to name: only what Azure DevOps said is left to report.
#[derive(Clone, Debug)]
pub struct CreateRejection {
    pub message: String,
}

/// How one details request ended. A failure names the work item it was for, so
/// the main thread can stop asking about that one rather than about all of
/// them.
#[derive(Debug)]
pub enum DetailsOutcome {
    Fetched(DetailsUpdate),
    Failed { key: TicketKey, message: String },
}

/// A comment that never landed. It names the work item it was typed on, so the
/// row can stop waiting on it, and carries what Azure DevOps said.
#[derive(Clone, Debug)]
pub struct CommentRejection {
    pub key: TicketKey,
    pub message: String,
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
    /// Azure DevOps turned the pull away to shed load, naming how long to leave
    /// it. Nothing was written and nothing is wrong: the timer holds off and the
    /// title says so, rather than an error toast arriving every minute.
    Throttled {
        retry_after: Duration,
    },
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
    /// Add a work item to the project, answering with the copy the server
    /// stored. `fields` are the operations that set its fields and `parent` is
    /// the work item it hangs under. A source that cannot create one says so
    /// rather than pretending to have.
    fn create_work_item(
        &self,
        _work_item_type: &str,
        _fields: &[Value],
        _parent: Option<i64>,
    ) -> Result<(Ticket, Vec<RelationRecord>)> {
        Err(anyhow!("this source cannot create work items"))
    }
    /// The states one work item type allows, which is what the state picker
    /// offers once a pull has cached them.
    fn work_item_type_states(&self, work_item_type: &str) -> Result<Vec<StateOption>>;
    /// Every work item type the project's process offers, which the Type field
    /// of the new-work-item form lists. A source that cannot read them answers
    /// with none, and the form falls back to the types the rows already carry.
    fn work_item_types(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
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
    /// Leave one comment on a work item, answering with the record the server
    /// stored. `html` is the body as rich text. A source that cannot take one
    /// says so rather than pretending to have posted it, because a comment
    /// quietly dropped is worse than one refused.
    fn post_comment(&self, _id: i64, _html: &str) -> Result<CommentRecord> {
        Err(anyhow!("comments are not supported by this source"))
    }
    /// How long the responses read since this was last asked want to be left
    /// alone, from the rate-limit budget they reported. Reading it clears it. A
    /// source that reports no budget — every fake, and every response with room
    /// to spare — answers with nothing and is asked again at the usual time.
    fn throttled_for(&self) -> Option<Duration> {
        None
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

    fn create_work_item(
        &self,
        work_item_type: &str,
        fields: &[Value],
        parent: Option<i64>,
    ) -> Result<(Ticket, Vec<RelationRecord>)> {
        AzureClient::create_work_item(self, work_item_type, fields, parent)
    }

    fn work_item_type_states(&self, work_item_type: &str) -> Result<Vec<StateOption>> {
        self.fetch_work_item_type_states(work_item_type)
    }

    fn work_item_types(&self) -> Result<Vec<String>> {
        self.fetch_work_item_types()
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

    fn post_comment(&self, id: i64, html: &str) -> Result<CommentRecord> {
        AzureClient::post_comment(self, id, html)
    }

    fn throttled_for(&self) -> Option<Duration> {
        AzureClient::throttled_for(self)
    }
}

/// Which slice of Azure DevOps a connector's sources read: the project the work
/// items come from, and the extra WIQL condition narrowing it. A pull records
/// this beside the rows, so a later run can tell which project a database holds
/// and a changed condition can force a full pull.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncScope {
    pub organization: String,
    pub project: String,
    /// The extra WIQL condition ANDed into both pulls, or `None` for a project
    /// pulled entire.
    pub condition: Option<String>,
}

impl From<&AzureConfig> for SyncScope {
    fn from(config: &AzureConfig) -> Self {
        Self {
            organization: config.organization.clone(),
            project: config.project.clone(),
            condition: config.scope.clone(),
        }
    }
}

/// Opens a source the first time one is needed, so the TUI never waits for an
/// access token before it draws.
pub trait SourceConnector: Send {
    fn connect(&mut self) -> Result<Box<dyn WorkItemSource>>;
    /// What this connector's sources pull. A source standing in for Azure
    /// DevOps answers with nothing, and the pull records nothing about where
    /// its work items came from.
    fn scope(&self) -> Option<SyncScope> {
        None
    }
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

    fn scope(&self) -> Option<SyncScope> {
        Some(SyncScope::from(&self.config))
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

/// One pull, run to completion on the calling thread, for the `ticket-tui
/// sync` subcommand: the same worker the TUI drives, with nobody to hand the
/// reloaded rows to. `full` brings the whole project down rather than starting
/// from the stored watermark, which is how a database is rebuilt.
///
/// The signed-in display name is written alongside the rows, the way a pull
/// under the TUI has the main thread write it, so `@me` still resolves for a
/// database only ever synced from the command line.
pub fn pull_once(
    database: PathBuf,
    connector: Box<dyn SourceConnector>,
    full: bool,
) -> SyncOutcome {
    let (events, received) = mpsc::channel();
    let mut worker = Worker::new(database, connector);
    worker.force_full = full;
    let outcome = worker.pull(&events);
    drop(events);
    for event in received {
        if let SyncEvent::DisplayName(name) = event
            && let Ok(repository) = worker.repository()
        {
            drop(repository.set_meta(db::ME_DISPLAY_NAME_KEY, &name));
        }
    }
    outcome
}

fn work(
    database: PathBuf,
    connector: Box<dyn SourceConnector>,
    requests: &Receiver<SyncRequest>,
    events: &Sender<SyncEvent>,
) {
    let mut worker = Worker::new(database, connector);
    while let Ok(request) = requests.recv() {
        let event = match request {
            SyncRequest::Pull(origin) => {
                let outcome = worker.pull(events);
                SyncEvent::Finished {
                    origin,
                    outcome,
                    // Read after the pull: a pull that still landed its data is
                    // exactly the one whose successor has to be pushed out.
                    pause: worker.throttle_pause(),
                }
            }
            SyncRequest::Edit(request) => {
                SyncEvent::Edited(Box::new(worker.edit(&request, events)))
            }
            SyncRequest::Details(key) => SyncEvent::Details(Box::new(worker.details(key, events))),
            SyncRequest::Identities => SyncEvent::Identities(worker.identities(events)),
            SyncRequest::ClassificationNodes => {
                SyncEvent::ClassificationNodes(worker.classification_nodes(events))
            }
            SyncRequest::Comment { key, text } => {
                SyncEvent::Commented(Box::new(worker.comment(key, &text, events)))
            }
            SyncRequest::WorkItemTypes => SyncEvent::WorkItemTypes(worker.work_item_types(events)),
            SyncRequest::Create {
                work_item_type,
                patch,
                parent,
            } => SyncEvent::Created(Box::new(worker.create(
                &work_item_type,
                &patch,
                parent,
                events,
            ))),
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
    /// The longest wait a request the current pull gave up on quietly asked
    /// for. Taken with the pull's outcome, so a pull that landed anyway still
    /// tells the timer to hold off.
    throttled: Option<Duration>,
    /// Whether every pull brings the whole project down rather than starting
    /// from the stored watermark. Only `ticket-tui sync --full` sets it: the
    /// TUI's worker lets the watermark decide.
    force_full: bool,
}

impl Worker {
    fn new(database: PathBuf, connector: Box<dyn SourceConnector>) -> Self {
        Self {
            database,
            connector,
            source: None,
            repository: None,
            typed_states: HashSet::new(),
            typed_states_seeded: false,
            throttled: None,
            force_full: false,
        }
    }

    /// A timer pull turned away for throttling is not retried here: the main
    /// thread owns the clock, and sleeping on this thread would only hold every
    /// edit typed in the meantime behind a pull nobody asked for.
    fn pull(&mut self, events: &Sender<SyncEvent>) -> SyncOutcome {
        match self.try_pull(events) {
            Ok(outcome) => outcome,
            Err(error) => azure::throttle_delay(&error).map_or_else(
                || SyncOutcome::Failed(format!("{error:#}")),
                |retry_after| SyncOutcome::Throttled { retry_after },
            ),
        }
    }

    /// How long this pull wants the next one held off: the budget the source's
    /// responses reported, and any request the pull passed over because it was
    /// throttled. The longer of the two wins, and reading it clears both.
    fn throttle_pause(&mut self) -> Option<Duration> {
        let noted = self.throttled.take();
        let reported = self
            .source
            .as_deref()
            .and_then(WorkItemSource::throttled_for);
        match (noted, reported) {
            (Some(noted), Some(reported)) => Some(noted.max(reported)),
            (noted, reported) => noted.or(reported),
        }
    }

    /// Notes a request this pull gave up on quietly when what stopped it was
    /// throttling, and says so. The pull still lands; the wait rides out with
    /// it, so the timer holds off rather than asking for the same refusal a
    /// minute later.
    fn note_throttle(&mut self, error: &anyhow::Error) -> bool {
        let Some(delay) = azure::throttle_delay(error) else {
            return false;
        };
        self.throttled = Some(self.throttled.map_or(delay, |held| held.max(delay)));
        true
    }

    /// Runs one request the user is waiting on, waiting out a single throttle.
    /// Azure DevOps turning an edit away is nothing the person who pressed the
    /// key can act on, so the worker sleeps out the wait it asked for — capped,
    /// because nothing else is written while it waits — and tries once more. A
    /// second refusal is reported as the rejection it is, in words that say
    /// when to try again rather than quoting a status code.
    fn awaiting_throttle<T>(&mut self, attempt: impl Fn(&mut Self) -> Result<T>) -> Result<T> {
        let error = match attempt(self) {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let Some(delay) = azure::throttle_delay(&error) else {
            return Err(error);
        };
        thread::sleep(delay.min(MAX_THROTTLE_WAIT));
        attempt(self).map_err(|error| {
            azure::throttle_delay(&error).map_or(error, |delay| anyhow!(throttled_message(delay)))
        })
    }

    /// Writes one field back to Azure DevOps. A refusal is reported as itself
    /// rather than as a failed sync, because the row it belongs to has to be
    /// put back on the main thread.
    fn edit(
        &mut self,
        request: &EditRequest,
        events: &Sender<SyncEvent>,
    ) -> Result<EditApplied, EditRejection> {
        self.awaiting_throttle(|worker| worker.try_edit(request, events))
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
        match self.awaiting_throttle(|worker| worker.try_details(&key, events)) {
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
            let fetched = self.source(events)?.fetch_details(ticket.key.id);
            let details = match fetched {
                Ok(details) => details,
                // Throttling is about the next request as much as this one, so
                // the rest of the batch is left for a later pull rather than
                // worked through collecting refusals.
                Err(error) if self.note_throttle(&error) => break,
                Err(_) => continue,
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

    /// Posts one comment and stores it, answering with the record Azure DevOps
    /// kept. Nothing is written locally unless the post landed, so a refusal
    /// leaves the discussion exactly as it was.
    fn comment(
        &mut self,
        key: TicketKey,
        text: &str,
        events: &Sender<SyncEvent>,
    ) -> Result<CommentRecord, CommentRejection> {
        self.awaiting_throttle(|worker| worker.try_comment(&key, text, events))
            .map_err(|error| CommentRejection {
                key,
                message: format!("{error:#}"),
            })
    }

    /// The comment lands in its own transaction and moves no `details_rev`:
    /// the work item's stored details are still whatever the last fetch read,
    /// so a later fetch is free to read the discussion again and settle it.
    fn try_comment(
        &mut self,
        key: &TicketKey,
        text: &str,
        events: &Sender<SyncEvent>,
    ) -> Result<CommentRecord> {
        let posted = self
            .source(events)?
            .post_comment(key.id, &azure::comment_html(text))?;
        // The request named the work item, so the row lands on that one
        // whatever the answer says it is about.
        let comment = CommentRecord {
            ticket: key.clone(),
            ..posted
        };
        self.repository()?.insert_comment(&comment)?;
        Ok(comment)
    }

    /// Reads the work item types the project's process offers and stores them
    /// for the next session, answering with what was stored. Like the team
    /// members, a failure answers with nothing and says nothing: the form
    /// already offers every type the database has a work item of.
    fn work_item_types(&mut self, events: &Sender<SyncEvent>) -> Vec<String> {
        let types = match self.source(events) {
            Ok(source) => source.work_item_types().unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        if types.is_empty() {
            return types;
        }
        if let Ok(repository) = self.repository() {
            drop(repository.replace_work_item_types(&types));
        }
        types
    }

    /// Adds one work item and stores it, answering with the copy Azure DevOps
    /// kept. Nothing is written locally unless the create landed, so a refusal
    /// leaves the project exactly as it was and there is no half-made row to
    /// clean up.
    fn create(
        &mut self,
        work_item_type: &str,
        patch: &[Value],
        parent: Option<i64>,
        events: &Sender<SyncEvent>,
    ) -> Result<CreatedWorkItem, CreateRejection> {
        self.awaiting_throttle(|worker| worker.try_create(work_item_type, patch, parent, events))
            .map_err(|error| CreateRejection {
                message: format!("{error:#}"),
            })
    }

    /// The links come back with the work item — the create URL asks for them —
    /// so the parent it was filed under is stored with it rather than waiting
    /// on the next pull to notice.
    fn try_create(
        &mut self,
        work_item_type: &str,
        patch: &[Value],
        parent: Option<i64>,
        events: &Sender<SyncEvent>,
    ) -> Result<CreatedWorkItem> {
        let (ticket, relations) =
            self.source(events)?
                .create_work_item(work_item_type, patch, parent)?;
        self.repository()?.upsert(&ticket, &relations)?;
        Ok(CreatedWorkItem { ticket, relations })
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
    /// so everything comes down once and leaves a watermark for next time. A
    /// scope that no longer matches the one the rows were pulled under is the
    /// other way to lose that starting point.
    fn try_pull(&mut self, events: &Sender<SyncEvent>) -> Result<SyncOutcome> {
        let outcome = match self.watermark()? {
            // A scope that has moved makes the stored rows the wrong slice of
            // the project: the condition may have widened, and only a full pull
            // brings in what it now admits, or narrowed, and only a full pull
            // drops what it now excludes.
            Some(watermark) if !self.force_full && !self.rescoped()? => {
                self.pull_changed(watermark, events)?
            }
            _ => self.pull_everything(events)?,
        };
        // Which project these rows came from, recorded after the pull that
        // brought them rather than before it: a pull that failed says nothing
        // about what the database holds.
        self.record_scope()?;
        Ok(outcome)
    }

    fn watermark(&mut self) -> Result<Option<Timestamp>> {
        let Some(stored) = self.repository()?.meta(db::WATERMARK_KEY)? else {
            return Ok(None);
        };
        Ok(Timestamp::parse(&stored).ok())
    }

    /// Whether the configured scope differs from the one the stored rows were
    /// pulled under. A connector that does not know its own scope — every fake
    /// — never rescopes anything.
    fn rescoped(&mut self) -> Result<bool> {
        let Some(scope) = self.connector.scope() else {
            return Ok(false);
        };
        Ok(self.stored_condition()? != scope.condition)
    }

    /// The WIQL condition the stored rows were pulled under. It is written as
    /// the empty string when there is none, so a database that has never
    /// recorded one and a database pulled whole read the same.
    fn stored_condition(&mut self) -> Result<Option<String>> {
        Ok(self
            .repository()?
            .meta(db::SYNC_SCOPE_KEY)?
            .filter(|condition| !condition.is_empty()))
    }

    /// Records the project and condition this pull ran under, and only when one
    /// of them moved: an idle project's pull still leaves the file untouched,
    /// which is what keeps every other reader from reloading for nothing.
    fn record_scope(&mut self) -> Result<()> {
        let Some(scope) = self.connector.scope() else {
            return Ok(());
        };
        let condition = scope.condition.clone().unwrap_or_default();
        let repository = self.repository()?;
        for (key, value) in [
            (db::ORGANIZATION_KEY, scope.organization.as_str()),
            (db::PROJECT_KEY, scope.project.as_str()),
            (db::SYNC_SCOPE_KEY, condition.as_str()),
        ] {
            if repository.meta(key)?.as_deref() != Some(value) {
                repository.set_meta(key, value)?;
            }
        }
        Ok(())
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
            let fetched = self.source(events)?.work_item_type_states(work_item_type);
            let states = match fetched {
                Ok(states) => states,
                Err(error) if self.note_throttle(&error) => break,
                Err(_) => continue,
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
    /// How many pulls in a row Azure DevOps has turned away. Every one past the
    /// first doubles the configured interval, so a project that is throttling
    /// is asked less and less often until one pull gets through.
    throttles: u32,
}

impl SyncScheduler {
    /// `None` disables the timer, leaving only pulls the user asks for.
    #[must_use]
    pub const fn new(interval: Option<Duration>) -> Self {
        Self {
            interval,
            next_due: None,
            in_flight: false,
            throttles: 0,
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

    /// Marks the pull in flight as finished and books the next timer pull. Any
    /// backoff consecutive throttles built up is dropped here: only throttles
    /// in a row are worth pushing the timer out for.
    pub fn finish(&mut self, now: Instant) {
        self.in_flight = false;
        self.throttles = 0;
        self.schedule_next(now);
    }

    /// Azure DevOps asked to be left alone for `retry_after`. The next pull goes
    /// no earlier than that, and no earlier than the doubled interval a run of
    /// throttles has reached; answers with when the next attempt is due, which
    /// is what the title counts down even when the timer is off entirely.
    pub fn pause(&mut self, now: Instant, retry_after: Duration) -> Instant {
        self.in_flight = false;
        self.throttles = self.throttles.saturating_add(1);
        let delay = retry_after.max(self.backoff());
        let until = now.checked_add(delay).unwrap_or(now);
        // `--refresh 0` stays off: a throttle is a reason to pull later, never
        // a reason to start pulling at all.
        self.next_due = self.interval.and(Some(until));
        until
    }

    /// The floor a run of throttles puts under the next pull. The first one
    /// honours the wait Azure DevOps named and nothing more; each one after it
    /// doubles the configured interval, up to [`MAX_BACKOFF`].
    fn backoff(&self) -> Duration {
        let Some(interval) = self.interval.filter(|_| self.throttles > 1) else {
            return Duration::ZERO;
        };
        // Shifting is capped well before it could overflow; the cap below is
        // what actually decides the answer.
        interval
            .saturating_mul(1 << (self.throttles - 1).min(16))
            .min(MAX_BACKOFF)
    }

    /// Gives up on the timer entirely, for when the worker is gone.
    pub const fn stop(&mut self) {
        self.interval = None;
        self.next_due = None;
        self.in_flight = false;
        self.throttles = 0;
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
    use crate::azure::{RequestRejected, Throttled};
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
            description_html: String::new(),
            created_at: ts("2026-01-01T00:00:00Z"),
            changed_at: ts("2026-02-01T00:00:00Z"),
            web_url: format!("https://dev.azure.com/demo/atlas/_workitems/edit/{id}"),
            details_rev: 0,
        }
    }

    /// One write the worker made: the work item id, and the document it sent.
    type SentPatch = (i64, Vec<Value>);

    /// One create the worker made: the type, the operations that set its
    /// fields, and the parent it was filed under.
    type Creation = (String, Vec<Value>, Option<i64>);

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
        /// Every work item whose details were asked for, in order, throttled
        /// refusals included.
        detailed: Arc<Mutex<Vec<i64>>>,
        /// The waits this source turns work item reads and writes away with,
        /// in order. An empty list answers everything it is asked.
        throttles: Arc<Mutex<Vec<Duration>>>,
        /// The same, for the comments and history endpoints, so a test can
        /// throttle what a pull reads on the side without throttling the pull.
        detail_throttles: Arc<Mutex<Vec<Duration>>>,
        /// The project's team members, or `None` for a source that cannot list
        /// them, which is what the trait's default leaves behind.
        team_members: Option<Vec<Identity>>,
        /// The project's classification trees, or `None` for a source that
        /// cannot read them.
        classification_nodes: Option<Vec<ClassificationNode>>,
        /// The comment a post answers with, or `None` to refuse the post the
        /// way a work item nobody may comment on would.
        comment: Option<CommentRecord>,
        /// Every comment body posted, with the work item it was posted on.
        posted: Arc<Mutex<Vec<(i64, String)>>>,
        /// The types this source says the project's process offers, or `None`
        /// for one that cannot read them.
        work_item_types: Option<Vec<String>>,
        /// Every create the worker sent: the type, the field operations, and
        /// the parent it named.
        created: Arc<Mutex<Vec<Creation>>>,
        /// What this source pulls, for the tests about recording it. `None`
        /// stands in for a source that does not know, which is what leaves
        /// every other test's database free of sync scope rows.
        scope: Option<SyncScope>,
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

        /// Turns the next requests away for throttling, naming one wait each.
        fn throttling(self, waits: Vec<Duration>) -> Self {
            *self.throttles.lock().unwrap() = waits;
            self
        }

        /// The same for the details endpoints, which a pull reads on the side.
        fn throttling_details(self, waits: Vec<Duration>) -> Self {
            *self.detail_throttles.lock().unwrap() = waits;
            self
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

        /// The record a post answers with, which is what the worker stores.
        fn commenting(self, comment: CommentRecord) -> Self {
            Self {
                comment: Some(comment),
                ..self
            }
        }

        fn with_types(self, types: Vec<&str>) -> Self {
            Self {
                work_item_types: Some(types.into_iter().map(ToOwned::to_owned).collect()),
                ..self
            }
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

        /// Which project this source pulls, and how much of it.
        fn scoped(self, condition: Option<&str>) -> Self {
            Self {
                scope: Some(SyncScope {
                    organization: "demo".into(),
                    project: "atlas".into(),
                    condition: condition.map(ToOwned::to_owned),
                }),
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
            if let Some(refusal) = throttled(&self.throttles) {
                return Err(refusal);
            }
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
            if let Some(refusal) = throttled(&self.throttles) {
                return Err(refusal);
            }
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

        fn work_item_types(&self) -> Result<Vec<String>> {
            self.work_item_types
                .clone()
                .context("the fake source cannot list work item types")
        }

        fn create_work_item(
            &self,
            work_item_type: &str,
            fields: &[Value],
            parent: Option<i64>,
        ) -> Result<(Ticket, Vec<RelationRecord>)> {
            self.created
                .lock()
                .unwrap()
                .push((work_item_type.to_owned(), fields.to_vec(), parent));
            if let Some((status, message)) = &self.refusal {
                return Err(anyhow::Error::new(RequestRejected::new(
                    *status,
                    "https://dev.azure.com/demo/atlas/_apis/wit/workitems/$Issue".to_owned(),
                    message.clone(),
                )));
            }
            self.stored
                .clone()
                .context("the fake source was not given a stored copy")
        }

        fn post_comment(&self, id: i64, html: &str) -> Result<CommentRecord> {
            self.posted.lock().unwrap().push((id, html.to_owned()));
            self.comment
                .clone()
                .context("HTTP 403: the work item is read only")
        }

        /// Two requests over the wire: one page of comments and one of updates.
        fn fetch_details(&self, id: i64) -> Result<WorkItemDetails> {
            *self.requests.lock().unwrap() += 2;
            self.detailed.lock().unwrap().push(id);
            if let Some(refusal) = throttled(&self.detail_throttles) {
                return Err(refusal);
            }
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

        fn scope(&self) -> Option<SyncScope> {
            self.scope.clone()
        }
    }

    /// The next refusal a throttling queue has left, shaped like the one Azure
    /// DevOps answers a spent budget with.
    fn throttled(waits: &Mutex<Vec<Duration>>) -> Option<anyhow::Error> {
        let mut waits = waits.lock().unwrap();
        (!waits.is_empty()).then(|| {
            anyhow::Error::new(Throttled::new(
                waits.remove(0),
                429,
                "https://dev.azure.com/demo/_apis/wit/wiql",
                "too many requests",
            ))
        })
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

    fn stored_meta(path: &PathBuf, key: &str) -> Option<String> {
        SqliteTicketRepository::open_existing(path)
            .unwrap()
            .meta(key)
            .unwrap()
    }

    fn stored_watermark(path: &PathBuf) -> Option<String> {
        stored_meta(path, db::WATERMARK_KEY)
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

    /// The next pull's outcome and the wait it asked the timer to hold off for.
    fn pulled_with_pause(handle: &SyncHandle) -> (SyncOutcome, Option<Duration>) {
        loop {
            match next_event(handle) {
                SyncEvent::Finished { outcome, pause, .. } => return (outcome, pause),
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

    /// How the next comment ended, past the display name the first connect
    /// reports.
    fn commented(handle: &SyncHandle) -> Result<CommentRecord, CommentRejection> {
        loop {
            match next_event(handle) {
                SyncEvent::Commented(result) => return *result,
                SyncEvent::DisplayName(_) => continue,
                other => panic!("expected a comment to finish, got {other:?}"),
            }
        }
    }

    /// One comment as Azure DevOps hands it back, carrying the id, date, and
    /// author only the server can give it.
    fn posted_comment(id: i64, at: &str, text: &str) -> CommentRecord {
        CommentRecord {
            ticket: TicketKey {
                organization: "demo".into(),
                id: 1,
            },
            comment_id: id,
            created_at: ts(at),
            author: Some("Jacob Ragsdale".into()),
            text: text.into(),
        }
    }

    fn stored_comments(path: &PathBuf) -> Vec<CommentRecord> {
        SqliteTicketRepository::open_existing(path)
            .unwrap()
            .load_graph()
            .unwrap()
            .comments
    }

    fn finished(handle: &SyncHandle) -> (PullOrigin, SyncOutcome) {
        match next_event(handle) {
            SyncEvent::Finished {
                origin, outcome, ..
            } => (origin, outcome),
            other => panic!("expected a finished pull, got {other:?}"),
        }
    }

    #[test]
    fn a_pull_records_the_project_and_condition_it_ran_under() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let source = FakeSource::with(vec![Ok(SyncBatch {
            tickets: vec![ticket(7, "Pulled")],
            relations: Vec::new(),
        })])
        .scoped(Some("[System.WorkItemType] <> 'Test Case'"));
        let handle = SyncHandle::spawn(path.clone(), Box::new(source)).unwrap();

        handle.send(SyncRequest::Pull(PullOrigin::Timer)).unwrap();
        assert!(matches!(pulled(&handle), SyncOutcome::Pulled { .. }));

        assert_eq!(
            stored_meta(&path, db::ORGANIZATION_KEY).as_deref(),
            Some("demo"),
            "the database records which project filled it"
        );
        assert_eq!(
            stored_meta(&path, db::PROJECT_KEY).as_deref(),
            Some("atlas")
        );
        assert_eq!(
            stored_meta(&path, db::SYNC_SCOPE_KEY).as_deref(),
            Some("[System.WorkItemType] <> 'Test Case'"),
            "and how much of it was asked for"
        );
    }

    #[test]
    fn a_changed_scope_pulls_the_project_again_and_stores_the_new_condition() {
        let directory = tempdir().unwrap();
        let path = watermarked_database(
            &directory,
            &[ticket(1, "Existing")],
            &TicketGraph::default(),
            "2026-02-01T00:00:00Z",
        );
        let mut repository = SqliteTicketRepository::open_existing(&path).unwrap();
        repository
            .set_meta(db::SYNC_SCOPE_KEY, "[System.WorkItemType] <> 'Test Case'")
            .unwrap();
        drop(repository);
        let source = FakeSource::with(vec![
            Ok(SyncBatch {
                tickets: vec![ticket(7, "In the new scope")],
                relations: Vec::new(),
            }),
            Ok(SyncBatch {
                tickets: vec![ticket(8, "Changed since")],
                relations: Vec::new(),
            }),
        ])
        .scoped(Some("[System.ChangedDate] > @today-180"));
        let handle = SyncHandle::spawn(path.clone(), Box::new(source)).unwrap();

        handle.send(SyncRequest::Pull(PullOrigin::User)).unwrap();
        let SyncOutcome::Pulled { mode, .. } = pulled(&handle) else {
            panic!("expected a successful pull");
        };
        assert_eq!(
            mode,
            SyncMode::Full,
            "a watermark says what changed, not what the old condition kept out"
        );
        assert_eq!(
            stored_meta(&path, db::SYNC_SCOPE_KEY).as_deref(),
            Some("[System.ChangedDate] > @today-180")
        );
        let stored = SqliteTicketRepository::open_existing(&path).unwrap();
        assert_eq!(
            stored.load_all().unwrap().len(),
            1,
            "the work items the old condition let through are gone"
        );

        handle.send(SyncRequest::Pull(PullOrigin::User)).unwrap();
        let SyncOutcome::Pulled { mode, .. } = pulled(&handle) else {
            panic!("expected a successful pull");
        };
        assert_eq!(
            mode,
            SyncMode::Incremental,
            "the condition matches what the rows were pulled under, so the watermark is trusted"
        );
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
                | SyncEvent::ClassificationNodes(_)
                | SyncEvent::WorkItemTypes(_)
                | SyncEvent::Created(_)
                | SyncEvent::Commented(_) => continue,
                SyncEvent::Stopped => panic!("the worker stopped early"),
            }
        }
        assert_eq!(seen, ["edit", "pull"], "requests are answered in order");
    }

    /// The answer to the next create, past the display name the first connect
    /// reports.
    fn created(handle: &SyncHandle) -> Result<CreatedWorkItem, CreateRejection> {
        loop {
            match next_event(handle) {
                SyncEvent::Created(result) => return *result,
                SyncEvent::DisplayName(_) => continue,
                other => panic!("expected a create to finish, got {other:?}"),
            }
        }
    }

    /// The types the next work item types request answers with.
    fn fetched_types(handle: &SyncHandle) -> Vec<String> {
        loop {
            match next_event(handle) {
                SyncEvent::WorkItemTypes(types) => return types,
                SyncEvent::DisplayName(_) => continue,
                other => panic!("expected the work item types, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_created_work_item_is_stored_with_its_links_before_it_is_reported() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let mut child = ticket(42, "Honour Retry-After");
        child.work_item_type = "Issue".into();
        let parent = TicketKey {
            organization: "demo".into(),
            id: 1,
        };
        let source = FakeSource::storing(
            child.clone(),
            vec![RelationRecord {
                from: child.key.clone(),
                to: parent.clone(),
                kind: RelationKind::Parent,
            }],
        );
        let sent = Arc::clone(&source.created);
        let handle = SyncHandle::spawn(path.clone(), Box::new(source)).unwrap();
        let fields = vec![crate::edit::set_field(
            crate::edit::TITLE_FIELD,
            "Honour Retry-After",
        )];

        handle
            .send(SyncRequest::Create {
                work_item_type: "Issue".into(),
                patch: fields.clone(),
                parent: Some(1),
            })
            .unwrap();
        let created = created(&handle).expect("the create was accepted");

        assert_eq!(created.ticket.key.id, 42);
        assert_eq!(
            sent.lock().unwrap().clone(),
            vec![("Issue".to_owned(), fields, Some(1))],
            "the type, the field operations, and the parent all travel as they were given"
        );

        let repository = SqliteTicketRepository::open_existing(&path).unwrap();
        let stored = repository.load_all().unwrap();
        assert!(
            stored.iter().any(|ticket| ticket.key.id == 42),
            "the work item is in SQLite before the main thread hears about it"
        );
        let graph = repository.load_graph().unwrap();
        assert_eq!(
            graph.parents_of(&created.ticket.key),
            vec![parent.clone()],
            "the child knows its parent"
        );
        assert_eq!(
            graph.children_of(&parent),
            vec![created.ticket.key],
            "and the parent knows its child"
        );
    }

    #[test]
    fn a_refused_create_writes_nothing_and_reports_what_azure_devops_said() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let handle = SyncHandle::spawn(
            path.clone(),
            Box::new(FakeSource::refusing(400, "TF401320: rule error")),
        )
        .unwrap();

        handle
            .send(SyncRequest::Create {
                work_item_type: "Issue".into(),
                patch: Vec::new(),
                parent: None,
            })
            .unwrap();
        let rejection = created(&handle).expect_err("the create was refused");

        assert!(
            rejection.message.contains("TF401320: rule error"),
            "the refusal travels as it came: {}",
            rejection.message
        );
        assert_eq!(
            SqliteTicketRepository::open_existing(&path)
                .unwrap()
                .load_all()
                .unwrap()
                .len(),
            1,
            "nothing was written for a work item that was never created"
        );
    }

    #[test]
    fn the_work_item_types_are_read_once_and_cached_for_the_next_session() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let handle = SyncHandle::spawn(
            path.clone(),
            Box::new(
                FakeSource::with(vec![Ok(SyncBatch::default())]).with_types(vec!["Epic", "Issue"]),
            ),
        )
        .unwrap();

        handle.send(SyncRequest::WorkItemTypes).unwrap();

        assert_eq!(fetched_types(&handle), ["Epic", "Issue"]);
        assert_eq!(
            SqliteTicketRepository::open_existing(&path)
                .unwrap()
                .load_work_item_types()
                .unwrap(),
            ["Epic", "Issue"],
            "stored in the order the process listed them, for the next run"
        );
    }

    #[test]
    fn a_source_that_cannot_list_the_work_item_types_says_nothing_and_stores_nothing() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let handle = SyncHandle::spawn(
            path.clone(),
            Box::new(FakeSource::with(vec![Ok(SyncBatch::default())])),
        )
        .unwrap();

        handle.send(SyncRequest::WorkItemTypes).unwrap();

        assert!(
            fetched_types(&handle).is_empty(),
            "an endpoint nobody asked for is not worth a toast"
        );
        assert!(
            SqliteTicketRepository::open_existing(&path)
                .unwrap()
                .load_work_item_types()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_comment_is_posted_as_rich_text_and_stored_on_its_own() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let source = FakeSource::with(vec![Ok(SyncBatch::default())]).commenting(posted_comment(
            9,
            "2026-03-04T09:15:00Z",
            "blocked on <auth>",
        ));
        let posted = Arc::clone(&source.posted);
        let handle = SyncHandle::spawn(path.clone(), Box::new(source)).unwrap();

        handle
            .send(SyncRequest::Comment {
                key: TicketKey {
                    organization: "demo".into(),
                    id: 1,
                },
                text: "blocked on <auth>".into(),
            })
            .unwrap();
        let comment = commented(&handle).expect("the post was accepted");

        assert_eq!(comment.comment_id, 9);
        assert_eq!(comment.ticket.id, 1);
        assert_eq!(
            posted.lock().unwrap().clone(),
            vec![(1, "<p>blocked on &lt;auth&gt;</p>".to_owned())],
            "what was typed goes out escaped, in a paragraph"
        );

        let stored = stored_comments(&path);
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].text, "blocked on <auth>");
        assert_eq!(
            SqliteTicketRepository::open_existing(&path)
                .unwrap()
                .load_all()
                .unwrap()[0]
                .details_rev,
            0,
            "a comment moves no details revision, so the next fetch still settles it"
        );
    }

    #[test]
    fn a_refused_comment_stores_nothing_and_names_the_work_item() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let handle = SyncHandle::spawn(
            path.clone(),
            Box::new(FakeSource::with(vec![Ok(SyncBatch::default())])),
        )
        .unwrap();

        handle
            .send(SyncRequest::Comment {
                key: TicketKey {
                    organization: "demo".into(),
                    id: 1,
                },
                text: "blocked on auth".into(),
            })
            .unwrap();
        let rejection = commented(&handle).expect_err("the post was refused");

        assert_eq!(rejection.key.id, 1);
        assert!(
            rejection.message.contains("read only"),
            "{}",
            rejection.message
        );
        assert!(
            stored_comments(&path).is_empty(),
            "a refusal writes nothing at all"
        );
    }

    #[test]
    fn a_pull_that_does_not_name_the_work_item_leaves_its_new_comment_alone() {
        let directory = tempdir().unwrap();
        let path = watermarked_database(
            &directory,
            &[ticket(1, "Commented on"), ticket(2, "Untouched")],
            &TicketGraph::default(),
            "2026-02-01T00:00:00Z",
        );
        let mut moved = ticket(2, "Untouched");
        moved.changed_at = ts("2026-04-01T00:00:00Z");
        let source = FakeSource::with(vec![Ok(SyncBatch {
            tickets: vec![moved],
            relations: Vec::new(),
        })])
        .listing(vec![1, 2])
        .commenting(posted_comment(
            9,
            "2026-03-04T09:15:00Z",
            "Merged into main",
        ));
        let handle = SyncHandle::spawn(path.clone(), Box::new(source)).unwrap();

        handle
            .send(SyncRequest::Comment {
                key: TicketKey {
                    organization: "demo".into(),
                    id: 1,
                },
                text: "Merged into main".into(),
            })
            .unwrap();
        commented(&handle).expect("the post was accepted");

        handle.send(SyncRequest::Pull(PullOrigin::Timer)).unwrap();
        let outcome = pulled(&handle);
        assert!(
            matches!(outcome, SyncOutcome::Pulled { count: 1, .. }),
            "only the other work item moved: {outcome:?}"
        );

        let stored = stored_comments(&path);
        assert_eq!(stored.len(), 1, "a pull that skipped #1 kept its comment");
        assert_eq!(stored[0].ticket.id, 1);
        assert_eq!(stored[0].text, "Merged into main");
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
    fn a_throttled_pull_is_a_pause_rather_than_a_failure_and_writes_nothing() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let source = FakeSource::with(vec![Ok(SyncBatch::default())])
            .throttling(vec![Duration::from_secs(45)]);
        let handle = SyncHandle::spawn(path.clone(), Box::new(source)).unwrap();

        handle.send(SyncRequest::Pull(PullOrigin::Timer)).unwrap();
        let (outcome, pause) = pulled_with_pause(&handle);
        let SyncOutcome::Throttled { retry_after } = outcome else {
            panic!("expected a throttled pull, got {outcome:?}");
        };
        assert_eq!(
            retry_after,
            Duration::from_secs(45),
            "the wait Azure DevOps named is the wait that comes back"
        );
        assert_eq!(pause, None, "the refusal carries the wait, not the budget");

        let stored = SqliteTicketRepository::open_existing(&path).unwrap();
        assert_eq!(stored.load_all().unwrap()[0].title, "Existing");
    }

    #[test]
    fn a_pull_whose_details_are_throttled_lands_and_asks_the_timer_to_hold_off() {
        let directory = tempdir().unwrap();
        let stale = ts("2026-02-01T00:00:00Z");
        let path = watermarked_database(
            &directory,
            &[ticket(1, "Existing"), ticket(2, "Existing too")],
            &TicketGraph::default(),
            &stale.to_rfc3339(),
        );
        let moved: Vec<Ticket> = [1, 2]
            .into_iter()
            .map(|id| Ticket {
                changed_at: ts("2026-03-01T00:00:00Z"),
                ..ticket(id, "Moved")
            })
            .collect();
        let source = FakeSource::with(vec![Ok(SyncBatch {
            tickets: moved,
            relations: Vec::new(),
        })])
        .throttling_details(vec![Duration::from_secs(45)]);
        let handle = SyncHandle::spawn(path.clone(), Box::new(source.clone())).unwrap();

        handle.send(SyncRequest::Pull(PullOrigin::Timer)).unwrap();
        let (outcome, pause) = pulled_with_pause(&handle);
        assert!(
            matches!(outcome, SyncOutcome::Pulled { count: 2, .. }),
            "the work items still land: {outcome:?}"
        );
        assert_eq!(
            pause,
            Some(Duration::from_secs(45)),
            "the wait rides out with the pull that survived it"
        );
        assert_eq!(
            source.detailed.lock().unwrap().len(),
            1,
            "the rest of the batch is left for a later pull rather than asked \
             for one refusal at a time"
        );
    }

    #[test]
    fn a_throttled_edit_waits_out_the_delay_and_lands_on_the_second_try() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let source = FakeSource::storing(ticket(1, "Rewritten"), Vec::new())
            .throttling(vec![Duration::ZERO]);
        let handle = SyncHandle::spawn(path, Box::new(source.clone())).unwrap();

        handle
            .send(SyncRequest::Edit(edit_request(1, "Active", 1)))
            .unwrap();
        let applied = edited(&handle).expect("the second try lands");
        assert_eq!(applied.ticket.title, "Rewritten");
        assert_eq!(
            source.patches.lock().unwrap().len(),
            2,
            "the write was tried once more after the wait"
        );
    }

    #[test]
    fn an_edit_throttled_twice_is_refused_in_words_that_say_when_to_try_again() {
        let directory = tempdir().unwrap();
        let path = seeded_database(&directory);
        let source = FakeSource::storing(ticket(1, "Rewritten"), Vec::new())
            .throttling(vec![Duration::ZERO, Duration::from_secs(45)]);
        let handle = SyncHandle::spawn(path, Box::new(source.clone())).unwrap();

        handle
            .send(SyncRequest::Edit(edit_request(1, "Active", 1)))
            .unwrap();
        let rejection = edited(&handle).expect_err("a second throttle is a refusal");
        assert_eq!(
            rejection.message,
            "Azure DevOps is throttling requests; try again in 45s"
        );
        assert!(
            !rejection.conflict,
            "throttling is not the work item moving on"
        );
        assert_eq!(
            source.patches.lock().unwrap().len(),
            2,
            "one retry, not a loop"
        );
    }

    #[test]
    fn consecutive_throttles_push_the_timer_out_and_a_success_puts_it_back() {
        let start = Instant::now();
        let mut scheduler = SyncScheduler::new(Some(Duration::from_secs(60)));
        let wait = Duration::from_secs(45);

        scheduler.start();
        assert_eq!(
            scheduler.pause(start, wait),
            start + wait,
            "the first throttle honours the wait it was given and nothing more"
        );
        assert!(!scheduler.in_flight());
        assert!(!scheduler.due(start + Duration::from_secs(44)));
        assert!(scheduler.due(start + wait));
        assert_eq!(scheduler.time_until_due(start), Some(wait));

        for expected in [120, 240, 480, 600, 600] {
            scheduler.start();
            assert_eq!(
                scheduler.pause(start, wait),
                start + Duration::from_secs(expected),
                "each throttle in a row doubles the interval, up to ten minutes"
            );
        }

        scheduler.start();
        scheduler.finish(start);
        assert!(
            scheduler.due(start + Duration::from_secs(60)),
            "a success puts the timer back on its configured interval"
        );
        scheduler.start();
        assert_eq!(
            scheduler.pause(start, wait),
            start + wait,
            "and puts the doubling back to where it started"
        );
    }

    #[test]
    fn a_throttled_pull_books_nothing_when_the_timer_is_off() {
        let start = Instant::now();
        let mut scheduler = SyncScheduler::new(None);
        assert!(scheduler.request_user_pull());

        assert_eq!(
            scheduler.pause(start, Duration::from_secs(45)),
            start + Duration::from_secs(45),
            "the title still has a wait to count down"
        );
        assert!(
            !scheduler.due(start + Duration::from_secs(86_400)),
            "--refresh 0 stays off; a throttle is no reason to start pulling"
        );
        assert_eq!(scheduler.time_until_due(start), None);
        assert!(
            scheduler.request_user_pull(),
            "the sync keypress still works through a pause"
        );
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
