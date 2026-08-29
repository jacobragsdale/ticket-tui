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

pub(super) const INFO_NOTIFICATION_DURATION: Duration = Duration::from_secs(4);
pub(super) const ERROR_NOTIFICATION_DURATION: Duration = Duration::from_secs(8);

/// Which way the draggable pane divider runs in the current layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DividerOrientation {
    /// A column between the tickets and details panes (wide layout).
    Vertical,
    /// A row between the stacked tickets and details panes.
    Horizontal,
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

/// Percentage of the workspace given to the tickets pane when the panes sit
/// side by side, and when they are stacked.
pub const DEFAULT_PANE_SPLIT_WIDE: u16 = 62;

pub const DEFAULT_PANE_SPLIT_STACKED: u16 = 56;

/// Safety rails for a stored or dragged split, applied on top of the cell
/// minimums below.
pub(crate) const MIN_SPLIT_PERCENT: u16 = 20;

pub(crate) const MAX_SPLIT_PERCENT: u16 = 80;

/// Cells each pane keeps while the divider is dragged.
const MIN_TICKETS_COLUMNS: u16 = 40;

const MIN_DETAILS_COLUMNS: u16 = 30;

const MIN_PANE_ROWS: u16 = 6;

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

/// What every screen sits inside. A screen is handed one of these on
/// each event, and reads and writes it rather than owning any of it.
#[derive(Debug)]
pub struct Shell {
    pub focus: Focus,
    pub narrow_details: bool,
    pub pane_split_wide: u16,
    pub pane_split_stacked: u16,
    pub(crate) content_area: Rect,
    pub(crate) divider: Option<DividerOrientation>,
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
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            focus: Focus::Tickets,
            narrow_details: false,
            pane_split_wide: DEFAULT_PANE_SPLIT_WIDE,
            pane_split_stacked: DEFAULT_PANE_SPLIT_STACKED,
            content_area: Rect::ZERO,
            divider: None,
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
            sync_source: None,
            sync_target: None,
            synced_at: None,
            synced_wall_clock: None,
            sync_error: None,
            sync_paused_until: None,
            me: None,
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

    /// What the table title appends after the sort order, most urgent first.
    #[must_use]
    pub fn activity_label(&self) -> Option<String> {
        if self.sync_enabled && self.sync_pending {
            return Some("Syncing…".into());
        }
        if self.reload_pending {
            return Some("Reloading…".into());
        }
        if self.sync_enabled
            && let Some(left) = self.sync_pause_left()
        {
            return Some(format!("Sync paused {}", remaining_wait(left)));
        }
        if self.sync_enabled && self.sync_error.is_some() {
            return Some("Sync failed".into());
        }
        if self.stale {
            return Some("Stale".into());
        }
        self.synced_at
            .filter(|_| self.sync_enabled)
            .map(|at| format!("Synced {}", relative_age(at.elapsed())))
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

    #[must_use]
    pub const fn content_area(&self) -> Rect {
        self.content_area
    }

    #[must_use]
    pub const fn divider_orientation(&self) -> Option<DividerOrientation> {
        self.divider
    }

    /// Moves the divider under the pointer: the tickets pane keeps everything up
    /// to the pointer, the details pane the rest.
    pub(super) fn drag_divider(&mut self, column: u16, row: u16) {
        match self.divider {
            Some(DividerOrientation::Vertical) => {
                let span = self.content_area.width;
                let cells = column.saturating_sub(self.content_area.x);
                self.pane_split_wide =
                    split_percent(cells, span, MIN_TICKETS_COLUMNS, MIN_DETAILS_COLUMNS);
            }
            Some(DividerOrientation::Horizontal) => {
                let span = self.content_area.height;
                let cells = row.saturating_sub(self.content_area.y);
                self.pane_split_stacked = split_percent(cells, span, MIN_PANE_ROWS, MIN_PANE_ROWS);
            }
            None => {}
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
                PointerTarget::PaneDivider | PointerTarget::DismissOverlay => None,
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
            DragKind::Text | DragKind::Cancelled | DragKind::Divider
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
        self.notification.as_ref().map(|notification| {
            notification
                .expires_at
                .saturating_duration_since(Instant::now())
        })
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

    /// Records the workspace the panes were last split inside, and which way the
    /// divider runs there. The narrow layout passes `None`: it has no divider.
    pub const fn set_content_layout(&mut self, area: Rect, divider: Option<DividerOrientation>) {
        self.content_area = area;
        self.divider = divider;
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
        if self
            .notification
            .as_ref()
            .is_some_and(|notification| Instant::now() >= notification.expires_at)
        {
            self.notification = None;
            return true;
        }
        false
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
