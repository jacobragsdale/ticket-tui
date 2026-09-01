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
    AgentContext, ArmContext, PendingEditContext, SearchContext, SortContext, SyncContext,
    TicketContext, TicketReference, TicketsContext, WorkItemsContext,
};
use crate::classification::{self, ClassificationNode, NodeKind};
use crate::columns::{ColumnLayout, TableLayout};
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
    ArtifactLink, CommentRecord, DetailsUpdate, FamilySnapshot, FamilyTreeEntry, HistoryRecord,
    Identity, PullRequest, RelationKind, RelationRecord, Repo, SortDirection, SortField,
    StateCatalog, StateCategory, StateOption, Ticket, TicketGraph, TicketKey, compare_tickets,
    path_leaf, same_text,
};
pub use crate::model::{RowDensity, SearchOrder};
use crate::notify::PrMarks;
use crate::pointer::{
    DragKind, PointerState, ScrollState, ScrollSurface, SelectableSurface, TextEditor, TextPos,
    TextSelection,
};
pub use crate::pointer::{EditableField, HitRegions, OverlayAnchor, PaneSplit, PointerTarget};
use crate::search::{SearchEngine, SearchMatch};
use crate::session::{NamedView, Session, TabSession};
use crate::sprint::{self, SprintSummary, SummaryRow, SummaryRowKind};
pub use crate::sync::Snapshot;
use crate::sync::{ReparentApplied, ReparentRejection};
use crate::text_input::TextInput;
use crate::timestamp::Timestamp;

pub mod acr;
pub mod aks;
pub mod cursor;
pub mod key_vault;
pub mod pipelines;
pub mod pull_requests;
pub mod repos;
mod screen;
pub mod shell;
pub mod work_items;

pub use acr::AcrScreen;
pub use aks::AksScreen;
pub use cursor::ListCursor;
pub use key_vault::KeyVaultScreen;
pub use pipelines::PipelinesScreen;
pub use pull_requests::PullRequestsScreen;
pub use repos::ReposScreen;
pub use screen::{Screen, TabId};
pub(crate) use shell::relative_age;
pub use shell::{
    DEFAULT_PANE_SPLIT_DETAILS, DEFAULT_PANE_SPLIT_STACKED, DEFAULT_PANE_SPLIT_WIDE,
    DividerOrientation, Focus, NotificationLevel, PaneSeam, PointerUpdate, Shell, SyncStatus,
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
    /// A command the palette chose for the tab that is showing, which the
    /// shell hands to that tab's screen.
    RunCommand(CommandId),
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
    /// Something for the cluster worker: read the pods again, follow a log,
    /// restart a pod. Nothing here waits on it either.
    Aks(crate::aks::AksRequest),
    /// Hand the terminal to `kubectl exec -it` on one pod, and take it back
    /// when the shell in there exits. `container` is the one the log follows;
    /// with none, kubectl picks the pod's default.
    ExecShell {
        context: String,
        key: crate::aks::PodKey,
        container: Option<String>,
    },
    /// Something for the subscription worker behind the ACR and Key Vault
    /// tabs. Nothing here waits on it either.
    Arm(crate::arm_watch::ArmRequest),
    /// Put the secret on screen on the clipboard. It travels as the newtype
    /// rather than as a `String` so that a `{:?}` of an action — in a log, a
    /// panic, a test failure — cannot print it.
    CopySecret(crate::arm::Secret),
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
    Path,
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
            Self::Path => "path",
        }
    }
}

/// The application: the shell every screen shares, and the screens themselves.
pub struct App {
    pub shell: Shell,
    /// The tab keys `1`–`7` switch between. Every screen keeps its own state
    /// while another is showing.
    pub tab: TabId,
    /// Which tab one of the shared overlays — help, the palette, the columns
    /// editor, the database overlay — is open over, when it is open over one
    /// other than the work items. The work items screen draws and drives
    /// those overlays; while this is set, every key and click goes to it, and
    /// what it decides comes back to the tab named here.
    overlay_for: Option<TabId>,
    pub work_items: WorkItemsScreen,
    pub repos: ReposScreen,
    pub pull_requests: PullRequestsScreen,
    pub pipelines: PipelinesScreen,
    pub aks: AksScreen,
    pub acr: AcrScreen,
    pub key_vault: KeyVaultScreen,
    /// The pull requests as the last snapshot left them, for telling a vote
    /// that has landed from a pull that changed nothing. `None` until the
    /// first snapshot of the run, which is the baseline rather than news.
    pull_request_marks: Option<PrMarks>,
}

impl App {
    #[must_use]
    pub fn new(tickets: Vec<Ticket>) -> Self {
        let mut shell = Shell::default();
        let work_items = WorkItemsScreen::new(&mut shell, tickets);
        Self {
            shell,
            tab: TabId::WorkItems,
            overlay_for: None,
            work_items,
            repos: ReposScreen::default(),
            pull_requests: PullRequestsScreen::default(),
            pipelines: PipelinesScreen::default(),
            aks: AksScreen::default(),
            acr: AcrScreen::default(),
            key_vault: KeyVaultScreen::default(),
            pull_request_marks: None,
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
            TabId::Aks => &self.aks,
            TabId::Acr => &self.acr,
            TabId::KeyVault => &self.key_vault,
        }
    }

    /// Switches tabs, closing whatever the screen being left had open. The
    /// screen keeps everything else: its query, its cursor, its scroll.
    pub fn select_tab(&mut self, tab: TabId) {
        if tab == self.tab {
            return;
        }
        self.close_shell_overlay();
        let (shell, screen) = self.screen();
        screen.close_overlay(shell);
        self.tab = tab;
    }

    /// The overlays every tab shares, the quick capture row among them. On the
    /// work items they are the screen's own; on any other tab the work items
    /// screen opens them on that tab's behalf.
    const SHELL_OVERLAYS: [CommandId; 5] = [
        CommandId::Help,
        CommandId::Palette,
        CommandId::Columns,
        CommandId::DatabaseInfo,
        CommandId::QuickCapture,
    ];

    /// Whether one of the shared overlays is open over a tab other than the
    /// work items, which is when the frame draws it over that tab.
    #[must_use]
    pub fn shell_overlay_open(&self) -> bool {
        self.overlay_for.is_some() && self.work_items.shell_overlay_open()
    }

    fn close_shell_overlay(&mut self) {
        if self.overlay_for.take().is_some() && self.work_items.shell_overlay_open() {
            self.work_items.close_overlay(&mut self.shell);
        }
    }

    /// Runs one command on the tab showing: a shared overlay opens over it,
    /// anything else is the screen's own.
    fn run_tab_command(&mut self, id: CommandId) -> AppAction {
        if self.tab != TabId::WorkItems && Self::SHELL_OVERLAYS.contains(&id) {
            self.work_items
                .open_shell_overlay(&mut self.shell, id, self.tab);
            self.overlay_for = Some(self.tab);
            return AppAction::None;
        }
        let action = match self.tab {
            TabId::WorkItems => self.work_items.run_command(&mut self.shell, id),
            TabId::Repos => self.repos.run_command(&mut self.shell, id),
            TabId::PullRequests => self.pull_requests.run_command(&mut self.shell, id),
            TabId::Pipelines => self.pipelines.run_command(&mut self.shell, id),
            TabId::Aks => self.aks.run_command(&mut self.shell, id),
            TabId::Acr => self.acr.run_command(&mut self.shell, id),
            TabId::KeyVault => self.key_vault.run_command(&mut self.shell, id),
        };
        self.apply(action)
    }

    /// One key while a shared overlay is open over another tab: the work
    /// items screen drives the overlay, and the columns editor edits that
    /// tab's layout rather than the work items'.
    fn handle_overlay_key(&mut self, key: KeyEvent) -> AppAction {
        let action = if self.work_items.mode == WorkItemMode::Columns {
            let Self {
                shell,
                work_items,
                repos,
                pull_requests,
                pipelines,
                aks,
                acr,
                key_vault,
                tab,
                ..
            } = self;
            // The work items never get here: their own screen edits its own
            // columns. The arm only keeps the match exhaustive.
            let layout: &mut dyn ColumnLayout = match tab {
                TabId::WorkItems | TabId::Repos => &mut repos.layout,
                TabId::PullRequests => &mut pull_requests.layout,
                TabId::Pipelines => pipelines.columns_mut(),
                TabId::Aks => aks.columns_mut(),
                TabId::Acr => acr.columns_mut(),
                TabId::KeyVault => key_vault.columns_mut(),
            };
            work_items.handle_columns_key_on(shell, key, layout);
            AppAction::None
        } else {
            self.work_items.handle_key(&mut self.shell, key)
        };
        if !self.work_items.shell_overlay_open() {
            self.overlay_for = None;
        }
        self.apply(action)
    }

    /// One click while a shared overlay is open over another tab. Only the
    /// overlay's own targets count: the tab underneath is not there to be
    /// clicked through the overlay.
    fn handle_overlay_mouse(&mut self, mouse: MouseEvent) -> PointerUpdate {
        if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
            let target = self
                .shell
                .hit_regions
                .resolve(mouse.column, mouse.row)
                .filter(|region| region.layer != crate::pointer::PointerLayer::Base)
                // A tab's search row sits on the modal layer so it wins over
                // its own table, not so it can be reached through an overlay.
                .filter(|region| {
                    !matches!(
                        region.target,
                        PointerTarget::SearchField | PointerTarget::ClearQuery
                    )
                })
                .map(|region| region.target.clone());
            let Some(target) = target else {
                self.shell.pointer.clear_press();
                return PointerUpdate::none(false);
            };
            if self.work_items.mode == WorkItemMode::Columns {
                let Self {
                    shell,
                    work_items,
                    repos,
                    pull_requests,
                    pipelines,
                    aks,
                    acr,
                    key_vault,
                    tab,
                    ..
                } = self;
                let layout: &mut dyn ColumnLayout = match tab {
                    TabId::WorkItems | TabId::Repos => &mut repos.layout,
                    TabId::PullRequests => &mut pull_requests.layout,
                    TabId::Pipelines => pipelines.columns_mut(),
                    TabId::Aks => aks.columns_mut(),
                    TabId::Acr => acr.columns_mut(),
                    TabId::KeyVault => key_vault.columns_mut(),
                };
                if work_items.apply_column_target(shell, &target, layout) {
                    shell.pointer.clear_press();
                    return PointerUpdate::none(true);
                }
            }
        }
        let update = self.work_items.handle_mouse(&mut self.shell, mouse);
        if !self.work_items.shell_overlay_open() {
            self.overlay_for = None;
        }
        PointerUpdate {
            action: self.apply(update.action),
            redraw: update.redraw,
        }
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
            Jump::Pod(_) => TabId::Aks,
            Jump::Registry(_) | Jump::Repository { .. } => TabId::Acr,
            Jump::Vault(_) | Jump::VaultItem { .. } => TabId::KeyVault,
        };
        let previous = self.tab;
        self.select_tab(tab);
        let (shell, screen) = self.screen();
        let found = screen.select(shell, jump);
        if found {
            self.shell.record_jump(jump.clone());
        } else {
            self.tab = previous;
            self.shell.set_error(jump.missing_message());
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
            AppAction::RunCommand(id) => self.run_tab_command(id),
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
        self.shell.set_artifact_labels(
            pull_requests
                .iter()
                .map(|request| (request.id, request.title.clone(), request.status))
                .collect(),
            runs.iter()
                .map(|run| (run.id, run.build_number.clone(), run.status, run.result))
                .collect(),
        );
        self.work_items
            .replace_prepared_tickets(&mut self.shell, snapshot);
        self.pipelines.set_pipelines(pipelines, runs, &self.shell);
        self.pull_requests
            .set_pull_requests(pull_requests.clone(), &self.shell);
        self.relate_repos(&pull_requests);
        self.announce_pull_requests(&pull_requests);
    }

    /// A vote landing on one you wrote, one of yours closing, or one turning
    /// up wanting your review: news wherever you are, whichever tab is
    /// showing. A snapshot with no pull requests in it at all — the database
    /// reload carries none — says nothing about them and is left alone.
    fn announce_pull_requests(&mut self, pull_requests: &[PullRequest]) {
        if pull_requests.is_empty() {
            return;
        }
        let (marks, news) = crate::notify::pull_request_news(
            self.pull_request_marks.as_ref(),
            pull_requests,
            self.shell.me(),
        );
        self.pull_request_marks = Some(marks);
        for (title, body) in news {
            self.shell.notify(NotificationLevel::Info, &title, &body);
        }
    }

    /// Gives the Repos tab the repositories the shell holds and what the other
    /// two tabs have open against each. Called after every reload, and once
    /// at start-up from the database, so the tab is never empty until the
    /// first pull lands.
    pub fn relate_repos(&mut self, pull_requests: &[PullRequest]) {
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

    /// The whole workspace as an agent reads it: what the shell knows, and
    /// every tab's slice whether or not it is the one showing — an agent asked
    /// about a pull request should not have to ask the user to press `3`.
    #[must_use]
    pub fn agent_context(&self) -> AgentContext {
        AgentContext {
            database_path: self.shell.database_path.display().to_string(),
            me: self.shell.me.clone(),
            sync: self.work_items.sync_context(&self.shell),
            pending_edits: self.work_items.pending_edit_contexts(),
            active_tab: match self.tab {
                TabId::WorkItems => "work_items",
                TabId::Repos => "repos",
                TabId::PullRequests => "pull_requests",
                TabId::Pipelines => "pipelines",
                TabId::Aks => "aks",
                TabId::Acr => "acr",
                TabId::KeyVault => "key_vault",
            }
            .to_owned(),
            work_items: self.work_items.agent_context(&self.shell),
            repos: self.repos.agent_context(&self.shell),
            pull_requests: self.pull_requests.agent_context(&self.shell),
            pipelines: self.pipelines.agent_context(&self.shell),
            aks: self.aks.agent_context(&self.shell),
            acr: self.acr.agent_context(),
            key_vault: self.key_vault.agent_context(),
            arm: ArmContext {
                subscription: self.shell.arm_subscription().map(str::to_owned),
                offline: self.shell.arm_state().is_some(),
                last_error: self.shell.arm_state().map(str::to_owned),
            },
        }
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
            pane_split_details: self.shell.pane_split_details,
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
        self.shell.pane_split_details = session
            .pane_split_details
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
            aks,
            acr,
            key_vault,
            ..
        } = session;
        self.work_items
            .restore(&mut self.shell, work_items, selected);
        Screen::restore(&mut self.repos, &mut self.shell, repos);
        Screen::restore(&mut self.pull_requests, &mut self.shell, pull_requests);
        Screen::restore(&mut self.pipelines, &mut self.shell, pipelines);
        Screen::restore(&mut self.aks, &mut self.shell, aks);
        Screen::restore(&mut self.acr, &mut self.shell, acr);
        Screen::restore(&mut self.key_vault, &mut self.shell, key_vault);
        self.shell.session_dirty = false;
    }

    /// The shell and the screen the keyboard and the mouse are talking to,
    /// handed back apart so an event can be given one with the other.
    pub fn screen(&mut self) -> (&mut Shell, &mut dyn Screen) {
        let screen: &mut dyn Screen = match self.tab {
            TabId::WorkItems => &mut self.work_items,
            TabId::Repos => &mut self.repos,
            TabId::PullRequests => &mut self.pull_requests,
            TabId::Pipelines => &mut self.pipelines,
            TabId::Aks => &mut self.aks,
            TabId::Acr => &mut self.acr,
            TabId::KeyVault => &mut self.key_vault,
        };
        (&mut self.shell, screen)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        if self.shell_overlay_open() {
            return self.handle_overlay_key(key);
        }
        // `1`–`7` switch tabs from anywhere the digit is not being typed into
        // something. An overlay is closed on the way out rather than left open
        // behind the tab that comes back.
        // A confirmation that is up answers every key first, so nothing
        // switches tabs or opens over it while it is armed.
        let screen_is_free = {
            let screen = self.screen().1;
            screen.active_editor().is_none() && !screen.modal_open()
        };
        if let KeyCode::Char(character) = key.code
            && !key.modifiers.contains(KeyModifiers::CONTROL)
            && let Some(tab) = TabId::from_number(character)
            && screen_is_free
        {
            self.select_tab(tab);
            return AppAction::None;
        }
        // The shared overlays open over any tab; the work items screen
        // answers its own keys, the others hand these four up.
        if self.tab != TabId::WorkItems
            && screen_is_free
            && let Some(id) = command_for_key(key, self.tab)
            && Self::SHELL_OVERLAYS.contains(&id)
        {
            return self.run_tab_command(id);
        }
        let (shell, screen) = self.screen();
        let action = screen.handle_key(shell, key);
        self.apply(action)
    }

    /// The mouse still goes to the screen's own entry point: the pointer state
    /// it answers with is the shell's, not something a screen reports. A click
    /// on the tab bar never reaches a screen — the bar is the shell's.
    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> PointerUpdate {
        if self.shell_overlay_open() {
            return self.handle_overlay_mouse(mouse);
        }
        // The bar belongs to the shell: its tabs, and the two controls at its
        // right end, are answered here rather than by whichever screen is
        // showing \u{2014} `Actions` and `?` open over every tab.
        if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left))
            && let Some(region) = self.shell.hit_regions.resolve(mouse.column, mouse.row)
            && let Some(target) = match region.target {
                PointerTarget::SelectTab { index } => {
                    TabId::ALL.get(index).copied().map(BarTarget::Tab)
                }
                PointerTarget::OpenPalette => Some(BarTarget::Command(CommandId::Palette)),
                PointerTarget::OpenHelp => Some(BarTarget::Command(CommandId::Help)),
                _ => None,
            }
        {
            self.shell.pointer.set_position(mouse.column, mouse.row);
            // The press that started this click went to the screen, so the
            // release has to end it here: left open, every later mouse move
            // would read as a drag.
            self.shell.pointer.clear_press();
            self.shell.pointer.clear_selection();
            return match target {
                BarTarget::Tab(tab) => {
                    self.select_tab(tab);
                    PointerUpdate::none(true)
                }
                BarTarget::Command(id) => PointerUpdate {
                    action: self.run_tab_command(id),
                    redraw: true,
                },
            };
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

/// What a click on the tab bar asks for: another tab, or one of the two
/// controls at its right end.
enum BarTarget {
    Tab(TabId),
    Command(CommandId),
}
