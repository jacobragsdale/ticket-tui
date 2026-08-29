use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;
use serde_json::Value;

use crate::agent_context::{
    AgentContext, PendingEditContext, SearchContext, SortContext, SyncContext, TicketContext,
    TicketReference, TicketsContext,
};
use crate::classification::{self, ClassificationNode, NodeKind};
use crate::columns::TableLayout;
use crate::command::{
    Command, CommandId, EDIT_MENU, EditMenuEntry, REMOVE_PARENT_ROW, command_for_key,
    matching_commands,
};
pub use crate::edit::FieldEdit;
use crate::edit::{EditApplied, EditRejection, EditRequest, normalize_tags};
use crate::export;
pub use crate::filter::FacetTarget;
use crate::filter::{
    FacetValue, FilterField, FilterSet, FilterToken, MatchContext, ParsedQuery, Sentinel,
    WorkItemSchema, days_untouched, facet_values, format_query, is_stale, parse_query, stale_query,
};
pub use crate::model::Jump;
use crate::model::{
    CommentRecord, DetailsUpdate, FamilySnapshot, FamilyTreeEntry, HistoryRecord, Identity,
    RelationKind, RelationRecord, Repo, SortDirection, SortField, StateCatalog, StateCategory,
    StateOption, Ticket, TicketGraph, TicketKey, compare_tickets, path_leaf, same_text,
};
pub use crate::model::{RowDensity, SearchOrder};
use crate::pointer::{
    DragKind, PointerState, ScrollState, ScrollSurface, SelectableSurface, TextEditor, TextPos,
    TextSelection,
};
pub use crate::pointer::{EditableField, HitRegions, OverlayAnchor, PointerTarget};
use crate::search::{SearchEngine, SearchMatch};
use crate::session::{NamedView, Session, TabSession};
use crate::sprint::{self, SprintSummary, SummaryRow, SummaryRowKind};
pub use crate::sync::Snapshot;
use crate::sync::{ReparentApplied, ReparentRejection};
use crate::text_input::TextInput;
use crate::timestamp::Timestamp;

pub mod cursor;
pub mod pipelines;
mod placeholder;
pub mod pull_requests;
pub mod repos;
mod screen;
pub mod shell;
pub mod work_items;

pub use cursor::ListCursor;
pub use pipelines::PipelinesScreen;
pub use placeholder::PlaceholderScreen;
pub use pull_requests::PullRequestsScreen;
pub use repos::ReposScreen;
pub use screen::{Screen, TabId};
pub use shell::{
    DEFAULT_PANE_SPLIT_STACKED, DEFAULT_PANE_SPLIT_WIDE, DividerOrientation, Focus,
    NotificationLevel, PointerUpdate, Shell,
};
use shell::{MAX_SPLIT_PERCENT, MIN_SPLIT_PERCENT};
pub use work_items::{
    BuiltinView, ChildProgress, ChildProgressIndex, ColumnOverlay, DEFAULT_STALE_DAYS,
    DeleteConfirm, EditMenu, EditScope, FacetBar, FilterOverlay, FormField, FormFieldId,
    FormFieldKind, FormKind, FormOverlay, FormPicker, PRIORITY_CHOICES, PROGRESS_BAR_CELLS,
    PaletteState, PriorityPicker, PromptField, SortDraft, SprintOverlay, StatePicker, SyncTarget,
    TextPrompt, TypePicker, UNASSIGNED_LABEL, ViewRow, ViewRowKind, ViewsOverlay, WorkItemMode,
    WorkItemsScreen,
};

#[derive(Clone, Debug, PartialEq)]
pub enum AppAction {
    None,
    /// Go where a reference points: the shell switches tabs if it has to and
    /// asks the screen that holds it to select it.
    Follow(Jump),
    /// Read one repository's branches, for the branch picker.
    FetchBranches(String),
    /// Start one pipeline on one branch.
    TriggerRun {
        pipeline_id: i64,
        branch: String,
    },
    /// Stop one run, or retry the jobs that failed in it.
    RunAction {
        run_id: i64,
        retry: bool,
    },
    /// Complete, abandon, or set auto-complete on one pull request.
    PullRequestAction {
        repo_id: String,
        id: i64,
        action: crate::sync::PrAction,
    },
    /// Leave one comment on one pull request.
    CommentOnPullRequest {
        repo_id: String,
        id: i64,
        text: String,
    },
    /// Record one vote on one pull request, as the signed-in user.
    VotePullRequest {
        repo_id: String,
        id: i64,
        vote: i8,
    },
    /// Clone one repository into the workspace, or fetch or pull the one
    /// already there. The local thread runs git; nothing here waits on it.
    LocalGit(crate::local::LocalRequest),
    /// Read the pending approvals now rather than at the next poll.
    RefreshApprovals,
    /// Approve or reject one approval, with an optional word about why.
    AnswerApproval {
        id: String,
        approve: bool,
        comment: String,
    },
    /// `[` and `]`: back and forward through everywhere this run has been,
    /// across tabs.
    HistoryBack,
    HistoryForward,
    Sync,
    /// Write one field back to Azure DevOps, one request per work item. An
    /// ordinary edit carries a single request; a bulk change over the checked
    /// rows carries one for each of them, and the worker takes them in the
    /// order they are listed.
    Edit(Vec<EditRequest>),
    /// Read the project's team members, so the assignee picker can offer
    /// somebody with no work item in the database yet. Asked for once a
    /// session, when that picker first opens; the picker does not wait on it.
    FetchIdentities,
    /// Read the project's iteration and area trees, so both node pickers can
    /// offer a sprint no work item sits in yet. Asked for once a session, when
    /// either picker first opens on a cache that is empty or stale; the picker
    /// does not wait on it.
    FetchClassificationNodes,
    /// Read the work item types the project's process offers, for a form's
    /// Type field. Asked for once a session, when the first form opens; the
    /// form does not wait on it.
    FetchWorkItemTypes,
    /// Add one work item to the project. `patch` sets its fields and nothing
    /// else — the parent travels as a link the client appends — and, like a
    /// comment, nothing is shown until Azure DevOps has stored it: a work item
    /// has no id, revision, or URL until the server gives it one.
    Create {
        work_item_type: String,
        patch: Vec<Value>,
        parent: Option<i64>,
    },
    /// Move one work item under a different parent, or out from under the one
    /// it has when `new_parent` is `None`. The graph already carries the move,
    /// so a refusal puts both halves of the old link back.
    Reparent {
        key: TicketKey,
        new_parent: Option<i64>,
    },
    /// Send work items to the project's recycle bin, one request each, which
    /// the worker takes in the order they are listed. Nothing leaves the table
    /// until Azure DevOps has taken the delete: a row dropped for a delete that
    /// was refused is a lie the next pull undoes.
    Delete(Vec<TicketKey>),
    /// Leave one comment on one work item. Nothing appears on the work item
    /// until Azure DevOps has stored it, so this is the one write the table
    /// does not make optimistically.
    Comment {
        key: TicketKey,
        text: String,
    },
    /// Hand one work item's description to the user's editor. It carries the
    /// markup Azure DevOps stores, because that is what the editor is opened
    /// on and what an edit has to hand back. This is the one action that takes
    /// the terminal away from the TUI while it runs.
    EditDescription {
        key: TicketKey,
        html: String,
    },
    OpenUrl(String),
    Copy {
        text: String,
        content: CopiedContent,
    },
    WriteFile {
        path: PathBuf,
        contents: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopiedContent {
    Text,
    Id,
    Url,
    Title,
    MarkdownLink,
    Summary,
}

impl CopiedContent {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Id => "id",
            Self::Url => "url",
            Self::Title => "title",
            Self::MarkdownLink => "markdown link",
            Self::Summary => "summary",
        }
    }
}

/// The application: the shell every screen shares, and the screens themselves.
/// There is one today; #665 puts a tab bar over them.
pub struct App {
    pub shell: Shell,
    /// The tab keys `1`–`4` switch between. Every screen keeps its own state
    /// while another is showing.
    pub tab: TabId,
    pub work_items: WorkItemsScreen,
    pub repos: ReposScreen,
    pub pull_requests: PullRequestsScreen,
    pub pipelines: PipelinesScreen,
}

impl App {
    #[must_use]
    pub fn new(tickets: Vec<Ticket>) -> Self {
        let mut shell = Shell::default();
        let work_items = WorkItemsScreen::new(&mut shell, tickets);
        Self {
            shell,
            tab: TabId::WorkItems,
            work_items,
            repos: ReposScreen::default(),
            pull_requests: PullRequestsScreen::default(),
            pipelines: PipelinesScreen::default(),
        }
    }

    /// Everything the tab bar draws: each tab, whether it is the one showing,
    /// and whatever it has waiting.
    #[must_use]
    pub fn tabs(&self) -> Vec<(TabId, bool, Option<String>)> {
        TabId::ALL
            .into_iter()
            .map(|tab| (tab, tab == self.tab, self.screen_for(tab).badge()))
            .collect()
    }

    #[must_use]
    fn screen_for(&self, tab: TabId) -> &dyn Screen {
        match tab {
            TabId::WorkItems => &self.work_items,
            TabId::Repos => &self.repos,
            TabId::PullRequests => &self.pull_requests,
            TabId::Pipelines => &self.pipelines,
        }
    }

    /// Switches tabs, closing whatever the screen being left had open. The
    /// screen keeps everything else: its query, its cursor, its scroll.
    pub fn select_tab(&mut self, tab: TabId) {
        if tab == self.tab {
            return;
        }
        let (shell, screen) = self.screen();
        screen.close_overlay(shell);
        self.tab = tab;
    }

    /// Goes where a reference points: the tab that holds it comes up, and the
    /// screen there settles on it. One nothing holds says so rather than
    /// switching to an empty tab.
    pub fn follow(&mut self, jump: &Jump) -> bool {
        // Where the walk starts from is what `[` comes back to.
        let here = {
            let (shell, screen) = self.screen();
            screen.here(shell)
        };
        if let Some(here) = here {
            self.shell.record_jump(here);
        }
        let tab = match jump {
            Jump::WorkItem(_) | Jump::WorkItems(_) => TabId::WorkItems,
            Jump::Repo(_) => TabId::Repos,
            Jump::PullRequest { .. } => TabId::PullRequests,
            Jump::Pipeline(_) | Jump::Run(_) => TabId::Pipelines,
        };
        let previous = self.tab;
        self.select_tab(tab);
        let (shell, screen) = self.screen();
        let found = screen.select(shell, jump);
        if found {
            self.shell.record_jump(jump.clone());
        } else {
            self.tab = previous;
            self.shell
                .set_error(format!("{} is not in this database", jump.describe()));
        }
        found
    }

    /// `[`: back to wherever the run was before this, on whatever tab.
    pub fn history_back(&mut self) {
        if self.shell.history.len() < 2 {
            self.shell.set_status("Nothing to go back to");
            return;
        }
        let current = self.shell.history.pop().expect("a place to leave");
        let Some(previous) = self.shell.history.last().cloned() else {
            self.shell.history.push(current);
            return;
        };
        self.shell.future.push(current);
        self.walk_to(&previous);
    }

    /// `]`: forward again through what `[` came off.
    pub fn history_forward(&mut self) {
        let Some(next) = self.shell.future.pop() else {
            self.shell.set_status("Nothing to go forward to");
            return;
        };
        self.shell.history.push(next.clone());
        self.walk_to(&next);
    }

    /// A step through the history, which does not record itself.
    fn walk_to(&mut self, jump: &Jump) {
        let history = std::mem::take(&mut self.shell.history);
        let future = std::mem::take(&mut self.shell.future);
        self.follow(jump);
        self.shell.history = history;
        self.shell.future = future;
        self.shell.session_dirty = true;
    }

    /// Carries out the shell's own half of an action and passes the rest on to
    /// the event loop.
    fn apply(&mut self, action: AppAction) -> AppAction {
        match action {
            AppAction::Follow(jump) => {
                self.follow(&jump);
                AppAction::None
            }
            AppAction::HistoryBack => {
                self.history_back();
                AppAction::None
            }
            AppAction::HistoryForward => {
                self.history_forward();
                AppAction::None
            }
            other => other,
        }
    }

    /// Hands one reload to every screen that has a slice of it: the rows and
    /// their graph to the work items, the definitions and runs to Pipelines,
    /// and the repositories to the shell every tab reads.
    pub fn apply_snapshot(&mut self, snapshot: Snapshot) {
        let pipelines = snapshot.pipelines.clone();
        let runs = snapshot.runs.clone();
        let pull_requests = snapshot.pull_requests.clone();
        self.shell.set_work_item_titles(
            snapshot
                .tickets
                .iter()
                .map(|ticket| (ticket.key.id, ticket.title.clone()))
                .collect(),
        );
        self.work_items
            .replace_prepared_tickets(&mut self.shell, snapshot);
        self.pipelines.set_pipelines(pipelines, runs, &self.shell);
        self.pull_requests
            .set_pull_requests(pull_requests.clone(), &self.shell);
        self.repos.set_repos(&self.shell);
        self.repos.set_related(
            pull_requests
                .iter()
                .filter(|request| !request.status.is_closed())
                .map(|request| (request.repo_id.clone(), request.id, request.title.clone()))
                .collect(),
            self.pipelines
                .pipelines()
                .iter()
                .filter_map(|pipeline| {
                    pipeline
                        .repo_id
                        .as_ref()
                        .map(|repo| (repo.clone(), pipeline.id, pipeline.name.clone()))
                })
                .collect(),
        );
    }

    /// The whole session: which tab was showing, each tab's slice, and what
    /// the shell keeps across all of them.
    #[must_use]
    pub fn snapshot_session(&self) -> Session {
        let mut session = Session {
            active_tab: self.tab,
            bookmarks: self.work_items.bookmark_keys(),
            history: self.shell.history().to_vec(),
            show_finished: self.work_items.show_finished(),
            selected: self
                .work_items
                .selected_ticket()
                .map(|ticket| ticket.key.clone()),
            pane_split_wide: self.shell.pane_split_wide,
            pane_split_stacked: self.shell.pane_split_stacked,
            stale_days: self.work_items.remembered_stale_days(),
            ..Session::default()
        };
        for tab in TabId::ALL {
            session.set_tab(tab, self.screen_for(tab).snapshot());
        }
        session
    }

    /// The same, coming back on the next run: every tab is put back, and the
    /// run reopens on the one it was left on.
    pub fn restore_session(&mut self, session: Session) {
        self.shell.pane_split_wide = session
            .pane_split_wide
            .clamp(MIN_SPLIT_PERCENT, MAX_SPLIT_PERCENT);
        self.shell.pane_split_stacked = session
            .pane_split_stacked
            .clamp(MIN_SPLIT_PERCENT, MAX_SPLIT_PERCENT);
        self.work_items
            .restore_shared(session.stale_days, session.show_finished, &session);
        self.shell.history = session.history.clone();
        self.tab = session.active_tab;
        let selected = session.selected.clone();
        let Session {
            work_items,
            repos,
            pull_requests,
            pipelines,
            ..
        } = session;
        self.work_items
            .restore(&mut self.shell, work_items, selected);
        Screen::restore(&mut self.repos, &mut self.shell, repos);
        Screen::restore(&mut self.pull_requests, &mut self.shell, pull_requests);
        Screen::restore(&mut self.pipelines, &mut self.shell, pipelines);
        self.shell.session_dirty = false;
    }

    /// The shell and the screen the keyboard and the mouse are talking to,
    /// handed back apart so an event can be given one with the other. #665
    /// makes which screen this is a matter of the tab bar.
    pub fn screen(&mut self) -> (&mut Shell, &mut dyn Screen) {
        let screen: &mut dyn Screen = match self.tab {
            TabId::WorkItems => &mut self.work_items,
            TabId::Repos => &mut self.repos,
            TabId::PullRequests => &mut self.pull_requests,
            TabId::Pipelines => &mut self.pipelines,
        };
        (&mut self.shell, screen)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        // `1`–`4` switch tabs from anywhere the digit is not being typed into
        // something. An overlay is closed on the way out rather than left open
        // behind the tab that comes back.
        if let KeyCode::Char(character) = key.code
            && !key.modifiers.contains(KeyModifiers::CONTROL)
            && let Some(tab) = TabId::from_number(character)
            && self.screen().1.active_editor().is_none()
        {
            self.select_tab(tab);
            return AppAction::None;
        }
        let (shell, screen) = self.screen();
        let action = screen.handle_key(shell, key);
        self.apply(action)
    }

    /// The mouse still goes to the screen's own entry point: the pointer state
    /// it answers with is the shell's, not something a screen reports. A click
    /// on the tab bar never reaches a screen — the bar is the shell's.
    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> PointerUpdate {
        if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left))
            && let Some(region) = self.shell.hit_regions.resolve(mouse.column, mouse.row)
            && let PointerTarget::SelectTab { index } = region.target
            && let Some(tab) = TabId::ALL.get(index).copied()
        {
            self.shell.pointer.set_position(mouse.column, mouse.row);
            self.select_tab(tab);
            return PointerUpdate::none(true);
        }
        let (shell, screen) = self.screen();
        let update = screen.handle_mouse(shell, mouse);
        PointerUpdate {
            action: self.apply(update.action),
            redraw: update.redraw,
        }
    }

    pub fn handle_paste(&mut self, pasted: &str) {
        let (shell, screen) = self.screen();
        screen.handle_paste(shell, pasted);
    }

    pub fn handle_resize(&mut self) {
        self.shell.handle_resize();
    }
}
