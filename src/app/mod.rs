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
    days_untouched, facet_values, format_query, is_stale, parse_query, stale_query,
};
use crate::model::{
    CommentRecord, DetailsUpdate, FamilySnapshot, FamilyTreeEntry, HistoryRecord, Identity,
    RelationKind, RelationRecord, SortDirection, SortField, StateCatalog, StateCategory,
    StateOption, Ticket, TicketGraph, TicketKey, compare_tickets, path_leaf, same_text,
};
pub use crate::model::{RowDensity, SearchOrder};
use crate::pointer::{
    DragKind, PointerState, ScrollState, ScrollSurface, SelectableSurface, TextEditor, TextPos,
    TextSelection,
};
pub use crate::pointer::{EditableField, HitRegions, OverlayAnchor, PointerTarget};
use crate::search::{SearchDocuments, SearchEngine, SearchMatch};
use crate::session::{NamedView, Session};
use crate::sprint::{self, SprintSummary, SummaryRow, SummaryRowKind};
use crate::sync::{ReparentApplied, ReparentRejection};
use crate::text_input::TextInput;
use crate::timestamp::Timestamp;

mod context;
mod edits;
mod family;
mod forms;
mod history;
mod pickers;
mod pointer;
mod query;
#[cfg(test)]
mod tests;
mod views;

use edits::{BulkEdit, PendingEdit, UndoEntry};
pub use edits::{DeleteConfirm, EditMenu, EditScope, PromptField, SyncTarget, TextPrompt};
pub use family::{ChildProgress, ChildProgressIndex};
pub use forms::{FormField, FormFieldId, FormFieldKind, FormKind, FormOverlay, FormPicker};
pub use pickers::{
    AssigneeCandidate, AssigneePicker, NodePicker, NodeRow, ParentCandidate, ParentPicker,
    PriorityPicker, StatePicker, TypePicker,
};
pub use pointer::{DividerOrientation, PointerUpdate};
pub use query::{ColumnOverlay, FacetBar, FilterOverlay, PaletteState, SortDraft};
use views::builtin_named;
pub use views::{BuiltinView, SprintOverlay, ViewRow, ViewRowKind, ViewsOverlay};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AppMode {
    #[default]
    Browse,
    Search,
    Sort,
    Help,
    Filter,
    Columns,
    Palette,
    Views,
    Info,
    /// The read-only board for one iteration: who has how much, and how much
    /// of it is finished.
    Sprint,
    Facets,
    /// The list of field editors `e` opens.
    Edit,
    /// The states the selected work item can be moved to.
    StatePicker,
    /// The priorities the selected work item can be given, `Clear` included.
    PriorityPicker,
    /// A single-line field editor, for the Title and Tags rows of the Edit menu.
    Prompt,
    /// The people the selected work item can be assigned to, filtered by typing.
    AssigneePicker,
    /// The iteration or area tree the selected work item can be moved into,
    /// filtered by typing. Which of the two is on [`NodePicker::kind`].
    NodePicker,
    /// A multi-field form, such as the one `n` opens to file a new work item.
    Form,
    /// The work item types a form's Type field can name.
    TypePicker,
    /// The work items the selected one can be filed under, filtered by typing.
    ParentPicker,
    /// The last word before a work item goes to the recycle bin.
    ConfirmDelete,
}

/// How long a work item may sit untouched before the Changed column flags it,
/// when neither a flag, a variable, nor the session says otherwise.
pub const DEFAULT_STALE_DAYS: u16 = 14;

/// The thresholds the palette's **Set stale threshold** steps through, which
/// is how the setting is changed without a number to type: a sprint, a
/// fortnight, three weeks, a month.
pub const STALE_DAY_CHOICES: [u16; 4] = [7, 14, 21, 30];

/// A threshold of zero days would flag every open work item the moment it was
/// touched, which is not a threshold at all, so one day is the floor.
const MIN_STALE_DAYS: u16 = 1;

/// Percentage of the workspace given to the tickets pane when the panes sit
/// side by side, and when they are stacked.
pub const DEFAULT_PANE_SPLIT_WIDE: u16 = 62;

pub const DEFAULT_PANE_SPLIT_STACKED: u16 = 56;

/// Safety rails for a stored or dragged split, applied on top of the cell
/// minimums below.
const MIN_SPLIT_PERCENT: u16 = 20;

const MAX_SPLIT_PERCENT: u16 = 80;

/// Cells each pane keeps while the divider is dragged.
const MIN_TICKETS_COLUMNS: u16 = 40;

const MIN_DETAILS_COLUMNS: u16 = 30;

const MIN_PANE_ROWS: u16 = 6;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Focus {
    #[default]
    Tickets,
    Family,
    Details,
}

impl Focus {
    #[must_use]
    pub const fn is_details_pane(self) -> bool {
        matches!(self, Self::Family | Self::Details)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AppAction {
    None,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationLevel {
    Info,
    Error,
}

#[derive(Debug)]
struct Notification {
    message: String,
    level: NotificationLevel,
    expires_at: Instant,
}

const INFO_NOTIFICATION_DURATION: Duration = Duration::from_secs(4);

const ERROR_NOTIFICATION_DURATION: Duration = Duration::from_secs(8);

/// The priorities the picker offers, in the order it lists them. `None` is the
/// `Clear` row, which takes the field off the work item rather than writing an
/// empty value.
pub const PRIORITY_CHOICES: [Option<i64>; 5] = [Some(1), Some(2), Some(3), Some(4), None];

/// What the assignee picker calls nobody at all, and the row that unassigns.
pub const UNASSIGNED_LABEL: &str = "Unassigned";

/// How long a cached copy of the classification trees is trusted before either
/// picker asks Azure DevOps for them again. Sprints are added a few times a
/// year, so an hour is generous and still keeps a long-running session honest.
const CLASSIFICATION_MAX_AGE_SECONDS: i64 = 3600;

/// What a new work item is filed as unless the Type field says otherwise,
/// which is what the Basic process calls its everyday unit of work.
pub const DEFAULT_WORK_ITEM_TYPE: &str = "Issue";

/// The five built-in views, in the order the overlay lists them. All but
/// `Stale` take the table's default order, newest change first; `Stale` turns
/// it around, so the item nobody has touched for longest is the first row.
pub const BUILTIN_VIEWS: [BuiltinView; 5] = [
    BuiltinView {
        name: "Mine",
        query: "assignee:@me",
        sort_field: SortField::Changed,
        sort_direction: SortDirection::Descending,
    },
    BuiltinView {
        name: "Unassigned",
        query: "assignee:@none",
        sort_field: SortField::Changed,
        sort_direction: SortDirection::Descending,
    },
    BuiltinView {
        name: "Doing",
        query: "state:doing",
        sort_field: SortField::Changed,
        sort_direction: SortDirection::Descending,
    },
    BuiltinView {
        name: "Stale",
        query: "changed:>14d state:@open",
        sort_field: SortField::Changed,
        sort_direction: SortDirection::Ascending,
    },
    BuiltinView {
        name: "Current sprint",
        query: "iteration:@current",
        sort_field: SortField::Changed,
        sort_direction: SortDirection::Descending,
    },
];

/// What the sprint summary says when it has no iteration to count: no sprint
/// is scheduled around today and no row is selected to borrow one from. Split
/// across lines here rather than wrapped at paint time, so it sits inside the
/// overlay whatever the terminal is doing.
const NO_SPRINT_NOTICE: [&str; 4] = [
    "No sprint to summarise.",
    "",
    "No iteration is scheduled around today,",
    "and no work item is selected.",
];

/// How many edits back `u` can go in one session. Twenty is far more than a
/// mis-click needs, and short enough that the stack never becomes a memory of
/// the session in its own right.
const UNDO_DEPTH: usize = 20;

/// How many refused work items a bulk change names before it counts the rest.
/// Three is enough to act on and short enough to read in one notification.
const NAMED_BULK_FAILURES: usize = 3;

/// How many cells wide the details pane draws the bar beside the ratio.
pub const PROGRESS_BAR_CELLS: usize = 6;

#[derive(Debug)]
pub struct PreparedTickets {
    tickets: Vec<Ticket>,
    search_documents: SearchDocuments,
    graph: TicketGraph,
    /// The states each work item type allows, empty until a sync cached them.
    states: StateCatalog,
}

impl PreparedTickets {
    #[must_use]
    pub fn new(tickets: Vec<Ticket>) -> Self {
        Self::with_graph(tickets, TicketGraph::default())
    }

    #[must_use]
    pub fn with_graph(tickets: Vec<Ticket>, graph: TicketGraph) -> Self {
        let search_documents = SearchDocuments::prepare(&tickets);
        Self {
            tickets,
            search_documents,
            graph,
            states: StateCatalog::default(),
        }
    }

    /// The cached work item type states that came out of the same database
    /// read, so the state picker and the rows never disagree.
    #[must_use]
    pub fn with_states(mut self, states: StateCatalog) -> Self {
        self.states = states;
        self
    }

    #[must_use]
    pub fn ticket_count(&self) -> usize {
        self.tickets.len()
    }

    /// The work item type states read alongside these rows.
    #[must_use]
    pub const fn states(&self) -> &StateCatalog {
        &self.states
    }
}

pub struct App {
    tickets: Arc<Vec<Ticket>>,
    visible: Vec<SearchMatch>,
    search: SearchEngine,
    search_generation: u64,
    pending_selection: Option<TicketKey>,
    pub search_pending: bool,
    query: TextInput,
    search_history: Vec<String>,
    search_history_index: Option<usize>,
    search_history_draft: String,
    pub search_order: SearchOrder,
    /// Whether the table lists work the workflow has finished with. Off by
    /// default, because the view a manager opens on is the open backlog; the
    /// choice is kept in the session file.
    show_finished: bool,
    pub row_density: RowDensity,
    pub sort_field: SortField,
    pub sort_direction: SortDirection,
    pub layout: TableLayout,
    pub mode: AppMode,
    pub focus: Focus,
    pub table_state: TableState,
    pub table: ScrollState,
    pub details: ScrollState,
    /// Which row of the details pane's scrolling content the family tree's
    /// first row was last drawn on. The heading above it wraps, so only the
    /// renderer knows where the tree starts, and the family cursor needs it to
    /// scroll itself back into view.
    pub details_family_row: usize,
    pub family_cursor: Option<TicketKey>,
    pub help: ScrollState,
    pub sort: ScrollState,
    pub narrow_details: bool,
    pub pane_split_wide: u16,
    pub pane_split_stacked: u16,
    /// The remembered stale threshold: what the palette last set, and what the
    /// session file carries between runs.
    stale_days: u16,
    /// The threshold this run was started under, from `--stale-days` or
    /// `TICKET_TUI_STALE_DAYS`. It stands over the remembered value until the
    /// palette moves the setting, and is never written back to the session: a
    /// flag passed once should not quietly become the setting.
    stale_days_override: Option<u16>,
    content_area: Rect,
    divider: Option<DividerOrientation>,
    pub reload_pending: bool,
    pub should_quit: bool,
    pub session_dirty: bool,
    notification: Option<Notification>,
    pub sort_draft: SortDraft,
    pub hit_regions: HitRegions,
    pub pointer: PointerState,
    pub filter_overlay: FilterOverlay,
    pub column_overlay: ColumnOverlay,
    pub palette: PaletteState,
    pub views_overlay: ViewsOverlay,
    pub sprint_overlay: SprintOverlay,
    pub facet_bar: FacetBar,
    pub edit_menu: EditMenu,
    pub state_picker: StatePicker,
    pub priority_picker: PriorityPicker,
    pub assignee_picker: AssigneePicker,
    pub parent_picker: ParentPicker,
    pub node_picker: NodePicker,
    pub type_picker: TypePicker,
    /// The open multi-field form, if there is one.
    pub form: Option<FormOverlay>,
    /// How far the open form's field list is scrolled, kept beside every other
    /// surface's offset rather than inside the widget.
    pub form_scroll: ScrollState,
    /// The last form `Esc` closed, kept whole so reopening it brings back every
    /// field and the cursor with them. It lives in memory for the session only:
    /// the session file records how the table is arranged, not a half-typed
    /// work item.
    form_draft: Option<FormOverlay>,
    /// The form a create is out on. It is held rather than dropped so a refusal
    /// can put it back with everything still in it, and it is what stops a
    /// second create being sent on top of the first.
    pending_create: Option<FormOverlay>,
    /// What the open delete confirmation is about, if one is open.
    pub delete_confirm: Option<DeleteConfirm>,
    /// The open single-line field editor, if there is one.
    pub prompt: Option<TextPrompt>,
    /// Where the open picker or prompt is drawn: centred, as every
    /// keyboard-opened one is, or hung off the details-pane field that was
    /// clicked to open it.
    pub overlay_anchor: OverlayAnchor,
    bookmarks: HashSet<TicketKey>,
    selected_keys: HashSet<TicketKey>,
    recent: Vec<TicketKey>,
    future: Vec<TicketKey>,
    views: Vec<NamedView>,
    pub active_view: Option<String>,
    graph: TicketGraph,
    /// Done out of total over each parent's direct children, rebuilt whenever
    /// the rows or the graph move rather than counted again every frame.
    child_progress: ChildProgressIndex,
    /// The states Azure DevOps allows for each work item type. Empty until a
    /// sync has fetched them, which is what [`App::states_for`] falls back for.
    state_catalog: StateCatalog,
    pub loaded_at: Instant,
    pub database_path: PathBuf,
    pub stale: bool,
    pub data_signature: u128,
    /// Whether a pull from Azure DevOps is in flight.
    pub sync_pending: bool,
    /// The work item whose comments and history are being read, if one is.
    /// The details pane says so where that history is about to appear.
    pub details_pending: Option<TicketKey>,
    /// Edits sent to Azure DevOps and not answered yet, keyed by work item.
    pending_edits: HashMap<TicketKey, PendingEdit>,
    /// The moves waiting on Azure DevOps, each remembering the parent the work
    /// item hung under before it was made. A refusal puts that parent back.
    pending_reparents: HashMap<TicketKey, Option<TicketKey>>,
    /// Bulk changes with answers still to come, newest last. There is normally
    /// at most one, but a second started before the first has finished is
    /// counted on its own rather than taking the first one's place.
    bulk_edits: Vec<BulkEdit>,
    /// The edits this session has landed, oldest first, each one ready to be
    /// put back by `u`. Capped at [`UNDO_DEPTH`]; it is not written anywhere,
    /// so it starts empty every run.
    undo_stack: Vec<UndoEntry>,
    /// How many dispatches this session has made, which is where an undo entry
    /// gets the number that gathers a bulk change's work items into one.
    undo_groups: u64,
    /// Work items with a comment posted and not answered yet. A comment is not
    /// optimistic, so this is only what stops a second one being typed on top
    /// of the first.
    pending_comments: HashSet<TicketKey>,
    /// Work items sent to the recycle bin and not answered yet. A delete is not
    /// optimistic either — the row stays until Azure DevOps has taken it — so
    /// this is what stops the same work item being deleted twice and what keeps
    /// the cursor off a row that is on its way out.
    pending_deletes: HashSet<TicketKey>,
    /// Why there is nothing to write to, reported when an edit is attempted
    /// without a configured Azure DevOps project.
    offline_reason: Option<String>,
    /// Whether Azure DevOps is configured at all: an offline run browses the
    /// database and reports no sync state.
    sync_enabled: bool,
    /// Where the rows come from — the organization and project, how often they
    /// are pulled, and the scope narrowing them — as the database overlay
    /// reports it. `None` until the run resolves a project.
    sync_source: Option<String>,
    /// The same project, as the agent context publishes it. `None` until the
    /// run resolves one.
    sync_target: Option<SyncTarget>,
    /// When the last successful pull finished, which is not `loaded_at`: a
    /// SQLite reload moves that too.
    synced_at: Option<Instant>,
    /// The same moment on the wall clock, because the agent context has to say
    /// when the last pull landed and an `Instant` only says how long ago.
    synced_wall_clock: Option<Timestamp>,
    /// The last pull's error, kept so the same timer failure is reported once.
    sync_error: Option<String>,
    /// When the next pull may go out, for a timer Azure DevOps asked to hold
    /// off. Not a failure: the title counts it down instead of saying the sync
    /// broke, and nothing is announced.
    sync_paused_until: Option<Instant>,
    /// Display name of the signed-in Azure DevOps user, so their own work
    /// items can stand out. `None` until a sync records one.
    me: Option<String>,
    /// The people the project's teams hold, as the last identity fetch cached
    /// them. The assignee picker offers these alongside everybody the rows
    /// already name, and reads their addresses out of here.
    identities: Vec<Identity>,
    /// Whether the team members have been asked for this session, so opening
    /// the picker a second time costs nothing.
    identities_requested: bool,
    /// The project's iteration and area trees as the last fetch flattened them,
    /// read out of the database at startup. Both node pickers are built from
    /// these, and `current_iteration` reads the sprint out of them.
    classification_nodes: Vec<ClassificationNode>,
    /// When those trees were last read from Azure DevOps, so a picker opening
    /// on a fresh cache asks for nothing at all.
    classification_fetched_at: Option<Timestamp>,
    /// Whether the trees have been asked for this session, so opening either
    /// picker a second time costs nothing.
    classification_requested: bool,
    /// The work item types the project's process offers, as the last fetch
    /// cached them, read out of the database at startup. A form's Type field is
    /// built from these.
    work_item_types: Vec<String>,
    /// Whether the types have been asked for this session, so opening a second
    /// form costs nothing.
    work_item_types_requested: bool,
}

/// Which editor a clicked details-pane field opens. Every one of them is a
/// command already, so a click and the Edit menu reach the same code.
#[must_use]
const fn command_for_field(field: EditableField) -> CommandId {
    match field {
        EditableField::Title => CommandId::EditTitle,
        EditableField::State => CommandId::ChangeState,
        EditableField::Assignee => CommandId::EditAssignee,
        EditableField::Priority => CommandId::EditPriority,
        EditableField::Tags => CommandId::EditTags,
        EditableField::Iteration => CommandId::EditIteration,
        EditableField::Area => CommandId::EditArea,
    }
}

/// Compact wording for a wait still to come, coarse on purpose: the exact
/// second the timer comes back is nobody's business, and a title that ticks
/// every second is a title that has to be redrawn every second.
fn remaining_wait(left: Duration) -> String {
    // Rounded up, so a two minute pause read a millisecond after it started
    // still says two minutes rather than counting down from one.
    let seconds = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m", seconds.div_ceil(60))
    }
}

/// Compact relative wording shared by the freshness and sync labels.
fn relative_age(age: Duration) -> String {
    if age.as_secs() < 45 {
        "just now".into()
    } else if age.as_secs() < 3600 {
        format!("{}m ago", age.as_secs() / 60)
    } else if age.as_secs() < 86_400 {
        format!("{}h ago", age.as_secs() / 3600)
    } else {
        format!("{}d ago", age.as_secs() / 86_400)
    }
}

impl App {
    #[must_use]
    pub fn new(tickets: Vec<Ticket>) -> Self {
        let prepared = PreparedTickets::new(tickets);
        let search = SearchEngine::from_documents(prepared.search_documents);
        let mut app = Self {
            tickets: Arc::new(prepared.tickets),
            visible: Vec::new(),
            search,
            search_generation: 0,
            pending_selection: None,
            search_pending: false,
            query: TextInput::default(),
            search_history: Vec::new(),
            search_history_index: None,
            search_history_draft: String::new(),
            search_order: SearchOrder::Relevance,
            show_finished: false,
            row_density: RowDensity::Compact,
            sort_field: SortField::Changed,
            sort_direction: SortDirection::Descending,
            layout: TableLayout::default(),
            mode: AppMode::Browse,
            focus: Focus::Tickets,
            table_state: TableState::default(),
            table: ScrollState::default(),
            details: ScrollState::default(),
            details_family_row: 0,
            family_cursor: None,
            help: ScrollState::default(),
            sort: ScrollState::default(),
            narrow_details: false,
            pane_split_wide: DEFAULT_PANE_SPLIT_WIDE,
            pane_split_stacked: DEFAULT_PANE_SPLIT_STACKED,
            stale_days: DEFAULT_STALE_DAYS,
            stale_days_override: None,
            content_area: Rect::ZERO,
            divider: None,
            reload_pending: false,
            should_quit: false,
            session_dirty: false,
            notification: None,
            sort_draft: SortDraft {
                field_index: 0,
                direction: SortDirection::Descending,
            },
            hit_regions: HitRegions::default(),
            pointer: PointerState::default(),
            filter_overlay: FilterOverlay::default(),
            column_overlay: ColumnOverlay::default(),
            palette: PaletteState::default(),
            views_overlay: ViewsOverlay::default(),
            sprint_overlay: SprintOverlay::default(),
            facet_bar: FacetBar::default(),
            edit_menu: EditMenu::default(),
            state_picker: StatePicker::default(),
            priority_picker: PriorityPicker::default(),
            assignee_picker: AssigneePicker::default(),
            parent_picker: ParentPicker::default(),
            node_picker: NodePicker::default(),
            prompt: None,
            overlay_anchor: OverlayAnchor::Centered,
            bookmarks: HashSet::new(),
            selected_keys: HashSet::new(),
            recent: Vec::new(),
            future: Vec::new(),
            views: Vec::new(),
            active_view: None,
            graph: prepared.graph,
            child_progress: ChildProgressIndex::default(),
            state_catalog: prepared.states,
            loaded_at: Instant::now(),
            database_path: PathBuf::new(),
            stale: false,
            data_signature: 0,
            sync_pending: false,
            details_pending: None,
            pending_edits: HashMap::new(),
            pending_reparents: HashMap::new(),
            bulk_edits: Vec::new(),
            undo_stack: Vec::new(),
            undo_groups: 0,
            pending_comments: HashSet::new(),
            pending_deletes: HashSet::new(),
            delete_confirm: None,
            type_picker: TypePicker::default(),
            form: None,
            form_scroll: ScrollState::default(),
            form_draft: None,
            pending_create: None,
            work_item_types: Vec::new(),
            work_item_types_requested: false,
            offline_reason: None,
            sync_enabled: false,
            sync_source: None,
            sync_target: None,
            synced_at: None,
            synced_wall_clock: None,
            sync_error: None,
            sync_paused_until: None,
            me: None,
            identities: Vec::new(),
            identities_requested: false,
            classification_nodes: Vec::new(),
            classification_fetched_at: None,
            classification_requested: false,
        };
        app.refresh_child_progress();
        app.show_all(None);
        app
    }

    #[must_use]
    pub fn tickets(&self) -> &[Ticket] {
        &self.tickets
    }

    #[must_use]
    pub fn visible_count(&self) -> usize {
        self.visible.len()
    }

    pub fn visible_tickets(&self) -> impl ExactSizeIterator<Item = &Ticket> {
        self.visible
            .iter()
            .map(|entry| &self.tickets[entry.ticket_index])
    }

    #[must_use]
    pub fn selected_ticket(&self) -> Option<&Ticket> {
        let selected = self.table_state.selected()?;
        let entry = self.visible.get(selected)?;
        self.tickets.get(entry.ticket_index)
    }

    #[must_use]
    pub fn selected_row(&self) -> Option<usize> {
        self.table_state.selected()
    }

    pub fn configure_database(&mut self, path: PathBuf, signature: u128) {
        self.database_path = path;
        self.data_signature = signature;
        self.loaded_at = Instant::now();
        self.stale = false;
    }

    pub fn set_workspace_graph(&mut self, graph: TicketGraph) {
        self.graph = graph;
        self.refresh_child_progress();
        self.sync_family_state();
    }

    #[must_use]
    pub fn freshness_label(&self) -> String {
        relative_age(self.loaded_at.elapsed())
    }

    /// Turns on the sync parts of the UI. An offline run leaves them off, so
    /// the table title says nothing about a sync that can not happen.
    pub const fn enable_sync(&mut self) {
        self.sync_enabled = true;
    }

    /// A pull has started.
    pub const fn begin_sync(&mut self) {
        self.sync_pending = true;
    }

    /// A pull succeeded. The tickets it brought are applied separately, so this
    /// only records that Azure DevOps was reached.
    pub fn finish_sync(&mut self) {
        self.sync_pending = false;
        self.sync_error = None;
        self.sync_paused_until = None;
        self.synced_at = Some(Instant::now());
        self.synced_wall_clock = Some(Timestamp::now());
    }

    /// Azure DevOps asked to be left alone until `until`, and the timer agreed.
    /// Nothing is wrong and nothing is announced: this is the pause the title
    /// counts down, and the next success clears it. Deliberately not
    /// [`Self::fail_sync`] — a throttled pull is the service working as
    /// designed, and an error toast a minute would only be noise.
    pub fn pause_sync(&mut self, until: Instant) {
        self.sync_pending = false;
        self.sync_error = None;
        self.sync_paused_until = Some(until);
    }

    /// A pull failed. Reports whether the failure is worth a notification: the
    /// same error on consecutive timer pulls is not, because the table title
    /// already says the sync is failing. `announce` forces one anyway, for a
    /// pull the user asked for.
    pub fn fail_sync(&mut self, error: &str, announce: bool) -> bool {
        self.sync_pending = false;
        self.sync_paused_until = None;
        let repeated = self.sync_error.as_deref() == Some(error);
        self.sync_error = Some(error.to_owned());
        announce || !repeated
    }

    /// What the table title appends after the sort order, most urgent first.
    #[must_use]
    pub fn activity_label(&self) -> Option<String> {
        if self.sync_enabled && self.sync_pending {
            return Some("Syncing…".into());
        }
        if self.reload_pending {
            return Some("Reloading…".into());
        }
        if self.sync_enabled
            && let Some(left) = self.sync_pause_left()
        {
            return Some(format!("Sync paused {}", remaining_wait(left)));
        }
        if self.sync_enabled && self.sync_error.is_some() {
            return Some("Sync failed".into());
        }
        if self.stale {
            return Some("Stale".into());
        }
        self.synced_at
            .filter(|_| self.sync_enabled)
            .map(|at| format!("Synced {}", relative_age(at.elapsed())))
    }

    /// Where the rows are pulled from, as the database overlay reports it.
    pub fn set_sync_source(&mut self, source: Option<String>) {
        self.sync_source = source;
    }

    /// The same project, for the agent context, which needs the organization,
    /// the project, and the interval apart rather than as one line of prose.
    pub fn set_sync_target(&mut self, target: Option<SyncTarget>) {
        self.sync_target = target;
    }

    /// The database overlay's one-line account of the sync: where the rows come
    /// from, and how the last pull went. An offline run says why it is offline
    /// there instead — a missing organization, or a database another project
    /// filled.
    #[must_use]
    pub fn sync_summary(&self) -> String {
        let state = if self.sync_enabled {
            self.sync_state()
        } else {
            self.offline_reason.as_ref().map_or_else(
                || "offline; no Azure DevOps organization configured".to_owned(),
                |reason| format!("offline; {reason}"),
            )
        };
        match &self.sync_source {
            Some(source) => format!("{source} · {state}"),
            None => state,
        }
    }

    /// How the last pull went, for a run that can pull at all.
    fn sync_state(&self) -> String {
        let last = self
            .synced_at
            .map_or_else(|| "not yet".to_owned(), |at| relative_age(at.elapsed()));
        if self.sync_pending {
            format!("in progress, last {last}")
        } else if let Some(left) = self.sync_pause_left() {
            format!(
                "paused for throttling, next in {}, last {last}",
                remaining_wait(left)
            )
        } else if let Some(error) = &self.sync_error {
            format!("failed, last {last}: {error}")
        } else {
            last
        }
    }

    /// How long the throttling pause still has to run, or `None` once it is
    /// over and the timer is free again.
    fn sync_pause_left(&self) -> Option<Duration> {
        let left = self
            .sync_paused_until?
            .saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left)
    }

    pub fn set_status(&mut self, message: impl Into<String>) {
        self.set_notification(message, NotificationLevel::Info, INFO_NOTIFICATION_DURATION);
    }

    pub fn set_error(&mut self, message: impl Into<String>) {
        self.set_notification(
            message,
            NotificationLevel::Error,
            ERROR_NOTIFICATION_DURATION,
        );
    }

    #[must_use]
    pub fn notification(&self) -> Option<(&str, NotificationLevel)> {
        self.notification
            .as_ref()
            .map(|notification| (notification.message.as_str(), notification.level))
    }

    pub fn tick(&mut self) -> bool {
        if self
            .notification
            .as_ref()
            .is_some_and(|notification| Instant::now() >= notification.expires_at)
        {
            self.notification = None;
            return true;
        }
        false
    }

    #[must_use]
    pub fn next_wakeup(&self) -> Option<Duration> {
        self.notification.as_ref().map(|notification| {
            notification
                .expires_at
                .saturating_duration_since(Instant::now())
        })
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        // Ctrl-C quits from every mode; other bindings only apply in browse mode.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && command_for_key(key) == Some(CommandId::Quit)
        {
            return self.run_command(CommandId::Quit);
        }

        match self.mode {
            AppMode::Browse => self.handle_browse_key(key),
            AppMode::Search => self.handle_search_key(key),
            AppMode::Sort => self.handle_sort_key(key),
            AppMode::Help => {
                self.handle_help_key(key);
                AppAction::None
            }
            AppMode::Filter => {
                self.handle_filter_key(key);
                AppAction::None
            }
            AppMode::Columns => {
                self.handle_columns_key(key);
                AppAction::None
            }
            AppMode::Palette => self.handle_palette_key(key),
            AppMode::Views => self.handle_views_key(key),
            AppMode::Info => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('i') | KeyCode::Char('q')
                ) {
                    self.mode = AppMode::Browse;
                }
                AppAction::None
            }
            AppMode::Sprint => {
                self.handle_sprint_key(key);
                AppAction::None
            }
            AppMode::Facets => {
                self.handle_facet_key(key);
                AppAction::None
            }
            AppMode::Edit => self.handle_edit_menu_key(key),
            AppMode::StatePicker => self.handle_state_picker_key(key),
            AppMode::PriorityPicker => self.handle_priority_picker_key(key),
            AppMode::Prompt => self.handle_prompt_key(key),
            AppMode::AssigneePicker => self.handle_assignee_picker_key(key),
            AppMode::ParentPicker => self.handle_parent_picker_key(key),
            AppMode::NodePicker => self.handle_node_picker_key(key),
            AppMode::Form => self.handle_form_key(key),
            AppMode::TypePicker => self.handle_type_picker_key(key),
            AppMode::ConfirmDelete => self.handle_delete_confirm_key(key),
        }
    }

    pub fn handle_resize(&mut self) {
        self.pointer.clear_selection();
        if matches!(
            self.pointer.drag(),
            DragKind::Text | DragKind::Cancelled | DragKind::Divider
        ) {
            self.pointer.set_drag(DragKind::Cancelled);
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> AppAction {
        // Navigation keys depend on the focused pane; everything else is a command.
        match key.code {
            KeyCode::Char(' ') if self.focus != Focus::Family => self.toggle_row_selection(),
            KeyCode::Tab => self.toggle_focus(),
            KeyCode::Down | KeyCode::Char('j') => self.move_focused(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_focused(-1),
            KeyCode::PageDown => match self.focus {
                Focus::Family => self.move_family_cursor(self.family_page_size()),
                Focus::Tickets | Focus::Details => self.move_focused(10),
            },
            KeyCode::PageUp => match self.focus {
                Focus::Family => self.move_family_cursor(-self.family_page_size()),
                Focus::Tickets | Focus::Details => self.move_focused(-10),
            },
            KeyCode::Home => match self.focus {
                Focus::Tickets => self.select_row(0),
                Focus::Family => self.move_family_cursor_to_edge(false),
                Focus::Details => self.details.scroll_to(0),
            },
            KeyCode::End => match self.focus {
                Focus::Tickets => self.select_row(self.visible.len().saturating_sub(1)),
                Focus::Family => self.move_family_cursor_to_edge(true),
                Focus::Details => self.details.scroll_to(self.details.max_offset()),
            },
            KeyCode::Enter => match self.focus {
                Focus::Tickets => {}
                Focus::Family => {
                    if let Some(key) = self.family_cursor.clone() {
                        self.jump_to_ticket(&key);
                    }
                }
                Focus::Details => {
                    // A field the pointer is resting on opens its editor, the
                    // way clicking it would; anywhere else still opens the
                    // work item in the browser.
                    if let Some(field) = self.pointed_edit_field() {
                        return self.open_field_editor(field);
                    }
                    self.record_history();
                    return self.open_selected();
                }
            },
            KeyCode::Esc if !self.query.is_empty() => self.set_query(String::new()),
            KeyCode::Esc if !self.selected_keys.is_empty() => self.selected_keys.clear(),
            _ => {
                if let Some(id) = command_for_key(key) {
                    return self.run_command(id);
                }
            }
        }
        AppAction::None
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.finish_search(),
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.recall_previous_search();
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.recall_next_search();
            }
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),
            _ => self.edit_query(|query| {
                query.handle_key(key);
            }),
        }
        AppAction::None
    }

    fn handle_sort_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.sort_draft.field_index = self.sort_draft.field_index.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.sort_draft.field_index =
                    (self.sort_draft.field_index + 1).min(SortField::ALL.len() - 1);
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.sort_draft.direction = SortDirection::Ascending;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.sort_draft.direction = SortDirection::Descending;
            }
            KeyCode::Enter => {
                self.set_sort(
                    SortField::ALL[self.sort_draft.field_index],
                    self.sort_draft.direction,
                );
                self.mode = AppMode::Browse;
            }
            _ => {}
        }
        AppAction::None
    }

    fn handle_help_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.help.scroll_by(-1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.help.scroll_by(1);
            }
            KeyCode::PageUp => {
                self.help.scroll_by(-5);
            }
            KeyCode::PageDown => {
                self.help.scroll_by(5);
            }
            KeyCode::Home => self.help.scroll_to(0),
            KeyCode::End => self.help.scroll_to(self.help.max_offset()),
            _ => {}
        }
    }

    fn close_overlay(&mut self) {
        match self.mode {
            AppMode::Views if self.views_overlay.naming.is_some() => {
                self.views_overlay.naming = None;
            }
            AppMode::Prompt => self.close_prompt(),
            AppMode::Form => self.cancel_form(),
            AppMode::ConfirmDelete => self.cancel_delete(),
            AppMode::AssigneePicker => self.close_picker(self.assignee_picker.scope),
            AppMode::NodePicker => self.close_picker(self.node_picker.scope),
            AppMode::TypePicker => self.close_picker(EditScope::Form(self.type_picker.field)),
            AppMode::Facets => self.mode = AppMode::Browse,
            AppMode::Filter if self.filter_overlay.showing_values => {
                self.filter_overlay.showing_values = false;
                self.filter_overlay.value_index = 0;
                self.filter_overlay.scroll.scroll_to(0);
            }
            AppMode::Browse | AppMode::Search => {}
            _ => self.mode = AppMode::Browse,
        }
        self.pointer.clear_selection();
    }

    fn move_focused(&mut self, delta: isize) {
        match self.focus {
            Focus::Tickets => self.move_selection(delta),
            Focus::Family => self.move_family_cursor(delta),
            Focus::Details => self.scroll_details(delta),
        }
    }

    fn scroll_details(&mut self, delta: isize) {
        let delta = i32::try_from(delta).unwrap_or(if delta.is_negative() {
            i32::MIN
        } else {
            i32::MAX
        });
        self.details.scroll_by(delta);
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Tickets => Focus::Details,
            Focus::Family => Focus::Details,
            Focus::Details => Focus::Tickets,
        };
        self.narrow_details = self.focus.is_details_pane();
    }

    fn toggle_narrow_details(&mut self) {
        self.narrow_details = !self.narrow_details;
        if self.narrow_details {
            if !self.focus.is_details_pane() {
                self.focus = Focus::Details;
            }
        } else {
            self.focus = Focus::Tickets;
        }
    }

    fn set_notification(
        &mut self,
        message: impl Into<String>,
        level: NotificationLevel,
        duration: Duration,
    ) {
        self.notification = Some(Notification {
            message: message.into(),
            level,
            expires_at: Instant::now() + duration,
        });
    }

    fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let current = self.table_state.selected().unwrap_or_default();
        let next = current
            .saturating_add_signed(delta)
            .min(self.visible.len() - 1);
        self.select_row(next);
    }

    fn select_row(&mut self, row: usize) {
        if self.visible.is_empty() {
            self.table_state.select(None);
            self.table.offset = 0;
        } else {
            let row = row.min(self.visible.len() - 1);
            self.table_state.select(Some(row));
            self.table.ensure_visible(row);
        }
        self.details.scroll_to(0);
        self.sync_family_state();
    }

    fn visible_row(&self, key: &TicketKey) -> Option<usize> {
        self.visible
            .iter()
            .position(|entry| self.tickets[entry.ticket_index].key == *key)
    }

    fn jump_to_ticket(&mut self, key: &TicketKey) {
        if self
            .selected_ticket()
            .is_some_and(|ticket| ticket.key == *key)
        {
            return;
        }
        let Some(row) = self.visible_row(key) else {
            if let Some(ticket) = self.ticket_by_key(key) {
                // The family tree shows finished relatives whether or not the
                // table does, so say which of the two is in the way.
                let reason = if self.finished_hidden() && StateCategory::of(&ticket.state).is_done()
                {
                    "finished, and finished tickets are hidden"
                } else {
                    "hidden by the current search"
                };
                self.set_status(format!("{id} is {reason}", id = key.id));
            } else {
                self.set_error(format!("{id} is not in this database", id = key.id));
            }
            return;
        };
        self.record_history();
        self.select_row(row);
        self.record_history();
    }

    fn open_selected(&self) -> AppAction {
        self.selected_ticket().map_or(AppAction::None, |ticket| {
            AppAction::OpenUrl(ticket.web_url.clone())
        })
    }
}

const fn mode_name(mode: AppMode) -> &'static str {
    match mode {
        AppMode::Browse => "browse",
        AppMode::Search => "search",
        AppMode::Sort => "sort",
        AppMode::Help => "help",
        AppMode::Filter => "filter",
        AppMode::Columns => "columns",
        AppMode::Palette => "palette",
        AppMode::Views => "views",
        AppMode::Info => "info",
        AppMode::Sprint => "sprint",
        AppMode::Facets => "facets",
        AppMode::Edit => "edit",
        AppMode::StatePicker => "state-picker",
        AppMode::PriorityPicker => "priority-picker",
        AppMode::Prompt => "prompt",
        AppMode::AssigneePicker => "assignee-picker",
        AppMode::NodePicker => "node-picker",
        AppMode::Form => "form",
        AppMode::TypePicker => "type-picker",
        AppMode::ParentPicker => "parent-picker",
        AppMode::ConfirmDelete => "confirm-delete",
    }
}

const fn focus_name(focus: Focus) -> &'static str {
    match focus {
        Focus::Tickets => "tickets",
        Focus::Family => "family",
        Focus::Details => "details",
    }
}

/// Holds a threshold at or above the one-day floor, wherever it came from: a
/// flag, a variable, or a session file written by hand.
const fn clamp_stale_days(days: u16) -> u16 {
    if days < MIN_STALE_DAYS {
        MIN_STALE_DAYS
    } else {
        days
    }
}

/// Turns a divider position, measured in cells from the start of the workspace,
/// into a percentage for the first pane. The clamp keeps `first_min` cells for
/// that pane and `second_min` cells plus the one-cell divider for the other,
/// then holds the result inside the 20..=80 safety rails.
fn split_percent(cells: u16, span: u16, first_min: u16, second_min: u16) -> u16 {
    if span == 0 {
        return MIN_SPLIT_PERCENT;
    }
    let span = u32::from(span);
    let low = (u32::from(first_min) * 100)
        .div_ceil(span)
        .clamp(u32::from(MIN_SPLIT_PERCENT), u32::from(MAX_SPLIT_PERCENT));
    let high = (span.saturating_sub(u32::from(second_min) + 1) * 100 / span)
        .min(u32::from(MAX_SPLIT_PERCENT))
        .max(low);
    let percent = u32::from(cells) * 100 / span;
    u16::try_from(percent.clamp(low, high)).unwrap_or(MIN_SPLIT_PERCENT)
}

fn clamp_pos_to_snapshot(
    snapshot: &crate::pointer::SelectableSnapshot,
    column: u16,
    row: u16,
) -> Option<TextPos> {
    if snapshot.cells.is_empty() {
        return None;
    }
    let line = if row < snapshot.rect.y {
        0
    } else {
        usize::from(row.saturating_sub(snapshot.rect.y)).min(snapshot.cells.len() - 1)
    };
    let width = snapshot.cells[line].len();
    let col = if column < snapshot.rect.x {
        0
    } else {
        usize::from(column.saturating_sub(snapshot.rect.x)).min(width)
    };
    Some(TextPos { line, col })
}
