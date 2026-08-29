//! `Screen`: what a tab is. The shell hands one of these every event and every
//! frame, and knows nothing else about it. Today `WorkItemsScreen` is the only
//! implementor; Repos, Pull requests and Pipelines join it behind the tab bar.

use ratatui::Frame;
use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

use super::{AppAction, PointerUpdate, Shell};
use crate::columns::ColumnLayout;
use crate::model::Jump;
use crate::pointer::{PointerTarget, ScrollState, ScrollSurface, TextEditor};
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

    /// One mouse event. The default is the plain reading of it — hover, a
    /// click on whatever region is under the pointer, and the wheel over
    /// whichever surface it is on — which is all a list screen needs. A screen
    /// with text selection and draggable furniture overrides it.
    fn handle_mouse(&mut self, shell: &mut Shell, mouse: MouseEvent) -> PointerUpdate {
        shell.pointer.set_position(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                    -3
                } else {
                    3
                };
                let surface = shell
                    .hit_regions
                    .resolve(mouse.column, mouse.row)
                    .and_then(|region| region.scroll);
                let moved =
                    surface.is_some_and(|surface| self.scroll_state_mut(surface).scroll_by(delta));
                PointerUpdate::none(moved)
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(target) = shell
                    .hit_regions
                    .resolve(mouse.column, mouse.row)
                    .map(|region| region.target.clone())
                else {
                    return PointerUpdate::none(false);
                };
                let action = self.activate_target(shell, target, mouse.column, mouse.row);
                PointerUpdate::action(action)
            }
            _ => PointerUpdate::none(shell.refresh_hover()),
        }
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

    /// Where one of the screen's scrolling surfaces has got to.
    fn scroll_state(&self, surface: ScrollSurface) -> ScrollState;

    /// The same surface, to scroll it.
    fn scroll_state_mut(&mut self, surface: ScrollSurface) -> &mut ScrollState;

    /// The columns the Columns overlay draws.
    fn columns(&self) -> &dyn ColumnLayout;

    /// The same, to edit: whichever list this screen is
    /// showing, without the overlay knowing what its columns are.
    fn columns_mut(&mut self) -> &mut dyn ColumnLayout;

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
