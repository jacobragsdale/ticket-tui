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
use crate::model::{CompletionOptions, Jump, MergeStrategy, PullRequest};
use crate::pointer::{PointerTarget, ScrollState, ScrollSurface, TextEditor};
use crate::session::TabSession;
use crate::sync::PrAction;
use crate::text_input::TextInput;

mod columns;
mod filters;
mod rows;
#[cfg(test)]
pub(crate) mod tests;

pub use columns::PrColumn;
pub use filters::{PrField, PrSchema};
pub use rows::PrRow;

/// How a vote reads while it is out.
const fn vote_verb(vote: i8) -> &'static str {
    match vote {
        10 => "Approving",
        5 => "Approving with suggestions on",
        -5 => "Waiting for the author on",
        -10 => "Rejecting",
        _ => "Clearing my vote on",
    }
}

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
    /// The completion form: how it should land.
    Complete,
    /// `Abandon !123? · X again`.
    ConfirmAbandon,
    /// The one-line comment prompt.
    Comment,
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
    /// Votes out on the wire, with what they were before, so a refusal can put
    /// the glyph back.
    pending_votes: Vec<(i64, i8)>,
    /// Votes that landed, oldest first, for `u`.
    undo_votes: Vec<(i64, i8)>,
    /// The completion form's state, and which of its three rows is focused.
    completion: CompletionOptions,
    completion_field: usize,
    /// Whether the open completion form is turning auto-complete on rather
    /// than completing now.
    auto_completing: bool,
    comment: TextInput,
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
            pending_votes: Vec::new(),
            undo_votes: Vec::new(),
            completion: CompletionOptions::default(),
            completion_field: 0,
            auto_completing: false,
            comment: TextInput::default(),
        }
    }
}

impl PullRequestsScreen {
    /// What the last pull found. The cursor stays on the pull request it was
    /// on, wherever that now sorts: a pull under a review queue must not move
    /// the hand to a different pull request.
    pub fn set_pull_requests(&mut self, requests: Vec<PullRequest>, shell: &Shell) {
        let selected = self.selected(shell).map(|row| row.request.id);
        self.repo_names = shell
            .repos()
            .iter()
            .map(|repo| (repo.id.clone(), repo.name.clone()))
            .collect();
        self.requests = requests;
        self.to_review = self.to_review(shell);
        self.settle_cursor(shell, selected);
    }

    /// Puts the cursor back on one pull request, or clamps it when that one
    /// is no longer on the table.
    fn settle_cursor(&mut self, shell: &Shell, id: Option<i64>) {
        let rows = self.visible(shell);
        match id.and_then(|id| rows.iter().position(|row| row.request.id == id)) {
            Some(index) => self.cursor.focus(index),
            None => self.cursor.clamp(rows.len()),
        }
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

    /// This tab's slice of the context file: the queue, the one the cursor is
    /// on with everything the details pane draws, and how many wait on you.
    #[must_use]
    pub fn agent_context(&self, shell: &Shell) -> crate::agent_context::PullRequestsContext {
        let rows = self.visible(shell);
        let me = shell.me.clone().unwrap_or_default();
        crate::agent_context::PullRequestsContext {
            selected: rows.get(self.cursor.index).map(|row| {
                let request = &row.request;
                crate::agent_context::PullRequestContext {
                    row: self.row_context(row, &me),
                    reviewers: request
                        .reviewers
                        .iter()
                        .map(|reviewer| crate::agent_context::ReviewerContext {
                            name: reviewer.display_name.clone(),
                            vote: reviewer.vote,
                            is_required: reviewer.is_required,
                        })
                        .collect(),
                    work_items: request.work_items.clone(),
                    build: request.build.as_ref().map(|build| {
                        crate::agent_context::PrBuildContext {
                            status: build.status.clone(),
                            run_id: build.run_id,
                        }
                    }),
                    auto_complete: request.auto_complete_set_by.is_some(),
                    thread_count: request.threads.len(),
                    unresolved_threads: request
                        .threads
                        .iter()
                        .filter(|thread| thread.status == "active")
                        .count(),
                }
            }),
            visible_rows: rows.iter().map(|row| self.row_context(row, &me)).collect(),
            to_review_count: self.to_review(shell),
            closed_shown: self.show_closed,
        }
    }

    /// One pull request as an agent reads it.
    fn row_context(&self, row: &PrRow, me: &str) -> crate::agent_context::PullRequestRowContext {
        crate::agent_context::PullRequestRowContext {
            id: row.request.id,
            repo: row.repo.clone(),
            title: row.request.title.clone(),
            author: row.request.created_by.display_name.clone(),
            status: row.request.status.as_str().to_owned(),
            is_draft: row.request.is_draft,
            source_branch: row.source_branch(),
            target_branch: row.target_branch(),
            merge_status: row.request.merge_status.clone(),
            my_vote: self.my_vote(row.request.id, me),
            web_url: row.request.url.clone(),
        }
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

    /// `C`: the completion form, refused here rather than at Azure DevOps
    /// when the merge cannot happen — with what to do about it.
    pub fn open_complete(&mut self, shell: &mut Shell) {
        let Some(row) = self.selected(shell) else {
            shell.set_error("No pull request to complete");
            return;
        };
        if row.request.status.is_closed() {
            shell.set_error(format!("!{} is already closed", row.request.id));
            return;
        }
        if row.request.has_conflicts() {
            shell.set_error(format!(
                "!{} has merge conflicts — press o to sort them out in the browser",
                row.request.id
            ));
            return;
        }
        if row
            .request
            .build
            .as_ref()
            .is_some_and(|build| build.status.eq_ignore_ascii_case("rejected"))
        {
            shell.set_error(format!(
                "the build on !{} is failing — press o to look at it",
                row.request.id
            ));
            return;
        }
        self.completion = CompletionOptions::default();
        self.completion_field = 0;
        self.mode = PrMode::Complete;
    }

    /// The completion options as the form has them.
    #[must_use]
    pub fn completion(&self) -> CompletionOptions {
        self.completion.clone()
    }

    #[must_use]
    pub const fn completion_field(&self) -> usize {
        self.completion_field
    }

    /// Sends the completion.
    pub fn complete(&mut self, shell: &mut Shell) -> AppAction {
        self.mode = PrMode::Browse;
        let Some(row) = self.selected(shell) else {
            return AppAction::None;
        };
        shell.set_status(format!("Completing !{}\u{2026}", row.request.id));
        AppAction::PullRequestAction {
            repo_id: row.request.repo_id.clone(),
            id: row.request.id,
            action: PrAction::Complete(CompletionOptions {
                // The head the row on screen was read at: a source branch that
                // has moved since is a merge Azure DevOps should refuse.
                last_merge_source_commit: row.request.last_merge_source_commit.clone(),
                ..self.completion.clone()
            }),
        }
    }

    /// `X`: the confirmation, then the abandon.
    pub fn confirm_abandon(&mut self, shell: &mut Shell) {
        let Some(row) = self.selected(shell) else {
            shell.set_error("No pull request to abandon");
            return;
        };
        if row.request.status.is_closed() {
            shell.set_error(format!("!{} is already closed", row.request.id));
            return;
        }
        self.mode = PrMode::ConfirmAbandon;
    }

    /// `t`: turns auto-complete on or off. Turning it on takes the completion
    /// form first, since that is what auto-complete will do when it fires.
    pub fn toggle_auto_complete(&mut self, shell: &mut Shell) -> AppAction {
        let Some(row) = self.selected(shell) else {
            shell.set_error("No pull request to set auto-complete on");
            return AppAction::None;
        };
        if row.request.auto_complete_set_by.is_some() {
            shell.set_status(format!("Turning auto-complete off on !{}", row.request.id));
            return AppAction::PullRequestAction {
                repo_id: row.request.repo_id.clone(),
                id: row.request.id,
                action: PrAction::AutoComplete(false),
            };
        }
        self.completion = CompletionOptions::default();
        self.completion_field = 0;
        self.auto_completing = true;
        self.mode = PrMode::Complete;
        AppAction::None
    }

    /// `n`: the one-line comment prompt.
    pub fn open_comment(&mut self, shell: &mut Shell) {
        if self.selected(shell).is_none() {
            shell.set_error("No pull request to comment on");
            return;
        }
        self.comment = TextInput::default();
        self.mode = PrMode::Comment;
    }

    /// What the comment prompt holds.
    #[must_use]
    pub fn comment_text(&self) -> &str {
        self.comment.text()
    }

    #[must_use]
    pub fn comment_cursor(&self) -> usize {
        self.comment.cursor()
    }

    /// Sends the comment. An empty one is refused here rather than posted.
    pub fn send_comment(&mut self, shell: &mut Shell) -> AppAction {
        let text = self.comment.text().trim().to_owned();
        if text.is_empty() {
            shell.set_error("A comment needs something in it");
            return AppAction::None;
        }
        let Some(row) = self.selected(shell) else {
            self.mode = PrMode::Browse;
            return AppAction::None;
        };
        self.mode = PrMode::Browse;
        shell.set_status(format!("Commenting on !{}\u{2026}", row.request.id));
        AppAction::CommentOnPullRequest {
            repo_id: row.request.repo_id.clone(),
            id: row.request.id,
            text,
        }
    }

    /// What Azure DevOps answered a write with: the pull request as it stands.
    pub fn apply_pull_request(&mut self, shell: &mut Shell, updated: PullRequest) {
        let id = updated.id;
        let status = updated.status;
        if let Some(held) = self.requests.iter_mut().find(|held| held.id == id) {
            // The list pages carry things a single read does not, so what was
            // read per pull request is kept.
            let mut updated = updated;
            if updated.threads.is_empty() {
                updated.threads.clone_from(&held.threads);
            }
            if updated.work_items.is_empty() {
                updated.work_items.clone_from(&held.work_items);
            }
            if updated.build.is_none() {
                updated.build.clone_from(&held.build);
            }
            *held = updated;
        } else {
            self.requests.push(updated);
        }
        self.cursor.clamp(self.visible(shell).len());
        shell.set_status(match status {
            crate::model::PrStatus::Completed => format!("!{id} completed"),
            crate::model::PrStatus::Abandoned => format!("!{id} abandoned"),
            crate::model::PrStatus::Active => format!("!{id} updated"),
        });
    }

    /// A comment Azure DevOps took, which joins the Discussion section.
    pub fn apply_comment(&mut self, shell: &mut Shell, id: i64, thread: crate::model::PrThread) {
        if let Some(request) = self.requests.iter_mut().find(|request| request.id == id) {
            request.threads.push(thread);
        }
        shell.set_status(format!("Commented on !{id}"));
    }

    /// Records my vote on the pull request under the cursor. Optimistic: the
    /// glyph changes at once and a refusal puts it back.
    pub fn vote(&mut self, shell: &mut Shell, vote: i8) -> AppAction {
        let Some(row) = self.selected(shell) else {
            shell.set_error("No pull request to vote on");
            return AppAction::None;
        };
        if row.request.status.is_closed() {
            shell.set_error(format!("!{} is closed", row.request.id));
            return AppAction::None;
        }
        let Some(me) = shell.me().map(str::to_owned) else {
            shell.set_error("Nobody is signed in to vote as");
            return AppAction::None;
        };
        let id = row.request.id;
        let previous = self.my_vote(id, &me);
        self.apply_vote(id, &me, vote);
        self.pending_votes.retain(|(held, _)| *held != id);
        self.pending_votes.push((id, previous));
        shell.set_status(format!("{} !{id}", vote_verb(vote)));
        AppAction::VotePullRequest {
            repo_id: row.request.repo_id.clone(),
            id,
            vote,
        }
    }

    /// The vote the signed-in user has on one pull request.
    #[must_use]
    pub fn my_vote(&self, id: i64, me: &str) -> i8 {
        self.requests
            .iter()
            .find(|request| request.id == id)
            .and_then(|request| {
                request
                    .reviewers
                    .iter()
                    .find(|reviewer| crate::model::same_text(&reviewer.display_name, me))
            })
            .map_or(0, |reviewer| reviewer.vote)
    }

    /// Writes a vote into the stored copy, adding the voter as a reviewer when
    /// they were not one — which is what the endpoint does.
    fn apply_vote(&mut self, id: i64, me: &str, vote: i8) {
        let Some(request) = self.requests.iter_mut().find(|request| request.id == id) else {
            return;
        };
        if let Some(reviewer) = request
            .reviewers
            .iter_mut()
            .find(|reviewer| crate::model::same_text(&reviewer.display_name, me))
        {
            reviewer.vote = vote;
        } else {
            request.reviewers.push(crate::model::PrReviewer {
                id: String::new(),
                display_name: me.to_owned(),
                unique_name: None,
                vote,
                is_required: false,
            });
        }
    }

    /// A vote Azure DevOps took: the optimistic copy was right, and what it
    /// replaced becomes what `u` puts back.
    pub fn vote_accepted(&mut self, id: i64) {
        if let Some(position) = self.pending_votes.iter().position(|(held, _)| *held == id) {
            let entry = self.pending_votes.remove(position);
            self.undo_votes.push(entry);
            if self.undo_votes.len() > 20 {
                self.undo_votes.remove(0);
            }
        }
    }

    /// A vote Azure DevOps refused: the glyph goes back to what it was.
    pub fn vote_rejected(&mut self, shell: &mut Shell, id: i64, refusal: &str) {
        if let Some(position) = self.pending_votes.iter().position(|(held, _)| *held == id) {
            let (_, previous) = self.pending_votes.remove(position);
            if let Some(me) = shell.me().map(str::to_owned) {
                self.apply_vote(id, &me, previous);
            }
        }
        shell.set_error(format!("Vote on !{id} refused: {refusal}"));
    }

    /// `u`: puts the last vote back, which is a vote of its own.
    pub fn undo_vote(&mut self, shell: &mut Shell) -> AppAction {
        let Some((id, previous)) = self.undo_votes.pop() else {
            shell.set_error("Nothing to undo");
            return AppAction::None;
        };
        let Some(request) = self
            .requests
            .iter()
            .find(|request| request.id == id)
            .cloned()
        else {
            return AppAction::None;
        };
        let Some(me) = shell.me().map(str::to_owned) else {
            return AppAction::None;
        };
        self.apply_vote(id, &me, previous);
        shell.set_status(format!("Put my vote on !{id} back"));
        AppAction::VotePullRequest {
            repo_id: request.repo_id,
            id,
            vote: previous,
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

    /// Where the details pane's references point: the repository, the work
    /// items it closes — one line each, named from the work-item rows when the
    /// database holds them — and the build that gates it.
    #[must_use]
    pub fn jumps(
        &self,
        shell: &Shell,
        titles: &dyn Fn(i64) -> Option<String>,
    ) -> Vec<(String, Jump)> {
        let Some(row) = self.selected(shell) else {
            return Vec::new();
        };
        let mut jumps = vec![(row.repo.clone(), Jump::Repo(row.repo.clone()))];
        for id in &row.request.work_items {
            let label =
                titles(*id).map_or_else(|| format!("#{id}"), |title| format!("#{id}  {title}"));
            jumps.push((label, Jump::WorkItems(vec![*id])));
        }
        if row.request.work_items.len() > 1 {
            jumps.push((
                format!("All {} work items", row.request.work_items.len()),
                Jump::WorkItems(row.request.work_items.clone()),
            ));
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

    /// The completion form: three rows, `Space` or the arrows change one,
    /// `Ctrl-S` or `Enter` sends it.
    fn handle_complete_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => {
                self.mode = PrMode::Browse;
                self.auto_completing = false;
            }
            KeyCode::Down | KeyCode::Tab => self.completion_field = (self.completion_field + 1) % 3,
            KeyCode::Up => self.completion_field = (self.completion_field + 2) % 3,
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => match self.completion_field {
                0 => {
                    let index = MergeStrategy::ALL
                        .iter()
                        .position(|strategy| *strategy == self.completion.strategy)
                        .unwrap_or_default();
                    let next = if key.code == KeyCode::Left {
                        (index + MergeStrategy::ALL.len() - 1) % MergeStrategy::ALL.len()
                    } else {
                        (index + 1) % MergeStrategy::ALL.len()
                    };
                    self.completion.strategy = MergeStrategy::ALL[next];
                }
                1 => self.completion.delete_source = !self.completion.delete_source,
                _ => {
                    self.completion.transition_work_items = !self.completion.transition_work_items;
                }
            },
            KeyCode::Enter => {
                if self.auto_completing {
                    self.auto_completing = false;
                    self.mode = PrMode::Browse;
                    let Some(row) = self.selected(shell) else {
                        return AppAction::None;
                    };
                    shell.set_status(format!("Turning auto-complete on for !{}", row.request.id));
                    return AppAction::PullRequestAction {
                        repo_id: row.request.repo_id.clone(),
                        id: row.request.id,
                        action: PrAction::AutoComplete(true),
                    };
                }
                return self.complete(shell);
            }
            _ => {}
        }
        AppAction::None
    }

    /// `X` again abandons it; anything else leaves it alone.
    fn handle_abandon_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        self.mode = PrMode::Browse;
        if key.code != KeyCode::Char('X') {
            return AppAction::None;
        }
        let Some(row) = self.selected(shell) else {
            return AppAction::None;
        };
        shell.set_status(format!("Abandoning !{}\u{2026}", row.request.id));
        AppAction::PullRequestAction {
            repo_id: row.request.repo_id.clone(),
            id: row.request.id,
            action: PrAction::Abandon,
        }
    }

    /// The one-line comment prompt.
    fn handle_comment_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.mode = PrMode::Browse,
            KeyCode::Enter => return self.send_comment(shell),
            _ => {
                self.comment.handle_key(key);
            }
        }
        AppAction::None
    }

    fn handle_command_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        command_for_key(key, TabId::PullRequests)
            .map_or(AppAction::None, |id| self.run_command(shell, id))
    }

    /// One command, whether a key, a button in the details pane, or the
    /// palette asked for it.
    pub fn run_command(&mut self, shell: &mut Shell, id: CommandId) -> AppAction {
        match id {
            CommandId::Search => self.mode = PrMode::Search,
            CommandId::Open => return self.open_in_browser(shell),
            CommandId::Sync => return AppAction::Sync,
            CommandId::HistoryBack => return AppAction::HistoryBack,
            CommandId::HistoryForward => return AppAction::HistoryForward,
            CommandId::ApprovePr => return self.vote(shell, 10),
            CommandId::SuggestPr => return self.vote(shell, 5),
            CommandId::WaitPr => return self.vote(shell, -5),
            CommandId::RejectPr => return self.vote(shell, -10),
            CommandId::UndoVote => return self.undo_vote(shell),
            CommandId::CompletePr => self.open_complete(shell),
            CommandId::AbandonPr => self.confirm_abandon(shell),
            CommandId::AutoCompletePr => return self.toggle_auto_complete(shell),
            CommandId::CommentPr => self.open_comment(shell),
            CommandId::ToggleClosedPrs | CommandId::ToggleFinished => {
                self.show_closed = !self.show_closed;
            }
            CommandId::Quit => shell.should_quit = true,
            _ => {}
        }
        AppAction::None
    }
}

impl Screen for PullRequestsScreen {
    fn handle_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match self.mode {
            PrMode::Search => return self.handle_search_key(key),
            PrMode::Complete => return self.handle_complete_key(shell, key),
            PrMode::ConfirmAbandon => return self.handle_abandon_key(shell, key),
            PrMode::Comment => return self.handle_comment_key(shell, key),
            PrMode::Browse => {}
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
            PointerTarget::FocusDetails => shell.focus = Focus::Details,
            PointerTarget::Follow(jump) => return AppAction::Follow(jump),
            // The details pane's buttons stand for the keys they name.
            PointerTarget::RunCommand(id) => return self.run_command(shell, id),
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
        match self.mode {
            PrMode::Search => Some(TextEditor::Search),
            PrMode::Comment => Some(TextEditor::Prompt),
            _ => None,
        }
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

    fn here(&self, shell: &Shell) -> Option<Jump> {
        self.selected(shell).map(|row| Jump::PullRequest {
            repo: row.repo.clone(),
            id: row.request.id,
        })
    }

    fn select(&mut self, shell: &mut Shell, jump: &Jump) -> bool {
        let Jump::PullRequest { id, .. } = jump else {
            return false;
        };
        let Some(request) = self.requests.iter().find(|request| request.id == *id) else {
            return false;
        };
        // A closed pull request is worth landing on even while they are hidden.
        if request.status.is_closed() {
            self.show_closed = true;
        }
        let position = |screen: &Self| {
            screen
                .visible(shell)
                .iter()
                .position(|row| row.request.id == *id)
        };
        let index = match position(self) {
            Some(index) => index,
            // On file but filtered out: the reference wins over the query,
            // which is cleared rather than reported as a missing row.
            None => {
                self.query.clear();
                self.active_view = None;
                match position(self) {
                    Some(index) => index,
                    None => return false,
                }
            }
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
            PrMode::Complete => "↑↓ rows  ←→/Space change  Enter send  Esc cancel",
            PrMode::ConfirmAbandon => "X abandon it  Esc leave it",
            PrMode::Comment => "Type a line  Enter post  Esc cancel",
            PrMode::Browse => {
                "↑↓/jk move  a/A/w/x vote  u undo  n comment  C complete  X abandon  t auto  o open  ? help"
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
        // What the linked work items are called comes from the shell, which
        // is where every tab reads what another tab's rows are named.
        let titles = shell.work_item_titles().to_vec();
        crate::ui::pull_requests::render(frame, self, shell, &titles, area);
    }
}
