use std::cmp::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;

use crate::model::{SortDirection, SortField, Ticket, TicketKey, compare_tickets};
use crate::search::{SearchEngine, SearchMatch};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AppMode {
    #[default]
    Browse,
    Search,
    Sort,
    Help,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Focus {
    #[default]
    Tickets,
    Details,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SearchOrder {
    #[default]
    Relevance,
    Field,
}

impl SearchOrder {
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Relevance => Self::Field,
            Self::Field => Self::Relevance,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Relevance => "Relevance",
            Self::Field => "Field",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppAction {
    None,
    Reload,
    OpenUrl(String),
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
    pub table: Option<Rect>,
    pub table_body: Option<Rect>,
    pub id_column: Option<Rect>,
    pub headers: Vec<(Rect, SortField)>,
    pub details: Option<Rect>,
    pub detail_url: Option<Rect>,
    pub sort_rows: Vec<(Rect, SortField)>,
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
    pub sort_field: SortField,
    pub sort_direction: SortDirection,
    pub mode: AppMode,
    pub focus: Focus,
    pub table_state: TableState,
    pub details_scroll: u16,
    pub details_max_scroll: u16,
    pub help_scroll: u16,
    pub help_max_scroll: u16,
    pub narrow_details: bool,
    pub should_quit: bool,
    notification: Option<Notification>,
    pub sort_draft: SortDraft,
    pub hit_regions: HitRegions,
}

impl App {
    #[must_use]
    pub fn new(tickets: Vec<Ticket>) -> Self {
        let search = SearchEngine::new(&tickets);
        let mut app = Self {
            tickets: Arc::new(tickets),
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
            sort_field: SortField::Changed,
            sort_direction: SortDirection::Descending,
            mode: AppMode::Browse,
            focus: Focus::Tickets,
            table_state: TableState::default(),
            details_scroll: 0,
            details_max_scroll: 0,
            help_scroll: 0,
            help_max_scroll: 0,
            narrow_details: false,
            should_quit: false,
            notification: None,
            sort_draft: SortDraft {
                field_index: 0,
                direction: SortDirection::Descending,
            },
            hit_regions: HitRegions::default(),
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

    pub fn replace_tickets(&mut self, tickets: Vec<Ticket>) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.tickets = Arc::new(tickets);
        self.search.replace_tickets(&self.tickets);
        if self.query.is_empty() {
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
        if result.generation != self.search_generation || self.query.is_empty() {
            return false;
        }

        let selected = self
            .pending_selection
            .take()
            .or_else(|| self.selected_ticket().map(|ticket| ticket.key.clone()));
        self.visible = result.matches;
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
        if self.query.is_empty() {
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
    }

    pub fn toggle_search_order(&mut self) {
        if self.query.is_empty() {
            return;
        }
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.search_order = self.search_order.toggled();
        self.sort_visible();
        self.restore_selection(selected.as_ref());
        self.set_status(format!("Search order: {}", self.search_order.label()));
    }

    pub fn toggle_sort(&mut self, field: SortField) {
        let direction = if self.sort_field == field {
            self.sort_direction.toggled()
        } else if matches!(field, SortField::Changed | SortField::Priority) {
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
        if self.mode == AppMode::Sort {
            if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                return self.handle_click(mouse.column, mouse.row);
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
            KeyCode::Char('v') if !self.query.is_empty() => self.toggle_search_order(),
            KeyCode::Char('d') => self.toggle_narrow_details(),
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
            KeyCode::Enter | KeyCode::Char('o') => return self.open_selected(),
            KeyCode::Esc if !self.query.is_empty() => self.set_query(String::new()),
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
            let row_index = self.table_state.offset() + usize::from(row - body.y);
            if row_index < self.visible.len() {
                self.select_row(row_index);
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
        self.search_generation = self.search.submit(&self.query);
        self.search_pending = true;
    }

    fn show_all(&mut self, selected: Option<&TicketKey>) {
        self.visible = (0..self.tickets.len())
            .map(|ticket_index| SearchMatch {
                ticket_index,
                score: 0,
            })
            .collect();
        self.sort_visible();
        self.restore_selection(selected);
    }

    fn sort_visible(&mut self) {
        let tickets = Arc::clone(&self.tickets);
        let field = self.sort_field;
        let direction = self.sort_direction;
        let relevance_first = !self.query.is_empty() && self.search_order == SearchOrder::Relevance;
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
