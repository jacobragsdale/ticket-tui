//! The form overlay that files a new work item, and the one-row capture that
//! fills one in without asking.

use super::*;

/// What every work item captured with `+` is tagged. It is the triage hook and
/// it is not configurable: a `tags:inbox` view finds everything captured and
/// not yet filed, and the point of the row is that there is nothing to decide.
const CAPTURE_TAG: &str = "inbox";

/// Which form is open. A form Esc closed is kept under this, so `n` brings back
/// what was typed and a form opened over a different work item never inherits
/// somebody else's draft.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormKind {
    /// The new work item form `n` opens, hanging under nothing in particular.
    NewWorkItem,
    /// The new child form `N` opens over a work item, carrying the id of the
    /// work item the new one hangs under. That id is part of the kind, so a
    /// draft left under one parent never reopens under another.
    NewChild(i64),
    /// The form `+` fills in and submits without ever showing it. It is the
    /// same create as any other; the kind is what keeps the cursor where it
    /// was when the work item lands.
    QuickCapture,
}

/// Which value one field of a form holds. A form is read back by these rather
/// than by row number, so adding a field to one never moves what submit reads,
/// and a picker knows where to write its answer.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum FormFieldId {
    #[default]
    Type,
    Title,
    Parent,
    Iteration,
    Area,
    Assignee,
    Priority,
    Tags,
}

/// The picker one form field opens, which is the same picker the Actions menu
/// opens over a work item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormPicker {
    WorkItemType,
    Iteration,
    Area,
    Assignee,
}

/// How one field is filled in: typed into, or chosen from a list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormFieldKind {
    Text,
    /// `Enter` opens the picker, which writes the choice back into the field.
    Picker(FormPicker),
}

/// One field of a form: what it is called, how it is filled in, and what it
/// holds. A picker's choice lands in the same [`TextInput`] typing would, so
/// every field is read back the same way whatever filled it.
#[derive(Clone, Debug)]
pub struct FormField {
    pub id: FormFieldId,
    /// What the row is called, which is also what a refusal names.
    pub label: &'static str,
    pub kind: FormFieldKind,
    /// Whether submitting the form without it is refused.
    pub required: bool,
    /// Whether whoever opened the form filled this in and it cannot be changed
    /// here — the parent a child is being filed under.
    pub read_only: bool,
    pub input: TextInput,
    /// What an empty field says, so a blank row still reads as a field.
    pub placeholder: &'static str,
    /// What the row reads as, when that is not the value itself: a parent is
    /// held as the id a create needs and shown as the work item it names.
    pub display: Option<String>,
}

impl FormField {
    #[must_use]
    pub fn text(id: FormFieldId, label: &'static str) -> Self {
        Self {
            id,
            label,
            kind: FormFieldKind::Text,
            required: false,
            read_only: false,
            input: TextInput::default(),
            placeholder: "",
            display: None,
        }
    }

    #[must_use]
    pub fn picker(id: FormFieldId, label: &'static str, picker: FormPicker) -> Self {
        Self {
            kind: FormFieldKind::Picker(picker),
            ..Self::text(id, label)
        }
    }

    #[must_use]
    pub fn required(self) -> Self {
        Self {
            required: true,
            ..self
        }
    }

    #[must_use]
    pub fn read_only(self) -> Self {
        Self {
            read_only: true,
            ..self
        }
    }

    #[must_use]
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.input = TextInput::new(value);
        self
    }

    #[must_use]
    pub fn with_placeholder(self, placeholder: &'static str) -> Self {
        Self {
            placeholder,
            ..self
        }
    }

    /// Gives the field a reading of its own, for a value that is not worth
    /// showing as it is written: `#595` is stored, `#595 Tech debt and
    /// architecture foundation` is read.
    #[must_use]
    pub fn with_display(self, display: impl Into<String>) -> Self {
        Self {
            display: Some(display.into()),
            ..self
        }
    }

    #[must_use]
    pub fn value(&self) -> &str {
        self.input.text()
    }

    /// What the row shows, which is the value unless whoever opened the form
    /// gave the field a reading of its own.
    #[must_use]
    pub fn shown(&self) -> &str {
        self.display.as_deref().unwrap_or_else(|| self.value())
    }

    /// Whether the field says nothing, which is what a required field is
    /// refused for. Whitespace is nothing.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.value().trim().is_empty()
    }

    /// Whether typing goes into this field. A picker field is filled from its
    /// list, and a read-only one by whoever opened the form.
    #[must_use]
    pub const fn is_typed(&self) -> bool {
        matches!(self.kind, FormFieldKind::Text) && !self.read_only
    }

    /// The picker `Enter` opens on this field, if it opens one.
    #[must_use]
    pub const fn picker_kind(&self) -> Option<FormPicker> {
        match self.kind {
            FormFieldKind::Picker(picker) if !self.read_only => Some(picker),
            _ => None,
        }
    }
}

/// A multi-field overlay: an ordered list of fields and a cursor over them, and
/// nothing at all about what any of them mean. Which fields a form has, what
/// its frame says, and what submitting it does all belong to whoever built it,
/// so the next form in the app is a field list rather than a second widget.
#[derive(Clone, Debug)]
pub struct FormOverlay {
    /// Which form this is, which is what a kept draft is filed under.
    pub kind: FormKind,
    /// What the frame says.
    pub title: String,
    pub fields: Vec<FormField>,
    /// The field under the cursor.
    pub index: usize,
}

impl FormOverlay {
    #[must_use]
    pub fn new(kind: FormKind, title: impl Into<String>, fields: Vec<FormField>) -> Self {
        Self {
            kind,
            title: title.into(),
            fields,
            index: 0,
        }
    }

    #[must_use]
    pub fn focused(&self) -> Option<&FormField> {
        self.fields.get(self.index)
    }

    /// Moves the cursor a field at a time, wrapping at both ends: a form is a
    /// ring, so a step past the last field comes back to the first rather than
    /// stopping dead against it.
    pub fn move_focus(&mut self, delta: isize) {
        let count = isize::try_from(self.fields.len()).unwrap_or(isize::MAX);
        if count == 0 {
            return;
        }
        let index = isize::try_from(self.index).unwrap_or_default() + delta;
        self.focus(usize::try_from(index.rem_euclid(count)).unwrap_or_default());
    }

    pub fn focus(&mut self, index: usize) {
        self.index = index.min(self.fields.len().saturating_sub(1));
    }

    #[must_use]
    pub fn field(&self, id: FormFieldId) -> Option<&FormField> {
        self.fields.iter().find(|field| field.id == id)
    }

    #[must_use]
    pub fn index_of(&self, id: FormFieldId) -> Option<usize> {
        self.fields.iter().position(|field| field.id == id)
    }

    /// What one field holds, or nothing at all for a field this form does not
    /// have: a form is read by name, so asking after a field it never had is an
    /// empty answer rather than a panic.
    #[must_use]
    pub fn value(&self, id: FormFieldId) -> &str {
        self.field(id).map_or("", FormField::value)
    }

    /// Writes one field, which is how a picker hands its choice back.
    pub fn set_value(&mut self, id: FormFieldId, value: impl Into<String>) {
        if let Some(field) = self.fields.iter_mut().find(|field| field.id == id) {
            field.input = TextInput::new(value);
        }
    }

    /// The first required field left empty, which is what a refused submit
    /// names and moves the cursor to.
    #[must_use]
    pub fn first_blank_required(&self) -> Option<&FormField> {
        self.fields
            .iter()
            .find(|field| field.required && field.is_blank())
    }

    /// Whether every required field says something, which is what leaves the
    /// `[Create]` button lit rather than greyed.
    #[must_use]
    pub fn is_submittable(&self) -> bool {
        self.first_blank_required().is_none()
    }
}

/// What one form field holding a whole number says, or a refusal naming it. An
/// empty field is no number at all rather than a bad one: both the parent and
/// the priority are optional.
pub(super) fn form_number(form: &FormOverlay, id: FormFieldId) -> Result<Option<i64>, String> {
    let Some(field) = form.field(id) else {
        return Ok(None);
    };
    let text = field.value().trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<i64>()
        .map(Some)
        .map_err(|_| format!("{} must be a whole number, not \"{text}\"", field.label))
}

impl WorkItemsScreen {
    /// Opens the new work item form: `n`. A draft `Esc` left behind comes back
    /// exactly as it was, cursor and all, so a form closed to go and read
    /// something is not a form retyped.
    pub fn open_create_form(&mut self, shell: &mut Shell) -> AppAction {
        if self.pending_create.is_some() {
            shell.set_error("A work item is already being created");
            return AppAction::None;
        }
        let form = self.take_draft(FormKind::NewWorkItem).unwrap_or_else(|| {
            FormOverlay::new(
                FormKind::NewWorkItem,
                "New work item",
                self.create_form_fields(None),
            )
        });
        self.open_form(shell, form)
    }

    /// Opens the new child form: `N`, the Actions menu's New child row, or the
    /// palette. Breaking an Epic into Issues or an Issue into Tasks is the
    /// commonest thing anybody files, and none of it is worth retyping, so the
    /// form opens with the parent fixed, the type the parent's own type breaks
    /// down into, and the area and iteration the parent sits in. The draft is
    /// kept per parent, so a child half typed under one work item is not
    /// offered under the next.
    pub fn open_child_form(&mut self, shell: &mut Shell) -> AppAction {
        if self.pending_create.is_some() {
            shell.set_error("A work item is already being created");
            return AppAction::None;
        }
        let Some(parent) = self.selected_ticket() else {
            shell.set_error("No work item is selected");
            return AppAction::None;
        };
        let id = parent.key.id;
        let kind = FormKind::NewChild(id);
        let form = self.take_draft(kind).unwrap_or_else(|| {
            FormOverlay::new(
                kind,
                format!("New child of #{id}"),
                self.create_form_fields(Some(id)),
            )
        });
        self.open_form(shell, form)
    }

    /// Shows one form and asks for whatever it needs that is not in memory yet.
    /// Every form opens this way, so the placement, the cursor, and the single
    /// types fetch a session are the same for all of them.
    fn open_form(&mut self, shell: &mut Shell, form: FormOverlay) -> AppAction {
        self.form = Some(form);
        self.mode = WorkItemMode::Form;
        shell.overlay_anchor = OverlayAnchor::Centered;
        self.form_scroll.scroll_to(0);
        if self.work_item_types_requested {
            AppAction::None
        } else {
            self.work_item_types_requested = true;
            AppAction::FetchWorkItemTypes
        }
    }

    /// The draft kept for one form, if the last one cancelled was that form.
    /// A draft of some other form is left where it is: what was typed about
    /// one work item is no use in a form about another.
    fn take_draft(&mut self, kind: FormKind) -> Option<FormOverlay> {
        if self
            .form_draft
            .as_ref()
            .is_some_and(|draft| draft.kind == kind)
        {
            self.form_draft.take()
        } else {
            None
        }
    }

    /// The fields of the new work item form, in the order they are filled in.
    /// The area and the iteration start where the work item the form was
    /// opened over sits, falling back to the sprint the project is in, because
    /// new work almost always joins the work beside it.
    ///
    /// `parent` is filled in by whoever opened the form, and a form that has
    /// one is a form about that work item: the parent row is fixed and reads
    /// as the work item rather than as its id, the type is the one the
    /// parent's own type breaks down into, and the area and the iteration are
    /// the parent's rather than the selected row's.
    #[must_use]
    fn create_form_fields(&self, parent: Option<i64>) -> Vec<FormField> {
        let parent_ticket = parent.and_then(|id| self.ticket_by_id(id));
        let inherited = parent_ticket.or_else(|| self.selected_ticket());
        let iteration = inherited
            .map(|ticket| ticket.iteration_path.clone())
            .or_else(|| self.current_iteration())
            .unwrap_or_default();
        let area = inherited
            .map(|ticket| ticket.area_path.clone())
            .unwrap_or_default();
        let work_item_type = parent_ticket.map_or_else(
            || DEFAULT_WORK_ITEM_TYPE.to_owned(),
            |ticket| self.child_work_item_type(&ticket.work_item_type),
        );
        let parent_field = FormField::text(FormFieldId::Parent, "Parent")
            .with_placeholder("none — a work item id");
        vec![
            FormField::picker(FormFieldId::Type, "Type", FormPicker::WorkItemType)
                .required()
                .with_value(work_item_type),
            FormField::text(FormFieldId::Title, "Title")
                .required()
                .with_placeholder("what needs doing"),
            match parent {
                Some(id) => parent_field
                    .with_value(id.to_string())
                    .with_display(match parent_ticket {
                        Some(ticket) => format!("#{id} {}", ticket.title),
                        None => format!("#{id}"),
                    })
                    .read_only(),
                None => parent_field,
            },
            FormField::picker(FormFieldId::Iteration, "Iteration", FormPicker::Iteration)
                .with_value(iteration)
                .with_placeholder("the project root"),
            FormField::picker(FormFieldId::Area, "Area", FormPicker::Area)
                .with_value(area)
                .with_placeholder("the project root"),
            FormField::picker(FormFieldId::Assignee, "Assignee", FormPicker::Assignee)
                .with_placeholder("nobody"),
            FormField::text(FormFieldId::Priority, "Priority").with_placeholder("unset — 1 to 4"),
            FormField::text(FormFieldId::Tags, "Tags").with_placeholder("semicolon separated"),
        ]
    }

    /// One work item by the id a form field names, whatever organization it
    /// came from: a form holds an id and nothing else, and every row on screen
    /// came from the same project.
    #[must_use]
    fn ticket_by_id(&self, id: i64) -> Option<&Ticket> {
        self.tickets.iter().find(|ticket| ticket.key.id == id)
    }

    pub(super) fn handle_form_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.cancel_form(),
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.submit_form(shell);
            }
            KeyCode::Up | KeyCode::BackTab => self.move_form_focus(-1),
            KeyCode::Down | KeyCode::Tab => self.move_form_focus(1),
            KeyCode::Enter => return self.activate_form_field(shell),
            // Everything else is typing, and a field filled from a picker or by
            // whoever opened the form takes none of it.
            _ => {
                if let Some(field) = self.focused_form_field_mut()
                    && field.is_typed()
                {
                    field.input.handle_key(key);
                }
            }
        }
        AppAction::None
    }

    pub(super) fn focused_form_field_mut(&mut self) -> Option<&mut FormField> {
        let form = self.form.as_mut()?;
        form.fields.get_mut(form.index)
    }

    fn move_form_focus(&mut self, delta: isize) {
        if let Some(form) = self.form.as_mut() {
            form.move_focus(delta);
            let index = form.index;
            self.form_scroll.ensure_visible(index);
        }
    }

    pub(super) fn focus_form_field(&mut self, index: usize) {
        if let Some(form) = self.form.as_mut() {
            form.focus(index);
            let index = form.index;
            self.form_scroll.ensure_visible(index);
        }
    }

    /// `Enter` on a field: a picker field opens its picker, and anything else
    /// moves on to the next field. Submitting is deliberately not bound here —
    /// `Ctrl-S` and `[Create]` do that — so a stray `Enter` halfway down a form
    /// never files a half-typed work item.
    pub(super) fn activate_form_field(&mut self, shell: &mut Shell) -> AppAction {
        let Some(field) = self.form.as_ref().and_then(FormOverlay::focused) else {
            return AppAction::None;
        };
        let (id, picker, current) = (field.id, field.picker_kind(), field.value().to_owned());
        match picker {
            Some(FormPicker::WorkItemType) => {
                self.open_type_picker(id, current);
                AppAction::None
            }
            Some(FormPicker::Iteration) => {
                let current = if current.is_empty() {
                    self.current_iteration().unwrap_or_default()
                } else {
                    current
                };
                self.show_node_picker(NodeKind::Iteration, current, EditScope::Form(id))
            }
            Some(FormPicker::Area) => {
                self.show_node_picker(NodeKind::Area, current, EditScope::Form(id))
            }
            Some(FormPicker::Assignee) => {
                let current = (!current.trim().is_empty()).then_some(current);
                self.show_assignee_picker(shell, current, EditScope::Form(id))
            }
            None => {
                self.move_form_focus(1);
                AppAction::None
            }
        }
    }

    /// Writes a picker's choice back into the form that opened it and returns
    /// to it, which is what every picker a form field opens does with its
    /// answer.
    pub(super) fn fill_form_field(&mut self, id: FormFieldId, value: impl Into<String>) {
        if let Some(form) = self.form.as_mut() {
            form.set_value(id, value);
            if let Some(index) = form.index_of(id) {
                form.focus(index);
            }
        }
        self.mode = if self.form.is_some() {
            WorkItemMode::Form
        } else {
            WorkItemMode::Browse
        };
    }

    /// Where a picker goes when it closes with nothing chosen: back to the form
    /// that opened it, or to the table.
    pub(super) fn close_picker(&mut self, scope: EditScope) {
        self.mode = if matches!(scope, EditScope::Form(_)) && self.form.is_some() {
            WorkItemMode::Form
        } else {
            WorkItemMode::Browse
        };
    }

    /// What a picker's frame says it is changing: the work item or the checked
    /// rows, or the form that opened it.
    #[must_use]
    pub fn scope_label(&self, scope: EditScope) -> String {
        match scope {
            EditScope::Form(_) => self
                .form
                .as_ref()
                .map_or_else(|| scope.label(), |form| form.title.clone()),
            other => other.label(),
        }
    }

    /// `Esc`: the form closes and everything typed into it is kept, so `n`
    /// brings it back. The draft lives in memory for the session and is never
    /// written to the session file.
    pub(super) fn cancel_form(&mut self) {
        self.form_draft = self.form.take();
        self.mode = WorkItemMode::Browse;
    }

    /// Files the form: `Ctrl-S`, or `[Create]`. Everything that can be refused
    /// before the network is refused here — a required field left empty, a
    /// parent or a priority that is not a number — with the form left open on
    /// the field at fault rather than a document of nonsense sent out.
    pub(super) fn submit_form(&mut self, shell: &mut Shell) -> AppAction {
        let Some(form) = self.form.as_ref() else {
            self.mode = WorkItemMode::Browse;
            return AppAction::None;
        };
        if let Some(missing) = form.first_blank_required() {
            let (label, id) = (missing.label, missing.id);
            let index = form.index_of(id).unwrap_or_default();
            self.focus_form_field(index);
            shell.set_error(format!("{label} is required"));
            return AppAction::None;
        }
        let parent = match form_number(form, FormFieldId::Parent) {
            Ok(parent) => parent,
            Err(message) => {
                self.refuse_form(shell, FormFieldId::Parent, message);
                return AppAction::None;
            }
        };
        let priority = match form_number(form, FormFieldId::Priority) {
            Ok(priority) => priority,
            Err(message) => {
                self.refuse_form(shell, FormFieldId::Priority, message);
                return AppAction::None;
            }
        };
        let work_item_type = form.value(FormFieldId::Type).trim().to_owned();
        let mut edits = vec![FieldEdit::title(form.value(FormFieldId::Title).trim())];
        let assignee = form.value(FormFieldId::Assignee).trim().to_owned();
        if !assignee.is_empty() {
            edits.push(self.assignee_edit(&assignee));
        }
        if let Some(priority) = priority {
            edits.push(FieldEdit::priority(priority));
        }
        let iteration = form.value(FormFieldId::Iteration).trim().to_owned();
        if !iteration.is_empty() {
            edits.push(FieldEdit::iteration(&iteration));
        }
        let area = form.value(FormFieldId::Area).trim().to_owned();
        if !area.is_empty() {
            edits.push(FieldEdit::area(&area));
        }
        let tags = normalize_tags(form.value(FormFieldId::Tags));
        if !tags.is_empty() {
            edits.push(FieldEdit::tags(&tags));
        }
        if let Some(reason) = shell.write_refusal() {
            shell.set_error(format!("Work item not created: {reason}"));
            return AppAction::None;
        }
        let patch: Vec<Value> = edits.iter().flat_map(FieldEdit::patch).collect();
        // The form is held rather than dropped: a refusal has to put it back
        // with everything still in it.
        self.pending_create = self.form.take();
        self.mode = WorkItemMode::Browse;
        shell.set_status(format!("Creating {work_item_type}\u{2026}"));
        AppAction::Create {
            work_item_type,
            patch,
            parent,
        }
    }

    /// Refuses a submit and puts the cursor on the field that caused it, so the
    /// message and the caret name the same thing.
    fn refuse_form(&mut self, shell: &mut Shell, id: FormFieldId, message: String) {
        if let Some(index) = self.form.as_ref().and_then(|form| form.index_of(id)) {
            self.focus_form_field(index);
        }
        shell.set_error(message);
    }

    /// Whether a work item is waiting to be created. The database watcher
    /// stands down while one is, because the sync worker is writing that row
    /// itself.
    #[must_use]
    pub fn creates_pending(&self) -> bool {
        self.pending_create.is_some()
    }

    /// Files the work item Azure DevOps stored. Nothing was shown for it until
    /// now — a work item has no id, revision, or URL until the server gives it
    /// one — so this is where the row appears, with the links it came back
    /// carrying, and the selection moves onto it. A row the current query would
    /// hide is no use to anybody, so the query is cleared and the status line
    /// says it was.
    pub fn apply_created(
        &mut self,
        shell: &mut Shell,
        ticket: Ticket,
        relations: Vec<RelationRecord>,
    ) {
        let captured = self
            .pending_create
            .take()
            .is_some_and(|form| form.kind == FormKind::QuickCapture);
        let key = ticket.key.clone();
        let headline = format!("Created {} #{}", ticket.work_item_type, key.id);
        let hidden = self.query_would_hide(&ticket);
        let index = self.tickets.len();
        Arc::make_mut(&mut self.tickets).push(ticket);
        self.reindex_tickets();
        self.search.push_document(index, &self.tickets[index]);
        self.graph.replace_relations_from(&key, relations);
        self.refresh_child_progress();
        // A capture goes nowhere: the point of `+` is to leave the cursor, the
        // query and the tab exactly as they were. The row joins the table if
        // the query has room for it, and the notice carries the id.
        if captured {
            self.resubmit_query(shell);
            shell.set_status(headline);
            return;
        }
        if hidden {
            self.set_query(shell, String::new());
        }
        self.show_all(shell, Some(&key));
        self.details.scroll_to(0);
        shell.set_status(if hidden {
            format!("{headline} \u{b7} search cleared so it is visible")
        } else {
            headline
        });
    }

    /// Whether the query on screen would leave a work item off the table. A
    /// search term counts as hiding it whatever it says: the matching runs on
    /// the search thread and answers a frame or two later, and a new work item
    /// nobody can see is worse than a query cleared a little eagerly.
    #[must_use]
    fn query_would_hide(&self, ticket: &Ticket) -> bool {
        let parsed = self.parsed_query();
        !parsed.fuzzy.trim().is_empty()
            || !parsed
                .filters
                .matches(ticket, self.bookmarks.contains(&ticket.key))
    }

    /// A work item that was never created. The form comes back exactly as it
    /// went out, so nothing typed is lost and the refusal can be answered where
    /// it was caused.
    pub fn reject_create(&mut self, shell: &mut Shell, message: &str) {
        if let Some(form) = self.pending_create.take() {
            if form.kind == FormKind::QuickCapture {
                // The row comes back with the thought in it rather than a
                // five-field form nobody opened.
                self.capture
                    .set_text(form.value(FormFieldId::Title).to_owned());
                self.mode = WorkItemMode::Capture;
            } else {
                self.form = Some(form);
                self.mode = WorkItemMode::Form;
                shell.overlay_anchor = OverlayAnchor::Centered;
            }
        }
        shell.set_error(format!("Work item not created: {message}"));
    }

    /// Opens the quick capture row: `+`, from any tab, because that is where
    /// the thoughts arrive. One create is out at a time, the same as the form.
    pub(super) fn open_capture(&mut self, shell: &mut Shell) -> AppAction {
        if self.pending_create.is_some() {
            shell.set_error("A work item is already being created");
            return AppAction::None;
        }
        self.capture.clear();
        self.mode = WorkItemMode::Capture;
        AppAction::None
    }

    pub(super) fn handle_capture_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.cancel_capture(),
            KeyCode::Enter => return self.submit_capture(shell),
            _ => {
                self.capture.handle_key(key);
            }
        }
        AppAction::None
    }

    /// `Esc`: nothing is kept. A one-line title that was abandoned was not
    /// worth keeping, so there is no draft here as there is on the form.
    pub(super) fn cancel_capture(&mut self) {
        self.capture.clear();
        self.mode = WorkItemMode::Browse;
    }

    /// `Enter`: the title, and every other field defaulted rather than asked —
    /// the type `n` defaults to, `@me`, the sprint the project is in, and the
    /// one constant tag that is the triage hook. It goes out as a form filled
    /// in and submitted, so a capture is the same create as `n`: the same
    /// write-through, the same refusal, the same notice.
    fn submit_capture(&mut self, shell: &mut Shell) -> AppAction {
        let title = self.capture.text().trim().to_owned();
        if title.is_empty() {
            shell.set_error("A work item needs a title");
            return AppAction::None;
        }
        // Refused here rather than by the submit below, so the row stays open
        // with the thought still in it.
        if let Some(reason) = shell.write_refusal() {
            shell.set_error(format!("Work item not created: {reason}"));
            return AppAction::None;
        }
        let fields = vec![
            FormField::picker(FormFieldId::Type, "Type", FormPicker::WorkItemType)
                .required()
                .with_value(DEFAULT_WORK_ITEM_TYPE),
            FormField::text(FormFieldId::Title, "Title")
                .required()
                .with_value(title),
            FormField::picker(FormFieldId::Iteration, "Iteration", FormPicker::Iteration)
                .with_value(self.current_iteration().unwrap_or_default()),
            FormField::picker(FormFieldId::Assignee, "Assignee", FormPicker::Assignee)
                .with_value(shell.me().unwrap_or_default()),
            FormField::text(FormFieldId::Tags, "Tags").with_value(CAPTURE_TAG),
        ];
        self.capture.clear();
        self.form = Some(FormOverlay::new(
            FormKind::QuickCapture,
            "Quick capture",
            fields,
        ));
        self.submit_form(shell)
    }
}
