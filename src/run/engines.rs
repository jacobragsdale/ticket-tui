//! The background workers the run owns: the sync worker and its pending
//! writes, the details reader, the local-clone scanner, the reload thread, and
//! the agent context file they publish through.

use super::*;

/// How often the workspace is read while the Repos tab is showing. It is a
/// handful of `git status` calls, and none of them touches the network.
pub(super) const LOCAL_SCAN_CADENCE: Duration = Duration::from_secs(60);

/// The local side of the Repos tab: the thread that reads the workspace and
/// runs git in it, and when it last looked.
#[derive(Default)]
pub(super) struct LocalRuntime {
    pub(super) worker: Option<LocalHandle>,
    /// When the workspace was last read. `None` asks for a read on the next
    /// turn, which is how a finished clone or pull shows its result.
    pub(super) scanned: Option<Instant>,
    /// Whether the Repos tab was showing last turn, so opening it reads the
    /// workspace at once rather than up to a minute later.
    pub(super) showing: bool,
}

/// Everything the event loop needs to keep the database in step with Azure
/// DevOps: the worker thread, the timer that feeds it, and why there is no
/// worker when there is none.
pub(super) struct SyncRuntime {
    pub(super) worker: Option<SyncHandle>,
    /// The pipeline watcher, on its own thread with its own client, so a log
    /// fetch never queues behind a pull. `None` for a run with no project to
    /// watch.
    pub(super) pipelines: Option<WatchHandle>,
    /// Whether the watcher has been told the Pipelines tab is showing, so the
    /// message is sent when it changes rather than every turn.
    pub(super) watching_tab: bool,
    /// The run and log node the watcher was last told about, for the same
    /// reason.
    pub(super) watching_run: (Option<i64>, Option<LogTarget>),
    /// The runs the watcher has been asked to follow.
    pub(super) watched_runs: Vec<i64>,
    /// The approvals the last read found, so only one that was not there
    /// before is announced. `None` until the first read, which is the
    /// baseline: a queue that was already waiting is not news.
    pub(super) approvals_seen: Option<HashSet<String>>,
    /// Clones on this machine: their own thread, so a clone that takes a
    /// minute never holds up an edit.
    pub(super) local: LocalRuntime,
    pub(super) scheduler: SyncScheduler,
    pub(super) config: Option<AzureConfig>,
    /// Why Azure DevOps could not be resolved, reported when the user asks for
    /// a sync anyway.
    pub(super) offline_reason: Option<String>,
    /// When to read the selected work item's comments and history.
    pub(super) details: DetailsEngine,
}

/// How long the selection has to stay on one work item before its comments and
/// history are worth two requests. Holding `j` down the table crosses dozens of
/// rows; none of them is being read.
pub(super) const DETAILS_REST: Duration = Duration::from_millis(300);

/// When to ask for the selected work item's comments and revision history.
///
/// The trigger is the selection coming to rest, not the selection changing, so
/// scrolling costs nothing. One request is in flight at a time, and a work item
/// whose details could not be read is not asked about again for the rest of the
/// run: a failure is a notification, never a loop.
#[derive(Debug, Default)]
pub(super) struct DetailsEngine {
    /// The work item the selection is sitting on and when it landed there.
    pub(super) resting: Option<(TicketKey, Instant)>,
    pub(super) in_flight: Option<TicketKey>,
    pub(super) failed: HashSet<TicketKey>,
    /// Whether a failure has already been reported this run.
    pub(super) reported: bool,
}

impl DetailsEngine {
    /// The work item to read now, if the selection has been on one whose
    /// stored details are behind it for [`DETAILS_REST`]. Called every turn of
    /// the event loop, which is what makes the rest period a rest period.
    pub(super) fn due(&mut self, selected: Option<&Ticket>, now: Instant) -> Option<TicketKey> {
        let Some(key) = self.wanted(selected) else {
            self.resting = None;
            return None;
        };
        match &self.resting {
            Some((resting, since)) if *resting == key => {
                if now.duration_since(*since) < DETAILS_REST {
                    return None;
                }
            }
            // Somewhere new: the rest period starts over from here.
            _ => {
                self.resting = Some((key, now));
                return None;
            }
        }
        if self.in_flight.is_some() {
            return None;
        }
        self.resting = None;
        self.in_flight = Some(key.clone());
        Some(key)
    }

    /// The selected work item, when what is stored for it is behind the
    /// revision on screen and reading it has not already failed.
    fn wanted(&self, selected: Option<&Ticket>) -> Option<TicketKey> {
        let ticket = selected?;
        (ticket.details_rev < ticket.revision && !self.failed.contains(&ticket.key))
            .then(|| ticket.key.clone())
    }

    /// The request answered, whatever it said.
    pub(super) fn finish(&mut self) {
        self.in_flight = None;
    }

    /// The request failed. Reports whether it is worth saying so: only the
    /// first failure of the run is, because every one after it says the same
    /// thing about a pane the user did not ask to fill.
    pub(super) fn fail(&mut self, key: TicketKey) -> bool {
        self.finish();
        self.failed.insert(key);
        !std::mem::replace(&mut self.reported, true)
    }

    /// How long the event loop may sleep before the rest period is up.
    pub(super) fn time_until_due(&self, now: Instant) -> Option<Duration> {
        if self.in_flight.is_some() {
            return None;
        }
        self.resting
            .as_ref()
            .map(|(_, since)| DETAILS_REST.saturating_sub(now.duration_since(*since)))
    }
}

impl SyncRuntime {
    /// What a pull the user asked for reports, and where it pulled from.
    pub(super) fn status_for(&self, mode: SyncMode, count: usize, extras: PulledExtras) -> String {
        let synced = sync::pull_summary(mode, count, extras);
        self.config.as_ref().map_or_else(
            || synced.clone(),
            |config| format!("{synced} from {}/{}", config.organization, config.project),
        )
    }

    /// Why nothing can be sent, for a run with no worker.
    pub(super) fn offline_message(&self) -> String {
        self.offline_reason
            .clone()
            .unwrap_or_else(|| "Azure DevOps is not configured".to_owned())
    }

    /// Hands one request to the worker, or says why it could not: there is no
    /// worker, or the one there was has stopped.
    pub(super) fn send(&self, request: SyncRequest) -> Result<(), String> {
        match &self.worker {
            Some(worker) => worker.send(request).map_err(|error| format!("{error:#}")),
            None => Err(self.offline_message()),
        }
    }

    /// Gives up on syncing for the rest of the run, which only happens when the
    /// worker thread is gone.
    pub(super) fn stop(&mut self, app: &mut App, error: &str) {
        self.worker = None;
        self.scheduler.stop();
        if app.shell.fail_sync(error, true) {
            app.shell.set_error(format!("Sync stopped: {error}"));
        }
    }
}

#[derive(Default)]
pub(super) struct ReloadEngine {
    pub(super) receiver: Option<Receiver<std::result::Result<Snapshot, String>>>,
}

/// How still the screen has to be before the files that follow it are
/// written, and how long a screen that never stills can put them off — a
/// spinner repaints ten times a second and publishes once a second, not ten
/// times.
const SETTLE_QUIET: Duration = Duration::from_millis(300);
const SETTLE_LIMIT: Duration = Duration::from_secs(1);

/// When the files that trail the screen — the agent context and the session —
/// are owed a write. Writing after every frame costs a `create_dir_all`, a
/// temp file and a rename per keystroke, which on a VDI with an antivirus
/// reading every write is most of what a keystroke costs.
#[derive(Default)]
pub(super) struct Settle {
    /// The first frame drawn since the last write, and the last one. Both are
    /// `None` while nothing is waiting to be written.
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Settle {
    /// Records a drawn frame.
    pub(super) fn drew(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    /// Whether a write is owed: a frame was drawn since the last one, and
    /// either the screen has been still for a moment or it has been moving
    /// long enough that waiting for it to still is waiting forever.
    pub(super) fn due(&self, now: Instant) -> bool {
        self.wakeup(now).is_some_and(|left| left.is_zero())
    }

    /// How long until `due`, so the loop can wake for it. `None` when nothing
    /// is waiting to be written.
    pub(super) fn wakeup(&self, now: Instant) -> Option<Duration> {
        let quiet = SETTLE_QUIET.saturating_sub(now.saturating_duration_since(self.last?));
        let limit = SETTLE_LIMIT.saturating_sub(now.saturating_duration_since(self.first?));
        Some(quiet.min(limit))
    }

    /// The files are written; the clock starts again at the next frame.
    pub(super) fn wrote(&mut self) {
        self.first = None;
        self.last = None;
    }
}

pub(super) struct AgentContextPublisher {
    pub(super) path: PathBuf,
    pub(super) last: Option<AgentContext>,
}

impl AgentContextPublisher {
    pub(super) fn new(database: &Path) -> Self {
        Self {
            path: agent_context::path_for(database),
            last: None,
        }
    }

    pub(super) fn publish(&mut self, app: &App) -> Result<()> {
        let context = app.agent_context();
        if self.last.as_ref() == Some(&context) {
            return Ok(());
        }
        agent_context::save(&self.path, &context)?;
        self.last = Some(context);
        Ok(())
    }

    pub(super) fn remove(&self) -> Result<()> {
        agent_context::remove(&self.path)
    }
}

impl ReloadEngine {
    pub(super) fn start(&mut self, path: &Path) -> Result<bool> {
        if self.receiver.is_some() {
            return Ok(false);
        }

        let path = path.to_path_buf();
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("ticket-reload".into())
            .spawn(move || {
                let result = (|| -> Result<Snapshot> {
                    let repository = SqliteTicketRepository::open_existing(&path)?;
                    let tickets = repository.load_all()?;
                    let graph = repository.load_graph()?;
                    let states = repository.load_type_states()?;
                    let repos = repository.load_repos()?;
                    Ok(Snapshot::with_graph(tickets, graph)
                        .with_states(states)
                        .with_repos(repos))
                })()
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send(result);
            })
            .context("failed to start database reload worker")?;
        self.receiver = Some(receiver);
        Ok(true)
    }

    pub(super) fn try_result(&mut self) -> Option<std::result::Result<Snapshot, String>> {
        let result = match self.receiver.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("database reload worker stopped".into()),
        };
        self.receiver = None;
        Some(result)
    }
}
