use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::widgets::TableState;

use crate::agent_context::{
    AgentContext, SearchContext, SortContext, TicketContext, TicketReference, TicketsContext,
};
use crate::columns::TableLayout;
use crate::command::{Command, CommandId, matching_commands};
use crate::export;
pub use crate::filter::FacetTarget;
use crate::filter::{
    FacetValue, FilterField, FilterToken, ParsedQuery, facet_values, format_query, parse_query,
};
use crate::model::{
    CommentRecord, FamilySnapshot, FamilyTreeEntry, HistoryRecord, RelationRecord, SortDirection,
    SortField, Ticket, TicketGraph, TicketKey, compare_tickets,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppAction {
    None,
    Reload,
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

#[derive(Clone, Debug, Default)]
pub struct ViewsOverlay {
    pub index: usize,
    pub naming: Option<TextInput>,
    pub scroll: ScrollState,
}

#[derive(Debug)]
pub struct PreparedTickets {
    tickets: Vec<Ticket>,
    search_documents: SearchDocuments,
    graph: TicketGraph,
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
        }
    }

    #[must_use]
    pub fn ticket_count(&self) -> usize {
        self.tickets.len()
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
    bookmarks: HashSet<TicketKey>,
    selected_keys: HashSet<TicketKey>,
    recent: Vec<TicketKey>,
    future: Vec<TicketKey>,
    views: Vec<NamedView>,
    pub active_view: Option<String>,
    graph: TicketGraph,
    pub loaded_at: Instant,
    pub database_path: PathBuf,
    pub stale: bool,
    pub data_signature: u128,
    /// Display name of the signed-in Azure DevOps user, so their own work
    /// items can stand out. `None` until a sync records one.
    me: Option<String>,
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
            bookmarks: HashSet::new(),
            selected_keys: HashSet::new(),
            recent: Vec::new(),
            future: Vec::new(),
            views: Vec::new(),
            active_view: None,
            graph: prepared.graph,
            loaded_at: Instant::now(),
            database_path: PathBuf::new(),
            stale: false,
            data_signature: 0,
            me: None,
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

    pub fn set_workspace_graph(&mut self, graph: crate::model::TicketGraph) {
        self.graph = graph;
        self.sync_family_state();
    }

    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    #[must_use]
    pub fn freshness_label(&self) -> String {
        let age = self.loaded_at.elapsed();
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

    pub fn replace_tickets(&mut self, tickets: Vec<Ticket>) {
        self.replace_prepared_tickets(PreparedTickets::new(tickets));
    }

    pub fn replace_prepared_tickets(&mut self, prepared: PreparedTickets) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.tickets = Arc::new(prepared.tickets);
        self.graph = prepared.graph;
        self.search.replace_documents(prepared.search_documents);
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
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return AppAction::None;
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
        if matches!(self.pointer.drag(), DragKind::Text | DragKind::Cancelled) {
            self.pointer.set_drag(DragKind::Cancelled);
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('/') => self.begin_search(),
            KeyCode::Char('s') => {
                self.sort_draft = SortDraft {
                    field_index: SortField::ALL
                        .iter()
                        .position(|field| *field == self.sort_field)
                        .unwrap_or_default(),
                    direction: self.sort_direction,
                };
                self.mode = AppMode::Sort;
            }
            KeyCode::Char('?') => {
                self.help.scroll_to(0);
                self.mode = AppMode::Help;
            }
            KeyCode::Char('r') => return AppAction::Reload,
            KeyCode::Char('v') if !self.fuzzy_query().is_empty() => self.toggle_search_order(),
            KeyCode::Char('V') => self.open_views(),
            KeyCode::Char('c') => self.toggle_row_density(),
            KeyCode::Char('d') => self.toggle_narrow_details(),
            KeyCode::Char('f') => self.open_facets(0),
            KeyCode::Char('+') => self.open_filters(),
            KeyCode::Char('w') => self.open_columns(),
            KeyCode::Char('p') | KeyCode::Char(':') => self.open_palette(),
            KeyCode::Char('i') => {
                self.mode = AppMode::Info;
            }
            KeyCode::Char('m') => self.toggle_bookmark(),
            KeyCode::Char('y') => return self.copy_with(CopiedContent::Id, export::copy_ids),
            KeyCode::Char(' ') if self.focus != Focus::Family => self.toggle_row_selection(),
            KeyCode::Char('[') => self.history_back(),
            KeyCode::Char(']') => self.history_forward(),
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
            KeyCode::Char('o') => {
                self.record_history();
                return self.open_selected();
            }
            KeyCode::Esc if !self.query.is_empty() => self.set_query(String::new()),
            KeyCode::Esc if !self.selected_keys.is_empty() => self.selected_keys.clear(),
            _ => {}
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
            DragKind::Cancelled => PointerUpdate::none(hover_changed),
            DragKind::None => {
                if let Some(surface) = self.pointer.press_scrollbar() {
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
            PointerTarget::OpenPalette => self.open_palette(),
            PointerTarget::OpenHelp => {
                self.help.scroll_to(0);
                self.mode = AppMode::Help;
            }
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
        }
        AppAction::None
    }

    fn close_overlay(&mut self) {
        match self.mode {
            AppMode::Views if self.views_overlay.naming.is_some() => {
                self.views_overlay.naming = None;
            }
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
        self.open_palette();
        self.palette.query = TextInput::new("copy");
    }

    fn place_caret(&mut self, editor: TextEditor, column: u16, row: u16) {
        let Some(snapshot) = self
            .hit_regions
            .selectable(match editor {
                TextEditor::Search => SelectableSurface::Search,
                TextEditor::Palette | TextEditor::ViewName => SelectableSurface::Overlay,
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
            CommandId::Palette => {
                self.open_palette();
                AppAction::None
            }
            CommandId::Filters => {
                self.open_facets(0);
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
            CommandId::Reload => AppAction::Reload,
            CommandId::Open => {
                self.record_history();
                self.open_selected()
            }
            CommandId::ToggleDensity => {
                self.toggle_row_density();
                AppAction::None
            }
            CommandId::ToggleSearchOrder => {
                self.toggle_search_order();
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
        }
    }

    pub fn restore_session(&mut self, session: Session) {
        self.sort_field = session.sort_field;
        self.sort_direction = session.sort_direction;
        self.search_order = session.search_order;
        self.row_density = session.row_density;
        self.layout = TableLayout::from_session_columns(&session.columns, session.auto_hide);
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
    }
}

const fn focus_name(focus: Focus) -> &'static str {
    match focus {
        Focus::Tickets => "tickets",
        Focus::Family => "family",
        Focus::Details => "details",
    }
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
            created_at: crate::timestamp::ts("2026-01-01T00:00:00Z"),
            changed_at: crate::timestamp::ts(changed_at),
            web_url: format!("https://dev.azure.com/demo/atlas/_workitems/edit/{id}"),
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
    fn defaults_to_changed_descending() {
        let app = App::new(vec![
            ticket(1, "Older", "2026-01-01T00:00:00Z"),
            ticket(2, "Newer", "2026-02-01T00:00:00Z"),
        ]);

        assert_eq!(app.visible_tickets().next().unwrap().key.id, 2);
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
    }

    #[test]
    fn my_own_work_items_are_recognised_whatever_the_casing() {
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
        ]);
        let mut mine = app.tickets()[0].clone();
        mine.assigned_to = Some("  avery CHEN ".into());
        let mut theirs = app.tickets()[1].clone();
        theirs.assigned_to = Some("Jordan Patel".into());
        let mut unassigned = app.tickets()[1].clone();
        unassigned.assigned_to = None;

        assert!(!app.is_mine(&mine), "nobody is \"me\" until a name is set");

        app.set_me(Some("Avery Chen".into()));

        assert_eq!(app.me(), Some("Avery Chen"));
        assert!(app.is_mine(&mine));
        assert!(!app.is_mine(&theirs));
        assert!(!app.is_mine(&unassigned));
        assert_eq!(app.agent_context().me.as_deref(), Some("Avery Chen"));
    }

    #[test]
    fn fuzzy_result_then_field_sort_preserves_selected_ticket() {
        let mut app = App::new(vec![
            ticket(1, "Search alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Search beta", "2026-02-01T00:00:00Z"),
        ]);
        app.select_row(1);
        let selected = app.selected_ticket().unwrap().key.clone();

        app.set_query("search".into());
        await_search(&mut app);
        app.set_sort(SortField::Title, SortDirection::Ascending);

        assert_eq!(app.selected_ticket().unwrap().key, selected);
    }

    #[test]
    fn search_order_can_switch_from_relevance_to_strict_field_sorting() {
        let mut app = App::new(vec![
            ticket(1, "alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "prefix alpha suffix", "2026-02-01T00:00:00Z"),
        ]);
        app.set_query("alpha".into());
        app.visible = vec![
            SearchMatch {
                ticket_index: 0,
                score: 100,
            },
            SearchMatch {
                ticket_index: 1,
                score: 1,
            },
        ];
        app.sort_visible();

        assert_eq!(app.search_order, SearchOrder::Relevance);
        assert_eq!(app.visible_tickets().next().unwrap().key.id, 1);

        app.toggle_search_order();

        assert_eq!(app.search_order, SearchOrder::Field);
        assert_eq!(app.visible_tickets().next().unwrap().key.id, 2);
    }

    #[test]
    fn escape_clears_query_from_browse_mode() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.set_query("search".into());
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
    fn search_editor_inserts_and_deletes_at_the_cursor() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.mode = AppMode::Search;
        app.set_query("ac".into());

        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(app.query(), "abc");
        assert_eq!(app.query_cursor(), 2);

        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.query(), "ac");
        assert_eq!(app.query_cursor(), 1);

        app.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(app.query(), "a");
        assert_eq!(app.query_cursor(), 1);
    }

    #[test]
    fn search_editor_handles_unicode_word_deletion_and_paste() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.mode = AppMode::Search;
        app.set_query("alpha café".into());

        app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.query(), "alpha ");
        assert_eq!(app.query_cursor(), 6);

        app.handle_paste("tea\nshop\u{7}");
        assert_eq!(app.query(), "alpha tea shop");
        assert_eq!(app.query_cursor(), 14);
    }

    #[test]
    fn completed_searches_can_be_recalled_and_return_to_the_draft() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);

        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        app.set_query("alpha".into());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        app.set_query("beta".into());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(app.query(), "beta");
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(app.query(), "alpha");
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(app.query(), "beta");
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert!(app.query().is_empty());
    }

    #[test]
    fn sorting_and_reload_preserve_selected_ticket_view_context() {
        let original = vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            ticket(3, "Gamma", "2026-03-01T00:00:00Z"),
        ];
        let mut app = App::new(original.clone());
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
    }

    #[test]
    fn missing_selection_resets_view_context_after_reload() {
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
        ]);
        app.select_row(1);
        app.details.set_viewport(0, 5);
        app.details.scroll_to(3);
        app.table.offset = 1;

        app.replace_tickets(vec![ticket(3, "Gamma", "2026-03-01T00:00:00Z")]);

        assert_eq!(app.selected_ticket().unwrap().key.id, 3);
        assert_eq!(app.details.offset, 0);
        assert_eq!(app.table.offset, 0);
    }

    #[test]
    fn row_density_toggles_between_compact_and_comfortable() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);

        assert_eq!(app.row_density, RowDensity::Compact);
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert_eq!(app.row_density, RowDensity::Comfortable);
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert_eq!(app.row_density, RowDensity::Compact);
    }

    #[test]
    fn facet_bar_keyboard_toggles_the_focused_value() {
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
        ]);
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert_eq!(app.mode, AppMode::Facets);
        assert_eq!(app.focused_bar_field(), Some(FilterField::State));

        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(app.query().to_ascii_lowercase().contains("state:"));
        assert_eq!(app.visible_count(), 2);

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, AppMode::Browse);
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
    fn facet_toggle_rewrites_the_query_and_chip_removal_clears_it() {
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
    fn pane_shortcuts_keep_narrow_view_and_focus_together() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);

        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Details);
        assert!(app.narrow_details);

        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Tickets);
        assert!(!app.narrow_details);
    }

    #[test]
    fn scrolling_is_bounded_to_rendered_content() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.focus = Focus::Details;
        app.details.set_viewport(0, 4);

        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.details.offset, 4);

        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.details.offset, 0);

        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.details.offset, 4);
    }

    #[test]
    fn notifications_retain_level_and_schedule_expiration() {
        let mut app = App::new(Vec::new());

        app.set_status("Reloaded");
        assert_eq!(
            app.notification(),
            Some(("Reloaded", NotificationLevel::Info))
        );
        assert!(app.next_wakeup().is_some());

        app.set_error("Reload failed");
        assert_eq!(
            app.notification(),
            Some(("Reload failed", NotificationLevel::Error))
        );
    }

    #[test]
    fn help_navigation_clamps_to_rendered_content() {
        let mut app = App::new(Vec::new());
        app.mode = AppMode::Help;
        app.help.set_viewport(0, 3);

        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.help.offset, 3);
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.help.offset, 0);
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
    fn selected_family_tree_is_always_fully_expanded() {
        let app = family_app();
        let tree = app.visible_family_tree();
        assert_eq!(
            tree.iter().map(|entry| entry.key.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(
            tree.iter()
                .any(|entry| entry.key.id == 2 && entry.is_current)
        );
        assert_eq!(app.family_cursor.as_ref().map(|key| key.id), Some(2));
    }

    #[test]
    fn tab_toggles_between_tickets_and_details() {
        let mut app = family_app();
        assert_eq!(app.focus, Focus::Tickets);

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Details);
        assert!(app.narrow_details);

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Tickets);
        assert!(!app.narrow_details);

        app.focus = Focus::Family;
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Details);

        let mut without_family = App::new(vec![ticket(1, "Solo", "2026-01-01T00:00:00Z")]);
        press(&mut without_family, KeyCode::Tab);
        assert_eq!(without_family.focus, Focus::Details);
        press(&mut without_family, KeyCode::Tab);
        assert_eq!(without_family.focus, Focus::Tickets);
    }

    #[test]
    fn enter_does_not_open_a_ticket_from_the_tickets_pane() {
        let mut app = family_app();

        assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
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
    fn family_cursor_movement_clamps_at_both_ends() {
        let mut app = family_app();
        app.focus = Focus::Family;
        press(&mut app, KeyCode::Home);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.family_cursor.as_ref().map(|key| key.id), Some(1));

        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.family_cursor.as_ref().map(|key| key.id), Some(3));
    }

    #[test]
    fn family_enter_selects_a_visible_ticket_and_records_history_once() {
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
    }

    #[test]
    fn jumping_to_a_hidden_family_ticket_explains_why() {
        let mut app = family_app();
        app.focus = Focus::Family;
        app.visible
            .retain(|entry| app.tickets[entry.ticket_index].key.id != 3);
        app.family_cursor = Some(family_key(3));
        let query = app.query().to_owned();
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.selected_ticket().unwrap().key.id, 2);
        assert_eq!(app.query(), query);
        assert_eq!(
            app.notification(),
            Some(("3 is hidden by the current search", NotificationLevel::Info))
        );
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
    fn family_cursor_movement_scrolls_the_details_viewport() {
        let mut app = family_app();
        app.focus = Focus::Family;
        app.details.set_viewport(2, 20);

        press(&mut app, KeyCode::End);
        assert!(app.details.offset > 0);
        press(&mut app, KeyCode::Home);
        assert_eq!(app.details.offset, 0);
    }

    #[test]
    fn family_navigation_does_not_mark_the_session_dirty() {
        let mut app = family_app();
        app.session_dirty = false;
        app.focus = Focus::Family;
        press(&mut app, KeyCode::Down);
        assert!(!app.session_dirty);
    }
}
