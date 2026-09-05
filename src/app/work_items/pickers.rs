//! The overlays that pick a value: state, priority, assignee, parent,
//! classification node and work item type.

use super::*;

/// The state picker, built when it opens so it never reads the network.
#[derive(Clone, Debug, Default)]
pub struct StatePicker {
    /// Where the cursor is and how far the list is scrolled.
    pub cursor: ListCursor,
    /// Every state the selected work item's type allows.
    pub options: Vec<StateOption>,
    /// The state the work item is already in, which `Enter` treats as a no-op.
    pub current: String,
    /// What the picker was opened over, shown in its title.
    pub scope: EditScope,
}

/// The priority picker, built when it opens from the row it was opened on.
#[derive(Clone, Debug, Default)]
pub struct PriorityPicker {
    /// Where the cursor is and how far the list is scrolled.
    pub cursor: ListCursor,
    /// The priority the work item already has, which `Enter` treats as a no-op.
    pub current: Option<i64>,
    /// The work item the picker was opened for, shown in its title.
    pub id: i64,
}

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
            Some(name) => !self.unassigned && same_text(&self.display, name),
            None => self.unassigned,
        }
    }
}

/// The assignee picker: everybody worth offering, filtered by whatever has been
/// typed. Built when it opens, so it never waits for the network.
#[derive(Clone, Debug, Default)]
pub struct AssigneePicker {
    /// Where the cursor is and how far the list is scrolled.
    pub cursor: ListCursor,
    /// Every candidate, in the order they were gathered.
    pub candidates: Vec<AssigneeCandidate>,
    pub query: TextInput,
    /// Who holds the work item now, which `Enter` treats as a no-op.
    pub current: Option<String>,
    /// What the picker was opened over, shown in its title.
    pub scope: EditScope,
}

/// One work item the parent picker offers: enough of it to read the row and to
/// match what has been typed, which is its id and its title.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParentCandidate {
    pub key: TicketKey,
    pub work_item_type: String,
    pub title: String,
}

/// The parent picker: every work item the selected one could be filed under,
/// filtered by whatever has been typed. Built when it opens from the rows
/// already loaded, so it never waits for the network.
///
/// A work item cannot be its own ancestor, so neither the work item itself nor
/// anything below it is ever a candidate. That is what makes a cycle
/// unaskable-for rather than merely refused.
#[derive(Clone, Debug)]
pub struct ParentPicker {
    /// Where the cursor is and how far the list is scrolled.
    pub cursor: ListCursor,
    /// Every work item that could be the parent, in table order.
    pub candidates: Vec<ParentCandidate>,
    pub query: TextInput,
    /// The work item being moved, which is what the picker's title names.
    pub child: TicketKey,
    /// The parent it hangs under now, which `Enter` treats as a no-op.
    pub current: Option<TicketKey>,
}

impl Default for ParentPicker {
    /// A picker nobody has opened yet, over no work item: id `0`, the same
    /// stand-in [`EditScope::default`] uses for a scope nothing has been
    /// chosen for. [`WorkItemsScreen::open_parent_picker`] fills it in before it is read.
    fn default() -> Self {
        Self {
            candidates: Vec::new(),
            query: TextInput::default(),
            cursor: ListCursor::default(),
            child: TicketKey {
                organization: String::new(),
                id: 0,
            },
            current: None,
        }
    }
}

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
    /// Where the cursor is and how far the list is scrolled.
    pub cursor: ListCursor,
    /// Which tree is open, which is also the field a choice is written to.
    pub kind: NodeKind,
    /// Every row, in tree order.
    pub rows: Vec<NodeRow>,
    pub query: TextInput,
    /// The path the work item carries now, which `Enter` treats as a no-op.
    pub current: String,
    /// What the picker was opened over, shown in its title.
    pub scope: EditScope,
}

impl Default for NodePicker {
    fn default() -> Self {
        Self {
            kind: NodeKind::Iteration,
            rows: Vec::new(),
            query: TextInput::default(),
            cursor: ListCursor::default(),
            current: String::new(),
            scope: EditScope::default(),
        }
    }
}

/// The work item type picker: every type the project's process offers, built
/// when it opens so it never waits for the network.
#[derive(Clone, Debug, Default)]
pub struct TypePicker {
    /// Where the cursor is and how far the list is scrolled.
    pub cursor: ListCursor,
    pub options: Vec<String>,
    /// The type the form already names, which the picker marks.
    pub current: String,
    /// The form field the choice is written back into.
    pub field: FormFieldId,
}

/// How each stock process breaks work down, top to bottom: Basic, Agile, Scrum
/// and CMMI. `GET /<project>/_apis/wit/workitemtypes` answers in an order of
/// its own that is not a hierarchy — an org's list can read `Issue, Epic,
/// Task` — so the breakdown is held here rather than read out of that list.
///
/// Basic comes first because it wins a tie, which is what a project whose own
/// types have not been read yet is assumed to use.
const WORK_ITEM_BREAKDOWNS: [&[&str]; 4] = [
    &["Epic", "Issue", "Task"],
    &["Epic", "Feature", "User Story", "Task"],
    &["Epic", "Feature", "Product Backlog Item", "Task"],
    &["Epic", "Feature", "Requirement", "Task"],
];

/// The breakdown of one type into the type under it, in a process that breaks
/// work down this way. A type the chain says nothing about has no obvious
/// child, and neither has the last type in it.
#[must_use]
fn child_in(breakdown: &[&'static str], parent_type: &str) -> Option<&'static str> {
    breakdown
        .iter()
        .position(|name| *name == parent_type)
        .and_then(|at| breakdown.get(at + 1))
        .copied()
}

/// Whether one of the people already gathered is this one, so nobody is
/// offered twice under a different spelling.
#[must_use]
pub(super) fn names_someone_listed(candidates: &[AssigneeCandidate], name: &str) -> bool {
    candidates
        .iter()
        .any(|candidate| !candidate.unassigned && same_text(&candidate.display, name))
}

/// Whether every character typed appears in `haystack` in that order, ignoring
/// case: `jr` finds `Jacob Ragsdale`, and so does `ragsd`.
#[must_use]
pub(super) fn fuzzy_contains(haystack: &str, query: &str) -> bool {
    let mut remaining = haystack.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| remaining.any(|found| found == wanted))
}

impl WorkItemsScreen {
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
    pub fn merge_identities(&mut self, shell: &mut Shell, identities: Vec<Identity>) {
        if identities.is_empty() {
            return;
        }
        for identity in identities {
            match self
                .identities
                .iter_mut()
                .find(|known| same_text(&known.display_name, &identity.display_name))
            {
                Some(known) if known.unique_name.is_none() => {
                    known.unique_name = identity.unique_name;
                }
                Some(_) => {}
                None => self.identities.push(identity),
            }
        }
        if self.mode != WorkItemMode::AssigneePicker {
            return;
        }
        let focused = self
            .assignee_matches()
            .get(self.assignee_picker.cursor.index)
            .map(|candidate| candidate.display.clone());
        self.assignee_picker.candidates = self.assignee_candidates(shell);
        let matches = self.assignee_matches();
        let index = focused
            .and_then(|display| {
                matches
                    .iter()
                    .position(|candidate| candidate.display == display)
            })
            .unwrap_or(self.assignee_picker.cursor.index)
            .min(matches.len().saturating_sub(1));
        self.focus_assignee(index);
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

    /// `S`, and the Actions menu's State row: the states this work item's type
    /// allows, with the one it is in already under the cursor. The list is
    /// whatever is cached or already in the database, so this never waits.
    ///
    /// With two or more rows checked the picker moves all of them, and says so
    /// in its title. The states it offers are still the selected row's type's,
    /// which is the only type it could ask about; a state another checked work
    /// item's type does not allow is refused by Azure DevOps and named in the
    /// summary.
    pub(super) fn open_state_picker(&mut self, shell: &mut Shell) {
        let scope = self.edit_scope();
        let Some(ticket) = self.selected_ticket() else {
            shell.set_error("No work item is selected");
            return;
        };
        let current = ticket.state.clone();
        let work_item_type = ticket.work_item_type.clone();
        let options = self.states_for(&work_item_type);
        if options.is_empty() {
            shell.set_error(format!("No states are known for {work_item_type}"));
            return;
        }
        let index = options
            .iter()
            .position(|option| option.name == current)
            .unwrap_or_default();
        self.state_picker = StatePicker {
            options,
            cursor: ListCursor {
                index,
                scroll: ScrollState::default(),
            },
            current,
            scope,
        };
        self.state_picker.cursor.focus(index);
        self.mode = WorkItemMode::StatePicker;
    }

    pub(super) fn handle_state_picker_key(
        &mut self,
        shell: &mut Shell,
        key: KeyEvent,
    ) -> AppAction {
        let last = self.state_picker.options.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('S') => self.mode = WorkItemMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_state(self.state_picker.cursor.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_state((self.state_picker.cursor.index + 1).min(last));
            }
            KeyCode::PageUp => self.focus_state(self.state_picker.cursor.index.saturating_sub(5)),
            KeyCode::PageDown => self.focus_state((self.state_picker.cursor.index + 5).min(last)),
            KeyCode::Home => self.focus_state(0),
            KeyCode::End => self.focus_state(last),
            KeyCode::Enter => return self.choose_state(shell, self.state_picker.cursor.index),
            _ => {}
        }
        AppAction::None
    }

    fn focus_state(&mut self, index: usize) {
        self.state_picker.cursor.focus(index);
    }

    /// Confirms one state. Choosing the state the work item is already in
    /// closes the picker and writes nothing; anything else takes the ordinary
    /// write-through path, so the row changes at once and reverts if Azure
    /// DevOps refuses the transition. A picker opened over the checked rows
    /// moves every one of them, so the state the row under the cursor is in is
    /// a change to make there rather than a no-op.
    pub(super) fn choose_state(&mut self, shell: &mut Shell, index: usize) -> AppAction {
        let Some(option) = self.state_picker.options.get(index).cloned() else {
            self.mode = WorkItemMode::Browse;
            return AppAction::None;
        };
        self.mode = WorkItemMode::Browse;
        if !self.state_picker.scope.is_bulk() && option.name == self.state_picker.current {
            return AppAction::None;
        }
        self.edit_checked(shell, FieldEdit::state(&option.name))
    }

    /// The Actions menu's Priority row: 1 to 4 and a `Clear` row, with the
    /// priority the work item already has under the cursor.
    pub(super) fn open_priority_picker(&mut self, shell: &mut Shell) {
        let Some(ticket) = self.selected_ticket() else {
            shell.set_error("No work item is selected");
            return;
        };
        let current = ticket.priority;
        let id = ticket.key.id;
        let index = PRIORITY_CHOICES
            .iter()
            .position(|choice| *choice == current)
            .unwrap_or_default();
        self.priority_picker = PriorityPicker {
            cursor: ListCursor {
                index,
                scroll: ScrollState::default(),
            },
            current,
            id,
        };
        self.priority_picker.cursor.focus(index);
        self.mode = WorkItemMode::PriorityPicker;
    }

    pub(super) fn handle_priority_picker_key(
        &mut self,
        shell: &mut Shell,
        key: KeyEvent,
    ) -> AppAction {
        let last = PRIORITY_CHOICES.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => self.mode = WorkItemMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_priority(self.priority_picker.cursor.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_priority((self.priority_picker.cursor.index + 1).min(last));
            }
            KeyCode::Home => self.focus_priority(0),
            KeyCode::End => self.focus_priority(last),
            KeyCode::Enter => {
                return self.choose_priority(shell, self.priority_picker.cursor.index);
            }
            _ => {}
        }
        AppAction::None
    }

    fn focus_priority(&mut self, index: usize) {
        self.priority_picker.cursor.focus(index);
    }

    /// Confirms one priority. The priority the work item already has is a
    /// no-op, and `Clear` takes the field off it rather than writing an empty
    /// value, so the Pri cell empties.
    pub(super) fn choose_priority(&mut self, shell: &mut Shell, index: usize) -> AppAction {
        let Some(choice) = PRIORITY_CHOICES.get(index).copied() else {
            self.mode = WorkItemMode::Browse;
            return AppAction::None;
        };
        self.mode = WorkItemMode::Browse;
        if choice == self.priority_picker.current {
            return AppAction::None;
        }
        match choice {
            Some(priority) => self.edit_selected(shell, FieldEdit::priority(priority)),
            None => self.edit_selected(shell, FieldEdit::clear_priority()),
        }
    }

    /// Who the assignee picker offers, in the order it lists them: nobody, the
    /// signed-in user, everybody the database has ever seen a work item
    /// assigned to, and then the rest of the project's teams. Nobody appears
    /// twice, so a team member already holding work keeps their earlier place.
    #[must_use]
    fn assignee_candidates(&self, shell: &Shell) -> Vec<AssigneeCandidate> {
        let mut candidates = vec![AssigneeCandidate {
            display: UNASSIGNED_LABEL.to_owned(),
            unique: None,
            unassigned: true,
            me: false,
        }];
        if let Some(me) = shell
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
                .find(|identity| same_text(&identity.display_name, display))
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

    /// `a`, and the Actions menu's Assignee row: everybody worth offering, with
    /// whoever holds the work item under the cursor. The list is built from
    /// what is already in memory, so the picker opens at once; the project's
    /// teams are asked for the first time it is opened and merged in when they
    /// arrive. With two or more rows checked it reassigns all of them, and
    /// says so in its title.
    pub(super) fn open_assignee_picker(&mut self, shell: &mut Shell) -> AppAction {
        let scope = self.edit_scope();
        let Some(ticket) = self.selected_ticket() else {
            shell.set_error("No work item is selected");
            return AppAction::None;
        };
        let current = ticket.assigned_to.clone();
        self.show_assignee_picker(shell, current, scope)
    }

    /// The assignee picker itself, over whoever holds the work item now — or
    /// over the name a form field carries — and whatever it was opened for.
    /// Both the Actions menu and a form's Assignee field come through here, so
    /// the list, its cursor, and the one fetch a session are the same either
    /// way.
    pub(super) fn show_assignee_picker(
        &mut self,
        shell: &mut Shell,
        current: Option<String>,
        scope: EditScope,
    ) -> AppAction {
        let candidates = self.assignee_candidates(shell);
        let index = candidates
            .iter()
            .position(|candidate| candidate.is_current(current.as_deref()))
            .unwrap_or_default();
        self.assignee_picker = AssigneePicker {
            candidates,
            query: TextInput::default(),
            cursor: ListCursor {
                index,
                scroll: ScrollState::default(),
            },
            current,
            scope,
        };
        self.assignee_picker.cursor.focus(index);
        self.mode = WorkItemMode::AssigneePicker;
        if self.identities_requested {
            AppAction::None
        } else {
            self.identities_requested = true;
            AppAction::FetchIdentities
        }
    }

    pub(super) fn handle_assignee_picker_key(
        &mut self,
        shell: &mut Shell,
        key: KeyEvent,
    ) -> AppAction {
        match key.code {
            KeyCode::Esc => self.close_picker(self.assignee_picker.scope),
            KeyCode::Up => self.move_assignee_selection(-1),
            KeyCode::Down => self.move_assignee_selection(1),
            KeyCode::PageUp => self.move_assignee_selection(-5),
            KeyCode::PageDown => self.move_assignee_selection(5),
            KeyCode::Enter => {
                return self.choose_assignee(shell, self.assignee_picker.cursor.index);
            }
            // Everything else is typing: Home, End, and the editing keys all
            // belong to the filter field, the way they do in the palette.
            _ => {
                let before = self.assignee_picker.query.text().to_owned();
                self.assignee_picker.query.handle_key(key);
                if self.assignee_picker.query.text() != before {
                    self.assignee_picker.cursor.reset();
                }
            }
        }
        AppAction::None
    }

    fn move_assignee_selection(&mut self, delta: isize) {
        let count = self.assignee_matches().len();
        self.assignee_picker.cursor.move_by(delta, count);
    }

    fn focus_assignee(&mut self, index: usize) {
        self.assignee_picker.cursor.focus(index);
    }

    /// Confirms one candidate. Whoever holds the work item already is a no-op,
    /// and `Unassigned` takes the field off it rather than writing an empty
    /// identity, so the Assignee cell empties. A picker opened over the checked
    /// rows reassigns every one of them, so whoever holds the row under the
    /// cursor is a change to make to the rest rather than a no-op.
    pub(super) fn choose_assignee(&mut self, shell: &mut Shell, index: usize) -> AppAction {
        let scope = self.assignee_picker.scope;
        let Some(candidate) = self.assignee_matches().get(index).cloned() else {
            self.close_picker(scope);
            return AppAction::None;
        };
        if let EditScope::Form(field) = scope {
            // A form holds a name, not an edit, so `Unassigned` empties the
            // field rather than writing a clear anywhere.
            let name = if candidate.unassigned {
                String::new()
            } else {
                candidate.display.clone()
            };
            self.fill_form_field(field, name);
            return AppAction::None;
        }
        self.mode = WorkItemMode::Browse;
        if !self.assignee_picker.scope.is_bulk()
            && candidate.is_current(self.assignee_picker.current.as_deref())
        {
            return AppAction::None;
        }
        if candidate.unassigned {
            return self.edit_checked(shell, FieldEdit::unassign());
        }
        self.edit_checked(
            shell,
            FieldEdit::assignee(&candidate.display, candidate.unique.as_deref()),
        )
    }

    /// The parent one work item hangs under now, as the graph holds it. Azure
    /// DevOps allows a work item only one, so the first is the one.
    #[must_use]
    pub fn parent_of(&self, key: &TicketKey) -> Option<TicketKey> {
        self.graph.parents_of(key).into_iter().next()
    }

    /// Whether the work item under the cursor has a parent to take off, which
    /// is what puts `Remove parent` in the Actions menu.
    #[must_use]
    pub fn selected_has_parent(&self) -> bool {
        self.selected_ticket()
            .is_some_and(|ticket| self.parent_of(&ticket.key).is_some())
    }

    /// Every work item the selected one could be filed under: all of them, less
    /// itself and everything already below it.
    ///
    /// Leaving the descendants out is what makes a cycle impossible to ask for.
    /// It is not the only guard — the graph on screen can be behind the
    /// project, and Azure DevOps refuses a cycle it can see whatever the picker
    /// offered — but it is the one that keeps the refusal from ever being
    /// earned by an honest choice.
    #[must_use]
    pub fn parent_candidates(&self, child: &TicketKey) -> Vec<ParentCandidate> {
        let below: HashSet<TicketKey> = self.graph.descendants_of(child).into_iter().collect();
        self.tickets
            .iter()
            .filter(|ticket| ticket.key != *child && !below.contains(&ticket.key))
            .map(|ticket| ParentCandidate {
                key: ticket.key.clone(),
                work_item_type: ticket.work_item_type.clone(),
                title: ticket.title.clone(),
            })
            .collect()
    }

    /// The candidates whatever has been typed leaves showing, which is what the
    /// picker draws and what its cursor counts over. Both the id and the title
    /// match, so `613` and `dispatcher` each find the same work item.
    #[must_use]
    pub fn parent_matches(&self) -> Vec<ParentCandidate> {
        let query = self.parent_picker.query.text().trim().to_owned();
        self.parent_picker
            .candidates
            .iter()
            .filter(|candidate| {
                query.is_empty()
                    || fuzzy_contains(&candidate.title, &query)
                    || fuzzy_contains(&candidate.key.id.to_string(), &query)
            })
            .cloned()
            .collect()
    }

    /// The Actions menu's `Set parent…` row: every work item this one could hang
    /// under, with the one it hangs under now under the cursor. The list is
    /// built from the rows already loaded, so the picker opens at once.
    pub(super) fn open_parent_picker(&mut self, shell: &mut Shell) {
        let Some(child) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            shell.set_error("No work item is selected");
            return;
        };
        let candidates = self.parent_candidates(&child);
        if candidates.is_empty() {
            shell.set_error("No other work item is loaded to file this one under");
            return;
        }
        let current = self.parent_of(&child);
        let index = current
            .as_ref()
            .and_then(|parent| {
                candidates
                    .iter()
                    .position(|candidate| candidate.key == *parent)
            })
            .unwrap_or_default();
        self.parent_picker = ParentPicker {
            candidates,
            query: TextInput::default(),
            cursor: ListCursor {
                index,
                scroll: ScrollState::default(),
            },
            child,
            current,
        };
        self.parent_picker.cursor.focus(index);
        self.mode = WorkItemMode::ParentPicker;
    }

    pub(super) fn handle_parent_picker_key(
        &mut self,
        shell: &mut Shell,
        key: KeyEvent,
    ) -> AppAction {
        match key.code {
            KeyCode::Esc => self.mode = WorkItemMode::Browse,
            KeyCode::Up => self.move_parent_selection(-1),
            KeyCode::Down => self.move_parent_selection(1),
            KeyCode::PageUp => self.move_parent_selection(-5),
            KeyCode::PageDown => self.move_parent_selection(5),
            KeyCode::Enter => return self.choose_parent(shell, self.parent_picker.cursor.index),
            // Everything else is typing, the way it is in the assignee picker.
            _ => {
                let before = self.parent_picker.query.text().to_owned();
                self.parent_picker.query.handle_key(key);
                if self.parent_picker.query.text() != before {
                    self.parent_picker.cursor.reset();
                }
            }
        }
        AppAction::None
    }

    fn move_parent_selection(&mut self, delta: isize) {
        let count = self.parent_matches().len();
        self.parent_picker.cursor.move_by(delta, count);
    }

    /// `Enter` in the parent picker: the work item moves under whatever the
    /// cursor is on. Choosing the parent it already has writes nothing.
    pub(super) fn choose_parent(&mut self, shell: &mut Shell, index: usize) -> AppAction {
        let Some(candidate) = self.parent_matches().get(index).cloned() else {
            self.mode = WorkItemMode::Browse;
            return AppAction::None;
        };
        self.mode = WorkItemMode::Browse;
        if self.parent_picker.current.as_ref() == Some(&candidate.key) {
            return AppAction::None;
        }
        let child = self.parent_picker.child.clone();
        self.begin_reparent(shell, &child, Some(candidate.key))
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
        if self.mode != WorkItemMode::NodePicker {
            return;
        }
        let focused = self
            .node_matches()
            .get(self.node_picker.cursor.index)
            .map(|row| row.path.clone());
        let kind = self.node_picker.kind;
        let current = self.node_picker.current.clone();
        self.node_picker.rows = self.node_rows(kind);
        let matches = self.node_matches();
        let index = focused
            .and_then(|path| matches.iter().position(|row| row.path == path))
            .or_else(|| matches.iter().position(|row| row.path == current))
            .unwrap_or(self.node_picker.cursor.index)
            .min(matches.len().saturating_sub(1));
        self.focus_node(index);
    }

    /// The sprint `@current` files into and the forms default to: the first
    /// configured team's, as its own settings say, or — without a team — the
    /// deepest iteration in the project whose dates contain today in UTC.
    /// `None` when no iteration is scheduled around today, which includes
    /// every project whose trees have never been fetched.
    #[must_use]
    pub fn current_iteration(&self) -> Option<String> {
        self.current_iterations().into_iter().next()
    }

    /// Every sprint `@current` matches: one a configured team, or the one the
    /// calendar names without a team.
    #[must_use]
    pub fn current_iterations(&self) -> Vec<String> {
        if !self.team_iterations.is_empty() {
            return self.team_iterations.clone();
        }
        classification::current_iteration(&self.classification_nodes, Timestamp::now().date())
            .map(|node| node.path.clone())
            .into_iter()
            .collect()
    }

    /// The sprints the configured teams are in, read out of the database at
    /// startup; every pull after that carries them in its snapshot.
    pub fn set_team_iterations(&mut self, iterations: Vec<String>) {
        self.team_iterations = iterations;
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

    /// The Actions menu's Iteration and Area rows: the project's tree, indented,
    /// with the node the work item sits in already under the cursor. The rows
    /// come out of what is already in memory, so the picker opens at once; the
    /// trees are asked for the first time either picker is opened on a cache
    /// that is empty or over an hour old, and merged in when they arrive.
    ///
    /// Iteration is the one of the two worth making in bulk — a sprint ends
    /// and its leftovers move on together — so with two or more rows checked
    /// it moves all of them and says so in its title. Area stays on the row
    /// under the cursor.
    pub(super) fn open_node_picker(&mut self, shell: &mut Shell, kind: NodeKind) -> AppAction {
        let scope = match kind {
            NodeKind::Iteration => self.edit_scope(),
            NodeKind::Area => {
                EditScope::Ticket(self.selected_ticket().map_or(0, |ticket| ticket.key.id))
            }
        };
        let Some(ticket) = self.selected_ticket() else {
            shell.set_error("No work item is selected");
            return AppAction::None;
        };
        let current = match kind {
            NodeKind::Area => ticket.area_path.clone(),
            NodeKind::Iteration => ticket.iteration_path.clone(),
        };
        self.show_node_picker(kind, current, scope)
    }

    /// The node picker itself, over the path the work item carries now — or the
    /// one a form field carries — and whatever it was opened for. Both the Edit
    /// menu and a form's Iteration field come through here, so the tree, its
    /// cursor, and the once-a-session fetch behind it are the same either way.
    pub(super) fn show_node_picker(
        &mut self,
        kind: NodeKind,
        current: String,
        scope: EditScope,
    ) -> AppAction {
        let rows = self.node_rows(kind);
        let index = rows
            .iter()
            .position(|row| row.path == current)
            .unwrap_or_default();
        self.node_picker = NodePicker {
            kind,
            rows,
            query: TextInput::default(),
            cursor: ListCursor {
                index,
                scroll: ScrollState::default(),
            },
            current,
            scope,
        };
        self.node_picker.cursor.focus(index);
        self.mode = WorkItemMode::NodePicker;
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

    pub(super) fn handle_node_picker_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.close_picker(self.node_picker.scope),
            KeyCode::Up => self.move_node_selection(-1),
            KeyCode::Down => self.move_node_selection(1),
            KeyCode::PageUp => self.move_node_selection(-5),
            KeyCode::PageDown => self.move_node_selection(5),
            KeyCode::Enter => return self.choose_node(shell, self.node_picker.cursor.index),
            // Everything else is typing, the way it is in the assignee picker.
            _ => {
                let before = self.node_picker.query.text().to_owned();
                self.node_picker.query.handle_key(key);
                if self.node_picker.query.text() != before {
                    self.node_picker.cursor.reset();
                }
            }
        }
        AppAction::None
    }

    fn move_node_selection(&mut self, delta: isize) {
        let count = self.node_matches().len();
        self.node_picker.cursor.move_by(delta, count);
    }

    fn focus_node(&mut self, index: usize) {
        self.node_picker.cursor.focus(index);
    }

    /// Confirms one node. The node the work item already sits in is a no-op;
    /// anything else writes the full backslash path to `System.IterationPath`
    /// or `System.AreaPath`, and the table column goes on showing the leaf. An
    /// iteration picker opened over the checked rows moves every one of them,
    /// so the sprint the row under the cursor is in is a change to make to the
    /// rest rather than a no-op.
    pub(super) fn choose_node(&mut self, shell: &mut Shell, index: usize) -> AppAction {
        let scope = self.node_picker.scope;
        let Some(row) = self.node_matches().get(index).cloned() else {
            self.close_picker(scope);
            return AppAction::None;
        };
        if let EditScope::Form(field) = scope {
            self.fill_form_field(field, row.path);
            return AppAction::None;
        }
        self.mode = WorkItemMode::Browse;
        if !self.node_picker.scope.is_bulk() && row.path == self.node_picker.current {
            return AppAction::None;
        }
        match self.node_picker.kind {
            NodeKind::Iteration => self.edit_checked(shell, FieldEdit::iteration(&row.path)),
            NodeKind::Area => self.edit_selected(shell, FieldEdit::area(&row.path)),
        }
    }

    /// The work item types the project's process offers, as the database
    /// cached them.
    #[must_use]
    pub fn work_item_types(&self) -> &[String] {
        &self.work_item_types
    }

    /// The types read out of the database at startup, so the first form of a
    /// run opens over the list the last one fetched.
    pub fn set_work_item_types(&mut self, types: Vec<String>) {
        self.work_item_types = types;
    }

    /// The types a fetch brought back. An empty answer changes nothing: the
    /// endpoint could not be read, and the cached list is better than none. An
    /// open picker is rebuilt around them, keeping the row under the cursor.
    pub fn merge_work_item_types(&mut self, types: Vec<String>) {
        if types.is_empty() {
            return;
        }
        self.work_item_types = types;
        if self.mode != WorkItemMode::TypePicker {
            return;
        }
        let focused = self
            .type_picker
            .options
            .get(self.type_picker.cursor.index)
            .cloned();
        self.type_picker.options = self.work_item_type_options();
        let index = focused
            .and_then(|name| self.type_picker.options.iter().position(|it| *it == name))
            .or_else(|| {
                self.type_picker
                    .options
                    .iter()
                    .position(|name| *name == self.type_picker.current)
            })
            .unwrap_or(self.type_picker.cursor.index)
            .min(self.type_picker.options.len().saturating_sub(1));
        self.focus_type(index);
    }

    /// The types a form's Type field offers: the cached process list when there
    /// is one, and otherwise every type the rows already carry, which is enough
    /// to file work alongside what is already there. The default is always
    /// among them, so a form on an empty database still has something to file.
    #[must_use]
    fn work_item_type_options(&self) -> Vec<String> {
        let mut options = self.work_item_types.clone();
        if options.is_empty() {
            for ticket in self.tickets.iter() {
                if !options.contains(&ticket.work_item_type) {
                    options.push(ticket.work_item_type.clone());
                }
            }
            options.sort();
        }
        if !options.iter().any(|name| name == DEFAULT_WORK_ITEM_TYPE) {
            options.insert(0, DEFAULT_WORK_ITEM_TYPE.to_owned());
        }
        options
    }

    /// Which of the stock breakdowns this project works to: the one naming the
    /// most of the types it offers. An Agile project answers to four of its own
    /// chain's names and to only three of Basic's, though it does have an
    /// Issue; a Basic one is the other way round. A tie goes to the first
    /// chain, so a project whose types have not been read yet, or whose process
    /// shares no name with any of them, is taken to work the everyday
    /// Epic/Issue/Task way.
    #[must_use]
    fn work_item_breakdown(&self) -> &'static [&'static str] {
        let mut best = WORK_ITEM_BREAKDOWNS[0];
        let mut matched = 0;
        for breakdown in WORK_ITEM_BREAKDOWNS {
            let count = breakdown
                .iter()
                .filter(|name| self.lists_work_item_type(name))
                .count();
            if count > matched {
                (best, matched) = (breakdown, count);
            }
        }
        best
    }

    /// Whether the project's own list of types names this one.
    #[must_use]
    fn lists_work_item_type(&self, name: &str) -> bool {
        self.work_item_types.iter().any(|known| known == name)
    }

    /// What breaking one work item down produces, as the process this project
    /// works to breaks work down: an Epic into Issues and an Issue into Tasks
    /// under Basic, an Epic into Features and a User Story into Tasks under
    /// Agile. A type with nothing under it keeps its own, and so does one whose
    /// child the project's own list does not offer, because a child of the same
    /// type is always defensible and an empty Type field never is.
    #[must_use]
    pub(super) fn child_work_item_type(&self, parent_type: &str) -> String {
        child_in(self.work_item_breakdown(), parent_type)
            .filter(|child| self.work_item_types.is_empty() || self.lists_work_item_type(child))
            .map_or_else(|| parent_type.to_owned(), ToOwned::to_owned)
    }

    /// The work item type picker, over the type the form names now.
    pub(super) fn open_type_picker(&mut self, field: FormFieldId, current: String) {
        let options = self.work_item_type_options();
        let index = options
            .iter()
            .position(|name| *name == current)
            .unwrap_or_default();
        self.type_picker = TypePicker {
            options,
            cursor: ListCursor {
                index,
                scroll: ScrollState::default(),
            },
            current,
            field,
        };
        self.type_picker.cursor.focus(index);
        self.mode = WorkItemMode::TypePicker;
    }

    pub(super) fn handle_type_picker_key(&mut self, key: KeyEvent) -> AppAction {
        let last = self.type_picker.options.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => self.close_picker(EditScope::Form(self.type_picker.field)),
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_type(self.type_picker.cursor.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_type((self.type_picker.cursor.index + 1).min(last));
            }
            KeyCode::PageUp => self.focus_type(self.type_picker.cursor.index.saturating_sub(5)),
            KeyCode::PageDown => self.focus_type((self.type_picker.cursor.index + 5).min(last)),
            KeyCode::Home => self.focus_type(0),
            KeyCode::End => self.focus_type(last),
            KeyCode::Enter => self.choose_work_item_type(self.type_picker.cursor.index),
            _ => {}
        }
        AppAction::None
    }

    fn focus_type(&mut self, index: usize) {
        self.type_picker.cursor.focus(index);
    }

    /// Confirms one type, which writes it back into the form field that opened
    /// the picker. Nothing is sent anywhere: a form is not a work item yet.
    pub(super) fn choose_work_item_type(&mut self, index: usize) {
        let field = self.type_picker.field;
        let Some(name) = self.type_picker.options.get(index).cloned() else {
            self.close_picker(EditScope::Form(field));
            return;
        };
        self.fill_form_field(field, name);
    }
}
