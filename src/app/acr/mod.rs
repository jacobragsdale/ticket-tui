//! The ACR screen: the subscription's container registries, and what the
//! details pane says about the one under the cursor.
//!
//! Nothing is read yet. This is the tab's seat — its keys, its columns and its
//! slice of the session — and B3 fills the table in from ARM.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::{AppAction, ListCursor, Screen, Shell, TabId};
use crate::columns::{ColumnId, ColumnLayout, TableLayout};
use crate::command::{CommandId, command_for_key};
use crate::pointer::{PointerTarget, ScrollState, ScrollSurface, TextEditor};
use crate::session::TabSession;
use crate::text_input::TextInput;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AcrMode {
    #[default]
    Browse,
    Search,
}

/// The columns the registry table draws. An ordinary [`ColumnId`], so the
/// table, the header sorting and the Columns overlay come for nothing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AcrColumn {
    #[default]
    Name,
    ResourceGroup,
    Sku,
    Location,
    LoginServer,
}

impl ColumnId for AcrColumn {
    fn all() -> &'static [Self] {
        &[
            Self::Name,
            Self::ResourceGroup,
            Self::Sku,
            Self::Location,
            Self::LoginServer,
        ]
    }

    fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::ResourceGroup => "group",
            Self::Sku => "sku",
            Self::Location => "location",
            Self::LoginServer => "login-server",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Registry",
            Self::ResourceGroup => "Resource group",
            Self::Sku => "SKU",
            Self::Location => "Location",
            Self::LoginServer => "Login server",
        }
    }

    fn default_width(self) -> u16 {
        match self {
            Self::Name => 0,
            Self::ResourceGroup => 20,
            Self::Sku => 8,
            Self::Location => 12,
            Self::LoginServer => 28,
        }
    }

    /// The login server is the registry's own name with a suffix on it, so it
    /// is worth asking for rather than worth the width by default.
    fn default_visible(self) -> bool {
        self != Self::LoginServer
    }

    fn right_aligned(self) -> bool {
        false
    }

    fn pinned(self) -> bool {
        self == Self::Name
    }

    fn flexible(self) -> bool {
        self == Self::Name
    }

    /// A registry name is shorter than a work item's title.
    fn min_flexible_width(self) -> u16 {
        18
    }
}

/// The ACR tab's state: the search box, how the table is arranged, and where
/// the cursor and the details pane have got to. The registries themselves
/// arrive with B3.
#[derive(Default)]
pub struct AcrScreen {
    pub mode: AcrMode,
    query: TextInput,
    pub layout: TableLayout<AcrColumn>,
    pub sort: (AcrColumn, bool),
    pub cursor: ListCursor,
    pub details: ScrollState,
}

impl AcrScreen {
    #[must_use]
    pub fn query(&self) -> &str {
        self.query.text()
    }

    #[must_use]
    pub fn query_cursor(&self) -> usize {
        self.query.cursor()
    }

    /// Sorts by one column, turning the direction around when it is already
    /// the one sorted by, the way every other table does.
    pub fn toggle_sort(&mut self, key: &str) {
        if let Some(column) = AcrColumn::from_key(key) {
            let (current, descending) = self.sort;
            self.sort = (column, if current == column { !descending } else { true });
        }
    }

    /// This tab's slice of the context file. Nothing is read yet, so it says
    /// where the tab stands and nothing more; B3 fills in the registries.
    #[must_use]
    pub fn agent_context(&self) -> crate::agent_context::AcrContext {
        crate::agent_context::AcrContext {
            level: "registries".to_owned(),
            visible_rows: 0,
        }
    }

    /// One command, whether a key, a chip in the details pane, or the palette
    /// asked for it.
    pub fn run_command(&mut self, shell: &mut Shell, id: CommandId) -> AppAction {
        match id {
            CommandId::Search => self.mode = AcrMode::Search,
            // Both verbs want a registry, and there are none until B3 reads
            // them.
            CommandId::Open => shell.set_status("Nothing to open yet"),
            CommandId::Sync => shell.set_status("Nothing to refresh yet"),
            CommandId::HistoryBack => return AppAction::HistoryBack,
            CommandId::HistoryForward => return AppAction::HistoryForward,
            CommandId::Quit => shell.should_quit = true,
            // The panes are the shell's: every tab shows the same two and
            // arranges them the same way.
            CommandId::ToggleDetails => shell.toggle_narrow_details(),
            CommandId::ResetPaneSplit => shell.reset_pane_split(),
            _ => {}
        }
        AppAction::None
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.mode = AcrMode::Browse,
            _ => {
                self.query.handle_key(key);
                self.cursor.reset();
            }
        }
        AppAction::None
    }
}

impl Screen for AcrScreen {
    fn handle_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        if self.mode == AcrMode::Search {
            return self.handle_search_key(key);
        }
        // The table is empty until B3 reads it, so every cursor key counts
        // over no rows and leaves the cursor at the top.
        match key.code {
            KeyCode::Tab => shell.toggle_focus(),
            KeyCode::Down | KeyCode::Char('j') => self.cursor.move_by(1, 0),
            KeyCode::Up | KeyCode::Char('k') => self.cursor.move_by(-1, 0),
            KeyCode::PageDown => self.cursor.page(1, 0),
            KeyCode::PageUp => self.cursor.page(-1, 0),
            KeyCode::Home => self.cursor.focus(0),
            KeyCode::End => self.cursor.move_by(isize::MAX, 0),
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.cursor.reset();
            }
            _ => {
                return command_for_key(key, TabId::Acr)
                    .map_or(AppAction::None, |id| self.run_command(shell, id));
            }
        }
        AppAction::None
    }

    fn handle_paste(&mut self, _shell: &mut Shell, pasted: &str) {
        if self.mode == AcrMode::Search {
            self.query.paste(pasted, true);
        }
    }

    fn activate_target(
        &mut self,
        shell: &mut Shell,
        target: PointerTarget,
        column: u16,
        row: u16,
    ) -> AppAction {
        match target {
            PointerTarget::SortHeader(key) => self.toggle_sort(key),
            // The details pane's chips stand for the keys they name.
            PointerTarget::RunCommand(id) => return self.run_command(shell, id),
            PointerTarget::CloseOverlay | PointerTarget::DismissOverlay => {
                self.close_overlay(shell);
            }
            PointerTarget::SearchField => {
                self.mode = AcrMode::Search;
                self.place_caret(shell, TextEditor::Search, column, row);
            }
            _ => {}
        }
        AppAction::None
    }

    fn place_caret(&mut self, _shell: &mut Shell, editor: TextEditor, column: u16, _row: u16) {
        if editor == TextEditor::Search {
            self.query.set_cursor(usize::from(column));
        }
    }

    fn close_overlay(&mut self, _shell: &mut Shell) {
        self.mode = AcrMode::Browse;
    }

    fn active_editor(&self) -> Option<TextEditor> {
        (self.mode == AcrMode::Search).then_some(TextEditor::Search)
    }

    fn scroll_state(&self, surface: ScrollSurface) -> ScrollState {
        match surface {
            ScrollSurface::Details => self.details,
            _ => self.cursor.scroll,
        }
    }

    fn scroll_state_mut(&mut self, surface: ScrollSurface) -> &mut ScrollState {
        match surface {
            ScrollSurface::Details => &mut self.details,
            _ => &mut self.cursor.scroll,
        }
    }

    fn columns(&self) -> &dyn ColumnLayout {
        &self.layout
    }

    fn columns_mut(&mut self) -> &mut dyn ColumnLayout {
        &mut self.layout
    }

    fn snapshot(&self) -> TabSession {
        TabSession {
            query: self.query.text().to_owned(),
            sort_field: self.sort.0.key().to_owned(),
            columns: self.layout.to_session_columns(),
            ..TabSession::default()
        }
    }

    fn restore(&mut self, _shell: &mut Shell, session: TabSession) {
        self.query = TextInput::new(session.query);
        if let Some(column) = AcrColumn::from_key(&session.sort_field) {
            self.sort = (column, self.sort.1);
        }
        self.layout = TableLayout::from_session_columns(&session.columns);
    }

    fn footer_hint(&self, _shell: &Shell) -> &str {
        match self.mode {
            AcrMode::Search => "←→ cursor  Ctrl-W delete word  Ctrl-U clear  Enter/Esc finish",
            AcrMode::Browse => "↑↓/jk move  / search  r refresh  c columns  ? help",
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
        crate::ui::acr::render(frame, self, shell, area);
    }
}
