//! The search box, the filter grammar, the facet bar, the column and view
//! overlays, the command palette and sorting.

use super::*;

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

impl App {
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

    /// The filters the table actually applies: the query's own, and while
    /// finished work is hidden an implicit `state:@open` alongside them.
    ///
    /// Writing the rule as the sentinel the grammar already has is what keeps
    /// the toggle and an explicit `state:` from fighting. A value in one field
    /// is ORed with the others in it, so a query naming a state of its own —
    /// `state:done`, or the `state:@open` the Stale view is written with —
    /// would be widened rather than narrowed by an implicit value beside it.
    /// So the implicit one is left off entirely wherever the query names a
    /// state, which is exactly the rule that makes `state:done` list finished
    /// work while the toggle stays on, and makes `state:@open` mean what it
    /// says rather than being applied twice.
    #[must_use]
    fn effective_filters(&self) -> FilterSet {
        let mut filters = self.parsed_query().filters;
        if self.hides_finished(&filters) {
            filters.insert(FilterField::State, Sentinel::Open.as_value());
        }
        filters
    }

    /// What the query's sentinels stand for right now: who is signed in and
    /// which sprint contains today, beside the clock its relative date bounds
    /// are measured back from. Built fresh for every pass over the rows, so a
    /// saved `assignee:@me` follows the name and `iteration:@current` follows
    /// the sprint rather than whatever either was when the view was written.
    #[must_use]
    pub fn match_context(&self) -> MatchContext {
        MatchContext::now()
            .with_me(self.shell.me.clone())
            .with_current_iteration(self.current_iteration())
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
        let filters = self.effective_filters();
        facet_values(
            self.tickets(),
            &filters,
            field,
            |ticket| self.bookmarks.contains(&ticket.key),
            &self.match_context(),
        )
    }

    pub fn toggle_filter(&mut self, field: FilterField, value: &str) {
        let mut parsed = self.parsed_query();
        parsed.filters.toggle(field, value);
        self.set_query(format_query(&parsed.filters, &parsed.fuzzy));
    }

    #[must_use]
    pub fn palette_commands(&self) -> Vec<Command> {
        matching_commands(self.palette.query.text(), !self.show_finished)
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
        let filters = self.effective_filters();
        facet_values(
            self.tickets(),
            &filters,
            self.facet_field(),
            |ticket| self.bookmarks.contains(&ticket.key),
            &self.match_context(),
        )
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
            Some(TextEditor::Form) => {
                if let Some(field) = self.focused_form_field_mut() {
                    field.input.paste(pasted, false);
                }
            }
            Some(TextEditor::Assignee) => self.assignee_picker.query.paste(pasted, false),
            Some(TextEditor::Parent) => self.parent_picker.query.paste(pasted, false),
            Some(TextEditor::Node) => self.node_picker.query.paste(pasted, false),
            None => {}
        }
    }

    /// Runs one edit against the search field and re-runs the search when the text
    /// actually changed; a bare caret move leaves the results alone.
    pub(super) fn edit_query(&mut self, edit: impl FnOnce(&mut TextInput)) {
        let before = self.query.text().to_owned();
        edit(&mut self.query);
        if self.query.text() != before {
            self.after_query_edit();
        }
    }

    fn after_query_edit(&mut self) {
        self.search_history_index = None;
        self.search_history_draft = self.query.text().to_owned();
        self.shell.session_dirty = true;
        self.resubmit_query();
    }

    /// Works the visible set out again from every row, for when what the table
    /// asks of them has changed rather than which rows there are.
    pub(super) fn resubmit_query(&mut self) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
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
            AppMode::ParentPicker => Some(TextEditor::Parent),
            AppMode::NodePicker => Some(TextEditor::Node),
            AppMode::Form => Some(TextEditor::Form),
            _ => None,
        }
    }

    pub fn set_sort(&mut self, field: SortField, direction: SortDirection) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.sort_field = field;
        self.sort_direction = direction;
        self.sort_visible();
        self.restore_selection(selected.as_ref());
        self.shell.session_dirty = true;
    }

    pub fn toggle_row_density(&mut self) {
        self.row_density = self.row_density.toggled();
        self.shell.session_dirty = true;
        self.shell
            .set_status(format!("Row density: {}", self.row_density.label()));
    }

    pub fn toggle_search_order(&mut self) {
        if self.fuzzy_query().is_empty() {
            return;
        }
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.search_order = self.search_order.toggled();
        self.sort_visible();
        self.restore_selection(selected.as_ref());
        self.shell.session_dirty = true;
        self.shell
            .set_status(format!("Search order: {}", self.search_order.label()));
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

    pub(super) fn begin_search(&mut self) {
        self.query.move_end();
        self.search_history_index = None;
        self.search_history_draft = self.query.text().to_owned();
        self.mode = AppMode::Search;
    }

    pub(super) fn finish_search(&mut self) {
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

    pub(super) fn recall_previous_search(&mut self) {
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

    pub(super) fn recall_next_search(&mut self) {
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

    pub(super) fn submit_search(&mut self) {
        self.search_generation = self.search.submit(&self.fuzzy_query());
        self.search_pending = true;
    }

    pub(super) fn show_all(&mut self, selected: Option<&TicketKey>) {
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

    pub(super) fn apply_filters(&mut self) {
        let filters = self.effective_filters();
        let context = self.match_context();
        let bookmarks = self.bookmarks.clone();
        let tickets = Arc::clone(&self.tickets);
        self.visible.retain(|entry| {
            filters.matches_in(
                &tickets[entry.ticket_index],
                bookmarks.contains(&tickets[entry.ticket_index].key),
                &context,
            )
        });
    }

    pub(super) fn sort_visible(&mut self) {
        let tickets = Arc::clone(&self.tickets);
        let field = self.sort_field;
        let direction = self.sort_direction;
        let relevance_first =
            !self.fuzzy_query().is_empty() && self.search_order == SearchOrder::Relevance;
        // Child progress is the one column whose value is not on the work
        // item, so the index supplies the ordering and `compare_tickets` is
        // left to break the ties.
        let progress = (field == SortField::Progress).then(|| self.child_progress.clone());
        self.visible.sort_by(|left, right| {
            let relevance = if relevance_first {
                right.score.cmp(&left.score)
            } else {
                Ordering::Equal
            };
            let left = &tickets[left.ticket_index];
            let right = &tickets[right.ticket_index];
            relevance
                .then_with(|| {
                    progress.as_ref().map_or(Ordering::Equal, |progress| {
                        progress.compare(&left.key, &right.key, direction)
                    })
                })
                .then_with(|| compare_tickets(left, right, field, direction))
        });
    }

    pub(super) fn restore_selection(&mut self, selected: Option<&TicketKey>) {
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

    pub(super) fn open_filters(&mut self) {
        self.filter_overlay = FilterOverlay::default();
        self.mode = AppMode::Filter;
    }

    pub(super) fn open_facets(&mut self, field_index: usize) {
        self.facet_bar.field_index = field_index.min(FilterField::BAR.len());
        self.facet_bar.value_index = 0;
        self.mode = AppMode::Facets;
    }

    pub(super) fn handle_facet_key(&mut self, key: KeyEvent) {
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

    pub(super) fn toggle_current_bar_facet(&mut self) {
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

    pub(super) fn open_palette(&mut self) {
        self.palette = PaletteState::default();
        self.mode = AppMode::Palette;
    }

    pub(super) fn handle_filter_key(&mut self, key: KeyEvent) {
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

    pub(super) fn toggle_current_facet(&mut self) {
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

    pub(super) fn remove_filter_token(&mut self, token: FilterToken) {
        let mut parsed = self.parsed_query();
        match token {
            FilterToken::Bookmarked => parsed.filters.bookmarked = false,
            FilterToken::Field { field, value } => parsed.filters.remove(field, &value),
        }
        self.set_query(format_query(&parsed.filters, &parsed.fuzzy));
    }

    pub(super) fn handle_columns_key(&mut self, key: KeyEvent) {
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
                self.shell.session_dirty = true;
            }
            KeyCode::Char('K') => {
                self.column_overlay.index = self.layout.move_column(self.column_overlay.index, -1);
                self.shell.session_dirty = true;
            }
            KeyCode::Char('J') => {
                self.column_overlay.index = self.layout.move_column(self.column_overlay.index, 1);
                self.shell.session_dirty = true;
            }
            KeyCode::Left | KeyCode::Char('-') | KeyCode::Char('<') => {
                self.layout.resize(self.column_overlay.index, -1);
                self.shell.session_dirty = true;
            }
            KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('>') => {
                self.layout.resize(self.column_overlay.index, 1);
                self.shell.session_dirty = true;
            }
            _ => {}
        }
    }

    fn focus_column(&mut self, index: usize) {
        self.column_overlay.index = index;
        self.column_overlay.scroll.ensure_visible(index);
    }

    pub(super) fn handle_palette_key(&mut self, key: KeyEvent) -> AppAction {
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

    pub(super) fn run_selected_command(&mut self) -> AppAction {
        let Some(command) = self.palette_commands().get(self.palette.selected).copied() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        self.mode = AppMode::Browse;
        self.run_command(command.id)
    }

    pub(super) fn run_command(&mut self, id: CommandId) -> AppAction {
        // Every command opens its overlay centred; clicking a field sets its
        // anchor afterwards, so a picker never inherits the last one's.
        self.shell.overlay_anchor = OverlayAnchor::Centered;
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
            CommandId::SetParent => {
                self.open_parent_picker();
                AppAction::None
            }
            CommandId::RemoveParent => self.remove_parent(),
            CommandId::EditDescription => self.edit_description(),
            CommandId::UndoEdit => self.undo_last_edit(),
            CommandId::AddComment => {
                self.open_prompt(PromptField::Comment);
                AppAction::None
            }
            CommandId::NewWorkItem => self.open_create_form(),
            CommandId::NewChild => self.open_child_form(),
            CommandId::DeleteWorkItem => {
                self.open_delete_confirm();
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
                self.shell.toggle_narrow_details();
                AppAction::None
            }
            CommandId::ToggleSearchOrder => {
                if !self.fuzzy_query().is_empty() {
                    self.toggle_search_order();
                }
                AppAction::None
            }
            CommandId::ToggleFinished => {
                self.toggle_show_finished();
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
                self.shell
                    .set_status(format!("Selected {} tickets", self.selected_keys.len()));
                AppAction::None
            }
            CommandId::ClearSelection => {
                self.selected_keys.clear();
                self.shell.set_status("Cleared selection");
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
            CommandId::SprintSummary => {
                self.open_sprint_summary();
                AppAction::None
            }
            CommandId::DatabaseInfo => {
                self.mode = AppMode::Info;
                AppAction::None
            }
            CommandId::Quit => {
                self.shell.should_quit = true;
                AppAction::None
            }
            CommandId::ResetPaneSplit => {
                self.shell.reset_pane_split();
                AppAction::None
            }
            CommandId::SetStaleThreshold => {
                self.cycle_stale_days();
                AppAction::None
            }
        }
    }
}
