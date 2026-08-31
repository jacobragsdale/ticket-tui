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

use crate::model::{Approval, Run, TimelineRecord};

/// How often the live runs of the whole project are read while anything is
/// worth reading them for.
pub const LIVE_RUNS_CADENCE: Duration = Duration::from_secs(15);

/// How often the timeline of the run on screen is read while it is going. A
/// finished run's timeline is read once and kept for the session.
pub const TIMELINE_CADENCE: Duration = Duration::from_secs(5);

/// How often the project's pending approvals are read. They change on a human
/// timescale, so once a minute is often enough.
pub const APPROVALS_CADENCE: Duration = Duration::from_secs(60);

/// How often the log of the node on screen is read while it is being written.
pub const LOG_CADENCE: Duration = Duration::from_secs(2);

/// What the log cadence falls back to once a log has stopped producing lines:
/// a task that is running but quiet is not worth two requests a second.
pub const QUIET_LOG_CADENCE: Duration = Duration::from_secs(5);

/// How many empty polls in a row mean a log has gone quiet.
const QUIET_AFTER: u32 = 2;

/// The node whose log is on screen, and how much of it is already held.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogTarget {
    pub log_id: i64,
    /// How many lines the screen holds, which is where the next fetch starts.
    pub from_line: usize,
    /// Whether the node is still writing. A finished one is read once, whole.
    pub live: bool,
}

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
    /// The run the details pane is showing, and the node whose log is under
    /// it: the timeline is read while the run is going, the log while the node
    /// is being written.
    Focus {
        run_id: i64,
        node: Option<LogTarget>,
    },
    /// Nothing is on screen worth reading a timeline for.
    Blur,
    /// Read the pending approvals now, which is what opening the overlay asks
    /// for rather than waiting out the minute.
    RefreshApprovals,
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
    /// New lines of one node's log, from the line the watcher was asked to
    /// start at. `finished` says the log will not grow again.
    LogLines {
        run_id: i64,
        log_id: i64,
        from_line: usize,
        lines: Vec<String>,
        finished: bool,
    },
    /// The work items one run built, read once when it is focused.
    RunWorkItems { run_id: i64, work_items: Vec<i64> },
    /// One run's stages, jobs and tasks, as they stand.
    Timeline {
        run_id: i64,
        records: Vec<TimelineRecord>,
    },
    /// Every approval the project is waiting on.
    Approvals(Vec<Approval>),
    /// A watched run has stopped. The shell toasts this whatever tab is
    /// showing, which is the point of watching one.
    RunFinished(Run),
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

    /// One run's timeline: its stages, jobs and tasks.
    fn timeline(&self, _run_id: i64) -> Result<Vec<TimelineRecord>> {
        Ok(Vec::new())
    }

    /// Every approval the project is waiting on.
    fn approvals(&self) -> Result<Vec<Approval>> {
        Ok(Vec::new())
    }

    /// One run as it stands, for a watched one that has left the live list.
    fn run(&self, _run_id: i64) -> Result<Option<Run>> {
        Ok(None)
    }

    /// The work items one run built.
    fn run_work_items(&self, _run_id: i64) -> Result<Vec<i64>> {
        Ok(Vec::new())
    }

    /// One node's log, from `start_line` on.
    fn log_lines(&self, _run_id: i64, _log_id: i64, _start_line: usize) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// How long the responses read since this was last asked want to be left
    /// alone, from the rate-limit budget they reported. Reading it clears it.
    fn throttled_for(&self) -> Option<Duration> {
        None
    }
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
    timeline: Cadence,
    tab_showing: bool,
    watched: Vec<i64>,
    /// The run whose timeline is worth reading, if one is on screen.
    focus: Option<i64>,
    /// The node whose log is on screen, and where its next fetch starts.
    node: Option<LogTarget>,
    log: Cadence,
    approvals: Cadence,
    /// Polls in a row that brought back nothing, which is what turns the log
    /// cadence down to the quiet one.
    quiet_polls: u32,
    /// Logs read whole because their node had finished: never read again.
    settled_logs: Vec<(i64, i64)>,
    /// Runs whose work items have been read, which happens once each.
    read_work_items: Vec<i64>,
    /// Runs whose timeline has finished moving. One of these is read once and
    /// never again: nothing about a finished run changes.
    settled: Vec<i64>,
    /// Whether the last poll failed, so a spell of failure is reported once.
    failing: bool,
}

impl Watcher {
    #[must_use]
    pub fn new(source: Box<dyn PipelineSource>) -> Self {
        Self {
            source,
            live: Cadence::new(LIVE_RUNS_CADENCE),
            timeline: Cadence::new(TIMELINE_CADENCE),
            tab_showing: false,
            watched: Vec::new(),
            focus: None,
            read_work_items: Vec::new(),
            node: None,
            log: Cadence::new(LOG_CADENCE),
            approvals: Cadence::new(APPROVALS_CADENCE),
            quiet_polls: 0,
            settled_logs: Vec::new(),
            settled: Vec::new(),
            failing: false,
        }
    }

    /// Whether anything is worth polling for: the tab is showing, or a run is
    /// being followed from another tab.
    #[must_use]
    pub fn is_watching(&self) -> bool {
        self.tab_showing || !self.watched.is_empty()
    }

    /// The log cadence as it stands: two seconds while lines are coming, five
    /// once the node has gone quiet.
    #[must_use]
    pub const fn log_cadence(&self) -> Duration {
        self.log.interval()
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
            WatchRequest::Focus { run_id, node } => {
                if self.focus != Some(*run_id) {
                    self.focus = Some(*run_id);
                    // A run just moved onto the screen is read at once rather
                    // than at the next tick of somebody else's cadence.
                    self.timeline = Cadence::new(TIMELINE_CADENCE);
                }
                // A different node is a different log, read at once; the same
                // node with more lines held is the same poll going on.
                let moved = self.node.map(|held| held.log_id) != node.map(|node| node.log_id);
                if moved {
                    self.log = Cadence::new(LOG_CADENCE);
                    self.quiet_polls = 0;
                }
                self.node = *node;
            }
            WatchRequest::RefreshApprovals => self.approvals = Cadence::new(APPROVALS_CADENCE),
            WatchRequest::Blur => {
                self.focus = None;
                self.node = None;
            }
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
        if !self.is_watching() {
            return Vec::new();
        }
        let mut events = Vec::new();
        if self.live.is_due(now) {
            match self.source.live_runs() {
                Ok(runs) => {
                    self.failing = false;
                    self.live.relax();
                    // A watched run that is no longer among the live ones has
                    // stopped; its own record says how it went.
                    let live_ids: Vec<i64> = runs.iter().map(|run| run.id).collect();
                    let finished: Vec<i64> = self
                        .watched
                        .iter()
                        .copied()
                        .filter(|run| !live_ids.contains(run))
                        .collect();
                    events.push(WatchEvent::LiveRuns(runs));
                    for run in finished {
                        self.watched.retain(|held| *held != run);
                        if let Ok(Some(record)) = self.source.run(run) {
                            events.push(WatchEvent::RunFinished(record));
                        }
                    }
                }
                Err(error) => {
                    self.live.stretch();
                    if !self.failing {
                        self.failing = true;
                        events.push(WatchEvent::Failed(format!("{error:#}")));
                    }
                }
            }
            self.live.polled(now);
        }
        if let Some(run) = self.focus.filter(|run| !self.read_work_items.contains(run)) {
            self.read_work_items.push(run);
            if let Ok(work_items) = self.source.run_work_items(run) {
                events.push(WatchEvent::RunWorkItems {
                    run_id: run,
                    work_items,
                });
            }
        }
        if let Some(run) = self.focus.filter(|run| !self.settled.contains(run))
            && self.timeline.is_due(now)
        {
            match self.source.timeline(run) {
                Ok(records) => {
                    self.timeline.relax();
                    // A run whose every node has finished is not going to move
                    // again, so its timeline is read once and kept.
                    if records.iter().all(|record| !record.state.is_live()) && !records.is_empty() {
                        self.settled.push(run);
                    }
                    events.push(WatchEvent::Timeline {
                        run_id: run,
                        records,
                    });
                }
                Err(error) => {
                    self.timeline.stretch();
                    if !self.failing {
                        self.failing = true;
                        events.push(WatchEvent::Failed(format!("{error:#}")));
                    }
                }
            }
            self.timeline.polled(now);
        }
        if let Some(target) = self.node.filter(|target| {
            !self
                .settled_logs
                .contains(&(self.focus.unwrap_or_default(), target.log_id))
        }) && let Some(run) = self.focus
            && self.log.is_due(now)
        {
            match self.source.log_lines(run, target.log_id, target.from_line) {
                Ok(lines) => {
                    if lines.is_empty() {
                        self.quiet_polls = self.quiet_polls.saturating_add(1);
                        if self.quiet_polls >= QUIET_AFTER {
                            self.log = Cadence::new(QUIET_LOG_CADENCE);
                        }
                    } else {
                        self.quiet_polls = 0;
                        self.log = Cadence::new(LOG_CADENCE);
                    }
                    if !target.live {
                        self.settled_logs.push((run, target.log_id));
                    }
                    events.push(WatchEvent::LogLines {
                        run_id: run,
                        log_id: target.log_id,
                        from_line: target.from_line,
                        lines,
                        finished: !target.live,
                    });
                }
                Err(error) => {
                    self.log.stretch();
                    if !self.failing {
                        self.failing = true;
                        events.push(WatchEvent::Failed(format!("{error:#}")));
                    }
                }
            }
            self.log.polled(now);
        }
        if self.approvals.is_due(now) {
            if let Ok(approvals) = self.source.approvals() {
                events.push(WatchEvent::Approvals(approvals));
            }
            self.approvals.polled(now);
        }
        if let Some(wait) = self.source.throttled_for() {
            self.live.stretch();
            self.timeline.stretch();
            self.log.stretch();
            self.live.hold_off(now, wait);
            self.timeline.hold_off(now, wait);
            self.log.hold_off(now, wait);
            events.push(WatchEvent::Throttled(wait));
        }
        events
    }

    /// How long the loop may block for before something is due. A watcher with
    /// nothing to watch waits for a request rather than for a clock.
    #[must_use]
    pub fn until_due(&self, now: Instant) -> Option<Duration> {
        if !self.is_watching() {
            return None;
        }
        let live = self.live.until_due(now);
        let timeline = self
            .focus
            .filter(|run| !self.settled.contains(run))
            .map_or(live, |_| self.timeline.until_due(now));
        let log = self.node.map_or(live, |_| self.log.until_due(now));
        Some(
            live.min(timeline)
                .min(log)
                .min(self.approvals.until_due(now)),
        )
    }
}

impl PipelineSource for crate::azure::AzureClient {
    fn live_runs(&self) -> Result<Vec<Run>> {
        self.fetch_live_runs()
    }

    fn timeline(&self, run_id: i64) -> Result<Vec<TimelineRecord>> {
        self.fetch_timeline(run_id)
    }

    fn log_lines(&self, run_id: i64, log_id: i64, start_line: usize) -> Result<Vec<String>> {
        self.fetch_log_lines(run_id, log_id, start_line)
    }

    fn run(&self, run_id: i64) -> Result<Option<Run>> {
        self.fetch_run(run_id)
    }

    fn approvals(&self) -> Result<Vec<Approval>> {
        self.fetch_approvals()
    }

    fn run_work_items(&self, run_id: i64) -> Result<Vec<i64>> {
        self.fetch_run_work_items(run_id)
    }

    fn throttled_for(&self) -> Option<Duration> {
        Self::throttled_for(self)
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
    pub fn spawn(config: crate::azure::AzureConfig) -> Result<Self> {
        let (request_sender, request_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("ticket-watch".into())
            .spawn(move || {
                let Ok(source) = crate::azure::AzureClient::connect(config) else {
                    return;
                };
                watch(
                    Watcher::new(Box::new(source)),
                    &request_receiver,
                    &event_sender,
                );
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
        /// How many timelines were read, and whether the run has finished.
        timelines: Arc<Mutex<usize>>,
        finished: Arc<Mutex<bool>>,
        /// The lines the next log read answers with, and the start line each
        /// read asked for.
        log: Arc<Mutex<Vec<String>>>,
        log_reads: Arc<Mutex<Vec<usize>>>,
        /// The errors the next reads answer with, oldest first.
        failures: Arc<Mutex<Vec<String>>>,
        /// The waits the source reports having been asked for.
        throttles: Arc<Mutex<Vec<Duration>>>,
        reads: Arc<Mutex<usize>>,
        /// How many times the approvals were read.
        approval_reads: Arc<Mutex<usize>>,
    }

    impl PipelineSource for FakeRuns {
        fn live_runs(&self) -> Result<Vec<Run>> {
            *self.reads.lock().unwrap() += 1;
            if let Some(error) = self.failures.lock().unwrap().pop() {
                return Err(anyhow::anyhow!(error));
            }
            Ok(self.runs.lock().unwrap().clone())
        }

        fn timeline(&self, _run_id: i64) -> Result<Vec<TimelineRecord>> {
            *self.timelines.lock().unwrap() += 1;
            let state = if *self.finished.lock().unwrap() {
                RunStatus::Completed
            } else {
                RunStatus::InProgress
            };
            Ok(vec![TimelineRecord {
                id: "stage-1".into(),
                parent_id: None,
                kind: crate::model::TimelineKind::Stage,
                name: "Build".into(),
                state,
                result: None,
                start: None,
                finish: None,
                percent_complete: None,
                log_id: None,
                order: 1,
                issues: Vec::new(),
            }])
        }

        fn log_lines(&self, _run_id: i64, _log_id: i64, start_line: usize) -> Result<Vec<String>> {
            self.log_reads.lock().unwrap().push(start_line);
            Ok(self.log.lock().unwrap().clone())
        }

        fn approvals(&self) -> Result<Vec<Approval>> {
            *self.approval_reads.lock().unwrap() += 1;
            Ok(Vec::new())
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
        timelines: Arc<Mutex<usize>>,
        finished: Arc<Mutex<bool>>,
        log: Arc<Mutex<Vec<String>>>,
        log_reads: Arc<Mutex<Vec<usize>>>,
        approval_reads: Arc<Mutex<usize>>,
    }

    fn watcher() -> Harness {
        let reads = Arc::new(Mutex::new(0));
        let throttles = Arc::new(Mutex::new(Vec::new()));
        let failures = Arc::new(Mutex::new(Vec::new()));
        let timelines = Arc::new(Mutex::new(0));
        let finished = Arc::new(Mutex::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_reads = Arc::new(Mutex::new(Vec::new()));
        let approval_reads = Arc::new(Mutex::new(0));
        let source = FakeRuns {
            runs: Arc::new(Mutex::new(vec![run(14)])),
            failures: Arc::clone(&failures),
            throttles: Arc::clone(&throttles),
            reads: Arc::clone(&reads),
            timelines: Arc::clone(&timelines),
            finished: Arc::clone(&finished),
            log: Arc::clone(&log),
            log_reads: Arc::clone(&log_reads),
            approval_reads: Arc::clone(&approval_reads),
        };
        Harness {
            watcher: Watcher::new(Box::new(source)),
            reads,
            throttles,
            failures,
            timelines,
            finished,
            log,
            log_reads,
            approval_reads,
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
            events.iter().any(|event| matches!(
                event,
                WatchEvent::LiveRuns(runs) if runs[0].id == 14
            )),
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
        assert!(events.iter().any(|event| matches!(
            event,
            WatchEvent::Failed(message) if message.contains("unreachable")
        )),);
        assert_eq!(watcher.live_cadence(), Duration::from_secs(30));

        let events = watcher.poll(start + Duration::from_secs(31));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, WatchEvent::Failed(_))),
            "the same failure is not said twice"
        );
        assert_eq!(
            watcher.live_cadence(),
            MAX_CADENCE.min(Duration::from_secs(60))
        );

        let events = watcher.poll(start + Duration::from_secs(120));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, WatchEvent::LiveRuns(_))),
            "and it reports again the moment it works"
        );
        assert_eq!(watcher.live_cadence(), LIVE_RUNS_CADENCE);
    }

    #[test]
    fn the_focused_runs_timeline_is_read_every_five_seconds_and_stops_when_it_finishes() {
        let Harness {
            mut watcher,
            timelines,
            finished,
            ..
        } = watcher();
        watcher.handle(&WatchRequest::TabShowing(true));
        watcher.handle(&WatchRequest::Focus {
            run_id: 14,
            node: None,
        });
        let start = Instant::now();

        let events = watcher.poll(start);
        assert!(
            events.iter().any(|event| matches!(
                event,
                WatchEvent::Timeline { run_id: 14, records } if records[0].name == "Build"
            )),
            "the run on screen is read the moment it gets there"
        );
        assert!(
            watcher.poll(start + Duration::from_secs(4)).is_empty(),
            "and not again until the cadence is up"
        );
        assert_eq!(*timelines.lock().unwrap(), 1);

        let events = watcher.poll(start + TIMELINE_CADENCE);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, WatchEvent::Timeline { .. })),
            "every five seconds while it is going"
        );
        assert_eq!(*timelines.lock().unwrap(), 2);

        // The run finishes: every node has stopped moving.
        *finished.lock().unwrap() = true;
        watcher.poll(start + Duration::from_secs(20));
        let reads = *timelines.lock().unwrap();
        watcher.poll(start + Duration::from_secs(120));
        assert_eq!(
            *timelines.lock().unwrap(),
            reads,
            "a finished run's timeline is read once and kept"
        );
    }

    #[test]
    fn a_growing_log_is_read_from_where_the_screen_left_off_and_slows_when_it_goes_quiet() {
        let Harness {
            mut watcher,
            log_reads,
            log,
            ..
        } = watcher();
        watcher.handle(&WatchRequest::TabShowing(true));
        *log.lock().unwrap() = vec!["one".to_owned(), "two".to_owned()];
        watcher.handle(&WatchRequest::Focus {
            run_id: 14,
            node: Some(LogTarget {
                log_id: 7,
                from_line: 0,
                live: true,
            }),
        });
        let start = Instant::now();

        let events = watcher.poll(start);
        assert!(
            events.iter().any(|event| matches!(
                event,
                WatchEvent::LogLines { log_id: 7, from_line: 0, lines, finished: false, .. }
                    if lines.len() == 2
            )),
            "the first poll brings back what is there"
        );
        assert_eq!(*log_reads.lock().unwrap(), [0], "starting at the top");

        // The screen now holds two lines, so the next poll asks for the third.
        watcher.handle(&WatchRequest::Focus {
            run_id: 14,
            node: Some(LogTarget {
                log_id: 7,
                from_line: 2,
                live: true,
            }),
        });
        *log.lock().unwrap() = vec!["three".to_owned()];
        let events = watcher.poll(start + LOG_CADENCE);
        assert!(
            events.iter().any(|event| matches!(
                event,
                WatchEvent::LogLines { from_line: 2, lines, .. } if lines == &["three".to_owned()]
            )),
            "and only what is new"
        );
        assert_eq!(
            *log_reads.lock().unwrap(),
            [0, 2],
            "each poll starts where the screen left off"
        );
        assert_eq!(watcher.log_cadence(), LOG_CADENCE);

        // Two empty polls in a row and the node is quiet.
        log.lock().unwrap().clear();
        watcher.poll(start + Duration::from_secs(4));
        assert_eq!(
            watcher.log_cadence(),
            LOG_CADENCE,
            "one quiet poll is nothing"
        );
        watcher.poll(start + Duration::from_secs(6));
        assert_eq!(
            watcher.log_cadence(),
            QUIET_LOG_CADENCE,
            "two in a row and it stops asking twice a second"
        );

        *log.lock().unwrap() = vec!["four".to_owned()];
        watcher.poll(start + Duration::from_secs(12));
        assert_eq!(
            watcher.log_cadence(),
            LOG_CADENCE,
            "and picks up again the moment there is something to read"
        );
    }

    #[test]
    fn a_finished_nodes_log_is_read_once_and_never_again() {
        let Harness {
            mut watcher,
            log_reads,
            log,
            ..
        } = watcher();
        watcher.handle(&WatchRequest::TabShowing(true));
        *log.lock().unwrap() = vec!["all of it".to_owned()];
        watcher.handle(&WatchRequest::Focus {
            run_id: 14,
            node: Some(LogTarget {
                log_id: 7,
                from_line: 0,
                live: false,
            }),
        });
        let start = Instant::now();

        let events = watcher.poll(start);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, WatchEvent::LogLines { finished: true, .. })),
            "the whole log, and it says it will not grow"
        );

        watcher.poll(start + Duration::from_secs(30));
        assert_eq!(
            log_reads.lock().unwrap().len(),
            1,
            "a finished node's log is never asked for twice"
        );
    }

    #[test]
    fn the_approvals_are_read_once_a_minute_and_when_the_overlay_asks() {
        let Harness {
            mut watcher,
            approval_reads,
            ..
        } = watcher();
        watcher.handle(&WatchRequest::TabShowing(true));
        let start = Instant::now();

        let events = watcher.poll(start);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, WatchEvent::Approvals(_))),
            "read once the moment there is anything to watch for"
        );
        assert!(
            watcher
                .poll(start + Duration::from_secs(30))
                .iter()
                .all(|event| !matches!(event, WatchEvent::Approvals(_))),
            "and not again for a minute"
        );
        assert_eq!(*approval_reads.lock().unwrap(), 1);

        // Opening the overlay asks for a fresh read rather than waiting.
        watcher.handle(&WatchRequest::RefreshApprovals);
        let events = watcher.poll(start + Duration::from_secs(31));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, WatchEvent::Approvals(_))),
        );
        assert_eq!(*approval_reads.lock().unwrap(), 2);
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
