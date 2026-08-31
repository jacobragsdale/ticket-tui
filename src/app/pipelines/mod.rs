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
use super::{AppAction, Focus, Screen, Shell, TabId};
use crate::columns::{ColumnId, ColumnLayout, TableLayout};
use crate::command::{CommandId, command_for_key};
use crate::filter::{MatchContext, ParsedQuery, parse_query};
use crate::model::{Approval, Jump, Pipeline, Run, TimelineRecord};
use crate::pointer::{PointerTarget, ScrollState, ScrollSurface, TextEditor};
use crate::session::TabSession;
use crate::timestamp::Timestamp;
use crate::watch::LogTarget;

/// How long a repository's branches are worth keeping before the picker asks
/// for them again.
const BRANCH_CACHE_SECONDS: i64 = 600;

/// The most lines one log is worth keeping in memory. Past this the oldest go
/// and a line at the top says how many.
pub(crate) const LOG_LINE_CAP: usize = 20_000;
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
    /// The approvals waiting on the project, and what to do about them.
    Approvals,
    /// The word that goes with an approval, before it is sent.
    ApprovalComment,
    /// The branch picker a run is started from.
    BranchPicker,
    /// `Cancel 20260829.4? · x again`, the delete-style confirmation.
    ConfirmCancel,
}

/// The branch picker: what it is for, what it is offering, and what has been
/// typed into its filter.
#[derive(Clone, Debug, Default)]
pub struct BranchPicker {
    /// The pipeline the chosen branch will be run on.
    pub pipeline_id: i64,
    /// The repository whose branches these are.
    pub repo_id: String,
    pub branches: Vec<String>,
    pub query: TextInput,
    pub cursor: ListCursor,
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
    /// The timelines that have come back, newest last, keyed by run.
    timelines: Vec<(i64, Vec<TimelineRecord>)>,
    /// The run the details pane is showing and where its tree cursor is.
    focused: Option<(i64, usize)>,
    /// The lines held per (run, log), newest last.
    logs: Vec<(i64, i64, Vec<String>)>,
    pub log_scroll: ScrollState,
    /// Whether the log pane is keeping the tail in view.
    log_follow: bool,
    /// Whether the log has the whole details pane.
    log_full: bool,
    /// Whether the log on screen has stopped growing.
    log_finished: bool,
    /// The runs being followed whatever tab is showing. They live for the
    /// session only.
    watched: Vec<i64>,
    pub branch_picker: BranchPicker,
    /// The branches each repository answered with, and when, so a picker
    /// opening on a fresh cache asks for nothing.
    branch_cache: Vec<(String, Vec<String>, Timestamp)>,
    /// The run the cancel confirmation is about.
    pub cancelling: Option<i64>,
    /// Every approval the project is waiting on, as the watcher last read
    /// them, and where the overlay's cursor is.
    approvals: Vec<Approval>,
    pub approval_cursor: ListCursor,
    /// The approval being answered, and whether the answer is yes.
    pending_answer: Option<(String, bool)>,
    pub approval_comment: TextInput,
    /// The work items each focused run said it built.
    run_work_items: Vec<(i64, Vec<i64>)>,
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
            timelines: Vec::new(),
            focused: None,
            logs: Vec::new(),
            log_scroll: ScrollState::default(),
            log_follow: true,
            log_full: false,
            log_finished: false,
            watched: Vec::new(),
            branch_picker: BranchPicker::default(),
            branch_cache: Vec::new(),
            cancelling: None,
            approvals: Vec::new(),
            approval_cursor: ListCursor::default(),
            pending_answer: None,
            approval_comment: TextInput::default(),
            run_work_items: Vec::new(),
        }
    }
}

impl PipelinesScreen {
    /// What the last pull found. The cursor stays on the pipeline or run it
    /// was on, wherever that now sorts, so a pull under a running list does
    /// not move the hand. What the watcher had that the pull did not — a run
    /// newer than the pull's window, or one further along than the file
    /// says — is kept.
    pub fn set_pipelines(&mut self, pipelines: Vec<Pipeline>, runs: Vec<Run>, shell: &Shell) {
        let selected_pipeline = self
            .visible_pipelines(shell)
            .get(self.pipeline_cursor.index)
            .map(|row| row.pipeline.id);
        let selected_run = match self.level {
            Level::Runs(_) => self.selected_run(shell).map(|row| row.run.id),
            Level::Pipelines => None,
        };
        self.repo_names = shell
            .repos()
            .iter()
            .map(|repo| (repo.id.clone(), repo.name.clone()))
            .collect();
        self.pipelines = pipelines;
        let mut merged = runs;
        let newest_stored = merged.iter().map(|run| run.id).max().unwrap_or_default();
        for held in std::mem::take(&mut self.runs) {
            match merged.iter_mut().find(|run| run.id == held.id) {
                // The watcher read it later than the pull did: a run that has
                // stopped is not put back in motion by an older read.
                Some(incoming) => {
                    if !held.status.is_live() && incoming.status.is_live() {
                        *incoming = held;
                    }
                }
                None if held.id > newest_stored => merged.push(held),
                None => {}
            }
        }
        merged.sort_by_key(|run| std::cmp::Reverse(run.id));
        self.runs = merged;
        let pipelines = self.visible_pipelines(shell);
        match selected_pipeline
            .and_then(|id| pipelines.iter().position(|row| row.pipeline.id == id))
        {
            Some(index) => self.pipeline_cursor.focus(index),
            None => self.pipeline_cursor.clamp(pipelines.len()),
        }
        let runs = self.visible_runs(shell);
        match selected_run.and_then(|id| runs.iter().position(|row| row.run.id == id)) {
            Some(index) => self.run_cursor.focus(index),
            None => self.run_cursor.clamp(runs.len()),
        }
    }

    /// Folds in what the watcher has seen. A run already on file is updated
    /// where it stands, so the cursor does not move under the user; one
    /// nobody has seen before joins the list. Nothing here touches SQLite:
    /// the next pull is what persists any of it.
    pub fn merge_live_runs(&mut self, live: Vec<Run>, shell: &Shell) {
        let selected = match self.level {
            Level::Runs(_) => self.selected_run(shell).map(|row| row.run.id),
            Level::Pipelines => None,
        };
        for run in live {
            if let Some(held) = self.runs.iter_mut().find(|held| held.id == run.id) {
                *held = run;
            } else {
                self.runs.push(run);
            }
        }
        self.runs.sort_by_key(|run| std::cmp::Reverse(run.id));
        // A run that joined the top of the list must not push the hand onto a
        // different row.
        if let Some(index) = selected.and_then(|id| {
            self.visible_runs(shell)
                .iter()
                .position(|row| row.run.id == id)
        }) {
            self.run_cursor.focus(index);
        }
    }

    /// What the watcher answered with for one run. Kept whole: a timeline is
    /// small, and replacing it is what makes a running node's state move.
    pub fn set_timeline(&mut self, run_id: i64, records: Vec<TimelineRecord>) {
        self.timelines.retain(|(held, _)| *held != run_id);
        self.timelines.push((run_id, records));
        // Only ever a handful: the run on screen, and whatever was on screen
        // before it.
        if self.timelines.len() > 8 {
            self.timelines.remove(0);
        }
        self.clamp_timeline_cursor();
    }

    /// The timeline of the run the details pane is showing, if one has come
    /// back yet.
    #[must_use]
    pub fn timeline(&self, run_id: i64) -> &[TimelineRecord] {
        self.timelines
            .iter()
            .find(|(held, _)| *held == run_id)
            .map_or(&[], |(_, records)| records.as_slice())
    }

    /// The run the details pane is on, which is whose timeline is worth
    /// reading.
    #[must_use]
    pub fn focused_run(&self) -> Option<i64> {
        self.focused.map(|(run, _)| run)
    }

    /// Puts the focus on whatever the cursor is now over, which is what tells
    /// the watcher whose timeline to read.
    pub fn sync_focus(&mut self, shell: &Shell) {
        let run = self.selected_run(shell).map(|row| row.run.id);
        self.focus_run(run);
    }

    /// Records which run the details pane settled on, so the tree cursor
    /// starts at the top of a run that has just come up.
    pub fn focus_run(&mut self, run_id: Option<i64>) {
        if self.focused.map(|(run, _)| run) != run_id {
            self.focused = run_id.map(|run| (run, 0));
        }
    }

    /// The node the timeline cursor is on.
    #[must_use]
    pub fn timeline_cursor(&self) -> usize {
        self.focused.map_or(0, |(_, index)| index)
    }

    /// Moves the timeline cursor, which is what `j` and `k` do while the
    /// details pane has the focus.
    pub fn move_timeline_cursor(&mut self, delta: isize) {
        let Some((run, index)) = self.focused else {
            return;
        };
        let count = self.timeline(run).len();
        if count == 0 {
            return;
        }
        let next = index
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        self.focused = Some((run, next));
    }

    fn clamp_timeline_cursor(&mut self) {
        let Some((run, index)) = self.focused else {
            return;
        };
        let count = self.timeline(run).len();
        self.focused = Some((run, index.min(count.saturating_sub(1))));
    }

    /// This tab's slice of the context file: which level it is on, what is
    /// selected, what the watcher is following, and what is waiting on a
    /// person.
    #[must_use]
    pub fn agent_context(&self, shell: &Shell) -> crate::agent_context::PipelinesContext {
        let pipelines = self.visible_pipelines(shell);
        let selected_pipeline = match self.level {
            Level::Pipelines => pipelines
                .get(self.pipeline_cursor.index)
                .map(|row| row.pipeline.clone()),
            Level::Runs(id) => self
                .pipelines
                .iter()
                .find(|pipeline| pipeline.id == id)
                .cloned(),
        };
        crate::agent_context::PipelinesContext {
            level: match self.level {
                Level::Pipelines => "pipelines",
                Level::Runs(_) => "runs",
            }
            .to_owned(),
            selected_pipeline: selected_pipeline.map(|pipeline| {
                crate::agent_context::PipelineContext {
                    id: pipeline.id,
                    name: pipeline.name.clone(),
                    folder: pipeline.folder.clone(),
                    repo: pipeline.repo_id.as_ref().map(|id| shell.repo_name(id)),
                    web_url: pipeline.url.clone(),
                }
            }),
            selected_run: self
                .selected_run(shell)
                .map(|row| self.run_context(&row.run)),
            following_log: self.following_log_context(),
            running: self.runs.iter().filter(|run| run.status.is_live()).count(),
            watched: self.watched_runs(),
            pending_approvals: self.approvals().len(),
        }
    }

    /// One run as an agent reads it, with its stages — the top level of the
    /// timeline, because the whole tree would be longer than the rest of the
    /// document.
    fn run_context(&self, run: &Run) -> crate::agent_context::RunContext {
        crate::agent_context::RunContext {
            id: run.id,
            pipeline_id: run.pipeline_id,
            build_number: run.build_number.clone(),
            status: run.status.as_str().to_owned(),
            result: run.result.map(|result| result.as_str().to_owned()),
            branch: rows::short_branch(&run.source_branch),
            requested_for: run.requested_for.clone(),
            started_at: run.start_time.map(|at| at.to_rfc3339()),
            finished_at: run.finish_time.map(|at| at.to_rfc3339()),
            web_url: run.url.clone(),
            stages: self
                .timeline(run.id)
                .iter()
                .filter(|record| record.kind == crate::model::TimelineKind::Stage)
                .map(|record| crate::agent_context::StageContext {
                    name: record.name.clone(),
                    state: record.state.as_str().to_owned(),
                    result: record.result.map(|result| result.as_str().to_owned()),
                })
                .collect(),
        }
    }

    /// The log the details pane is tailing, if it is on one.
    fn following_log_context(&self) -> Option<crate::agent_context::FollowingLogContext> {
        let (run_id, _) = self.focused?;
        let target = self.log_target()?;
        Some(crate::agent_context::FollowingLogContext {
            run_id,
            log_id: target.log_id,
            node: self.log_node_name().unwrap_or_default(),
            line_count: self.log(run_id, target.log_id).len(),
            following: self.log_following(),
        })
    }

    /// The node whose log is on screen: the one the tree cursor is on, or the
    /// deepest task still running when nobody has chosen one. This is what the
    /// watcher is asked to follow, and it moves on as tasks finish.
    #[must_use]
    pub fn log_target(&self) -> Option<LogTarget> {
        let (run, cursor) = self.focused?;
        let records = self.timeline(run);
        let chosen = records
            .get(cursor)
            .filter(|record| record.log_id.is_some())
            .or_else(|| {
                records
                    .iter()
                    .rfind(|record| record.log_id.is_some() && record.state.is_live())
            })?;
        let log_id = chosen.log_id?;
        Some(LogTarget {
            log_id,
            from_line: self.log(run, log_id).len(),
            live: chosen.state.is_live(),
        })
    }

    /// What the node whose log is showing is called, for the log's title.
    #[must_use]
    pub fn log_node_name(&self) -> Option<String> {
        let (run, cursor) = self.focused?;
        let records = self.timeline(run);
        records
            .get(cursor)
            .filter(|record| record.log_id.is_some())
            .or_else(|| {
                records
                    .iter()
                    .rfind(|record| record.log_id.is_some() && record.state.is_live())
            })
            .map(|record| record.name.clone())
    }

    /// The lines held for one log.
    #[must_use]
    pub fn log(&self, run_id: i64, log_id: i64) -> &[String] {
        self.logs
            .iter()
            .find(|(held_run, held_log, _)| *held_run == run_id && *held_log == log_id)
            .map_or(&[], |(_, _, lines)| lines.as_slice())
    }

    /// Folds new lines onto the end of a log. A poll that answers from a line
    /// the screen has already passed is dropped rather than duplicated, which
    /// is what makes a retried fetch harmless.
    pub fn append_log(
        &mut self,
        run_id: i64,
        log_id: i64,
        from_line: usize,
        lines: Vec<String>,
        finished: bool,
    ) {
        if lines.is_empty() {
            if finished {
                self.log_finished = true;
            }
            return;
        }
        let held = self
            .logs
            .iter_mut()
            .find(|(held_run, held_log, _)| *held_run == run_id && *held_log == log_id);
        let held = match held {
            Some((_, _, held)) => held,
            None => {
                self.logs.push((run_id, log_id, Vec::new()));
                // Only the log on screen and the one before it are worth
                // keeping; the rest are a fetch away.
                if self.logs.len() > 4 {
                    self.logs.remove(0);
                }
                &mut self.logs.last_mut().expect("just pushed").2
            }
        };
        if from_line > held.len() {
            return;
        }
        held.truncate(from_line);
        held.extend(lines);
        if held.len() > LOG_LINE_CAP {
            // One more than the overflow, because the line saying what went
            // takes a place of its own.
            let skipped = held.len() - LOG_LINE_CAP + 1;
            held.drain(..skipped);
            held.insert(0, format!("\u{2026} {skipped} earlier lines skipped"));
        }
        self.log_finished = finished;
        if self.log_follow {
            let viewport = self.log_scroll.viewport.max(1);
            self.log_scroll.content = held.len();
            self.log_scroll
                .scroll_to(held.len().saturating_sub(viewport));
        }
    }

    /// Whether the log pane is following the tail, which is what `End` puts it
    /// back to and scrolling up takes it out of.
    #[must_use]
    pub const fn log_following(&self) -> bool {
        self.log_follow
    }

    pub const fn follow_log(&mut self, follow: bool) {
        self.log_follow = follow;
    }

    /// Whether the log has the whole pane, which `l` toggles.
    #[must_use]
    pub const fn log_full_pane(&self) -> bool {
        self.log_full
    }

    pub const fn toggle_log_pane(&mut self) {
        self.log_full = !self.log_full;
    }

    /// Turns a watch on one run on or off. A watched run is followed whatever
    /// tab is showing, and says so when it stops.
    pub fn toggle_watch(&mut self, shell: &Shell) -> Option<(i64, bool)> {
        let run = match self.level {
            Level::Runs(_) => self
                .visible_runs(shell)
                .get(self.run_cursor.index)
                .map(|row| row.run.id),
            // On a definition, the run it last had: watching a pipeline means
            // watching what it is doing now.
            Level::Pipelines => self
                .visible_pipelines(shell)
                .get(self.pipeline_cursor.index)
                .and_then(|row| row.last_run.as_ref())
                .map(|run| run.id),
        }?;
        let watching = if self.watched.contains(&run) {
            self.watched.retain(|held| *held != run);
            false
        } else {
            self.watched.push(run);
            true
        };
        Some((run, watching))
    }

    /// Starts watching one run, which is what a run triggered from the TUI
    /// asks for.
    pub fn watch_run(&mut self, run_id: i64) {
        if !self.watched.contains(&run_id) {
            self.watched.push(run_id);
        }
    }

    pub fn unwatch_run(&mut self, run_id: i64) {
        self.watched.retain(|held| *held != run_id);
    }

    /// Whether one run is being followed, which is what the marker column
    /// paints.
    #[must_use]
    pub fn is_watched(&self, run_id: i64) -> bool {
        self.watched.contains(&run_id)
    }

    /// The runs being followed, which the watcher is told about.
    #[must_use]
    pub fn watched_runs(&self) -> Vec<i64> {
        self.watched.clone()
    }

    /// The project's pipelines, for the tabs that name them.
    #[must_use]
    pub fn pipelines(&self) -> &[Pipeline] {
        &self.pipelines
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
        match self.mode {
            PipelineMode::Search => return self.handle_search_key(shell, key),
            PipelineMode::BranchPicker => return self.handle_branch_key(shell, key),
            PipelineMode::ConfirmCancel => return self.handle_cancel_key(shell, key),
            PipelineMode::Approvals => return self.handle_approvals_key(shell, key),
            PipelineMode::ApprovalComment => return self.handle_comment_key(shell, key),
            PipelineMode::Browse => {}
        }
        if shell.focus == Focus::Details {
            match key.code {
                KeyCode::Char('l') => {
                    self.toggle_log_pane();
                    return AppAction::None;
                }
                KeyCode::End => {
                    self.follow_log(true);
                    return AppAction::None;
                }
                KeyCode::Down | KeyCode::Char('j') if self.log_full => {
                    self.log_scroll.scroll_by(1);
                    self.follow_log(false);
                    return AppAction::None;
                }
                KeyCode::Up | KeyCode::Char('k') if self.log_full => {
                    self.log_scroll.scroll_by(-1);
                    self.follow_log(false);
                    return AppAction::None;
                }
                KeyCode::PageUp if self.log_full => {
                    self.log_scroll.scroll_by(-10);
                    self.follow_log(false);
                    return AppAction::None;
                }
                KeyCode::PageDown if self.log_full => {
                    self.log_scroll.scroll_by(10);
                    return AppAction::None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.move_timeline_cursor(1);
                    return AppAction::None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.move_timeline_cursor(-1);
                    return AppAction::None;
                }
                KeyCode::Tab | KeyCode::Esc => {
                    shell.focus_list();
                    return AppAction::None;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Tab => {
                shell.toggle_focus();
                self.sync_focus(shell);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let count = self.row_count(shell);
                self.cursor_mut().move_by(1, count);
                self.sync_focus(shell);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let count = self.row_count(shell);
                self.cursor_mut().move_by(-1, count);
                self.sync_focus(shell);
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
            // Going down into a pipeline's runs; on the runs already, Enter
            // has nowhere further to go and leaves the cursor alone.
            KeyCode::Enter if self.level == Level::Pipelines => {
                self.open_runs(shell);
                self.sync_focus(shell);
            }
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
                    self.sync_focus(shell);
                }
                shell.focus = Focus::Tickets;
            }
            // A click in the timeline tree picks the node up, which is what
            // #684 hangs the log off.
            PointerTarget::TreeRow { index } => {
                shell.focus = Focus::Details;
                if let Some((run, _)) = self.focused {
                    let count = self.timeline(run).len();
                    if index < count {
                        self.focused = Some((run, index));
                    }
                }
            }
            // The gutter of a row: its left half moves the cursor, and the
            // marker itself is the watch, which is what it paints.
            PointerTarget::ToggleRowSelect { index } => {
                let count = self.row_count(shell);
                if index < count {
                    self.cursor_mut().focus(index);
                    self.sync_focus(shell);
                }
            }
            PointerTarget::ToggleBookmark { index } => {
                let count = self.row_count(shell);
                if index < count {
                    self.cursor_mut().focus(index);
                    self.sync_focus(shell);
                    self.toggle_watch(shell);
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
            // A reference in the details pane is the shell's to follow.
            PointerTarget::Follow(jump) => return AppAction::Follow(jump),
            // The details pane's buttons stand for the keys they name.
            PointerTarget::RunCommand(id) => return self.run_command(shell, id),
            PointerTarget::ApprovalRow { index } => {
                self.approval_cursor.focus(index);
            }
            PointerTarget::NodeOption { index } => {
                self.branch_picker.cursor.focus(index);
                return self.choose_branch(shell);
            }
            PointerTarget::NodeQuery => {
                self.branch_picker.query.set_cursor(usize::from(column));
            }
            PointerTarget::CloseOverlay | PointerTarget::DismissOverlay => {
                self.close_overlay(shell);
            }
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
        self.cancelling = None;
    }

    fn active_editor(&self) -> Option<TextEditor> {
        match self.mode {
            PipelineMode::Search => Some(TextEditor::Search),
            PipelineMode::BranchPicker => Some(TextEditor::Node),
            PipelineMode::ApprovalComment => Some(TextEditor::Prompt),
            _ => None,
        }
    }

    fn scroll_state(&self, surface: ScrollSurface) -> ScrollState {
        match surface {
            ScrollSurface::Details => self.log_scroll,
            _ => self.cursor().scroll,
        }
    }

    /// Scrolling the log by hand is what takes it out of follow mode, wherever
    /// the scroll came from — a key, the wheel, or the scrollbar thumb.
    fn scroll_state_mut(&mut self, surface: ScrollSurface) -> &mut ScrollState {
        match surface {
            ScrollSurface::Details => {
                self.log_follow = false;
                &mut self.log_scroll
            }
            _ => &mut self.cursor_mut().scroll,
        }
    }

    /// A run when the tab is showing one, the pipeline itself otherwise.
    fn here(&self, shell: &Shell) -> Option<Jump> {
        match self.level {
            Level::Runs(_) => self.selected_run(shell).map(|row| Jump::Run(row.run.id)),
            Level::Pipelines => self
                .visible_pipelines(shell)
                .get(self.pipeline_cursor.index)
                .map(|row| Jump::Pipeline(row.pipeline.id)),
        }
    }

    fn select(&mut self, shell: &mut Shell, jump: &Jump) -> bool {
        match jump {
            Jump::Pipeline(id) => {
                if !self.pipelines.iter().any(|pipeline| pipeline.id == *id) {
                    return false;
                }
                let position = |screen: &Self| {
                    screen
                        .visible_pipelines(shell)
                        .iter()
                        .position(|row| row.pipeline.id == *id)
                };
                let index = match position(self) {
                    Some(index) => index,
                    // On file but filtered out: the reference wins over the
                    // query, which is cleared rather than reported as a
                    // missing row.
                    None => {
                        self.pipeline_query.clear();
                        match position(self) {
                            Some(index) => index,
                            None => return false,
                        }
                    }
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
                let position = |screen: &Self| {
                    screen
                        .visible_runs(shell)
                        .iter()
                        .position(|row| row.run.id == *id)
                };
                let index = match position(self) {
                    Some(index) => index,
                    None => {
                        self.run_query.clear();
                        match position(self) {
                            Some(index) => index,
                            None => return false,
                        }
                    }
                };
                self.run_cursor.focus(index);
                self.focus_run(Some(*id));
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

    /// `◐2` while runs are going, `◇1` while an approval waits, both when
    /// both.
    fn badge(&self) -> Option<String> {
        let live = self.runs.iter().filter(|run| run.status.is_live()).count();
        let waiting = self.approvals.len();
        let mut badge = String::new();
        if live > 0 {
            badge.push_str(&format!("\u{25d0}{live}"));
        }
        if waiting > 0 {
            if !badge.is_empty() {
                badge.push(' ');
            }
            badge.push_str(&format!("\u{25c7}{waiting}"));
        }
        (!badge.is_empty()).then_some(badge)
    }

    fn snapshot(&self) -> TabSession {
        TabSession {
            query: self.pipeline_query.text().to_owned(),
            sort_field: self.pipeline_sort.0.key().to_owned(),
            columns: self.pipelines_layout.to_session_columns(),
            ..TabSession::default()
        }
    }

    fn restore(&mut self, _shell: &mut Shell, session: TabSession) {
        self.pipeline_query = TextInput::new(session.query);
        if let Some(column) = PipelineColumn::from_key(&session.sort_field) {
            self.pipeline_sort = (column, self.pipeline_sort.1);
        }
        self.pipelines_layout = TableLayout::from_session_columns(&session.columns);
    }

    fn footer_hint(&self, _shell: &Shell) -> &str {
        match (self.mode, self.level) {
            (PipelineMode::Search, _) => {
                "←→ cursor  Ctrl-W delete word  Ctrl-U clear  Enter/Esc finish"
            }
            (PipelineMode::BranchPicker, _) => {
                "Type to filter  ↑↓ choose  Enter run it  Esc cancel"
            }
            (PipelineMode::ConfirmCancel, _) => "x cancel the run  Esc leave it",
            (PipelineMode::Approvals, _) => "↑↓ choose  a approve  x reject  Esc close",
            (PipelineMode::ApprovalComment, _) => "Type a word  Enter send  Esc back",
            (_, Level::Pipelines) => {
                "↑↓/jk move  Enter runs  t run  W watch  A approvals  / search  o open  ? help"
            }
            (_, Level::Runs(_)) => {
                "↑↓/jk move  h back  x cancel  R retry  W watch  A approvals  o open  ? help"
            }
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

    /// The branch picker: type to filter, arrows to move, `Enter` to start.
    fn handle_branch_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.mode = PipelineMode::Browse,
            KeyCode::Enter => return self.choose_branch(shell),
            KeyCode::Down => {
                let count = self.branch_matches().len();
                self.branch_picker.cursor.move_by(1, count);
            }
            KeyCode::Up => {
                let count = self.branch_matches().len();
                self.branch_picker.cursor.move_by(-1, count);
            }
            _ => {
                self.branch_picker.query.handle_key(key);
                self.branch_picker.cursor.reset();
            }
        }
        AppAction::None
    }

    /// `x` again confirms the cancel; anything else calls it off.
    fn handle_cancel_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        let run = self.cancelling;
        self.mode = PipelineMode::Browse;
        self.cancelling = None;
        if key.code == KeyCode::Char('x')
            && let Some(run_id) = run
        {
            shell.set_status("Cancelling\u{2026}");
            return AppAction::RunAction {
                run_id,
                retry: false,
            };
        }
        AppAction::None
    }

    /// The approvals overlay: `a` approves, `x` rejects, both through the
    /// comment prompt.
    fn handle_approvals_key(&mut self, _shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('A') => self.mode = PipelineMode::Browse,
            KeyCode::Down | KeyCode::Char('j') => {
                let count = self.approvals.len();
                self.approval_cursor.move_by(1, count);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let count = self.approvals.len();
                self.approval_cursor.move_by(-1, count);
            }
            KeyCode::Char('a') => self.begin_answer(true),
            KeyCode::Char('x') => self.begin_answer(false),
            _ => {}
        }
        AppAction::None
    }

    /// The word that goes with an answer. Empty is fine: not every approval
    /// needs a reason.
    fn handle_comment_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => {
                self.pending_answer = None;
                self.mode = PipelineMode::Approvals;
            }
            KeyCode::Enter => return self.send_answer(shell),
            _ => {
                self.approval_comment.handle_key(key);
            }
        }
        AppAction::None
    }

    /// The global keys a list screen answers: search, open, sync, quit and the
    /// cross-tab history. The rest arrive with the tickets that need them.
    fn handle_command_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        command_for_key(key, TabId::Pipelines)
            .map_or(AppAction::None, |id| self.run_command(shell, id))
    }

    /// One command, whether a key, a button in the details pane, or the
    /// palette asked for it.
    pub fn run_command(&mut self, shell: &mut Shell, id: CommandId) -> AppAction {
        match id {
            CommandId::Search => self.mode = PipelineMode::Search,
            CommandId::Open => return self.open_in_browser(shell),
            CommandId::Sync => return AppAction::Sync,
            CommandId::HistoryBack => return AppAction::HistoryBack,
            CommandId::HistoryForward => return AppAction::HistoryForward,
            CommandId::RunPipeline => return self.open_branch_picker(shell),
            CommandId::Approvals => return self.open_approvals(),
            CommandId::CancelRun => self.confirm_cancel(shell),
            CommandId::RetryRun => return self.retry_run(shell),
            CommandId::WatchRun => self.watch_selected(shell),
            CommandId::Quit => shell.should_quit = true,
            // The panes are the shell's: every tab shows the same two and
            // arranges them the same way.
            CommandId::ToggleDetails => shell.toggle_narrow_details(),
            CommandId::ResetPaneSplit => shell.reset_pane_split(),
            _ => {}
        }
        AppAction::None
    }

    /// `W`: follows the run under the cursor, or stops following it. A run
    /// that has already stopped is refused rather than announced as finished
    /// the moment it is watched.
    fn watch_selected(&mut self, shell: &mut Shell) {
        let Some(row) = self.selected_run(shell) else {
            shell.set_error("No run to watch here");
            return;
        };
        if !row.run.status.is_live() && !self.is_watched(row.run.id) {
            shell.set_error(format!("{} has already finished", row.run.build_number));
            return;
        }
        if let Some((run, watching)) = self.toggle_watch(shell) {
            let word = if watching {
                "Watching"
            } else {
                "Stopped watching"
            };
            shell.set_status(format!("{word} run {run}"));
        }
    }
}

impl PipelinesScreen {
    /// Opens the branch picker over the pipeline under the cursor. It opens at
    /// once on whatever is cached — the default branch at the very least — and
    /// fills in when the worker answers.
    pub fn open_branch_picker(&mut self, shell: &mut Shell) -> AppAction {
        let Some(row) = self
            .visible_pipelines(shell)
            .get(self.pipeline_cursor.index)
            .cloned()
        else {
            shell.set_error("No pipeline to run here");
            return AppAction::None;
        };
        let Some(repo_id) = row.pipeline.repo_id.clone() else {
            shell.set_error(format!(
                "{} names no repository to pick a branch from",
                row.pipeline.name
            ));
            return AppAction::None;
        };
        let default = row
            .pipeline
            .default_branch
            .as_deref()
            .map(rows::short_branch)
            .unwrap_or_default();
        let (cached, fresh) = self.cached_branches(&repo_id);
        let mut branches = cached;
        if branches.is_empty() && !default.is_empty() {
            branches.push(default);
        }
        self.branch_picker = BranchPicker {
            pipeline_id: row.pipeline.id,
            repo_id: repo_id.clone(),
            branches,
            query: TextInput::default(),
            cursor: ListCursor::default(),
        };
        self.mode = PipelineMode::BranchPicker;
        if fresh {
            AppAction::None
        } else {
            AppAction::FetchBranches(repo_id)
        }
    }

    /// What the picker is showing: the branches the filter leaves.
    #[must_use]
    pub fn branch_matches(&self) -> Vec<String> {
        let needle = self.branch_picker.query.text().trim().to_lowercase();
        self.branch_picker
            .branches
            .iter()
            .filter(|branch| needle.is_empty() || branch.to_lowercase().contains(&needle))
            .cloned()
            .collect()
    }

    /// The branches held for one repository, and whether they are fresh enough
    /// to open on without asking again.
    fn cached_branches(&self, repo_id: &str) -> (Vec<String>, bool) {
        self.branch_cache
            .iter()
            .find(|(held, _, _)| held == repo_id)
            .map_or_else(
                || (Vec::new(), false),
                |(_, branches, at)| {
                    let fresh = at.seconds_until(Timestamp::now()) < BRANCH_CACHE_SECONDS;
                    (branches.clone(), fresh)
                },
            )
    }

    /// What the worker answered with, which fills an open picker without
    /// moving its cursor.
    pub fn set_branches(&mut self, repo_id: &str, branches: Vec<String>) {
        self.branch_cache.retain(|(held, _, _)| held != repo_id);
        self.branch_cache
            .push((repo_id.to_owned(), branches.clone(), Timestamp::now()));
        if self.branch_picker.repo_id == repo_id {
            self.branch_picker.branches = branches;
        }
    }

    /// Starts the pipeline the picker was opened over, on the branch chosen.
    pub fn choose_branch(&mut self, shell: &mut Shell) -> AppAction {
        let Some(branch) = self
            .branch_matches()
            .get(self.branch_picker.cursor.index)
            .cloned()
        else {
            self.mode = PipelineMode::Browse;
            return AppAction::None;
        };
        self.mode = PipelineMode::Browse;
        shell.set_status(format!(
            "Starting {} on {branch}\u{2026}",
            self.pipelines
                .iter()
                .find(|pipeline| pipeline.id == self.branch_picker.pipeline_id)
                .map_or_else(
                    || "the pipeline".to_owned(),
                    |pipeline| pipeline.name.clone()
                )
        ));
        AppAction::TriggerRun {
            pipeline_id: self.branch_picker.pipeline_id,
            branch,
        }
    }

    /// Takes the run Azure DevOps started: it goes to the top of the list, is
    /// selected, focused and watched, so its timeline and log start at once.
    pub fn accept_run(&mut self, shell: &mut Shell, run: Run) {
        let id = run.id;
        let number = run.build_number.clone();
        let pipeline = run.pipeline_id;
        self.merge_live_runs(vec![run], shell);
        self.level = Level::Runs(pipeline);
        self.run_cursor.reset();
        self.watch_run(id);
        self.focus_run(Some(id));
        shell.set_status(format!("Started {number}"));
    }

    /// `x` on a run that is going: the confirmation, then the cancel.
    pub fn confirm_cancel(&mut self, shell: &mut Shell) {
        let Some(row) = self.selected_run(shell) else {
            shell.set_error("No run to cancel here");
            return;
        };
        if !row.run.status.is_live() {
            shell.set_error(format!("{} has already finished", row.run.build_number));
            return;
        }
        self.cancelling = Some(row.run.id);
        self.mode = PipelineMode::ConfirmCancel;
    }

    /// `R` on a failed or canceled run: retry the jobs that failed.
    pub fn retry_run(&mut self, shell: &mut Shell) -> AppAction {
        let Some(row) = self.selected_run(shell) else {
            shell.set_error("No run to retry here");
            return AppAction::None;
        };
        if row.run.status.is_live() {
            shell.set_error(format!("{} is still going", row.run.build_number));
            return AppAction::None;
        }
        if matches!(row.run.result, Some(crate::model::RunResult::Succeeded)) {
            shell.set_error(format!(
                "{} succeeded \u{2014} nothing to retry; t runs it again",
                row.run.build_number
            ));
            return AppAction::None;
        }
        shell.set_status(format!("Retrying {}\u{2026}", row.run.build_number));
        AppAction::RunAction {
            run_id: row.run.id,
            retry: true,
        }
    }

    /// What the cancel confirmation is about, for the overlay to name.
    #[must_use]
    pub fn cancelling_run(&self) -> Option<RunRow> {
        let id = self.cancelling?;
        self.runs
            .iter()
            .find(|run| run.id == id)
            .map(|run| self.run_row(run))
    }
}

impl PipelinesScreen {
    /// The work items one run says it built, as the watcher read them.
    pub fn set_run_work_items(&mut self, run_id: i64, work_items: Vec<i64>) {
        self.run_work_items.retain(|(held, _)| *held != run_id);
        self.run_work_items.push((run_id, work_items));
        if self.run_work_items.len() > 8 {
            self.run_work_items.remove(0);
        }
    }

    #[must_use]
    pub fn run_work_items(&self, run_id: i64) -> &[i64] {
        self.run_work_items
            .iter()
            .find(|(held, _)| *held == run_id)
            .map_or(&[], |(_, ids)| ids.as_slice())
    }

    /// Where the details pane's references point: the repository the run
    /// built, the pull request it was raised for, and the work items it says
    /// it carried.
    #[must_use]
    pub fn run_jumps(&self, shell: &Shell) -> Vec<(String, Jump)> {
        let Some(row) = self.selected_run(shell) else {
            return Vec::new();
        };
        let mut jumps = Vec::new();
        if let Some(pipeline) = self
            .pipelines
            .iter()
            .find(|pipeline| pipeline.id == row.run.pipeline_id)
            && let Some(repo_id) = pipeline.repo_id.as_deref()
        {
            jumps.push((
                shell.repo_name(repo_id),
                Jump::Repo(shell.repo_name(repo_id)),
            ));
        }
        if let Some(pr) = row.run.pr_id {
            let repo = self
                .pipelines
                .iter()
                .find(|pipeline| pipeline.id == row.run.pipeline_id)
                .and_then(|pipeline| pipeline.repo_id.as_deref())
                .map_or_else(String::new, |id| shell.repo_name(id));
            jumps.push((format!("!{pr}"), Jump::PullRequest { repo, id: pr }));
        }
        let work_items = self.run_work_items(row.run.id);
        if !work_items.is_empty() {
            let label = work_items
                .iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(" ");
            jumps.push((label, Jump::WorkItems(work_items.to_vec())));
        }
        jumps
    }

    /// What the watcher last read. The cursor holds its place, so an overlay
    /// open while the list is refreshed does not jump under the hand.
    pub fn set_approvals(&mut self, approvals: Vec<Approval>) {
        self.approvals = approvals;
        self.approval_cursor.clamp(self.approvals.len());
    }

    #[must_use]
    pub fn approvals(&self) -> &[Approval] {
        &self.approvals
    }

    /// `A`: the approvals overlay, which asks for a fresh read on the way up.
    pub fn open_approvals(&mut self) -> AppAction {
        self.mode = PipelineMode::Approvals;
        self.approval_cursor.clamp(self.approvals.len());
        AppAction::RefreshApprovals
    }

    /// The approval the cursor is on, or the one gating the stage the timeline
    /// cursor is on, which is what `a` in the tree answers.
    #[must_use]
    pub fn selected_approval(&self) -> Option<&Approval> {
        self.approvals.get(self.approval_cursor.index)
    }

    /// The approval gating the run on screen, if one is.
    #[must_use]
    pub fn approval_for_focused_run(&self) -> Option<&Approval> {
        let run = self.focused_run()?;
        self.approvals
            .iter()
            .find(|approval| approval.run_id == Some(run))
    }

    /// Starts answering one approval: the comment prompt opens over it, and
    /// `Enter` sends whatever is in it, empty or not.
    pub fn begin_answer(&mut self, approve: bool) {
        let Some(id) = self.selected_approval().map(|approval| approval.id.clone()) else {
            return;
        };
        self.pending_answer = Some((id, approve));
        self.approval_comment = TextInput::default();
        self.mode = PipelineMode::ApprovalComment;
    }

    /// Whether the prompt open is an approval or a rejection, for its title.
    #[must_use]
    pub fn answering_approval(&self) -> Option<bool> {
        self.pending_answer.as_ref().map(|(_, approve)| *approve)
    }

    /// Sends the answer.
    pub fn send_answer(&mut self, shell: &mut Shell) -> AppAction {
        let Some((id, approve)) = self.pending_answer.take() else {
            self.mode = PipelineMode::Browse;
            return AppAction::None;
        };
        let comment = self.approval_comment.text().trim().to_owned();
        self.mode = PipelineMode::Approvals;
        shell.set_status(if approve {
            "Approving…"
        } else {
            "Rejecting…"
        });
        AppAction::AnswerApproval {
            id,
            approve,
            comment,
        }
    }

    /// Takes an approval off the list once Azure DevOps has taken the answer.
    pub fn approval_answered(&mut self, id: &str) {
        self.approvals.retain(|approval| approval.id != id);
        self.approval_cursor.clamp(self.approvals.len());
    }
}
