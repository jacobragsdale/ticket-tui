//! `Shell`: the state every screen shares — focus, the pointer, the
//! notification, the layout, and what the sync worker is doing.

use super::*;

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

impl Shell {
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
