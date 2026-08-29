//! The pipeline watcher: a second worker, on its own thread with its own
//! client, that polls Azure DevOps often enough for a run to look live.
//!
//! It is separate from the sync worker for two reasons. A log fetch must not
//! queue behind a 60-second pull, and an edit must not queue behind a log
//! fetch. And nothing here writes SQLite: what the watcher learns is merged
//! into what the Pipelines screen holds, and the next pull reconciles it. That
//! is what lets it poll as often as it does without touching the file every
//! reader watches.
//!
//! Azure DevOps has no event stream for runs — its own web UI polls — so this
//! polls too, on cadences that stretch when the rate-limit budget thins.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::model::Run;

/// How often the live runs of the whole project are read while anything is
/// worth reading them for.
pub const LIVE_RUNS_CADENCE: Duration = Duration::from_secs(15);

/// The longest any cadence stretches to when the budget is thin. Past this,
/// waiting longer stops being politeness and starts being uselessness.
pub const MAX_CADENCE: Duration = Duration::from_secs(60);

/// What the watcher can be asked to do. Every one of them is a statement about
/// what is worth polling, not a request for one poll: the watcher decides when
/// to go, and stops the moment nothing on screen needs it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WatchRequest {
    /// Whether the Pipelines tab is the one showing. A hidden tab with nothing
    /// watched is the watcher's cue to go quiet.
    TabShowing(bool),
    /// Keep following one run wherever the user goes. #685 puts this behind a
    /// key; the watcher only has to know it is asked for.
    Watch(i64),
    Unwatch(i64),
    Stop,
}

/// What the watcher has learnt. Every event is merged into what the Pipelines
/// screen holds; none of it is written to SQLite.
#[derive(Clone, Debug)]
pub enum WatchEvent {
    /// Every run in the project that is queued, going, or being cancelled.
    LiveRuns(Vec<Run>),
    /// Azure DevOps asked to be left alone, and for how long. Not an error:
    /// the cadences stretch and the overlay says so.
    Throttled(Duration),
    /// A poll failed. Reported once per spell of failure, the way the sync
    /// worker reports one.
    Failed(String),
    /// The thread is gone.
    Stopped,
}

/// What the watcher reads. Separate from `WorkItemSource` because a watcher
/// source never writes anything and never reads a work item.
pub trait PipelineSource: Send {
    /// Every run that is queued, in progress, or being cancelled.
    fn live_runs(&self) -> Result<Vec<Run>>;

    /// How long the responses read since this was last asked want to be left
    /// alone, from the rate-limit budget they reported. Reading it clears it.
    fn throttled_for(&self) -> Option<Duration> {
        None
    }
}

/// Opens a client for the watcher's own thread, the way [`crate::sync::SourceConnector`]
/// does for the sync worker.
pub trait PipelineConnector: Send {
    fn connect(&mut self) -> Result<Box<dyn PipelineSource>>;
}

/// One thing the watcher polls: how often it is meant to, how often it is
/// actually managing, and when it is next due.
#[derive(Clone, Copy, Debug)]
pub struct Cadence {
    base: Duration,
    current: Duration,
    due: Option<Instant>,
}

impl Cadence {
    #[must_use]
    pub const fn new(base: Duration) -> Self {
        Self {
            base,
            current: base,
            due: None,
        }
    }

    /// How long it is asking to wait, which is the base until something asks
    /// for more room.
    #[must_use]
    pub const fn interval(self) -> Duration {
        self.current
    }

    /// Whether this is due at `now`. Something never polled is due at once,
    /// which is what makes the first read happen the moment the tab opens.
    #[must_use]
    pub fn is_due(&self, now: Instant) -> bool {
        self.due.is_none_or(|due| now >= due)
    }

    /// How long until it is due, for the wait the loop blocks on.
    #[must_use]
    pub fn until_due(&self, now: Instant) -> Duration {
        self.due
            .map_or(Duration::ZERO, |due| due.saturating_duration_since(now))
    }

    /// Records a poll, which sets the next one.
    pub fn polled(&mut self, now: Instant) {
        self.due = Some(now + self.current);
    }

    /// Doubles the wait, up to [`MAX_CADENCE`]. Called when the budget is thin
    /// or a response was turned away.
    pub fn stretch(&mut self) {
        self.current = (self.current * 2).min(MAX_CADENCE);
    }

    /// Back to the written cadence, which a clean response earns.
    pub fn relax(&mut self) {
        self.current = self.base;
    }

    /// Puts the next poll off by at least `wait`, which is what a `Retry-After`
    /// asks for.
    pub fn hold_off(&mut self, now: Instant, wait: Duration) {
        let until = now + wait;
        self.due = Some(self.due.map_or(until, |due| due.max(until)));
    }
}

/// The watcher's own state, apart from the thread it usually runs on, so a
/// test can drive it with a clock of its own.
pub struct Watcher {
    source: Box<dyn PipelineSource>,
    live: Cadence,
    tab_showing: bool,
    watched: Vec<i64>,
    /// Whether the last poll failed, so a spell of failure is reported once.
    failing: bool,
}

impl Watcher {
    #[must_use]
    pub fn new(source: Box<dyn PipelineSource>) -> Self {
        Self {
            source,
            live: Cadence::new(LIVE_RUNS_CADENCE),
            tab_showing: false,
            watched: Vec::new(),
            failing: false,
        }
    }

    /// Whether anything is worth polling for: the tab is showing, or a run is
    /// being followed from another tab.
    #[must_use]
    pub fn is_watching(&self) -> bool {
        self.tab_showing || !self.watched.is_empty()
    }

    /// The live-runs cadence as it stands, which the database overlay reports.
    #[must_use]
    pub const fn live_cadence(&self) -> Duration {
        self.live.interval()
    }

    /// Takes one request. Answers whether the watcher should carry on.
    pub fn handle(&mut self, request: &WatchRequest) -> bool {
        match request {
            WatchRequest::TabShowing(showing) => self.tab_showing = *showing,
            WatchRequest::Watch(run) => {
                if !self.watched.contains(run) {
                    self.watched.push(*run);
                }
            }
            WatchRequest::Unwatch(run) => self.watched.retain(|held| held != run),
            WatchRequest::Stop => return false,
        }
        true
    }

    /// Everything due at `now`. Nothing at all while there is nothing to watch,
    /// which is what keeps a hidden tab from costing a request.
    pub fn poll(&mut self, now: Instant) -> Vec<WatchEvent> {
        if !self.is_watching() || !self.live.is_due(now) {
            return Vec::new();
        }
        let mut events = Vec::new();
        match self.source.live_runs() {
            Ok(runs) => {
                self.failing = false;
                self.live.relax();
                events.push(WatchEvent::LiveRuns(runs));
            }
            Err(error) => {
                self.live.stretch();
                if !self.failing {
                    self.failing = true;
                    events.push(WatchEvent::Failed(format!("{error:#}")));
                }
            }
        }
        if let Some(wait) = self.source.throttled_for() {
            self.live.stretch();
            self.live.hold_off(now, wait);
            events.push(WatchEvent::Throttled(wait));
        }
        self.live.polled(now);
        events
    }

    /// How long the loop may block for before something is due. A watcher with
    /// nothing to watch waits for a request rather than for a clock.
    #[must_use]
    pub fn until_due(&self, now: Instant) -> Option<Duration> {
        self.is_watching().then(|| self.live.until_due(now))
    }
}

impl PipelineSource for crate::azure::AzureClient {
    fn live_runs(&self) -> Result<Vec<Run>> {
        self.fetch_live_runs()
    }

    fn throttled_for(&self) -> Option<Duration> {
        Self::throttled_for(self)
    }
}

/// Opens the watcher's own client on the configured project.
#[derive(Clone, Debug)]
pub struct AzureWatchConnector {
    config: crate::azure::AzureConfig,
}

impl AzureWatchConnector {
    #[must_use]
    pub const fn new(config: crate::azure::AzureConfig) -> Self {
        Self { config }
    }
}

impl PipelineConnector for AzureWatchConnector {
    fn connect(&mut self) -> Result<Box<dyn PipelineSource>> {
        Ok(Box::new(crate::azure::AzureClient::connect(
            self.config.clone(),
        )?))
    }
}

/// The handle the main thread holds: requests in, events out.
pub struct WatchHandle {
    requests: Sender<WatchRequest>,
    events: Receiver<WatchEvent>,
    stopped: std::cell::Cell<bool>,
}

impl WatchHandle {
    /// Starts the watcher on its own thread with its own client.
    pub fn spawn(mut connector: Box<dyn PipelineConnector>) -> Result<Self> {
        let (request_sender, request_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("ticket-watch".into())
            .spawn(move || {
                let Ok(source) = connector.connect() else {
                    return;
                };
                watch(Watcher::new(source), &request_receiver, &event_sender);
            })
            .context("failed to start the pipeline watcher")?;
        Ok(Self {
            requests: request_sender,
            events: event_receiver,
            stopped: std::cell::Cell::new(false),
        })
    }

    /// Tells the watcher what is worth polling. Fails only when it is gone.
    pub fn send(&self, request: WatchRequest) -> Result<()> {
        self.requests
            .send(request)
            .context("the pipeline watcher stopped")
    }

    /// The next event, if one is waiting.
    pub fn try_event(&self) -> Option<WatchEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                (!self.stopped.replace(true)).then_some(WatchEvent::Stopped)
            }
        }
    }
}

/// The loop: wait until the earliest cadence is due or a request arrives,
/// whichever comes first, then poll whatever that made due.
fn watch(mut watcher: Watcher, requests: &Receiver<WatchRequest>, events: &Sender<WatchEvent>) {
    loop {
        let now = Instant::now();
        for event in watcher.poll(now) {
            if events.send(event).is_err() {
                return;
            }
        }
        let wait = watcher
            .until_due(Instant::now())
            .unwrap_or(Duration::from_secs(3600));
        match requests.recv_timeout(wait) {
            Ok(request) => {
                if !watcher.handle(&request) {
                    return;
                }
                // Everything else waiting is taken now, so a burst of requests
                // costs one poll rather than one each.
                while let Ok(request) = requests.try_recv() {
                    if !watcher.handle(&request) {
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::model::RunStatus;

    struct FakeRuns {
        runs: Arc<Mutex<Vec<Run>>>,
        /// The errors the next reads answer with, oldest first.
        failures: Arc<Mutex<Vec<String>>>,
        /// The waits the source reports having been asked for.
        throttles: Arc<Mutex<Vec<Duration>>>,
        reads: Arc<Mutex<usize>>,
    }

    impl PipelineSource for FakeRuns {
        fn live_runs(&self) -> Result<Vec<Run>> {
            *self.reads.lock().unwrap() += 1;
            if let Some(error) = self.failures.lock().unwrap().pop() {
                return Err(anyhow::anyhow!(error));
            }
            Ok(self.runs.lock().unwrap().clone())
        }

        fn throttled_for(&self) -> Option<Duration> {
            self.throttles.lock().unwrap().pop()
        }
    }

    fn run(id: i64) -> Run {
        Run {
            id,
            pipeline_id: 1,
            build_number: format!("20260829.{id}"),
            status: RunStatus::InProgress,
            result: None,
            source_branch: "refs/heads/main".into(),
            source_version: "abc1234".into(),
            requested_for: None,
            reason: "individualCI".into(),
            pr_id: None,
            queue_time: None,
            start_time: None,
            finish_time: None,
            url: String::new(),
        }
    }

    /// A watcher over a fake source, with the handles a test drives it by.
    struct Harness {
        watcher: Watcher,
        reads: Arc<Mutex<usize>>,
        throttles: Arc<Mutex<Vec<Duration>>>,
        failures: Arc<Mutex<Vec<String>>>,
    }

    fn watcher() -> Harness {
        let reads = Arc::new(Mutex::new(0));
        let throttles = Arc::new(Mutex::new(Vec::new()));
        let failures = Arc::new(Mutex::new(Vec::new()));
        let source = FakeRuns {
            runs: Arc::new(Mutex::new(vec![run(14)])),
            failures: Arc::clone(&failures),
            throttles: Arc::clone(&throttles),
            reads: Arc::clone(&reads),
        };
        Harness {
            watcher: Watcher::new(Box::new(source)),
            reads,
            throttles,
            failures,
        }
    }

    #[test]
    fn nothing_is_polled_while_the_tab_is_hidden_and_no_run_is_watched() {
        let Harness {
            mut watcher, reads, ..
        } = watcher();
        let now = Instant::now();

        assert!(watcher.poll(now).is_empty());
        assert_eq!(*reads.lock().unwrap(), 0, "a quiet watcher costs nothing");
        assert!(
            watcher.until_due(now).is_none(),
            "and waits for a request rather than for a clock"
        );

        // A run followed from another tab is reason enough on its own.
        watcher.handle(&WatchRequest::Watch(14));
        assert!(!watcher.poll(now).is_empty());
        assert_eq!(*reads.lock().unwrap(), 1);

        watcher.handle(&WatchRequest::Unwatch(14));
        assert!(watcher.poll(now + Duration::from_secs(60)).is_empty());
        assert_eq!(*reads.lock().unwrap(), 1, "and it goes quiet again");
    }

    #[test]
    fn the_live_runs_are_read_at_the_cadence_while_the_tab_is_showing() {
        let Harness {
            mut watcher, reads, ..
        } = watcher();
        watcher.handle(&WatchRequest::TabShowing(true));
        let start = Instant::now();

        let events = watcher.poll(start);
        assert!(
            matches!(events.as_slice(), [WatchEvent::LiveRuns(runs)] if runs[0].id == 14),
            "the first read happens the moment the tab opens"
        );

        assert!(
            watcher.poll(start + Duration::from_secs(14)).is_empty(),
            "and nothing again until it is due"
        );
        assert_eq!(*reads.lock().unwrap(), 1);

        assert!(!watcher.poll(start + LIVE_RUNS_CADENCE).is_empty());
        assert_eq!(*reads.lock().unwrap(), 2, "one read per cadence");
    }

    #[test]
    fn a_thin_budget_stretches_the_cadence_and_a_clean_response_puts_it_back() {
        let Harness {
            mut watcher,
            throttles,
            ..
        } = watcher();
        watcher.handle(&WatchRequest::TabShowing(true));
        let start = Instant::now();
        throttles.lock().unwrap().push(Duration::from_secs(30));

        let events = watcher.poll(start);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, WatchEvent::Throttled(wait) if *wait == Duration::from_secs(30))),
            "the watcher says it was asked to hold off"
        );
        assert_eq!(
            watcher.live_cadence(),
            Duration::from_secs(30),
            "and doubles its wait"
        );
        assert!(
            watcher.poll(start + Duration::from_secs(29)).is_empty(),
            "the hold-off it was asked for is honoured on top"
        );

        let events = watcher.poll(start + Duration::from_secs(31));
        assert!(!events.is_empty());
        assert_eq!(
            watcher.live_cadence(),
            LIVE_RUNS_CADENCE,
            "a clean response earns the written cadence back"
        );
    }

    #[test]
    fn a_failing_poll_is_reported_once_and_slows_down() {
        let Harness {
            mut watcher,
            failures,
            ..
        } = watcher();
        watcher.handle(&WatchRequest::TabShowing(true));
        failures.lock().unwrap().extend([
            "network unreachable".to_owned(),
            "network unreachable".to_owned(),
        ]);
        let start = Instant::now();

        let events = watcher.poll(start);
        assert!(
            matches!(events.as_slice(), [WatchEvent::Failed(message)] if message.contains("unreachable")),
        );
        assert_eq!(watcher.live_cadence(), Duration::from_secs(30));

        let events = watcher.poll(start + Duration::from_secs(31));
        assert!(events.is_empty(), "the same failure is not said twice");
        assert_eq!(
            watcher.live_cadence(),
            MAX_CADENCE.min(Duration::from_secs(60))
        );

        let events = watcher.poll(start + Duration::from_secs(120));
        assert!(
            matches!(events.as_slice(), [WatchEvent::LiveRuns(_)]),
            "and it reports again the moment it works"
        );
        assert_eq!(watcher.live_cadence(), LIVE_RUNS_CADENCE);
    }

    #[test]
    fn the_cadence_never_stretches_past_a_minute() {
        let mut cadence = Cadence::new(LIVE_RUNS_CADENCE);
        for _ in 0..10 {
            cadence.stretch();
        }
        assert_eq!(cadence.interval(), MAX_CADENCE);
        cadence.relax();
        assert_eq!(cadence.interval(), LIVE_RUNS_CADENCE);
    }
}
