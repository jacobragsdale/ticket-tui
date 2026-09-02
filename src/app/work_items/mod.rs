//! The work items screen: the table, the details pane and every overlay
//! that acts on a work item.

use ratatui::Frame;

use crate::columns::ColumnLayout;

use super::*;

use edits::{BulkEdit, PendingEdit, UndoEntry};
pub use edits::{DeleteConfirm, EditMenu, EditScope, PromptField, SyncTarget, TextPrompt};
pub use family::{ChildProgress, ChildProgressIndex};
pub use forms::{FormField, FormFieldId, FormFieldKind, FormKind, FormOverlay, FormPicker};
pub use pickers::{
    AssigneeCandidate, AssigneePicker, NodePicker, NodeRow, ParentCandidate, ParentPicker,
    PriorityPicker, StatePicker, TypePicker,
};
pub use query::{ColumnOverlay, FacetBar, FilterOverlay, PaletteState, SortDraft};
use views::builtin_named;
pub use views::{BuiltinView, SprintOverlay, ViewRow, ViewRowKind, ViewsOverlay};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkItemMode {
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
    /// A single-line field editor, for the Title and Tags rows of the Actions menu.
    Prompt,
    /// The people the selected work item can be assigned to, filtered by typing.
    AssigneePicker,
    /// The iteration or area tree the selected work item can be moved into,
    /// filtered by typing. Which of the two is on [`NodePicker::kind`].
    NodePicker,
    /// A multi-field form, such as the one `n` opens to file a new work item.
    Form,
    /// The one-row quick capture `+` opens over any tab: a title, and every
    /// other field defaulted rather than asked.
    Capture,
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

pub struct WorkItemsScreen {
    /// Which tab the open palette lists commands for. The palette, like the
    /// help, the columns editor and the database overlay, is drawn by this
    /// screen on behalf of whichever tab is showing.
    pub palette_tab: TabId,
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
    pub layout: TableLayout<SortField>,
    pub mode: WorkItemMode,
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
    /// The remembered stale threshold: what the palette last set, and what the
    /// session file carries between runs.
    stale_days: u16,
    /// The threshold this run was started under, from `--stale-days` or
    /// `TICKET_TUI_STALE_DAYS`. It stands over the remembered value until the
    /// palette moves the setting, and is never written back to the session: a
    /// flag passed once should not quietly become the setting.
    stale_days_override: Option<u16>,
    pub sort_draft: SortDraft,
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
    /// The title being typed into the quick capture row, empty while it is
    /// closed. There is no draft beside it: a one-line title that was
    /// abandoned was not worth keeping.
    pub capture: TextInput,
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
    bookmarks: HashSet<TicketKey>,
    selected_keys: HashSet<TicketKey>,
    views: Vec<NamedView>,
    pub active_view: Option<String>,
    graph: TicketGraph,
    /// Done out of total over each parent's direct children, rebuilt whenever
    /// the rows or the graph move rather than counted again every frame.
    child_progress: ChildProgressIndex,
    /// The states Azure DevOps allows for each work item type. Empty until a
    /// sync has fetched them, which is what [`WorkItemsScreen::states_for`] falls back for.
    state_catalog: StateCatalog,
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
    /// The sprint the configured team is in, as the last pull read it out of
    /// the team's own settings. `None` without a team, and then
    /// `current_iteration` reads the trees' dates instead.
    team_iteration: Option<String>,
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
/// command already, so a click and the Actions menu reach the same code.
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

impl WorkItemsScreen {
    #[must_use]
    pub fn new(shell: &mut Shell, tickets: Vec<Ticket>) -> Self {
        let prepared = Snapshot::new(tickets);
        let search = SearchEngine::from_documents(prepared.search_documents);
        let mut app = Self {
            palette_tab: TabId::WorkItems,
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
            mode: WorkItemMode::Browse,
            table_state: TableState::default(),
            table: ScrollState::default(),
            details: ScrollState::default(),
            details_family_row: 0,
            family_cursor: None,
            help: ScrollState::default(),
            sort: ScrollState::default(),
            stale_days: DEFAULT_STALE_DAYS,
            stale_days_override: None,
            sort_draft: SortDraft {
                field_index: 0,
                direction: SortDirection::Descending,
            },
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
            bookmarks: HashSet::new(),
            selected_keys: HashSet::new(),
            views: Vec::new(),
            active_view: None,
            graph: prepared.graph,
            child_progress: ChildProgressIndex::default(),
            state_catalog: prepared.states,
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
            capture: TextInput::default(),
            form: None,
            form_scroll: ScrollState::default(),
            form_draft: None,
            pending_create: None,
            work_item_types: Vec::new(),
            work_item_types_requested: false,
            identities: Vec::new(),
            identities_requested: false,
            classification_nodes: Vec::new(),
            classification_fetched_at: None,
            team_iteration: None,
            classification_requested: false,
        };
        app.refresh_child_progress();
        app.show_all(shell, None);
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

    pub fn set_workspace_graph(&mut self, shell: &mut Shell, graph: TicketGraph) {
        self.graph = graph;
        self.refresh_child_progress();
        self.sync_family_state(shell);
    }

    /// What the footer says when there is no notification over it: the keys
    /// that mean something in whatever mode the screen is in.
    #[must_use]
    pub fn footer_hint(&self, shell: &Shell) -> &str {
        match self.mode {
            WorkItemMode::Search => {
                "←→ cursor  Ctrl-P/N history  Ctrl-W delete word  Ctrl-U clear  Enter/Esc finish"
            }
            WorkItemMode::Sort => "↑↓ choose field  ←→ direction  Enter apply  Esc cancel",
            WorkItemMode::Help => "↑↓/jk scroll  PgUp/PgDn page  Home/End jump  ?/Esc close",
            WorkItemMode::Facets if self.facet_bar.field_index >= self.facet_bar.shown.len() => {
                "←→ field  Enter more filters  Esc back"
            }
            WorkItemMode::Facets => "←→/hl field  ↑↓/jk value  Space toggle  + more  Esc back",
            WorkItemMode::Filter if self.filter_overlay.showing_values => {
                "↑↓ values  Space toggle  ← fields  Esc close"
            }
            WorkItemMode::Filter => "↑↓ field  Enter values  Esc close",
            WorkItemMode::Columns => "↑↓ choose  Space show/hide  JK reorder  <> width  Esc close",
            WorkItemMode::Palette => "Type to filter  ↑↓ select  Enter run  Esc close",
            WorkItemMode::Views if self.views_overlay.naming.is_some() => {
                "Type a view name  Enter save  Esc cancel"
            }
            WorkItemMode::Views => "↑↓ choose  Enter load  n save  d delete  Esc close",
            WorkItemMode::Info => "Esc/i close",
            WorkItemMode::Sprint => "↑↓/jk row  ←→/hl sprint  Enter filter  Esc close",
            WorkItemMode::Edit => "\u{2191}\u{2193}/jk choose  Enter open  Esc close",
            WorkItemMode::StatePicker | WorkItemMode::PriorityPicker => {
                "\u{2191}\u{2193}/jk choose  Enter apply  Esc cancel"
            }
            WorkItemMode::Prompt => self
                .prompt
                .as_ref()
                .map_or("Enter save  Esc cancel", |prompt| prompt.field.hint()),
            WorkItemMode::AssigneePicker => {
                "Type to filter  \u{2191}\u{2193} select  Enter assign  Esc cancel"
            }
            WorkItemMode::NodePicker => {
                "Type to filter  \u{2191}\u{2193} select  Enter move  Esc cancel"
            }
            WorkItemMode::ParentPicker => {
                "Type to filter  \u{2191}\u{2193} select  Enter file under  Esc cancel"
            }
            WorkItemMode::TypePicker => "\u{2191}\u{2193}/jk choose  Enter apply  Esc cancel",
            WorkItemMode::Form => {
                "\u{2191}\u{2193}/Tab fields  Enter picker  Ctrl-S create  Esc cancel"
            }
            WorkItemMode::Capture => "Type a title  Enter create  Esc cancel",
            WorkItemMode::ConfirmDelete => "d delete  Esc cancel",
            WorkItemMode::Browse if shell.focus == Focus::Family => {
                "↑↓ move  Enter select  Tab details"
            }
            WorkItemMode::Browse if shell.focus == Focus::Details => {
                "↑↓/jk scroll details  Tab tickets  Enter/o open  / search  ? help  q quit"
            }
            WorkItemMode::Browse if !self.query().is_empty() => {
                "↑↓/jk move  f filters  Esc clear  ? help  q quit"
            }
            WorkItemMode::Browse => {
                "↑↓/jk move  / search  click/drag copy  wheel scroll  ? help  q quit"
            }
        }
    }

    pub fn handle_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        // Ctrl-C quits from every mode; other bindings only apply in browse mode.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && command_for_key(key, TabId::WorkItems) == Some(CommandId::Quit)
        {
            return self.run_command(shell, CommandId::Quit);
        }

        match self.mode {
            WorkItemMode::Browse => self.handle_browse_key(shell, key),
            WorkItemMode::Search => self.handle_search_key(shell, key),
            WorkItemMode::Sort => self.handle_sort_key(shell, key),
            WorkItemMode::Help => {
                self.handle_help_key(key);
                AppAction::None
            }
            WorkItemMode::Filter => {
                self.handle_filter_key(shell, key);
                AppAction::None
            }
            WorkItemMode::Columns => {
                self.handle_columns_key(shell, key);
                AppAction::None
            }
            WorkItemMode::Palette => self.handle_palette_key(shell, key),
            WorkItemMode::Views => self.handle_views_key(shell, key),
            WorkItemMode::Info => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('i') | KeyCode::Char('q')
                ) {
                    self.mode = WorkItemMode::Browse;
                }
                AppAction::None
            }
            WorkItemMode::Sprint => {
                self.handle_sprint_key(shell, key);
                AppAction::None
            }
            WorkItemMode::Facets => {
                self.handle_facet_key(shell, key);
                AppAction::None
            }
            WorkItemMode::Edit => self.handle_edit_menu_key(shell, key),
            WorkItemMode::StatePicker => self.handle_state_picker_key(shell, key),
            WorkItemMode::PriorityPicker => self.handle_priority_picker_key(shell, key),
            WorkItemMode::Prompt => self.handle_prompt_key(shell, key),
            WorkItemMode::AssigneePicker => self.handle_assignee_picker_key(shell, key),
            WorkItemMode::ParentPicker => self.handle_parent_picker_key(shell, key),
            WorkItemMode::NodePicker => self.handle_node_picker_key(shell, key),
            WorkItemMode::Form => self.handle_form_key(shell, key),
            WorkItemMode::Capture => self.handle_capture_key(shell, key),
            WorkItemMode::TypePicker => self.handle_type_picker_key(key),
            WorkItemMode::ConfirmDelete => self.handle_delete_confirm_key(shell, key),
        }
    }

    fn handle_browse_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        // Navigation keys depend on the focused pane; everything else is a command.
        match key.code {
            KeyCode::Char(' ') if shell.focus != Focus::Family => self.toggle_row_selection(),
            KeyCode::Tab => shell.toggle_focus(),
            KeyCode::Down | KeyCode::Char('j') => self.move_focused(shell, 1),
            KeyCode::Up | KeyCode::Char('k') => self.move_focused(shell, -1),
            KeyCode::PageDown => match shell.focus {
                Focus::Family => self.move_family_cursor(self.family_page_size()),
                Focus::Tickets | Focus::Details => self.move_focused(shell, 10),
            },
            KeyCode::PageUp => match shell.focus {
                Focus::Family => self.move_family_cursor(-self.family_page_size()),
                Focus::Tickets | Focus::Details => self.move_focused(shell, -10),
            },
            KeyCode::Home => match shell.focus {
                Focus::Tickets => self.select_row(shell, 0),
                Focus::Family => self.move_family_cursor_to_edge(false),
                Focus::Details => self.details.scroll_to(0),
            },
            KeyCode::End => match shell.focus {
                Focus::Tickets => self.select_row(shell, self.visible.len().saturating_sub(1)),
                Focus::Family => self.move_family_cursor_to_edge(true),
                Focus::Details => self.details.scroll_to(self.details.max_offset()),
            },
            KeyCode::Enter => match shell.focus {
                Focus::Tickets => {}
                Focus::Family => {
                    if let Some(key) = self.family_cursor.clone() {
                        self.jump_to_ticket(shell, &key);
                    }
                }
                Focus::Details => {
                    // A field the pointer is resting on opens its editor, the
                    // way clicking it would; anywhere else still opens the
                    // work item in the browser.
                    if let Some(field) = self.pointed_edit_field(shell) {
                        return self.open_field_editor(shell, field);
                    }
                    self.record_history(shell);
                    return self.open_selected();
                }
            },
            KeyCode::Esc if !self.query.is_empty() => self.set_query(shell, String::new()),
            KeyCode::Esc if !self.selected_keys.is_empty() => self.selected_keys.clear(),
            _ => {
                if let Some(id) = command_for_key(key, TabId::WorkItems) {
                    return self.run_command(shell, id);
                }
            }
        }
        AppAction::None
    }

    fn handle_search_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.finish_search(shell),
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.recall_previous_search(shell);
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.recall_next_search(shell);
            }
            KeyCode::Down => self.move_selection(shell, 1),
            KeyCode::Up => self.move_selection(shell, -1),
            _ => self.edit_query(shell, |query| {
                query.handle_key(key);
            }),
        }
        AppAction::None
    }

    fn handle_sort_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.mode = WorkItemMode::Browse,
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
                    shell,
                    SortField::ALL[self.sort_draft.field_index],
                    self.sort_draft.direction,
                );
                self.mode = WorkItemMode::Browse;
            }
            _ => {}
        }
        AppAction::None
    }

    fn handle_help_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') => self.mode = WorkItemMode::Browse,
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

    fn close_overlay(&mut self, shell: &mut Shell) {
        match self.mode {
            WorkItemMode::Views if self.views_overlay.naming.is_some() => {
                self.views_overlay.naming = None;
            }
            WorkItemMode::Prompt => self.close_prompt(),
            WorkItemMode::Form => self.cancel_form(),
            WorkItemMode::Capture => self.cancel_capture(),
            WorkItemMode::ConfirmDelete => self.cancel_delete(),
            WorkItemMode::AssigneePicker => self.close_picker(self.assignee_picker.scope),
            WorkItemMode::NodePicker => self.close_picker(self.node_picker.scope),
            WorkItemMode::TypePicker => self.close_picker(EditScope::Form(self.type_picker.field)),
            WorkItemMode::Facets => self.mode = WorkItemMode::Browse,
            WorkItemMode::Filter if self.filter_overlay.showing_values => {
                self.filter_overlay.showing_values = false;
                self.filter_overlay.value_index = 0;
                self.filter_overlay.scroll.scroll_to(0);
            }
            WorkItemMode::Browse | WorkItemMode::Search => {}
            _ => self.mode = WorkItemMode::Browse,
        }
        shell.pointer.clear_selection();
    }

    fn move_focused(&mut self, shell: &mut Shell, delta: isize) {
        match shell.focus {
            Focus::Tickets => self.move_selection(shell, delta),
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

    fn move_selection(&mut self, shell: &mut Shell, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let current = self.table_state.selected().unwrap_or_default();
        let next = current
            .saturating_add_signed(delta)
            .min(self.visible.len() - 1);
        self.select_row(shell, next);
    }

    pub fn select_row(&mut self, shell: &mut Shell, row: usize) {
        if self.visible.is_empty() {
            self.table_state.select(None);
            self.table.offset = 0;
        } else {
            let row = row.min(self.visible.len() - 1);
            self.table_state.select(Some(row));
            self.table.ensure_visible(row);
        }
        self.details.scroll_to(0);
        self.sync_family_state(shell);
    }

    fn visible_row(&self, key: &TicketKey) -> Option<usize> {
        self.visible
            .iter()
            .position(|entry| self.tickets[entry.ticket_index].key == *key)
    }

    fn jump_to_ticket(&mut self, shell: &mut Shell, key: &TicketKey) {
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
                shell.set_status(format!("{id} is {reason}", id = key.id));
            } else {
                shell.set_error(format!("{id} is not in this database", id = key.id));
            }
            return;
        };
        self.record_history(shell);
        self.select_row(shell, row);
        self.record_history(shell);
    }

    fn open_selected(&self) -> AppAction {
        self.selected_ticket().map_or(AppAction::None, |ticket| {
            AppAction::OpenUrl(ticket.web_url.clone())
        })
    }
}

const fn mode_name(mode: WorkItemMode) -> &'static str {
    match mode {
        WorkItemMode::Browse => "browse",
        WorkItemMode::Search => "search",
        WorkItemMode::Sort => "sort",
        WorkItemMode::Help => "help",
        WorkItemMode::Filter => "filter",
        WorkItemMode::Columns => "columns",
        WorkItemMode::Palette => "palette",
        WorkItemMode::Views => "views",
        WorkItemMode::Info => "info",
        WorkItemMode::Sprint => "sprint",
        WorkItemMode::Facets => "facets",
        WorkItemMode::Edit => "edit",
        WorkItemMode::StatePicker => "state-picker",
        WorkItemMode::PriorityPicker => "priority-picker",
        WorkItemMode::Prompt => "prompt",
        WorkItemMode::AssigneePicker => "assignee-picker",
        WorkItemMode::NodePicker => "node-picker",
        WorkItemMode::Form => "form",
        WorkItemMode::Capture => "capture",
        WorkItemMode::TypePicker => "type-picker",
        WorkItemMode::ParentPicker => "parent-picker",
        WorkItemMode::ConfirmDelete => "confirm-delete",
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

pub(crate) fn clamp_pos_to_snapshot(
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

impl Screen for WorkItemsScreen {
    fn handle_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        Self::handle_key(self, shell, key)
    }

    fn handle_paste(&mut self, shell: &mut Shell, pasted: &str) {
        Self::handle_paste(self, shell, pasted);
    }

    fn activate_target(
        &mut self,
        shell: &mut Shell,
        target: PointerTarget,
        column: u16,
        row: u16,
    ) -> AppAction {
        Self::activate_target(self, shell, target, column, row)
    }

    fn place_caret(&mut self, shell: &mut Shell, editor: TextEditor, column: u16, row: u16) {
        Self::place_caret(self, shell, editor, column, row);
    }

    fn close_overlay(&mut self, shell: &mut Shell) {
        Self::close_overlay(self, shell);
    }

    fn active_editor(&self) -> Option<TextEditor> {
        Self::active_editor(self)
    }

    fn scroll_state(&self, surface: ScrollSurface) -> ScrollState {
        Self::scroll_state(self, surface)
    }

    fn scroll_state_mut(&mut self, surface: ScrollSurface) -> &mut ScrollState {
        Self::scroll_state_mut(self, surface)
    }

    /// What carried the work item: the newest pull request still open on it,
    /// else the newest of any status, else the newest build it went out in.
    fn follow_target(&self, shell: &Shell) -> Result<(Jump, &'static str), String> {
        let ticket = self
            .selected_ticket()
            .ok_or_else(|| "No work item is selected".to_owned())?;
        let artifacts = self.artifacts_for(&ticket.key);
        // Only what the database holds: `g` goes where the details pane's own
        // link would, and a link to something nothing here has is drawn as
        // plain text rather than as somewhere to go.
        let requests = || {
            artifacts.iter().filter_map(|link| match &link.kind {
                crate::model::ArtifactKind::PullRequest { repo_id, id } => shell
                    .pull_request_label(*id)
                    .map(|(_, status)| (repo_id, *id, status)),
                _ => None,
            })
        };
        // Newest is the highest number: Azure DevOps counts them up.
        let open = requests()
            .filter(|(_, _, status)| !status.is_closed())
            .max_by_key(|(_, id, _)| *id);
        if let Some((repo_id, id, _)) = open.or_else(|| requests().max_by_key(|(_, id, _)| *id)) {
            return Ok((
                Jump::PullRequest {
                    repo: shell.repo_name(repo_id),
                    id,
                },
                "pull request",
            ));
        }
        artifacts
            .iter()
            .filter_map(|link| match link.kind {
                crate::model::ArtifactKind::Build(id) if shell.run_label(id).is_some() => Some(id),
                _ => None,
            })
            .max()
            .map(|id| (Jump::Run(id), "run"))
            .ok_or_else(|| format!("#{} has no linked pull request or build", ticket.key.id))
    }

    fn here(&self, _shell: &Shell) -> Option<Jump> {
        self.selected_ticket()
            .map(|ticket| Jump::WorkItem(ticket.key.clone()))
    }

    fn select(&mut self, shell: &mut Shell, jump: &Jump) -> bool {
        self.select_jump(shell, jump)
    }

    fn snapshot(&self) -> TabSession {
        Self::snapshot(self)
    }

    fn restore(&mut self, shell: &mut Shell, session: TabSession) {
        Self::restore(self, shell, session, None);
    }

    fn columns(&self) -> &dyn ColumnLayout {
        &self.layout
    }

    fn columns_mut(&mut self) -> &mut dyn ColumnLayout {
        &mut self.layout
    }

    fn footer_hint(&self, shell: &Shell) -> &str {
        Self::footer_hint(self, shell)
    }

    fn render(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
        crate::ui::render_screen(frame, self, shell, area);
    }
}
