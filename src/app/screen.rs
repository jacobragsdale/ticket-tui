//! `Screen`: what a tab is. The shell hands one of these every event and every
//! frame, and knows nothing else about it. All four tabs implement it, and the
//! mouse is read here rather than by any of them, so a press, a drag and a
//! click mean the same thing wherever they land.

use ratatui::Frame;
use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

use super::{AppAction, CopiedContent, PointerUpdate, Shell};
use crate::columns::ColumnLayout;
use crate::model::Jump;
use crate::pointer::{
    DragKind, PointerTarget, ScrollState, ScrollSurface, TextEditor, TextSelection,
    extract_selected_text, offset_from_thumb,
};
use crate::session::TabSession;
use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};

/// The four screens the shell puts behind keys `1`–`4`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TabId {
    #[default]
    WorkItems,
    Repos,
    PullRequests,
    Pipelines,
}

impl TabId {
    pub const ALL: [Self; 4] = [
        Self::WorkItems,
        Self::Repos,
        Self::PullRequests,
        Self::Pipelines,
    ];

    /// What the tab bar calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::WorkItems => "Work items",
            Self::Repos => "Repos",
            Self::PullRequests => "Pull requests",
            Self::Pipelines => "Pipelines",
        }
    }

    /// What the bar calls it when the full names do not fit.
    #[must_use]
    pub const fn short_label(self) -> &'static str {
        match self {
            Self::WorkItems => "Items",
            Self::Repos => "Repos",
            Self::PullRequests => "PRs",
            Self::Pipelines => "Runs",
        }
    }

    /// The digit that switches to it.
    #[must_use]
    pub const fn number(self) -> char {
        match self {
            Self::WorkItems => '1',
            Self::Repos => '2',
            Self::PullRequests => '3',
            Self::Pipelines => '4',
        }
    }

    /// The tab a digit asks for, if it is one of the four.
    #[must_use]
    pub fn from_number(character: char) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.number() == character)
    }

    /// Where it sits in the bar, which is what a click carries.
    #[must_use]
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|tab| *tab == self)
            .unwrap_or_default()
    }
}

pub trait Screen {
    /// One key, in whatever mode the screen is in.
    fn handle_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction;

    /// Text pasted into whichever editor the screen has open.
    fn handle_paste(&mut self, shell: &mut Shell, pasted: &str);

    /// One mouse event, read the same way on every tab: hover, a press that
    /// arms whatever is under it, a drag that moves a seam, a scrollbar thumb
    /// or a text selection, and a release that acts on what the press armed.
    ///
    /// A click lands on release rather than on press, so a press that moves
    /// away is not a click, and it acts on what was pressed rather than on
    /// whatever the pointer has wandered onto by the time the button is up.
    fn handle_mouse(&mut self, shell: &mut Shell, mouse: MouseEvent) -> PointerUpdate {
        shell.pointer.set_position(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp => self.handle_wheel(shell, mouse.column, mouse.row, -3),
            MouseEventKind::ScrollDown => self.handle_wheel(shell, mouse.column, mouse.row, 3),
            MouseEventKind::Down(MouseButton::Left) => shell.handle_press(mouse.column, mouse.row),
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved
                if shell.pointer.is_pressed() =>
            {
                self.handle_drag(shell, mouse.column, mouse.row)
            }
            MouseEventKind::Moved => shell.handle_hover(mouse.column, mouse.row),
            MouseEventKind::Up(MouseButton::Left) => {
                self.handle_release(shell, mouse.column, mouse.row)
            }
            _ => PointerUpdate::none(false),
        }
    }

    /// The wheel over whichever scrolling surface is under the pointer.
    fn handle_wheel(
        &mut self,
        shell: &mut Shell,
        column: u16,
        row: u16,
        delta: i32,
    ) -> PointerUpdate {
        let hover_changed = shell.refresh_hover();
        let Some(surface) = shell.hit_regions.resolve_scroll(column, row) else {
            return PointerUpdate::none(hover_changed);
        };
        let changed = self.scroll_state_mut(surface).scroll_by(delta);
        PointerUpdate::none(changed || hover_changed)
    }

    /// The pointer moving with the button down. What a drag does is settled
    /// once, on the first move away from the press, and held until the button
    /// comes up.
    fn handle_drag(&mut self, shell: &mut Shell, column: u16, row: u16) -> PointerUpdate {
        let hover = shell
            .hit_regions
            .resolve(column, row)
            .map(|region| region.target.clone());
        let hover_changed = hover != shell.pointer.hover;
        shell.pointer.hover = hover;
        if !shell.pointer.moved_from_origin(column, row)
            && matches!(shell.pointer.drag(), DragKind::None)
        {
            return PointerUpdate::none(hover_changed);
        }
        match shell.pointer.drag() {
            DragKind::Scrollbar { surface, grab } => {
                self.drag_scrollbar(shell, surface, row, grab);
                PointerUpdate::none(true)
            }
            DragKind::Text => {
                shell.update_text_drag(column, row);
                PointerUpdate::none(true)
            }
            DragKind::Divider { split } => {
                shell.drag_divider(split, column, row);
                PointerUpdate::none(true)
            }
            DragKind::Cancelled => PointerUpdate::none(hover_changed),
            DragKind::None => {
                if let Some(PointerTarget::PaneDivider { split }) = shell.pointer.press_target() {
                    let split = *split;
                    shell.pointer.set_drag(DragKind::Divider { split });
                    shell.drag_divider(split, column, row);
                    PointerUpdate::none(true)
                } else if let Some(surface) = shell.pointer.press_scrollbar() {
                    let grab = shell.scrollbar_grab(surface, shell.pointer.press_origin());
                    shell
                        .pointer
                        .set_drag(DragKind::Scrollbar { surface, grab });
                    self.drag_scrollbar(shell, surface, row, grab);
                    PointerUpdate::none(true)
                } else if let Some(surface) = shell.pointer.press_selectable() {
                    shell.pointer.set_drag(DragKind::Text);
                    if let Some(origin) = shell.pointer.press_origin()
                        && let Some(snapshot) = shell.hit_regions.selectable(surface)
                        && let Some(start) = snapshot.pos_at(origin.0, origin.1)
                    {
                        shell.pointer.selection = Some(TextSelection {
                            surface,
                            start,
                            end: start,
                        });
                    }
                    shell.update_text_drag(column, row);
                    PointerUpdate::none(true)
                } else {
                    shell.pointer.set_drag(DragKind::Cancelled);
                    PointerUpdate::none(hover_changed)
                }
            }
        }
    }

    /// The button coming up: a finished drag, or the click the press armed.
    fn handle_release(&mut self, shell: &mut Shell, column: u16, row: u16) -> PointerUpdate {
        let drag = shell.pointer.drag();
        let target = shell.pointer.press_target().cloned();
        let selection = shell.pointer.selection;
        shell.pointer.clear_press();
        shell.handle_hover(column, row);
        match drag {
            DragKind::Text => {
                if let Some(selection) = selection.filter(|selection| !selection.is_empty())
                    && let Some(snapshot) = shell.hit_regions.selectable(selection.surface)
                {
                    let text = extract_selected_text(snapshot, &selection);
                    if !text.is_empty() {
                        return PointerUpdate::action(AppAction::Copy {
                            text,
                            content: CopiedContent::Text,
                        });
                    }
                }
                PointerUpdate::none(true)
            }
            DragKind::Divider { .. } => {
                shell.session_dirty = true;
                PointerUpdate::none(true)
            }
            DragKind::Scrollbar { .. } | DragKind::Cancelled => PointerUpdate::none(true),
            DragKind::None => {
                let Some(target) = target else {
                    return PointerUpdate::none(true);
                };
                // The chips that switch panes belong to the pane system, not
                // to a screen, so every tab switches the same way.
                if shell.activate_pane_target(&target) {
                    return PointerUpdate::none(true);
                }
                PointerUpdate::action(self.activate_target(shell, target, column, row))
            }
        }
    }

    /// The thumb of one scrollbar, dragged to wherever the pointer has it.
    fn drag_scrollbar(&mut self, shell: &mut Shell, surface: ScrollSurface, row: u16, grab: i16) {
        let Some(metrics) = shell.hit_regions.scroll(surface) else {
            return;
        };
        let Some(thumb) = metrics.thumb() else {
            return;
        };
        let pointer = i32::from(row) - i32::from(grab);
        let track_y = i32::from(metrics.track.y);
        let rel = pointer.saturating_sub(track_y).max(0) as usize;
        let offset = offset_from_thumb(rel.min(thumb.travel), thumb.travel, thumb.max_offset);
        self.scroll_state_mut(surface).scroll_to(offset);
    }

    /// A click on one of the hit regions the screen registered while painting.
    fn activate_target(
        &mut self,
        shell: &mut Shell,
        target: PointerTarget,
        column: u16,
        row: u16,
    ) -> AppAction;

    /// A click inside a text editor, which moves the caret rather than acting.
    fn place_caret(&mut self, shell: &mut Shell, editor: TextEditor, column: u16, row: u16);

    /// `Esc` on whatever is open, or a click away from it.
    fn close_overlay(&mut self, shell: &mut Shell);

    /// The editor the caret is in, if the screen has one open.
    fn active_editor(&self) -> Option<TextEditor>;

    /// Whether a confirmation is up that every key has to answer first: a
    /// digit or a shared overlay does not reach past an armed delete.
    fn modal_open(&self) -> bool {
        false
    }

    /// Where one of the screen's scrolling surfaces has got to.
    fn scroll_state(&self, surface: ScrollSurface) -> ScrollState;

    /// The same surface, to scroll it.
    fn scroll_state_mut(&mut self, surface: ScrollSurface) -> &mut ScrollState;

    /// The columns the Columns overlay draws.
    fn columns(&self) -> &dyn ColumnLayout;

    /// The same, to edit: whichever list this screen is
    /// showing, without the overlay knowing what its columns are.
    fn columns_mut(&mut self) -> &mut dyn ColumnLayout;

    /// Where this screen is standing, as a jump: what `[` comes back to when
    /// something else is followed from here. `None` for a screen with nothing
    /// selected, which is not a place to come back to.
    fn here(&self, _shell: &Shell) -> Option<Jump> {
        None
    }

    /// Where `g` goes from the row under the cursor, and the noun the footer
    /// and the details pane's chip call it. `Err` is what the status line
    /// says instead, which is why this is not an `Option`: a row with nowhere
    /// to go says what it looked for.
    fn follow_target(&self, _shell: &Shell) -> Result<(Jump, &'static str), String> {
        Err("Nothing to go to from here".to_owned())
    }

    /// Settles on whatever a jump points at, and says whether this screen had
    /// it. A screen that answers `false` is not switched to, and the shell
    /// says the target is not on file.
    fn select(&mut self, _shell: &mut Shell, _jump: &Jump) -> bool {
        false
    }

    /// This tab's slice of the session file: what it is showing and how it is
    /// arranged. A screen with nothing worth remembering keeps the default.
    fn snapshot(&self) -> TabSession {
        TabSession::default()
    }

    /// The same, coming back from the file on the next run.
    fn restore(&mut self, _shell: &mut Shell, _session: TabSession) {}

    /// What the tab bar draws after this tab's name, when the tab has
    /// something waiting: `3` pull requests to review, `◐2` runs going.
    fn badge(&self) -> Option<String> {
        None
    }

    /// What the footer says when there is no notification over it.
    fn footer_hint(&self, shell: &Shell) -> &str;

    /// Paint the screen into the area the shell has left it.
    fn render(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect);
}
