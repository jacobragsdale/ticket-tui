//! `Shell`: the state every screen shares — focus, the pointer, the
//! notification, the layout, and what the sync worker is doing.

use super::*;
use crate::model::{PrStatus, RunResult, RunStatus};
use work_items::clamp_pos_to_snapshot;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Focus {
    #[default]
    Tickets,
    Family,
    Details,
}

impl Focus {
    #[must_use]
    pub const fn is_details_pane(self) -> bool {
        matches!(self, Self::Family | Self::Details)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationLevel {
    Info,
    Error,
}

#[derive(Debug)]
pub(crate) struct Notification {
    pub(crate) message: String,
    pub(crate) level: NotificationLevel,
    pub(crate) expires_at: Instant,
}

/// How long a row stays flagged after an edit of it lands or is taken back.
/// Two frames at the spinner's cadence: long enough to catch the eye on a row
/// that may be several away from the cursor, short enough not to linger.
pub(super) const FLASH_DURATION: Duration = Duration::from_millis(220);

pub(super) const INFO_NOTIFICATION_DURATION: Duration = Duration::from_secs(4);
pub(super) const ERROR_NOTIFICATION_DURATION: Duration = Duration::from_secs(8);

/// Which way a draggable pane seam runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DividerOrientation {
    /// A column between two panes side by side.
    Vertical,
    /// A row between two stacked panes.
    Horizontal,
}

/// One seam on screen this frame, as the renderer registered it: which way it
/// runs, the workspace its position is a percentage of, and the cells each
/// pane keeps while it is dragged. Every seam the app draws is one of these,
/// so dragging one is the same work wherever it was drawn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneSeam {
    pub orientation: DividerOrientation,
    pub workspace: Rect,
    pub first_min: u16,
    pub second_min: u16,
}

#[derive(Debug)]
pub struct PointerUpdate {
    pub action: AppAction,
    pub redraw: bool,
}

impl PointerUpdate {
    pub(super) fn none(redraw: bool) -> Self {
        Self {
            action: AppAction::None,
            redraw,
        }
    }

    pub(super) fn action(action: AppAction) -> Self {
        Self {
            action,
            redraw: true,
        }
    }
}

/// Percentage of the workspace given to the list pane when the panes sit
/// side by side, and when they are stacked.
pub const DEFAULT_PANE_SPLIT_WIDE: u16 = 62;

pub const DEFAULT_PANE_SPLIT_STACKED: u16 = 56;

/// Percentage of the details pane given to its first half, for a tab that
/// divides it again: the pipelines run above its log.
pub const DEFAULT_PANE_SPLIT_DETAILS: u16 = 55;

/// Safety rails for a stored or dragged split, applied on top of the cell
/// minimums the renderer registers with each seam.
pub(crate) const MIN_SPLIT_PERCENT: u16 = 20;

pub(crate) const MAX_SPLIT_PERCENT: u16 = 80;

/// Compact wording for a wait still to come, coarse on purpose: the exact
/// second the timer comes back is nobody's business, and a title that ticks
/// every second is a title that has to be redrawn every second.
fn remaining_wait(left: Duration) -> String {
    // Rounded up, so a two minute pause read a millisecond after it started
    // still says two minutes rather than counting down from one.
    let seconds = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m", seconds.div_ceil(60))
    }
}

/// Compact relative wording shared by the freshness and sync labels.
pub(crate) fn relative_age(age: Duration) -> String {
    if age.as_secs() < 45 {
        "just now".into()
    } else if age.as_secs() < 3600 {
        format!("{}m ago", age.as_secs() / 60)
    } else if age.as_secs() < 86_400 {
        format!("{}h ago", age.as_secs() / 3600)
    } else {
        format!("{}d ago", age.as_secs() / 86_400)
    }
}

/// Turns a divider position, measured in cells from the start of the workspace,
/// into a percentage for the first pane. The clamp keeps `first_min` cells for
/// that pane and `second_min` cells plus the one-cell divider for the other,
/// then holds the result inside the 20..=80 safety rails.
fn split_percent(cells: u16, span: u16, first_min: u16, second_min: u16) -> u16 {
    if span == 0 {
        return MIN_SPLIT_PERCENT;
    }
    let span = u32::from(span);
    let low = (u32::from(first_min) * 100)
        .div_ceil(span)
        .clamp(u32::from(MIN_SPLIT_PERCENT), u32::from(MAX_SPLIT_PERCENT));
    let high = (span.saturating_sub(u32::from(second_min) + 1) * 100 / span)
        .min(u32::from(MAX_SPLIT_PERCENT))
        .max(low);
    let percent = u32::from(cells) * 100 / span;
    u16::try_from(percent.clamp(low, high)).unwrap_or(MIN_SPLIT_PERCENT)
}

/// What the status bar says about the sync. The glyph and the colour are the
/// renderer's; this is the state, and the words for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyncStatus {
    /// Nothing to pull from: no organization configured, or a database
    /// another project filled.
    Offline,
    Syncing,
    Reloading,
    /// Azure DevOps asked to be left alone, and for how much longer.
    Paused(String),
    Failed,
    Stale,
    /// The sync is on and nothing has come back yet.
    Waiting,
    /// How long ago the last pull finished.
    Synced(String),
}

impl SyncStatus {
    /// What the bar writes after the glyph.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Offline => "Offline".to_owned(),
            Self::Syncing => "Syncing…".to_owned(),
            Self::Reloading => "Reloading…".to_owned(),
            Self::Paused(left) => format!("Sync paused {left}"),
            Self::Failed => "Sync failed".to_owned(),
            Self::Stale => "Stale".to_owned(),
            Self::Waiting => "Not synced".to_owned(),
            Self::Synced(age) => format!("Synced {age}"),
        }
    }
}

/// What every screen sits inside. A screen is handed one of these on
/// each event, and reads and writes it rather than owning any of it.
#[derive(Debug)]
pub struct Shell {
    pub focus: Focus,
    /// Whether the layout that shows one pane at a time is showing the
    /// details rather than the list. Shared by every tab, so switching tabs
    /// stays on the pane you were reading.
    pub narrow_details: bool,
    pub pane_split_wide: u16,
    pub pane_split_stacked: u16,
    pub pane_split_details: u16,
    /// The seams on screen this frame, by [`PaneSplit::index`]. Registered by
    /// the pane renderer and read by the drag.
    pub(crate) seams: [Option<PaneSeam>; PaneSplit::ALL.len()],
    pub reload_pending: bool,
    pub should_quit: bool,
    pub session_dirty: bool,
    pub(crate) notification: Option<Notification>,
    pub hit_regions: HitRegions,
    pub pointer: PointerState,
    /// Where the open picker or prompt is drawn: centred, as every
    /// keyboard-opened one is, or hung off the details-pane field that was
    /// clicked to open it.
    pub overlay_anchor: OverlayAnchor,
    pub loaded_at: Instant,
    pub database_path: PathBuf,
    pub stale: bool,
    pub data_signature: u128,
    /// Whether a pull from Azure DevOps is in flight.
    pub sync_pending: bool,
    /// Why there is nothing to write to, reported when an edit is attempted
    /// without a configured Azure DevOps project.
    /// The project's Git repositories, as the last pull found them. Every tab
    /// reads these: a pull request, a run and an artifact link all name a
    /// repository by its GUID.
    pub(crate) repos: Vec<Repo>,
    /// The directory the Repos tab looks for clones in and makes new ones
    /// under. `None` when there is no home directory to fall back on.
    pub(crate) workspace: Option<std::path::PathBuf>,
    /// What each work item is called, by id, so the tabs that only carry ids
    /// — a pull request's linked items, a run's — can name them.
    pub(crate) work_item_titles: Vec<(i64, String)>,
    /// What each pull request and run is called, for the work items tab, whose
    /// artifact links hold nothing but ids. Filled from the same snapshot the
    /// other tabs draw, so a link names what the database holds and says
    /// nothing about what it does not.
    pub(crate) pull_request_labels: Vec<(i64, String, PrStatus)>,
    pub(crate) run_labels: Vec<(i64, String, RunStatus, Option<RunResult>)>,
    /// What the pipeline watcher is doing, as the database overlay reports
    /// it. `None` for a run with no watcher at all.
    pub(crate) watch_state: Option<String>,
    /// Everywhere this run has been, oldest last, across every tab. `[` walks
    /// back through it and `]` forward through what `[` came off.
    pub(crate) history: Vec<Jump>,
    pub(crate) future: Vec<Jump>,
    pub(crate) offline_reason: Option<String>,
    /// Whether Azure DevOps is configured at all: an offline run browses the
    /// database and reports no sync state.
    pub(crate) sync_enabled: bool,
    /// Where the rows come from — the organization and project, how often they
    /// are pulled, and the scope narrowing them — as the database overlay
    /// reports it. `None` until the run resolves a project.
    /// The row an edit has just landed on, and how long it stays flagged.
    pub(crate) flash: Option<(TicketKey, Instant)>,
    pub(crate) sync_source: Option<String>,
    /// The same project, as the agent context publishes it. `None` until the
    /// run resolves one.
    pub(crate) sync_target: Option<SyncTarget>,
    /// When the last successful pull finished, which is not `loaded_at`: a
    /// SQLite reload moves that too.
    pub(crate) synced_at: Option<Instant>,
    /// The same moment on the wall clock, because the agent context has to say
    /// when the last pull landed and an `Instant` only says how long ago.
    pub(crate) synced_wall_clock: Option<Timestamp>,
    /// The last pull's error, kept so the same timer failure is reported once.
    pub(crate) sync_error: Option<String>,
    /// When the next pull may go out, for a timer Azure DevOps asked to hold
    /// off. Not a failure: the title counts it down instead of saying the sync
    /// broke, and nothing is announced.
    pub(crate) sync_paused_until: Option<Instant>,
    /// Display name of the signed-in Azure DevOps user, so their own work
    /// items can stand out. `None` until a sync records one.
    pub(crate) me: Option<String>,
    /// Why ARM cannot be reached, in one line, for the tabs that read it:
    /// no subscription resolved, or the Azure CLI is not signed in. `None`
    /// once a subscription has resolved, which is when those tabs can read.
    pub(crate) arm_state: Option<String>,
    /// The subscription the ACR and Key Vault tabs read, as the agent context
    /// publishes it. `None` until one resolves.
    pub(crate) arm_subscription: Option<String>,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            focus: Focus::Tickets,
            narrow_details: false,
            pane_split_wide: DEFAULT_PANE_SPLIT_WIDE,
            pane_split_stacked: DEFAULT_PANE_SPLIT_STACKED,
            pane_split_details: DEFAULT_PANE_SPLIT_DETAILS,
            seams: [None; PaneSplit::ALL.len()],
            reload_pending: false,
            should_quit: false,
            session_dirty: false,
            notification: None,
            hit_regions: HitRegions::default(),
            pointer: PointerState::default(),
            overlay_anchor: OverlayAnchor::Centered,
            loaded_at: Instant::now(),
            database_path: PathBuf::new(),
            stale: false,
            data_signature: 0,
            sync_pending: false,
            repos: Vec::new(),
            workspace: None,
            work_item_titles: Vec::new(),
            pull_request_labels: Vec::new(),
            run_labels: Vec::new(),
            watch_state: None,
            history: Vec::new(),
            future: Vec::new(),
            offline_reason: None,
            sync_enabled: false,
            flash: None,
            sync_source: None,
            sync_target: None,
            synced_at: None,
            synced_wall_clock: None,
            sync_error: None,
            sync_paused_until: None,
            me: None,
            arm_state: None,
            arm_subscription: None,
        }
    }
}

impl Shell {
    /// What the work items on file are called, for the tabs that name them by
    /// id.
    pub fn set_work_item_titles(&mut self, titles: Vec<(i64, String)>) {
        self.work_item_titles = titles;
    }

    #[must_use]
    pub fn work_item_titles(&self) -> &[(i64, String)] {
        &self.work_item_titles
    }

    /// What the pull requests and runs the artifact links point at are called.
    /// Both come from the snapshot every tab draws, so a link to something the
    /// database does not hold simply finds nothing here.
    pub fn set_artifact_labels(
        &mut self,
        pull_requests: Vec<(i64, String, PrStatus)>,
        runs: Vec<(i64, String, RunStatus, Option<RunResult>)>,
    ) {
        self.pull_request_labels = pull_requests;
        self.run_labels = runs;
    }

    /// One pull request's title and status, when the database holds it.
    #[must_use]
    pub fn pull_request_label(&self, id: i64) -> Option<(&str, PrStatus)> {
        self.pull_request_labels
            .iter()
            .find(|(held, _, _)| *held == id)
            .map(|(_, title, status)| (title.as_str(), *status))
    }

    /// One run's build number and how it went, when the database holds it.
    #[must_use]
    pub fn run_label(&self, id: i64) -> Option<(&str, RunStatus, Option<RunResult>)> {
        self.run_labels
            .iter()
            .find(|(held, ..)| *held == id)
            .map(|(_, number, status, result)| (number.as_str(), *status, *result))
    }

    /// What one work item is called, when the database holds it.
    #[must_use]
    pub fn work_item_title(&self, id: i64) -> Option<&str> {
        self.work_item_titles
            .iter()
            .find(|(held, _)| *held == id)
            .map(|(_, title)| title.as_str())
    }

    /// What the watcher is doing, for the database overlay.
    pub fn set_watch_state(&mut self, state: Option<String>) {
        self.watch_state = state;
    }

    #[must_use]
    pub fn watch_state(&self) -> Option<&str> {
        self.watch_state.as_deref()
    }

    /// What the last pull found. Written on every pull that changed them.
    pub fn set_repos(&mut self, repos: Vec<Repo>) {
        self.repos = repos;
    }

    #[must_use]
    pub fn repos(&self) -> &[Repo] {
        &self.repos
    }

    /// Where clones are looked for and made: `--workspace`, then
    /// `TICKET_TUI_WORKSPACE`, then `~/Development`.
    pub fn set_workspace(&mut self, workspace: Option<std::path::PathBuf>) {
        self.workspace = workspace;
    }

    #[must_use]
    pub fn workspace(&self) -> Option<&std::path::Path> {
        self.workspace.as_deref()
    }

    /// What a repository GUID is called, for the tabs that only ever see the
    /// GUID. An id nothing on file matches reads as the id itself, so a link
    /// still says something.
    #[must_use]
    pub fn repo_name(&self, id: &str) -> String {
        self.repos
            .iter()
            .find(|repo| repo.id == id)
            .map_or_else(|| id.to_owned(), |repo| repo.name.clone())
    }

    /// Records somewhere this run has been. Arriving where you already are is
    /// not a move, and going somewhere new is what closes off the forward
    /// list — the same rule a browser's back button follows.
    pub fn record_jump(&mut self, jump: Jump) {
        if self.history.last() == Some(&jump) {
            return;
        }
        self.history.push(jump);
        if self.history.len() > 50 {
            self.history.remove(0);
        }
        self.future.clear();
        self.session_dirty = true;
    }

    /// Everywhere this run has been, for the session file.
    #[must_use]
    pub fn history(&self) -> &[Jump] {
        &self.history
    }

    /// Forgets a target that is not there any more, wherever it sits in the
    /// history, so `[` never lands on a deleted row.
    pub fn forget_jump(&mut self, jump: &Jump) {
        self.history.retain(|held| held != jump);
        self.future.retain(|held| held != jump);
    }

    /// What the status bar's right-hand segment reports, most urgent first.
    #[must_use]
    pub fn sync_status(&self) -> SyncStatus {
        if self.sync_enabled && self.sync_pending {
            return SyncStatus::Syncing;
        }
        if self.reload_pending {
            return SyncStatus::Reloading;
        }
        if self.sync_enabled
            && let Some(left) = self.sync_pause_left()
        {
            return SyncStatus::Paused(remaining_wait(left));
        }
        if self.sync_enabled && self.sync_error.is_some() {
            return SyncStatus::Failed;
        }
        if self.stale {
            return SyncStatus::Stale;
        }
        if !self.sync_enabled {
            return SyncStatus::Offline;
        }
        self.synced_at.map_or(SyncStatus::Waiting, |at| {
            SyncStatus::Synced(relative_age(at.elapsed()))
        })
    }

    /// The project the rows come from, for the status bar to name beside the
    /// sync state when the row is wide enough for it.
    #[must_use]
    pub fn project_label(&self) -> Option<&str> {
        self.sync_target
            .as_ref()
            .map(|target| target.project.as_str())
    }

    /// A pull has started.
    pub const fn begin_sync(&mut self) {
        self.sync_pending = true;
    }

    pub fn configure_database(&mut self, path: PathBuf, signature: u128) {
        self.database_path = path;
        self.data_signature = signature;
        self.loaded_at = Instant::now();
        self.stale = false;
    }

    /// Which way the seam between a tab's list and its details runs, or
    /// `None` in the layout that shows one pane at a time and has no seam.
    #[must_use]
    pub fn divider_orientation(&self) -> Option<DividerOrientation> {
        self.seam_orientation(PaneSplit::Workspace)
    }

    /// The seam one split drew this frame, if it drew one.
    #[must_use]
    pub(crate) fn seam(&self, split: PaneSplit) -> Option<PaneSeam> {
        self.seams[split.index()]
    }

    #[must_use]
    pub fn seam_orientation(&self, split: PaneSplit) -> Option<DividerOrientation> {
        self.seam(split).map(|seam| seam.orientation)
    }

    /// Records a seam the renderer has just laid out, so a drag on it knows
    /// what it is moving. Called as the frame is painted, before the panes
    /// either side of it are drawn.
    pub(crate) fn set_seam(&mut self, split: PaneSplit, seam: PaneSeam) {
        self.seams[split.index()] = Some(seam);
    }

    /// Forgets last frame's seams, the way the hit regions are forgotten: a
    /// layout with no room for a seam registers none, and there is nothing
    /// left over to drag.
    pub(crate) fn clear_seams(&mut self) {
        self.seams = [None; PaneSplit::ALL.len()];
    }

    /// Moves a seam to the pointer: the first pane keeps everything up to it,
    /// the second the rest. Which stored split that percentage is depends on
    /// the seam, so the same drag serves every pane in the app.
    pub(super) fn drag_divider(&mut self, split: PaneSplit, column: u16, row: u16) {
        let Some(seam) = self.seam(split) else {
            return;
        };
        let percent = match seam.orientation {
            DividerOrientation::Vertical => split_percent(
                column.saturating_sub(seam.workspace.x),
                seam.workspace.width,
                seam.first_min,
                seam.second_min,
            ),
            DividerOrientation::Horizontal => split_percent(
                row.saturating_sub(seam.workspace.y),
                seam.workspace.height,
                seam.first_min,
                seam.second_min,
            ),
        };
        *self.split_percent_mut(split, seam.orientation) = percent;
    }

    /// The stored percentage a seam moves: the workspace keeps one for each
    /// way it can be arranged, and the details pane one of its own.
    fn split_percent_mut(&mut self, split: PaneSplit, orientation: DividerOrientation) -> &mut u16 {
        match (split, orientation) {
            (PaneSplit::Workspace, DividerOrientation::Vertical) => &mut self.pane_split_wide,
            (PaneSplit::Workspace, DividerOrientation::Horizontal) => &mut self.pane_split_stacked,
            (PaneSplit::Details, _) => &mut self.pane_split_details,
        }
    }

    /// The percentage the first pane of a split gets.
    #[must_use]
    pub(crate) const fn split_percent(
        &self,
        split: PaneSplit,
        orientation: DividerOrientation,
    ) -> u16 {
        match (split, orientation) {
            (PaneSplit::Workspace, DividerOrientation::Vertical) => self.pane_split_wide,
            (PaneSplit::Workspace, DividerOrientation::Horizontal) => self.pane_split_stacked,
            (PaneSplit::Details, _) => self.pane_split_details,
        }
    }

    /// The two chips the one-pane layout switches with, which are the pane
    /// system's rather than any screen's. Answers whether it took the click.
    pub(crate) fn activate_pane_target(&mut self, target: &PointerTarget) -> bool {
        match target {
            PointerTarget::NarrowTickets => {
                self.narrow_details = false;
                self.focus = Focus::Tickets;
                true
            }
            PointerTarget::NarrowDetails => {
                self.narrow_details = true;
                if !self.focus.is_details_pane() {
                    self.focus = Focus::Details;
                }
                true
            }
            _ => false,
        }
    }

    /// Turns on the sync parts of the UI. An offline run leaves them off, so
    /// the table title says nothing about a sync that can not happen.
    pub const fn enable_sync(&mut self) {
        self.sync_enabled = true;
    }

    /// A pull failed. Reports whether the failure is worth a notification: the
    /// same error on consecutive timer pulls is not, because the table title
    /// already says the sync is failing. `announce` forces one anyway, for a
    /// pull the user asked for.
    pub fn fail_sync(&mut self, error: &str, announce: bool) -> bool {
        self.sync_pending = false;
        self.sync_paused_until = None;
        let repeated = self.sync_error.as_deref() == Some(error);
        self.sync_error = Some(error.to_owned());
        announce || !repeated
    }

    /// A pull succeeded. The tickets it brought are applied separately, so this
    /// only records that Azure DevOps was reached.
    pub fn finish_sync(&mut self) {
        self.sync_pending = false;
        self.sync_error = None;
        self.sync_paused_until = None;
        self.synced_at = Some(Instant::now());
        self.synced_wall_clock = Some(Timestamp::now());
    }

    #[must_use]
    pub fn freshness_label(&self) -> String {
        relative_age(self.loaded_at.elapsed())
    }

    pub(super) fn handle_hover(&mut self, column: u16, row: u16) -> PointerUpdate {
        self.pointer.set_position(column, row);
        PointerUpdate::none(self.refresh_hover())
    }

    pub(super) fn handle_press(&mut self, column: u16, row: u16) -> PointerUpdate {
        let region = self.hit_regions.resolve(column, row).cloned();
        let selectable = self.hit_regions.resolve_selectable(column, row);
        self.pointer.clear_selection();
        if let Some(region) = region {
            let scrollbar = match region.target {
                PointerTarget::ScrollbarThumb { surface } => Some(surface),
                _ => None,
            };
            let selectable = match region.target {
                // Neither drags text: one resizes the panes, and the other is
                // the empty space around a dropdown.
                PointerTarget::PaneDivider { .. } | PointerTarget::DismissOverlay => None,
                _ => selectable,
            };
            self.pointer.hover = Some(region.target.clone());
            self.pointer
                .begin_press(region.target, column, row, selectable, scrollbar);
        } else {
            self.pointer.hover = None;
            self.pointer.clear_press();
        }
        PointerUpdate::none(true)
    }

    pub fn handle_resize(&mut self) {
        self.pointer.clear_selection();
        if matches!(
            self.pointer.drag(),
            DragKind::Text | DragKind::Cancelled | DragKind::Divider { .. }
        ) {
            self.pointer.set_drag(DragKind::Cancelled);
        }
    }

    #[must_use]
    pub fn hovered(&self) -> Option<&PointerTarget> {
        self.pointer.hover.as_ref()
    }

    pub(crate) fn hovered_region(&self) -> Option<&crate::pointer::PointerRegion> {
        let (column, row) = self.pointer.position()?;
        self.hit_regions.resolve(column, row)
    }

    /// Whether a work item is assigned to the signed-in user.
    #[must_use]
    pub fn is_mine(&self, ticket: &Ticket) -> bool {
        match (self.me.as_deref(), ticket.assigned_to.as_deref()) {
            (Some(me), Some(assignee)) => same_text(me, assignee),
            _ => false,
        }
    }

    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    #[must_use]
    pub fn me(&self) -> Option<&str> {
        self.me.as_deref()
    }

    #[must_use]
    pub fn next_wakeup(&self) -> Option<Duration> {
        let notification = self.notification.as_ref().map(|notification| {
            notification
                .expires_at
                .saturating_duration_since(Instant::now())
        });
        let flash = self
            .flash
            .as_ref()
            .map(|(_, until)| until.saturating_duration_since(Instant::now()));
        [notification, flash].into_iter().flatten().min()
    }

    #[must_use]
    pub fn notification(&self) -> Option<(&str, NotificationLevel)> {
        self.notification
            .as_ref()
            .map(|notification| (notification.message.as_str(), notification.level))
    }

    /// Azure DevOps asked to be left alone until `until`, and the timer agreed.
    /// Nothing is wrong and nothing is announced: this is the pause the title
    /// counts down, and the next success clears it. Deliberately not
    /// [`Self::fail_sync`] — a throttled pull is the service working as
    /// designed, and an error toast a minute would only be noise.
    pub fn pause_sync(&mut self, until: Instant) {
        self.sync_pending = false;
        self.sync_error = None;
        self.sync_paused_until = Some(until);
    }

    pub fn refresh_hover(&mut self) -> bool {
        let hover = self
            .pointer
            .position()
            .and_then(|(column, row)| self.hit_regions.resolve(column, row))
            .map(|region| region.target.clone());
        let changed = hover != self.pointer.hover;
        self.pointer.hover = hover;
        changed
    }

    /// Restores the built-in split for both layouts.
    pub(super) fn reset_pane_split(&mut self) {
        self.pane_split_wide = DEFAULT_PANE_SPLIT_WIDE;
        self.pane_split_stacked = DEFAULT_PANE_SPLIT_STACKED;
        self.pane_split_details = DEFAULT_PANE_SPLIT_DETAILS;
        self.session_dirty = true;
        self.set_status("Reset pane split");
    }

    pub(super) fn scrollbar_grab(&self, surface: ScrollSurface, origin: Option<(u16, u16)>) -> i16 {
        let Some((_, row)) = origin else {
            return 0;
        };
        let Some(metrics) = self.hit_regions.scroll(surface) else {
            return 0;
        };
        let Some(thumb) = metrics.thumb() else {
            return 0;
        };
        i16::try_from(row).unwrap_or(0)
            - i16::try_from(metrics.track.y.saturating_add(thumb.y)).unwrap_or(0)
    }

    #[must_use]
    pub fn selection(&self) -> Option<TextSelection> {
        self.pointer.selection
    }

    /// Flags one row: the edit on it has landed, or been taken back. The row
    /// paints its marker in the accent until the flash runs out, which the
    /// loop wakes for the way it wakes for an expiring notification.
    pub fn flash_row(&mut self, key: TicketKey) {
        self.flash = Some((key, Instant::now() + FLASH_DURATION));
    }

    /// Whether a row is flagged this instant.
    #[must_use]
    pub fn flashing(&self) -> bool {
        self.flash
            .as_ref()
            .is_some_and(|(_, until)| Instant::now() < *until)
    }

    /// Whether this row is the one flagged this instant.
    #[must_use]
    pub fn flashing_row(&self, key: &TicketKey) -> bool {
        self.flash
            .as_ref()
            .is_some_and(|(flashed, until)| flashed == key && Instant::now() < *until)
    }

    pub fn set_error(&mut self, message: impl Into<String>) {
        self.set_notification(
            message,
            NotificationLevel::Error,
            ERROR_NOTIFICATION_DURATION,
        );
    }

    pub fn set_me(&mut self, me: Option<String>) {
        self.me = me;
    }

    fn set_notification(
        &mut self,
        message: impl Into<String>,
        level: NotificationLevel,
        duration: Duration,
    ) {
        self.notification = Some(Notification {
            message: message.into(),
            level,
            expires_at: Instant::now() + duration,
        });
    }

    /// Why the TUI cannot write anything, told to whoever tries to.
    pub fn set_offline_reason(&mut self, reason: Option<String>) {
        self.offline_reason = reason;
    }

    /// Why ARM cannot be reached, or `None` once a subscription resolved.
    pub fn set_arm_state(&mut self, state: Option<String>) {
        self.arm_state = state;
    }

    /// The same reason, for the tab that has to say why its table is empty.
    #[must_use]
    pub fn arm_state(&self) -> Option<&str> {
        self.arm_state.as_deref()
    }

    /// The subscription the ARM tabs read, once one has resolved.
    pub fn set_arm_subscription(&mut self, subscription: Option<String>) {
        self.arm_subscription = subscription;
    }

    #[must_use]
    pub fn arm_subscription(&self) -> Option<&str> {
        self.arm_subscription.as_deref()
    }

    pub fn set_status(&mut self, message: impl Into<String>) {
        self.set_notification(message, NotificationLevel::Info, INFO_NOTIFICATION_DURATION);
    }

    /// A notice worth the longer wait an error gets, without being one: a
    /// watched build finishing is news you may have looked away from.
    pub fn set_news(&mut self, message: impl Into<String>) {
        self.set_notification(
            message,
            NotificationLevel::Info,
            ERROR_NOTIFICATION_DURATION,
        );
    }

    /// Where the rows are pulled from, as the database overlay reports it.
    pub fn set_sync_source(&mut self, source: Option<String>) {
        self.sync_source = source;
    }

    /// The same project, for the agent context, which needs the organization,
    /// the project, and the interval apart rather than as one line of prose.
    pub fn set_sync_target(&mut self, target: Option<SyncTarget>) {
        self.sync_target = target;
    }

    /// How long the throttling pause still has to run, or `None` once it is
    /// over and the timer is free again.
    fn sync_pause_left(&self) -> Option<Duration> {
        let left = self
            .sync_paused_until?
            .saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left)
    }

    /// How the last pull went, for a run that can pull at all.
    fn sync_state(&self) -> String {
        let last = self
            .synced_at
            .map_or_else(|| "not yet".to_owned(), |at| relative_age(at.elapsed()));
        if self.sync_pending {
            format!("in progress, last {last}")
        } else if let Some(left) = self.sync_pause_left() {
            format!(
                "paused for throttling, next in {}, last {last}",
                remaining_wait(left)
            )
        } else if let Some(error) = &self.sync_error {
            format!("failed, last {last}: {error}")
        } else {
            last
        }
    }

    /// The database overlay's one-line account of the sync: where the rows come
    /// from, and how the last pull went. An offline run says why it is offline
    /// there instead — a missing organization, or a database another project
    /// filled.
    #[must_use]
    pub fn sync_summary(&self) -> String {
        let state = if self.sync_enabled {
            self.sync_state()
        } else {
            self.offline_reason.as_ref().map_or_else(
                || "offline; no Azure DevOps organization configured".to_owned(),
                |reason| format!("offline; {reason}"),
            )
        };
        match &self.sync_source {
            Some(source) => format!("{source} · {state}"),
            None => state,
        }
    }

    pub fn tick(&mut self) -> bool {
        let mut redraw = false;
        if self
            .notification
            .as_ref()
            .is_some_and(|notification| Instant::now() >= notification.expires_at)
        {
            self.notification = None;
            redraw = true;
        }
        if self
            .flash
            .as_ref()
            .is_some_and(|(_, until)| Instant::now() >= *until)
        {
            self.flash = None;
            redraw = true;
        }
        redraw
    }

    /// Puts the keyboard back on the list pane, and the list back on screen
    /// where only one pane fits: leaving the details pane should not leave the
    /// keyboard on a pane that is no longer drawn.
    pub(super) fn focus_list(&mut self) {
        self.focus = Focus::Tickets;
        self.narrow_details = false;
    }

    pub(super) fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Tickets => Focus::Details,
            Focus::Family => Focus::Details,
            Focus::Details => Focus::Tickets,
        };
        self.narrow_details = self.focus.is_details_pane();
    }

    pub(super) fn toggle_narrow_details(&mut self) {
        self.narrow_details = !self.narrow_details;
        if self.narrow_details {
            if !self.focus.is_details_pane() {
                self.focus = Focus::Details;
            }
        } else {
            self.focus = Focus::Tickets;
        }
    }

    pub(super) fn update_text_drag(&mut self, column: u16, row: u16) {
        let Some(surface) = self
            .pointer
            .selection
            .map(|selection| selection.surface)
            .or_else(|| self.pointer.press_selectable())
        else {
            return;
        };
        let Some(snapshot) = self.hit_regions.selectable(surface) else {
            return;
        };
        let Some(end) = snapshot
            .pos_at(column, row)
            .or_else(|| clamp_pos_to_snapshot(snapshot, column, row))
        else {
            return;
        };
        if let Some(selection) = self.pointer.selection.as_mut() {
            selection.end = end;
        } else if let Some(origin) = self.pointer.press_origin()
            && let Some(start) = snapshot.pos_at(origin.0, origin.1)
        {
            self.pointer.selection = Some(TextSelection {
                surface,
                start,
                end,
            });
        }
    }

    /// Why nothing can be written right now, and `None` while Azure DevOps is
    /// configured. Every editor asks this before it changes anything.
    pub(super) fn write_refusal(&self) -> Option<String> {
        (!self.sync_enabled).then(|| {
            self.offline_reason
                .clone()
                .unwrap_or_else(|| "no Azure DevOps organization is configured".to_owned())
        })
    }
}
