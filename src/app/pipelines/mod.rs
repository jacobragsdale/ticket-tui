//! The Pipelines screen: the project's build definitions, the runs of the one
//! chosen, and what the details pane says about a run. The live parts — the
//! timeline, the log tail and the watcher that keeps them moving — arrive with
//! #682 to #684; this is the list the rest of the epic hangs off.

#[cfg(test)]
use crossterm::event::KeyModifiers;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::ListCursor;
use super::{AppAction, Screen, Shell, TabId};
use crate::columns::{ColumnId, ColumnLayout, TableLayout};
use crate::command::{CommandId, command_for_key};
use crate::filter::{MatchContext, ParsedQuery, parse_query};
use crate::model::{Jump, Pipeline, Run};
use crate::pointer::{PointerTarget, ScrollState, ScrollSurface, TextEditor};
use crate::session::TabSession;
use crate::text_input::TextInput;

mod columns;
mod filters;
pub mod rows;
#[cfg(test)]
pub(crate) mod tests;

pub use columns::{PipelineColumn, RunColumn};
pub use filters::{PipelineSchema, RunSchema};
pub use rows::{PipelineRow, RunRow};

/// Which of the two lists is showing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Level {
    /// Every pipeline in the project.
    #[default]
    Pipelines,
    /// The runs of one pipeline, which `Backspace` or `h` goes back up from.
    Runs(i64),
}

/// What the screen is doing, the way the work items screen has its own mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PipelineMode {
    #[default]
    Browse,
    Search,
}

pub struct PipelinesScreen {
    pipelines: Vec<Pipeline>,
    runs: Vec<Run>,
    /// What each pipeline's repository is called, filled in from the shell so
    /// the table and the `repo:` filter read names rather than GUIDs.
    repo_names: Vec<(String, String)>,
    level: Level,
    pub mode: PipelineMode,
    /// One query per level: going down into a pipeline's runs and back up
    /// again puts each list back the way it was left.
    pipeline_query: TextInput,
    run_query: TextInput,
    pub pipelines_layout: TableLayout<PipelineColumn>,
    pub runs_layout: TableLayout<RunColumn>,
    pub pipeline_sort: (PipelineColumn, bool),
    pub run_sort: (RunColumn, bool),
    pub pipeline_cursor: ListCursor,
    pub run_cursor: ListCursor,
    pub details: ScrollState,
}

impl Default for PipelinesScreen {
    fn default() -> Self {
        Self {
            pipelines: Vec::new(),
            runs: Vec::new(),
            repo_names: Vec::new(),
            level: Level::Pipelines,
            mode: PipelineMode::Browse,
            pipeline_query: TextInput::default(),
            run_query: TextInput::default(),
            pipelines_layout: TableLayout::default(),
            runs_layout: TableLayout::default(),
            // Newest first on both lists, which is what a pipeline page opens on.
            pipeline_sort: (PipelineColumn::LastRun, true),
            run_sort: (RunColumn::Run, true),
            pipeline_cursor: ListCursor::default(),
            run_cursor: ListCursor::default(),
            details: ScrollState::default(),
        }
    }
}

impl PipelinesScreen {
    /// What the last pull found. The screen keeps its cursor where it was, by
    /// row, so a pull under a running list does not move it.
    pub fn set_pipelines(&mut self, pipelines: Vec<Pipeline>, runs: Vec<Run>, shell: &Shell) {
        self.repo_names = shell
            .repos()
            .iter()
            .map(|repo| (repo.id.clone(), repo.name.clone()))
            .collect();
        self.pipelines = pipelines;
        self.runs = runs;
        self.clamp_cursors();
    }

    /// Folds in what the watcher has seen. A run already on file is updated
    /// where it stands, so the cursor does not move under the user; one
    /// nobody has seen before joins the list. Nothing here touches SQLite:
    /// the next pull is what persists any of it.
    pub fn merge_live_runs(&mut self, live: Vec<Run>) {
        for run in live {
            if let Some(held) = self.runs.iter_mut().find(|held| held.id == run.id) {
                *held = run;
            } else {
                self.runs.push(run);
            }
        }
        self.runs.sort_by_key(|run| std::cmp::Reverse(run.id));
    }

    /// The runs still going, which is what the watcher is asked to follow.
    #[must_use]
    pub fn live_run_ids(&self) -> Vec<i64> {
        self.runs
            .iter()
            .filter(|run| run.status.is_live())
            .map(|run| run.id)
            .collect()
    }

    #[must_use]
    pub const fn level(&self) -> Level {
        self.level
    }

    /// The pipeline whose runs are showing, if the runs level is.
    #[must_use]
    pub fn open_pipeline(&self) -> Option<&Pipeline> {
        let Level::Runs(id) = self.level else {
            return None;
        };
        self.pipelines.iter().find(|pipeline| pipeline.id == id)
    }

    /// Where the caret is in the search box of whichever level is showing.
    #[must_use]
    pub fn query_cursor(&self) -> usize {
        match self.level {
            Level::Pipelines => self.pipeline_query.cursor(),
            Level::Runs(_) => self.run_query.cursor(),
        }
    }

    /// Sets the query of whichever level is showing, as typing does.
    pub fn set_query(&mut self, query: String) {
        self.query_mut().set_text(query);
        self.cursor_mut().reset();
    }

    #[must_use]
    pub fn query(&self) -> &str {
        match self.level {
            Level::Pipelines => self.pipeline_query.text(),
            Level::Runs(_) => self.run_query.text(),
        }
    }

    fn query_mut(&mut self) -> &mut TextInput {
        match self.level {
            Level::Pipelines => &mut self.pipeline_query,
            Level::Runs(_) => &mut self.run_query,
        }
    }

    /// Every pipeline the query leaves, in the order the table draws them.
    #[must_use]
    pub fn visible_pipelines(&self, shell: &Shell) -> Vec<PipelineRow> {
        let parsed: ParsedQuery<PipelineSchema> = parse_query(self.pipeline_query.text());
        let context = self.match_context(shell);
        let mut rows: Vec<PipelineRow> = self
            .pipelines
            .iter()
            .map(|pipeline| self.row_for(pipeline))
            .filter(|row| {
                parsed.filters.matches_in(row, false, &context) && row.matches_fuzzy(&parsed.fuzzy)
            })
            .collect();
        let (column, descending) = self.pipeline_sort;
        rows.sort_by(|left, right| {
            let ordering = columns::compare_pipelines(left, right, column);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        rows
    }

    /// Every run of the open pipeline the query leaves, newest first.
    #[must_use]
    pub fn visible_runs(&self, shell: &Shell) -> Vec<RunRow> {
        let Level::Runs(pipeline_id) = self.level else {
            return Vec::new();
        };
        let parsed: ParsedQuery<RunSchema> = parse_query(self.run_query.text());
        let context = self.match_context(shell);
        let mut rows: Vec<RunRow> = self
            .runs
            .iter()
            .filter(|run| run.pipeline_id == pipeline_id)
            .map(|run| self.run_row(run))
            .filter(|row| {
                parsed.filters.matches_in(row, false, &context) && row.matches_fuzzy(&parsed.fuzzy)
            })
            .collect();
        let (column, descending) = self.run_sort;
        rows.sort_by(|left, right| {
            let ordering = columns::compare_runs(left, right, column);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        rows
    }

    /// The run the details pane is describing.
    #[must_use]
    pub fn selected_run(&self, shell: &Shell) -> Option<RunRow> {
        match self.level {
            Level::Pipelines => {
                let rows = self.visible_pipelines(shell);
                let row = rows.get(self.pipeline_cursor.index)?;
                row.last_run.as_ref().map(|run| self.run_row(run))
            }
            Level::Runs(_) => self.visible_runs(shell).get(self.run_cursor.index).cloned(),
        }
    }

    fn match_context(&self, shell: &Shell) -> MatchContext {
        MatchContext::now().with_me(shell.me().map(str::to_owned))
    }

    fn row_for(&self, pipeline: &Pipeline) -> PipelineRow {
        let last_run = self
            .runs
            .iter()
            .filter(|run| run.pipeline_id == pipeline.id)
            .max_by_key(|run| run.id)
            .cloned();
        PipelineRow {
            repo: pipeline.repo_id.as_ref().map_or_else(String::new, |id| {
                self.repo_names
                    .iter()
                    .find(|(repo_id, _)| repo_id == id)
                    .map_or_else(|| id.clone(), |(_, name)| name.clone())
            }),
            pipeline: pipeline.clone(),
            last_run,
        }
    }

    fn run_row(&self, run: &Run) -> RunRow {
        RunRow {
            pipeline: self
                .pipelines
                .iter()
                .find(|pipeline| pipeline.id == run.pipeline_id)
                .map_or_else(String::new, |pipeline| pipeline.name.clone()),
            run: run.clone(),
        }
    }

    /// Opens the runs of whatever the cursor is on.
    pub fn open_runs(&mut self, shell: &Shell) {
        let rows = self.visible_pipelines(shell);
        let Some(row) = rows.get(self.pipeline_cursor.index) else {
            return;
        };
        self.level = Level::Runs(row.pipeline.id);
        self.run_cursor.reset();
        self.details.scroll_to(0);
    }

    /// Back up to the pipelines.
    pub fn close_runs(&mut self) {
        self.level = Level::Pipelines;
        self.details.scroll_to(0);
    }

    /// How many rows the level showing has, which is what its cursor clamps to.
    fn row_count(&self, shell: &Shell) -> usize {
        match self.level {
            Level::Pipelines => self.visible_pipelines(shell).len(),
            Level::Runs(_) => self.visible_runs(shell).len(),
        }
    }

    fn clamp_cursors(&mut self) {
        let pipelines = self.pipelines.len();
        self.pipeline_cursor.clamp(pipelines);
    }

    /// The cursor of whichever level is showing.
    pub const fn cursor_mut(&mut self) -> &mut ListCursor {
        match self.level {
            Level::Pipelines => &mut self.pipeline_cursor,
            Level::Runs(_) => &mut self.run_cursor,
        }
    }

    #[must_use]
    pub const fn cursor(&self) -> &ListCursor {
        match self.level {
            Level::Pipelines => &self.pipeline_cursor,
            Level::Runs(_) => &self.run_cursor,
        }
    }

    /// Sorts by one column, turning the direction around when it is already
    /// the one sorted by, the way the work items table does.
    pub fn toggle_sort(&mut self, key: &str) {
        match self.level {
            Level::Pipelines => {
                if let Some(column) = PipelineColumn::from_key(key) {
                    let (current, descending) = self.pipeline_sort;
                    self.pipeline_sort =
                        (column, if current == column { !descending } else { true });
                }
            }
            Level::Runs(_) => {
                if let Some(column) = RunColumn::from_key(key) {
                    let (current, descending) = self.run_sort;
                    self.run_sort = (column, if current == column { !descending } else { true });
                }
            }
        }
    }

    /// What `o` opens: the run under the cursor, or the pipeline itself.
    #[must_use]
    pub fn open_in_browser(&self, shell: &Shell) -> AppAction {
        let url = match self.level {
            Level::Pipelines => {
                let rows = self.visible_pipelines(shell);
                rows.get(self.pipeline_cursor.index)
                    .map(|row| row.pipeline.url.clone())
            }
            Level::Runs(_) => self
                .visible_runs(shell)
                .get(self.run_cursor.index)
                .map(|row| row.run.url.clone()),
        };
        url.filter(|url| !url.is_empty())
            .map_or(AppAction::None, AppAction::OpenUrl)
    }
}

impl Screen for PipelinesScreen {
    fn handle_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        if self.mode == PipelineMode::Search {
            return self.handle_search_key(shell, key);
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                let count = self.row_count(shell);
                self.cursor_mut().move_by(1, count);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let count = self.row_count(shell);
                self.cursor_mut().move_by(-1, count);
            }
            KeyCode::PageDown => {
                let count = self.row_count(shell);
                self.cursor_mut().page(1, count);
            }
            KeyCode::PageUp => {
                let count = self.row_count(shell);
                self.cursor_mut().page(-1, count);
            }
            KeyCode::Home => self.cursor_mut().focus(0),
            KeyCode::End => {
                let count = self.row_count(shell);
                self.cursor_mut().move_by(isize::MAX, count);
            }
            KeyCode::Enter => self.open_runs(shell),
            KeyCode::Backspace | KeyCode::Char('h') => self.close_runs(),
            KeyCode::Esc if !self.query().is_empty() => {
                self.query_mut().clear();
                self.cursor_mut().reset();
            }
            _ => return self.handle_command_key(shell, key),
        }
        AppAction::None
    }

    fn handle_paste(&mut self, _shell: &mut Shell, pasted: &str) {
        if self.mode == PipelineMode::Search {
            self.query_mut().paste(pasted, true);
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
            PointerTarget::TableRow { index } => {
                let count = self.row_count(shell);
                if index < count {
                    self.cursor_mut().focus(index);
                }
            }
            // The id column of a list opens what the row is about in the
            // browser; on the pipelines level that is the run it last had.
            PointerTarget::OpenInBrowser { index } => {
                let count = self.row_count(shell);
                if index < count {
                    self.cursor_mut().focus(index);
                }
                return self.open_in_browser(shell);
            }
            PointerTarget::SortHeader(key) => self.toggle_sort(key),
            PointerTarget::SearchField => {
                self.mode = PipelineMode::Search;
                self.place_caret(shell, TextEditor::Search, column, row);
            }
            _ => {}
        }
        AppAction::None
    }

    fn place_caret(&mut self, _shell: &mut Shell, editor: TextEditor, column: u16, _row: u16) {
        if editor == TextEditor::Search {
            let cursor = usize::from(column);
            self.query_mut().set_cursor(cursor);
        }
    }

    fn close_overlay(&mut self, _shell: &mut Shell) {
        self.mode = PipelineMode::Browse;
    }

    fn active_editor(&self) -> Option<TextEditor> {
        (self.mode == PipelineMode::Search).then_some(TextEditor::Search)
    }

    fn scroll_state(&self, surface: ScrollSurface) -> ScrollState {
        match surface {
            ScrollSurface::Details => self.details,
            _ => self.cursor().scroll,
        }
    }

    fn scroll_state_mut(&mut self, surface: ScrollSurface) -> &mut ScrollState {
        match surface {
            ScrollSurface::Details => &mut self.details,
            _ => &mut self.cursor_mut().scroll,
        }
    }

    fn select(&mut self, shell: &mut Shell, jump: &Jump) -> bool {
        match jump {
            Jump::Pipeline(id) => {
                let Some(index) = self
                    .visible_pipelines(shell)
                    .iter()
                    .position(|row| row.pipeline.id == *id)
                else {
                    return false;
                };
                self.close_runs();
                self.pipeline_cursor.focus(index);
                true
            }
            Jump::Run(id) => {
                let Some(run) = self.runs.iter().find(|run| run.id == *id).cloned() else {
                    return false;
                };
                self.level = Level::Runs(run.pipeline_id);
                let index = self
                    .visible_runs(shell)
                    .iter()
                    .position(|row| row.run.id == *id)
                    .unwrap_or_default();
                self.run_cursor.focus(index);
                true
            }
            _ => false,
        }
    }

    fn columns(&self) -> &dyn ColumnLayout {
        match self.level {
            Level::Pipelines => &self.pipelines_layout,
            Level::Runs(_) => &self.runs_layout,
        }
    }

    fn columns_mut(&mut self) -> &mut dyn ColumnLayout {
        match self.level {
            Level::Pipelines => &mut self.pipelines_layout,
            Level::Runs(_) => &mut self.runs_layout,
        }
    }

    fn badge(&self) -> Option<String> {
        let live = self.runs.iter().filter(|run| run.status.is_live()).count();
        (live > 0).then(|| format!("\u{25d0}{live}"))
    }

    fn snapshot(&self) -> TabSession {
        TabSession {
            query: self.pipeline_query.text().to_owned(),
            sort_field: self.pipeline_sort.0.key().to_owned(),
            columns: self.pipelines_layout.to_session_columns(),
            auto_hide: Some(self.pipelines_layout.auto_hide),
            ..TabSession::default()
        }
    }

    fn restore(&mut self, _shell: &mut Shell, session: TabSession) {
        self.pipeline_query = TextInput::new(session.query);
        if let Some(column) = PipelineColumn::from_key(&session.sort_field) {
            self.pipeline_sort = (column, self.pipeline_sort.1);
        }
        self.pipelines_layout =
            TableLayout::from_session_columns(&session.columns, session.auto_hide);
    }

    fn footer_hint(&self, _shell: &Shell) -> &str {
        match (self.mode, self.level) {
            (PipelineMode::Search, _) => {
                "←→ cursor  Ctrl-W delete word  Ctrl-U clear  Enter/Esc finish"
            }
            (_, Level::Pipelines) => "↑↓/jk move  Enter runs  / search  o open  ? help  q quit",
            (_, Level::Runs(_)) => "↑↓/jk move  Backspace/h back  / search  o open  ? help  q quit",
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
        crate::ui::pipelines::render(frame, self, shell, area);
    }
}

impl PipelinesScreen {
    /// Typing into the search box, which belongs to whichever level is showing.
    fn handle_search_key(&mut self, _shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.mode = PipelineMode::Browse,
            _ => {
                self.query_mut().handle_key(key);
                self.cursor_mut().reset();
            }
        }
        AppAction::None
    }

    /// The global keys a list screen answers: search, open, sync, quit and the
    /// cross-tab history. The rest arrive with the tickets that need them.
    fn handle_command_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match command_for_key(key, TabId::Pipelines) {
            Some(CommandId::Search) => {
                self.mode = PipelineMode::Search;
                AppAction::None
            }
            Some(CommandId::Open) => self.open_in_browser(shell),
            Some(CommandId::Sync) => AppAction::Sync,
            Some(CommandId::HistoryBack) => AppAction::HistoryBack,
            Some(CommandId::HistoryForward) => AppAction::HistoryForward,
            Some(CommandId::Quit) => {
                shell.should_quit = true;
                AppAction::None
            }
            _ => AppAction::None,
        }
    }
}
