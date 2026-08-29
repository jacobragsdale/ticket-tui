//! The family of the selected work item: its tree, its cursor, and the
//! child progress every row carries.

use super::*;

/// How far a work item's direct children have got: how many are finished, and
/// how many there are.
///
/// Grandchildren are deliberately left out. A parent's progress is the work it
/// asked for directly, so an Epic reads over its Features rather than over
/// every Task underneath them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChildProgress {
    pub done: usize,
    pub total: usize,
}

impl ChildProgress {
    /// Whether every child is off the board, which is what makes an Epic read
    /// as finished without anybody counting its children.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.total > 0 && self.done >= self.total
    }

    /// The ratio as all three places write it: `3/7`.
    #[must_use]
    pub fn ratio(self) -> String {
        format!("{}/{}", self.done, self.total)
    }

    /// How many cells of a bar `width` wide are filled. Rounding never lies at
    /// either end: any progress at all fills one cell, and only a whole ratio
    /// fills the last one.
    #[must_use]
    pub const fn filled_cells(self, width: usize) -> usize {
        if width == 0 || self.total == 0 || self.done == 0 {
            return 0;
        }
        if self.done >= self.total {
            return width;
        }
        let scaled = self.done * width / self.total;
        if scaled == 0 {
            1
        } else if scaled >= width {
            width - 1
        } else {
            scaled
        }
    }
}

/// Done out of total over direct children, for every work item that has any.
///
/// Built in one pass over the relations and the states beside them, so drawing
/// forty rows costs forty hash lookups rather than forty walks of the graph. A
/// work item with no children is simply absent, which is what lets the table,
/// the family tree, and the details pane all show nothing at all for it.
#[derive(Clone, Debug, Default)]
pub struct ChildProgressIndex {
    by_parent: HashMap<TicketKey, ChildProgress>,
}

impl ChildProgressIndex {
    #[must_use]
    pub fn build(tickets: &[Ticket], graph: &TicketGraph) -> Self {
        let categories: HashMap<&TicketKey, StateCategory> = tickets
            .iter()
            .map(|ticket| (&ticket.key, StateCategory::of(&ticket.state)))
            .collect();
        // A child reached both by its parent's child link and by its own
        // parent link is still one child, so the pairs are deduplicated the
        // way `TicketGraph::children_of` does before anything is counted.
        let mut children: HashMap<&TicketKey, HashSet<&TicketKey>> = HashMap::new();
        for relation in &graph.relations {
            let (parent, child) = match relation.kind {
                RelationKind::Child => (&relation.from, &relation.to),
                RelationKind::Parent => (&relation.to, &relation.from),
                _ => continue,
            };
            if parent == child {
                continue;
            }
            children.entry(parent).or_default().insert(child);
        }
        let by_parent = children
            .into_iter()
            .map(|(parent, children)| {
                // A child the loaded set does not hold still counts against
                // the total: it is work that was asked for and is not known to
                // be finished.
                let done = children
                    .iter()
                    .filter(|child| {
                        categories
                            .get(*child)
                            .copied()
                            .is_some_and(StateCategory::is_done)
                    })
                    .count();
                (
                    parent.clone(),
                    ChildProgress {
                        done,
                        total: children.len(),
                    },
                )
            })
            .collect();
        Self { by_parent }
    }

    /// What one work item's children add up to, or nothing at all when it has
    /// none.
    #[must_use]
    pub fn of(&self, key: &TicketKey) -> Option<ChildProgress> {
        self.by_parent.get(key).copied()
    }

    /// Orders two work items by how far along they are, with the ones that
    /// have no children last however the sort runs — the same place an empty
    /// priority takes.
    pub(super) fn compare(
        &self,
        left: &TicketKey,
        right: &TicketKey,
        direction: SortDirection,
    ) -> Ordering {
        match (self.of(left), self.of(right)) {
            (Some(left), Some(right)) => {
                // Cross-multiplied rather than divided, so 1/2 and 2/4 tie
                // exactly and no ratio rounds its way past another.
                let ordering = (left.done * right.total).cmp(&(right.done * left.total));
                match direction {
                    SortDirection::Ascending => ordering,
                    SortDirection::Descending => ordering.reverse(),
                }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }
}

impl WorkItemsScreen {
    #[must_use]
    /// Settles on one of this screen's rows, if it holds it. A work item in the
    /// family tree of the row already selected moves the family cursor rather
    /// than the table, which is what clicking a tree row has always done.
    pub fn select_jump(&mut self, shell: &mut Shell, jump: &Jump) -> bool {
        match jump {
            Jump::WorkItem(key) => {
                if self.ticket_by_key(key).is_none() {
                    return false;
                }
                if self
                    .visible_family_tree()
                    .iter()
                    .any(|entry| &entry.key == key)
                {
                    shell.focus = Focus::Family;
                    self.family_cursor = Some(key.clone());
                    self.ensure_family_cursor_visible();
                } else if self
                    .selected_family()
                    .is_some_and(|family| family.extra_parents.iter().any(|parent| parent == key))
                {
                    shell.focus = Focus::Family;
                } else {
                    shell.focus = Focus::Details;
                }
                self.jump_to_ticket(shell, key);
                true
            }
            Jump::WorkItems(ids) => {
                if ids.is_empty() {
                    return false;
                }
                let query = ids
                    .iter()
                    .map(|id| format!("id:{id}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                self.set_query(shell, query);
                true
            }
            _ => false,
        }
    }

    pub fn ticket_by_key(&self, key: &TicketKey) -> Option<&Ticket> {
        self.tickets.iter().find(|ticket| ticket.key == *key)
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

    pub fn replace_tickets(&mut self, shell: &mut Shell, tickets: Vec<Ticket>) {
        self.replace_prepared_tickets(shell, Snapshot::new(tickets));
    }

    pub fn replace_prepared_tickets(&mut self, shell: &mut Shell, prepared: Snapshot) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        if !prepared.repos.is_empty() {
            shell.set_repos(prepared.repos.clone());
        }
        self.tickets = Arc::new(prepared.tickets);
        self.graph = prepared.graph;
        // A pull that has not cached the states yet must not throw away the
        // ones an earlier pull did.
        if !prepared.states.is_empty() {
            self.state_catalog = prepared.states;
        }
        self.search.replace_documents(prepared.search_documents);
        self.reapply_pending_edits();
        self.refresh_child_progress();
        shell.loaded_at = Instant::now();
        shell.stale = false;
        if self.fuzzy_query().is_empty() {
            self.show_all(shell, selected.as_ref());
        } else {
            self.pending_selection = selected;
            self.visible.clear();
            self.table_state.select(None);
            self.submit_search();
        }
    }

    pub(super) fn index_of(&self, key: &TicketKey) -> Option<usize> {
        self.tickets.iter().position(|ticket| ticket.key == *key)
    }

    /// Replaces one work item in place, keeping its search document and its
    /// parents' child counts in step so the next query and the next frame both
    /// see the new value.
    pub(super) fn set_ticket(&mut self, index: usize, ticket: Ticket) {
        Arc::make_mut(&mut self.tickets)[index] = ticket;
        self.search.update_document(index, &self.tickets[index]);
        self.refresh_child_progress();
    }

    /// Counts each parent's children again. Called wherever the rows or the
    /// relations move — a reload, a workspace graph, an edit settling — which
    /// is what keeps an Epic's ratio right as its issues close without any
    /// frame paying for the count.
    pub(super) fn refresh_child_progress(&mut self) {
        self.child_progress = ChildProgressIndex::build(&self.tickets, &self.graph);
    }

    /// How far one work item's direct children have got, or nothing at all
    /// when it has none.
    #[must_use]
    pub fn child_progress(&self, key: &TicketKey) -> Option<ChildProgress> {
        self.child_progress.of(key)
    }

    /// Re-applies the filters and the sort to the rows already on screen, for
    /// when one of them changed under the current ordering. The selection
    /// follows its work item rather than its row number.
    ///
    /// A change that takes the selected work item off the table — marking it
    /// Done while finished work is hidden — leaves the cursor where it was,
    /// which is the row that has moved up into its place. The cursor lands on
    /// the next piece of work rather than on nothing or back at the top, so
    /// marking a run of items Done reads as working down the list.
    pub(super) fn resettle_rows(&mut self, shell: &mut Shell) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        let row = self.table_state.selected();
        self.apply_filters(shell);
        self.sort_visible();
        if let Some(row) = row
            && selected
                .as_ref()
                .is_some_and(|key| self.visible_row(key).is_none())
        {
            self.select_row(shell, row);
            return;
        }
        self.restore_selection(shell, selected.as_ref());
    }

    pub(super) fn sync_family_state(&mut self, shell: &mut Shell) {
        self.reset_family_cursor();
        if shell.focus == Focus::Family && !self.selected_has_family() {
            shell.focus = Focus::Details;
        }
    }

    fn reset_family_cursor(&mut self) {
        self.family_cursor = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.clamp_family_cursor();
    }

    pub(super) fn family_page_size(&self) -> isize {
        let visible = self.visible_family_tree().len().max(1);
        let viewport = self.details.viewport.max(1);
        isize::try_from(viewport.min(visible)).unwrap_or(1)
    }

    pub(super) fn move_family_cursor(&mut self, delta: isize) {
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

    pub(super) fn move_family_cursor_to_edge(&mut self, last: bool) {
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

    pub(super) fn ensure_family_cursor_visible(&mut self) {
        let Some(cursor) = self.family_cursor.clone() else {
            return;
        };
        let tree = self.visible_family_tree();
        let Some(index) = tree.iter().position(|entry| entry.key == cursor) else {
            return;
        };
        // The tree sits below a heading that scrolls with it, so the row it
        // was last drawn on is where the cursor has to be kept.
        let line = self.details_family_row.saturating_add(index);
        let viewport = self.details.viewport.max(1);
        if line < self.details.offset {
            self.details.offset = line;
        } else if line >= self.details.offset.saturating_add(viewport) {
            self.details.offset = line
                .saturating_add(1)
                .saturating_sub(viewport)
                .min(self.details.max_offset());
        }
    }
}
