//! The search box, the filter grammar, the facet bar, the column and view
//! overlays, the command palette and sorting.

use super::*;
use crate::columns::ColumnLayout;

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
    /// The pills the last frame had room to draw. The bar takes as many of
    /// [`FilterField::BAR`] as fit, so what is a pill and what falls to the
    /// Filters overlay is a question of width.
    pub shown: Vec<FilterField>,
}

#[derive(Clone, Debug, Default)]
pub struct ColumnOverlay {
    /// Where the cursor is and how far the list is scrolled.
    pub cursor: ListCursor,
}

#[derive(Clone, Debug, Default)]
pub struct PaletteState {
    pub query: TextInput,
    pub selected: usize,
    pub scroll: ScrollState,
}

impl WorkItemsScreen {
    #[must_use]
    pub fn query(&self) -> &str {
        self.query.text()
    }

    #[must_use]
    pub const fn query_cursor(&self) -> usize {
        self.query.cursor()
    }

    #[must_use]
    pub fn parsed_query(&self) -> ParsedQuery<WorkItemSchema> {
        parse_query::<WorkItemSchema>(self.query.text())
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
    fn effective_filters(&self) -> FilterSet<WorkItemSchema> {
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
    pub fn match_context(&self, shell: &Shell) -> MatchContext {
        MatchContext::now()
            .with_me(shell.me.clone())
            .with_current_iteration(self.current_iteration())
    }

    #[must_use]
    pub fn fuzzy_query(&self) -> String {
        self.parsed_query().fuzzy
    }

    #[must_use]
    pub fn filter_tokens(&self) -> Vec<FilterToken<WorkItemSchema>> {
        self.parsed_query().filters.tokens()
    }

    /// The chips the bar draws: every token whose field is not already a facet
    /// pill, each with its place in [`Self::filter_tokens`] so a chip's `×`
    /// can name it without the pointer target naming a field.
    #[must_use]
    pub fn overflow_filter_tokens(&self) -> Vec<(usize, FilterToken<WorkItemSchema>)> {
        self.filter_tokens()
            .into_iter()
            .enumerate()
            .filter(|(_, token)| match token {
                FilterToken::Bookmarked => true,
                FilterToken::Field { field, .. } => !self.facet_bar.shown.contains(field),
            })
            .collect()
    }

    #[must_use]
    pub fn facets_for(&self, shell: &Shell, field: FilterField) -> Vec<FacetValue> {
        let filters = self.effective_filters();
        facet_values(
            self.tickets(),
            &filters,
            field,
            |ticket| self.bookmarks.contains(&ticket.key),
            &self.match_context(shell),
        )
    }

    pub fn toggle_filter(&mut self, shell: &mut Shell, field: FilterField, value: &str) {
        let mut parsed = self.parsed_query();
        parsed.filters.toggle(field, value);
        self.set_query(shell, format_query(&parsed.filters, &parsed.fuzzy));
    }

    #[must_use]
    pub fn palette_commands(&self) -> Vec<Command> {
        matching_commands(
            self.palette.query.text(),
            !self.show_finished,
            self.palette_tab,
        )
    }

    /// Opens one of the overlays every tab shares — the help, the palette,
    /// the columns editor, the database overlay — on behalf of `tab`, which
    /// is the one showing. The palette lists that tab's commands, and what it
    /// chooses goes back to that tab as [`AppAction::RunCommand`].
    pub fn open_shell_overlay(&mut self, shell: &mut Shell, id: CommandId, tab: TabId) {
        self.run_command(shell, id);
        self.palette_tab = tab;
    }

    /// Whether one of those shared overlays is open.
    #[must_use]
    pub fn shell_overlay_open(&self) -> bool {
        matches!(
            self.mode,
            WorkItemMode::Help | WorkItemMode::Palette | WorkItemMode::Columns | WorkItemMode::Info
        )
    }

    #[must_use]
    pub fn facet_field(&self) -> FilterField {
        FilterField::OVERLAY[self
            .filter_overlay
            .field_index
            .min(FilterField::OVERLAY.len() - 1)]
    }

    #[must_use]
    pub fn current_facets(&self, shell: &Shell) -> Vec<FacetValue> {
        let filters = self.effective_filters();
        facet_values(
            self.tickets(),
            &filters,
            self.facet_field(),
            |ticket| self.bookmarks.contains(&ticket.key),
            &self.match_context(shell),
        )
    }

    pub fn poll_search(&mut self, shell: &mut Shell) -> bool {
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
        self.apply_filters(shell);
        self.sort_visible();
        self.restore_selection(shell, selected.as_ref());
        self.search_pending = false;
        true
    }

    pub fn set_query(&mut self, shell: &mut Shell, query: String) {
        if self.query.text() == query {
            self.query.move_end();
            return;
        }
        self.query.set_text(query);
        self.after_query_edit(shell);
    }

    pub fn handle_paste(&mut self, shell: &mut Shell, pasted: &str) {
        match self.active_editor() {
            Some(TextEditor::Search) => self.edit_query(shell, |query| query.paste(pasted, true)),
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
    pub(super) fn edit_query(&mut self, shell: &mut Shell, edit: impl FnOnce(&mut TextInput)) {
        let before = self.query.text().to_owned();
        edit(&mut self.query);
        if self.query.text() != before {
            self.after_query_edit(shell);
        }
    }

    fn after_query_edit(&mut self, shell: &mut Shell) {
        self.search_history_index = None;
        self.search_history_draft = self.query.text().to_owned();
        shell.session_dirty = true;
        self.resubmit_query(shell);
    }

    /// Works the visible set out again from every row, for when what the table
    /// asks of them has changed rather than which rows there are.
    pub(super) fn resubmit_query(&mut self, shell: &mut Shell) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        if self.fuzzy_query().is_empty() {
            self.search_generation = self.search_generation.wrapping_add(1);
            self.search_pending = false;
            self.pending_selection = None;
            self.show_all(shell, selected.as_ref());
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

    pub(super) fn active_editor(&self) -> Option<TextEditor> {
        match self.mode {
            WorkItemMode::Search => Some(TextEditor::Search),
            WorkItemMode::Palette => Some(TextEditor::Palette),
            WorkItemMode::Views if self.views_overlay.naming.is_some() => {
                Some(TextEditor::ViewName)
            }
            WorkItemMode::Prompt => Some(TextEditor::Prompt),
            WorkItemMode::AssigneePicker => Some(TextEditor::Assignee),
            WorkItemMode::ParentPicker => Some(TextEditor::Parent),
            WorkItemMode::NodePicker => Some(TextEditor::Node),
            WorkItemMode::Form => Some(TextEditor::Form),
            _ => None,
        }
    }

    pub fn set_sort(&mut self, shell: &mut Shell, field: SortField, direction: SortDirection) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.sort_field = field;
        self.sort_direction = direction;
        self.sort_visible();
        self.restore_selection(shell, selected.as_ref());
        shell.session_dirty = true;
    }

    pub fn toggle_row_density(&mut self, shell: &mut Shell) {
        self.row_density = self.row_density.toggled();
        shell.session_dirty = true;
        shell.set_status(format!("Row density: {}", self.row_density.label()));
    }

    pub fn toggle_search_order(&mut self, shell: &mut Shell) {
        if self.fuzzy_query().is_empty() {
            return;
        }
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.search_order = self.search_order.toggled();
        self.sort_visible();
        self.restore_selection(shell, selected.as_ref());
        shell.session_dirty = true;
        shell.set_status(format!("Search order: {}", self.search_order.label()));
    }

    pub fn toggle_sort(&mut self, shell: &mut Shell, field: SortField) {
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
        self.set_sort(shell, field, direction);
    }

    pub(super) fn begin_search(&mut self) {
        self.query.move_end();
        self.search_history_index = None;
        self.search_history_draft = self.query.text().to_owned();
        self.mode = WorkItemMode::Search;
    }

    pub(super) fn finish_search(&mut self, shell: &mut Shell) {
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
        self.mode = WorkItemMode::Browse;
        self.record_history(shell);
    }

    pub(super) fn recall_previous_search(&mut self, shell: &mut Shell) {
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
        self.set_query(shell, query);
        self.search_history_draft = draft;
        self.search_history_index = Some(target);
    }

    pub(super) fn recall_next_search(&mut self, shell: &mut Shell) {
        let Some(index) = self.search_history_index else {
            return;
        };
        let draft = self.search_history_draft.clone();
        if index + 1 < self.search_history.len() {
            let target = index + 1;
            let query = self.search_history[target].clone();
            self.set_query(shell, query);
            self.search_history_draft = draft;
            self.search_history_index = Some(target);
        } else {
            self.set_query(shell, draft);
            self.search_history_index = None;
        }
    }

    pub(super) fn submit_search(&mut self) {
        self.search_generation = self.search.submit(&self.fuzzy_query());
        self.search_pending = true;
    }

    pub(super) fn show_all(&mut self, shell: &mut Shell, selected: Option<&TicketKey>) {
        self.visible = (0..self.tickets.len())
            .map(|ticket_index| SearchMatch {
                ticket_index,
                score: 0,
            })
            .collect();
        self.apply_filters(shell);
        self.sort_visible();
        self.restore_selection(shell, selected);
    }

    pub(super) fn apply_filters(&mut self, shell: &mut Shell) {
        let filters = self.effective_filters();
        let context = self.match_context(shell);
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

    pub(super) fn restore_selection(&mut self, shell: &mut Shell, selected: Option<&TicketKey>) {
        let row = selected.and_then(|key| {
            self.visible
                .iter()
                .position(|entry| self.tickets[entry.ticket_index].key == *key)
        });
        self.table_state
            .select((!self.visible.is_empty()).then_some(row.unwrap_or_default()));
        self.sync_family_state(shell);
        if selected.is_none() || row.is_none() {
            self.table.scroll_to(0);
            *self.table_state.offset_mut() = 0;
            self.details.scroll_to(0);
        }
    }

    pub(super) fn open_filters(&mut self) {
        self.filter_overlay = FilterOverlay::default();
        self.mode = WorkItemMode::Filter;
    }

    pub(super) fn open_facets(&mut self, field_index: usize) {
        self.facet_bar.field_index = field_index.min(self.facet_bar.shown.len());
        self.facet_bar.value_index = 0;
        self.mode = WorkItemMode::Facets;
    }

    pub(super) fn handle_facet_key(&mut self, shell: &mut Shell, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('f') => self.mode = WorkItemMode::Browse,
            KeyCode::Char('+') => self.open_filters(),
            KeyCode::Left | KeyCode::Char('h') => {
                self.facet_bar.field_index = self.facet_bar.field_index.saturating_sub(1);
                self.facet_bar.value_index = 0;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.facet_bar.field_index =
                    (self.facet_bar.field_index + 1).min(self.facet_bar.shown.len());
                self.facet_bar.value_index = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_facet_value(self.facet_bar.value_index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let count = self.focused_bar_facets(shell).len();
                if count > 0 {
                    self.focus_facet_value((self.facet_bar.value_index + 1).min(count - 1));
                }
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                if self.facet_bar.field_index >= self.facet_bar.shown.len() {
                    self.open_filters();
                } else {
                    self.toggle_current_bar_facet(shell);
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
        self.facet_bar
            .shown
            .get(self.facet_bar.field_index)
            .copied()
    }

    fn focused_bar_facets(&self, shell: &Shell) -> Vec<FacetValue> {
        self.focused_bar_field()
            .map_or_else(Vec::new, |field| self.facets_for(shell, field))
    }

    pub(super) fn toggle_current_bar_facet(&mut self, shell: &mut Shell) {
        let Some(field) = self.focused_bar_field() else {
            return;
        };
        let Some(value) = self
            .focused_bar_facets(shell)
            .get(self.facet_bar.value_index)
            .map(|facet| facet.value.clone())
        else {
            return;
        };
        self.toggle_filter(shell, field, &value);
    }

    fn open_columns(&mut self) {
        self.column_overlay.cursor.reset();
        self.mode = WorkItemMode::Columns;
    }

    pub(super) fn open_palette(&mut self) {
        self.palette = PaletteState::default();
        self.palette_tab = TabId::WorkItems;
        self.mode = WorkItemMode::Palette;
    }

    pub(super) fn handle_filter_key(&mut self, shell: &mut Shell, key: KeyEvent) {
        match key.code {
            KeyCode::Esc if self.filter_overlay.showing_values => {
                self.filter_overlay.showing_values = false;
                self.filter_overlay.value_index = 0;
            }
            KeyCode::Esc | KeyCode::Char('f') => self.mode = WorkItemMode::Browse,
            KeyCode::Left | KeyCode::Char('h') if self.filter_overlay.showing_values => {
                self.filter_overlay.showing_values = false;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter
                if !self.filter_overlay.showing_values =>
            {
                self.filter_overlay.showing_values = true;
                self.filter_overlay.value_index = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_filter_cursor(shell, -1),
            KeyCode::Down | KeyCode::Char('j') => self.move_filter_cursor(shell, 1),
            KeyCode::Char(' ') | KeyCode::Enter if self.filter_overlay.showing_values => {
                self.toggle_current_facet(shell);
            }
            _ => {}
        }
    }

    fn move_filter_cursor(&mut self, shell: &mut Shell, delta: isize) {
        let index = if self.filter_overlay.showing_values {
            let count = self.current_facets(shell).len();
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
                .min(FilterField::OVERLAY.len() - 1);
            self.filter_overlay.field_index
        };
        self.filter_overlay.scroll.ensure_visible(index);
    }

    pub(super) fn toggle_current_facet(&mut self, shell: &mut Shell) {
        let field = self.facet_field();
        let Some(value) = self
            .current_facets(shell)
            .get(self.filter_overlay.value_index)
            .map(|facet| facet.value.clone())
        else {
            return;
        };
        self.toggle_filter(shell, field, &value);
    }

    /// Takes one filter off, by its place in [`Self::filter_tokens`] — which is
    /// the index a chip's `×` carries.
    pub(super) fn remove_filter_token(&mut self, shell: &mut Shell, index: usize) {
        let Some(token) = self.filter_tokens().into_iter().nth(index) else {
            return;
        };
        let mut parsed = self.parsed_query();
        match token {
            FilterToken::Bookmarked => parsed.filters.bookmarked = false,
            FilterToken::Field { field, value } => parsed.filters.remove(field, &value),
        }
        self.set_query(shell, format_query(&parsed.filters, &parsed.fuzzy));
    }

    pub(super) fn handle_columns_key(&mut self, shell: &mut Shell, key: KeyEvent) {
        // The editor works on whichever layout it is handed, so the same keys
        // edit another tab's columns; here that is this screen's own.
        let mut layout = std::mem::take(&mut self.layout);
        self.handle_columns_key_on(shell, key, &mut layout);
        self.layout = layout;
    }

    /// The columns editor's keys, applied to `layout`: this screen's own, or
    /// another tab's when the editor was opened there.
    pub fn handle_columns_key_on(
        &mut self,
        shell: &mut Shell,
        key: KeyEvent,
        layout: &mut dyn ColumnLayout,
    ) {
        let last = layout.count().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('w') | KeyCode::Enter => self.mode = WorkItemMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_column(self.column_overlay.cursor.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_column((self.column_overlay.cursor.index + 1).min(last));
            }
            KeyCode::Char(' ') => {
                let index = self.column_overlay.cursor.index;
                layout.toggle_visible(index);
                shell.session_dirty = true;
            }
            KeyCode::Char('K') => {
                let index = self.column_overlay.cursor.index;
                self.column_overlay.cursor.index = layout.move_column(index, -1);
                shell.session_dirty = true;
            }
            KeyCode::Char('J') => {
                let index = self.column_overlay.cursor.index;
                self.column_overlay.cursor.index = layout.move_column(index, 1);
                shell.session_dirty = true;
            }
            KeyCode::Left | KeyCode::Char('-') | KeyCode::Char('<') => {
                let index = self.column_overlay.cursor.index;
                layout.resize(index, -1);
                shell.session_dirty = true;
            }
            KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('>') => {
                let index = self.column_overlay.cursor.index;
                layout.resize(index, 1);
                shell.session_dirty = true;
            }
            _ => {}
        }
    }

    /// A click on one of the columns editor's rows or controls, applied to
    /// `layout`. Answers whether the target was one of the editor's.
    pub fn apply_column_target(
        &mut self,
        shell: &mut Shell,
        target: &PointerTarget,
        layout: &mut dyn ColumnLayout,
    ) -> bool {
        match *target {
            PointerTarget::ColumnToggle { index } => {
                self.column_overlay.cursor.focus(index);
                layout.toggle_visible(index);
            }
            PointerTarget::ColumnMove { index, delta } => {
                self.column_overlay.cursor.index = layout.move_column(index, delta);
            }
            PointerTarget::ColumnResize { index, delta } => {
                self.column_overlay.cursor.focus(index);
                layout.resize(index, delta);
            }
            _ => return false,
        }
        shell.session_dirty = true;
        true
    }

    fn focus_column(&mut self, index: usize) {
        self.column_overlay.cursor.focus(index);
    }

    pub(super) fn handle_palette_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.mode = WorkItemMode::Browse,
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_palette_selection(-1);
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_palette_selection(1);
            }
            KeyCode::Up => self.move_palette_selection(-1),
            KeyCode::Down => self.move_palette_selection(1),
            KeyCode::Enter => return self.run_selected_command(shell),
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

    pub(super) fn run_selected_command(&mut self, shell: &mut Shell) -> AppAction {
        let Some(command) = self.palette_commands().get(self.palette.selected).copied() else {
            self.mode = WorkItemMode::Browse;
            return AppAction::None;
        };
        self.mode = WorkItemMode::Browse;
        // A palette opened for another tab hands its choice back to that
        // tab rather than running it on the work items.
        if self.palette_tab != TabId::WorkItems {
            self.palette_tab = TabId::WorkItems;
            return AppAction::RunCommand(command.id);
        }
        self.run_command(shell, command.id)
    }

    pub fn run_command(&mut self, shell: &mut Shell, id: CommandId) -> AppAction {
        // Every command opens its overlay centred; clicking a field sets its
        // anchor afterwards, so a picker never inherits the last one's.
        shell.overlay_anchor = OverlayAnchor::Centered;
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
                self.open_state_picker(shell);
                AppAction::None
            }
            CommandId::EditTitle => {
                self.open_prompt(shell, PromptField::Title);
                AppAction::None
            }
            CommandId::EditPriority => {
                self.open_priority_picker(shell);
                AppAction::None
            }
            CommandId::EditTags => {
                self.open_prompt(shell, PromptField::Tags);
                AppAction::None
            }
            CommandId::EditAssignee => self.open_assignee_picker(shell),
            CommandId::EditIteration => self.open_node_picker(shell, NodeKind::Iteration),
            CommandId::EditArea => self.open_node_picker(shell, NodeKind::Area),
            CommandId::SetParent => {
                self.open_parent_picker(shell);
                AppAction::None
            }
            CommandId::RemoveParent => self.remove_parent(shell),
            CommandId::EditDescription => self.edit_description(shell),
            CommandId::UndoEdit => self.undo_last_edit(shell),
            CommandId::AddComment => {
                self.open_prompt(shell, PromptField::Comment);
                AppAction::None
            }
            CommandId::NewWorkItem => self.open_create_form(shell),
            CommandId::NewChild => self.open_child_form(shell),
            CommandId::DeleteWorkItem => {
                self.open_delete_confirm(shell);
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
                self.mode = WorkItemMode::Sort;
                AppAction::None
            }
            CommandId::Help => {
                self.help.scroll_to(0);
                self.mode = WorkItemMode::Help;
                AppAction::None
            }
            CommandId::Sync => AppAction::Sync,
            CommandId::Open => {
                self.record_history(shell);
                self.open_selected()
            }
            CommandId::ToggleDensity => {
                self.toggle_row_density(shell);
                AppAction::None
            }
            CommandId::ToggleDetails => {
                shell.toggle_narrow_details();
                AppAction::None
            }
            CommandId::ToggleSearchOrder => {
                if !self.fuzzy_query().is_empty() {
                    self.toggle_search_order(shell);
                }
                AppAction::None
            }
            CommandId::ToggleFinished => {
                self.toggle_show_finished(shell);
                AppAction::None
            }
            CommandId::ToggleBookmark => {
                self.toggle_bookmark(shell);
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
                shell.set_status(format!("Selected {} tickets", self.selected_keys.len()));
                AppAction::None
            }
            CommandId::ClearSelection => {
                self.selected_keys.clear();
                shell.set_status("Cleared selection");
                AppAction::None
            }
            // Where this run has been is the shell's, not one screen's: the
            // walk can cross tabs.
            CommandId::HistoryBack => AppAction::HistoryBack,
            CommandId::HistoryForward => AppAction::HistoryForward,
            // The Repos tab's own; the palette does not offer them here.
            // Another tab's verbs: nothing to do with a work item.
            CommandId::CloneRepo
            | CommandId::FetchRepo
            | CommandId::PullRepo
            | CommandId::ApprovePr
            | CommandId::SuggestPr
            | CommandId::WaitPr
            | CommandId::RejectPr
            | CommandId::UndoVote
            | CommandId::CompletePr
            | CommandId::AbandonPr
            | CommandId::AutoCompletePr
            | CommandId::CommentPr
            | CommandId::ToggleClosedPrs
            | CommandId::RunPipeline
            | CommandId::CancelRun
            | CommandId::RetryRun
            | CommandId::WatchRun
            | CommandId::Approvals
            | CommandId::ShowLogs
            | CommandId::DescribePod
            | CommandId::PreviousLogs
            | CommandId::NextContainer => AppAction::None,
            CommandId::SprintSummary => {
                self.open_sprint_summary();
                AppAction::None
            }
            CommandId::DatabaseInfo => {
                self.mode = WorkItemMode::Info;
                AppAction::None
            }
            CommandId::Quit => {
                shell.should_quit = true;
                AppAction::None
            }
            CommandId::ResetPaneSplit => {
                shell.reset_pane_split();
                AppAction::None
            }
            CommandId::SetStaleThreshold => {
                self.cycle_stale_days(shell);
                AppAction::None
            }
        }
    }
}
