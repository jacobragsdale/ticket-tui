use std::cmp::Ordering;
use std::sync::Arc;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppAction {
    None,
    Reload,
    OpenUrl(String),
}

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
    pub sort_field: SortField,
    pub sort_direction: SortDirection,
    pub mode: AppMode,
    pub focus: Focus,
    pub table_state: TableState,
    pub details_scroll: u16,
    pub narrow_details: bool,
    pub should_quit: bool,
    pub status: Option<String>,
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
            sort_field: SortField::Changed,
            sort_direction: SortDirection::Descending,
            mode: AppMode::Browse,
            focus: Focus::Tickets,
            table_state: TableState::default(),
            details_scroll: 0,
            narrow_details: false,
            should_quit: false,
            status: None,
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
        self.status = Some(message.into());
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
        self.sort_visible(true);
        self.restore_selection(selected.as_ref());
        self.search_pending = false;
        true
    }

    pub fn set_query(&mut self, query: String) {
        if self.query == query {
            return;
        }
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.query = query;
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
        self.sort_visible(!self.query.is_empty());
        self.restore_selection(selected.as_ref());
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
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                    self.mode = AppMode::Browse;
                }
                AppAction::None
            }
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> AppAction {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if self.contains(self.hit_regions.details, mouse.column, mouse.row) {
                    self.details_scroll = self.details_scroll.saturating_sub(3);
                } else {
                    self.move_selection(-3);
                }
            }
            MouseEventKind::ScrollDown => {
                if self.contains(self.hit_regions.details, mouse.column, mouse.row) {
                    self.details_scroll = self.details_scroll.saturating_add(3);
                } else {
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
            KeyCode::Char('/') => self.mode = AppMode::Search,
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
            KeyCode::Char('?') => self.mode = AppMode::Help,
            KeyCode::Char('r') => return AppAction::Reload,
            KeyCode::Char('d') => self.narrow_details = !self.narrow_details,
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Tickets => Focus::Details,
                    Focus::Details => Focus::Tickets,
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_focused(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_focused(-1),
            KeyCode::PageDown => self.move_focused(10),
            KeyCode::PageUp => self.move_focused(-10),
            KeyCode::Home => self.select_row(0),
            KeyCode::End => self.select_row(self.visible.len().saturating_sub(1)),
            KeyCode::Enter | KeyCode::Char('o') => return self.open_selected(),
            KeyCode::Esc if !self.query.is_empty() => self.set_query(String::new()),
            _ => {}
        }
        AppAction::None
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.mode = AppMode::Browse,
            KeyCode::Backspace => {
                let mut query = self.query.clone();
                query.pop();
                self.set_query(query);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.set_query(String::new());
            }
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let mut query = self.query.clone();
                query.push(character);
                self.set_query(query);
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
            self.mode = AppMode::Search;
            return AppAction::None;
        }
        if self.contains(self.hit_regions.detail_url, column, row) {
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
            Focus::Details => {
                self.details_scroll = if delta.is_negative() {
                    self.details_scroll
                        .saturating_sub(delta.unsigned_abs() as u16)
                } else {
                    self.details_scroll.saturating_add(delta as u16)
                };
            }
        }
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
        self.sort_visible(false);
        self.restore_selection(selected);
    }

    fn sort_visible(&mut self, relevance_first: bool) {
        let tickets = Arc::clone(&self.tickets);
        let field = self.sort_field;
        let direction = self.sort_direction;
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
        self.details_scroll = 0;
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
}
