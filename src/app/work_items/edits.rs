//! Writing to Azure DevOps: field edits, undo, bulk changes, comments,
//! reparenting and deletion.

use super::*;

/// The Actions menu's cursor. The entries themselves are [`EDIT_MENU`].
#[derive(Clone, Debug, Default)]
pub struct EditMenu {
    pub index: usize,
    pub scroll: ScrollState,
}

/// What an editor is about to change: the one work item under the cursor, or
/// every checked row at once. Two or more checked rows make a bulk change of
/// the state picker, the assignee picker, and the iteration tree — the edits
/// sprint hygiene means making ten at a time. Every other editor stays on the
/// row under the cursor, because the same title or the same description on ten
/// work items is never what was meant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditScope {
    /// One work item, named by its id.
    Ticket(i64),
    /// Every checked row, counted.
    Checked(usize),
    /// One field of an open form, which is not a work item at all: the choice
    /// is written back into the form and nothing is sent anywhere.
    Form(FormFieldId),
}

impl Default for EditScope {
    fn default() -> Self {
        Self::Ticket(0)
    }
}

impl EditScope {
    /// What an overlay title calls the scope, so a bulk change is never made
    /// by accident: `#613`, or `5 tickets`.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Ticket(id) => format!("#{id}"),
            Self::Checked(count) => format!("{count} tickets"),
            Self::Form(_) => "the form".to_owned(),
        }
    }

    /// Whether the editor acts on the checked rows rather than on the one
    /// under the cursor. The value the row under the cursor already carries is
    /// only a no-op for the second: the others may well be somewhere else.
    #[must_use]
    pub const fn is_bulk(self) -> bool {
        matches!(self, Self::Checked(_))
    }
}

/// Which field a [`TextPrompt`] is editing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptField {
    Title,
    Tags,
    /// A new comment on the work item, which starts empty rather than
    /// prefilled: there is nothing to edit, only something to say.
    Comment,
}

impl PromptField {
    /// What the prompt calls the field, in its title and its notifications.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Tags => "Tags",
            Self::Comment => "Comment",
        }
    }

    /// What the prompt's frame says, which always names the work item it is
    /// for. A comment is left on a work item rather than in a field of it, so
    /// it reads that way.
    #[must_use]
    pub fn title(self, id: i64) -> String {
        match self {
            Self::Comment => format!("Comment on #{id}"),
            other => format!("{} \u{b7} #{id}", other.label()),
        }
    }

    /// What the footer says while the prompt is open.
    #[must_use]
    pub const fn hint(self) -> &'static str {
        match self {
            Self::Title => "Type a title  Enter save  Esc cancel",
            Self::Tags => "Semicolon separated  Enter save  Esc cancel",
            Self::Comment => "Type a comment  Enter post  Esc cancel",
        }
    }
}

/// A single-line field editor, prefilled with what the work item says now. The
/// Title and Tags rows of the Actions menu both open one.
#[derive(Clone, Debug)]
pub struct TextPrompt {
    pub field: PromptField,
    pub input: TextInput,
    /// The work item the prompt was opened for, shown in its title.
    pub id: i64,
    /// The text the prompt opened with; saving that back writes nothing.
    pub original: String,
}

/// The confirmation a delete asks for: what is about to go, and what it leaves
/// behind. It is built when the overlay opens and read straight by the
/// renderer, so nothing about it is worked out again a frame.
///
/// The child count is the point of the whole overlay. A delete takes the one
/// work item and nothing under it, so an Epic with eight issues leaves eight
/// issues hanging under nothing — which is the moment somebody wants to be
/// told, before the delete rather than after it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeleteConfirm {
    /// The work items the confirm covers, in the order the table holds them.
    pub keys: Vec<TicketKey>,
    /// The id and title of the one work item, for a delete of a single row.
    /// `None` for a checked set, which is counted rather than named.
    pub subject: Option<(i64, String)>,
    /// The direct children the work items have between them, which the delete
    /// leaves behind rather than taking with it.
    pub children: usize,
}

impl DeleteConfirm {
    /// What the overlay asks, in a line: the work item by id and title, or the
    /// checked rows by count.
    #[must_use]
    pub fn question(&self) -> String {
        match &self.subject {
            Some((id, title)) => format!("Delete #{id} {title}?"),
            None => format!("Delete {} tickets?", self.keys.len()),
        }
    }

    /// What the delete leaves behind, or nothing at all when it leaves nothing.
    /// The wording is deliberately about the children rather than about the
    /// delete: what is at stake is the work under the row, not the row.
    #[must_use]
    pub fn orphans(&self) -> Option<String> {
        if self.children == 0 {
            return None;
        }
        let whose = if self.subject.is_some() {
            "Its"
        } else {
            "Their"
        };
        let (children, verb) = if self.children == 1 {
            ("1 child".to_owned(), "is")
        } else {
            (format!("{} children", self.children), "are")
        };
        Some(format!(
            "{whose} {children} {verb} not deleted \u{2014} left with no parent."
        ))
    }
}

/// An edit waiting on Azure DevOps. `original` is the row as it was before the
/// change, restored if the write is refused; applying `edit` to it gives back
/// the optimistic copy the table is showing, which is how a pull that lands
/// first is topped up again.
#[derive(Clone, Debug)]
pub(super) struct PendingEdit {
    pub(super) original: Ticket,
    pub(super) edit: FieldEdit,
    /// When the edit was dispatched, so the agent context can say how long it
    /// has been in flight.
    pub(super) since: Timestamp,
    /// What this edit means for the undo stack once it lands.
    pub(super) undo: UndoRole,
}

impl PendingEdit {
    fn optimistic(&self) -> Ticket {
        let mut ticket = self.original.clone();
        self.edit.apply(&mut ticket);
        ticket
    }
}

/// What an edit in flight is to the undo stack.
#[derive(Clone, Debug)]
pub(super) enum UndoRole {
    /// An ordinary edit, filed under the dispatch that made it once it lands.
    /// The work items of one bulk change share a number, so they gather into
    /// a single entry and one `u` takes the whole change back.
    Undoable(u64),
    /// An edit that is itself an undo, which is not filed: taking one back
    /// would only put the change on again, and the edit under it on the stack
    /// would never be reached. The line is what the status says when this
    /// lands, and is `None` for one work item of an undo whose summary speaks
    /// for the whole of it.
    Undoing(Option<String>),
}

/// One work item on the undo stack: the change that puts its field back the
/// way it was before the edit that is being taken back.
#[derive(Clone, Debug)]
pub(super) struct UndoStep {
    key: TicketKey,
    edit: FieldEdit,
}

/// Everything one press of `u` takes back. An ordinary edit is one work item;
/// a bulk change over the checked rows is all of them under a single entry, so
/// `u` puts the whole change back rather than unpicking it a row at a time.
#[derive(Clone, Debug)]
pub(super) struct UndoEntry {
    /// The dispatch these came from, so a bulk change's work items gather here
    /// as their answers arrive rather than stacking up one entry apiece.
    group: u64,
    /// What the field is called, such as `State`.
    label: String,
    /// The value the edit wrote, which is the half of the story the work items
    /// share however different the values they are going back to are.
    wrote: String,
    steps: Vec<UndoStep>,
}

impl UndoEntry {
    /// What the status line says once the undo has landed. One work item names
    /// the value both ways — `Undid State on #613 (Doing → To Do)`; a bulk
    /// change put several different values back, so it counts them instead.
    fn headline(&self) -> String {
        match self.steps.as_slice() {
            [step] => format!(
                "Undid {} on #{} ({} → {})",
                self.label,
                step.key.id,
                self.wrote,
                step.edit.value_text()
            ),
            steps => format!("Undid {} on {} tickets", self.label, steps.len()),
        }
    }
}

/// The Azure DevOps project a run pulls from, as the agent context reports it.
/// The database overlay says the same thing in prose; this is the machine
/// readable half.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncTarget {
    pub organization: String,
    pub project: String,
    /// Seconds between timer pulls, `0` when `--refresh 0` left the sync key
    /// as the only thing that pulls.
    pub refresh_seconds: u64,
}

/// How a change of several work items reads once the last answer is in. A
/// bulk change counts what landed; an undo of one says what it took back
/// instead, which the count would only get in the way of.
#[derive(Clone, Debug)]
pub(super) enum BulkHeadline {
    /// The change as a notification says it, such as `State → Doing`, with the
    /// tally put in front of it.
    Changed(String),
    /// The whole line an undo says for itself.
    Undone(String),
    /// A set of work items sent to the recycle bin, which has nothing to say
    /// for itself beyond how many went.
    Deleted,
}

impl BulkHeadline {
    /// The line for a change every work item took.
    fn all_landed(&self, updated: usize) -> String {
        match self {
            Self::Changed(summary) => format!("Updated {updated} tickets · {summary}"),
            Self::Undone(line) => line.clone(),
            Self::Deleted => format!("Deleted {updated} tickets"),
        }
    }

    /// What the tally of a change that did not land everywhere leads with.
    const fn verb(&self) -> &'static str {
        match self {
            Self::Changed(_) => "Updated",
            Self::Undone(_) => "Undid",
            Self::Deleted => "Deleted",
        }
    }
}

/// One change asked of several work items at once, and what has come back of
/// it. Each edit is its own request with its own revision test, so they land
/// one at a time; this counts the answers so the change speaks once, when the
/// last of them is in, rather than once a row. An undo of a bulk change is
/// gathered the same way, so it too is never left half done in silence.
#[derive(Clone, Debug)]
pub(super) struct BulkEdit {
    /// What the whole change says for itself once every answer is in.
    headline: BulkHeadline,
    /// How many work items it was asked of, answered or not.
    total: usize,
    /// How many of them Azure DevOps accepted.
    updated: usize,
    /// What went wrong, one line a work item, in the order they answered.
    failures: Vec<String>,
    /// The work items still waiting on an answer.
    outstanding: HashSet<TicketKey>,
}

impl BulkEdit {
    /// Files one answer, and says whether that was the last one outstanding.
    fn record(&mut self, key: &TicketKey, failure: Option<String>) -> bool {
        self.outstanding.remove(key);
        match failure {
            Some(failure) => self.failures.push(failure),
            None => self.updated += 1,
        }
        self.outstanding.is_empty()
    }

    /// Whether anything did not land, which is what makes the summary an error
    /// rather than a status.
    fn failed(&self) -> bool {
        !self.failures.is_empty()
    }

    /// The one notification the whole change leaves behind: how many landed,
    /// and which work items did not.
    fn notification(&self) -> String {
        if self.failures.is_empty() {
            return self.headline.all_landed(self.updated);
        }
        let mut named: Vec<String> = self
            .failures
            .iter()
            .take(NAMED_BULK_FAILURES)
            .cloned()
            .collect();
        let unnamed = self.failures.len() - named.len();
        if unnamed > 0 {
            named.push(format!("+{unnamed} more"));
        }
        format!(
            "{} {} of {} · {}",
            self.headline.verb(),
            self.updated,
            self.total,
            named.join(" · ")
        )
    }
}

impl WorkItemsScreen {
    /// Asks for one field of the selected work item to be written back to
    /// Azure DevOps. The row carries the change at once, so the table never
    /// waits for the network; the action this returns is what actually sends
    /// it, and a refusal puts the row back. Every edit feature goes this way.
    pub fn edit_selected(&mut self, shell: &mut Shell, edit: FieldEdit) -> AppAction {
        let Some(key) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            shell.set_error("No work item is selected");
            return AppAction::None;
        };
        self.edit_ticket(shell, &key, edit)
    }

    /// [`Self::edit_selected`] for a work item that is not the selected row.
    pub fn edit_ticket(
        &mut self,
        shell: &mut Shell,
        key: &TicketKey,
        edit: FieldEdit,
    ) -> AppAction {
        let label = edit.label().to_owned();
        let undo = UndoRole::Undoable(self.next_undo_group());
        match self.begin_edit(shell, key, edit, undo) {
            Ok(request) => AppAction::Edit(vec![request]),
            Err(reason) => {
                shell.set_error(format!("#{} {label} not saved: {reason}", key.id));
                AppAction::None
            }
        }
    }

    /// [`Self::edit_selected`] for every checked row, which is what the state
    /// picker, the assignee picker, and the iteration tree do when two or more
    /// rows are checked. Each work item gets its own request, its own revision
    /// test, and its own optimistic row; a refusal reverts only the row it
    /// names, and the answers are gathered into one summary rather than a
    /// notification apiece. Anything less than two checked rows is not a bulk
    /// change at all and goes the ordinary way.
    pub fn edit_checked(&mut self, shell: &mut Shell, edit: FieldEdit) -> AppAction {
        let targets = self.checked_keys();
        if targets.len() < 2 {
            return self.edit_selected(shell, edit);
        }
        let mut requests = Vec::new();
        let mut failures = Vec::new();
        // One number for the whole change, so `u` puts every row of it back at
        // once rather than a row a press.
        let group = self.next_undo_group();
        for key in targets {
            // The picker's no-op rule, applied a row at a time: a work item
            // already carrying the value is left alone rather than written to.
            if !self.would_change(&key, &edit) {
                continue;
            }
            match self.begin_edit(shell, &key, edit.clone(), UndoRole::Undoable(group)) {
                Ok(request) => requests.push(request),
                Err(reason) => failures.push(format!("#{} failed: {reason}", key.id)),
            }
        }
        let total = requests.len() + failures.len();
        if total == 0 {
            shell.set_status(format!("Nothing to change · {}", edit.summary()));
            return AppAction::None;
        }
        let bulk = BulkEdit {
            headline: BulkHeadline::Changed(edit.summary()),
            total,
            updated: 0,
            failures,
            outstanding: requests.iter().map(|request| request.key.clone()).collect(),
        };
        if bulk.outstanding.is_empty() {
            // Nothing could even be asked, so the whole change is already told.
            shell.set_error(bulk.notification());
            return AppAction::None;
        }
        self.bulk_edits.push(bulk);
        AppAction::Edit(requests)
    }

    /// Starts one edit: the row takes the change at once, the copy it had is
    /// kept for a refusal, and the request to send comes back. `Err` is why
    /// the work item cannot be written, phrased to follow `not saved:`.
    fn begin_edit(
        &mut self,
        shell: &mut Shell,
        key: &TicketKey,
        edit: FieldEdit,
        undo: UndoRole,
    ) -> Result<EditRequest, String> {
        if let Some(reason) = shell.write_refusal() {
            // Nothing to write to, so the row is left exactly as it is.
            return Err(reason);
        }
        if self.pending_edits.contains_key(key) {
            // The revision a second edit would test with is already stale, so
            // it could only earn a conflict.
            return Err("an earlier edit is still in flight".to_owned());
        }
        let Some(index) = self.index_of(key) else {
            return Err("it is not in this database".to_owned());
        };
        let pending = PendingEdit {
            original: self.tickets[index].clone(),
            edit: edit.clone(),
            since: Timestamp::now(),
            undo,
        };
        let request = EditRequest {
            key: key.clone(),
            expected_revision: pending.original.revision,
            edit,
        };
        self.set_ticket(index, pending.optimistic());
        self.pending_edits.insert(key.clone(), pending);
        Ok(request)
    }

    /// The checked rows, in the order the table holds them, so a bulk change
    /// goes out the way it reads on screen.
    fn checked_keys(&self) -> Vec<TicketKey> {
        if self.selected_keys.is_empty() {
            return Vec::new();
        }
        self.tickets
            .iter()
            .filter(|ticket| self.selected_keys.contains(&ticket.key))
            .map(|ticket| ticket.key.clone())
            .collect()
    }

    /// Whether an edit would leave a work item any different from how it reads
    /// now, which is how a bulk change knows what it has nothing to do to.
    fn would_change(&self, key: &TicketKey, edit: &FieldEdit) -> bool {
        self.index_of(key).is_some_and(|index| {
            let mut changed = self.tickets[index].clone();
            edit.apply(&mut changed);
            changed != self.tickets[index]
        })
    }

    /// What an editor that can act on several rows is about to change, which
    /// is what its title says: every checked row when two or more are checked,
    /// and the row under the cursor otherwise.
    #[must_use]
    pub(super) fn edit_scope(&self) -> EditScope {
        let checked = self.checked_keys().len();
        if checked >= 2 {
            EditScope::Checked(checked)
        } else {
            EditScope::Ticket(self.selected_ticket().map_or(0, |ticket| ticket.key.id))
        }
    }

    /// Whether an edit is waiting on Azure DevOps. The database watcher stands
    /// down while one is, because the sync worker is writing that row itself.
    #[must_use]
    pub fn edits_pending(&self) -> bool {
        !self.pending_edits.is_empty()
    }

    /// Swaps in the copy Azure DevOps stored, so the row shows the revision and
    /// changed date the server settled on rather than the optimistic guess.
    pub fn apply_edit(&mut self, shell: &mut Shell, applied: EditApplied) {
        let key = applied.ticket.key.clone();
        let pending = self.pending_edits.remove(&key);
        self.graph.replace_relations_from(&key, applied.relations);
        if let Some(index) = self.index_of(&key) {
            self.set_ticket(index, applied.ticket);
            self.resettle_rows(shell);
        }
        let mut landed = format!("Updated #{} · {}", key.id, applied.edit.summary());
        if let Some(PendingEdit { original, undo, .. }) = pending {
            match undo {
                UndoRole::Undoable(group) => self.record_undo(group, &original, &applied.edit),
                UndoRole::Undoing(Some(line)) => landed = line,
                UndoRole::Undoing(None) => {}
            }
        }
        shell.flash_row(key.clone());
        if !self.record_bulk_outcome(shell, &key, None) {
            shell.set_status(landed);
        }
    }

    /// A number no other dispatch shares, so the work items of one bulk change
    /// gather under one undo entry and nothing else joins them.
    fn next_undo_group(&mut self) -> u64 {
        self.undo_groups += 1;
        self.undo_groups
    }

    /// Files an edit that landed on the undo stack, so `u` can put the work
    /// item back. The value restored comes off `before`, the copy the row
    /// carried until the write, so a field that was empty then goes back to
    /// cleared rather than emptied. An edit whose field a row does not model
    /// is not filed: nothing could be read back off it to restore.
    fn record_undo(&mut self, group: u64, before: &Ticket, edit: &FieldEdit) {
        let Some(undo) = edit.undoing(before) else {
            return;
        };
        let step = UndoStep {
            key: before.key.clone(),
            edit: undo,
        };
        if let Some(entry) = self
            .undo_stack
            .iter_mut()
            .find(|entry| entry.group == group)
        {
            entry.steps.push(step);
            return;
        }
        if self.undo_stack.len() == UNDO_DEPTH {
            // The oldest goes, so the stack stays a way back out of a
            // mis-click rather than a log of the session.
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(UndoEntry {
            group,
            label: edit.label().to_owned(),
            wrote: edit.value_text(),
            steps: vec![step],
        });
    }

    /// Takes back the last edit that landed: `u`. Every work item it changed
    /// goes to the value it carried before, sent down the ordinary edit path
    /// with a fresh revision test, so a work item that has moved on in Azure
    /// DevOps since refuses the undo exactly as it would refuse any other
    /// edit. An undo of several work items is gathered like a bulk change, so
    /// one that only partly lands says which rows are still where they were
    /// rather than leaving it half done in silence.
    ///
    /// An undo is not itself undoable. Filing one would make `u` a toggle
    /// between the last two values, and the edit under it on the stack would
    /// never be reached; a refused undo is dropped from the stack too, because
    /// the value it was going to restore is exactly what the conflict says is
    /// no longer to be trusted.
    pub fn undo_last_edit(&mut self, shell: &mut Shell) -> AppAction {
        let Some(entry) = self.undo_stack.pop() else {
            shell.set_status("Nothing to undo");
            return AppAction::None;
        };
        let headline = entry.headline();
        let mut requests = Vec::new();
        let mut failures = Vec::new();
        for step in &entry.steps {
            // An undo of one work item says its line as it lands, like any
            // other edit; an undo of several is spoken for by its summary.
            let line = (entry.steps.len() == 1).then(|| headline.clone());
            match self.begin_edit(shell, &step.key, step.edit.clone(), UndoRole::Undoing(line)) {
                Ok(request) => requests.push(request),
                Err(reason) => failures.push(format!("#{} failed: {reason}", step.key.id)),
            }
        }
        let bulk = BulkEdit {
            headline: BulkHeadline::Undone(headline),
            total: requests.len() + failures.len(),
            updated: 0,
            failures,
            outstanding: requests.iter().map(|request| request.key.clone()).collect(),
        };
        if bulk.outstanding.is_empty() {
            // Nothing could even be asked, so nothing was taken back: the
            // change goes back on the stack, to try again once whatever is in
            // the way has cleared.
            shell.set_error(bulk.notification());
            self.undo_stack.push(entry);
            return AppAction::None;
        }
        if entry.steps.len() > 1 {
            self.bulk_edits.push(bulk);
        }
        AppAction::Edit(requests)
    }

    /// Puts a refused edit back the way it was and says which field did not
    /// save, so a change is never dropped quietly. Only the work item named is
    /// reverted: the others a bulk change touched are left as they are.
    pub fn reject_edit(&mut self, shell: &mut Shell, rejection: &EditRejection) {
        if let Some(pending) = self.pending_edits.remove(&rejection.key)
            && let Some(index) = self.index_of(&rejection.key)
        {
            self.set_ticket(index, pending.original);
        }
        shell.flash_row(rejection.key.clone());
        if !self.record_bulk_outcome(shell, &rejection.key, Some(rejection.failure())) {
            shell.set_error(rejection.notification());
        }
    }

    /// Files one answer against the bulk change that asked for it, and says
    /// whether one did. A work item edited on its own belongs to no bulk
    /// change and speaks for itself; one that belongs to a bulk change stays
    /// quiet until the last of its work items has answered, and then the whole
    /// tally goes up at once.
    fn record_bulk_outcome(
        &mut self,
        shell: &mut Shell,
        key: &TicketKey,
        failure: Option<String>,
    ) -> bool {
        let Some(index) = self
            .bulk_edits
            .iter()
            .position(|bulk| bulk.outstanding.contains(key))
        else {
            return false;
        };
        if !self.bulk_edits[index].record(key, failure) {
            return true;
        }
        let bulk = self.bulk_edits.remove(index);
        let message = bulk.notification();
        if bulk.failed() {
            shell.set_error(message);
        } else {
            shell.set_status(message);
        }
        true
    }

    /// Puts the optimistic copies back on top of a pull that finished while an
    /// edit was still in flight, so an edited row does not flicker back to the
    /// value the pull brought. That pulled row becomes what a refusal restores,
    /// because it is the freshest copy the edit did not make.
    pub(super) fn reapply_pending_edits(&mut self) {
        if self.pending_edits.is_empty() {
            return;
        }
        let keys: Vec<TicketKey> = self.pending_edits.keys().cloned().collect();
        for key in keys {
            let Some(index) = self.index_of(&key) else {
                continue;
            };
            let pulled = self.tickets[index].clone();
            let Some(pending) = self.pending_edits.get_mut(&key) else {
                continue;
            };
            pending.original = pulled;
            let optimistic = pending.optimistic();
            self.set_ticket(index, optimistic);
        }
    }

    /// `e`: the list of field editors. Every editor is one row of
    /// [`EDIT_MENU`], so a new one appears here by being added there.
    pub(super) fn open_edit_menu(&mut self) {
        self.edit_menu = EditMenu::default();
        self.mode = WorkItemMode::Edit;
    }

    pub(super) fn handle_edit_menu_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        let last = self.edit_menu_entries().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('e') => self.mode = WorkItemMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_edit_entry(self.edit_menu.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_edit_entry((self.edit_menu.index + 1).min(last));
            }
            KeyCode::Home => self.focus_edit_entry(0),
            KeyCode::End => self.focus_edit_entry(last),
            KeyCode::Enter => return self.run_edit_menu_entry(shell, self.edit_menu.index),
            _ => {}
        }
        AppAction::None
    }

    fn focus_edit_entry(&mut self, index: usize) {
        self.edit_menu.index = index;
        self.edit_menu.scroll.ensure_visible(index);
    }

    /// Runs one Actions menu entry, which is the command it names. Each editor
    /// opens itself, so nothing here knows what a state or a title is.
    pub(super) fn run_edit_menu_entry(&mut self, shell: &mut Shell, index: usize) -> AppAction {
        let Some(entry) = self.edit_menu_entries().get(index).copied() else {
            self.mode = WorkItemMode::Browse;
            return AppAction::None;
        };
        self.mode = WorkItemMode::Browse;
        self.run_command(shell, entry.command)
    }

    /// The Actions menu as it stands for the row under the cursor: [`EDIT_MENU`],
    /// plus `Remove parent` under `Set parent…` when there is a parent to
    /// remove. Every reader of the menu goes through here, so the cursor, the
    /// mouse, and the drawing all count the same rows.
    #[must_use]
    pub fn edit_menu_entries(&self) -> Vec<EditMenuEntry> {
        let mut entries = EDIT_MENU.to_vec();
        if !self.selected_has_parent() {
            return entries;
        }
        let after = entries
            .iter()
            .position(|entry| entry.command == CommandId::SetParent)
            .map_or(entries.len(), |index| index + 1);
        entries.insert(after, REMOVE_PARENT_ROW);
        entries
    }

    /// The Actions menu's `Remove parent` row: the work item comes out of its
    /// family and hangs under nothing.
    pub(super) fn remove_parent(&mut self, shell: &mut Shell) -> AppAction {
        let Some(child) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            shell.set_error("No work item is selected");
            return AppAction::None;
        };
        if self.parent_of(&child).is_none() {
            shell.set_error(format!("#{} has no parent to remove", child.id));
            return AppAction::None;
        }
        self.begin_reparent(shell, &child, None)
    }

    /// Starts one move: the graph takes it at once in both directions, the
    /// parent it had is kept for a refusal, and the action that sends it comes
    /// back. The child progress of the parent it left and the parent it joined
    /// are both rebuilt here, so neither ratio is stale for a frame.
    pub(super) fn begin_reparent(
        &mut self,
        shell: &mut Shell,
        child: &TicketKey,
        new_parent: Option<TicketKey>,
    ) -> AppAction {
        if let Some(reason) = shell.write_refusal() {
            shell.set_error(format!("#{} not moved: {reason}", child.id));
            return AppAction::None;
        }
        if self.pending_reparents.contains_key(child) {
            shell.set_error(format!("#{}: an earlier move is still in flight", child.id));
            return AppAction::None;
        }
        let previous = self.parent_of(child);
        self.graph.reparent(child, new_parent.as_ref());
        self.refresh_child_progress();
        self.pending_reparents.insert(child.clone(), previous);
        shell.set_status(match new_parent.as_ref() {
            Some(parent) => format!("Moving #{} under #{}\u{2026}", child.id, parent.id),
            None => format!("Detaching #{}\u{2026}", child.id),
        });
        AppAction::Reparent {
            key: child.clone(),
            new_parent: new_parent.map(|parent| parent.id),
        }
    }

    /// Whether a move is waiting on Azure DevOps. The database watcher stands
    /// down while one is, for the same reason it does for an edit: the sync
    /// worker is writing those rows itself.
    #[must_use]
    pub fn reparents_pending(&self) -> bool {
        !self.pending_reparents.is_empty()
    }

    /// Settles a move Azure DevOps accepted. The links the server sent back
    /// replace the ones held for the work item, and the other half of the
    /// hierarchy link is rewritten from them, so the family the old parent
    /// still thought it had is gone whatever the optimistic guess did.
    pub fn apply_reparent(&mut self, shell: &mut Shell, applied: ReparentApplied) {
        let key = applied.ticket.key.clone();
        self.pending_reparents.remove(&key);
        let parent = applied.parent.clone();
        self.graph.replace_relations_from(&key, applied.relations);
        self.graph.reparent(&key, parent.as_ref());
        if let Some(index) = self.index_of(&key) {
            self.set_ticket(index, applied.ticket);
            self.resettle_rows(shell);
        }
        self.refresh_child_progress();
        shell.set_status(match parent {
            Some(parent) => format!("Moved #{} under #{}", key.id, parent.id),
            None => format!("Detached #{}", key.id),
        });
    }

    /// A move that did not land, so the graph goes back the way it was — both
    /// halves of the link, and the child progress of both parents with them.
    pub fn reject_reparent(&mut self, shell: &mut Shell, rejection: &ReparentRejection) {
        if let Some(previous) = self.pending_reparents.remove(&rejection.key) {
            self.graph.reparent(&rejection.key, previous.as_ref());
            self.refresh_child_progress();
        }
        let tail = if rejection.conflict {
            " \u{b7} it changed in Azure DevOps; syncing"
        } else {
            ""
        };
        shell.set_error(format!(
            "#{} not moved: {}{tail}",
            rejection.key.id, rejection.message
        ));
    }

    /// The Actions menu's Title and Tags rows: a single-line field prefilled with
    /// what the work item says now, edited with the same keys as the
    /// named-view editor.
    pub(super) fn open_prompt(&mut self, shell: &mut Shell, field: PromptField) {
        let Some(ticket) = self.selected_ticket() else {
            shell.set_error("No work item is selected");
            return;
        };
        let original = match field {
            PromptField::Title => ticket.title.clone(),
            PromptField::Tags => ticket.tags.join("; "),
            PromptField::Comment => String::new(),
        };
        let id = ticket.key.id;
        self.prompt = Some(TextPrompt {
            field,
            input: TextInput::new(original.clone()),
            id,
            original,
        });
        self.mode = WorkItemMode::Prompt;
    }

    pub(super) fn handle_prompt_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.close_prompt(),
            KeyCode::Enter => return self.submit_prompt(shell),
            _ => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.input.handle_key(key);
                }
            }
        }
        AppAction::None
    }

    pub(super) fn close_prompt(&mut self) {
        self.prompt = None;
        self.mode = WorkItemMode::Browse;
    }

    /// Saves what the prompt holds. A title is trimmed, and one that is empty
    /// or only whitespace is refused here rather than sent, with the prompt
    /// left open on it. A tag list is normalised. Text that comes back to what
    /// the work item already says closes the prompt without a write.
    pub(super) fn submit_prompt(&mut self, shell: &mut Shell) -> AppAction {
        let Some(prompt) = self.prompt.as_ref() else {
            self.mode = WorkItemMode::Browse;
            return AppAction::None;
        };
        let field = prompt.field;
        let original = prompt.original.trim().to_owned();
        let edited = match field {
            PromptField::Title | PromptField::Comment => prompt.input.text().trim().to_owned(),
            PromptField::Tags => normalize_tags(prompt.input.text()),
        };
        if edited.is_empty() {
            match field {
                PromptField::Title => {
                    shell.set_error(format!("#{} title cannot be empty", prompt.id));
                    return AppAction::None;
                }
                PromptField::Comment => {
                    shell.set_error(format!("#{} comment cannot be empty", prompt.id));
                    return AppAction::None;
                }
                PromptField::Tags => {}
            }
        }
        self.close_prompt();
        if field != PromptField::Comment && edited == original {
            return AppAction::None;
        }
        match field {
            PromptField::Title => self.edit_selected(shell, FieldEdit::title(&edited)),
            PromptField::Tags => self.edit_selected(shell, FieldEdit::tags(&edited)),
            PromptField::Comment => self.comment_selected(shell, edited),
        }
    }

    /// Asks for the selected work item's description to be opened in the
    /// user's editor. Nothing is written here: the action carries the markup
    /// out to the editor hand-off, which brings back whatever was saved and
    /// sends it down [`Self::edit_ticket`] like any other field. Only the
    /// refusals worth making before somebody spends minutes typing are made
    /// here.
    pub(super) fn edit_description(&mut self, shell: &mut Shell) -> AppAction {
        let Some(ticket) = self.selected_ticket() else {
            shell.set_error("No work item is selected");
            return AppAction::None;
        };
        let key = ticket.key.clone();
        let html = ticket.description_html.clone();
        if let Some(reason) = shell.write_refusal() {
            shell.set_error(format!("#{} description not saved: {reason}", key.id));
            return AppAction::None;
        }
        AppAction::EditDescription { key, html }
    }

    /// Asks for a comment to be left on the selected work item. Unlike a field
    /// edit nothing is shown until Azure DevOps has stored it: a comment has no
    /// id, date, or author until the server gives it one, and a line that
    /// turned out never to have been posted is worse than a moment's wait.
    pub fn comment_selected(&mut self, shell: &mut Shell, text: String) -> AppAction {
        let Some(key) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            shell.set_error("No work item is selected");
            return AppAction::None;
        };
        let refusal = shell.write_refusal().or_else(|| {
            self.pending_comments
                .contains(&key)
                .then(|| "an earlier comment is still in flight".to_owned())
        });
        if let Some(reason) = refusal {
            shell.set_error(format!("#{} comment not posted: {reason}", key.id));
            return AppAction::None;
        }
        self.pending_comments.insert(key.clone());
        AppAction::Comment { key, text }
    }

    /// Whether a comment is waiting on Azure DevOps. The database watcher
    /// stands down while one is, because the sync worker is writing that row
    /// itself.
    #[must_use]
    pub fn comments_pending(&self) -> bool {
        !self.pending_comments.is_empty()
    }

    /// Files the comment Azure DevOps stored, so the details pane shows it at
    /// once rather than waiting for the pull that would bring it back.
    pub fn apply_comment(&mut self, shell: &mut Shell, comment: CommentRecord) {
        self.pending_comments.remove(&comment.ticket);
        let id = comment.ticket.id;
        self.graph.add_comment(comment);
        shell.set_status(format!("Commented on #{id}"));
    }

    /// A comment that never landed. Nothing was shown for it and nothing is
    /// stored, so only the notification is left to say so.
    pub fn reject_comment(&mut self, shell: &mut Shell, key: &TicketKey, message: &str) {
        self.pending_comments.remove(key);
        shell.set_error(format!("#{} comment not posted: {message}", key.id));
    }

    /// Who a typed assignee names. A name the database already knows is written
    /// by the address the assignee picker would have used, and anything else
    /// goes out as it was typed for Azure DevOps to resolve — the same rule
    /// `ticket-tui create --assignee` follows.
    #[must_use]
    pub(super) fn assignee_edit(&self, name: &str) -> FieldEdit {
        self.identities
            .iter()
            .find(|identity| {
                same_text(&identity.display_name, name)
                    || identity
                        .unique_name
                        .as_deref()
                        .is_some_and(|unique| same_text(unique, name))
            })
            .map_or_else(
                || FieldEdit::assignee(name, None),
                |identity| {
                    FieldEdit::assignee(&identity.display_name, identity.unique_name.as_deref())
                },
            )
    }

    /// The Actions menu's **Delete work item…** row, and the same by name in the
    /// palette. There is no key bound to it: every other editor is a keypress
    /// away because the worst it can do is a value somebody types over, and
    /// this one takes the work item off the board.
    ///
    /// The confirmation is opened over every checked row when two or more are
    /// checked, and over the row under the cursor otherwise — the rule the
    /// bulk editors already follow.
    pub(super) fn open_delete_confirm(&mut self, shell: &mut Shell) {
        if let Some(reason) = shell.write_refusal() {
            shell.set_error(reason);
            return;
        }
        let checked = self.checked_keys();
        let keys = if checked.len() >= 2 {
            checked
        } else {
            self.selected_ticket()
                .map(|ticket| ticket.key.clone())
                .into_iter()
                .collect()
        };
        if keys.is_empty() {
            shell.set_error("No work item is selected");
            return;
        }
        if keys.iter().any(|key| self.pending_deletes.contains(key)) {
            shell.set_error("That work item is already being deleted");
            return;
        }
        // A child going the same way is not an orphan, so a delete of a parent
        // and its children together warns about neither.
        let doomed: HashSet<TicketKey> = keys.iter().cloned().collect();
        let children = keys
            .iter()
            .flat_map(|key| self.graph.children_of(key))
            .filter(|child| !doomed.contains(child))
            .count();
        let subject = match keys.as_slice() {
            [key] => self
                .ticket_by_key(key)
                .map(|ticket| (ticket.key.id, ticket.title.clone())),
            _ => None,
        };
        self.delete_confirm = Some(DeleteConfirm {
            keys,
            subject,
            children,
        });
        self.mode = WorkItemMode::ConfirmDelete;
        shell.overlay_anchor = OverlayAnchor::Centered;
    }

    pub(super) fn handle_delete_confirm_key(
        &mut self,
        shell: &mut Shell,
        key: KeyEvent,
    ) -> AppAction {
        match key.code {
            // Enter is not it. The confirmation of a delete should take a
            // letter nobody presses on the way past, and `Esc` should be the
            // reflex that gets out of it.
            KeyCode::Char('d') | KeyCode::Char('D') => self.confirm_delete(shell),
            KeyCode::Esc | KeyCode::Char('q') => {
                self.cancel_delete();
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    /// Sends the confirmed work items to the recycle bin, one request each,
    /// which the worker takes in the order the table holds them. Nothing leaves
    /// the table here: a row is dropped when Azure DevOps says the work item is
    /// gone, so a refusal leaves it exactly where it was.
    pub fn confirm_delete(&mut self, shell: &mut Shell) -> AppAction {
        self.mode = WorkItemMode::Browse;
        let Some(confirm) = self.delete_confirm.take() else {
            return AppAction::None;
        };
        let keys = confirm.keys;
        match keys.as_slice() {
            [] => return AppAction::None,
            [key] => shell.set_status(format!("Deleting #{}\u{2026}", key.id)),
            keys => {
                // The same tracker a bulk edit uses, so a checked-set delete
                // speaks once when the last answer is in rather than once a row.
                self.bulk_edits.push(BulkEdit {
                    headline: BulkHeadline::Deleted,
                    total: keys.len(),
                    updated: 0,
                    failures: Vec::new(),
                    outstanding: keys.iter().cloned().collect(),
                });
                shell.set_status(format!("Deleting {} tickets\u{2026}", keys.len()));
            }
        }
        self.pending_deletes.extend(keys.iter().cloned());
        AppAction::Delete(keys)
    }

    /// Closes the confirmation without deleting anything. Nothing was written
    /// and nothing was changed on screen, so there is nothing to say about it.
    pub fn cancel_delete(&mut self) {
        self.delete_confirm = None;
        self.mode = WorkItemMode::Browse;
    }

    /// Whether a work item is on its way to the recycle bin. The database
    /// watcher stands down while one is, because the sync worker is taking that
    /// row out of the file itself.
    #[must_use]
    pub fn deletes_pending(&self) -> bool {
        !self.pending_deletes.is_empty()
    }

    /// Takes a deleted work item off the table. Azure DevOps has it in the
    /// recycle bin and the sync worker has already taken it out of SQLite, so
    /// this is where memory catches up: the row, its links in both directions,
    /// and everything the session was holding about it.
    pub fn apply_deleted(&mut self, shell: &mut Shell, key: &TicketKey) {
        self.pending_deletes.remove(key);
        self.forget_ticket(shell, key);
        if !self.record_bulk_outcome(shell, key, None) {
            shell.set_status(format!(
                "Deleted #{} \u{b7} restore it from the Azure DevOps recycle bin",
                key.id
            ));
        }
    }

    /// A work item that is still there. Nothing was taken off the table for it,
    /// so nothing has to be put back — only the refusal has to be reported.
    pub fn reject_delete(&mut self, shell: &mut Shell, key: &TicketKey, message: &str) {
        self.pending_deletes.remove(key);
        if !self.record_bulk_outcome(shell, key, Some(format!("#{} failed: {message}", key.id))) {
            shell.set_error(format!("#{} not deleted: {message}", key.id));
        }
    }

    /// Drops one work item out of memory: the row, its links, its discussion,
    /// and every set the session keeps work items in. Its children are left
    /// where they are — a delete takes the one work item — so only the links
    /// naming it go, which is what leaves them parentless rather than gone.
    ///
    /// A delete is not undoable, so nothing is filed for it; an edit already on
    /// the undo stack for this work item is dropped instead, because there is
    /// no longer a row to put anything back on.
    fn forget_ticket(&mut self, shell: &mut Shell, key: &TicketKey) {
        let Some(index) = self.index_of(key) else {
            return;
        };
        let next = self.next_after_removal(key);
        let was_on_screen = self
            .selected_ticket()
            .is_some_and(|ticket| ticket.key == *key);
        Arc::make_mut(&mut self.tickets).remove(index);
        self.graph.forget(key);
        self.bookmarks.remove(key);
        self.selected_keys.remove(key);
        self.pending_edits.remove(key);
        self.pending_comments.remove(key);
        shell.forget_jump(&Jump::WorkItem(key.clone()));
        if self.details_pending.as_ref() == Some(key) {
            self.details_pending = None;
        }
        for entry in &mut self.undo_stack {
            entry.steps.retain(|step| step.key != *key);
        }
        self.undo_stack.retain(|entry| !entry.steps.is_empty());
        // Every row index the search documents held moved with the row, so
        // they are built again rather than patched.
        self.search.replace_tickets(&self.tickets);
        self.refresh_child_progress();
        if self.fuzzy_query().is_empty() {
            self.show_all(shell, next.as_ref());
        } else {
            self.pending_selection = next;
            self.visible.clear();
            self.table_state.select(None);
            self.submit_search();
        }
        if was_on_screen {
            // The details pane is now over a different work item, so it reads
            // from the top rather than from wherever the last one was scrolled
            // to.
            self.details.scroll_to(0);
        }
    }

    /// Where the cursor goes when one work item leaves. It stays on the work
    /// item it is on unless that is the one going, in which case it takes the
    /// next row down — the one about to move up into its place, so deleting a
    /// run of rows reads as working down the list — and the row above only
    /// when there is nothing below. Rows already on their way to the recycle
    /// bin are passed over, so a checked-set delete never parks the cursor on
    /// a row that is about to go too.
    fn next_after_removal(&self, key: &TicketKey) -> Option<TicketKey> {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        if selected.as_ref().is_some_and(|held| held != key) {
            return selected;
        }
        let row = self.visible_row(key)?;
        let rows: Vec<TicketKey> = self
            .visible
            .iter()
            .map(|entry| self.tickets[entry.ticket_index].key.clone())
            .collect();
        let survives =
            |candidate: &&TicketKey| *candidate != key && !self.pending_deletes.contains(candidate);
        rows.iter()
            .skip(row.saturating_add(1))
            .find(survives)
            .or_else(|| rows.iter().take(row).rev().find(survives))
            .cloned()
    }
}
