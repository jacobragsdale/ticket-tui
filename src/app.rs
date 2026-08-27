use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;

use crate::columns::TableLayout;
use crate::command::{Command, CommandId, matching_commands};
use crate::export;
use crate::filter::{
    FacetValue, FilterField, FilterToken, ParsedQuery, facet_values, format_query, parse_query,
};
use crate::import::ImportFormat;
use crate::model::{
    CommentRecord, HistoryRecord, RelationRecord, SortDirection, SortField, Ticket, TicketGraph,
    TicketKey, compare_tickets,
};
pub use crate::model::{RowDensity, SearchOrder};
use crate::search::{SearchDocuments, SearchEngine, SearchMatch};
use crate::session::{self, NamedView, Session};

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
    Prompt,
    Facets,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Focus {
    #[default]
    Tickets,
    Details,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppAction {
    None,
    Reload,
    OpenUrl(String),
    Copy(String),
    WriteFile { path: PathBuf, contents: String },
    Import { path: PathBuf, format: ImportFormat },
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
pub struct HitRegions {
    pub search: Option<Rect>,
    pub chips: Vec<(Rect, FilterToken)>,
    pub facet_pills: Vec<(Rect, FacetTarget)>,
    pub facet_values: Vec<(Rect, usize)>,
    pub table: Option<Rect>,
    pub table_body: Option<Rect>,
    pub id_column: Option<Rect>,
    pub headers: Vec<(Rect, SortField)>,
    pub details: Option<Rect>,
    pub detail_url: Option<Rect>,
    pub sort_rows: Vec<(Rect, SortField)>,
    pub overlay_rows: Vec<(Rect, usize)>,
}

#[derive(Clone, Debug, Default)]
pub struct FilterOverlay {
    pub field_index: usize,
    pub value_index: usize,
    pub showing_values: bool,
    pub scroll: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FacetTarget {
    Field(FilterField),
    More,
}

#[derive(Clone, Debug, Default)]
pub struct FacetBar {
    pub field_index: usize,
    pub value_index: usize,
}

#[derive(Clone, Debug, Default)]
pub struct ColumnOverlay {
    pub index: usize,
}

#[derive(Clone, Debug, Default)]
pub struct PaletteState {
    pub query: String,
    pub cursor: usize,
    pub selected: usize,
}

#[derive(Clone, Debug, Default)]
pub struct ViewsOverlay {
    pub index: usize,
    pub naming: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptKind {
    ImportJson,
    ImportCsv,
}

#[derive(Clone, Debug)]
pub struct PromptState {
    pub kind: PromptKind,
    pub buffer: String,
    pub cursor: usize,
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
    pub query: String,
    pub query_cursor: usize,
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
    pub details_scroll: u16,
    pub details_max_scroll: u16,
    pub help_scroll: u16,
    pub help_max_scroll: u16,
    pub overlay_scroll: u16,
    pub overlay_max_scroll: u16,
    pub narrow_details: bool,
    pub reload_pending: bool,
    pub should_quit: bool,
    pub session_dirty: bool,
    notification: Option<Notification>,
    pub sort_draft: SortDraft,
    pub hit_regions: HitRegions,
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
    pub read_only: bool,
    pub stale: bool,
    pub data_signature: u128,
    pub prompt: Option<PromptState>,
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
            query: String::new(),
            query_cursor: 0,
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
            details_scroll: 0,
            details_max_scroll: 0,
            help_scroll: 0,
            help_max_scroll: 0,
            overlay_scroll: 0,
            overlay_max_scroll: 0,
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
            read_only: false,
            stale: false,
            data_signature: 0,
            prompt: None,
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
    pub fn parsed_query(&self) -> ParsedQuery {
        parse_query(&self.query)
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

    #[must_use]
    pub fn views(&self) -> &[NamedView] {
        &self.views
    }

    #[must_use]
    pub fn palette_commands(&self) -> Vec<Command> {
        matching_commands(&self.palette.query)
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

    pub fn set_overlay_max_scroll(&mut self, maximum: u16) {
        self.overlay_max_scroll = maximum;
        self.overlay_scroll = self.overlay_scroll.min(maximum);
    }

    pub fn configure_database(&mut self, path: PathBuf, read_only: bool, signature: u128) {
        self.database_path = path;
        self.read_only = read_only;
        self.data_signature = signature;
        self.loaded_at = Instant::now();
        self.stale = false;
    }

    pub fn set_workspace_graph(&mut self, graph: crate::model::TicketGraph) {
        self.graph = graph;
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
    pub fn ticket_title(&self, key: &TicketKey) -> Option<&str> {
        self.tickets
            .iter()
            .find(|ticket| ticket.key == *key)
            .map(|ticket| ticket.title.as_str())
    }

    #[must_use]
    pub fn relations_from(&self, key: &TicketKey) -> Vec<&RelationRecord> {
        self.graph.relations_from(key)
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
        let cursor = query.chars().count();
        self.set_query_at(query, cursor);
    }

    pub fn handle_paste(&mut self, pasted: &str) {
        if self.mode == AppMode::Prompt {
            if let Some(prompt) = self.prompt.as_mut() {
                let pasted: String = pasted
                    .chars()
                    .filter(|character| !character.is_control())
                    .collect();
                prompt.buffer.push_str(&pasted);
                prompt.cursor = prompt.buffer.chars().count();
            }
            return;
        }
        if self.mode != AppMode::Search {
            return;
        }
        let pasted: String = pasted
            .chars()
            .filter_map(|character| match character {
                '\r' | '\n' | '\t' => Some(' '),
                character if character.is_control() => None,
                character => Some(character),
            })
            .collect();
        if pasted.is_empty() {
            return;
        }
        let byte = byte_index(&self.query, self.query_cursor);
        let mut query = self.query.clone();
        query.insert_str(byte, &pasted);
        let cursor = self.query_cursor + pasted.chars().count();
        self.set_query_at(query, cursor);
    }

    pub fn set_details_max_scroll(&mut self, maximum: u16) {
        self.details_max_scroll = maximum;
        self.details_scroll = self.details_scroll.min(maximum);
    }

    pub fn set_help_max_scroll(&mut self, maximum: u16) {
        self.help_max_scroll = maximum;
        self.help_scroll = self.help_scroll.min(maximum);
    }

    fn set_query_at(&mut self, query: String, cursor: usize) {
        if self.query == query {
            self.query_cursor = cursor.min(query.chars().count());
            return;
        }
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.query = query;
        self.query_cursor = cursor.min(self.query.chars().count());
        self.search_history_index = None;
        self.search_history_draft.clone_from(&self.query);
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
            AppMode::Prompt => self.handle_prompt_key(key),
            AppMode::Facets => {
                self.handle_facet_key(key);
                AppAction::None
            }
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> AppAction {
        if self.mode == AppMode::Help {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.help_scroll = self.help_scroll.saturating_sub(3);
                }
                MouseEventKind::ScrollDown => {
                    self.help_scroll = self.help_scroll.saturating_add(3).min(self.help_max_scroll);
                }
                _ => {}
            }
            return AppAction::None;
        }
        if self.mode == AppMode::Facets {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.facet_bar.value_index = self.facet_bar.value_index.saturating_sub(3);
                    return AppAction::None;
                }
                MouseEventKind::ScrollDown => {
                    let count = self.focused_bar_facets().len();
                    if count > 0 {
                        self.facet_bar.value_index =
                            (self.facet_bar.value_index + 3).min(count - 1);
                    }
                    return AppAction::None;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    return self.handle_click(mouse.column, mouse.row);
                }
                _ => {}
            }
        }
        if matches!(
            self.mode,
            AppMode::Sort
                | AppMode::Filter
                | AppMode::Columns
                | AppMode::Palette
                | AppMode::Views
                | AppMode::Info
                | AppMode::Prompt
        ) {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.overlay_scroll = self.overlay_scroll.saturating_sub(3);
                }
                MouseEventKind::ScrollDown => {
                    self.overlay_scroll = self
                        .overlay_scroll
                        .saturating_add(3)
                        .min(self.overlay_max_scroll);
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    return self.handle_click(mouse.column, mouse.row);
                }
                _ => {}
            }
            return AppAction::None;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if self.contains(self.hit_regions.details, mouse.column, mouse.row) {
                    self.focus = Focus::Details;
                    self.narrow_details = true;
                    self.scroll_details(-3);
                } else {
                    self.focus = Focus::Tickets;
                    self.narrow_details = false;
                    self.move_selection(-3);
                }
            }
            MouseEventKind::ScrollDown => {
                if self.contains(self.hit_regions.details, mouse.column, mouse.row) {
                    self.focus = Focus::Details;
                    self.narrow_details = true;
                    self.scroll_details(3);
                } else {
                    self.focus = Focus::Tickets;
                    self.narrow_details = false;
                    self.move_selection(3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                return self.handle_click(mouse.column, mouse.row);
            }
            _ => {}
        }
        AppAction::None
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
                self.help_scroll = 0;
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
            KeyCode::Char('y') => return self.copy_with(export::copy_ids),
            KeyCode::Char(' ') => self.toggle_row_selection(),
            KeyCode::Char('[') => self.history_back(),
            KeyCode::Char(']') => self.history_forward(),
            KeyCode::Tab => self.toggle_focus(),
            KeyCode::Down | KeyCode::Char('j') => self.move_focused(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_focused(-1),
            KeyCode::PageDown => self.move_focused(10),
            KeyCode::PageUp => self.move_focused(-10),
            KeyCode::Home => match self.focus {
                Focus::Tickets => self.select_row(0),
                Focus::Details => self.details_scroll = 0,
            },
            KeyCode::End => match self.focus {
                Focus::Tickets => self.select_row(self.visible.len().saturating_sub(1)),
                Focus::Details => self.details_scroll = self.details_max_scroll,
            },
            KeyCode::Enter | KeyCode::Char('o') => {
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
            KeyCode::Left => self.query_cursor = self.query_cursor.saturating_sub(1),
            KeyCode::Right => {
                self.query_cursor = (self.query_cursor + 1).min(self.query.chars().count());
            }
            KeyCode::Home => self.query_cursor = 0,
            KeyCode::End => self.query_cursor = self.query.chars().count(),
            KeyCode::Backspace => self.delete_query_character(true),
            KeyCode::Delete => self.delete_query_character(false),
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_query_word();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.set_query(String::new());
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.recall_previous_search();
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.recall_next_search();
            }
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let mut query = self.query.clone();
                let byte = byte_index(&query, self.query_cursor);
                query.insert(byte, character);
                self.set_query_at(query, self.query_cursor + 1);
            }
            _ => {}
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
                self.help_scroll = self.help_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.help_scroll = self.help_scroll.saturating_add(1).min(self.help_max_scroll);
            }
            KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(5),
            KeyCode::PageDown => {
                self.help_scroll = self.help_scroll.saturating_add(5).min(self.help_max_scroll);
            }
            KeyCode::Home => self.help_scroll = 0,
            KeyCode::End => self.help_scroll = self.help_max_scroll,
            _ => {}
        }
    }

    fn handle_click(&mut self, column: u16, row: u16) -> AppAction {
        if matches!(
            self.mode,
            AppMode::Browse | AppMode::Search | AppMode::Facets
        ) {
            if self.mode == AppMode::Facets
                && let Some((_, index)) = self
                    .hit_regions
                    .facet_values
                    .iter()
                    .find(|(area, _)| contains(*area, column, row))
            {
                self.facet_bar.value_index = *index;
                self.toggle_current_bar_facet();
                return AppAction::None;
            }
            if let Some((_, target)) = self
                .hit_regions
                .facet_pills
                .iter()
                .find(|(area, _)| contains(*area, column, row))
                .copied()
            {
                match target {
                    FacetTarget::More => self.open_filters(),
                    FacetTarget::Field(field) => {
                        let index = FilterField::BAR
                            .iter()
                            .position(|entry| *entry == field)
                            .unwrap_or_default();
                        self.open_facets(index);
                    }
                }
                return AppAction::None;
            }
            if self.mode == AppMode::Facets {
                self.mode = AppMode::Browse;
            }
        }
        if self.mode == AppMode::Filter {
            if let Some((_, index)) = self
                .hit_regions
                .overlay_rows
                .iter()
                .find(|(area, _)| contains(*area, column, row))
            {
                if self.filter_overlay.showing_values {
                    self.filter_overlay.value_index = *index;
                    self.toggle_current_facet();
                } else {
                    self.filter_overlay.field_index = *index;
                    self.filter_overlay.showing_values = true;
                    self.filter_overlay.value_index = 0;
                    self.overlay_scroll = 0;
                }
            }
            return AppAction::None;
        }
        if self.mode == AppMode::Columns {
            if let Some((_, index)) = self
                .hit_regions
                .overlay_rows
                .iter()
                .find(|(area, _)| contains(*area, column, row))
            {
                self.column_overlay.index = *index;
                self.layout.toggle_visible(*index);
                self.session_dirty = true;
            }
            return AppAction::None;
        }
        if self.mode == AppMode::Palette {
            if let Some((_, index)) = self
                .hit_regions
                .overlay_rows
                .iter()
                .find(|(area, _)| contains(*area, column, row))
            {
                self.palette.selected = *index;
                return self.run_selected_command();
            }
            return AppAction::None;
        }
        if self.mode == AppMode::Views {
            if self.views_overlay.naming.is_some() {
                return AppAction::None;
            }
            if let Some((_, index)) = self
                .hit_regions
                .overlay_rows
                .iter()
                .find(|(area, _)| contains(*area, column, row))
            {
                self.views_overlay.index = *index;
                self.apply_view_at(*index);
            }
            return AppAction::None;
        }
        if self.mode == AppMode::Sort {
            if let Some((_, field)) = self
                .hit_regions
                .sort_rows
                .iter()
                .find(|(area, _)| contains(*area, column, row))
            {
                self.toggle_sort(*field);
                self.mode = AppMode::Browse;
            }
            return AppAction::None;
        }
        if self.mode == AppMode::Help {
            return AppAction::None;
        }
        if self.contains(self.hit_regions.search, column, row) {
            self.begin_search();
            return AppAction::None;
        }
        if let Some((_, token)) = self
            .hit_regions
            .chips
            .iter()
            .find(|(area, _)| contains(*area, column, row))
        {
            self.remove_filter_token(token.clone());
            return AppAction::None;
        }
        if self.contains(self.hit_regions.detail_url, column, row) {
            self.focus = Focus::Details;
            self.narrow_details = true;
            return self.open_selected();
        }
        if let Some((_, field)) = self
            .hit_regions
            .headers
            .iter()
            .find(|(area, _)| contains(*area, column, row))
        {
            let field = *field;
            self.toggle_sort(field);
            return AppAction::None;
        }
        if let Some(body) = self.hit_regions.table_body
            && contains(body, column, row)
        {
            self.focus = Focus::Tickets;
            self.narrow_details = false;
            let row_index = self.table_state.offset()
                + usize::from((row - body.y) / self.row_density.row_height());
            if row_index < self.visible.len() {
                self.select_row(row_index);
                self.record_history();
                if self.contains(self.hit_regions.id_column, column, row) {
                    return self.open_selected();
                }
            }
        }
        AppAction::None
    }

    fn move_focused(&mut self, delta: isize) {
        match self.focus {
            Focus::Tickets => self.move_selection(delta),
            Focus::Details => self.scroll_details(delta),
        }
    }

    fn scroll_details(&mut self, delta: isize) {
        self.details_scroll = if delta.is_negative() {
            self.details_scroll
                .saturating_sub(delta.unsigned_abs() as u16)
        } else {
            self.details_scroll
                .saturating_add(delta as u16)
                .min(self.details_max_scroll)
        };
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Tickets => Focus::Details,
            Focus::Details => Focus::Tickets,
        };
        self.narrow_details = self.focus == Focus::Details;
    }

    fn toggle_narrow_details(&mut self) {
        self.narrow_details = !self.narrow_details;
        self.focus = if self.narrow_details {
            Focus::Details
        } else {
            Focus::Tickets
        };
    }

    fn delete_query_character(&mut self, backwards: bool) {
        let character_count = self.query.chars().count();
        let remove_at = if backwards {
            let Some(index) = self.query_cursor.checked_sub(1) else {
                return;
            };
            index
        } else {
            if self.query_cursor >= character_count {
                return;
            }
            self.query_cursor
        };
        let start = byte_index(&self.query, remove_at);
        let end = byte_index(&self.query, remove_at + 1);
        let mut query = self.query.clone();
        query.replace_range(start..end, "");
        self.set_query_at(
            query,
            if backwards {
                remove_at
            } else {
                self.query_cursor
            },
        );
    }

    fn delete_query_word(&mut self) {
        if self.query_cursor == 0 {
            return;
        }
        let characters: Vec<_> = self.query.chars().collect();
        let mut start = self.query_cursor;
        while start > 0 && characters[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !characters[start - 1].is_whitespace() {
            start -= 1;
        }
        let start_byte = byte_index(&self.query, start);
        let end_byte = byte_index(&self.query, self.query_cursor);
        let mut query = self.query.clone();
        query.replace_range(start_byte..end_byte, "");
        self.set_query_at(query, start);
    }

    fn begin_search(&mut self) {
        self.query_cursor = self.query.chars().count();
        self.search_history_index = None;
        self.search_history_draft.clone_from(&self.query);
        self.mode = AppMode::Search;
    }

    fn finish_search(&mut self) {
        if !self.query.is_empty()
            && self
                .search_history
                .last()
                .is_none_or(|previous| previous != &self.query)
        {
            const HISTORY_LIMIT: usize = 50;
            if self.search_history.len() == HISTORY_LIMIT {
                self.search_history.remove(0);
            }
            self.search_history.push(self.query.clone());
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
            self.search_history_draft.clone_from(&self.query);
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
        } else {
            self.table_state
                .select(Some(row.min(self.visible.len() - 1)));
        }
        self.details_scroll = 0;
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
        if selected.is_none() || row.is_none() {
            *self.table_state.offset_mut() = 0;
            self.details_scroll = 0;
        }
    }

    fn contains(&self, area: Option<Rect>, column: u16, row: u16) -> bool {
        area.is_some_and(|area| contains(area, column, row))
    }

    fn open_filters(&mut self) {
        self.filter_overlay = FilterOverlay::default();
        self.overlay_scroll = 0;
        self.mode = AppMode::Filter;
    }

    fn open_facets(&mut self, field_index: usize) {
        self.facet_bar.field_index = field_index.min(FilterField::BAR.len());
        self.facet_bar.value_index = 0;
        self.overlay_scroll = 0;
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
                self.facet_bar.value_index = self.facet_bar.value_index.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let count = self.focused_bar_facets().len();
                if count > 0 {
                    self.facet_bar.value_index = (self.facet_bar.value_index + 1).min(count - 1);
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
        self.overlay_scroll = 0;
        self.mode = AppMode::Columns;
    }

    fn open_palette(&mut self) {
        self.palette = PaletteState::default();
        self.overlay_scroll = 0;
        self.mode = AppMode::Palette;
    }

    fn open_views(&mut self) {
        self.views_overlay = ViewsOverlay::default();
        self.overlay_scroll = 0;
        self.mode = AppMode::Views;
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc if self.filter_overlay.showing_values => {
                self.filter_overlay.showing_values = false;
                self.filter_overlay.value_index = 0;
                self.overlay_scroll = 0;
            }
            KeyCode::Esc | KeyCode::Char('f') => self.mode = AppMode::Browse,
            KeyCode::Left | KeyCode::Char('h') if self.filter_overlay.showing_values => {
                self.filter_overlay.showing_values = false;
                self.overlay_scroll = 0;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter
                if !self.filter_overlay.showing_values =>
            {
                self.filter_overlay.showing_values = true;
                self.filter_overlay.value_index = 0;
                self.overlay_scroll = 0;
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
        if self.filter_overlay.showing_values {
            let count = self.current_facets().len();
            if count == 0 {
                return;
            }
            self.filter_overlay.value_index = self
                .filter_overlay
                .value_index
                .saturating_add_signed(delta)
                .min(count - 1);
        } else {
            self.filter_overlay.field_index = self
                .filter_overlay
                .field_index
                .saturating_add_signed(delta)
                .min(FilterField::ALL.len() - 1);
        }
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
                self.column_overlay.index = self.column_overlay.index.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.column_overlay.index = (self.column_overlay.index + 1).min(last);
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

    fn handle_palette_key(&mut self, key: KeyEvent) -> AppAction {
        let commands = self.palette_commands();
        match key.code {
            KeyCode::Esc => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.palette.selected = self.palette.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !commands.is_empty() {
                    self.palette.selected = (self.palette.selected + 1).min(commands.len() - 1);
                }
            }
            KeyCode::Up => self.palette.selected = self.palette.selected.saturating_sub(1),
            KeyCode::Down => {
                if !commands.is_empty() {
                    self.palette.selected = (self.palette.selected + 1).min(commands.len() - 1);
                }
            }
            KeyCode::Enter => return self.run_selected_command(),
            KeyCode::Backspace => {
                self.palette.query.pop();
                self.palette.selected = 0;
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.palette.query.push(character);
                self.palette.selected = 0;
            }
            _ => {}
        }
        AppAction::None
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
                self.views_overlay.naming = Some(self.active_view.clone().unwrap_or_default());
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
                self.help_scroll = 0;
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
            CommandId::CopyId => self.copy_with(export::copy_ids),
            CommandId::CopyUrl => self.copy_with(export::copy_urls),
            CommandId::CopyTitle => self.copy_with(export::copy_titles),
            CommandId::CopyMarkdown => self.copy_with(export::copy_markdown_links),
            CommandId::CopySummary => self.copy_with(export::copy_summaries),
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
            CommandId::ImportJson => self.begin_import(PromptKind::ImportJson),
            CommandId::ImportCsv => self.begin_import(PromptKind::ImportCsv),
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

    fn begin_import(&mut self, kind: PromptKind) -> AppAction {
        if self.read_only {
            self.set_error("Database is open read-only; import is disabled");
            return AppAction::None;
        }
        self.prompt = Some(PromptState {
            kind,
            buffer: String::new(),
            cursor: 0,
        });
        self.mode = AppMode::Prompt;
        AppAction::None
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> AppAction {
        let Some(prompt) = self.prompt.as_mut() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
                self.mode = AppMode::Browse;
            }
            KeyCode::Enter => {
                let path = prompt.buffer.trim().to_owned();
                let format = match prompt.kind {
                    PromptKind::ImportJson => ImportFormat::Json,
                    PromptKind::ImportCsv => ImportFormat::Csv,
                };
                self.prompt = None;
                self.mode = AppMode::Browse;
                if path.is_empty() {
                    return AppAction::None;
                }
                if self.read_only {
                    self.set_error("Database is open read-only; import is disabled");
                    return AppAction::None;
                }
                return AppAction::Import {
                    path: PathBuf::from(path),
                    format,
                };
            }
            KeyCode::Backspace => {
                prompt.buffer.pop();
                prompt.cursor = prompt.buffer.chars().count();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                prompt.buffer.push(character);
                prompt.cursor = prompt.buffer.chars().count();
            }
            _ => {}
        }
        AppAction::None
    }

    fn handle_views_key(&mut self, key: KeyEvent) -> AppAction {
        if self.views_overlay.naming.is_some() {
            match key.code {
                KeyCode::Esc => self.views_overlay.naming = None,
                KeyCode::Enter => {
                    let name = self
                        .views_overlay
                        .naming
                        .take()
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    if !name.is_empty() {
                        self.save_view(name);
                    }
                }
                KeyCode::Backspace => {
                    if let Some(name) = self.views_overlay.naming.as_mut() {
                        name.pop();
                    }
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(name) = self.views_overlay.naming.as_mut() {
                        name.push(character);
                    }
                }
                _ => {}
            }
            return AppAction::None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('V') => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.views_overlay.index = self.views_overlay.index.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.views.is_empty() {
                    self.views_overlay.index =
                        (self.views_overlay.index + 1).min(self.views.len() - 1);
                }
            }
            KeyCode::Enter => self.apply_view_at(self.views_overlay.index),
            KeyCode::Char('n') => {
                self.views_overlay.naming = Some(String::new());
            }
            KeyCode::Char('d') | KeyCode::Delete => self.delete_view_at(self.views_overlay.index),
            _ => {}
        }
        AppAction::None
    }

    fn save_view(&mut self, name: String) {
        let view = NamedView {
            name: name.clone(),
            query: self.query.clone(),
            sort_field: session::encode_sort_field(self.sort_field).to_owned(),
            sort_direction: session::encode_direction(self.sort_direction).to_owned(),
            search_order: session::encode_search_order(self.search_order).to_owned(),
            row_density: session::encode_density(self.row_density).to_owned(),
            columns: session::encode_layout(&self.layout),
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
        self.sort_field = session::decode_sort_field(&view.sort_field).unwrap_or(self.sort_field);
        self.sort_direction = session::decode_direction(&view.sort_direction);
        self.search_order = session::decode_search_order(&view.search_order);
        self.row_density = session::decode_density(&view.row_density);
        self.layout = session::decode_layout(&view.columns, Some(view.auto_hide));
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

    fn copy_with(&self, formatter: fn(&[&Ticket]) -> String) -> AppAction {
        let tickets = self.export_targets();
        if tickets.is_empty() {
            return AppAction::None;
        }
        AppAction::Copy(formatter(&tickets))
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
            query: self.query.clone(),
            sort_field: session::encode_sort_field(self.sort_field).to_owned(),
            sort_direction: session::encode_direction(self.sort_direction).to_owned(),
            search_order: session::encode_search_order(self.search_order).to_owned(),
            row_density: session::encode_density(self.row_density).to_owned(),
            columns: session::encode_layout(&self.layout),
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
        self.sort_field =
            session::decode_sort_field(&session.sort_field).unwrap_or(self.sort_field);
        self.sort_direction = session::decode_direction(&session.sort_direction);
        self.search_order = session::decode_search_order(&session.search_order);
        self.row_density = session::decode_density(&session.row_density);
        self.layout = session::decode_layout(&session.columns, session.auto_hide);
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

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

fn byte_index(text: &str, character_index: usize) -> usize {
    text.char_indices()
        .nth(character_index)
        .map_or(text.len(), |(index, _)| index)
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
            created_at: "2026-01-01T00:00:00Z".into(),
            changed_at: changed_at.into(),
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
        app.query = "alpha".into();
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

        assert!(app.query.is_empty());
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
        assert_eq!(app.query, "abc");
        assert_eq!(app.query_cursor, 2);

        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.query, "ac");
        assert_eq!(app.query_cursor, 1);

        app.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(app.query, "a");
        assert_eq!(app.query_cursor, 1);
    }

    #[test]
    fn search_editor_handles_unicode_word_deletion_and_paste() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.mode = AppMode::Search;
        app.set_query("alpha café".into());

        app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.query, "alpha ");
        assert_eq!(app.query_cursor, 6);

        app.handle_paste("tea\nshop\u{7}");
        assert_eq!(app.query, "alpha tea shop");
        assert_eq!(app.query_cursor, 14);
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
        assert_eq!(app.query, "beta");
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(app.query, "alpha");
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(app.query, "beta");
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert!(app.query.is_empty());
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
        app.details_scroll = 3;
        app.details_max_scroll = 5;
        *app.table_state.offset_mut() = 1;

        app.set_sort(SortField::Title, SortDirection::Descending);
        assert_eq!(app.selected_ticket().unwrap().key, selected);
        assert_eq!(app.details_scroll, 3);
        assert_eq!(app.table_state.offset(), 1);

        app.replace_tickets(original);
        assert_eq!(app.selected_ticket().unwrap().key, selected);
        assert_eq!(app.details_scroll, 3);
        assert_eq!(app.table_state.offset(), 1);
    }

    #[test]
    fn missing_selection_resets_view_context_after_reload() {
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
        ]);
        app.select_row(1);
        app.details_scroll = 3;
        *app.table_state.offset_mut() = 1;

        app.replace_tickets(vec![ticket(3, "Gamma", "2026-03-01T00:00:00Z")]);

        assert_eq!(app.selected_ticket().unwrap().key.id, 3);
        assert_eq!(app.details_scroll, 0);
        assert_eq!(app.table_state.offset(), 0);
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
        assert!(app.query.to_ascii_lowercase().contains("state:"));
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

        assert!(app.query.contains("state:"));
        let token = app.filter_tokens().pop().unwrap();
        app.remove_filter_token(token);
        assert!(app.query.is_empty());
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

        assert_eq!(app.query, "state:active");
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
        let action = app.copy_with(export::copy_ids);
        assert_eq!(action, AppAction::Copy("1\n2\n".into()));
    }

    #[test]
    fn command_palette_runs_density_toggle() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.open_palette();
        app.palette.query = "density".into();
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
        app.set_details_max_scroll(4);

        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.details_scroll, 4);

        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.details_scroll, 0);

        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.details_scroll, 4);
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
        app.set_help_max_scroll(3);

        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.help_scroll, 3);
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.help_scroll, 0);
    }
}
