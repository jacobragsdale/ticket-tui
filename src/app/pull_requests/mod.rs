//! The Pull requests screen: what is waiting on you, what you have out, and
//! the decisions worth making without opening a browser. The diff, the threads
//! and the line comments stay in the browser, behind `o`.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::{AppAction, Focus, ListCursor, Screen, Shell, TabId};
use crate::columns::{ColumnLayout, TableLayout};
use crate::command::{CommandId, command_for_key};
use crate::filter::{MatchContext, ParsedQuery, parse_query};
use crate::model::{Jump, PullRequest};
use crate::pointer::{PointerTarget, ScrollState, ScrollSurface, TextEditor};
use crate::session::TabSession;
use crate::text_input::TextInput;

mod columns;
mod filters;
mod rows;
#[cfg(test)]
pub(crate) mod tests;

pub use columns::PrColumn;
pub use filters::{PrField, PrSchema};
pub use rows::PrRow;

/// The views the tab opens with. `@me` is whoever the last sync signed in as,
/// so a saved view follows the person rather than the name they had.
pub const BUILT_IN_VIEWS: &[(&str, &str)] = &[
    ("To review", "reviewer:@me vote:none status:active"),
    ("Mine", "author:@me"),
    ("Active", "status:active"),
    ("Recently closed", "status:completed status:abandoned"),
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PrMode {
    #[default]
    Browse,
    Search,
}

pub struct PullRequestsScreen {
    requests: Vec<PullRequest>,
    /// What each repository is called, so the table and `repo:` read names.
    repo_names: Vec<(String, String)>,
    pub mode: PrMode,
    query: TextInput,
    pub layout: TableLayout<PrColumn>,
    pub sort: (PrColumn, bool),
    pub cursor: ListCursor,
    pub details: ScrollState,
    /// Whether closed pull requests are on the table. Off by default, the way
    /// finished work items are.
    show_closed: bool,
    pub active_view: Option<String>,
    /// How many are waiting on the signed-in user, worked out when the list
    /// is set so the tab bar can ask for it without a shell.
    to_review: usize,
}

impl Default for PullRequestsScreen {
    fn default() -> Self {
        Self {
            requests: Vec::new(),
            repo_names: Vec::new(),
            mode: PrMode::Browse,
            query: TextInput::default(),
            layout: TableLayout::default(),
            // Newest change first, which is what a review queue wants.
            sort: (PrColumn::Age, true),
            cursor: ListCursor::default(),
            details: ScrollState::default(),
            show_closed: false,
            active_view: None,
            to_review: 0,
        }
    }
}

impl PullRequestsScreen {
    /// What the last pull found.
    pub fn set_pull_requests(&mut self, requests: Vec<PullRequest>, shell: &Shell) {
        self.repo_names = shell
            .repos()
            .iter()
            .map(|repo| (repo.id.clone(), repo.name.clone()))
            .collect();
        self.requests = requests;
        self.cursor.clamp(self.requests.len());
        self.to_review = self.to_review(shell);
    }

    #[must_use]
    pub fn query(&self) -> &str {
        self.query.text()
    }

    #[must_use]
    pub fn query_cursor(&self) -> usize {
        self.query.cursor()
    }

    pub fn set_query(&mut self, query: String) {
        self.query.set_text(query);
        self.cursor.reset();
    }

    /// Whether closed pull requests are being left off, which the chip says.
    #[must_use]
    pub const fn closed_hidden(&self) -> bool {
        !self.show_closed
    }

    pub const fn show_closed(&mut self, show: bool) {
        self.show_closed = show;
    }

    /// How many closed pull requests the table is leaving out.
    #[must_use]
    pub fn hidden_closed(&self, shell: &Shell) -> usize {
        if self.show_closed {
            return 0;
        }
        let parsed: ParsedQuery<PrSchema> = parse_query(self.query.text());
        let context = self.match_context(shell);
        self.rows()
            .into_iter()
            .filter(|row| {
                row.request.status.is_closed()
                    && parsed.filters.matches_in(row, false, &context)
                    && row.matches_fuzzy(&parsed.fuzzy)
            })
            .count()
    }

    /// Every pull request the query leaves, in the order the table draws them.
    #[must_use]
    pub fn visible(&self, shell: &Shell) -> Vec<PrRow> {
        let parsed: ParsedQuery<PrSchema> = parse_query(self.query.text());
        let context = self.match_context(shell);
        // A query that names a status of its own is asking for those, whatever
        // the toggle says — the same rule finished work items follow.
        let names_status = self.query.text().contains("status:");
        let mut rows: Vec<PrRow> = self
            .rows()
            .into_iter()
            .filter(|row| {
                (self.show_closed || names_status || !row.request.status.is_closed())
                    && parsed.filters.matches_in(row, false, &context)
                    && row.matches_fuzzy(&parsed.fuzzy)
            })
            .collect();
        let (column, descending) = self.sort;
        rows.sort_by(|left, right| {
            let ordering = columns::compare(left, right, column);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        rows
    }

    /// Every pull request on file, as the table sees them.
    fn rows(&self) -> Vec<PrRow> {
        self.requests
            .iter()
            .map(|request| PrRow {
                repo: self
                    .repo_names
                    .iter()
                    .find(|(id, _)| *id == request.repo_id)
                    .map_or_else(|| request.repo_id.clone(), |(_, name)| name.clone()),
                request: request.clone(),
            })
            .collect()
    }

    /// The pull request under the cursor.
    #[must_use]
    pub fn selected(&self, shell: &Shell) -> Option<PrRow> {
        self.visible(shell).get(self.cursor.index).cloned()
    }

    fn match_context(&self, shell: &Shell) -> MatchContext {
        MatchContext::now().with_me(shell.me().map(str::to_owned))
    }

    /// How many pull requests are waiting on the signed-in user's vote, which
    /// is what the tab badge counts.
    #[must_use]
    pub fn to_review(&self, shell: &Shell) -> usize {
        let Some(me) = shell.me() else {
            return 0;
        };
        self.requests
            .iter()
            .filter(|request| !request.status.is_closed())
            .filter(|request| {
                request.reviewers.iter().any(|reviewer| {
                    crate::model::same_text(&reviewer.display_name, me) && reviewer.vote == 0
                })
            })
            .count()
    }

    /// Loads one of the built-in views, which are queries and nothing more.
    pub fn apply_view(&mut self, name: &str) {
        if let Some((name, query)) = BUILT_IN_VIEWS
            .iter()
            .find(|(view, _)| view.eq_ignore_ascii_case(name))
        {
            self.set_query((*query).to_owned());
            self.active_view = Some((*name).to_owned());
        }
    }

    /// Sorts by one column, turning it around when it is already the one.
    pub fn toggle_sort(&mut self, key: &str) {
        if let Some(column) = <PrColumn as crate::columns::ColumnId>::from_key(key) {
            let (current, descending) = self.sort;
            self.sort = (column, if current == column { !descending } else { true });
        }
    }

    /// What `o` opens: the pull request's own page.
    #[must_use]
    pub fn open_in_browser(&self, shell: &Shell) -> AppAction {
        self.selected(shell)
            .map(|row| row.request.url)
            .filter(|url| !url.is_empty())
            .map_or(AppAction::None, AppAction::OpenUrl)
    }

    /// Where the details pane's references point: the repository, the branches
    /// and the work items the pull request closes.
    #[must_use]
    pub fn jumps(&self, shell: &Shell) -> Vec<(String, Jump)> {
        let Some(row) = self.selected(shell) else {
            return Vec::new();
        };
        let mut jumps = vec![(row.repo.clone(), Jump::Repo(row.repo.clone()))];
        if !row.request.work_items.is_empty() {
            let label = row
                .request
                .work_items
                .iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(" ");
            jumps.push((label, Jump::WorkItems(row.request.work_items.clone())));
        }
        if let Some(run) = row.request.build.as_ref().and_then(|build| build.run_id) {
            jumps.push((format!("Run {run}"), Jump::Run(run)));
        }
        jumps
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.mode = PrMode::Browse,
            _ => {
                self.query.handle_key(key);
                self.cursor.reset();
            }
        }
        AppAction::None
    }

    fn handle_command_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match command_for_key(key, TabId::PullRequests) {
            Some(CommandId::Search) => {
                self.mode = PrMode::Search;
                AppAction::None
            }
            Some(CommandId::Open) => self.open_in_browser(shell),
            Some(CommandId::Sync) => AppAction::Sync,
            Some(CommandId::HistoryBack) => AppAction::HistoryBack,
            Some(CommandId::HistoryForward) => AppAction::HistoryForward,
            Some(CommandId::ToggleFinished) => {
                self.show_closed = !self.show_closed;
                AppAction::None
            }
            Some(CommandId::Quit) => {
                shell.should_quit = true;
                AppAction::None
            }
            _ => AppAction::None,
        }
    }
}

impl Screen for PullRequestsScreen {
    fn handle_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        if self.mode == PrMode::Search {
            return self.handle_search_key(key);
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                let count = self.visible(shell).len();
                self.cursor.move_by(1, count);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let count = self.visible(shell).len();
                self.cursor.move_by(-1, count);
            }
            KeyCode::PageDown => {
                let count = self.visible(shell).len();
                self.cursor.page(1, count);
            }
            KeyCode::PageUp => {
                let count = self.visible(shell).len();
                self.cursor.page(-1, count);
            }
            KeyCode::Home => self.cursor.focus(0),
            KeyCode::End => {
                let count = self.visible(shell).len();
                self.cursor.move_by(isize::MAX, count);
            }
            KeyCode::Tab => shell.focus = Focus::Details,
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.active_view = None;
                self.cursor.reset();
            }
            _ => return self.handle_command_key(shell, key),
        }
        AppAction::None
    }

    fn handle_paste(&mut self, _shell: &mut Shell, pasted: &str) {
        if self.mode == PrMode::Search {
            self.query.paste(pasted, true);
        }
    }

    fn activate_target(
        &mut self,
        shell: &mut Shell,
        target: PointerTarget,
        column: u16,
        _row: u16,
    ) -> AppAction {
        match target {
            PointerTarget::TableRow { index } | PointerTarget::ToggleRowSelect { index } => {
                if index < self.visible(shell).len() {
                    self.cursor.focus(index);
                }
                shell.focus = Focus::Tickets;
            }
            // The id column opens the pull request page, the way it opens a
            // work item on tab 1.
            PointerTarget::OpenInBrowser { index } => {
                if index < self.visible(shell).len() {
                    self.cursor.focus(index);
                }
                return self.open_in_browser(shell);
            }
            PointerTarget::SortHeader(key) => self.toggle_sort(key),
            PointerTarget::Follow(jump) => return AppAction::Follow(jump),
            PointerTarget::ShowFinished => self.show_closed = true,
            PointerTarget::SearchField => {
                self.mode = PrMode::Search;
                self.query.set_cursor(usize::from(column));
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
        self.mode = PrMode::Browse;
    }

    fn active_editor(&self) -> Option<TextEditor> {
        (self.mode == PrMode::Search).then_some(TextEditor::Search)
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

    fn select(&mut self, shell: &mut Shell, jump: &Jump) -> bool {
        let Jump::PullRequest { id, .. } = jump else {
            return false;
        };
        // A closed pull request is worth landing on even while they are hidden.
        if self
            .requests
            .iter()
            .any(|request| request.id == *id && request.status.is_closed())
        {
            self.show_closed = true;
        }
        let Some(index) = self
            .visible(shell)
            .iter()
            .position(|row| row.request.id == *id)
        else {
            return false;
        };
        self.cursor.focus(index);
        true
    }

    fn columns(&self) -> &dyn ColumnLayout {
        &self.layout
    }

    fn columns_mut(&mut self) -> &mut dyn ColumnLayout {
        &mut self.layout
    }

    /// What is waiting on the signed-in user's vote. The count is worked out
    /// when the pull requests are set, since a badge is drawn without a shell
    /// to ask.
    fn badge(&self) -> Option<String> {
        (self.to_review > 0).then(|| self.to_review.to_string())
    }

    fn snapshot(&self) -> TabSession {
        TabSession {
            query: self.query.text().to_owned(),
            sort_field: <PrColumn as crate::columns::ColumnId>::key(self.sort.0).to_owned(),
            columns: self.layout.to_session_columns(),
            auto_hide: Some(self.layout.auto_hide),
            active_view: self.active_view.clone(),
            ..TabSession::default()
        }
    }

    fn restore(&mut self, _shell: &mut Shell, session: TabSession) {
        self.query = TextInput::new(session.query);
        if let Some(column) = <PrColumn as crate::columns::ColumnId>::from_key(&session.sort_field)
        {
            self.sort = (column, self.sort.1);
        }
        self.layout = TableLayout::from_session_columns(&session.columns, session.auto_hide);
        self.active_view = session.active_view;
    }

    fn footer_hint(&self, _shell: &Shell) -> &str {
        match self.mode {
            PrMode::Search => "←→ cursor  Ctrl-W delete word  Ctrl-U clear  Enter/Esc finish",
            PrMode::Browse => "↑↓/jk move  / search  o open  Tab details  ? help  q quit",
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
        crate::ui::pull_requests::render(frame, self, shell, area);
    }
}
