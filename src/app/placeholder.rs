//! The screen a tab shows before its own ticket lands: the name of the tab,
//! the issue that fills it in, and nothing else. It keeps the tab bar honest —
//! every tab switches, and the empty ones say why they are empty.

use crossterm::event::KeyEvent;

use crate::command::{CommandId, command_for_key};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

use super::{AppAction, Screen, Shell, TabId};
use crate::columns::ColumnLayout;
use crate::model::Jump;
use crate::pointer::{PointerTarget, ScrollState, ScrollSurface, TextEditor};

pub struct PlaceholderScreen {
    tab: TabId,
    /// The issue that turns this tab into a real one.
    ticket: &'static str,
    /// A surface to answer scroll requests with, so the shell never has to ask
    /// whether a screen is real.
    scroll: ScrollState,
    columns: NoColumns,
}

impl PlaceholderScreen {
    #[must_use]
    pub const fn new(tab: TabId, ticket: &'static str) -> Self {
        Self {
            tab,
            ticket,
            scroll: ScrollState {
                offset: 0,
                content: 0,
                viewport: 0,
            },
            columns: NoColumns,
        }
    }
}

impl Screen for PlaceholderScreen {
    /// A stub has no state to act on, so it answers the global keys that need
    /// none — quit, sync, and the cross-tab history — and nothing else. The
    /// rest arrive with the tab's own ticket.
    fn handle_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match command_for_key(key, self.tab) {
            Some(CommandId::Quit) => {
                shell.should_quit = true;
                AppAction::None
            }
            Some(CommandId::Sync) => AppAction::Sync,
            Some(CommandId::HistoryBack) => AppAction::HistoryBack,
            Some(CommandId::HistoryForward) => AppAction::HistoryForward,
            _ => AppAction::None,
        }
    }

    fn handle_paste(&mut self, _shell: &mut Shell, _pasted: &str) {}

    fn activate_target(
        &mut self,
        _shell: &mut Shell,
        _target: PointerTarget,
        _column: u16,
        _row: u16,
    ) -> AppAction {
        AppAction::None
    }

    fn place_caret(&mut self, _shell: &mut Shell, _editor: TextEditor, _column: u16, _row: u16) {}

    fn close_overlay(&mut self, _shell: &mut Shell) {}

    /// A placeholder has no rows to settle on, but it does hold the tab: a
    /// jump to something it will one day show brings the tab up, and the
    /// screen says which ticket fills it in. The rows arrive with that ticket.
    fn select(&mut self, _shell: &mut Shell, jump: &Jump) -> bool {
        matches!(
            (self.tab, jump),
            (TabId::Repos, Jump::Repo(_))
                | (TabId::PullRequests, Jump::PullRequest { .. })
                | (TabId::Pipelines, Jump::Pipeline(_) | Jump::Run(_))
        )
    }

    fn active_editor(&self) -> Option<TextEditor> {
        None
    }

    fn scroll_state(&self, _surface: ScrollSurface) -> ScrollState {
        self.scroll
    }

    fn scroll_state_mut(&mut self, _surface: ScrollSurface) -> &mut ScrollState {
        &mut self.scroll
    }

    fn columns(&self) -> &dyn ColumnLayout {
        &self.columns
    }

    fn columns_mut(&mut self) -> &mut dyn ColumnLayout {
        &mut self.columns
    }

    fn footer_hint(&self, _shell: &Shell) -> &str {
        "1–4 switch tabs  p palette  ? help  q quit"
    }

    fn render(&mut self, frame: &mut Frame<'_>, _shell: &mut Shell, area: Rect) {
        let lines = vec![
            Line::from(""),
            Line::from(format!("{} — coming in {}", self.tab.label(), self.ticket)),
        ];
        frame.render_widget(
            Paragraph::new(lines)
                .alignment(ratatui::layout::Alignment::Center)
                .block(Block::bordered().title(format!(" {} ", self.tab.label())))
                .style(Style::default()),
            area,
        );
    }
}

/// A layout with nothing in it, so the Columns overlay opens on an empty list
/// rather than on somebody else's columns.
struct NoColumns;

impl ColumnLayout for NoColumns {
    fn count(&self) -> usize {
        0
    }

    fn label(&self, _index: usize) -> &'static str {
        ""
    }

    fn is_visible(&self, _index: usize) -> bool {
        false
    }

    fn width(&self, _index: usize) -> u16 {
        0
    }

    fn flexible(&self, _index: usize) -> bool {
        false
    }

    fn toggle_visible(&mut self, _index: usize) {}

    fn move_column(&mut self, index: usize, _delta: isize) -> usize {
        index
    }

    fn resize(&mut self, _index: usize, _delta: i16) {}
}
