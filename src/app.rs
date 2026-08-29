use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;

use crate::agent_context::{
    AgentContext, SearchContext, SortContext, TicketContext, TicketReference, TicketsContext,
};
use crate::classification::{self, ClassificationNode, NodeKind};
use crate::columns::TableLayout;
use crate::command::{Command, CommandId, EDIT_MENU, command_for_key, matching_commands};
pub use crate::edit::FieldEdit;
use crate::edit::{EditApplied, EditRejection, EditRequest, normalize_tags};
use crate::export;
pub use crate::filter::FacetTarget;
use crate::filter::{
    FacetValue, FilterField, FilterToken, ParsedQuery, facet_values, format_query, parse_query,
};
use crate::model::{
    CommentRecord, DetailsUpdate, FamilySnapshot, FamilyTreeEntry, HistoryRecord, Identity,
    RelationRecord, SortDirection, SortField, StateCatalog, StateOption, Ticket, TicketGraph,
    TicketKey, compare_tickets, path_leaf,
};
pub use crate::model::{RowDensity, SearchOrder};
use crate::pointer::{
    self, DragKind, PointerState, ScrollState, ScrollSurface, SelectableSurface, TextEditor,
    TextPos, TextSelection,
};
pub use crate::pointer::{HitRegions, PointerTarget};
use crate::search::{SearchDocuments, SearchEngine, SearchMatch};
use crate::session::{self, NamedView, Session};
use crate::text_input::TextInput;
use crate::timestamp::Timestamp;

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
}

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

/// Which way the draggable pane divider runs in the current layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DividerOrientation {
    /// A column between the tickets and details panes (wide layout).
    Vertical,
    /// A row between the stacked tickets and details panes.
    Horizontal,
}

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
    /// Write one field of one work item back to Azure DevOps.
    Edit(EditRequest),
    /// Read the project's team members, so the assignee picker can offer
    /// somebody with no work item in the database yet. Asked for once a
    /// session, when that picker first opens; the picker does not wait on it.
    FetchIdentities,
    /// Read the project's iteration and area trees, so both node pickers can
    /// offer a sprint no work item sits in yet. Asked for once a session, when
    /// either picker first opens on a cache that is empty or stale; the picker
    /// does not wait on it.
    FetchClassificationNodes,
    /// Leave one comment on one work item. Nothing appears on the work item
    /// until Azure DevOps has stored it, so this is the one write the table
    /// does not make optimistically.
    Comment {
        key: TicketKey,
        text: String,
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

#[derive(Debug)]
pub struct PointerUpdate {
    pub action: AppAction,
    pub redraw: bool,
}

impl PointerUpdate {
    fn none(redraw: bool) -> Self {
        Self {
            action: AppAction::None,
            redraw,
        }
    }

    fn action(action: AppAction) -> Self {
        Self {
            action,
            redraw: true,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SortDraft {
    pub field_index: usize,
    pub direction: SortDirection,
}

#[derive(Clone, Debug, Default)]
pub struct FilterOverlay {
    pub field_index: usize,
    pub value_index: usize,
    pub showing_values: bool,
    pub scroll: ScrollState,
}

#[derive(Clone, Debug, Default)]
pub struct FacetBar {
    pub field_index: usize,
    pub value_index: usize,
    pub scroll: ScrollState,
}

#[derive(Clone, Debug, Default)]
pub struct ColumnOverlay {
    pub index: usize,
    pub scroll: ScrollState,
}

#[derive(Clone, Debug, Default)]
pub struct PaletteState {
    pub query: TextInput,
    pub selected: usize,
    pub scroll: ScrollState,
}

/// The Edit menu's cursor. The entries themselves are [`EDIT_MENU`].
#[derive(Clone, Debug, Default)]
pub struct EditMenu {
    pub index: usize,
    pub scroll: ScrollState,
}

/// The state picker, built when it opens so it never reads the network.
#[derive(Clone, Debug, Default)]
pub struct StatePicker {
    /// Every state the selected work item's type allows.
    pub options: Vec<StateOption>,
    pub index: usize,
    pub scroll: ScrollState,
    /// The state the work item is already in, which `Enter` treats as a no-op.
    pub current: String,
    /// The work item the picker was opened for, shown in its title.
    pub id: i64,
}

/// The priorities the picker offers, in the order it lists them. `None` is the
/// `Clear` row, which takes the field off the work item rather than writing an
/// empty value.
pub const PRIORITY_CHOICES: [Option<i64>; 5] = [Some(1), Some(2), Some(3), Some(4), None];

/// The priority picker, built when it opens from the row it was opened on.
#[derive(Clone, Debug, Default)]
pub struct PriorityPicker {
    pub index: usize,
    pub scroll: ScrollState,
    /// The priority the work item already has, which `Enter` treats as a no-op.
    pub current: Option<i64>,
    /// The work item the picker was opened for, shown in its title.
    pub id: i64,
}

/// Which field a [`TextPrompt`] is editing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptField {
    Title,
    Tags,
    /// A new comment on the work item, which starts empty rather than
    /// prefilled: there is nothing to edit, only something to say.
    Comment,
}

impl PromptField {
    /// What the prompt calls the field, in its title and its notifications.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Tags => "Tags",
            Self::Comment => "Comment",
        }
    }

    /// What the prompt's frame says, which always names the work item it is
    /// for. A comment is left on a work item rather than in a field of it, so
    /// it reads that way.
    #[must_use]
    pub fn title(self, id: i64) -> String {
        match self {
            Self::Comment => format!("Comment on #{id}"),
            other => format!("{} \u{b7} #{id}", other.label()),
        }
    }

    /// What the footer says while the prompt is open.
    #[must_use]
    pub const fn hint(self) -> &'static str {
        match self {
            Self::Title => "Type a title  Enter save  Esc cancel",
            Self::Tags => "Semicolon separated  Enter save  Esc cancel",
            Self::Comment => "Type a comment  Enter post  Esc cancel",
        }
    }
}

/// What the assignee picker calls nobody at all, and the row that unassigns.
pub const UNASSIGNED_LABEL: &str = "Unassigned";

/// One row of the assignee picker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssigneeCandidate {
    /// The name the row shows and the Assignee cell reads after the write.
    pub display: String,
    /// The sign-in address a write is best addressed to, when one is known.
    pub unique: Option<String>,
    /// Whether choosing this row takes the work item off whoever holds it.
    pub unassigned: bool,
    /// Whether this is the signed-in user, which the row says out loud.
    pub me: bool,
}

impl AssigneeCandidate {
    /// Whether this row is who the work item is assigned to already, which the
    /// picker marks and `Enter` treats as a no-op.
    #[must_use]
    pub fn is_current(&self, current: Option<&str>) -> bool {
        match current {
            Some(name) => !self.unassigned && same_name(&self.display, name),
            None => self.unassigned,
        }
    }
}

/// The assignee picker: everybody worth offering, filtered by whatever has been
/// typed. Built when it opens, so it never waits for the network.
#[derive(Clone, Debug, Default)]
pub struct AssigneePicker {
    /// Every candidate, in the order they were gathered.
    pub candidates: Vec<AssigneeCandidate>,
    pub query: TextInput,
    /// The cursor, counted over the candidates the query left showing.
    pub index: usize,
    pub scroll: ScrollState,
    /// Who holds the work item now, which `Enter` treats as a no-op.
    pub current: Option<String>,
    /// The work item the picker was opened for, shown in its title.
    pub id: i64,
}

/// How long a cached copy of the classification trees is trusted before either
/// picker asks Azure DevOps for them again. Sprints are added a few times a
/// year, so an hour is generous and still keeps a long-running session honest.
const CLASSIFICATION_MAX_AGE_SECONDS: i64 = 3600;

/// One row of an iteration or area picker: a node of the tree, flattened.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeRow {
    /// The full backslash path the field is written with, such as
    /// `development\Sprint 1`.
    pub path: String,
    /// How far the row is indented: zero for the project root, one for its
    /// children, and so on.
    pub depth: usize,
    /// The days an iteration runs between, as the row shows them, such as
    /// `Aug 25 – Sep 5`. Areas and unscheduled iterations have none.
    pub dates: Option<String>,
    /// Whether today falls inside those days, which the row says out loud.
    pub current_period: bool,
}

impl NodeRow {
    /// A row for a path with no node behind it: the fallback list, and the work
    /// item's own path when the trees no longer name it.
    #[must_use]
    pub fn of(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            depth: path.matches('\\').count(),
            dates: None,
            current_period: false,
        }
    }

    /// The last segment, which is what the row shows and what the table column
    /// and the filters match on.
    #[must_use]
    pub fn leaf(&self) -> &str {
        path_leaf(&self.path)
    }

    /// The two spaces per level that draw the tree.
    #[must_use]
    pub fn indent(&self) -> String {
        "  ".repeat(self.depth)
    }
}

/// The iteration or area picker: the project's tree flattened to indented rows,
/// filtered by whatever has been typed. Built when it opens from the cached
/// nodes, so it never waits for the network.
#[derive(Clone, Debug)]
pub struct NodePicker {
    /// Which tree is open, which is also the field a choice is written to.
    pub kind: NodeKind,
    /// Every row, in tree order.
    pub rows: Vec<NodeRow>,
    pub query: TextInput,
    /// The cursor, counted over the rows the query left showing.
    pub index: usize,
    pub scroll: ScrollState,
    /// The path the work item carries now, which `Enter` treats as a no-op.
    pub current: String,
    /// The work item the picker was opened for, shown in its title.
    pub id: i64,
}

impl Default for NodePicker {
    fn default() -> Self {
        Self {
            kind: NodeKind::Iteration,
            rows: Vec::new(),
            query: TextInput::default(),
            index: 0,
            scroll: ScrollState::default(),
            current: String::new(),
            id: 0,
        }
    }
}

/// Azure DevOps echoes display names back with inconsistent casing and spacing,
/// so two names are the same person when they are the same after both.
#[must_use]
fn same_name(left: &str, right: &str) -> bool {
    left.trim().to_lowercase() == right.trim().to_lowercase()
}

/// Whether one of the people already gathered is this one, so nobody is
/// offered twice under a different spelling.
#[must_use]
fn names_someone_listed(candidates: &[AssigneeCandidate], name: &str) -> bool {
    candidates
        .iter()
        .any(|candidate| !candidate.unassigned && same_name(&candidate.display, name))
}

/// Whether every character typed appears in `haystack` in that order, ignoring
/// case: `jr` finds `Jacob Ragsdale`, and so does `ragsd`.
#[must_use]
fn fuzzy_contains(haystack: &str, query: &str) -> bool {
    let mut remaining = haystack.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| remaining.any(|found| found == wanted))
}

/// A single-line field editor, prefilled with what the work item says now. The
/// Title and Tags rows of the Edit menu both open one.
#[derive(Clone, Debug)]
pub struct TextPrompt {
    pub field: PromptField,
    pub input: TextInput,
    /// The work item the prompt was opened for, shown in its title.
    pub id: i64,
    /// The text the prompt opened with; saving that back writes nothing.
    pub original: String,
}

#[derive(Clone, Debug, Default)]
pub struct ViewsOverlay {
    pub index: usize,
    pub naming: Option<TextInput>,
    pub scroll: ScrollState,
}

/// An edit waiting on Azure DevOps. `original` is the row as it was before the
/// change, restored if the write is refused; applying `edit` to it gives back
/// the optimistic copy the table is showing, which is how a pull that lands
/// first is topped up again.
#[derive(Clone, Debug)]
struct PendingEdit {
    original: Ticket,
    edit: FieldEdit,
}

impl PendingEdit {
    fn optimistic(&self) -> Ticket {
        let mut ticket = self.original.clone();
        self.edit.apply(&mut ticket);
        ticket
    }
}

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
    pub row_density: RowDensity,
    pub sort_field: SortField,
    pub sort_direction: SortDirection,
    pub layout: TableLayout,
    pub mode: AppMode,
    pub focus: Focus,
    pub table_state: TableState,
    pub table: ScrollState,
    pub details: ScrollState,
    pub family_cursor: Option<TicketKey>,
    pub help: ScrollState,
    pub sort: ScrollState,
    pub narrow_details: bool,
    pub pane_split_wide: u16,
    pub pane_split_stacked: u16,
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
    pub facet_bar: FacetBar,
    pub edit_menu: EditMenu,
    pub state_picker: StatePicker,
    pub priority_picker: PriorityPicker,
    pub assignee_picker: AssigneePicker,
    pub node_picker: NodePicker,
    /// The open single-line field editor, if there is one.
    pub prompt: Option<TextPrompt>,
    bookmarks: HashSet<TicketKey>,
    selected_keys: HashSet<TicketKey>,
    recent: Vec<TicketKey>,
    future: Vec<TicketKey>,
    views: Vec<NamedView>,
    pub active_view: Option<String>,
    graph: TicketGraph,
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
    /// Work items with a comment posted and not answered yet. A comment is not
    /// optimistic, so this is only what stops a second one being typed on top
    /// of the first.
    pending_comments: HashSet<TicketKey>,
    /// Why there is nothing to write to, reported when an edit is attempted
    /// without a configured Azure DevOps project.
    offline_reason: Option<String>,
    /// Whether Azure DevOps is configured at all: an offline run browses the
    /// database and reports no sync state.
    sync_enabled: bool,
    /// When the last successful pull finished, which is not `loaded_at`: a
    /// SQLite reload moves that too.
    synced_at: Option<Instant>,
    /// The last pull's error, kept so the same timer failure is reported once.
    sync_error: Option<String>,
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
            row_density: RowDensity::Compact,
            sort_field: SortField::Changed,
            sort_direction: SortDirection::Descending,
            layout: TableLayout::default(),
            mode: AppMode::Browse,
            focus: Focus::Tickets,
            table_state: TableState::default(),
            table: ScrollState::default(),
            details: ScrollState::default(),
            family_cursor: None,
            help: ScrollState::default(),
            sort: ScrollState::default(),
            narrow_details: false,
            pane_split_wide: DEFAULT_PANE_SPLIT_WIDE,
            pane_split_stacked: DEFAULT_PANE_SPLIT_STACKED,
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
            facet_bar: FacetBar::default(),
            edit_menu: EditMenu::default(),
            state_picker: StatePicker::default(),
            priority_picker: PriorityPicker::default(),
            assignee_picker: AssigneePicker::default(),
            node_picker: NodePicker::default(),
            prompt: None,
            bookmarks: HashSet::new(),
            selected_keys: HashSet::new(),
            recent: Vec::new(),
            future: Vec::new(),
            views: Vec::new(),
            active_view: None,
            graph: prepared.graph,
            state_catalog: prepared.states,
            loaded_at: Instant::now(),
            database_path: PathBuf::new(),
            stale: false,
            data_signature: 0,
            sync_pending: false,
            details_pending: None,
            pending_edits: HashMap::new(),
            pending_comments: HashSet::new(),
            offline_reason: None,
            sync_enabled: false,
            synced_at: None,
            sync_error: None,
            me: None,
            identities: Vec::new(),
            identities_requested: false,
            classification_nodes: Vec::new(),
            classification_fetched_at: None,
            classification_requested: false,
        };
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

    #[must_use]
    pub fn agent_context(&self) -> AgentContext {
        let parsed = self.parsed_query();
        let visible_rows = self
            .visible_tickets()
            .skip(self.table.offset)
            .take(self.table.viewport)
            .map(|ticket| self.ticket_context(ticket))
            .collect();
        let checked_tickets = self
            .tickets()
            .iter()
            .filter(|ticket| self.selected_keys.contains(&ticket.key))
            .map(|ticket| self.ticket_context(ticket))
            .collect();
        AgentContext {
            database_path: self.database_path.display().to_string(),
            me: self.me.clone(),
            mode: mode_name(self.mode).into(),
            focus: focus_name(self.focus).into(),
            screen: if self.narrow_details {
                "details"
            } else {
                "workspace"
            }
            .into(),
            active_view: self.active_view.clone(),
            search: SearchContext {
                query: self.query.text().to_owned(),
                fuzzy_text: parsed.fuzzy,
                filters: parsed
                    .filters
                    .tokens()
                    .into_iter()
                    .map(|token| token.chip_label())
                    .collect(),
                pending: self.search_pending,
                order: self.search_order,
            },
            sort: SortContext {
                field: self.sort_field,
                direction: self.sort_direction,
                row_density: self.row_density,
            },
            tickets: TicketsContext {
                total_count: self.tickets.len(),
                matching_count: self.visible.len(),
                viewport_start: self.table.offset,
                viewport_size: self.table.viewport,
                visible_rows,
            },
            selected_ticket: self
                .selected_ticket()
                .map(|ticket| self.ticket_context(ticket)),
            checked_tickets,
            family_cursor: self.family_cursor.as_ref().map(|key| TicketReference {
                organization: key.organization.clone(),
                id: key.id,
            }),
            details_scroll_line: u16::try_from(self.details.offset).unwrap_or(u16::MAX),
        }
    }

    fn ticket_context(&self, ticket: &Ticket) -> TicketContext {
        TicketContext {
            organization: ticket.key.organization.clone(),
            project: ticket.project.clone(),
            id: ticket.key.id,
            work_item_type: ticket.work_item_type.clone(),
            title: ticket.title.clone(),
            state: ticket.state.clone(),
            assigned_to: ticket.assigned_to.clone(),
            priority: ticket.priority,
            tags: ticket.tags.clone(),
            web_url: ticket.web_url.clone(),
            bookmarked: self.bookmarks.contains(&ticket.key),
            checked: self.selected_keys.contains(&ticket.key),
        }
    }

    #[must_use]
    pub fn query(&self) -> &str {
        self.query.text()
    }

    #[must_use]
    pub const fn query_cursor(&self) -> usize {
        self.query.cursor()
    }

    #[must_use]
    pub fn parsed_query(&self) -> ParsedQuery {
        parse_query(self.query.text())
    }

    #[must_use]
    pub fn fuzzy_query(&self) -> String {
        self.parsed_query().fuzzy
    }

    #[must_use]
    pub fn filter_tokens(&self) -> Vec<FilterToken> {
        self.parsed_query().filters.tokens()
    }

    #[must_use]
    pub fn overflow_filter_tokens(&self) -> Vec<FilterToken> {
        self.filter_tokens()
            .into_iter()
            .filter(|token| match token {
                FilterToken::Bookmarked => true,
                FilterToken::Field { field, .. } => !field.on_bar(),
            })
            .collect()
    }

    #[must_use]
    pub fn facets_for(&self, field: FilterField) -> Vec<FacetValue> {
        let filters = self.parsed_query().filters;
        facet_values(self.tickets(), &filters, field, |ticket| {
            self.bookmarks.contains(&ticket.key)
        })
    }

    pub fn toggle_filter(&mut self, field: FilterField, value: &str) {
        let mut parsed = self.parsed_query();
        parsed.filters.toggle(field, value);
        self.set_query(format_query(&parsed.filters, &parsed.fuzzy));
    }

    #[must_use]
    pub fn is_bookmarked(&self, key: &TicketKey) -> bool {
        self.bookmarks.contains(key)
    }

    #[must_use]
    pub fn is_row_selected(&self, key: &TicketKey) -> bool {
        self.selected_keys.contains(key)
    }

    pub fn set_me(&mut self, me: Option<String>) {
        self.me = me;
    }

    #[must_use]
    pub fn me(&self) -> Option<&str> {
        self.me.as_deref()
    }

    /// Whether a work item is assigned to the signed-in user. Azure DevOps
    /// echoes display names back with inconsistent casing, so compare loosely.
    #[must_use]
    pub fn is_mine(&self, ticket: &Ticket) -> bool {
        match (self.me.as_deref(), ticket.assigned_to.as_deref()) {
            (Some(me), Some(assignee)) => me
                .trim()
                .chars()
                .flat_map(char::to_lowercase)
                .eq(assignee.trim().chars().flat_map(char::to_lowercase)),
            _ => false,
        }
    }

    /// The people a previous session cached, read out of the database as the
    /// TUI opens so the first assignee picker of the run is already complete.
    pub fn set_identities(&mut self, identities: Vec<Identity>) {
        self.identities = identities;
    }

    #[must_use]
    pub fn identities(&self) -> &[Identity] {
        &self.identities
    }

    /// Folds the project's team members into the people the picker offers, and
    /// into an open picker, so a list that opened without them fills in where
    /// it stands rather than closing and reopening. A name already held keeps
    /// its place and only gains an address it was missing.
    pub fn merge_identities(&mut self, identities: Vec<Identity>) {
        if identities.is_empty() {
            return;
        }
        for identity in identities {
            match self
                .identities
                .iter_mut()
                .find(|known| same_name(&known.display_name, &identity.display_name))
            {
                Some(known) if known.unique_name.is_none() => {
                    known.unique_name = identity.unique_name;
                }
                Some(_) => {}
                None => self.identities.push(identity),
            }
        }
        if self.mode != AppMode::AssigneePicker {
            return;
        }
        let focused = self
            .assignee_matches()
            .get(self.assignee_picker.index)
            .map(|candidate| candidate.display.clone());
        self.assignee_picker.candidates = self.assignee_candidates();
        let matches = self.assignee_matches();
        let index = focused
            .and_then(|display| {
                matches
                    .iter()
                    .position(|candidate| candidate.display == display)
            })
            .unwrap_or(self.assignee_picker.index)
            .min(matches.len().saturating_sub(1));
        self.focus_assignee(index);
    }

    #[must_use]
    pub fn views(&self) -> &[NamedView] {
        &self.views
    }

    #[must_use]
    pub fn palette_commands(&self) -> Vec<Command> {
        matching_commands(self.palette.query.text())
    }

    #[must_use]
    pub fn facet_field(&self) -> FilterField {
        FilterField::ALL[self
            .filter_overlay
            .field_index
            .min(FilterField::ALL.len() - 1)]
    }

    #[must_use]
    pub fn current_facets(&self) -> Vec<FacetValue> {
        let filters = self.parsed_query().filters;
        facet_values(self.tickets(), &filters, self.facet_field(), |ticket| {
            self.bookmarks.contains(&ticket.key)
        })
    }

    pub fn configure_database(&mut self, path: PathBuf, signature: u128) {
        self.database_path = path;
        self.data_signature = signature;
        self.loaded_at = Instant::now();
        self.stale = false;
    }

    /// The states Azure DevOps allows for a work item type, cached by a sync.
    pub fn set_state_catalog(&mut self, catalog: StateCatalog) {
        self.state_catalog = catalog;
    }

    /// What the state picker offers for one work item type: the states Azure
    /// DevOps listed for it, in the order its process template runs them.
    ///
    /// Until a sync has cached those, the states already in the database stand
    /// in, ordered by category and then by name, so the picker still opens on a
    /// database that has never seen the states endpoint.
    #[must_use]
    pub fn states_for(&self, work_item_type: &str) -> Vec<StateOption> {
        let cached = self.state_catalog.states_for(work_item_type);
        if !cached.is_empty() {
            return cached.to_vec();
        }
        let mut seen: Vec<StateOption> = Vec::new();
        for ticket in self
            .tickets
            .iter()
            .filter(|ticket| ticket.work_item_type == work_item_type)
        {
            if !seen.iter().any(|state| state.name == ticket.state) {
                seen.push(StateOption::of(&ticket.state));
            }
        }
        seen.sort_by(|left, right| {
            left.category
                .rank()
                .cmp(&right.category.rank())
                .then_with(|| left.name.cmp(&right.name))
        });
        seen
    }

    pub fn set_workspace_graph(&mut self, graph: crate::model::TicketGraph) {
        self.graph = graph;
        self.sync_family_state();
    }

    pub fn mark_stale(&mut self) {
        self.stale = true;
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
        self.synced_at = Some(Instant::now());
    }

    /// A pull failed. Reports whether the failure is worth a notification: the
    /// same error on consecutive timer pulls is not, because the table title
    /// already says the sync is failing. `announce` forces one anyway, for a
    /// pull the user asked for.
    pub fn fail_sync(&mut self, error: &str, announce: bool) -> bool {
        self.sync_pending = false;
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

    /// The database overlay's one-line account of the last sync.
    #[must_use]
    pub fn sync_summary(&self) -> String {
        if !self.sync_enabled {
            return "offline; no Azure DevOps organization configured".into();
        }
        let last = self
            .synced_at
            .map_or_else(|| "not yet".to_owned(), |at| relative_age(at.elapsed()));
        if self.sync_pending {
            format!("in progress, last {last}")
        } else if let Some(error) = &self.sync_error {
            format!("failed, last {last}: {error}")
        } else {
            last
        }
    }

    #[must_use]
    pub fn ticket_by_key(&self, key: &TicketKey) -> Option<&Ticket> {
        self.tickets.iter().find(|ticket| ticket.key == *key)
    }

    #[must_use]
    pub fn ticket_title(&self, key: &TicketKey) -> Option<&str> {
        self.ticket_by_key(key).map(|ticket| ticket.title.as_str())
    }

    #[must_use]
    pub fn relations_from(&self, key: &TicketKey) -> Vec<&RelationRecord> {
        self.graph.relations_from(key)
    }

    #[must_use]
    pub fn family_of(&self, key: &TicketKey) -> FamilySnapshot {
        self.graph.family(key)
    }

    #[must_use]
    pub fn selected_family(&self) -> Option<FamilySnapshot> {
        Some(self.family_of(&self.selected_ticket()?.key))
    }

    #[must_use]
    pub fn selected_has_family(&self) -> bool {
        self.selected_family()
            .is_some_and(|family| family.has_family())
    }

    #[must_use]
    pub fn visible_family_tree(&self) -> Vec<FamilyTreeEntry> {
        self.selected_ticket()
            .map(|ticket| self.graph.visible_family_tree(&ticket.key))
            .unwrap_or_default()
    }

    #[must_use]
    pub fn comments_for(&self, key: &TicketKey) -> Vec<&CommentRecord> {
        self.graph.comments_for(key)
    }

    #[must_use]
    pub fn history_for(&self, key: &TicketKey) -> Vec<&HistoryRecord> {
        self.graph.history_for(key)
    }

    /// Swaps in the comments and history just read for one work item, leaving
    /// every other work item's alone, and records the revision they were read
    /// at so the pane stops asking. Nothing else about the row moves: this is
    /// what keeps a details fetch from costing a reload.
    pub fn apply_details(&mut self, update: DetailsUpdate) {
        self.graph.replace_details(&update.key, update.details);
        if let Some(index) = self.index_of(&update.key) {
            Arc::make_mut(&mut self.tickets)[index].details_rev = update.revision;
        }
    }

    pub fn replace_tickets(&mut self, tickets: Vec<Ticket>) {
        self.replace_prepared_tickets(PreparedTickets::new(tickets));
    }

    pub fn replace_prepared_tickets(&mut self, prepared: PreparedTickets) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.tickets = Arc::new(prepared.tickets);
        self.graph = prepared.graph;
        // A pull that has not cached the states yet must not throw away the
        // ones an earlier pull did.
        if !prepared.states.is_empty() {
            self.state_catalog = prepared.states;
        }
        self.search.replace_documents(prepared.search_documents);
        self.reapply_pending_edits();
        self.loaded_at = Instant::now();
        self.stale = false;
        if self.fuzzy_query().is_empty() {
            self.show_all(selected.as_ref());
        } else {
            self.pending_selection = selected;
            self.visible.clear();
            self.table_state.select(None);
            self.submit_search();
        }
    }

    /// Asks for one field of the selected work item to be written back to
    /// Azure DevOps. The row carries the change at once, so the table never
    /// waits for the network; the action this returns is what actually sends
    /// it, and a refusal puts the row back. Every edit feature goes this way.
    pub fn edit_selected(&mut self, edit: FieldEdit) -> AppAction {
        let Some(key) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            self.set_error("No work item is selected");
            return AppAction::None;
        };
        self.edit_ticket(&key, edit)
    }

    /// [`Self::edit_selected`] for a work item that is not the selected row.
    pub fn edit_ticket(&mut self, key: &TicketKey, edit: FieldEdit) -> AppAction {
        let refusal = |reason: &str| format!("#{} {} not saved: {reason}", key.id, edit.label());
        if !self.sync_enabled {
            // Nothing to write to, so the row is left exactly as it is.
            let reason = self
                .offline_reason
                .clone()
                .unwrap_or_else(|| "no Azure DevOps organization is configured".to_owned());
            let message = refusal(&reason);
            self.set_error(message);
            return AppAction::None;
        }
        if self.pending_edits.contains_key(key) {
            // The revision a second edit would test with is already stale, so
            // it could only earn a conflict.
            let message = refusal("an earlier edit is still in flight");
            self.set_error(message);
            return AppAction::None;
        }
        let Some(index) = self.index_of(key) else {
            let message = refusal("it is not in this database");
            self.set_error(message);
            return AppAction::None;
        };
        let pending = PendingEdit {
            original: self.tickets[index].clone(),
            edit: edit.clone(),
        };
        let request = EditRequest {
            key: key.clone(),
            expected_revision: pending.original.revision,
            edit,
        };
        self.set_ticket(index, pending.optimistic());
        self.pending_edits.insert(key.clone(), pending);
        AppAction::Edit(request)
    }

    /// Whether an edit is waiting on Azure DevOps. The database watcher stands
    /// down while one is, because the sync worker is writing that row itself.
    #[must_use]
    pub fn edits_pending(&self) -> bool {
        !self.pending_edits.is_empty()
    }

    /// Swaps in the copy Azure DevOps stored, so the row shows the revision and
    /// changed date the server settled on rather than the optimistic guess.
    pub fn apply_edit(&mut self, applied: EditApplied) {
        let key = applied.ticket.key.clone();
        self.pending_edits.remove(&key);
        self.graph.replace_relations_from(&key, applied.relations);
        if let Some(index) = self.index_of(&key) {
            self.set_ticket(index, applied.ticket);
            self.resettle_rows();
        }
        self.set_status(format!("Updated #{} · {}", key.id, applied.edit.summary()));
    }

    /// Puts a refused edit back the way it was and says which field did not
    /// save, so a change is never dropped quietly.
    pub fn reject_edit(&mut self, rejection: &EditRejection) {
        if let Some(pending) = self.pending_edits.remove(&rejection.key)
            && let Some(index) = self.index_of(&rejection.key)
        {
            self.set_ticket(index, pending.original);
        }
        self.set_error(rejection.notification());
    }

    /// Why the TUI cannot write anything, told to whoever tries to.
    pub fn set_offline_reason(&mut self, reason: Option<String>) {
        self.offline_reason = reason;
    }

    fn index_of(&self, key: &TicketKey) -> Option<usize> {
        self.tickets.iter().position(|ticket| ticket.key == *key)
    }

    /// Replaces one work item in place, keeping its search document in step so
    /// the next query sees the new value.
    fn set_ticket(&mut self, index: usize, ticket: Ticket) {
        Arc::make_mut(&mut self.tickets)[index] = ticket;
        self.search.update_document(index, &self.tickets[index]);
    }

    /// Re-applies the filters and the sort to the rows already on screen, for
    /// when one of them changed under the current ordering. The selection
    /// follows its work item rather than its row number.
    fn resettle_rows(&mut self) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.apply_filters();
        self.sort_visible();
        self.restore_selection(selected.as_ref());
    }

    /// Puts the optimistic copies back on top of a pull that finished while an
    /// edit was still in flight, so an edited row does not flicker back to the
    /// value the pull brought. That pulled row becomes what a refusal restores,
    /// because it is the freshest copy the edit did not make.
    fn reapply_pending_edits(&mut self) {
        if self.pending_edits.is_empty() {
            return;
        }
        let keys: Vec<TicketKey> = self.pending_edits.keys().cloned().collect();
        for key in keys {
            let Some(index) = self.index_of(&key) else {
                continue;
            };
            let pulled = self.tickets[index].clone();
            let Some(pending) = self.pending_edits.get_mut(&key) else {
                continue;
            };
            pending.original = pulled;
            let optimistic = pending.optimistic();
            self.set_ticket(index, optimistic);
        }
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

    pub fn poll_search(&mut self) -> bool {
        let Some(result) = self.search.try_result() else {
            return false;
        };
        if result.generation != self.search_generation || self.fuzzy_query().is_empty() {
            return false;
        }

        let selected = self
            .pending_selection
            .take()
            .or_else(|| self.selected_ticket().map(|ticket| ticket.key.clone()));
        self.visible = result.matches;
        self.apply_filters();
        self.sort_visible();
        self.restore_selection(selected.as_ref());
        self.search_pending = false;
        true
    }

    pub fn set_query(&mut self, query: String) {
        if self.query.text() == query {
            self.query.move_end();
            return;
        }
        self.query.set_text(query);
        self.after_query_edit();
    }

    pub fn handle_paste(&mut self, pasted: &str) {
        match self.active_editor() {
            Some(TextEditor::Search) => self.edit_query(|query| query.paste(pasted, true)),
            Some(TextEditor::Palette) => {
                self.edit_palette_query(|query| query.paste(pasted, true));
            }
            Some(TextEditor::ViewName) => {
                if let Some(name) = self.views_overlay.naming.as_mut() {
                    name.paste(pasted, false);
                }
            }
            Some(TextEditor::Prompt) => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.input.paste(pasted, false);
                }
            }
            Some(TextEditor::Assignee) => self.assignee_picker.query.paste(pasted, false),
            Some(TextEditor::Node) => self.node_picker.query.paste(pasted, false),
            None => {}
        }
    }

    /// Runs one edit against the search field and re-runs the search when the text
    /// actually changed; a bare caret move leaves the results alone.
    fn edit_query(&mut self, edit: impl FnOnce(&mut TextInput)) {
        let before = self.query.text().to_owned();
        edit(&mut self.query);
        if self.query.text() != before {
            self.after_query_edit();
        }
    }

    fn after_query_edit(&mut self) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.search_history_index = None;
        self.search_history_draft = self.query.text().to_owned();
        self.session_dirty = true;
        if self.fuzzy_query().is_empty() {
            self.search_generation = self.search_generation.wrapping_add(1);
            self.search_pending = false;
            self.pending_selection = None;
            self.show_all(selected.as_ref());
        } else {
            self.pending_selection = selected;
            self.submit_search();
        }
    }

    /// Runs one edit against the palette filter and restarts the command list when
    /// the text changed.
    fn edit_palette_query(&mut self, edit: impl FnOnce(&mut TextInput)) {
        let before = self.palette.query.text().to_owned();
        edit(&mut self.palette.query);
        if self.palette.query.text() != before {
            self.palette.selected = 0;
            self.palette.scroll.scroll_to(0);
        }
    }

    fn active_editor(&self) -> Option<TextEditor> {
        match self.mode {
            AppMode::Search => Some(TextEditor::Search),
            AppMode::Palette => Some(TextEditor::Palette),
            AppMode::Views if self.views_overlay.naming.is_some() => Some(TextEditor::ViewName),
            AppMode::Prompt => Some(TextEditor::Prompt),
            AppMode::AssigneePicker => Some(TextEditor::Assignee),
            AppMode::NodePicker => Some(TextEditor::Node),
            _ => None,
        }
    }

    pub fn set_table_viewport(&mut self, rows: usize) {
        self.table.set_viewport(rows, self.visible.len());
    }

    /// The scroll bookkeeping for one surface. The table measures its content from
    /// the visible rows, so that length is refreshed on the way out.
    #[must_use]
    pub fn scroll_state(&self, surface: ScrollSurface) -> ScrollState {
        match surface {
            ScrollSurface::Table => ScrollState {
                content: self.visible.len(),
                ..self.table
            },
            ScrollSurface::Details => self.details,
            ScrollSurface::Help => self.help,
            ScrollSurface::Sort => self.sort,
            ScrollSurface::Filter => self.filter_overlay.scroll,
            ScrollSurface::Columns => self.column_overlay.scroll,
            ScrollSurface::Palette => self.palette.scroll,
            ScrollSurface::Views => self.views_overlay.scroll,
            ScrollSurface::FacetMenu => self.facet_bar.scroll,
            ScrollSurface::EditMenu => self.edit_menu.scroll,
            ScrollSurface::StatePicker => self.state_picker.scroll,
            ScrollSurface::PriorityPicker => self.priority_picker.scroll,
            ScrollSurface::AssigneePicker => self.assignee_picker.scroll,
            ScrollSurface::NodePicker => self.node_picker.scroll,
        }
    }

    pub fn scroll_state_mut(&mut self, surface: ScrollSurface) -> &mut ScrollState {
        if matches!(surface, ScrollSurface::Table) {
            self.table.content = self.visible.len();
        }
        match surface {
            ScrollSurface::Table => &mut self.table,
            ScrollSurface::Details => &mut self.details,
            ScrollSurface::Help => &mut self.help,
            ScrollSurface::Sort => &mut self.sort,
            ScrollSurface::Filter => &mut self.filter_overlay.scroll,
            ScrollSurface::Columns => &mut self.column_overlay.scroll,
            ScrollSurface::Palette => &mut self.palette.scroll,
            ScrollSurface::Views => &mut self.views_overlay.scroll,
            ScrollSurface::FacetMenu => &mut self.facet_bar.scroll,
            ScrollSurface::EditMenu => &mut self.edit_menu.scroll,
            ScrollSurface::StatePicker => &mut self.state_picker.scroll,
            ScrollSurface::PriorityPicker => &mut self.priority_picker.scroll,
            ScrollSurface::AssigneePicker => &mut self.assignee_picker.scroll,
            ScrollSurface::NodePicker => &mut self.node_picker.scroll,
        }
    }

    #[must_use]
    pub fn hovered(&self) -> Option<&PointerTarget> {
        self.pointer.hover.as_ref()
    }

    pub(crate) fn hovered_region(&self) -> Option<&pointer::PointerRegion> {
        let (column, row) = self.pointer.position()?;
        self.hit_regions.resolve(column, row)
    }

    #[must_use]
    pub fn selection(&self) -> Option<TextSelection> {
        self.pointer.selection
    }

    pub fn set_sort(&mut self, field: SortField, direction: SortDirection) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.sort_field = field;
        self.sort_direction = direction;
        self.sort_visible();
        self.restore_selection(selected.as_ref());
        self.session_dirty = true;
    }

    pub fn toggle_row_density(&mut self) {
        self.row_density = self.row_density.toggled();
        self.session_dirty = true;
        self.set_status(format!("Row density: {}", self.row_density.label()));
    }

    pub fn toggle_search_order(&mut self) {
        if self.fuzzy_query().is_empty() {
            return;
        }
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.search_order = self.search_order.toggled();
        self.sort_visible();
        self.restore_selection(selected.as_ref());
        self.session_dirty = true;
        self.set_status(format!("Search order: {}", self.search_order.label()));
    }

    pub fn toggle_sort(&mut self, field: SortField) {
        let direction = if self.sort_field == field {
            self.sort_direction.toggled()
        } else if matches!(
            field,
            SortField::Changed | SortField::Priority | SortField::Created
        ) {
            SortDirection::Descending
        } else {
            SortDirection::Ascending
        };
        self.set_sort(field, direction);
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
            AppMode::Facets => {
                self.handle_facet_key(key);
                AppAction::None
            }
            AppMode::Edit => self.handle_edit_menu_key(key),
            AppMode::StatePicker => self.handle_state_picker_key(key),
            AppMode::PriorityPicker => self.handle_priority_picker_key(key),
            AppMode::Prompt => self.handle_prompt_key(key),
            AppMode::AssigneePicker => self.handle_assignee_picker_key(key),
            AppMode::NodePicker => self.handle_node_picker_key(key),
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> PointerUpdate {
        self.pointer.set_position(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp => self.handle_wheel(mouse.column, mouse.row, -3),
            MouseEventKind::ScrollDown => self.handle_wheel(mouse.column, mouse.row, 3),
            MouseEventKind::Down(MouseButton::Left) => self.handle_press(mouse.column, mouse.row),
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved
                if self.pointer.is_pressed() =>
            {
                self.handle_drag(mouse.column, mouse.row)
            }
            MouseEventKind::Moved => self.handle_hover(mouse.column, mouse.row),
            MouseEventKind::Up(MouseButton::Left) => self.handle_release(mouse.column, mouse.row),
            _ => PointerUpdate::none(false),
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

    fn handle_hover(&mut self, column: u16, row: u16) -> PointerUpdate {
        self.pointer.set_position(column, row);
        PointerUpdate::none(self.refresh_hover())
    }

    pub fn refresh_hover(&mut self) -> bool {
        let hover = self
            .pointer
            .position()
            .and_then(|(column, row)| self.hit_regions.resolve(column, row))
            .map(|region| region.target.clone());
        let changed = hover != self.pointer.hover;
        self.pointer.hover = hover;
        changed
    }

    fn handle_press(&mut self, column: u16, row: u16) -> PointerUpdate {
        let region = self.hit_regions.resolve(column, row).cloned();
        let selectable = self.hit_regions.resolve_selectable(column, row);
        self.pointer.clear_selection();
        if let Some(region) = region {
            let scrollbar = match region.target {
                PointerTarget::ScrollbarThumb { surface } => Some(surface),
                _ => None,
            };
            let selectable = match region.target {
                PointerTarget::PaneDivider => None,
                _ => selectable,
            };
            self.pointer.hover = Some(region.target.clone());
            self.pointer
                .begin_press(region.target, column, row, selectable, scrollbar);
        } else {
            self.pointer.hover = None;
            self.pointer.clear_press();
        }
        PointerUpdate::none(true)
    }

    fn handle_drag(&mut self, column: u16, row: u16) -> PointerUpdate {
        let hover = self
            .hit_regions
            .resolve(column, row)
            .map(|region| region.target.clone());
        let hover_changed = hover != self.pointer.hover;
        self.pointer.hover = hover;
        if !self.pointer.moved_from_origin(column, row)
            && matches!(self.pointer.drag(), DragKind::None)
        {
            return PointerUpdate::none(hover_changed);
        }
        match self.pointer.drag() {
            DragKind::Scrollbar { surface, grab } => {
                self.drag_scrollbar(surface, row, grab);
                PointerUpdate::none(true)
            }
            DragKind::Text => {
                self.update_text_drag(column, row);
                PointerUpdate::none(true)
            }
            DragKind::Divider => {
                self.drag_divider(column, row);
                PointerUpdate::none(true)
            }
            DragKind::Cancelled => PointerUpdate::none(hover_changed),
            DragKind::None => {
                if matches!(
                    self.pointer.press_target(),
                    Some(PointerTarget::PaneDivider)
                ) {
                    self.pointer.set_drag(DragKind::Divider);
                    self.drag_divider(column, row);
                    PointerUpdate::none(true)
                } else if let Some(surface) = self.pointer.press_scrollbar() {
                    let grab = self.scrollbar_grab(surface, self.pointer.press_origin());
                    self.pointer.set_drag(DragKind::Scrollbar { surface, grab });
                    self.drag_scrollbar(surface, row, grab);
                    PointerUpdate::none(true)
                } else if let Some(surface) = self.pointer.press_selectable() {
                    self.pointer.set_drag(DragKind::Text);
                    if let Some(origin) = self.pointer.press_origin()
                        && let Some(snapshot) = self.hit_regions.selectable(surface)
                        && let Some(start) = snapshot.pos_at(origin.0, origin.1)
                    {
                        self.pointer.selection = Some(TextSelection {
                            surface,
                            start,
                            end: start,
                        });
                    }
                    self.update_text_drag(column, row);
                    PointerUpdate::none(true)
                } else {
                    self.pointer.set_drag(DragKind::Cancelled);
                    PointerUpdate::none(hover_changed)
                }
            }
        }
    }

    fn handle_release(&mut self, column: u16, row: u16) -> PointerUpdate {
        let drag = self.pointer.drag();
        let target = self.pointer.press_target().cloned();
        let selection = self.pointer.selection;
        self.pointer.clear_press();
        self.handle_hover(column, row);
        match drag {
            DragKind::Text => {
                if let Some(selection) = selection.filter(|selection| !selection.is_empty())
                    && let Some(snapshot) = self.hit_regions.selectable(selection.surface)
                {
                    let text = pointer::extract_selected_text(snapshot, &selection);
                    if !text.is_empty() {
                        return PointerUpdate::action(AppAction::Copy {
                            text,
                            content: CopiedContent::Text,
                        });
                    }
                }
                PointerUpdate::none(true)
            }
            DragKind::Divider => {
                self.session_dirty = true;
                PointerUpdate::none(true)
            }
            DragKind::Scrollbar { .. } | DragKind::Cancelled => PointerUpdate::none(true),
            DragKind::None => {
                if let Some(target) = target {
                    PointerUpdate::action(self.activate_target(target, column, row))
                } else {
                    PointerUpdate::none(true)
                }
            }
        }
    }

    fn handle_wheel(&mut self, column: u16, row: u16, delta: i32) -> PointerUpdate {
        let hover_changed = self.refresh_hover();
        let Some(surface) = self.hit_regions.resolve_scroll(column, row) else {
            return PointerUpdate::none(hover_changed);
        };
        let changed = self.scroll_surface(surface, delta);
        PointerUpdate::none(changed || hover_changed)
    }

    fn activate_target(&mut self, target: PointerTarget, column: u16, row: u16) -> AppAction {
        match target {
            PointerTarget::SearchField => {
                self.begin_search();
                self.place_caret(TextEditor::Search, column, row);
            }
            PointerTarget::ClearQuery => self.set_query(String::new()),
            PointerTarget::OpenPalette => return self.run_command(CommandId::Palette),
            PointerTarget::OpenHelp => return self.run_command(CommandId::Help),
            PointerTarget::CopyActions => self.open_copy_actions(),
            PointerTarget::CloseOverlay => self.close_overlay(),
            PointerTarget::NarrowTickets => {
                self.narrow_details = false;
                self.focus = Focus::Tickets;
            }
            PointerTarget::NarrowDetails => {
                self.narrow_details = true;
                if !self.focus.is_details_pane() {
                    self.focus = Focus::Details;
                }
            }
            PointerTarget::FocusTickets => {
                self.focus = Focus::Tickets;
                self.narrow_details = false;
            }
            PointerTarget::FocusDetails => {
                self.focus = Focus::Details;
            }
            PointerTarget::TableRow { index } => {
                self.focus = Focus::Tickets;
                self.narrow_details = false;
                if index < self.visible.len() {
                    self.select_row(index);
                    self.record_history();
                }
            }
            PointerTarget::OpenTicket { index } => {
                self.focus = Focus::Tickets;
                self.narrow_details = false;
                if index < self.visible.len() {
                    self.select_row(index);
                    self.record_history();
                    return self.open_selected();
                }
            }
            PointerTarget::ToggleBookmark { index } => {
                if index < self.visible.len() {
                    self.select_row(index);
                    self.toggle_bookmark();
                }
            }
            PointerTarget::ToggleRowSelect { index } => {
                if index < self.visible.len() {
                    self.select_row(index);
                    self.toggle_row_selection();
                }
            }
            PointerTarget::SortHeader(field) => self.toggle_sort(field),
            PointerTarget::OpenSelectedUrl => {
                self.focus = Focus::Details;
                self.narrow_details = true;
                return self.open_selected();
            }
            PointerTarget::JumpToTicket(key) => {
                if self
                    .visible_family_tree()
                    .iter()
                    .any(|entry| entry.key == key)
                {
                    self.focus = Focus::Family;
                    self.family_cursor = Some(key.clone());
                    self.ensure_family_cursor_visible();
                } else if self
                    .selected_family()
                    .is_some_and(|family| family.extra_parents.iter().any(|parent| parent == &key))
                {
                    self.focus = Focus::Family;
                } else {
                    self.focus = Focus::Details;
                }
                self.jump_to_ticket(&key);
            }
            PointerTarget::FacetPill(target) => match target {
                FacetTarget::More => self.open_filters(),
                FacetTarget::Field(field) => {
                    let index = FilterField::BAR
                        .iter()
                        .position(|entry| *entry == field)
                        .unwrap_or_default();
                    self.open_facets(index);
                }
            },
            PointerTarget::FacetValue { index } => {
                self.facet_bar.value_index = index;
                self.toggle_current_bar_facet();
            }
            PointerTarget::DismissFacet => {
                if self.mode == AppMode::Facets {
                    self.mode = AppMode::Browse;
                }
            }
            PointerTarget::RemoveChip(token) => self.remove_filter_token(token),
            PointerTarget::SortChoose(field) => {
                self.toggle_sort(field);
                self.mode = AppMode::Browse;
            }
            PointerTarget::SortSetDirection(direction) => {
                self.sort_draft.direction = direction;
            }
            PointerTarget::FilterRow { index } => {
                if self.filter_overlay.showing_values {
                    self.filter_overlay.value_index = index;
                    self.toggle_current_facet();
                } else {
                    self.filter_overlay.field_index = index;
                    self.filter_overlay.showing_values = true;
                    self.filter_overlay.value_index = 0;
                    self.filter_overlay.scroll.scroll_to(0);
                }
            }
            PointerTarget::ColumnToggle { index } => {
                self.column_overlay.index = index;
                self.layout.toggle_visible(index);
                self.session_dirty = true;
            }
            PointerTarget::ColumnMove { index, delta } => {
                self.column_overlay.index = self.layout.move_column(index, delta);
                self.session_dirty = true;
            }
            PointerTarget::ColumnResize { index, delta } => {
                self.column_overlay.index = index;
                self.layout.resize(index, delta);
                self.session_dirty = true;
            }
            PointerTarget::PaletteCommand { index } => {
                self.palette.selected = index;
                return self.run_selected_command();
            }
            PointerTarget::PaletteQuery => {
                self.place_caret(TextEditor::Palette, column, row);
            }
            PointerTarget::EditMenuRow { index } => {
                self.edit_menu.index = index;
                return self.run_edit_menu_entry(index);
            }
            PointerTarget::StateOption { index } => {
                self.state_picker.index = index;
                return self.choose_state(index);
            }
            PointerTarget::PriorityOption { index } => {
                self.priority_picker.index = index;
                return self.choose_priority(index);
            }
            PointerTarget::AssigneeOption { index } => {
                self.assignee_picker.index = index;
                return self.choose_assignee(index);
            }
            PointerTarget::AssigneeQuery => {
                self.place_caret(TextEditor::Assignee, column, row);
            }
            PointerTarget::NodeOption { index } => {
                self.node_picker.index = index;
                return self.choose_node(index);
            }
            PointerTarget::NodeQuery => {
                self.place_caret(TextEditor::Node, column, row);
            }
            PointerTarget::PromptInput => {
                self.place_caret(TextEditor::Prompt, column, row);
            }
            PointerTarget::SubmitPrompt => return self.submit_prompt(),
            PointerTarget::CancelPrompt => self.close_prompt(),
            PointerTarget::ViewRow { index } => {
                self.views_overlay.index = index;
                self.apply_view_at(index);
            }
            PointerTarget::SaveView => {
                if self.views_overlay.naming.is_some() {
                    if let Some(name) = self
                        .views_overlay
                        .naming
                        .take()
                        .map(|name| name.text().trim().to_owned())
                        .filter(|name| !name.is_empty())
                    {
                        self.save_view(name);
                    }
                } else {
                    self.views_overlay.naming =
                        Some(TextInput::new(self.active_view.clone().unwrap_or_default()));
                }
            }
            PointerTarget::DeleteView => self.delete_view_at(self.views_overlay.index),
            PointerTarget::ViewName => {
                self.place_caret(TextEditor::ViewName, column, row);
            }
            PointerTarget::CancelNaming => self.views_overlay.naming = None,
            PointerTarget::OverlayBody => {}
            PointerTarget::ScrollbarTrack { surface, page_down } => {
                let step =
                    i32::try_from(self.scroll_state(surface).page_step()).unwrap_or(i32::MAX);
                self.scroll_surface(surface, if page_down { step } else { -step });
            }
            PointerTarget::ScrollbarThumb { .. } => {}
            PointerTarget::PaneDivider => {}
        }
        AppAction::None
    }

    fn close_overlay(&mut self) {
        match self.mode {
            AppMode::Views if self.views_overlay.naming.is_some() => {
                self.views_overlay.naming = None;
            }
            AppMode::Prompt => self.close_prompt(),
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

    fn open_copy_actions(&mut self) {
        self.run_command(CommandId::Palette);
        self.palette.query = TextInput::new("copy");
    }

    fn place_caret(&mut self, editor: TextEditor, column: u16, row: u16) {
        let Some(snapshot) = self
            .hit_regions
            .selectable(match editor {
                TextEditor::Search => SelectableSurface::Search,
                TextEditor::Palette
                | TextEditor::ViewName
                | TextEditor::Prompt
                | TextEditor::Assignee
                | TextEditor::Node => SelectableSurface::Overlay,
            })
            .and_then(|snapshot| snapshot.pos_at(column, row))
            .or_else(|| {
                self.hit_regions.resolve(column, row).map(|region| TextPos {
                    line: 0,
                    col: usize::from(column.saturating_sub(region.rect.x)),
                })
            })
        else {
            return;
        };
        let index = snapshot.col;
        match editor {
            TextEditor::Search => self.query.set_cursor(index),
            TextEditor::Palette => self.palette.query.set_cursor(index),
            TextEditor::ViewName => {
                if let Some(name) = self.views_overlay.naming.as_mut() {
                    name.set_cursor(index);
                }
            }
            TextEditor::Prompt => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.input.set_cursor(index);
                }
            }
            TextEditor::Assignee => self.assignee_picker.query.set_cursor(index),
            TextEditor::Node => self.node_picker.query.set_cursor(index),
        }
    }

    fn update_text_drag(&mut self, column: u16, row: u16) {
        let Some(surface) = self
            .pointer
            .selection
            .map(|selection| selection.surface)
            .or_else(|| self.pointer.press_selectable())
        else {
            return;
        };
        let Some(snapshot) = self.hit_regions.selectable(surface) else {
            return;
        };
        let Some(end) = snapshot
            .pos_at(column, row)
            .or_else(|| clamp_pos_to_snapshot(snapshot, column, row))
        else {
            return;
        };
        if let Some(selection) = self.pointer.selection.as_mut() {
            selection.end = end;
        } else if let Some(origin) = self.pointer.press_origin()
            && let Some(start) = snapshot.pos_at(origin.0, origin.1)
        {
            self.pointer.selection = Some(TextSelection {
                surface,
                start,
                end,
            });
        }
    }

    fn scrollbar_grab(&self, surface: ScrollSurface, origin: Option<(u16, u16)>) -> i16 {
        let Some((_, row)) = origin else {
            return 0;
        };
        let Some(metrics) = self.hit_regions.scroll(surface) else {
            return 0;
        };
        let Some(thumb) = metrics.thumb() else {
            return 0;
        };
        i16::try_from(row).unwrap_or(0)
            - i16::try_from(metrics.track.y.saturating_add(thumb.y)).unwrap_or(0)
    }

    fn drag_scrollbar(&mut self, surface: ScrollSurface, row: u16, grab: i16) {
        let Some(metrics) = self.hit_regions.scroll(surface) else {
            return;
        };
        let Some(thumb) = metrics.thumb() else {
            return;
        };
        let pointer = i32::from(row) - i32::from(grab);
        let track_y = i32::from(metrics.track.y);
        let rel = pointer.saturating_sub(track_y).max(0) as usize;
        let offset =
            pointer::offset_from_thumb(rel.min(thumb.travel), thumb.travel, thumb.max_offset);
        self.scroll_state_mut(surface).scroll_to(offset);
    }

    fn scroll_surface(&mut self, surface: ScrollSurface, delta: i32) -> bool {
        self.scroll_state_mut(surface).scroll_by(delta)
    }

    /// Records the workspace the panes were last split inside, and which way the
    /// divider runs there. The narrow layout passes `None`: it has no divider.
    pub const fn set_content_layout(&mut self, area: Rect, divider: Option<DividerOrientation>) {
        self.content_area = area;
        self.divider = divider;
    }

    #[must_use]
    pub const fn content_area(&self) -> Rect {
        self.content_area
    }

    #[must_use]
    pub const fn divider_orientation(&self) -> Option<DividerOrientation> {
        self.divider
    }

    /// Moves the divider under the pointer: the tickets pane keeps everything up
    /// to the pointer, the details pane the rest.
    fn drag_divider(&mut self, column: u16, row: u16) {
        match self.divider {
            Some(DividerOrientation::Vertical) => {
                let span = self.content_area.width;
                let cells = column.saturating_sub(self.content_area.x);
                self.pane_split_wide =
                    split_percent(cells, span, MIN_TICKETS_COLUMNS, MIN_DETAILS_COLUMNS);
            }
            Some(DividerOrientation::Horizontal) => {
                let span = self.content_area.height;
                let cells = row.saturating_sub(self.content_area.y);
                self.pane_split_stacked = split_percent(cells, span, MIN_PANE_ROWS, MIN_PANE_ROWS);
            }
            None => {}
        }
    }

    /// Restores the built-in split for both layouts.
    fn reset_pane_split(&mut self) {
        self.pane_split_wide = DEFAULT_PANE_SPLIT_WIDE;
        self.pane_split_stacked = DEFAULT_PANE_SPLIT_STACKED;
        self.session_dirty = true;
        self.set_status("Reset pane split");
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

    fn begin_search(&mut self) {
        self.query.move_end();
        self.search_history_index = None;
        self.search_history_draft = self.query.text().to_owned();
        self.mode = AppMode::Search;
    }

    fn finish_search(&mut self) {
        if !self.query.is_empty()
            && self
                .search_history
                .last()
                .is_none_or(|previous| previous != self.query.text())
        {
            const HISTORY_LIMIT: usize = 50;
            if self.search_history.len() == HISTORY_LIMIT {
                self.search_history.remove(0);
            }
            self.search_history.push(self.query.text().to_owned());
        }
        self.search_history_index = None;
        self.search_history_draft.clear();
        self.mode = AppMode::Browse;
        self.record_history();
    }

    fn recall_previous_search(&mut self) {
        if self.search_history.is_empty() {
            return;
        }
        let target = self.search_history_index.map_or_else(
            || self.search_history.len() - 1,
            |index| index.saturating_sub(1),
        );
        if self.search_history_index.is_none() {
            self.search_history_draft = self.query.text().to_owned();
        }
        let draft = self.search_history_draft.clone();
        let query = self.search_history[target].clone();
        self.set_query(query);
        self.search_history_draft = draft;
        self.search_history_index = Some(target);
    }

    fn recall_next_search(&mut self) {
        let Some(index) = self.search_history_index else {
            return;
        };
        let draft = self.search_history_draft.clone();
        if index + 1 < self.search_history.len() {
            let target = index + 1;
            let query = self.search_history[target].clone();
            self.set_query(query);
            self.search_history_draft = draft;
            self.search_history_index = Some(target);
        } else {
            self.set_query(draft);
            self.search_history_index = None;
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
            if self.ticket_by_key(key).is_some() {
                self.set_status(format!("{id} is hidden by the current search", id = key.id));
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

    fn submit_search(&mut self) {
        self.search_generation = self.search.submit(&self.fuzzy_query());
        self.search_pending = true;
    }

    fn show_all(&mut self, selected: Option<&TicketKey>) {
        self.visible = (0..self.tickets.len())
            .map(|ticket_index| SearchMatch {
                ticket_index,
                score: 0,
            })
            .collect();
        self.apply_filters();
        self.sort_visible();
        self.restore_selection(selected);
    }

    fn apply_filters(&mut self) {
        let filters = self.parsed_query().filters;
        let bookmarks = self.bookmarks.clone();
        let tickets = Arc::clone(&self.tickets);
        self.visible.retain(|entry| {
            filters.matches(
                &tickets[entry.ticket_index],
                bookmarks.contains(&tickets[entry.ticket_index].key),
            )
        });
    }

    fn sort_visible(&mut self) {
        let tickets = Arc::clone(&self.tickets);
        let field = self.sort_field;
        let direction = self.sort_direction;
        let relevance_first =
            !self.fuzzy_query().is_empty() && self.search_order == SearchOrder::Relevance;
        self.visible.sort_by(|left, right| {
            let relevance = if relevance_first {
                right.score.cmp(&left.score)
            } else {
                Ordering::Equal
            };
            relevance.then_with(|| {
                compare_tickets(
                    &tickets[left.ticket_index],
                    &tickets[right.ticket_index],
                    field,
                    direction,
                )
            })
        });
    }

    fn restore_selection(&mut self, selected: Option<&TicketKey>) {
        let row = selected.and_then(|key| {
            self.visible
                .iter()
                .position(|entry| self.tickets[entry.ticket_index].key == *key)
        });
        self.table_state
            .select((!self.visible.is_empty()).then_some(row.unwrap_or_default()));
        self.sync_family_state();
        if selected.is_none() || row.is_none() {
            self.table.scroll_to(0);
            *self.table_state.offset_mut() = 0;
            self.details.scroll_to(0);
        }
    }

    fn sync_family_state(&mut self) {
        self.reset_family_cursor();
        if self.focus == Focus::Family && !self.selected_has_family() {
            self.focus = Focus::Details;
        }
    }

    fn reset_family_cursor(&mut self) {
        self.family_cursor = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.clamp_family_cursor();
    }

    fn family_page_size(&self) -> isize {
        let visible = self.visible_family_tree().len().max(1);
        let viewport = self.details.viewport.max(1);
        isize::try_from(viewport.min(visible)).unwrap_or(1)
    }

    fn move_family_cursor(&mut self, delta: isize) {
        let tree = self.visible_family_tree();
        if tree.is_empty() {
            return;
        }
        let current = self
            .family_cursor
            .as_ref()
            .and_then(|key| tree.iter().position(|entry| entry.key == *key))
            .unwrap_or(0);
        let next = current
            .saturating_add_signed(delta)
            .min(tree.len().saturating_sub(1));
        self.family_cursor = Some(tree[next].key.clone());
        self.ensure_family_cursor_visible();
    }

    fn move_family_cursor_to_edge(&mut self, last: bool) {
        let tree = self.visible_family_tree();
        let Some(entry) = (if last { tree.last() } else { tree.first() }) else {
            return;
        };
        self.family_cursor = Some(entry.key.clone());
        self.ensure_family_cursor_visible();
    }

    fn clamp_family_cursor(&mut self) {
        let tree = self.visible_family_tree();
        if tree.is_empty() {
            if self.selected_ticket().is_none() {
                self.family_cursor = None;
            }
            return;
        }
        if self
            .family_cursor
            .as_ref()
            .is_some_and(|key| tree.iter().any(|entry| entry.key == *key))
        {
            return;
        }
        let mut walk = self.family_cursor.clone();
        while let Some(key) = walk {
            if let Some(parent) = self.graph.parents_of(&key).into_iter().next() {
                if tree.iter().any(|entry| entry.key == parent) {
                    self.family_cursor = Some(parent);
                    return;
                }
                walk = Some(parent);
            } else {
                break;
            }
        }
        self.family_cursor = tree.first().map(|entry| entry.key.clone());
    }

    fn ensure_family_cursor_visible(&mut self) {
        let Some(cursor) = self.family_cursor.clone() else {
            return;
        };
        let tree = self.visible_family_tree();
        let Some(index) = tree.iter().position(|entry| entry.key == cursor) else {
            return;
        };
        let line = index.saturating_add(1);
        let viewport = self.details.viewport.max(1);
        if index == 0 {
            self.details.offset = 0;
        } else if line < self.details.offset {
            self.details.offset = line;
        } else if line >= self.details.offset.saturating_add(viewport) {
            self.details.offset = line
                .saturating_add(1)
                .saturating_sub(viewport)
                .min(self.details.max_offset());
        }
    }

    fn open_filters(&mut self) {
        self.filter_overlay = FilterOverlay::default();
        self.mode = AppMode::Filter;
    }

    fn open_facets(&mut self, field_index: usize) {
        self.facet_bar.field_index = field_index.min(FilterField::BAR.len());
        self.facet_bar.value_index = 0;
        self.mode = AppMode::Facets;
    }

    fn handle_facet_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('f') => self.mode = AppMode::Browse,
            KeyCode::Char('+') => self.open_filters(),
            KeyCode::Left | KeyCode::Char('h') => {
                self.facet_bar.field_index = self.facet_bar.field_index.saturating_sub(1);
                self.facet_bar.value_index = 0;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.facet_bar.field_index =
                    (self.facet_bar.field_index + 1).min(FilterField::BAR.len());
                self.facet_bar.value_index = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_facet_value(self.facet_bar.value_index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let count = self.focused_bar_facets().len();
                if count > 0 {
                    self.focus_facet_value((self.facet_bar.value_index + 1).min(count - 1));
                }
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                if self.facet_bar.field_index >= FilterField::BAR.len() {
                    self.open_filters();
                } else {
                    self.toggle_current_bar_facet();
                }
            }
            _ => {}
        }
    }

    fn focus_facet_value(&mut self, index: usize) {
        self.facet_bar.value_index = index;
        self.facet_bar.scroll.ensure_visible(index);
    }

    fn focused_bar_field(&self) -> Option<FilterField> {
        FilterField::BAR.get(self.facet_bar.field_index).copied()
    }

    fn focused_bar_facets(&self) -> Vec<FacetValue> {
        self.focused_bar_field()
            .map_or_else(Vec::new, |field| self.facets_for(field))
    }

    fn toggle_current_bar_facet(&mut self) {
        let Some(field) = self.focused_bar_field() else {
            return;
        };
        let Some(value) = self
            .focused_bar_facets()
            .get(self.facet_bar.value_index)
            .map(|facet| facet.value.clone())
        else {
            return;
        };
        self.toggle_filter(field, &value);
    }

    fn open_columns(&mut self) {
        self.column_overlay.index = 0;
        self.mode = AppMode::Columns;
    }

    fn open_palette(&mut self) {
        self.palette = PaletteState::default();
        self.mode = AppMode::Palette;
    }

    fn open_views(&mut self) {
        self.views_overlay = ViewsOverlay::default();
        self.mode = AppMode::Views;
    }

    /// `e`: the list of field editors. Every editor is one row of
    /// [`EDIT_MENU`], so a new one appears here by being added there.
    fn open_edit_menu(&mut self) {
        self.edit_menu = EditMenu::default();
        self.mode = AppMode::Edit;
    }

    fn handle_edit_menu_key(&mut self, key: KeyEvent) -> AppAction {
        let last = EDIT_MENU.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('e') => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_edit_entry(self.edit_menu.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_edit_entry((self.edit_menu.index + 1).min(last));
            }
            KeyCode::Home => self.focus_edit_entry(0),
            KeyCode::End => self.focus_edit_entry(last),
            KeyCode::Enter => return self.run_edit_menu_entry(self.edit_menu.index),
            _ => {}
        }
        AppAction::None
    }

    fn focus_edit_entry(&mut self, index: usize) {
        self.edit_menu.index = index;
        self.edit_menu.scroll.ensure_visible(index);
    }

    /// Runs one Edit menu entry, which is the command it names. Each editor
    /// opens itself, so nothing here knows what a state or a title is.
    fn run_edit_menu_entry(&mut self, index: usize) -> AppAction {
        let Some(entry) = EDIT_MENU.get(index) else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        self.mode = AppMode::Browse;
        self.run_command(entry.command)
    }

    /// `S`, and the Edit menu's State row: the states this work item's type
    /// allows, with the one it is in already under the cursor. The list is
    /// whatever is cached or already in the database, so this never waits.
    fn open_state_picker(&mut self) {
        let Some(ticket) = self.selected_ticket() else {
            self.set_error("No work item is selected");
            return;
        };
        let current = ticket.state.clone();
        let work_item_type = ticket.work_item_type.clone();
        let id = ticket.key.id;
        let options = self.states_for(&work_item_type);
        if options.is_empty() {
            self.set_error(format!("No states are known for {work_item_type}"));
            return;
        }
        let index = options
            .iter()
            .position(|option| option.name == current)
            .unwrap_or_default();
        self.state_picker = StatePicker {
            options,
            index,
            scroll: ScrollState::default(),
            current,
            id,
        };
        self.state_picker.scroll.ensure_visible(index);
        self.mode = AppMode::StatePicker;
    }

    fn handle_state_picker_key(&mut self, key: KeyEvent) -> AppAction {
        let last = self.state_picker.options.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('S') => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_state(self.state_picker.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_state((self.state_picker.index + 1).min(last));
            }
            KeyCode::PageUp => self.focus_state(self.state_picker.index.saturating_sub(5)),
            KeyCode::PageDown => self.focus_state((self.state_picker.index + 5).min(last)),
            KeyCode::Home => self.focus_state(0),
            KeyCode::End => self.focus_state(last),
            KeyCode::Enter => return self.choose_state(self.state_picker.index),
            _ => {}
        }
        AppAction::None
    }

    fn focus_state(&mut self, index: usize) {
        self.state_picker.index = index;
        self.state_picker.scroll.ensure_visible(index);
    }

    /// Confirms one state. Choosing the state the work item is already in
    /// closes the picker and writes nothing; anything else takes the ordinary
    /// write-through path, so the row changes at once and reverts if Azure
    /// DevOps refuses the transition.
    fn choose_state(&mut self, index: usize) -> AppAction {
        let Some(option) = self.state_picker.options.get(index).cloned() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        self.mode = AppMode::Browse;
        if option.name == self.state_picker.current {
            return AppAction::None;
        }
        self.edit_selected(FieldEdit::state(&option.name))
    }

    /// The Edit menu's Priority row: 1 to 4 and a `Clear` row, with the
    /// priority the work item already has under the cursor.
    fn open_priority_picker(&mut self) {
        let Some(ticket) = self.selected_ticket() else {
            self.set_error("No work item is selected");
            return;
        };
        let current = ticket.priority;
        let id = ticket.key.id;
        let index = PRIORITY_CHOICES
            .iter()
            .position(|choice| *choice == current)
            .unwrap_or_default();
        self.priority_picker = PriorityPicker {
            index,
            scroll: ScrollState::default(),
            current,
            id,
        };
        self.priority_picker.scroll.ensure_visible(index);
        self.mode = AppMode::PriorityPicker;
    }

    fn handle_priority_picker_key(&mut self, key: KeyEvent) -> AppAction {
        let last = PRIORITY_CHOICES.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_priority(self.priority_picker.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_priority((self.priority_picker.index + 1).min(last));
            }
            KeyCode::Home => self.focus_priority(0),
            KeyCode::End => self.focus_priority(last),
            KeyCode::Enter => return self.choose_priority(self.priority_picker.index),
            _ => {}
        }
        AppAction::None
    }

    fn focus_priority(&mut self, index: usize) {
        self.priority_picker.index = index;
        self.priority_picker.scroll.ensure_visible(index);
    }

    /// Confirms one priority. The priority the work item already has is a
    /// no-op, and `Clear` takes the field off it rather than writing an empty
    /// value, so the Pri cell empties.
    fn choose_priority(&mut self, index: usize) -> AppAction {
        let Some(choice) = PRIORITY_CHOICES.get(index).copied() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        self.mode = AppMode::Browse;
        if choice == self.priority_picker.current {
            return AppAction::None;
        }
        match choice {
            Some(priority) => self.edit_selected(FieldEdit::priority(priority)),
            None => self.edit_selected(FieldEdit::clear_priority()),
        }
    }

    /// Who the assignee picker offers, in the order it lists them: nobody, the
    /// signed-in user, everybody the database has ever seen a work item
    /// assigned to, and then the rest of the project's teams. Nobody appears
    /// twice, so a team member already holding work keeps their earlier place.
    #[must_use]
    fn assignee_candidates(&self) -> Vec<AssigneeCandidate> {
        let mut candidates = vec![AssigneeCandidate {
            display: UNASSIGNED_LABEL.to_owned(),
            unique: None,
            unassigned: true,
            me: false,
        }];
        if let Some(me) = self
            .me
            .as_deref()
            .map(str::trim)
            .filter(|me| !me.is_empty())
        {
            candidates.push(self.candidate_for(me, true));
        }
        let mut assigned: Vec<&str> = self
            .tickets
            .iter()
            .filter_map(|ticket| ticket.assigned_to.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect();
        assigned.sort_by_key(|name| name.to_lowercase());
        for name in assigned {
            if !names_someone_listed(&candidates, name) {
                candidates.push(self.candidate_for(name, false));
            }
        }
        for identity in &self.identities {
            if !names_someone_listed(&candidates, &identity.display_name) {
                candidates.push(AssigneeCandidate {
                    display: identity.display_name.clone(),
                    unique: identity.unique_name.clone(),
                    unassigned: false,
                    me: false,
                });
            }
        }
        candidates
    }

    /// One candidate for a name the rows carry, with the sign-in address filled
    /// in from the cached identities when they know one.
    fn candidate_for(&self, display: &str, me: bool) -> AssigneeCandidate {
        AssigneeCandidate {
            display: display.to_owned(),
            unique: self
                .identities
                .iter()
                .find(|identity| same_name(&identity.display_name, display))
                .and_then(|identity| identity.unique_name.clone()),
            unassigned: false,
            me,
        }
    }

    /// The candidates whatever has been typed leaves showing, which is what the
    /// picker draws and what its cursor counts over.
    #[must_use]
    pub fn assignee_matches(&self) -> Vec<AssigneeCandidate> {
        let query = self.assignee_picker.query.text().trim().to_owned();
        self.assignee_picker
            .candidates
            .iter()
            .filter(|candidate| {
                query.is_empty()
                    || fuzzy_contains(&candidate.display, &query)
                    || candidate
                        .unique
                        .as_deref()
                        .is_some_and(|unique| fuzzy_contains(unique, &query))
            })
            .cloned()
            .collect()
    }

    /// `a`, and the Edit menu's Assignee row: everybody worth offering, with
    /// whoever holds the work item under the cursor. The list is built from
    /// what is already in memory, so the picker opens at once; the project's
    /// teams are asked for the first time it is opened and merged in when they
    /// arrive.
    fn open_assignee_picker(&mut self) -> AppAction {
        let Some(ticket) = self.selected_ticket() else {
            self.set_error("No work item is selected");
            return AppAction::None;
        };
        let current = ticket.assigned_to.clone();
        let id = ticket.key.id;
        let candidates = self.assignee_candidates();
        let index = candidates
            .iter()
            .position(|candidate| candidate.is_current(current.as_deref()))
            .unwrap_or_default();
        self.assignee_picker = AssigneePicker {
            candidates,
            query: TextInput::default(),
            index,
            scroll: ScrollState::default(),
            current,
            id,
        };
        self.assignee_picker.scroll.ensure_visible(index);
        self.mode = AppMode::AssigneePicker;
        if self.identities_requested {
            AppAction::None
        } else {
            self.identities_requested = true;
            AppAction::FetchIdentities
        }
    }

    fn handle_assignee_picker_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.mode = AppMode::Browse,
            KeyCode::Up => self.move_assignee_selection(-1),
            KeyCode::Down => self.move_assignee_selection(1),
            KeyCode::PageUp => self.move_assignee_selection(-5),
            KeyCode::PageDown => self.move_assignee_selection(5),
            KeyCode::Enter => return self.choose_assignee(self.assignee_picker.index),
            // Everything else is typing: Home, End, and the editing keys all
            // belong to the filter field, the way they do in the palette.
            _ => {
                let before = self.assignee_picker.query.text().to_owned();
                self.assignee_picker.query.handle_key(key);
                if self.assignee_picker.query.text() != before {
                    self.assignee_picker.index = 0;
                    self.assignee_picker.scroll.scroll_to(0);
                }
            }
        }
        AppAction::None
    }

    fn move_assignee_selection(&mut self, delta: isize) {
        let count = self.assignee_matches().len();
        if count == 0 {
            self.assignee_picker.index = 0;
            return;
        }
        let index = self
            .assignee_picker
            .index
            .saturating_add_signed(delta)
            .min(count - 1);
        self.focus_assignee(index);
    }

    fn focus_assignee(&mut self, index: usize) {
        self.assignee_picker.index = index;
        self.assignee_picker.scroll.ensure_visible(index);
    }

    /// Confirms one candidate. Whoever holds the work item already is a no-op,
    /// and `Unassigned` takes the field off it rather than writing an empty
    /// identity, so the Assignee cell empties.
    fn choose_assignee(&mut self, index: usize) -> AppAction {
        let Some(candidate) = self.assignee_matches().get(index).cloned() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        self.mode = AppMode::Browse;
        if candidate.is_current(self.assignee_picker.current.as_deref()) {
            return AppAction::None;
        }
        if candidate.unassigned {
            return self.edit_selected(FieldEdit::unassign());
        }
        self.edit_selected(FieldEdit::assignee(
            &candidate.display,
            candidate.unique.as_deref(),
        ))
    }

    /// The project's iteration and area trees as the database holds them.
    #[must_use]
    pub fn classification_nodes(&self) -> &[ClassificationNode] {
        &self.classification_nodes
    }

    /// The trees read out of the database at startup, with the time they were
    /// last fetched, so a picker opening on a fresh cache asks for nothing.
    pub fn set_classification_nodes(
        &mut self,
        nodes: Vec<ClassificationNode>,
        fetched_at: Option<Timestamp>,
    ) {
        self.classification_nodes = nodes;
        self.classification_fetched_at = fetched_at;
    }

    /// The trees a fetch brought back. An empty answer changes nothing: the
    /// endpoint could not be read, and the cached nodes are better than none.
    /// An open picker is rebuilt around them, keeping the row under the cursor.
    pub fn merge_classification_nodes(&mut self, nodes: Vec<ClassificationNode>) {
        if nodes.is_empty() {
            return;
        }
        self.classification_nodes = nodes;
        self.classification_fetched_at = Some(Timestamp::now());
        if self.mode != AppMode::NodePicker {
            return;
        }
        let focused = self
            .node_matches()
            .get(self.node_picker.index)
            .map(|row| row.path.clone());
        let kind = self.node_picker.kind;
        let current = self.node_picker.current.clone();
        self.node_picker.rows = self.node_rows(kind);
        let matches = self.node_matches();
        let index = focused
            .and_then(|path| matches.iter().position(|row| row.path == path))
            .or_else(|| matches.iter().position(|row| row.path == current))
            .unwrap_or(self.node_picker.index)
            .min(matches.len().saturating_sub(1));
        self.focus_node(index);
    }

    /// The sprint the project is in: the deepest iteration whose dates contain
    /// today in UTC. `None` when no iteration is scheduled around today, which
    /// includes every project whose trees have never been fetched.
    #[must_use]
    pub fn current_iteration(&self) -> Option<String> {
        classification::current_iteration(&self.classification_nodes, Timestamp::now().date())
            .map(|node| node.path.clone())
    }

    /// The rows one picker offers: the cached tree when there is one, and
    /// otherwise the distinct paths the work items already carry, which is
    /// enough to move work between the sprints actually in use. Either way the
    /// work item's own node is among them — a work item always sits somewhere
    /// in the tree it is planned into — so the cursor has somewhere to start.
    #[must_use]
    fn node_rows(&self, kind: NodeKind) -> Vec<NodeRow> {
        let today = Timestamp::now().date();
        let rows: Vec<NodeRow> = self
            .classification_nodes
            .iter()
            .filter(|node| node.kind == kind)
            .map(|node| NodeRow {
                path: node.path.clone(),
                depth: node.depth,
                dates: node.date_range(),
                current_period: node.contains(today),
            })
            .collect();
        if rows.is_empty() {
            return self.database_node_rows(kind);
        }
        rows
    }

    /// The fallback, for a project whose trees have never been fetched: every
    /// distinct path of one kind the database holds, in order, indented by the
    /// depth read off the path itself.
    #[must_use]
    fn database_node_rows(&self, kind: NodeKind) -> Vec<NodeRow> {
        let mut paths: Vec<&str> = self
            .tickets
            .iter()
            .map(|ticket| match kind {
                NodeKind::Area => ticket.area_path.as_str(),
                NodeKind::Iteration => ticket.iteration_path.as_str(),
            })
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .collect();
        paths.sort_unstable();
        paths.dedup();
        paths.into_iter().map(NodeRow::of).collect()
    }

    /// The rows whatever has been typed leaves showing, matched on the whole
    /// path so `q3s7` finds `development\Q3\Sprint 7`.
    #[must_use]
    pub fn node_matches(&self) -> Vec<NodeRow> {
        let query = self.node_picker.query.text().trim().to_owned();
        self.node_picker
            .rows
            .iter()
            .filter(|row| query.is_empty() || fuzzy_contains(&row.path, &query))
            .cloned()
            .collect()
    }

    /// The Edit menu's Iteration and Area rows: the project's tree, indented,
    /// with the node the work item sits in already under the cursor. The rows
    /// come out of what is already in memory, so the picker opens at once; the
    /// trees are asked for the first time either picker is opened on a cache
    /// that is empty or over an hour old, and merged in when they arrive.
    fn open_node_picker(&mut self, kind: NodeKind) -> AppAction {
        let Some(ticket) = self.selected_ticket() else {
            self.set_error("No work item is selected");
            return AppAction::None;
        };
        let current = match kind {
            NodeKind::Area => ticket.area_path.clone(),
            NodeKind::Iteration => ticket.iteration_path.clone(),
        };
        let id = ticket.key.id;
        let rows = self.node_rows(kind);
        let index = rows
            .iter()
            .position(|row| row.path == current)
            .unwrap_or_default();
        self.node_picker = NodePicker {
            kind,
            rows,
            query: TextInput::default(),
            index,
            scroll: ScrollState::default(),
            current,
            id,
        };
        self.node_picker.scroll.ensure_visible(index);
        self.mode = AppMode::NodePicker;
        if self.should_fetch_classification_nodes() {
            self.classification_requested = true;
            AppAction::FetchClassificationNodes
        } else {
            AppAction::None
        }
    }

    /// Whether opening a picker should ask Azure DevOps for the trees: once a
    /// session at most, and not at all while a cache under an hour old is
    /// loaded, so the second open costs nothing and so does the first one after
    /// a restart.
    #[must_use]
    fn should_fetch_classification_nodes(&self) -> bool {
        if self.classification_requested {
            return false;
        }
        if self.classification_nodes.is_empty() {
            return true;
        }
        self.classification_fetched_at.is_none_or(|fetched| {
            fetched.seconds_until(Timestamp::now()) >= CLASSIFICATION_MAX_AGE_SECONDS
        })
    }

    fn handle_node_picker_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.mode = AppMode::Browse,
            KeyCode::Up => self.move_node_selection(-1),
            KeyCode::Down => self.move_node_selection(1),
            KeyCode::PageUp => self.move_node_selection(-5),
            KeyCode::PageDown => self.move_node_selection(5),
            KeyCode::Enter => return self.choose_node(self.node_picker.index),
            // Everything else is typing, the way it is in the assignee picker.
            _ => {
                let before = self.node_picker.query.text().to_owned();
                self.node_picker.query.handle_key(key);
                if self.node_picker.query.text() != before {
                    self.node_picker.index = 0;
                    self.node_picker.scroll.scroll_to(0);
                }
            }
        }
        AppAction::None
    }

    fn move_node_selection(&mut self, delta: isize) {
        let count = self.node_matches().len();
        if count == 0 {
            self.node_picker.index = 0;
            return;
        }
        let index = self
            .node_picker
            .index
            .saturating_add_signed(delta)
            .min(count - 1);
        self.focus_node(index);
    }

    fn focus_node(&mut self, index: usize) {
        self.node_picker.index = index;
        self.node_picker.scroll.ensure_visible(index);
    }

    /// Confirms one node. The node the work item already sits in is a no-op;
    /// anything else writes the full backslash path to `System.IterationPath`
    /// or `System.AreaPath`, and the table column goes on showing the leaf.
    fn choose_node(&mut self, index: usize) -> AppAction {
        let Some(row) = self.node_matches().get(index).cloned() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        self.mode = AppMode::Browse;
        if row.path == self.node_picker.current {
            return AppAction::None;
        }
        match self.node_picker.kind {
            NodeKind::Iteration => self.edit_selected(FieldEdit::iteration(&row.path)),
            NodeKind::Area => self.edit_selected(FieldEdit::area(&row.path)),
        }
    }

    /// The Edit menu's Title and Tags rows: a single-line field prefilled with
    /// what the work item says now, edited with the same keys as the
    /// named-view editor.
    fn open_prompt(&mut self, field: PromptField) {
        let Some(ticket) = self.selected_ticket() else {
            self.set_error("No work item is selected");
            return;
        };
        let original = match field {
            PromptField::Title => ticket.title.clone(),
            PromptField::Tags => ticket.tags.join("; "),
            PromptField::Comment => String::new(),
        };
        let id = ticket.key.id;
        self.prompt = Some(TextPrompt {
            field,
            input: TextInput::new(original.clone()),
            id,
            original,
        });
        self.mode = AppMode::Prompt;
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.close_prompt(),
            KeyCode::Enter => return self.submit_prompt(),
            _ => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.input.handle_key(key);
                }
            }
        }
        AppAction::None
    }

    fn close_prompt(&mut self) {
        self.prompt = None;
        self.mode = AppMode::Browse;
    }

    /// Saves what the prompt holds. A title is trimmed, and one that is empty
    /// or only whitespace is refused here rather than sent, with the prompt
    /// left open on it. A tag list is normalised. Text that comes back to what
    /// the work item already says closes the prompt without a write.
    fn submit_prompt(&mut self) -> AppAction {
        let Some(prompt) = self.prompt.as_ref() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        let field = prompt.field;
        let original = prompt.original.trim().to_owned();
        let edited = match field {
            PromptField::Title | PromptField::Comment => prompt.input.text().trim().to_owned(),
            PromptField::Tags => normalize_tags(prompt.input.text()),
        };
        if edited.is_empty() {
            match field {
                PromptField::Title => {
                    self.set_error(format!("#{} title cannot be empty", prompt.id));
                    return AppAction::None;
                }
                PromptField::Comment => {
                    self.set_error(format!("#{} comment cannot be empty", prompt.id));
                    return AppAction::None;
                }
                PromptField::Tags => {}
            }
        }
        self.close_prompt();
        if field != PromptField::Comment && edited == original {
            return AppAction::None;
        }
        match field {
            PromptField::Title => self.edit_selected(FieldEdit::title(&edited)),
            PromptField::Tags => self.edit_selected(FieldEdit::tags(&edited)),
            PromptField::Comment => self.comment_selected(edited),
        }
    }

    /// Asks for a comment to be left on the selected work item. Unlike a field
    /// edit nothing is shown until Azure DevOps has stored it: a comment has no
    /// id, date, or author until the server gives it one, and a line that
    /// turned out never to have been posted is worse than a moment's wait.
    pub fn comment_selected(&mut self, text: String) -> AppAction {
        let Some(key) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            self.set_error("No work item is selected");
            return AppAction::None;
        };
        let refusal = |reason: &str| format!("#{} comment not posted: {reason}", key.id);
        if !self.sync_enabled {
            let reason = self
                .offline_reason
                .clone()
                .unwrap_or_else(|| "no Azure DevOps organization is configured".to_owned());
            let message = refusal(&reason);
            self.set_error(message);
            return AppAction::None;
        }
        if self.pending_comments.contains(&key) {
            let message = refusal("an earlier comment is still in flight");
            self.set_error(message);
            return AppAction::None;
        }
        self.pending_comments.insert(key.clone());
        AppAction::Comment { key, text }
    }

    /// Whether a comment is waiting on Azure DevOps. The database watcher
    /// stands down while one is, because the sync worker is writing that row
    /// itself.
    #[must_use]
    pub fn comments_pending(&self) -> bool {
        !self.pending_comments.is_empty()
    }

    /// Files the comment Azure DevOps stored, so the details pane shows it at
    /// once rather than waiting for the pull that would bring it back.
    pub fn apply_comment(&mut self, comment: CommentRecord) {
        self.pending_comments.remove(&comment.ticket);
        let id = comment.ticket.id;
        self.graph.add_comment(comment);
        self.set_status(format!("Commented on #{id}"));
    }

    /// A comment that never landed. Nothing was shown for it and nothing is
    /// stored, so only the notification is left to say so.
    pub fn reject_comment(&mut self, key: &TicketKey, message: &str) {
        self.pending_comments.remove(key);
        self.set_error(format!("#{} comment not posted: {message}", key.id));
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc if self.filter_overlay.showing_values => {
                self.filter_overlay.showing_values = false;
                self.filter_overlay.value_index = 0;
            }
            KeyCode::Esc | KeyCode::Char('f') => self.mode = AppMode::Browse,
            KeyCode::Left | KeyCode::Char('h') if self.filter_overlay.showing_values => {
                self.filter_overlay.showing_values = false;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter
                if !self.filter_overlay.showing_values =>
            {
                self.filter_overlay.showing_values = true;
                self.filter_overlay.value_index = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_filter_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_filter_cursor(1),
            KeyCode::Char(' ') | KeyCode::Enter if self.filter_overlay.showing_values => {
                self.toggle_current_facet();
            }
            _ => {}
        }
    }

    fn move_filter_cursor(&mut self, delta: isize) {
        let index = if self.filter_overlay.showing_values {
            let count = self.current_facets().len();
            if count == 0 {
                return;
            }
            self.filter_overlay.value_index = self
                .filter_overlay
                .value_index
                .saturating_add_signed(delta)
                .min(count - 1);
            self.filter_overlay.value_index
        } else {
            self.filter_overlay.field_index = self
                .filter_overlay
                .field_index
                .saturating_add_signed(delta)
                .min(FilterField::ALL.len() - 1);
            self.filter_overlay.field_index
        };
        self.filter_overlay.scroll.ensure_visible(index);
    }

    fn toggle_current_facet(&mut self) {
        let field = self.facet_field();
        let Some(value) = self
            .current_facets()
            .get(self.filter_overlay.value_index)
            .map(|facet| facet.value.clone())
        else {
            return;
        };
        self.toggle_filter(field, &value);
    }

    fn remove_filter_token(&mut self, token: FilterToken) {
        let mut parsed = self.parsed_query();
        match token {
            FilterToken::Bookmarked => parsed.filters.bookmarked = false,
            FilterToken::Field { field, value } => parsed.filters.remove(field, &value),
        }
        self.set_query(format_query(&parsed.filters, &parsed.fuzzy));
    }

    fn handle_columns_key(&mut self, key: KeyEvent) {
        let last = self.layout.columns.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('w') | KeyCode::Enter => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_column(self.column_overlay.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_column((self.column_overlay.index + 1).min(last));
            }
            KeyCode::Char(' ') => {
                self.layout.toggle_visible(self.column_overlay.index);
                self.session_dirty = true;
            }
            KeyCode::Char('K') => {
                self.column_overlay.index = self.layout.move_column(self.column_overlay.index, -1);
                self.session_dirty = true;
            }
            KeyCode::Char('J') => {
                self.column_overlay.index = self.layout.move_column(self.column_overlay.index, 1);
                self.session_dirty = true;
            }
            KeyCode::Left | KeyCode::Char('-') | KeyCode::Char('<') => {
                self.layout.resize(self.column_overlay.index, -1);
                self.session_dirty = true;
            }
            KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('>') => {
                self.layout.resize(self.column_overlay.index, 1);
                self.session_dirty = true;
            }
            _ => {}
        }
    }

    fn focus_column(&mut self, index: usize) {
        self.column_overlay.index = index;
        self.column_overlay.scroll.ensure_visible(index);
    }

    fn handle_palette_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_palette_selection(-1);
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_palette_selection(1);
            }
            KeyCode::Up => self.move_palette_selection(-1),
            KeyCode::Down => self.move_palette_selection(1),
            KeyCode::Enter => return self.run_selected_command(),
            _ => self.edit_palette_query(|query| {
                query.handle_key(key);
            }),
        }
        AppAction::None
    }

    fn move_palette_selection(&mut self, delta: isize) {
        let count = self.palette_commands().len();
        if count == 0 {
            self.palette.selected = 0;
            return;
        }
        self.palette.selected = self
            .palette
            .selected
            .saturating_add_signed(delta)
            .min(count - 1);
        self.palette.scroll.ensure_visible(self.palette.selected);
    }

    fn run_selected_command(&mut self) -> AppAction {
        let Some(command) = self.palette_commands().get(self.palette.selected).copied() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        self.mode = AppMode::Browse;
        self.run_command(command.id)
    }

    fn run_command(&mut self, id: CommandId) -> AppAction {
        match id {
            CommandId::Search => {
                self.begin_search();
                AppAction::None
            }
            CommandId::Palette => {
                self.open_palette();
                AppAction::None
            }
            CommandId::Filters => {
                self.open_facets(0);
                AppAction::None
            }
            CommandId::MoreFilters => {
                self.open_filters();
                AppAction::None
            }
            CommandId::Columns => {
                self.open_columns();
                AppAction::None
            }
            CommandId::Views => {
                self.open_views();
                AppAction::None
            }
            CommandId::EditMenu => {
                self.open_edit_menu();
                AppAction::None
            }
            CommandId::ChangeState => {
                self.open_state_picker();
                AppAction::None
            }
            CommandId::EditTitle => {
                self.open_prompt(PromptField::Title);
                AppAction::None
            }
            CommandId::EditPriority => {
                self.open_priority_picker();
                AppAction::None
            }
            CommandId::EditTags => {
                self.open_prompt(PromptField::Tags);
                AppAction::None
            }
            CommandId::EditAssignee => self.open_assignee_picker(),
            CommandId::EditIteration => self.open_node_picker(NodeKind::Iteration),
            CommandId::EditArea => self.open_node_picker(NodeKind::Area),
            CommandId::AddComment => {
                self.open_prompt(PromptField::Comment);
                AppAction::None
            }
            CommandId::SaveView => {
                self.open_views();
                self.views_overlay.naming =
                    Some(TextInput::new(self.active_view.clone().unwrap_or_default()));
                AppAction::None
            }
            CommandId::Sort => {
                self.sort_draft = SortDraft {
                    field_index: SortField::ALL
                        .iter()
                        .position(|field| *field == self.sort_field)
                        .unwrap_or_default(),
                    direction: self.sort_direction,
                };
                self.mode = AppMode::Sort;
                AppAction::None
            }
            CommandId::Help => {
                self.help.scroll_to(0);
                self.mode = AppMode::Help;
                AppAction::None
            }
            CommandId::Sync => AppAction::Sync,
            CommandId::Open => {
                self.record_history();
                self.open_selected()
            }
            CommandId::ToggleDensity => {
                self.toggle_row_density();
                AppAction::None
            }
            CommandId::ToggleDetails => {
                self.toggle_narrow_details();
                AppAction::None
            }
            CommandId::ToggleSearchOrder => {
                if !self.fuzzy_query().is_empty() {
                    self.toggle_search_order();
                }
                AppAction::None
            }
            CommandId::ToggleBookmark => {
                self.toggle_bookmark();
                AppAction::None
            }
            CommandId::CopyId => self.copy_with(CopiedContent::Id, export::copy_ids),
            CommandId::CopyUrl => self.copy_with(CopiedContent::Url, export::copy_urls),
            CommandId::CopyTitle => self.copy_with(CopiedContent::Title, export::copy_titles),
            CommandId::CopyMarkdown => {
                self.copy_with(CopiedContent::MarkdownLink, export::copy_markdown_links)
            }
            CommandId::CopySummary => {
                self.copy_with(CopiedContent::Summary, export::copy_summaries)
            }
            CommandId::ExportJson => self.export_with("json", export::export_json),
            CommandId::ExportCsv => self.export_with("csv", export::export_csv),
            CommandId::SelectAll => {
                self.selected_keys = self
                    .visible_tickets()
                    .map(|ticket| ticket.key.clone())
                    .collect();
                self.set_status(format!("Selected {} tickets", self.selected_keys.len()));
                AppAction::None
            }
            CommandId::ClearSelection => {
                self.selected_keys.clear();
                self.set_status("Cleared selection");
                AppAction::None
            }
            CommandId::HistoryBack => {
                self.history_back();
                AppAction::None
            }
            CommandId::HistoryForward => {
                self.history_forward();
                AppAction::None
            }
            CommandId::DatabaseInfo => {
                self.mode = AppMode::Info;
                AppAction::None
            }
            CommandId::Quit => {
                self.should_quit = true;
                AppAction::None
            }
            CommandId::ResetPaneSplit => {
                self.reset_pane_split();
                AppAction::None
            }
        }
    }

    fn handle_views_key(&mut self, key: KeyEvent) -> AppAction {
        if self.views_overlay.naming.is_some() {
            match key.code {
                KeyCode::Esc => self.views_overlay.naming = None,
                KeyCode::Enter => {
                    if let Some(name) = self
                        .views_overlay
                        .naming
                        .take()
                        .map(|name| name.text().trim().to_owned())
                        .filter(|name| !name.is_empty())
                    {
                        self.save_view(name);
                    }
                }
                _ => {
                    if let Some(name) = self.views_overlay.naming.as_mut() {
                        name.handle_key(key);
                    }
                }
            }
            return AppAction::None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('V') => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_view(self.views_overlay.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.views.is_empty() {
                    self.focus_view((self.views_overlay.index + 1).min(self.views.len() - 1));
                }
            }
            KeyCode::Enter => self.apply_view_at(self.views_overlay.index),
            KeyCode::Char('n') => self.views_overlay.naming = Some(TextInput::default()),
            KeyCode::Char('d') | KeyCode::Delete => self.delete_view_at(self.views_overlay.index),
            _ => {}
        }
        AppAction::None
    }

    fn focus_view(&mut self, index: usize) {
        self.views_overlay.index = index;
        self.views_overlay.scroll.ensure_visible(index);
    }

    fn save_view(&mut self, name: String) {
        let view = NamedView {
            name: name.clone(),
            query: self.query.text().to_owned(),
            sort_field: self.sort_field,
            sort_direction: self.sort_direction,
            search_order: self.search_order,
            row_density: self.row_density,
            columns: self.layout.to_session_columns(),
            auto_hide: self.layout.auto_hide,
        };
        if let Some(existing) = self
            .views
            .iter_mut()
            .find(|candidate| candidate.name == name)
        {
            *existing = view;
        } else {
            self.views.push(view);
        }
        self.active_view = Some(name.clone());
        self.session_dirty = true;
        self.set_status(format!("Saved view '{name}'"));
    }

    fn apply_view_at(&mut self, index: usize) {
        let Some(view) = self.views.get(index).cloned() else {
            return;
        };
        self.active_view = Some(view.name.clone());
        self.sort_field = view.sort_field;
        self.sort_direction = view.sort_direction;
        self.search_order = view.search_order;
        self.row_density = view.row_density;
        self.layout = TableLayout::from_session_columns(&view.columns, Some(view.auto_hide));
        self.session_dirty = true;
        self.set_query(view.query);
        self.mode = AppMode::Browse;
        self.set_status(format!("Loaded view '{}'", view.name));
    }

    fn delete_view_at(&mut self, index: usize) {
        if index >= self.views.len() {
            return;
        }
        let removed = self.views.remove(index);
        if self.active_view.as_deref() == Some(removed.name.as_str()) {
            self.active_view = None;
        }
        if !self.views.is_empty() {
            self.views_overlay.index = self.views_overlay.index.min(self.views.len() - 1);
        } else {
            self.views_overlay.index = 0;
        }
        self.session_dirty = true;
        self.set_status(format!("Deleted view '{}'", removed.name));
    }

    fn toggle_bookmark(&mut self) {
        let Some(key) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            return;
        };
        if self.bookmarks.remove(&key) {
            self.set_status(format!("Removed bookmark {}", key.id));
        } else {
            self.bookmarks.insert(key.clone());
            self.set_status(format!("Bookmarked {}", key.id));
        }
        self.session_dirty = true;
        if self.parsed_query().filters.bookmarked {
            let selected = Some(key);
            if self.fuzzy_query().is_empty() {
                self.show_all(selected.as_ref());
            } else {
                self.pending_selection = selected;
                self.submit_search();
            }
        }
    }

    fn toggle_row_selection(&mut self) {
        let Some(key) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            return;
        };
        if !self.selected_keys.remove(&key) {
            self.selected_keys.insert(key);
        }
    }

    fn export_targets(&self) -> Vec<&Ticket> {
        if self.selected_keys.is_empty() {
            return self.selected_ticket().into_iter().collect();
        }
        self.tickets()
            .iter()
            .filter(|ticket| self.selected_keys.contains(&ticket.key))
            .collect()
    }

    fn copy_with(&self, content: CopiedContent, formatter: fn(&[&Ticket]) -> String) -> AppAction {
        let tickets = self.export_targets();
        if tickets.is_empty() {
            return AppAction::None;
        }
        AppAction::Copy {
            text: formatter(&tickets),
            content,
        }
    }

    fn export_with(&self, extension: &str, formatter: fn(&[&Ticket]) -> String) -> AppAction {
        let tickets = self.export_targets();
        if tickets.is_empty() {
            return AppAction::None;
        }
        AppAction::WriteFile {
            path: PathBuf::from(format!("ticket-tui-export.{extension}")),
            contents: formatter(&tickets),
        }
    }

    fn record_history(&mut self) {
        let Some(key) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            return;
        };
        if self.recent.last() == Some(&key) {
            return;
        }
        self.recent.push(key);
        if self.recent.len() > 50 {
            self.recent.remove(0);
        }
        self.future.clear();
        self.session_dirty = true;
    }

    fn history_back(&mut self) {
        if self.recent.len() < 2 {
            return;
        }
        let current = self.recent.pop().expect("recent ticket exists");
        self.future.push(current);
        let key = self.recent.last().cloned();
        self.restore_selection(key.as_ref());
        self.session_dirty = true;
    }

    fn history_forward(&mut self) {
        let Some(key) = self.future.pop() else {
            return;
        };
        self.recent.push(key.clone());
        self.restore_selection(Some(&key));
        self.session_dirty = true;
    }

    pub fn snapshot_session(&self) -> Session {
        Session {
            query: self.query.text().to_owned(),
            sort_field: self.sort_field,
            sort_direction: self.sort_direction,
            search_order: self.search_order,
            row_density: self.row_density,
            columns: self.layout.to_session_columns(),
            auto_hide: Some(self.layout.auto_hide),
            bookmarks: self
                .bookmarks
                .iter()
                .map(session::SessionKey::from)
                .collect(),
            recent: self.recent.iter().map(session::SessionKey::from).collect(),
            views: self.views.clone(),
            active_view: self.active_view.clone(),
            selected: self
                .selected_ticket()
                .map(|ticket| session::SessionKey::from(&ticket.key)),
            pane_split_wide: self.pane_split_wide,
            pane_split_stacked: self.pane_split_stacked,
        }
    }

    pub fn restore_session(&mut self, session: Session) {
        self.sort_field = session.sort_field;
        self.sort_direction = session.sort_direction;
        self.search_order = session.search_order;
        self.row_density = session.row_density;
        self.layout = TableLayout::from_session_columns(&session.columns, session.auto_hide);
        self.pane_split_wide = session
            .pane_split_wide
            .clamp(MIN_SPLIT_PERCENT, MAX_SPLIT_PERCENT);
        self.pane_split_stacked = session
            .pane_split_stacked
            .clamp(MIN_SPLIT_PERCENT, MAX_SPLIT_PERCENT);
        self.bookmarks = session.bookmarks.iter().map(TicketKey::from).collect();
        self.recent = session.recent.iter().map(TicketKey::from).collect();
        self.views = session.views;
        self.active_view = session.active_view;
        let selected = session.selected.as_ref().map(TicketKey::from);
        if session.query.is_empty() {
            self.show_all(selected.as_ref());
        } else {
            self.set_query(session.query);
            if let Some(selected) = selected {
                self.pending_selection = Some(selected);
            }
        }
        self.session_dirty = false;
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
        AppMode::Facets => "facets",
        AppMode::Edit => "edit",
        AppMode::StatePicker => "state-picker",
        AppMode::PriorityPicker => "priority-picker",
        AppMode::Prompt => "prompt",
        AppMode::AssigneePicker => "assignee-picker",
        AppMode::NodePicker => "node-picker",
    }
}

const fn focus_name(focus: Focus) -> &'static str {
    match focus {
        Focus::Tickets => "tickets",
        Focus::Family => "family",
        Focus::Details => "details",
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
    snapshot: &pointer::SelectableSnapshot,
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

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::model::StateCategory;

    fn ticket(id: i64, title: &str, changed_at: &str) -> Ticket {
        Ticket {
            key: TicketKey {
                organization: "demo".into(),
                id,
            },
            project: "atlas".into(),
            revision: 1,
            work_item_type: "Task".into(),
            title: title.into(),
            state: "Active".into(),
            reason: None,
            assigned_to: Some("Avery".into()),
            priority: Some(2),
            area_path: "Atlas".into(),
            iteration_path: "Atlas\\Sprint 1".into(),
            tags: vec![],
            description: String::new(),
            description_html: String::new(),
            created_at: crate::timestamp::ts("2026-01-01T00:00:00Z"),
            changed_at: crate::timestamp::ts(changed_at),
            web_url: format!("https://dev.azure.com/demo/atlas/_workitems/edit/{id}"),
            details_rev: 0,
        }
    }

    fn await_search(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.search_pending {
            app.poll_search();
            assert!(Instant::now() < deadline, "search worker timed out");
            thread::yield_now();
        }
    }

    #[test]
    fn agent_context_describes_the_live_ticket_workspace() {
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            ticket(3, "Gamma", "2026-03-01T00:00:00Z"),
        ]);
        app.configure_database(PathBuf::from("/tmp/tickets.sqlite3"), 0);
        app.set_table_viewport(2);
        app.set_query("state:Active".into());
        app.toggle_row_selection();
        app.focus = Focus::Details;
        app.mode = AppMode::Filter;
        app.active_view = Some("Active work".into());

        let context = app.agent_context();

        assert_eq!(context.database_path, "/tmp/tickets.sqlite3");
        assert_eq!(context.mode, "filter");
        assert_eq!(context.focus, "details");
        assert_eq!(context.active_view.as_deref(), Some("Active work"));
        assert_eq!(context.search.filters, vec!["state:Active"]);
        assert_eq!(context.tickets.total_count, 3);
        assert_eq!(context.tickets.matching_count, 3);
        assert_eq!(context.tickets.visible_rows.len(), 2);
        assert_eq!(context.selected_ticket.as_ref().unwrap().id, 3);
        assert!(context.selected_ticket.as_ref().unwrap().checked);
        assert_eq!(context.checked_tickets.len(), 1);
        assert_eq!(context.checked_tickets[0].id, 3);

        let mut mine = app.tickets()[0].clone();
        mine.assigned_to = Some("  avery CHEN ".into());
        let mut theirs = app.tickets()[1].clone();
        theirs.assigned_to = Some("Jordan Patel".into());
        let mut unassigned = app.tickets()[1].clone();
        unassigned.assigned_to = None;
        assert!(!app.is_mine(&mine), "nobody is \"me\" until a name is set");

        app.set_me(Some("Avery Chen".into()));

        assert_eq!(app.me(), Some("Avery Chen"));
        assert!(app.is_mine(&mine), "casing and padding do not matter");
        assert!(!app.is_mine(&theirs));
        assert!(!app.is_mine(&unassigned));
        assert_eq!(app.agent_context().me.as_deref(), Some("Avery Chen"));
    }

    #[test]
    fn search_order_switches_between_relevance_and_field_sorting_and_keeps_the_selection() {
        let mut app = App::new(vec![
            ticket(1, "Search alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Search beta", "2026-02-01T00:00:00Z"),
        ]);
        app.select_row(1);
        let selected = app.selected_ticket().unwrap().key.clone();
        assert_eq!(selected.id, 1, "the newest ticket leads by default");

        app.set_query("search".into());
        await_search(&mut app);
        app.set_sort(SortField::Title, SortDirection::Ascending);
        assert_eq!(app.selected_ticket().unwrap().key, selected);

        app.visible = vec![
            SearchMatch {
                ticket_index: 1,
                score: 100,
            },
            SearchMatch {
                ticket_index: 0,
                score: 1,
            },
        ];
        app.sort_visible();
        assert_eq!(app.search_order, SearchOrder::Relevance);
        assert_eq!(
            app.visible_tickets().next().unwrap().key.id,
            2,
            "relevance leads with the best scoring match"
        );

        app.toggle_search_order();

        assert_eq!(app.search_order, SearchOrder::Field);
        assert_eq!(
            app.visible_tickets().next().unwrap().key.id,
            1,
            "field order falls back to the sort column"
        );

        let mut without_fuzzy = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        let order = without_fuzzy.search_order;
        without_fuzzy.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        assert_eq!(
            without_fuzzy.search_order, order,
            "there is nothing to re-rank without a fuzzy query"
        );
    }

    #[test]
    fn pasting_fills_the_search_editor_and_escape_clears_the_query() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.mode = AppMode::Search;
        app.handle_paste("search\n");
        assert_eq!(app.query(), "search ");
        assert_eq!(app.query_cursor(), 7);
        app.mode = AppMode::Browse;

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(app.query().is_empty());
        assert_eq!(app.visible_count(), 1);
    }

    #[test]
    fn reload_during_search_does_not_keep_stale_indices() {
        let mut app = App::new(vec![
            ticket(1, "Search alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Search beta", "2026-02-01T00:00:00Z"),
        ]);
        app.set_query("search".into());
        await_search(&mut app);

        app.replace_tickets(vec![ticket(1, "Search alpha", "2026-01-01T00:00:00Z")]);

        assert_eq!(app.visible_count(), 0);
        await_search(&mut app);
        assert_eq!(app.visible_count(), 1);
        assert_eq!(app.selected_ticket().unwrap().key.id, 1);
    }

    #[test]
    fn sorting_and_reload_keep_the_view_context_unless_the_selection_is_gone() {
        let original = vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            ticket(3, "Gamma", "2026-03-01T00:00:00Z"),
        ];
        let mut app = App::new(original.clone());
        assert_eq!(
            app.visible_tickets().next().unwrap().key.id,
            3,
            "tickets start sorted by most recently changed"
        );
        app.select_row(1);
        let selected = app.selected_ticket().unwrap().key.clone();
        app.details.set_viewport(0, 5);
        app.details.scroll_to(3);
        app.table.offset = 1;
        app.table.viewport = 2;

        app.set_sort(SortField::Title, SortDirection::Descending);
        assert_eq!(app.selected_ticket().unwrap().key, selected);
        assert_eq!(app.details.offset, 3);
        assert_eq!(app.table.offset, 1);

        app.replace_tickets(original);
        assert_eq!(app.selected_ticket().unwrap().key, selected);
        assert_eq!(app.details.offset, 3);
        assert_eq!(app.table.offset, 1);

        app.replace_tickets(vec![ticket(9, "Delta", "2026-03-01T00:00:00Z")]);
        assert_eq!(app.selected_ticket().unwrap().key.id, 9);
        assert_eq!(app.details.offset, 0, "a lost selection resets the details");
        assert_eq!(app.table.offset, 0, "a lost selection resets the table");
    }

    #[test]
    fn structured_query_filters_tickets_and_keeps_fuzzy_search() {
        let mut app = App::new(vec![
            ticket(1, "Search alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Other beta", "2026-02-01T00:00:00Z"),
        ]);
        app.set_query("state:active search".into());
        await_search(&mut app);

        assert_eq!(app.visible_count(), 1);
        assert_eq!(app.visible_tickets().next().unwrap().key.id, 1);
        assert_eq!(app.fuzzy_query(), "search");
        assert_eq!(app.filter_tokens().len(), 1);
    }

    #[test]
    fn a_facet_toggle_rewrites_the_query_and_removing_the_chip_clears_it() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.open_filters();
        app.filter_overlay.showing_values = true;
        app.filter_overlay.field_index = 0;
        app.toggle_current_facet();

        assert!(app.query().contains("state:"));
        let token = app.filter_tokens().pop().unwrap();
        app.remove_filter_token(token);
        assert!(app.query().is_empty());
    }

    #[test]
    fn named_views_restore_query_and_sort() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.set_query("state:active".into());
        app.set_sort(SortField::Title, SortDirection::Ascending);
        app.save_view("Active".into());
        app.set_query(String::new());
        app.set_sort(SortField::Changed, SortDirection::Descending);

        app.apply_view_at(0);

        assert_eq!(app.query(), "state:active");
        assert_eq!(app.sort_field, SortField::Title);
        assert_eq!(app.active_view.as_deref(), Some("Active"));
    }

    #[test]
    fn bookmarks_multi_select_and_copy_use_selected_tickets() {
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
        ]);
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        assert!(app.is_bookmarked(&app.selected_ticket().unwrap().key));

        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        app.select_row(1);
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        let action = app.copy_with(CopiedContent::Id, export::copy_ids);
        assert_eq!(
            action,
            AppAction::Copy {
                text: "1\n2\n".into(),
                content: CopiedContent::Id,
            }
        );
    }

    #[test]
    fn command_palette_runs_density_toggle() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.open_palette();
        app.palette.query = TextInput::new("density");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.row_density, RowDensity::Comfortable);
        assert_eq!(app.mode, AppMode::Browse);
    }

    #[test]
    fn every_bound_key_runs_its_command_from_browse_mode() {
        for command in crate::command::COMMANDS
            .iter()
            .filter(|command| !command.keys.is_empty())
        {
            for key in command.keys {
                let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
                app.handle_key(KeyEvent::new(key.code, key.modifiers));
                let expected = match command.id {
                    CommandId::Sort => Some(AppMode::Sort),
                    CommandId::Help => Some(AppMode::Help),
                    CommandId::Views => Some(AppMode::Views),
                    CommandId::Columns => Some(AppMode::Columns),
                    CommandId::Palette => Some(AppMode::Palette),
                    CommandId::DatabaseInfo => Some(AppMode::Info),
                    CommandId::Search => Some(AppMode::Search),
                    CommandId::Filters => Some(AppMode::Facets),
                    CommandId::MoreFilters => Some(AppMode::Filter),
                    CommandId::EditMenu => Some(AppMode::Edit),
                    CommandId::ChangeState => Some(AppMode::StatePicker),
                    _ => None,
                };
                if let Some(mode) = expected {
                    assert_eq!(app.mode, mode, "{:?} via {}", command.id, key.label());
                }
            }
        }
    }

    fn family_key(id: i64) -> TicketKey {
        TicketKey {
            organization: "demo".into(),
            id,
        }
    }

    fn family_app() -> App {
        let mut parent = ticket(1, "Parent", "2026-01-01T00:00:00Z");
        parent.work_item_type = "Feature".into();
        let mut child = ticket(2, "Child", "2026-02-01T00:00:00Z");
        child.work_item_type = "Task".into();
        let grandchild = ticket(3, "Grandchild", "2026-01-15T00:00:00Z");
        let mut app = App::new(vec![parent, child, grandchild]);
        app.set_workspace_graph(TicketGraph {
            relations: vec![
                RelationRecord {
                    from: family_key(2),
                    to: family_key(1),
                    kind: crate::model::RelationKind::Parent,
                },
                RelationRecord {
                    from: family_key(3),
                    to: family_key(2),
                    kind: crate::model::RelationKind::Parent,
                },
            ],
            ..TicketGraph::default()
        });
        app
    }

    fn press(app: &mut App, code: KeyCode) -> AppAction {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn pane_keys_move_focus_and_only_the_details_pane_opens_on_enter() {
        let mut app = family_app();
        assert_eq!(app.focus, Focus::Tickets);

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Details);
        assert!(app.narrow_details, "the narrow layout follows the focus");

        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.focus, Focus::Tickets);
        assert!(!app.narrow_details);

        app.focus = Focus::Family;
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Details);

        app.focus = Focus::Tickets;
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            AppAction::None,
            "Enter must not open a browser from the tickets pane"
        );
        assert!(matches!(
            press(&mut app, KeyCode::Char('o')),
            AppAction::OpenUrl(_)
        ));
        app.focus = Focus::Details;
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            AppAction::OpenUrl(_)
        ));
    }

    #[test]
    fn family_cursor_movement_clamps_and_scrolls_the_details_viewport() {
        let mut app = family_app();
        app.focus = Focus::Family;
        app.details.set_viewport(2, 20);

        press(&mut app, KeyCode::Home);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.family_cursor.as_ref().map(|key| key.id), Some(1));
        assert_eq!(app.details.offset, 0);

        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.family_cursor.as_ref().map(|key| key.id), Some(3));
        assert!(
            app.details.offset > 0,
            "the details pane scrolls to keep the cursor visible"
        );
    }

    #[test]
    fn family_enter_selects_visible_tickets_records_history_once_and_explains_hidden_ones() {
        let mut app = family_app();
        assert_eq!(app.selected_ticket().unwrap().key.id, 2);
        app.focus = Focus::Family;

        let opened = press(&mut app, KeyCode::Char('o'));
        assert!(matches!(opened, AppAction::OpenUrl(_)));
        assert_eq!(app.selected_ticket().unwrap().key.id, 2);

        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.selected_ticket().unwrap().key.id, 3);
        assert_eq!(app.focus, Focus::Family);
        assert_eq!(
            app.recent.iter().map(|key| key.id).collect::<Vec<_>>(),
            vec![2, 3]
        );

        press(&mut app, KeyCode::Char('['));
        assert_eq!(app.selected_ticket().unwrap().key.id, 2);

        app.visible
            .retain(|entry| app.tickets[entry.ticket_index].key.id != 3);
        app.family_cursor = Some(family_key(3));
        let query = app.query().to_owned();
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.selected_ticket().unwrap().key.id, 2);
        assert_eq!(app.query(), query, "a hidden target changes no search");
        assert_eq!(
            app.notification(),
            Some(("3 is hidden by the current search", NotificationLevel::Info))
        );
    }

    #[test]
    fn a_background_sync_leaves_the_search_box_and_the_selection_alone() {
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
        ]);
        press(&mut app, KeyCode::Char('/'));
        for character in "alp".chars() {
            press(&mut app, KeyCode::Char(character));
        }
        await_search(&mut app);
        let selected = app.selected_ticket().unwrap().key.clone();

        // The sync worker's rows land while the user is still typing.
        let mut refreshed = app.tickets().to_vec();
        refreshed.push(ticket(3, "Gamma", "2026-03-01T00:00:00Z"));
        app.replace_prepared_tickets(PreparedTickets::new(refreshed));
        await_search(&mut app);

        assert_eq!(app.mode, AppMode::Search);
        assert_eq!(app.query(), "alp");
        assert_eq!(app.tickets().len(), 3);
        assert_eq!(app.selected_ticket().unwrap().key, selected);

        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.query(), "alph", "the caret stayed where it was");
    }

    #[test]
    fn family_selection_and_cursor_restore_after_reload() {
        let mut app = family_app();
        app.focus = Focus::Family;
        press(&mut app, KeyCode::Down);
        assert_eq!(app.family_cursor.as_ref().map(|key| key.id), Some(3));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.selected_ticket().unwrap().key.id, 3);

        let graph = app.graph.clone();
        let tickets = app.tickets().to_vec();
        app.replace_prepared_tickets(PreparedTickets::with_graph(tickets, graph));

        assert_eq!(app.selected_ticket().unwrap().key.id, 3);
        assert_eq!(app.family_cursor.as_ref().map(|key| key.id), Some(3));
        assert_eq!(
            app.visible_family_tree()
                .iter()
                .map(|entry| entry.key.id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn clicking_the_pane_divider_neither_acts_nor_selects_text() {
        let mut app = App::new(vec![ticket(1, "One", "2026-01-02T00:00:00Z")]);
        let rect = Rect {
            x: 60,
            y: 5,
            width: 1,
            height: 10,
        };
        app.set_content_layout(
            Rect {
                x: 0,
                y: 4,
                width: 130,
                height: 20,
            },
            Some(DividerOrientation::Vertical),
        );
        // A selectable pane sits under the divider; pressing the divider must
        // still not start a selection in it.
        app.hit_regions.push(pointer::region(
            Rect {
                x: 0,
                y: 4,
                width: 130,
                height: 20,
            },
            PointerTarget::FocusDetails,
            pointer::PointerLayer::Base,
            Some(SelectableSurface::Details),
            None,
        ));
        app.hit_regions.push(pointer::region(
            rect,
            PointerTarget::PaneDivider,
            pointer::PointerLayer::Base,
            None,
            None,
        ));
        app.session_dirty = false;

        let point = |kind| MouseEvent {
            kind,
            column: rect.x,
            row: rect.y + 3,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(point(MouseEventKind::Down(MouseButton::Left)));
        let update = app.handle_mouse(point(MouseEventKind::Up(MouseButton::Left)));

        assert!(matches!(update.action, AppAction::None));
        assert!(app.selection().is_none(), "a divider press selects no text");
        assert_eq!(app.pane_split_wide, DEFAULT_PANE_SPLIT_WIDE);
        assert!(!app.session_dirty, "a press with no drag changes nothing");

        app.pane_split_wide = 71;
        app.pane_split_stacked = 45;
        let session = app.snapshot_session();
        let mut restored = App::new(vec![ticket(1, "One", "2026-01-02T00:00:00Z")]);
        restored.restore_session(session);
        assert_eq!(restored.pane_split_wide, 71, "the split is remembered");
        assert_eq!(restored.pane_split_stacked, 45);

        restored.session_dirty = false;
        restored.run_command(CommandId::ResetPaneSplit);
        assert_eq!(restored.pane_split_wide, DEFAULT_PANE_SPLIT_WIDE);
        assert_eq!(restored.pane_split_stacked, DEFAULT_PANE_SPLIT_STACKED);
        assert!(restored.session_dirty);
    }

    /// Three work items over a configured Azure DevOps project, which is what
    /// an edit needs to go anywhere.
    fn editing_app() -> App {
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            ticket(3, "Gamma", "2026-03-01T00:00:00Z"),
        ]);
        app.enable_sync();
        app.set_table_viewport(3);
        app
    }

    fn edit_request(app: &mut App, edit: FieldEdit) -> EditRequest {
        match app.edit_selected(edit) {
            AppAction::Edit(request) => request,
            other => panic!("expected an edit to be dispatched, got {other:?}"),
        }
    }

    /// The work item as Azure DevOps hands it back: the field written, and the
    /// revision and changed date it decided on.
    fn stored_copy(app: &App, key: &TicketKey, state: &str) -> Ticket {
        let mut ticket = app.ticket_by_key(key).expect("the row is loaded").clone();
        ticket.state = state.to_owned();
        ticket.revision += 1;
        ticket.changed_at = crate::timestamp::ts("2026-04-01T00:00:00Z");
        ticket
    }

    #[test]
    fn an_edit_shows_at_once_and_the_stored_copy_replaces_it() {
        let mut app = editing_app();
        let request = edit_request(&mut app, FieldEdit::state("Doing"));
        let key = request.key.clone();

        assert_eq!(request.expected_revision, 1, "the row's revision is tested");
        assert_eq!(request.edit.summary(), "State → Doing");
        assert!(app.edits_pending());
        assert_eq!(
            app.ticket_by_key(&key).unwrap().state,
            "Doing",
            "the row does not wait for the network"
        );

        app.set_query("Doing".into());
        await_search(&mut app);
        assert_eq!(
            app.visible_count(),
            1,
            "the search index follows the optimistic value"
        );
        app.set_query(String::new());
        await_search(&mut app);

        let stored = stored_copy(&app, &key, "Doing");
        app.apply_edit(EditApplied {
            ticket: stored.clone(),
            relations: Vec::new(),
            edit: FieldEdit::state("Doing"),
        });

        assert!(!app.edits_pending());
        assert_eq!(app.ticket_by_key(&key), Some(&stored), "the server wins");
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Updated #3 · State → Doing")
        );
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.key.id),
            Some(key.id),
            "the selection stays on the work item it was on"
        );
    }

    #[test]
    fn a_refused_edit_puts_the_row_back_and_names_the_field() {
        let mut app = editing_app();
        let request = edit_request(&mut app, FieldEdit::state("Doing"));
        let before = app.tickets().to_vec();

        app.reject_edit(&EditRejection {
            key: request.key.clone(),
            label: "State".into(),
            conflict: true,
            message: "the test operation on /rev failed".into(),
        });

        assert!(!app.edits_pending());
        assert_eq!(
            app.ticket_by_key(&request.key).unwrap().state,
            "Active",
            "a refused write leaves nothing of itself behind"
        );
        assert_ne!(before, app.tickets());
        let (message, level) = app.notification().expect("a refusal is always reported");
        assert!(message.contains("#3 changed in Azure DevOps"), "{message}");
        assert!(message.contains("State not saved"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
    }

    #[test]
    fn a_pull_that_lands_during_an_edit_keeps_the_optimistic_value() {
        let mut app = editing_app();
        let request = edit_request(&mut app, FieldEdit::state("Doing"));
        let key = request.key.clone();

        // A pull that was already in flight when the edit went out: it cannot
        // know about the edit, but it must not undo it on screen either.
        let mut pulled = ticket(3, "Gamma renamed", "2026-03-02T00:00:00Z");
        pulled.revision = 4;
        app.replace_prepared_tickets(PreparedTickets::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            pulled.clone(),
        ]));

        let row = app.ticket_by_key(&key).expect("the row survived the pull");
        assert_eq!(row.state, "Doing", "the edit is still showing");
        assert_eq!(row.title, "Gamma renamed", "everything else is the pull's");
        assert!(app.edits_pending());

        app.reject_edit(&EditRejection {
            key: key.clone(),
            label: "State".into(),
            conflict: false,
            message: "field is read only".into(),
        });
        assert_eq!(
            app.ticket_by_key(&key),
            Some(&pulled),
            "a refusal restores the freshest copy the edit did not make"
        );
    }

    #[test]
    fn an_edit_leaves_the_filtered_view_only_once_it_lands() {
        let mut app = editing_app();
        app.set_query("state:Active".into());
        assert_eq!(app.visible_count(), 3);

        let request = edit_request(&mut app, FieldEdit::state("Done"));
        assert_eq!(
            app.visible_count(),
            3,
            "the row stays where it is while the write is in flight"
        );

        let stored = stored_copy(&app, &request.key, "Done");
        app.apply_edit(EditApplied {
            ticket: stored,
            relations: Vec::new(),
            edit: request.edit.clone(),
        });

        assert_eq!(
            app.visible_count(),
            2,
            "the filter drops the row when the change lands"
        );
        assert_eq!(app.query(), "state:Active", "the query is left alone");
    }

    #[test]
    fn an_offline_app_refuses_an_edit_and_changes_nothing() {
        let mut app = App::new(vec![ticket(1, "Alpha", "2026-01-01T00:00:00Z")]);
        app.set_offline_reason(Some("no Azure DevOps organization; pass --org".into()));

        assert_eq!(
            app.edit_selected(FieldEdit::state("Doing")),
            AppAction::None
        );

        assert_eq!(app.tickets()[0].state, "Active");
        assert!(!app.edits_pending());
        let (message, level) = app.notification().expect("the refusal is reported");
        assert!(message.contains("State not saved"), "{message}");
        assert!(message.contains("--org"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
    }

    #[test]
    fn a_second_edit_of_the_same_row_waits_for_the_first_to_answer() {
        let mut app = editing_app();
        let request = edit_request(&mut app, FieldEdit::state("Doing"));

        assert_eq!(app.edit_selected(FieldEdit::state("Done")), AppAction::None);
        assert_eq!(app.ticket_by_key(&request.key).unwrap().state, "Doing");
        let (message, _) = app.notification().unwrap();
        assert!(
            message.contains("an earlier edit is still in flight"),
            "{message}"
        );

        app.apply_edit(EditApplied {
            ticket: stored_copy(&app, &request.key, "Doing"),
            relations: Vec::new(),
            edit: request.edit,
        });
        assert!(
            matches!(
                app.edit_selected(FieldEdit::state("Done")),
                AppAction::Edit(_)
            ),
            "the next edit goes out once the first has answered"
        );
    }

    /// The states a Basic-process Task moves through, as a sync would have
    /// cached them.
    fn task_states() -> Vec<StateOption> {
        vec![
            StateOption::new("To Do", StateCategory::Proposed),
            StateOption::new("Doing", StateCategory::InProgress),
            StateOption::new("Done", StateCategory::Completed),
        ]
    }

    /// An editable app whose rows are all in the first state, with the states
    /// their type allows already cached.
    fn picker_app() -> App {
        let mut tickets = vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            ticket(3, "Gamma", "2026-03-01T00:00:00Z"),
        ];
        for ticket in &mut tickets {
            ticket.state = "To Do".into();
        }
        let mut app = App::new(tickets);
        app.enable_sync();
        app.set_table_viewport(3);
        let mut catalog = StateCatalog::default();
        catalog.insert("Task", task_states());
        app.set_state_catalog(catalog);
        app
    }

    fn state_names(options: &[StateOption]) -> Vec<&str> {
        options.iter().map(|option| option.name.as_str()).collect()
    }

    fn shift(app: &mut App, ch: char) -> AppAction {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::SHIFT))
    }

    #[test]
    fn the_state_picker_opens_on_the_current_state_and_enter_writes_the_one_chosen() {
        let mut app = picker_app();

        assert_eq!(shift(&mut app, 'S'), AppAction::None);
        assert_eq!(app.mode, AppMode::StatePicker);
        assert_eq!(
            state_names(&app.state_picker.options),
            ["To Do", "Doing", "Done"]
        );
        assert_eq!(app.state_picker.current, "To Do");
        assert_eq!(
            app.state_picker.index, 0,
            "the state the work item is in starts under the cursor"
        );
        assert_eq!(app.state_picker.id, 3, "the picker names the selected row");

        press(&mut app, KeyCode::Down);
        let AppAction::Edit(request) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing another state should dispatch an edit");
        };

        assert_eq!(app.mode, AppMode::Browse);
        assert_eq!(request.key.id, 3);
        assert_eq!(
            request.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                serde_json::json!({"op": "add", "path": "/fields/System.State", "value": "Doing"}),
            ]
        );
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.state.as_str()),
            Some("Doing"),
            "the row shows the new state without waiting for Azure DevOps"
        );
        assert!(app.edits_pending());
    }

    #[test]
    fn choosing_the_current_state_or_pressing_escape_writes_nothing() {
        let mut app = picker_app();

        shift(&mut app, 'S');
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            AppAction::None,
            "the state it is already in is a no-op"
        );
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None, "a no-op closes silently");

        shift(&mut app, 'S');
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Esc), AppAction::None);
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None);
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.state.as_str()),
            Some("To Do"),
            "cancelling leaves the row exactly as it was"
        );
    }

    #[test]
    fn the_edit_menu_lists_the_field_editors_and_opens_the_one_chosen() {
        let mut app = picker_app();

        assert_eq!(press(&mut app, KeyCode::Char('e')), AppAction::None);
        assert_eq!(app.mode, AppMode::Edit);
        assert_eq!(
            EDIT_MENU
                .iter()
                .map(|entry| entry.label)
                .collect::<Vec<_>>(),
            [
                "State",
                "Title",
                "Priority",
                "Tags",
                "Assignee",
                "Iteration",
                "Area",
                "Add comment"
            ],
            "later field editors append their own row"
        );
        assert_eq!(app.edit_menu.index, 0);

        assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
        assert_eq!(app.mode, AppMode::StatePicker);
        assert_eq!(
            state_names(&app.state_picker.options),
            ["To Do", "Doing", "Done"]
        );

        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.mode, AppMode::Edit);
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.mode, AppMode::Browse, "e closes the menu it opened");
    }

    /// An editable app whose selected row — the most recently changed one — has
    /// a priority and a tag to open the field editors on.
    fn edit_app() -> App {
        let mut gamma = ticket(3, "Gamma", "2026-03-01T00:00:00Z");
        gamma.priority = Some(1);
        gamma.tags = vec!["rust".into()];
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            gamma,
        ]);
        app.enable_sync();
        app.set_table_viewport(3);
        app
    }

    /// Opens the Edit menu and runs the row at `index`, the way a hand does.
    fn open_editor(app: &mut App, index: usize) {
        press(app, KeyCode::Char('e'));
        for _ in 0..index {
            press(app, KeyCode::Down);
        }
        press(app, KeyCode::Enter);
    }

    fn prompt_text(app: &App) -> String {
        app.prompt
            .as_ref()
            .expect("a prompt should be open")
            .input
            .text()
            .to_owned()
    }

    /// Clears the prompt and types `text` into it, one key at a time.
    fn type_over(app: &mut App, text: &str) {
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        for character in text.chars() {
            press(app, KeyCode::Char(character));
        }
    }

    #[test]
    fn the_title_prompt_opens_on_the_current_title_and_saves_a_trimmed_one() {
        let mut app = edit_app();

        open_editor(&mut app, 1);
        assert_eq!(app.mode, AppMode::Prompt);
        assert_eq!(prompt_text(&app), "Gamma", "the prompt opens prefilled");

        type_over(&mut app, "  Renamed gamma  ");
        let AppAction::Edit(request) = press(&mut app, KeyCode::Enter) else {
            panic!("a new title should dispatch an edit");
        };

        assert_eq!(app.mode, AppMode::Browse);
        assert!(app.prompt.is_none());
        assert_eq!(request.key.id, 3);
        assert_eq!(
            request.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                serde_json::json!({
                    "op": "add",
                    "path": "/fields/System.Title",
                    "value": "Renamed gamma",
                }),
            ],
            "the title is trimmed before it is sent"
        );
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.title.as_str()),
            Some("Renamed gamma"),
            "the row shows the new title without waiting for Azure DevOps"
        );
    }

    #[test]
    fn an_empty_title_is_refused_locally_and_an_unchanged_one_writes_nothing() {
        let mut app = edit_app();

        open_editor(&mut app, 1);
        type_over(&mut app, "   ");
        assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
        assert_eq!(
            app.mode,
            AppMode::Prompt,
            "a blank title leaves the prompt open to fix"
        );
        assert!(!app.edits_pending(), "nothing was sent");
        let (message, level) = app.notification().expect("a refusal is reported");
        assert!(message.contains("title cannot be empty"), "{message}");
        assert_eq!(level, NotificationLevel::Error);

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, AppMode::Browse);
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.title.as_str()),
            Some("Gamma"),
            "cancelling leaves the row exactly as it was"
        );

        let mut app = edit_app();
        open_editor(&mut app, 1);
        assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
        assert_eq!(
            app.notification(),
            None,
            "an unchanged title closes silently"
        );
    }

    #[test]
    fn the_priority_picker_opens_on_the_current_value_and_writes_the_one_chosen() {
        let mut app = edit_app();

        open_editor(&mut app, 2);
        assert_eq!(app.mode, AppMode::PriorityPicker);
        assert_eq!(app.priority_picker.current, Some(1));
        assert_eq!(
            app.priority_picker.index, 0,
            "the priority the work item has starts under the cursor"
        );
        assert_eq!(app.priority_picker.id, 3);

        assert_eq!(
            press(&mut app, KeyCode::Enter),
            AppAction::None,
            "the priority it already has is a no-op"
        );
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None);

        open_editor(&mut app, 2);
        press(&mut app, KeyCode::Down);
        let AppAction::Edit(request) = press(&mut app, KeyCode::Enter) else {
            panic!("another priority should dispatch an edit");
        };
        assert_eq!(
            request.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                serde_json::json!({
                    "op": "add",
                    "path": "/fields/Microsoft.VSTS.Common.Priority",
                    "value": 2,
                }),
            ]
        );
        assert_eq!(
            app.selected_ticket().and_then(|ticket| ticket.priority),
            Some(2),
            "the Pri cell shows the new priority at once"
        );
    }

    #[test]
    fn clearing_the_priority_removes_the_field_and_empties_the_cell() {
        let mut app = edit_app();

        open_editor(&mut app, 2);
        press(&mut app, KeyCode::End);
        let AppAction::Edit(request) = press(&mut app, KeyCode::Enter) else {
            panic!("Clear should dispatch an edit");
        };
        assert_eq!(
            request.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                serde_json::json!({
                    "op": "remove",
                    "path": "/fields/Microsoft.VSTS.Common.Priority",
                }),
            ],
            "a priority goes back to unset by being removed"
        );
        assert_eq!(
            app.selected_ticket().and_then(|ticket| ticket.priority),
            None
        );
    }

    #[test]
    fn the_tags_prompt_trims_deduplicates_and_rejoins_what_it_saves() {
        let mut app = edit_app();

        open_editor(&mut app, 3);
        assert_eq!(app.mode, AppMode::Prompt);
        assert_eq!(
            prompt_text(&app),
            "rust",
            "the prompt opens on the tags held"
        );

        type_over(&mut app, "rust; Rust ;; tui");
        let AppAction::Edit(request) = press(&mut app, KeyCode::Enter) else {
            panic!("a new tag list should dispatch an edit");
        };
        assert_eq!(
            request.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                serde_json::json!({
                    "op": "add",
                    "path": "/fields/System.Tags",
                    "value": "rust; tui",
                }),
            ]
        );
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.tags.clone()),
            Some(vec!["rust".to_owned(), "tui".to_owned()]),
            "the Tags cell shows the normalised list at once"
        );
    }

    #[test]
    fn a_tag_list_that_normalises_to_what_is_there_writes_nothing() {
        let mut app = edit_app();

        open_editor(&mut app, 3);
        type_over(&mut app, "  rust ;; RUST ");
        assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None);
    }

    /// The Edit menu row that opens the comment box, found by the command it
    /// runs so adding a field editor above it moves nothing here.
    fn comment_row() -> usize {
        EDIT_MENU
            .iter()
            .position(|entry| entry.command == CommandId::AddComment)
            .expect("the Edit menu offers a comment row")
    }

    /// One comment as Azure DevOps hands it back, already carrying the id,
    /// date, and author only the server can give it.
    fn comment(id: i64, at: &str, text: &str) -> CommentRecord {
        CommentRecord {
            ticket: TicketKey {
                organization: "demo".into(),
                id: 3,
            },
            comment_id: id,
            created_at: crate::timestamp::ts(at),
            author: Some("Jacob Ragsdale".into()),
            text: text.into(),
        }
    }

    #[test]
    fn the_comment_prompt_opens_empty_and_posts_what_was_typed() {
        let mut app = edit_app();

        open_editor(&mut app, comment_row());
        assert_eq!(app.mode, AppMode::Prompt);
        assert_eq!(
            prompt_text(&app),
            "",
            "there is nothing to edit, only to say"
        );
        let prompt = app.prompt.as_ref().expect("a prompt should be open");
        assert_eq!(prompt.field, PromptField::Comment);
        assert_eq!(
            prompt.field.title(prompt.id),
            "Comment on #3",
            "the prompt names the work item it is about"
        );

        type_over(&mut app, "  Merged into main  ");
        let action = press(&mut app, KeyCode::Enter);
        assert_eq!(
            action,
            AppAction::Comment {
                key: app.selected_ticket().unwrap().key.clone(),
                text: "Merged into main".into(),
            },
            "the comment is trimmed before it is sent"
        );
        assert_eq!(app.mode, AppMode::Browse);
        assert!(app.prompt.is_none());
        assert!(
            app.comments_pending(),
            "the post is waiting on Azure DevOps"
        );
        assert!(
            app.comments_for(&app.selected_ticket().unwrap().key)
                .is_empty(),
            "nothing is shown until the server has stored it"
        );

        assert_eq!(
            app.comment_selected("And again".into()),
            AppAction::None,
            "one comment at a time"
        );
        let (message, level) = app.notification().expect("the second attempt says so");
        assert!(message.contains("still in flight"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
    }

    #[test]
    fn a_blank_comment_is_refused_locally_and_leaves_the_prompt_open() {
        let mut app = edit_app();

        open_editor(&mut app, comment_row());
        type_over(&mut app, "   ");
        assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
        assert_eq!(
            app.mode,
            AppMode::Prompt,
            "a blank comment leaves the prompt open to fix"
        );
        assert!(!app.comments_pending(), "nothing was sent");
        let (message, level) = app.notification().expect("a refusal is reported");
        assert!(message.contains("comment cannot be empty"), "{message}");
        assert_eq!(level, NotificationLevel::Error);

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, AppMode::Browse);
        assert!(app.prompt.is_none());
    }

    #[test]
    fn a_stored_comment_joins_the_discussion_in_date_order() {
        let mut app = edit_app();
        let key = app.selected_ticket().unwrap().key.clone();

        app.comment_selected("Merged into main".into());
        app.apply_comment(comment(9, "2026-03-04T00:00:00Z", "Merged into main"));

        assert!(!app.comments_pending(), "the post was answered");
        assert_eq!(
            app.comments_for(&key)
                .iter()
                .map(|held| held.text.as_str())
                .collect::<Vec<_>>(),
            ["Merged into main"]
        );
        let (message, level) = app.notification().expect("the post reports itself");
        assert_eq!(message, "Commented on #3");
        assert_eq!(level, NotificationLevel::Info);

        // A details fetch that lands afterwards brings the same comment back;
        // it replaces the one already held rather than doubling it, and an
        // older comment files ahead of it.
        app.apply_comment(comment(5, "2026-03-01T00:00:00Z", "Blocked on the API"));
        app.apply_comment(comment(9, "2026-03-04T00:00:00Z", "Merged into main"));
        assert_eq!(
            app.comments_for(&key)
                .iter()
                .map(|held| held.text.as_str())
                .collect::<Vec<_>>(),
            ["Blocked on the API", "Merged into main"]
        );
    }

    #[test]
    fn a_refused_comment_changes_nothing_and_says_why() {
        let mut app = edit_app();
        let key = app.selected_ticket().unwrap().key.clone();

        app.comment_selected("Merged into main".into());
        app.reject_comment(&key, "HTTP 403: the work item is read only");

        assert!(app.comments_for(&key).is_empty(), "nothing was filed");
        assert!(!app.comments_pending(), "the row is free to try again");
        let (message, level) = app.notification().expect("a refusal is reported");
        assert_eq!(
            message,
            "#3 comment not posted: HTTP 403: the work item is read only"
        );
        assert_eq!(level, NotificationLevel::Error);

        assert!(
            matches!(
                app.comment_selected("Merged into main".into()),
                AppAction::Comment { .. }
            ),
            "a refusal does not block the next attempt"
        );
    }

    #[test]
    fn a_prompt_takes_a_paste_at_its_caret() {
        let mut app = edit_app();

        open_editor(&mut app, 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.handle_paste("Pasted\ttitle");
        assert_eq!(prompt_text(&app), "Pastedtitle");
    }

    #[test]
    fn the_picker_lists_cached_states_and_otherwise_the_ones_already_in_the_database() {
        let typed = |id: i64, work_item_type: &str, state: &str| {
            let mut ticket = ticket(id, "Row", "2026-01-01T00:00:00Z");
            ticket.work_item_type = work_item_type.to_owned();
            ticket.state = state.to_owned();
            ticket
        };
        let mut app = App::new(vec![
            typed(1, "Bug", "Done"),
            typed(2, "Bug", "New"),
            typed(3, "Bug", "Active"),
            typed(4, "Bug", "New"),
            typed(5, "Bug", "Approved"),
            typed(6, "Task", "Doing"),
        ]);

        assert_eq!(
            state_names(&app.states_for("Bug")),
            ["Approved", "New", "Active", "Done"],
            "the fallback runs Proposed, InProgress, Resolved, Completed, Removed, then name"
        );
        assert_eq!(state_names(&app.states_for("Task")), ["Doing"]);
        assert!(
            app.states_for("Epic").is_empty(),
            "a type with no rows and nothing cached has no states"
        );

        let mut catalog = StateCatalog::default();
        catalog.insert(
            "Bug",
            vec![
                StateOption::new("New", StateCategory::Proposed),
                StateOption::new("Active", StateCategory::InProgress),
                StateOption::new("Resolved", StateCategory::Resolved),
                StateOption::new("Closed", StateCategory::Completed),
            ],
        );
        app.set_state_catalog(catalog);

        assert_eq!(
            state_names(&app.states_for("Bug")),
            ["New", "Active", "Resolved", "Closed"],
            "cached states win, in the order the process template runs them"
        );
        assert_eq!(
            state_names(&app.states_for("Task")),
            ["Doing"],
            "a type without cached states still falls back"
        );
    }

    /// An editable app whose rows name three different people, with the
    /// signed-in user holding none of them.
    fn assignee_app() -> App {
        let mut alpha = ticket(1, "Alpha", "2026-01-01T00:00:00Z");
        alpha.assigned_to = Some("Priya Nair".into());
        let mut beta = ticket(2, "Beta", "2026-02-01T00:00:00Z");
        beta.assigned_to = None;
        let mut gamma = ticket(3, "Gamma", "2026-03-01T00:00:00Z");
        gamma.assigned_to = Some("Avery Chen".into());
        let mut app = App::new(vec![alpha, beta, gamma]);
        app.enable_sync();
        app.set_table_viewport(3);
        app.set_me(Some("Jacob Ragsdale".into()));
        app
    }

    fn candidate_names(app: &App) -> Vec<String> {
        app.assignee_matches()
            .into_iter()
            .map(|candidate| candidate.display)
            .collect()
    }

    fn type_query(app: &mut App, text: &str) {
        for character in text.chars() {
            press(app, KeyCode::Char(character));
        }
    }

    #[test]
    fn the_assignee_picker_lists_nobody_then_me_then_the_database_and_starts_on_the_current_one() {
        let mut app = assignee_app();

        assert_eq!(
            press(&mut app, KeyCode::Char('a')),
            AppAction::FetchIdentities,
            "the first open asks for the project's teams"
        );
        assert_eq!(app.mode, AppMode::AssigneePicker);
        assert_eq!(
            candidate_names(&app),
            ["Unassigned", "Jacob Ragsdale", "Avery Chen", "Priya Nair"],
            "nobody, then me, then everybody the rows name, sorted"
        );
        assert!(
            app.assignee_matches()[1].me,
            "the signed-in user is marked as such"
        );
        assert_eq!(
            app.assignee_picker.index, 2,
            "the picker opens on whoever holds the work item"
        );
        assert_eq!(app.assignee_picker.id, 3, "it names the selected row");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, AppMode::Browse);
        assert_eq!(
            press(&mut app, KeyCode::Char('a')),
            AppAction::None,
            "the teams are asked for once a session"
        );
    }

    #[test]
    fn typing_filters_the_assignee_picker_and_enter_assigns_who_is_left() {
        let mut app = assignee_app();
        app.set_identities(vec![Identity::new(
            "Jacob Ragsdale",
            Some("jacob@example.com".into()),
        )]);

        press(&mut app, KeyCode::Char('a'));
        type_query(&mut app, "jr");
        assert_eq!(
            candidate_names(&app),
            ["Jacob Ragsdale"],
            "the filter matches characters in order, not only whole words"
        );
        assert_eq!(
            app.assignee_picker.index, 0,
            "typing moves to the first hit"
        );

        let AppAction::Edit(request) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing somebody else should dispatch an edit");
        };
        assert_eq!(app.mode, AppMode::Browse);
        assert_eq!(request.key.id, 3);
        assert_eq!(
            request.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                serde_json::json!({
                    "op": "add",
                    "path": "/fields/System.AssignedTo",
                    "value": "jacob@example.com",
                }),
            ],
            "the write carries the address when the picker knows one"
        );
        assert_eq!(
            app.selected_ticket()
                .and_then(|ticket| ticket.assigned_to.clone()),
            Some("Jacob Ragsdale".to_owned()),
            "the cell reads as the display name, not the address"
        );
        assert!(app.is_mine(app.selected_ticket().unwrap()));
    }

    #[test]
    fn a_person_with_no_address_is_written_by_name_and_unassigned_removes_the_field() {
        let mut app = assignee_app();

        press(&mut app, KeyCode::Char('a'));
        type_query(&mut app, "priya");
        let AppAction::Edit(request) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing somebody else should dispatch an edit");
        };
        assert_eq!(
            request.edit.patch(),
            vec![serde_json::json!({
                "op": "add",
                "path": "/fields/System.AssignedTo",
                "value": "Priya Nair",
            })],
            "a name the database only ever saw is sent as itself"
        );

        let mut app = assignee_app();
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        let AppAction::Edit(request) = press(&mut app, KeyCode::Enter) else {
            panic!("Unassigned should dispatch an edit");
        };
        assert_eq!(
            request.edit.patch(),
            vec![serde_json::json!({"op": "remove", "path": "/fields/System.AssignedTo"})],
            "nobody is written by taking the field off the work item"
        );
        assert_eq!(
            app.selected_ticket()
                .and_then(|ticket| ticket.assigned_to.clone()),
            None,
            "the Assignee cell empties at once"
        );
    }

    #[test]
    fn choosing_the_current_assignee_or_pressing_escape_writes_nothing() {
        let mut app = assignee_app();

        press(&mut app, KeyCode::Char('a'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            AppAction::None,
            "whoever holds it already is a no-op"
        );
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None, "a no-op closes silently");

        // The same again for a work item nobody holds, where Unassigned is the
        // row the picker opens on.
        app.select_row(1);
        assert_eq!(
            app.selected_ticket()
                .and_then(|ticket| ticket.assigned_to.clone()),
            None
        );
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.assignee_picker.index, 0);
        assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None);

        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Esc), AppAction::None);
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
    }

    #[test]
    fn team_members_land_in_an_open_picker_without_moving_the_cursor() {
        let mut app = assignee_app();

        press(&mut app, KeyCode::Char('a'));
        let focused = app.assignee_matches()[app.assignee_picker.index]
            .display
            .clone();
        assert_eq!(focused, "Avery Chen");

        app.merge_identities(vec![
            Identity::new("Avery Chen", Some("avery@example.com".into())),
            Identity::new("Dana Okafor", Some("dana@example.com".into())),
        ]);

        assert_eq!(
            candidate_names(&app),
            [
                "Unassigned",
                "Jacob Ragsdale",
                "Avery Chen",
                "Priya Nair",
                "Dana Okafor"
            ],
            "a team member nobody holds work for is appended after the database's"
        );
        assert_eq!(
            app.assignee_matches()[app.assignee_picker.index].display,
            focused,
            "the cursor stays on the person it was on"
        );
        assert_eq!(
            app.assignee_matches()[2].unique.as_deref(),
            Some("avery@example.com"),
            "somebody already listed gains the address the teams knew"
        );

        type_query(&mut app, "dana");
        let AppAction::Edit(request) = press(&mut app, KeyCode::Enter) else {
            panic!("a merged-in team member should be choosable");
        };
        assert_eq!(
            request.edit.patch(),
            vec![serde_json::json!({
                "op": "add",
                "path": "/fields/System.AssignedTo",
                "value": "dana@example.com",
            })]
        );
    }

    /// The two trees a project with a nested quarter has, as a fetch flattens
    /// them. Sprint 1 is the one running today, whenever today is.
    fn classification_trees() -> Vec<ClassificationNode> {
        let today = Timestamp::now().calendar_date();
        let day = || Timestamp::parse(&format!("{today}T00:00:00Z")).ok();
        vec![
            ClassificationNode::new(NodeKind::Area, "development", 0),
            ClassificationNode::new(NodeKind::Area, "development\\Platform", 1),
            ClassificationNode::new(NodeKind::Iteration, "development", 0),
            ClassificationNode {
                start_date: day(),
                finish_date: day(),
                ..ClassificationNode::new(NodeKind::Iteration, "development\\Sprint 1", 1)
            },
            ClassificationNode::new(NodeKind::Iteration, "development\\Q3", 1),
            ClassificationNode::new(NodeKind::Iteration, "development\\Q3\\Sprint 7", 2),
        ]
    }

    /// An editable app whose selected row is planned into `development\Q3` and
    /// `development\Platform`, both nodes of the trees above.
    fn planned_app() -> App {
        let mut app = edit_app();
        let planned: Vec<Ticket> = app
            .tickets()
            .iter()
            .map(|ticket| Ticket {
                iteration_path: "development\\Q3".into(),
                area_path: "development\\Platform".into(),
                ..ticket.clone()
            })
            .collect();
        app.replace_prepared_tickets(PreparedTickets::new(planned));
        app.set_table_viewport(3);
        app
    }

    /// The same app with the project's trees already cached.
    fn node_app() -> App {
        let mut app = planned_app();
        app.set_classification_nodes(classification_trees(), None);
        app
    }

    /// The rows the open picker is showing, as they are drawn: the indent, the
    /// leaf, and whether the row is marked as running today.
    fn node_rows(app: &App) -> Vec<String> {
        app.node_matches()
            .into_iter()
            .map(|row| {
                let current = if row.current_period { " current" } else { "" };
                format!("{}{}{current}", row.indent(), row.leaf())
            })
            .collect()
    }

    /// Runs the Edit menu's Iteration or Area row.
    fn open_nodes(app: &mut App, kind: NodeKind) -> AppAction {
        app.run_command(match kind {
            NodeKind::Iteration => CommandId::EditIteration,
            NodeKind::Area => CommandId::EditArea,
        })
    }

    #[test]
    fn the_iteration_picker_draws_the_tree_indented_and_opens_on_the_current_node() {
        let mut app = node_app();

        assert_eq!(
            open_nodes(&mut app, NodeKind::Iteration),
            AppAction::FetchClassificationNodes,
            "the first open asks for the project's trees"
        );
        assert_eq!(app.mode, AppMode::NodePicker);
        assert_eq!(
            node_rows(&app),
            ["development", "  Sprint 1 current", "  Q3", "    Sprint 7"],
            "two spaces a level, the leaf named, and the sprint running today marked"
        );
        assert!(
            app.node_matches()[1].dates.is_some(),
            "a scheduled iteration carries its date range"
        );
        assert_eq!(
            app.node_picker.index, 2,
            "the picker opens on the node the work item sits in"
        );
        assert_eq!(app.node_picker.current, "development\\Q3");
        assert_eq!(app.node_picker.id, 3, "it names the selected row");

        press(&mut app, KeyCode::Esc);
        assert_eq!(
            open_nodes(&mut app, NodeKind::Iteration),
            AppAction::None,
            "the trees are asked for once a session, so the second open is instant"
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(
            open_nodes(&mut app, NodeKind::Area),
            AppAction::None,
            "and the other picker shares that one fetch"
        );
        assert_eq!(
            node_rows(&app),
            ["development", "  Platform"],
            "the area picker draws the other tree, with no dates on it"
        );
        assert_eq!(app.node_picker.index, 1);
    }

    #[test]
    fn enter_on_another_node_writes_the_full_path_and_the_row_shows_its_leaf() {
        let mut app = node_app();

        open_nodes(&mut app, NodeKind::Iteration);
        type_query(&mut app, "sprint 1");
        assert_eq!(
            node_rows(&app),
            ["  Sprint 1 current"],
            "the filter matches characters in order over the whole path"
        );
        let AppAction::Edit(request) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing another node should dispatch an edit");
        };

        assert_eq!(app.mode, AppMode::Browse);
        assert_eq!(request.key.id, 3);
        assert_eq!(
            request.edit.patch(),
            vec![serde_json::json!({
                "op": "add",
                "path": "/fields/System.IterationPath",
                "value": "development\\Sprint 1",
            })],
            "the write carries the full backslash path, not the leaf"
        );
        let moved = app.selected_ticket().expect("a selected row");
        assert_eq!(moved.iteration_path, "development\\Sprint 1");
        assert_eq!(
            path_leaf(&moved.iteration_path),
            "Sprint 1",
            "the Iteration column goes on showing the leaf"
        );

        let mut app = node_app();
        open_nodes(&mut app, NodeKind::Area);
        press(&mut app, KeyCode::Up);
        let AppAction::Edit(request) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing another area should dispatch an edit");
        };
        assert_eq!(
            request.edit.patch(),
            vec![serde_json::json!({
                "op": "add",
                "path": "/fields/System.AreaPath",
                "value": "development",
            })]
        );
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.area_path.clone()),
            Some("development".to_owned())
        );
    }

    #[test]
    fn choosing_the_node_the_work_item_is_already_in_writes_nothing() {
        let mut app = node_app();

        open_nodes(&mut app, NodeKind::Iteration);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            AppAction::None,
            "the node it sits in already is a no-op"
        );
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None, "a no-op closes silently");

        open_nodes(&mut app, NodeKind::Iteration);
        press(&mut app, KeyCode::Up);
        assert_eq!(press(&mut app, KeyCode::Esc), AppAction::None);
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
    }

    #[test]
    fn a_picker_with_nothing_cached_lists_the_paths_the_database_holds() {
        let mut app = planned_app();

        open_nodes(&mut app, NodeKind::Iteration);
        assert_eq!(
            node_rows(&app),
            ["  Q3"],
            "every work item is in development\\Q3, indented by its own depth"
        );
        assert_eq!(app.node_picker.index, 0, "which is where the cursor starts");

        press(&mut app, KeyCode::Esc);
        open_nodes(&mut app, NodeKind::Area);
        assert_eq!(node_rows(&app), ["  Platform"]);
    }

    #[test]
    fn fetched_trees_land_in_an_open_picker_without_moving_the_cursor() {
        let mut app = planned_app();

        assert_eq!(
            open_nodes(&mut app, NodeKind::Iteration),
            AppAction::FetchClassificationNodes
        );
        assert_eq!(node_rows(&app), ["  Q3"]);
        let focused = app.node_matches()[app.node_picker.index].path.clone();

        app.merge_classification_nodes(classification_trees());
        assert_eq!(
            node_rows(&app),
            ["development", "  Sprint 1 current", "  Q3", "    Sprint 7"],
            "the fetched tree replaces the fallback in the open picker"
        );
        assert_eq!(
            app.node_matches()[app.node_picker.index].path,
            focused,
            "the cursor stays on the node it was on"
        );

        type_query(&mut app, "q3s7");
        let AppAction::Edit(request) = press(&mut app, KeyCode::Enter) else {
            panic!("a merged-in node should be choosable");
        };
        assert_eq!(
            request.edit.patch(),
            vec![serde_json::json!({
                "op": "add",
                "path": "/fields/System.IterationPath",
                "value": "development\\Q3\\Sprint 7",
            })]
        );
    }

    #[test]
    fn the_current_iteration_is_the_scheduled_one_containing_today() {
        let mut app = planned_app();
        assert_eq!(
            app.current_iteration(),
            None,
            "a project whose trees were never fetched has no current sprint"
        );

        app.set_classification_nodes(classification_trees(), None);
        assert_eq!(
            app.current_iteration(),
            Some("development\\Sprint 1".to_owned())
        );

        let undated: Vec<ClassificationNode> = classification_trees()
            .into_iter()
            .map(|node| ClassificationNode::new(node.kind, node.path, node.depth))
            .collect();
        app.set_classification_nodes(undated, None);
        assert_eq!(
            app.current_iteration(),
            None,
            "an iteration nobody scheduled is never the current one"
        );
    }

    #[test]
    fn a_pull_without_cached_states_keeps_the_ones_an_earlier_pull_brought() {
        let mut app = picker_app();
        let tickets = app.tickets().to_vec();

        app.replace_prepared_tickets(PreparedTickets::new(tickets.clone()));
        assert_eq!(
            state_names(&app.states_for("Task")),
            ["To Do", "Doing", "Done"],
            "a pull that has not read the states endpoint must not empty the picker"
        );

        let mut catalog = StateCatalog::default();
        catalog.insert(
            "Task",
            vec![StateOption::new("Cut", StateCategory::Removed)],
        );
        app.replace_prepared_tickets(PreparedTickets::new(tickets).with_states(catalog));
        assert_eq!(state_names(&app.states_for("Task")), ["Cut"]);
    }
}
