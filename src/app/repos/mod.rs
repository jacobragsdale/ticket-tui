//! The Repos screen: the project's repositories, what is open against them,
//! and which of them are checked out on this machine. Deliberately not a git
//! client — detect, clone, status, fetch and pull, then `o` for the rest.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::{AppAction, CopiedContent, Focus, ListCursor, Screen, Shell, TabId};
use crate::columns::{ColumnLayout, TableLayout};
use crate::command::{CommandId, command_for_key};
use crate::filter::{MatchContext, ParsedQuery, parse_query};
use crate::local::LocalRequest;
use crate::model::{GitJob, Jump, LocalRepo, Repo};
use crate::pointer::{PointerTarget, ScrollState, ScrollSurface, TextEditor};
use crate::session::TabSession;
use crate::text_input::TextInput;

mod columns;
mod filters;
mod rows;
#[cfg(test)]
pub(crate) mod tests;

pub use columns::RepoColumn;
pub use filters::{RepoField, RepoSchema};
pub use rows::RepoRow;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RepoMode {
    #[default]
    Browse,
    Search,
}

pub struct ReposScreen {
    repos: Vec<Repo>,
    /// What the local-repos thread last found on this machine, by repository.
    local: Vec<(String, LocalRepo)>,
    /// How many active pull requests and pipelines each repository has.
    pull_request_counts: Vec<(String, usize)>,
    pipeline_counts: Vec<(String, usize)>,
    /// The pull requests and pipelines the details pane links to.
    pull_requests: Vec<(String, i64, String)>,
    pipelines: Vec<(String, i64, String)>,
    pub mode: RepoMode,
    query: TextInput,
    pub layout: TableLayout<RepoColumn>,
    pub sort: (RepoColumn, bool),
    pub cursor: ListCursor,
    pub details: ScrollState,
    /// What git is doing to a repository right now, by repository id. Held
    /// apart from `local` because a clone has no local entry to hang it on.
    jobs: Vec<(String, GitJob)>,
}

impl Default for ReposScreen {
    fn default() -> Self {
        Self {
            repos: Vec::new(),
            local: Vec::new(),
            pull_request_counts: Vec::new(),
            pipeline_counts: Vec::new(),
            pull_requests: Vec::new(),
            pipelines: Vec::new(),
            mode: RepoMode::Browse,
            query: TextInput::default(),
            layout: TableLayout::default(),
            sort: (RepoColumn::Name, false),
            cursor: ListCursor::default(),
            details: ScrollState::default(),
            jobs: Vec::new(),
        }
    }
}

impl ReposScreen {
    /// What the last pull found, and what the other tabs have against each.
    pub fn set_repos(&mut self, shell: &Shell) {
        self.repos = shell.repos().to_vec();
        self.cursor.clamp(self.repos.len());
    }

    /// The pull requests and pipelines each repository has, from the snapshot
    /// the other tabs are drawing.
    pub fn set_related(
        &mut self,
        pull_requests: Vec<(String, i64, String)>,
        pipelines: Vec<(String, i64, String)>,
    ) {
        self.pull_request_counts = counts(&pull_requests);
        self.pipeline_counts = counts(&pipelines);
        self.pull_requests = pull_requests;
        self.pipelines = pipelines;
    }

    /// What git has started or finished doing to one repository. The busy
    /// state outlives the command by a moment: it is cleared by the rescan
    /// that follows, so the row never blinks back to a status that is stale.
    pub fn set_job(&mut self, repo_id: &str, job: Option<GitJob>) {
        self.jobs.retain(|(held, _)| held != repo_id);
        if let Some(job) = job {
            self.jobs.push((repo_id.to_owned(), job));
        }
    }

    /// What git is doing to one repository, if anything.
    #[must_use]
    pub fn job_for(&self, repo_id: &str) -> Option<GitJob> {
        self.jobs
            .iter()
            .find(|(held, _)| held == repo_id)
            .map(|(_, job)| *job)
    }

    /// What the local-repos thread found. Keyed by repository id.
    pub fn set_local(&mut self, local: Vec<(String, LocalRepo)>) {
        self.local = local;
    }

    /// The same, with whatever git is doing to it folded in. A clone has no
    /// local entry yet, so it borrows one whose only content is the job.
    fn local_with_job(&self, repo_id: &str) -> Option<LocalRepo> {
        let job = self.job_for(repo_id);
        match self.local_for(repo_id) {
            Some(local) => Some(LocalRepo {
                busy: job,
                ..local.clone()
            }),
            None => job.map(|job| LocalRepo {
                busy: Some(job),
                ..LocalRepo::default()
            }),
        }
    }

    /// What one repository looks like on this machine, if it is here at all.
    #[must_use]
    pub fn local_for(&self, repo_id: &str) -> Option<&LocalRepo> {
        self.local
            .iter()
            .find(|(held, _)| held == repo_id)
            .map(|(_, local)| local)
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

    /// Every repository the query leaves, in the order the table draws them.
    #[must_use]
    pub fn visible(&self, shell: &Shell) -> Vec<RepoRow> {
        let parsed: ParsedQuery<RepoSchema> = parse_query(self.query.text());
        let context = MatchContext::now().with_me(shell.me().map(str::to_owned));
        let mut rows: Vec<RepoRow> = self
            .rows()
            .into_iter()
            .filter(|row| {
                parsed.filters.matches_in(row, false, &context) && row.matches_fuzzy(&parsed.fuzzy)
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

    fn rows(&self) -> Vec<RepoRow> {
        self.repos
            .iter()
            .map(|repo| RepoRow {
                local: self.local_with_job(&repo.id),
                pull_requests: count_for(&self.pull_request_counts, &repo.id),
                pipelines: count_for(&self.pipeline_counts, &repo.id),
                repo: repo.clone(),
            })
            .collect()
    }

    #[must_use]
    pub fn selected(&self, shell: &Shell) -> Option<RepoRow> {
        self.visible(shell).get(self.cursor.index).cloned()
    }

    /// Where the details pane's references point: the repository's active pull
    /// requests and the pipelines that build it.
    #[must_use]
    pub fn jumps(&self, shell: &Shell) -> Vec<(String, Jump)> {
        let Some(row) = self.selected(shell) else {
            return Vec::new();
        };
        let mut jumps = Vec::new();
        for (repo_id, id, title) in &self.pull_requests {
            if *repo_id == row.repo.id {
                jumps.push((
                    format!("!{id}  {title}"),
                    Jump::PullRequest {
                        repo: row.repo.name.clone(),
                        id: *id,
                    },
                ));
            }
        }
        for (repo_id, id, name) in &self.pipelines {
            if *repo_id == row.repo.id {
                jumps.push((name.clone(), Jump::Pipeline(*id)));
            }
        }
        jumps
    }

    pub fn toggle_sort(&mut self, key: &str) {
        if let Some(column) = <RepoColumn as crate::columns::ColumnId>::from_key(key) {
            let (current, descending) = self.sort;
            self.sort = (column, if current == column { !descending } else { true });
        }
    }

    /// What `o` opens: the repository's own page.
    #[must_use]
    pub fn open_in_browser(&self, shell: &Shell) -> AppAction {
        self.selected(shell)
            .map(|row| row.repo.web_url)
            .filter(|url| !url.is_empty())
            .map_or(AppAction::None, AppAction::OpenUrl)
    }

    /// `y`: the ssh URL, which is what a clone by hand needs.
    #[must_use]
    pub fn copy_ssh_url(&self, shell: &Shell) -> AppAction {
        self.selected(shell)
            .map(|row| row.repo.ssh_url)
            .filter(|url| !url.is_empty())
            .map_or(AppAction::None, |text| AppAction::Copy {
                text,
                content: CopiedContent::Url,
            })
    }

    /// `C`: clone the selected repository into the workspace. Refusals are
    /// notifications, not errors in the log: there is always something else
    /// to do with the row.
    pub fn clone_selected(&mut self, shell: &mut Shell) -> AppAction {
        let Some(row) = self.selected(shell) else {
            return AppAction::None;
        };
        if let Some(local) = row.local.as_ref() {
            shell.set_error(format!(
                "{} is already at {}",
                row.repo.name,
                local.path.display()
            ));
            return AppAction::None;
        }
        let Some(workspace) = shell.workspace().map(std::path::Path::to_path_buf) else {
            shell.set_error(
                "Nowhere to clone into \u{2014} pass --workspace or set TICKET_TUI_WORKSPACE"
                    .to_owned(),
            );
            return AppAction::None;
        };
        let Some(url) = clone_url(&row.repo) else {
            shell.set_error(format!("Azure DevOps gave {} no clone URL", row.repo.name));
            return AppAction::None;
        };
        shell.set_news(format!(
            "Cloning {} into {}",
            row.repo.name,
            workspace.display()
        ));
        AppAction::LocalGit(LocalRequest::Clone {
            repo_id: row.repo.id,
            url,
            into: workspace.join(&row.repo.name),
        })
    }

    /// `G` and `P`: fetch the selected clone, or fast-forward it. Neither is
    /// offered for a repository that is not on the machine, and a pull is
    /// refused while anything is uncommitted rather than being attempted and
    /// failing halfway.
    pub fn git_selected(&mut self, shell: &mut Shell, pull: bool) -> AppAction {
        let Some(row) = self.selected(shell) else {
            return AppAction::None;
        };
        let Some(local) = row.local.as_ref() else {
            shell.set_error(format!(
                "{} is not on this machine \u{2014} C clones it",
                row.repo.name
            ));
            return AppAction::None;
        };
        if let Some(job) = local.busy {
            shell.set_error(format!(
                "Already {} {}",
                job.label().trim_end_matches('\u{2026}'),
                row.repo.name
            ));
            return AppAction::None;
        }
        if pull && local.dirty {
            shell.set_error(format!(
                "{} has uncommitted changes \u{2014} commit or stash them first",
                row.repo.name
            ));
            return AppAction::None;
        }
        let path = local.path.clone();
        let repo_id = row.repo.id.clone();
        AppAction::LocalGit(if pull {
            LocalRequest::Pull { repo_id, path }
        } else {
            LocalRequest::Fetch { repo_id, path }
        })
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.mode = RepoMode::Browse,
            _ => {
                self.query.handle_key(key);
                self.cursor.reset();
            }
        }
        AppAction::None
    }

    fn handle_command_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match command_for_key(key, TabId::Repos) {
            Some(CommandId::Search) => {
                self.mode = RepoMode::Search;
                AppAction::None
            }
            Some(CommandId::Open) => self.open_in_browser(shell),
            Some(CommandId::CopyUrl | CommandId::CopyId) => self.copy_ssh_url(shell),
            Some(CommandId::CloneRepo) => self.clone_selected(shell),
            Some(CommandId::FetchRepo) => self.git_selected(shell, false),
            Some(CommandId::PullRepo) => self.git_selected(shell, true),
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

/// What a clone reads from: ssh by default, because that is what a developer
/// with an SSH key set up wants, and https when `TICKET_TUI_CLONE_PROTOCOL`
/// says so or there is no ssh URL on file.
fn clone_url(repo: &Repo) -> Option<String> {
    let https = std::env::var("TICKET_TUI_CLONE_PROTOCOL")
        .is_ok_and(|protocol| protocol.eq_ignore_ascii_case("https"));
    let (first, second) = if https {
        (&repo.remote_url, &repo.ssh_url)
    } else {
        (&repo.ssh_url, &repo.remote_url)
    };
    [first, second]
        .into_iter()
        .find(|url| !url.is_empty())
        .cloned()
}

/// How many of something each repository has.
fn counts(items: &[(String, i64, String)]) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for (repo_id, _, _) in items {
        if let Some((_, count)) = counts.iter_mut().find(|(held, _)| held == repo_id) {
            *count += 1;
        } else {
            counts.push((repo_id.clone(), 1));
        }
    }
    counts
}

fn count_for(counts: &[(String, usize)], repo_id: &str) -> usize {
    counts
        .iter()
        .find(|(held, _)| held == repo_id)
        .map_or(0, |(_, count)| *count)
}

impl Screen for ReposScreen {
    fn handle_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        if self.mode == RepoMode::Search {
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
                self.cursor.reset();
            }
            _ => return self.handle_command_key(shell, key),
        }
        AppAction::None
    }

    fn handle_paste(&mut self, _shell: &mut Shell, pasted: &str) {
        if self.mode == RepoMode::Search {
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
            // The name column opens the repository page, the way the id column
            // opens a work item.
            PointerTarget::OpenInBrowser { index } => {
                if index < self.visible(shell).len() {
                    self.cursor.focus(index);
                }
                return self.open_in_browser(shell);
            }
            PointerTarget::SortHeader(key) => self.toggle_sort(key),
            // The details pane's buttons stand for the keys they name.
            PointerTarget::RunCommand(CommandId::CloneRepo) => return self.clone_selected(shell),
            PointerTarget::RunCommand(CommandId::FetchRepo) => {
                return self.git_selected(shell, false);
            }
            PointerTarget::RunCommand(CommandId::PullRepo) => {
                return self.git_selected(shell, true);
            }
            PointerTarget::Follow(jump) => return AppAction::Follow(jump),
            // Every URL line copies what it says.
            PointerTarget::CopyText(text) => {
                return AppAction::Copy {
                    text,
                    content: CopiedContent::Url,
                };
            }
            PointerTarget::SearchField => {
                self.mode = RepoMode::Search;
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
        self.mode = RepoMode::Browse;
    }

    fn active_editor(&self) -> Option<TextEditor> {
        (self.mode == RepoMode::Search).then_some(TextEditor::Search)
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
        let Jump::Repo(name) = jump else {
            return false;
        };
        let Some(index) = self
            .visible(shell)
            .iter()
            .position(|row| row.repo.name.eq_ignore_ascii_case(name))
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

    fn snapshot(&self) -> TabSession {
        TabSession {
            query: self.query.text().to_owned(),
            sort_field: <RepoColumn as crate::columns::ColumnId>::key(self.sort.0).to_owned(),
            columns: self.layout.to_session_columns(),
            auto_hide: Some(self.layout.auto_hide),
            ..TabSession::default()
        }
    }

    fn restore(&mut self, _shell: &mut Shell, session: TabSession) {
        self.query = TextInput::new(session.query);
        if let Some(column) =
            <RepoColumn as crate::columns::ColumnId>::from_key(&session.sort_field)
        {
            self.sort = (column, self.sort.1);
        }
        self.layout = TableLayout::from_session_columns(&session.columns, session.auto_hide);
    }

    fn footer_hint(&self, _shell: &Shell) -> &str {
        match self.mode {
            RepoMode::Search => "←→ cursor  Ctrl-W delete word  Ctrl-U clear  Enter/Esc finish",
            RepoMode::Browse => "↑↓/jk move  / search  o open  y copy ssh  Tab details  q quit",
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
        crate::ui::repos::render(frame, self, shell, area);
    }
}
