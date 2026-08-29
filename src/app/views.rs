//! Saved and built-in views, the sprint summary, and the two settings the
//! views share: the stale threshold and whether finished work is on the table.

use super::*;

/// A view the app always offers, above whatever the user has saved: one of the
/// questions asked every morning, each written as a query somebody could have
/// typed themselves.
///
/// A built-in is not kept in the session file, cannot be deleted, and cannot be
/// saved over. It carries only a query and a sort — the columns, the row
/// density, and the search order stay as the user has them, because a view
/// answering a question has no business rearranging the table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuiltinView {
    pub name: &'static str,
    pub query: &'static str,
    pub sort_field: SortField,
    pub sort_direction: SortDirection,
}

/// The built-in a name belongs to. A built-in owns its name: one cannot be
/// saved over, and a stored view carrying the name — from a session written
/// before the built-ins existed — is dropped on load rather than listed a
/// second time under the same heading.
#[must_use]
pub(super) fn builtin_named(name: &str) -> Option<&'static BuiltinView> {
    BUILTIN_VIEWS
        .iter()
        .find(|view| view.name.eq_ignore_ascii_case(name.trim()))
}

/// What one line of the views overlay is. The headings are shown but cannot be
/// loaded, so the cursor steps over them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewRowKind {
    Heading,
    /// An index into [`BUILTIN_VIEWS`].
    Builtin(usize),
    /// An index into the views the user has saved.
    Saved(usize),
}

/// One line of the views overlay, ready to paint: the built-ins under their
/// heading, then whatever the user has saved under theirs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewRow {
    pub kind: ViewRowKind,
    /// The heading's text, or the view's name.
    pub label: String,
    /// The query the view loads, and empty for a heading.
    pub query: String,
    /// Whether this is the view the table is showing.
    pub active: bool,
}

impl ViewRow {
    #[must_use]
    pub const fn is_heading(&self) -> bool {
        matches!(self.kind, ViewRowKind::Heading)
    }

    fn heading(label: &str) -> Self {
        Self {
            kind: ViewRowKind::Heading,
            label: label.to_owned(),
            query: String::new(),
            active: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ViewsOverlay {
    pub index: usize,
    pub naming: Option<TextInput>,
    pub scroll: ScrollState,
}

/// What the sprint summary overlay is looking at.
///
/// The iteration is held here rather than read afresh each frame because
/// `\u{2190}`/`\u{2192}` walk away from the one the overlay opened on, and the
/// counts themselves are recomputed from the work items every time they are
/// asked for, so nothing here can go stale behind a sync.
#[derive(Clone, Debug, Default)]
pub struct SprintOverlay {
    /// The iteration path being counted, and `None` when there was none to
    /// open on.
    pub iteration: Option<String>,
    /// Which line the cursor is on, as an index into the overlay's rows.
    pub index: usize,
    pub scroll: ScrollState,
}

impl App {
    /// Whether the table is leaving finished work out right now: the setting
    /// says to, and the query names no state of its own that overrides it.
    #[must_use]
    pub fn finished_hidden(&self) -> bool {
        self.hides_finished(&self.parsed_query().filters)
    }

    pub(super) fn hides_finished(&self, filters: &FilterSet) -> bool {
        !self.show_finished && filters.selected_count(FilterField::State) == 0
    }

    /// Whether the session is set to list finished work, whatever the query
    /// in front of it currently asks for. This is what the palette command
    /// turns over and what the session file carries.
    #[must_use]
    pub const fn show_finished(&self) -> bool {
        self.show_finished
    }

    /// How many work items the finished rule is keeping off the table: the
    /// ones the query's filters match and the rule does not.
    ///
    /// Counted when it is asked for rather than carried along, so it is right
    /// however the visible set was last narrowed. It reads over the whole
    /// database rather than over the fuzzy result, so under a typed search it
    /// says how much finished work the filters hold rather than how much that
    /// search would have found.
    #[must_use]
    pub fn hidden_finished(&self) -> usize {
        if !self.finished_hidden() {
            return 0;
        }
        let filters = self.parsed_query().filters;
        let context = self.match_context();
        self.tickets
            .iter()
            .filter(|ticket| {
                StateCategory::of(&ticket.state).is_done()
                    && filters.matches_in(ticket, self.bookmarks.contains(&ticket.key), &context)
            })
            .count()
    }

    /// Lists or hides finished work. The visible set is rebuilt rather than
    /// filtered again, because showing them has to put rows back and filtering
    /// can only take them away.
    pub fn set_show_finished(&mut self, show: bool) {
        if self.show_finished == show {
            return;
        }
        self.show_finished = show;
        self.session_dirty = true;
        self.resubmit_query();
        self.set_status(format!(
            "Finished tickets: {}",
            if show { "shown" } else { "hidden" }
        ));
    }

    pub fn toggle_show_finished(&mut self) {
        self.set_show_finished(!self.show_finished);
    }

    /// How long a work item may sit untouched before it is flagged.
    ///
    /// The run's `--stale-days` or `TICKET_TUI_STALE_DAYS` stands over the
    /// value the session remembers, which stands over the built-in fortnight.
    /// Moving the setting from the palette clears the override, so the palette
    /// has the last word for the rest of the run and is what gets remembered.
    #[must_use]
    pub const fn stale_days(&self) -> u16 {
        match self.stale_days_override {
            Some(days) => days,
            None => self.stale_days,
        }
    }

    /// The threshold `--stale-days` or `TICKET_TUI_STALE_DAYS` asked for. It
    /// is applied after the session has been restored, so a flag beats what
    /// the last run left behind, and it is deliberately not remembered.
    pub const fn override_stale_days(&mut self, days: u16) {
        self.stale_days_override = Some(clamp_stale_days(days));
    }

    /// The palette's **Set stale threshold**: step to the next threshold above
    /// the one in force, wrapping round at the end of the list. A value that
    /// came from a flag is stepped away from and then forgotten, because the
    /// setting has now been asked for explicitly.
    pub fn cycle_stale_days(&mut self) {
        let current = self.stale_days();
        let next = STALE_DAY_CHOICES
            .into_iter()
            .find(|choice| *choice > current)
            .unwrap_or(STALE_DAY_CHOICES[0]);
        self.set_stale_days(next);
    }

    /// Moves the threshold and remembers it. The status line names the query
    /// the highlight now stands for, so the filter and the colour are visibly
    /// the same question.
    pub fn set_stale_days(&mut self, days: u16) {
        let days = clamp_stale_days(days);
        self.stale_days = days;
        self.stale_days_override = None;
        self.session_dirty = true;
        self.set_status(format!("Stale after {days} days · {}", stale_query(days)));
    }

    /// How many whole days a work item has been sitting when it counts as
    /// stale, and `None` when it does not. The details pane reports the
    /// number; the table only needs to know there is one.
    #[must_use]
    pub fn stale_age_days(&self, ticket: &Ticket) -> Option<i64> {
        self.stale_age_days_at(ticket, Timestamp::now())
    }

    /// The same against a fixed instant, which is how the highlight is tested
    /// without reaching for the clock.
    #[must_use]
    pub fn stale_age_days_at(&self, ticket: &Ticket, now: Timestamp) -> Option<i64> {
        is_stale(ticket, self.stale_days(), now).then(|| days_untouched(ticket, now))
    }

    #[must_use]
    pub fn views(&self) -> &[NamedView] {
        &self.views
    }

    /// Every line the views overlay shows: the built-ins under a `Built-in`
    /// heading, then the user's own under `Saved`, which is left out entirely
    /// when they have saved none.
    #[must_use]
    pub fn view_rows(&self) -> Vec<ViewRow> {
        let active = |name: &str| self.active_view.as_deref() == Some(name);
        let mut rows = vec![ViewRow::heading("Built-in")];
        rows.extend(
            BUILTIN_VIEWS
                .iter()
                .enumerate()
                .map(|(index, view)| ViewRow {
                    kind: ViewRowKind::Builtin(index),
                    label: view.name.to_owned(),
                    query: view.query.to_owned(),
                    active: active(view.name),
                }),
        );
        if !self.views.is_empty() {
            rows.push(ViewRow::heading("Saved"));
            rows.extend(self.views.iter().enumerate().map(|(index, view)| ViewRow {
                kind: ViewRowKind::Saved(index),
                label: view.name.clone(),
                query: view.query.clone(),
                active: active(&view.name),
            }));
        }
        rows
    }

    /// Whether `d` and the overlay's Delete control have anything to act on:
    /// only a view the user saved can be deleted.
    #[must_use]
    pub fn can_delete_focused_view(&self) -> bool {
        matches!(
            self.view_rows()
                .get(self.views_overlay.index)
                .map(|row| row.kind),
            Some(ViewRowKind::Saved(_))
        )
    }

    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    pub(super) fn open_views(&mut self) {
        self.views_overlay = ViewsOverlay::default();
        self.focus_view(0);
        self.mode = AppMode::Views;
    }

    pub(super) fn handle_views_key(&mut self, key: KeyEvent) -> AppAction {
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
            KeyCode::Up | KeyCode::Char('k') => self.move_view_focus(false),
            KeyCode::Down | KeyCode::Char('j') => self.move_view_focus(true),
            KeyCode::Enter => self.apply_view_at(self.views_overlay.index),
            KeyCode::Char('n') => self.views_overlay.naming = Some(TextInput::default()),
            KeyCode::Char('d') | KeyCode::Delete => self.delete_view_at(self.views_overlay.index),
            _ => {}
        }
        AppAction::None
    }

    /// Puts the cursor on a row, stepping past a heading to the view under it,
    /// and onto the last view when the index runs off the end. There is always
    /// somewhere to land: the built-ins are never empty.
    fn focus_view(&mut self, index: usize) {
        let rows = self.view_rows();
        let loadable = |from: usize| {
            rows.iter()
                .enumerate()
                .skip(from)
                .find(|(_, row)| !row.is_heading())
                .map(|(index, _)| index)
        };
        let index = loadable(index)
            .or_else(|| rows.iter().rposition(|row: &ViewRow| !row.is_heading()))
            .unwrap_or_default();
        self.views_overlay.index = index;
        self.views_overlay.scroll.ensure_visible(index);
    }

    /// Moves the cursor one row on or back, skipping the headings, and stays
    /// put at either end of the list.
    fn move_view_focus(&mut self, forward: bool) {
        let current = self.views_overlay.index;
        let loadable: Vec<usize> = self
            .view_rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| !row.is_heading())
            .map(|(index, _)| index)
            .collect();
        let next = if forward {
            loadable.into_iter().find(|index| *index > current)
        } else {
            loadable.into_iter().rev().find(|index| *index < current)
        };
        if let Some(next) = next {
            self.focus_view(next);
        }
    }

    pub(super) fn save_view(&mut self, name: String) {
        if let Some(builtin) = builtin_named(&name) {
            let name = builtin.name;
            self.set_status(format!("'{name}' is a built-in view; choose another name"));
            return;
        }
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

    /// Loads the view on one row of the overlay, whichever kind it is. A
    /// heading loads nothing.
    pub(super) fn apply_view_at(&mut self, index: usize) {
        match self.view_rows().get(index).map(|row| row.kind) {
            Some(ViewRowKind::Builtin(builtin)) => self.apply_builtin_view(BUILTIN_VIEWS[builtin]),
            Some(ViewRowKind::Saved(saved)) => self.apply_saved_view(saved),
            Some(ViewRowKind::Heading) | None => {}
        }
    }

    /// A built-in sets the query and the sort and leaves the rest of the table
    /// alone, so loading one never rearranges the columns somebody has set up.
    fn apply_builtin_view(&mut self, view: BuiltinView) {
        self.active_view = Some(view.name.to_owned());
        self.sort_field = view.sort_field;
        self.sort_direction = view.sort_direction;
        self.session_dirty = true;
        self.set_query(view.query.to_owned());
        self.mode = AppMode::Browse;
        self.set_status(format!("Loaded view '{}'", view.name));
    }

    fn apply_saved_view(&mut self, index: usize) {
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

    pub(super) fn delete_view_at(&mut self, index: usize) {
        let saved = match self.view_rows().get(index).map(|row| row.kind) {
            Some(ViewRowKind::Saved(saved)) => saved,
            Some(ViewRowKind::Builtin(builtin)) => {
                let name = BUILTIN_VIEWS[builtin].name;
                self.set_status(format!("'{name}' is a built-in view and cannot be deleted"));
                return;
            }
            Some(ViewRowKind::Heading) | None => return,
        };
        let removed = self.views.remove(saved);
        if self.active_view.as_deref() == Some(removed.name.as_str()) {
            self.active_view = None;
        }
        self.focus_view(index);
        self.session_dirty = true;
        self.set_status(format!("Deleted view '{}'", removed.name));
    }

    /// The iteration the sprint summary counts: the sprint the project is in,
    /// falling back to the one the selected work item is planned into.
    ///
    /// The fallback is not a nicety. `current_iteration` reads the dates on
    /// the iteration tree and nothing else, so on a project whose sprints were
    /// never given a start and a finish — which is most of them, early on —
    /// nothing is ever current and the selected row is the only thing saying
    /// which sprint was meant.
    #[must_use]
    pub fn summary_iteration(&self) -> Option<String> {
        self.current_iteration()
            .or_else(|| {
                self.selected_ticket()
                    .map(|ticket| ticket.iteration_path.clone())
            })
            .map(|path| path.trim().to_owned())
            .filter(|path| !path.is_empty())
    }

    /// Counts the iteration the overlay is on, and nothing at all when it
    /// opened on none.
    ///
    /// The counts are taken over [`Self::tickets`] — every work item on file —
    /// rather than over the visible rows. The table hides finished work by
    /// default, so a summary reading the table would report a Done column that
    /// never filled up and a sprint that never finished.
    #[must_use]
    pub fn sprint_summary(&self) -> Option<SprintSummary> {
        let iteration = self.sprint_overlay.iteration.as_deref()?;
        Some(sprint::summarize(
            self.tickets(),
            iteration,
            self.stale_days(),
            Timestamp::now(),
        ))
    }

    /// The lines the overlay paints. One opened with no sprint to count says
    /// as much, rather than painting an empty grid.
    #[must_use]
    pub fn summary_rows(&self) -> Vec<SummaryRow> {
        self.sprint_summary().map_or_else(
            || {
                NO_SPRINT_NOTICE
                    .iter()
                    .copied()
                    .map(SummaryRow::note)
                    .collect()
            },
            |summary| summary.rows(),
        )
    }

    /// What the overlay's title bar names, which is the sprint rather than the
    /// path it hangs off: `Sprint 1`, not `development\Sprint 1`.
    #[must_use]
    pub fn summary_title(&self) -> String {
        self.sprint_overlay.iteration.as_deref().map_or_else(
            || " Sprint summary ".to_owned(),
            |iteration| format!(" Sprint summary \u{b7} {} ", path_leaf(iteration)),
        )
    }

    pub(super) fn open_sprint_summary(&mut self) {
        self.sprint_overlay.iteration = self.summary_iteration();
        self.sprint_overlay.scroll.scroll_to(0);
        self.focus_summary_row(0);
        self.mode = AppMode::Sprint;
    }

    pub(super) fn handle_sprint_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => self.move_summary_focus(false),
            KeyCode::Down | KeyCode::Char('j') => self.move_summary_focus(true),
            KeyCode::Left | KeyCode::Char('h') => self.step_summary_iteration(-1),
            KeyCode::Right | KeyCode::Char('l') => self.step_summary_iteration(1),
            KeyCode::Enter => self.apply_summary_row(self.sprint_overlay.index),
            _ => {}
        }
    }

    /// Puts the cursor on the first row at or after `index` that `Enter` can
    /// do something with, and on the top line when there is none.
    fn focus_summary_row(&mut self, index: usize) {
        let landing = self
            .summary_rows()
            .iter()
            .enumerate()
            .skip(index)
            .find(|(_, row)| row.is_selectable())
            .map_or(0, |(index, _)| index);
        self.sprint_overlay.index = landing;
        self.sprint_overlay.scroll.ensure_visible(landing);
    }

    /// Moves the cursor one grid row on or back, stepping over the headings
    /// and the tallies, and staying put at either end of the grid.
    fn move_summary_focus(&mut self, forward: bool) {
        let current = self.sprint_overlay.index;
        let selectable: Vec<usize> = self
            .summary_rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| row.is_selectable())
            .map(|(index, _)| index)
            .collect();
        let next = if forward {
            selectable.into_iter().find(|index| *index > current)
        } else {
            selectable.into_iter().rev().find(|index| *index < current)
        };
        if let Some(next) = next {
            self.sprint_overlay.index = next;
            self.sprint_overlay.scroll.ensure_visible(next);
        }
    }

    /// The iterations `\u{2190}`/`\u{2192}` step between: the cached iteration
    /// tree without its roots, since a project root is somewhere to file work
    /// rather than a sprint. Empty until the trees have been fetched, which is
    /// what leaves the two keys doing nothing.
    fn summary_iterations(&self) -> Vec<&str> {
        self.classification_nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Iteration && node.depth > 0)
            .map(|node| node.path.as_str())
            .collect()
    }

    /// Moves the overlay onto the previous or next cached iteration, stopping
    /// at either end of the tree rather than wrapping round it. An iteration
    /// the tree does not hold — a work item planned into a node fetched since —
    /// starts from whichever end the key points at.
    fn step_summary_iteration(&mut self, delta: isize) {
        let iterations = self.summary_iterations();
        let Some(last) = iterations.len().checked_sub(1) else {
            return;
        };
        let at = self
            .sprint_overlay
            .iteration
            .as_deref()
            .and_then(|current| {
                iterations
                    .iter()
                    .position(|node| node.eq_ignore_ascii_case(current))
            });
        let next = match at {
            Some(index) => index.saturating_add_signed(delta).min(last),
            None if delta < 0 => last,
            None => 0,
        };
        let next = iterations[next].to_owned();
        if self.sprint_overlay.iteration.as_deref() == Some(next.as_str()) {
            return;
        }
        self.sprint_overlay.iteration = Some(next);
        self.sprint_overlay.scroll.scroll_to(0);
        self.focus_summary_row(0);
    }

    /// `Enter` on a grid row: the table is filtered to that person's work in
    /// this iteration and the overlay closes, so the rows it counted are
    /// there to look at. The Total row asks for the iteration alone.
    ///
    /// The iteration goes into the query as the full path the counts were taken
    /// over, because that is what makes the two agree: the summary counts a
    /// work item whose whole `iteration_path` matches, while a leaf written on
    /// its own matches any node ending in that name, so two sprints named
    /// `Sprint 1` under different parents would put rows in the table the grid
    /// never counted. The status line still says the leaf, which is what the
    /// table, the chips, and the picker all call it.
    pub(super) fn apply_summary_row(&mut self, index: usize) {
        let Some(summary) = self.sprint_summary() else {
            return;
        };
        let assignee = match summary.rows().get(index).map(|row| row.kind) {
            Some(SummaryRowKind::Assignee(row)) => {
                let Some(counts) = summary.assignees.get(row) else {
                    return;
                };
                Some(counts.name.clone())
            }
            Some(SummaryRowKind::Total) => None,
            Some(SummaryRowKind::Heading | SummaryRowKind::Note | SummaryRowKind::Blank) | None => {
                return;
            }
        };
        let leaf = path_leaf(&summary.iteration).to_owned();
        let mut filters = FilterSet::default();
        filters.insert(FilterField::Iteration, summary.iteration.clone());
        if let Some(assignee) = assignee.clone() {
            filters.insert(FilterField::Assignee, assignee);
        }
        self.active_view = None;
        self.set_query(format_query(&filters, ""));
        self.mode = AppMode::Browse;
        self.set_status(match assignee {
            Some(name) => format!("{name} in {leaf}"),
            None => format!("All work in {leaf}"),
        });
    }
}
