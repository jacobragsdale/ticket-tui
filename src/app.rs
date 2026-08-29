use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;
use serde_json::Value;

use crate::agent_context::{
    AgentContext, PendingEditContext, SearchContext, SortContext, SyncContext, TicketContext,
    TicketReference, TicketsContext,
};
use crate::classification::{self, ClassificationNode, NodeKind};
use crate::columns::TableLayout;
use crate::command::{
    Command, CommandId, EDIT_MENU, EditMenuEntry, REMOVE_PARENT_ROW, command_for_key,
    matching_commands,
};
pub use crate::edit::FieldEdit;
use crate::edit::{EditApplied, EditRejection, EditRequest, normalize_tags};
use crate::export;
pub use crate::filter::FacetTarget;
use crate::filter::{
    FacetValue, FilterField, FilterToken, MatchContext, ParsedQuery, days_untouched, facet_values,
    format_query, is_stale, parse_query, stale_query,
};
use crate::model::{
    CommentRecord, DetailsUpdate, FamilySnapshot, FamilyTreeEntry, HistoryRecord, Identity,
    RelationKind, RelationRecord, SortDirection, SortField, StateCatalog, StateCategory,
    StateOption, Ticket, TicketGraph, TicketKey, compare_tickets, path_leaf,
};
pub use crate::model::{RowDensity, SearchOrder};
use crate::pointer::{
    self, DragKind, PointerState, ScrollState, ScrollSurface, SelectableSurface, TextEditor,
    TextPos, TextSelection,
};
pub use crate::pointer::{EditableField, HitRegions, OverlayAnchor, PointerTarget};
use crate::search::{SearchDocuments, SearchEngine, SearchMatch};
use crate::session::{self, NamedView, Session};
use crate::sync::{ReparentApplied, ReparentRejection};
use crate::text_input::TextInput;
use crate::timestamp::Timestamp;

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
    Facets,
    /// The list of field editors `e` opens.
    Edit,
    /// The states the selected work item can be moved to.
    StatePicker,
    /// The priorities the selected work item can be given, `Clear` included.
    PriorityPicker,
    /// A single-line field editor, for the Title and Tags rows of the Edit menu.
    Prompt,
    /// The people the selected work item can be assigned to, filtered by typing.
    AssigneePicker,
    /// The iteration or area tree the selected work item can be moved into,
    /// filtered by typing. Which of the two is on [`NodePicker::kind`].
    NodePicker,
    /// A multi-field form, such as the one `n` opens to file a new work item.
    Form,
    /// The work item types a form's Type field can name.
    TypePicker,
    /// The work items the selected one can be filed under, filtered by typing.
    ParentPicker,
}

/// How long a work item may sit untouched before the Changed column flags it,
/// when neither a flag, a variable, nor the session says otherwise.
pub const DEFAULT_STALE_DAYS: u16 = 14;

/// The thresholds the palette's **Set stale threshold** steps through, which
/// is how the setting is changed without a number to type: a sprint, a
/// fortnight, three weeks, a month.
pub const STALE_DAY_CHOICES: [u16; 4] = [7, 14, 21, 30];

/// A threshold of zero days would flag every open work item the moment it was
/// touched, which is not a threshold at all, so one day is the floor.
const MIN_STALE_DAYS: u16 = 1;

/// Percentage of the workspace given to the tickets pane when the panes sit
/// side by side, and when they are stacked.
pub const DEFAULT_PANE_SPLIT_WIDE: u16 = 62;
pub const DEFAULT_PANE_SPLIT_STACKED: u16 = 56;
/// Safety rails for a stored or dragged split, applied on top of the cell
/// minimums below.
const MIN_SPLIT_PERCENT: u16 = 20;
const MAX_SPLIT_PERCENT: u16 = 80;
/// Cells each pane keeps while the divider is dragged.
const MIN_TICKETS_COLUMNS: u16 = 40;
const MIN_DETAILS_COLUMNS: u16 = 30;
const MIN_PANE_ROWS: u16 = 6;

/// Which way the draggable pane divider runs in the current layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DividerOrientation {
    /// A column between the tickets and details panes (wide layout).
    Vertical,
    /// A row between the stacked tickets and details panes.
    Horizontal,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Focus {
    #[default]
    Tickets,
    Family,
    Details,
}

impl Focus {
    #[must_use]
    pub const fn is_details_pane(self) -> bool {
        matches!(self, Self::Family | Self::Details)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AppAction {
    None,
    Sync,
    /// Write one field back to Azure DevOps, one request per work item. An
    /// ordinary edit carries a single request; a bulk change over the checked
    /// rows carries one for each of them, and the worker takes them in the
    /// order they are listed.
    Edit(Vec<EditRequest>),
    /// Read the project's team members, so the assignee picker can offer
    /// somebody with no work item in the database yet. Asked for once a
    /// session, when that picker first opens; the picker does not wait on it.
    FetchIdentities,
    /// Read the project's iteration and area trees, so both node pickers can
    /// offer a sprint no work item sits in yet. Asked for once a session, when
    /// either picker first opens on a cache that is empty or stale; the picker
    /// does not wait on it.
    FetchClassificationNodes,
    /// Read the work item types the project's process offers, for a form's
    /// Type field. Asked for once a session, when the first form opens; the
    /// form does not wait on it.
    FetchWorkItemTypes,
    /// Add one work item to the project. `patch` sets its fields and nothing
    /// else — the parent travels as a link the client appends — and, like a
    /// comment, nothing is shown until Azure DevOps has stored it: a work item
    /// has no id, revision, or URL until the server gives it one.
    Create {
        work_item_type: String,
        patch: Vec<Value>,
        parent: Option<i64>,
    },
    /// Move one work item under a different parent, or out from under the one
    /// it has when `new_parent` is `None`. The graph already carries the move,
    /// so a refusal puts both halves of the old link back.
    Reparent {
        key: TicketKey,
        new_parent: Option<i64>,
    },
    /// Leave one comment on one work item. Nothing appears on the work item
    /// until Azure DevOps has stored it, so this is the one write the table
    /// does not make optimistically.
    Comment {
        key: TicketKey,
        text: String,
    },
    /// Hand one work item's description to the user's editor. It carries the
    /// markup Azure DevOps stores, because that is what the editor is opened
    /// on and what an edit has to hand back. This is the one action that takes
    /// the terminal away from the TUI while it runs.
    EditDescription {
        key: TicketKey,
        html: String,
    },
    OpenUrl(String),
    Copy {
        text: String,
        content: CopiedContent,
    },
    WriteFile {
        path: PathBuf,
        contents: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopiedContent {
    Text,
    Id,
    Url,
    Title,
    MarkdownLink,
    Summary,
}

impl CopiedContent {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Id => "id",
            Self::Url => "url",
            Self::Title => "title",
            Self::MarkdownLink => "markdown link",
            Self::Summary => "summary",
        }
    }
}

#[derive(Debug)]
pub struct PointerUpdate {
    pub action: AppAction,
    pub redraw: bool,
}

impl PointerUpdate {
    fn none(redraw: bool) -> Self {
        Self {
            action: AppAction::None,
            redraw,
        }
    }

    fn action(action: AppAction) -> Self {
        Self {
            action,
            redraw: true,
        }
    }
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

/// The Edit menu's cursor. The entries themselves are [`EDIT_MENU`].
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

/// The state picker, built when it opens so it never reads the network.
#[derive(Clone, Debug, Default)]
pub struct StatePicker {
    /// Every state the selected work item's type allows.
    pub options: Vec<StateOption>,
    pub index: usize,
    pub scroll: ScrollState,
    /// The state the work item is already in, which `Enter` treats as a no-op.
    pub current: String,
    /// What the picker was opened over, shown in its title.
    pub scope: EditScope,
}

/// The priorities the picker offers, in the order it lists them. `None` is the
/// `Clear` row, which takes the field off the work item rather than writing an
/// empty value.
pub const PRIORITY_CHOICES: [Option<i64>; 5] = [Some(1), Some(2), Some(3), Some(4), None];

/// The priority picker, built when it opens from the row it was opened on.
#[derive(Clone, Debug, Default)]
pub struct PriorityPicker {
    pub index: usize,
    pub scroll: ScrollState,
    /// The priority the work item already has, which `Enter` treats as a no-op.
    pub current: Option<i64>,
    /// The work item the picker was opened for, shown in its title.
    pub id: i64,
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

/// What the assignee picker calls nobody at all, and the row that unassigns.
pub const UNASSIGNED_LABEL: &str = "Unassigned";

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
            Some(name) => !self.unassigned && same_name(&self.display, name),
            None => self.unassigned,
        }
    }
}

/// The assignee picker: everybody worth offering, filtered by whatever has been
/// typed. Built when it opens, so it never waits for the network.
#[derive(Clone, Debug, Default)]
pub struct AssigneePicker {
    /// Every candidate, in the order they were gathered.
    pub candidates: Vec<AssigneeCandidate>,
    pub query: TextInput,
    /// The cursor, counted over the candidates the query left showing.
    pub index: usize,
    pub scroll: ScrollState,
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
    /// Every work item that could be the parent, in table order.
    pub candidates: Vec<ParentCandidate>,
    pub query: TextInput,
    /// The cursor, counted over the candidates the query left showing.
    pub index: usize,
    pub scroll: ScrollState,
    /// The work item being moved, which is what the picker's title names.
    pub child: TicketKey,
    /// The parent it hangs under now, which `Enter` treats as a no-op.
    pub current: Option<TicketKey>,
}

impl Default for ParentPicker {
    /// A picker nobody has opened yet, over no work item: id `0`, the same
    /// stand-in [`EditScope::default`] uses for a scope nothing has been
    /// chosen for. [`App::open_parent_picker`] fills it in before it is read.
    fn default() -> Self {
        Self {
            candidates: Vec::new(),
            query: TextInput::default(),
            index: 0,
            scroll: ScrollState::default(),
            child: TicketKey {
                organization: String::new(),
                id: 0,
            },
            current: None,
        }
    }
}

/// How long a cached copy of the classification trees is trusted before either
/// picker asks Azure DevOps for them again. Sprints are added a few times a
/// year, so an hour is generous and still keeps a long-running session honest.
const CLASSIFICATION_MAX_AGE_SECONDS: i64 = 3600;

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
    /// Which tree is open, which is also the field a choice is written to.
    pub kind: NodeKind,
    /// Every row, in tree order.
    pub rows: Vec<NodeRow>,
    pub query: TextInput,
    /// The cursor, counted over the rows the query left showing.
    pub index: usize,
    pub scroll: ScrollState,
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
            index: 0,
            scroll: ScrollState::default(),
            current: String::new(),
            scope: EditScope::default(),
        }
    }
}

/// Which form is open. A form Esc closed is kept under this, so `n` brings back
/// what was typed and a form opened over a different work item never inherits
/// somebody else's draft.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormKind {
    /// The new work item form `n` opens, hanging under nothing in particular.
    NewWorkItem,
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
    Assignee,
    Priority,
    Tags,
}

/// The picker one form field opens, which is the same picker the Edit menu
/// opens over a work item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormPicker {
    WorkItemType,
    Iteration,
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

    #[must_use]
    pub fn value(&self) -> &str {
        self.input.text()
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

/// The work item type picker: every type the project's process offers, built
/// when it opens so it never waits for the network.
#[derive(Clone, Debug, Default)]
pub struct TypePicker {
    pub options: Vec<String>,
    pub index: usize,
    pub scroll: ScrollState,
    /// The type the form already names, which the picker marks.
    pub current: String,
    /// The form field the choice is written back into.
    pub field: FormFieldId,
}

/// What a new work item is filed as unless the Type field says otherwise,
/// which is what the Basic process calls its everyday unit of work.
pub const DEFAULT_WORK_ITEM_TYPE: &str = "Issue";

/// What one form field holding a whole number says, or a refusal naming it. An
/// empty field is no number at all rather than a bad one: both the parent and
/// the priority are optional.
fn form_number(form: &FormOverlay, id: FormFieldId) -> Result<Option<i64>, String> {
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

/// Azure DevOps echoes display names back with inconsistent casing and spacing,
/// so two names are the same person when they are the same after both.
#[must_use]
fn same_name(left: &str, right: &str) -> bool {
    left.trim().to_lowercase() == right.trim().to_lowercase()
}

/// Whether one of the people already gathered is this one, so nobody is
/// offered twice under a different spelling.
#[must_use]
fn names_someone_listed(candidates: &[AssigneeCandidate], name: &str) -> bool {
    candidates
        .iter()
        .any(|candidate| !candidate.unassigned && same_name(&candidate.display, name))
}

/// Whether every character typed appears in `haystack` in that order, ignoring
/// case: `jr` finds `Jacob Ragsdale`, and so does `ragsd`.
#[must_use]
fn fuzzy_contains(haystack: &str, query: &str) -> bool {
    let mut remaining = haystack.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| remaining.any(|found| found == wanted))
}

/// A single-line field editor, prefilled with what the work item says now. The
/// Title and Tags rows of the Edit menu both open one.
#[derive(Clone, Debug)]
pub struct TextPrompt {
    pub field: PromptField,
    pub input: TextInput,
    /// The work item the prompt was opened for, shown in its title.
    pub id: i64,
    /// The text the prompt opened with; saving that back writes nothing.
    pub original: String,
}

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

/// The five built-in views, in the order the overlay lists them. All but
/// `Stale` take the table's default order, newest change first; `Stale` turns
/// it around, so the item nobody has touched for longest is the first row.
pub const BUILTIN_VIEWS: [BuiltinView; 5] = [
    BuiltinView {
        name: "Mine",
        query: "assignee:@me",
        sort_field: SortField::Changed,
        sort_direction: SortDirection::Descending,
    },
    BuiltinView {
        name: "Unassigned",
        query: "assignee:@none",
        sort_field: SortField::Changed,
        sort_direction: SortDirection::Descending,
    },
    BuiltinView {
        name: "Doing",
        query: "state:doing",
        sort_field: SortField::Changed,
        sort_direction: SortDirection::Descending,
    },
    BuiltinView {
        name: "Stale",
        query: "changed:>14d state:@open",
        sort_field: SortField::Changed,
        sort_direction: SortDirection::Ascending,
    },
    BuiltinView {
        name: "Current sprint",
        query: "iteration:@current",
        sort_field: SortField::Changed,
        sort_direction: SortDirection::Descending,
    },
];

/// The built-in a name belongs to. A built-in owns its name: one cannot be
/// saved over, and a stored view carrying the name — from a session written
/// before the built-ins existed — is dropped on load rather than listed a
/// second time under the same heading.
#[must_use]
fn builtin_named(name: &str) -> Option<&'static BuiltinView> {
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

/// An edit waiting on Azure DevOps. `original` is the row as it was before the
/// change, restored if the write is refused; applying `edit` to it gives back
/// the optimistic copy the table is showing, which is how a pull that lands
/// first is topped up again.
#[derive(Clone, Debug)]
struct PendingEdit {
    original: Ticket,
    edit: FieldEdit,
    /// When the edit was dispatched, so the agent context can say how long it
    /// has been in flight.
    since: Timestamp,
    /// What this edit means for the undo stack once it lands.
    undo: UndoRole,
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
enum UndoRole {
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

/// How many edits back `u` can go in one session. Twenty is far more than a
/// mis-click needs, and short enough that the stack never becomes a memory of
/// the session in its own right.
const UNDO_DEPTH: usize = 20;

/// One work item on the undo stack: the change that puts its field back the
/// way it was before the edit that is being taken back.
#[derive(Clone, Debug)]
struct UndoStep {
    key: TicketKey,
    edit: FieldEdit,
}

/// Everything one press of `u` takes back. An ordinary edit is one work item;
/// a bulk change over the checked rows is all of them under a single entry, so
/// `u` puts the whole change back rather than unpicking it a row at a time.
#[derive(Clone, Debug)]
struct UndoEntry {
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

/// How many refused work items a bulk change names before it counts the rest.
/// Three is enough to act on and short enough to read in one notification.
const NAMED_BULK_FAILURES: usize = 3;

/// How a change of several work items reads once the last answer is in. A
/// bulk change counts what landed; an undo of one says what it took back
/// instead, which the count would only get in the way of.
#[derive(Clone, Debug)]
enum BulkHeadline {
    /// The change as a notification says it, such as `State → Doing`, with the
    /// tally put in front of it.
    Changed(String),
    /// The whole line an undo says for itself.
    Undone(String),
}

impl BulkHeadline {
    /// The line for a change every work item took.
    fn all_landed(&self, updated: usize) -> String {
        match self {
            Self::Changed(summary) => format!("Updated {updated} tickets · {summary}"),
            Self::Undone(line) => line.clone(),
        }
    }

    /// What the tally of a change that did not land everywhere leads with.
    const fn verb(&self) -> &'static str {
        match self {
            Self::Changed(_) => "Updated",
            Self::Undone(_) => "Undid",
        }
    }
}

/// One change asked of several work items at once, and what has come back of
/// it. Each edit is its own request with its own revision test, so they land
/// one at a time; this counts the answers so the change speaks once, when the
/// last of them is in, rather than once a row. An undo of a bulk change is
/// gathered the same way, so it too is never left half done in silence.
#[derive(Clone, Debug)]
struct BulkEdit {
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

/// How many cells wide the details pane draws the bar beside the ratio.
pub const PROGRESS_BAR_CELLS: usize = 6;

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
    fn compare(&self, left: &TicketKey, right: &TicketKey, direction: SortDirection) -> Ordering {
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

#[derive(Debug)]
pub struct PreparedTickets {
    tickets: Vec<Ticket>,
    search_documents: SearchDocuments,
    graph: TicketGraph,
    /// The states each work item type allows, empty until a sync cached them.
    states: StateCatalog,
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
            states: StateCatalog::default(),
        }
    }

    /// The cached work item type states that came out of the same database
    /// read, so the state picker and the rows never disagree.
    #[must_use]
    pub fn with_states(mut self, states: StateCatalog) -> Self {
        self.states = states;
        self
    }

    #[must_use]
    pub fn ticket_count(&self) -> usize {
        self.tickets.len()
    }

    /// The work item type states read alongside these rows.
    #[must_use]
    pub const fn states(&self) -> &StateCatalog {
        &self.states
    }
}

pub struct App {
    tickets: Arc<Vec<Ticket>>,
    visible: Vec<SearchMatch>,
    search: SearchEngine,
    search_generation: u64,
    pending_selection: Option<TicketKey>,
    pub search_pending: bool,
    query: TextInput,
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
    pub table: ScrollState,
    pub details: ScrollState,
    /// Which row of the details pane's scrolling content the family tree's
    /// first row was last drawn on. The heading above it wraps, so only the
    /// renderer knows where the tree starts, and the family cursor needs it to
    /// scroll itself back into view.
    pub details_family_row: usize,
    pub family_cursor: Option<TicketKey>,
    pub help: ScrollState,
    pub sort: ScrollState,
    pub narrow_details: bool,
    pub pane_split_wide: u16,
    pub pane_split_stacked: u16,
    /// The remembered stale threshold: what the palette last set, and what the
    /// session file carries between runs.
    stale_days: u16,
    /// The threshold this run was started under, from `--stale-days` or
    /// `TICKET_TUI_STALE_DAYS`. It stands over the remembered value until the
    /// palette moves the setting, and is never written back to the session: a
    /// flag passed once should not quietly become the setting.
    stale_days_override: Option<u16>,
    content_area: Rect,
    divider: Option<DividerOrientation>,
    pub reload_pending: bool,
    pub should_quit: bool,
    pub session_dirty: bool,
    notification: Option<Notification>,
    pub sort_draft: SortDraft,
    pub hit_regions: HitRegions,
    pub pointer: PointerState,
    pub filter_overlay: FilterOverlay,
    pub column_overlay: ColumnOverlay,
    pub palette: PaletteState,
    pub views_overlay: ViewsOverlay,
    pub facet_bar: FacetBar,
    pub edit_menu: EditMenu,
    pub state_picker: StatePicker,
    pub priority_picker: PriorityPicker,
    pub assignee_picker: AssigneePicker,
    pub parent_picker: ParentPicker,
    pub node_picker: NodePicker,
    pub type_picker: TypePicker,
    /// The open multi-field form, if there is one.
    pub form: Option<FormOverlay>,
    /// How far the open form's field list is scrolled, kept beside every other
    /// surface's offset rather than inside the widget.
    pub form_scroll: ScrollState,
    /// The last form `Esc` closed, kept whole so reopening it brings back every
    /// field and the cursor with them. It lives in memory for the session only:
    /// the session file records how the table is arranged, not a half-typed
    /// work item.
    form_draft: Option<FormOverlay>,
    /// The form a create is out on. It is held rather than dropped so a refusal
    /// can put it back with everything still in it, and it is what stops a
    /// second create being sent on top of the first.
    pending_create: Option<FormOverlay>,
    /// The open single-line field editor, if there is one.
    pub prompt: Option<TextPrompt>,
    /// Where the open picker or prompt is drawn: centred, as every
    /// keyboard-opened one is, or hung off the details-pane field that was
    /// clicked to open it.
    pub overlay_anchor: OverlayAnchor,
    bookmarks: HashSet<TicketKey>,
    selected_keys: HashSet<TicketKey>,
    recent: Vec<TicketKey>,
    future: Vec<TicketKey>,
    views: Vec<NamedView>,
    pub active_view: Option<String>,
    graph: TicketGraph,
    /// Done out of total over each parent's direct children, rebuilt whenever
    /// the rows or the graph move rather than counted again every frame.
    child_progress: ChildProgressIndex,
    /// The states Azure DevOps allows for each work item type. Empty until a
    /// sync has fetched them, which is what [`App::states_for`] falls back for.
    state_catalog: StateCatalog,
    pub loaded_at: Instant,
    pub database_path: PathBuf,
    pub stale: bool,
    pub data_signature: u128,
    /// Whether a pull from Azure DevOps is in flight.
    pub sync_pending: bool,
    /// The work item whose comments and history are being read, if one is.
    /// The details pane says so where that history is about to appear.
    pub details_pending: Option<TicketKey>,
    /// Edits sent to Azure DevOps and not answered yet, keyed by work item.
    pending_edits: HashMap<TicketKey, PendingEdit>,
    /// The moves waiting on Azure DevOps, each remembering the parent the work
    /// item hung under before it was made. A refusal puts that parent back.
    pending_reparents: HashMap<TicketKey, Option<TicketKey>>,
    /// Bulk changes with answers still to come, newest last. There is normally
    /// at most one, but a second started before the first has finished is
    /// counted on its own rather than taking the first one's place.
    bulk_edits: Vec<BulkEdit>,
    /// The edits this session has landed, oldest first, each one ready to be
    /// put back by `u`. Capped at [`UNDO_DEPTH`]; it is not written anywhere,
    /// so it starts empty every run.
    undo_stack: Vec<UndoEntry>,
    /// How many dispatches this session has made, which is where an undo entry
    /// gets the number that gathers a bulk change's work items into one.
    undo_groups: u64,
    /// Work items with a comment posted and not answered yet. A comment is not
    /// optimistic, so this is only what stops a second one being typed on top
    /// of the first.
    pending_comments: HashSet<TicketKey>,
    /// Why there is nothing to write to, reported when an edit is attempted
    /// without a configured Azure DevOps project.
    offline_reason: Option<String>,
    /// Whether Azure DevOps is configured at all: an offline run browses the
    /// database and reports no sync state.
    sync_enabled: bool,
    /// Where the rows come from — the organization and project, how often they
    /// are pulled, and the scope narrowing them — as the database overlay
    /// reports it. `None` until the run resolves a project.
    sync_source: Option<String>,
    /// The same project, as the agent context publishes it. `None` until the
    /// run resolves one.
    sync_target: Option<SyncTarget>,
    /// When the last successful pull finished, which is not `loaded_at`: a
    /// SQLite reload moves that too.
    synced_at: Option<Instant>,
    /// The same moment on the wall clock, because the agent context has to say
    /// when the last pull landed and an `Instant` only says how long ago.
    synced_wall_clock: Option<Timestamp>,
    /// The last pull's error, kept so the same timer failure is reported once.
    sync_error: Option<String>,
    /// When the next pull may go out, for a timer Azure DevOps asked to hold
    /// off. Not a failure: the title counts it down instead of saying the sync
    /// broke, and nothing is announced.
    sync_paused_until: Option<Instant>,
    /// Display name of the signed-in Azure DevOps user, so their own work
    /// items can stand out. `None` until a sync records one.
    me: Option<String>,
    /// The people the project's teams hold, as the last identity fetch cached
    /// them. The assignee picker offers these alongside everybody the rows
    /// already name, and reads their addresses out of here.
    identities: Vec<Identity>,
    /// Whether the team members have been asked for this session, so opening
    /// the picker a second time costs nothing.
    identities_requested: bool,
    /// The project's iteration and area trees as the last fetch flattened them,
    /// read out of the database at startup. Both node pickers are built from
    /// these, and `current_iteration` reads the sprint out of them.
    classification_nodes: Vec<ClassificationNode>,
    /// When those trees were last read from Azure DevOps, so a picker opening
    /// on a fresh cache asks for nothing at all.
    classification_fetched_at: Option<Timestamp>,
    /// Whether the trees have been asked for this session, so opening either
    /// picker a second time costs nothing.
    classification_requested: bool,
    /// The work item types the project's process offers, as the last fetch
    /// cached them, read out of the database at startup. A form's Type field is
    /// built from these.
    work_item_types: Vec<String>,
    /// Whether the types have been asked for this session, so opening a second
    /// form costs nothing.
    work_item_types_requested: bool,
}

/// Which editor a clicked details-pane field opens. Every one of them is a
/// command already, so a click and the Edit menu reach the same code.
#[must_use]
const fn command_for_field(field: EditableField) -> CommandId {
    match field {
        EditableField::Title => CommandId::EditTitle,
        EditableField::State => CommandId::ChangeState,
        EditableField::Assignee => CommandId::EditAssignee,
        EditableField::Priority => CommandId::EditPriority,
        EditableField::Tags => CommandId::EditTags,
        EditableField::Iteration => CommandId::EditIteration,
        EditableField::Area => CommandId::EditArea,
    }
}

/// Compact wording for a wait still to come, coarse on purpose: the exact
/// second the timer comes back is nobody's business, and a title that ticks
/// every second is a title that has to be redrawn every second.
fn remaining_wait(left: Duration) -> String {
    // Rounded up, so a two minute pause read a millisecond after it started
    // still says two minutes rather than counting down from one.
    let seconds = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m", seconds.div_ceil(60))
    }
}

/// Compact relative wording shared by the freshness and sync labels.
fn relative_age(age: Duration) -> String {
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
            query: TextInput::default(),
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
            table: ScrollState::default(),
            details: ScrollState::default(),
            details_family_row: 0,
            family_cursor: None,
            help: ScrollState::default(),
            sort: ScrollState::default(),
            narrow_details: false,
            pane_split_wide: DEFAULT_PANE_SPLIT_WIDE,
            pane_split_stacked: DEFAULT_PANE_SPLIT_STACKED,
            stale_days: DEFAULT_STALE_DAYS,
            stale_days_override: None,
            content_area: Rect::ZERO,
            divider: None,
            reload_pending: false,
            should_quit: false,
            session_dirty: false,
            notification: None,
            sort_draft: SortDraft {
                field_index: 0,
                direction: SortDirection::Descending,
            },
            hit_regions: HitRegions::default(),
            pointer: PointerState::default(),
            filter_overlay: FilterOverlay::default(),
            column_overlay: ColumnOverlay::default(),
            palette: PaletteState::default(),
            views_overlay: ViewsOverlay::default(),
            facet_bar: FacetBar::default(),
            edit_menu: EditMenu::default(),
            state_picker: StatePicker::default(),
            priority_picker: PriorityPicker::default(),
            assignee_picker: AssigneePicker::default(),
            parent_picker: ParentPicker::default(),
            node_picker: NodePicker::default(),
            prompt: None,
            overlay_anchor: OverlayAnchor::Centered,
            bookmarks: HashSet::new(),
            selected_keys: HashSet::new(),
            recent: Vec::new(),
            future: Vec::new(),
            views: Vec::new(),
            active_view: None,
            graph: prepared.graph,
            child_progress: ChildProgressIndex::default(),
            state_catalog: prepared.states,
            loaded_at: Instant::now(),
            database_path: PathBuf::new(),
            stale: false,
            data_signature: 0,
            sync_pending: false,
            details_pending: None,
            pending_edits: HashMap::new(),
            pending_reparents: HashMap::new(),
            bulk_edits: Vec::new(),
            undo_stack: Vec::new(),
            undo_groups: 0,
            pending_comments: HashSet::new(),
            type_picker: TypePicker::default(),
            form: None,
            form_scroll: ScrollState::default(),
            form_draft: None,
            pending_create: None,
            work_item_types: Vec::new(),
            work_item_types_requested: false,
            offline_reason: None,
            sync_enabled: false,
            sync_source: None,
            sync_target: None,
            synced_at: None,
            synced_wall_clock: None,
            sync_error: None,
            sync_paused_until: None,
            me: None,
            identities: Vec::new(),
            identities_requested: false,
            classification_nodes: Vec::new(),
            classification_fetched_at: None,
            classification_requested: false,
        };
        app.refresh_child_progress();
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
    pub fn agent_context(&self) -> AgentContext {
        let parsed = self.parsed_query();
        let visible_rows = self
            .visible_tickets()
            .skip(self.table.offset)
            .take(self.table.viewport)
            .map(|ticket| self.ticket_context(ticket))
            .collect();
        let checked_tickets = self
            .tickets()
            .iter()
            .filter(|ticket| self.selected_keys.contains(&ticket.key))
            .map(|ticket| self.ticket_context(ticket))
            .collect();
        AgentContext {
            database_path: self.database_path.display().to_string(),
            me: self.me.clone(),
            sync: SyncContext {
                organization: self
                    .sync_target
                    .as_ref()
                    .map(|target| target.organization.clone()),
                project: self
                    .sync_target
                    .as_ref()
                    .map(|target| target.project.clone()),
                refresh_seconds: self
                    .sync_target
                    .as_ref()
                    .map_or(0, |target| target.refresh_seconds),
                in_progress: self.sync_pending,
                last_success_at: self.synced_wall_clock.map(Timestamp::to_rfc3339),
                last_error: self.sync_error.clone(),
                offline: !self.sync_enabled,
            },
            pending_edits: self.pending_edit_contexts(),
            mode: mode_name(self.mode).into(),
            focus: focus_name(self.focus).into(),
            screen: if self.narrow_details {
                "details"
            } else {
                "workspace"
            }
            .into(),
            active_view: self.active_view.clone(),
            search: SearchContext {
                query: self.query.text().to_owned(),
                fuzzy_text: parsed.fuzzy,
                filters: parsed
                    .filters
                    .tokens()
                    .into_iter()
                    .map(|token| token.chip_label())
                    .collect(),
                pending: self.search_pending,
                order: self.search_order,
            },
            sort: SortContext {
                field: self.sort_field,
                direction: self.sort_direction,
                row_density: self.row_density,
            },
            tickets: TicketsContext {
                total_count: self.tickets.len(),
                matching_count: self.visible.len(),
                viewport_start: self.table.offset,
                viewport_size: self.table.viewport,
                visible_rows,
            },
            selected_ticket: self
                .selected_ticket()
                .map(|ticket| self.ticket_context(ticket)),
            checked_tickets,
            family_cursor: self.family_cursor.as_ref().map(|key| TicketReference {
                organization: key.organization.clone(),
                id: key.id,
            }),
            details_scroll_line: u16::try_from(self.details.offset).unwrap_or(u16::MAX),
        }
    }

    /// The edits still waiting on Azure DevOps, lowest work item first. Sorted
    /// rather than taken in map order, because the context file is only
    /// rewritten when it changes and a reshuffled list would look like a change
    /// on every render.
    fn pending_edit_contexts(&self) -> Vec<PendingEditContext> {
        let mut edits: Vec<PendingEditContext> = self
            .pending_edits
            .iter()
            .map(|(key, pending)| PendingEditContext {
                id: key.id,
                field: pending.edit.label().to_owned(),
                value: pending.edit.value_text(),
                since: pending.since.to_rfc3339(),
            })
            .collect();
        edits.sort_by(|left, right| {
            left.id
                .cmp(&right.id)
                .then_with(|| left.field.cmp(&right.field))
        });
        edits
    }

    fn ticket_context(&self, ticket: &Ticket) -> TicketContext {
        TicketContext {
            organization: ticket.key.organization.clone(),
            project: ticket.project.clone(),
            id: ticket.key.id,
            work_item_type: ticket.work_item_type.clone(),
            title: ticket.title.clone(),
            state: ticket.state.clone(),
            assigned_to: ticket.assigned_to.clone(),
            priority: ticket.priority,
            tags: ticket.tags.clone(),
            web_url: ticket.web_url.clone(),
            bookmarked: self.bookmarks.contains(&ticket.key),
            checked: self.selected_keys.contains(&ticket.key),
        }
    }

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

    /// What the query's sentinels stand for right now: who is signed in and
    /// which sprint contains today, beside the clock its relative date bounds
    /// are measured back from. Built fresh for every pass over the rows, so a
    /// saved `assignee:@me` follows the name and `iteration:@current` follows
    /// the sprint rather than whatever either was when the view was written.
    #[must_use]
    pub fn match_context(&self) -> MatchContext {
        MatchContext::now()
            .with_me(self.me.clone())
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
        let filters = self.parsed_query().filters;
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
    pub fn is_bookmarked(&self, key: &TicketKey) -> bool {
        self.bookmarks.contains(key)
    }

    #[must_use]
    pub fn is_row_selected(&self, key: &TicketKey) -> bool {
        self.selected_keys.contains(key)
    }

    pub fn set_me(&mut self, me: Option<String>) {
        self.me = me;
    }

    #[must_use]
    pub fn me(&self) -> Option<&str> {
        self.me.as_deref()
    }

    /// Whether a work item is assigned to the signed-in user. Azure DevOps
    /// echoes display names back with inconsistent casing, so compare loosely.
    #[must_use]
    pub fn is_mine(&self, ticket: &Ticket) -> bool {
        match (self.me.as_deref(), ticket.assigned_to.as_deref()) {
            (Some(me), Some(assignee)) => me
                .trim()
                .chars()
                .flat_map(char::to_lowercase)
                .eq(assignee.trim().chars().flat_map(char::to_lowercase)),
            _ => false,
        }
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

    /// The threshold the session file carries, which is not the one in force
    /// while a flag or a variable overrides it.
    #[must_use]
    pub const fn remembered_stale_days(&self) -> u16 {
        self.stale_days
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
    pub fn merge_identities(&mut self, identities: Vec<Identity>) {
        if identities.is_empty() {
            return;
        }
        for identity in identities {
            match self
                .identities
                .iter_mut()
                .find(|known| same_name(&known.display_name, &identity.display_name))
            {
                Some(known) if known.unique_name.is_none() => {
                    known.unique_name = identity.unique_name;
                }
                Some(_) => {}
                None => self.identities.push(identity),
            }
        }
        if self.mode != AppMode::AssigneePicker {
            return;
        }
        let focused = self
            .assignee_matches()
            .get(self.assignee_picker.index)
            .map(|candidate| candidate.display.clone());
        self.assignee_picker.candidates = self.assignee_candidates();
        let matches = self.assignee_matches();
        let index = focused
            .and_then(|display| {
                matches
                    .iter()
                    .position(|candidate| candidate.display == display)
            })
            .unwrap_or(self.assignee_picker.index)
            .min(matches.len().saturating_sub(1));
        self.focus_assignee(index);
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

    #[must_use]
    pub fn palette_commands(&self) -> Vec<Command> {
        matching_commands(self.palette.query.text())
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
        facet_values(
            self.tickets(),
            &filters,
            self.facet_field(),
            |ticket| self.bookmarks.contains(&ticket.key),
            &self.match_context(),
        )
    }

    pub fn configure_database(&mut self, path: PathBuf, signature: u128) {
        self.database_path = path;
        self.data_signature = signature;
        self.loaded_at = Instant::now();
        self.stale = false;
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

    pub fn set_workspace_graph(&mut self, graph: crate::model::TicketGraph) {
        self.graph = graph;
        self.refresh_child_progress();
        self.sync_family_state();
    }

    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    #[must_use]
    pub fn freshness_label(&self) -> String {
        relative_age(self.loaded_at.elapsed())
    }

    /// Turns on the sync parts of the UI. An offline run leaves them off, so
    /// the table title says nothing about a sync that can not happen.
    pub const fn enable_sync(&mut self) {
        self.sync_enabled = true;
    }

    /// A pull has started.
    pub const fn begin_sync(&mut self) {
        self.sync_pending = true;
    }

    /// A pull succeeded. The tickets it brought are applied separately, so this
    /// only records that Azure DevOps was reached.
    pub fn finish_sync(&mut self) {
        self.sync_pending = false;
        self.sync_error = None;
        self.sync_paused_until = None;
        self.synced_at = Some(Instant::now());
        self.synced_wall_clock = Some(Timestamp::now());
    }

    /// Azure DevOps asked to be left alone until `until`, and the timer agreed.
    /// Nothing is wrong and nothing is announced: this is the pause the title
    /// counts down, and the next success clears it. Deliberately not
    /// [`Self::fail_sync`] — a throttled pull is the service working as
    /// designed, and an error toast a minute would only be noise.
    pub fn pause_sync(&mut self, until: Instant) {
        self.sync_pending = false;
        self.sync_error = None;
        self.sync_paused_until = Some(until);
    }

    /// A pull failed. Reports whether the failure is worth a notification: the
    /// same error on consecutive timer pulls is not, because the table title
    /// already says the sync is failing. `announce` forces one anyway, for a
    /// pull the user asked for.
    pub fn fail_sync(&mut self, error: &str, announce: bool) -> bool {
        self.sync_pending = false;
        self.sync_paused_until = None;
        let repeated = self.sync_error.as_deref() == Some(error);
        self.sync_error = Some(error.to_owned());
        announce || !repeated
    }

    /// What the table title appends after the sort order, most urgent first.
    #[must_use]
    pub fn activity_label(&self) -> Option<String> {
        if self.sync_enabled && self.sync_pending {
            return Some("Syncing…".into());
        }
        if self.reload_pending {
            return Some("Reloading…".into());
        }
        if self.sync_enabled
            && let Some(left) = self.sync_pause_left()
        {
            return Some(format!("Sync paused {}", remaining_wait(left)));
        }
        if self.sync_enabled && self.sync_error.is_some() {
            return Some("Sync failed".into());
        }
        if self.stale {
            return Some("Stale".into());
        }
        self.synced_at
            .filter(|_| self.sync_enabled)
            .map(|at| format!("Synced {}", relative_age(at.elapsed())))
    }

    /// Where the rows are pulled from, as the database overlay reports it.
    pub fn set_sync_source(&mut self, source: Option<String>) {
        self.sync_source = source;
    }

    /// The same project, for the agent context, which needs the organization,
    /// the project, and the interval apart rather than as one line of prose.
    pub fn set_sync_target(&mut self, target: Option<SyncTarget>) {
        self.sync_target = target;
    }

    /// The database overlay's one-line account of the sync: where the rows come
    /// from, and how the last pull went. An offline run says why it is offline
    /// there instead — a missing organization, or a database another project
    /// filled.
    #[must_use]
    pub fn sync_summary(&self) -> String {
        let state = if self.sync_enabled {
            self.sync_state()
        } else {
            self.offline_reason.as_ref().map_or_else(
                || "offline; no Azure DevOps organization configured".to_owned(),
                |reason| format!("offline; {reason}"),
            )
        };
        match &self.sync_source {
            Some(source) => format!("{source} · {state}"),
            None => state,
        }
    }

    /// How the last pull went, for a run that can pull at all.
    fn sync_state(&self) -> String {
        let last = self
            .synced_at
            .map_or_else(|| "not yet".to_owned(), |at| relative_age(at.elapsed()));
        if self.sync_pending {
            format!("in progress, last {last}")
        } else if let Some(left) = self.sync_pause_left() {
            format!(
                "paused for throttling, next in {}, last {last}",
                remaining_wait(left)
            )
        } else if let Some(error) = &self.sync_error {
            format!("failed, last {last}: {error}")
        } else {
            last
        }
    }

    /// How long the throttling pause still has to run, or `None` once it is
    /// over and the timer is free again.
    fn sync_pause_left(&self) -> Option<Duration> {
        let left = self
            .sync_paused_until?
            .saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left)
    }

    #[must_use]
    pub fn ticket_by_key(&self, key: &TicketKey) -> Option<&Ticket> {
        self.tickets.iter().find(|ticket| ticket.key == *key)
    }

    #[must_use]
    pub fn ticket_title(&self, key: &TicketKey) -> Option<&str> {
        self.ticket_by_key(key).map(|ticket| ticket.title.as_str())
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

    pub fn replace_tickets(&mut self, tickets: Vec<Ticket>) {
        self.replace_prepared_tickets(PreparedTickets::new(tickets));
    }

    pub fn replace_prepared_tickets(&mut self, prepared: PreparedTickets) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
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

    /// Asks for one field of the selected work item to be written back to
    /// Azure DevOps. The row carries the change at once, so the table never
    /// waits for the network; the action this returns is what actually sends
    /// it, and a refusal puts the row back. Every edit feature goes this way.
    pub fn edit_selected(&mut self, edit: FieldEdit) -> AppAction {
        let Some(key) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            self.set_error("No work item is selected");
            return AppAction::None;
        };
        self.edit_ticket(&key, edit)
    }

    /// [`Self::edit_selected`] for a work item that is not the selected row.
    pub fn edit_ticket(&mut self, key: &TicketKey, edit: FieldEdit) -> AppAction {
        let label = edit.label().to_owned();
        let undo = UndoRole::Undoable(self.next_undo_group());
        match self.begin_edit(key, edit, undo) {
            Ok(request) => AppAction::Edit(vec![request]),
            Err(reason) => {
                self.set_error(format!("#{} {label} not saved: {reason}", key.id));
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
    pub fn edit_checked(&mut self, edit: FieldEdit) -> AppAction {
        let targets = self.checked_keys();
        if targets.len() < 2 {
            return self.edit_selected(edit);
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
            match self.begin_edit(&key, edit.clone(), UndoRole::Undoable(group)) {
                Ok(request) => requests.push(request),
                Err(reason) => failures.push(format!("#{} failed: {reason}", key.id)),
            }
        }
        let total = requests.len() + failures.len();
        if total == 0 {
            self.set_status(format!("Nothing to change · {}", edit.summary()));
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
            self.set_error(bulk.notification());
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
        key: &TicketKey,
        edit: FieldEdit,
        undo: UndoRole,
    ) -> Result<EditRequest, String> {
        if !self.sync_enabled {
            // Nothing to write to, so the row is left exactly as it is.
            return Err(self
                .offline_reason
                .clone()
                .unwrap_or_else(|| "no Azure DevOps organization is configured".to_owned()));
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
    fn edit_scope(&self) -> EditScope {
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
    pub fn apply_edit(&mut self, applied: EditApplied) {
        let key = applied.ticket.key.clone();
        let pending = self.pending_edits.remove(&key);
        self.graph.replace_relations_from(&key, applied.relations);
        if let Some(index) = self.index_of(&key) {
            self.set_ticket(index, applied.ticket);
            self.resettle_rows();
        }
        let mut landed = format!("Updated #{} · {}", key.id, applied.edit.summary());
        if let Some(PendingEdit { original, undo, .. }) = pending {
            match undo {
                UndoRole::Undoable(group) => self.record_undo(group, &original, &applied.edit),
                UndoRole::Undoing(Some(line)) => landed = line,
                UndoRole::Undoing(None) => {}
            }
        }
        if !self.record_bulk_outcome(&key, None) {
            self.set_status(landed);
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
    pub fn undo_last_edit(&mut self) -> AppAction {
        let Some(entry) = self.undo_stack.pop() else {
            self.set_status("Nothing to undo");
            return AppAction::None;
        };
        let headline = entry.headline();
        let mut requests = Vec::new();
        let mut failures = Vec::new();
        for step in &entry.steps {
            // An undo of one work item says its line as it lands, like any
            // other edit; an undo of several is spoken for by its summary.
            let line = (entry.steps.len() == 1).then(|| headline.clone());
            match self.begin_edit(&step.key, step.edit.clone(), UndoRole::Undoing(line)) {
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
            self.set_error(bulk.notification());
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
    pub fn reject_edit(&mut self, rejection: &EditRejection) {
        if let Some(pending) = self.pending_edits.remove(&rejection.key)
            && let Some(index) = self.index_of(&rejection.key)
        {
            self.set_ticket(index, pending.original);
        }
        if !self.record_bulk_outcome(&rejection.key, Some(rejection.failure())) {
            self.set_error(rejection.notification());
        }
    }

    /// Files one answer against the bulk change that asked for it, and says
    /// whether one did. A work item edited on its own belongs to no bulk
    /// change and speaks for itself; one that belongs to a bulk change stays
    /// quiet until the last of its work items has answered, and then the whole
    /// tally goes up at once.
    fn record_bulk_outcome(&mut self, key: &TicketKey, failure: Option<String>) -> bool {
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
            self.set_error(message);
        } else {
            self.set_status(message);
        }
        true
    }

    /// Why the TUI cannot write anything, told to whoever tries to.
    pub fn set_offline_reason(&mut self, reason: Option<String>) {
        self.offline_reason = reason;
    }

    fn index_of(&self, key: &TicketKey) -> Option<usize> {
        self.tickets.iter().position(|ticket| ticket.key == *key)
    }

    /// Replaces one work item in place, keeping its search document and its
    /// parents' child counts in step so the next query and the next frame both
    /// see the new value.
    fn set_ticket(&mut self, index: usize, ticket: Ticket) {
        Arc::make_mut(&mut self.tickets)[index] = ticket;
        self.search.update_document(index, &self.tickets[index]);
        self.refresh_child_progress();
    }

    /// Counts each parent's children again. Called wherever the rows or the
    /// relations move — a reload, a workspace graph, an edit settling — which
    /// is what keeps an Epic's ratio right as its issues close without any
    /// frame paying for the count.
    fn refresh_child_progress(&mut self) {
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
    fn resettle_rows(&mut self) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.apply_filters();
        self.sort_visible();
        self.restore_selection(selected.as_ref());
    }

    /// Puts the optimistic copies back on top of a pull that finished while an
    /// edit was still in flight, so an edited row does not flicker back to the
    /// value the pull brought. That pulled row becomes what a refusal restores,
    /// because it is the freshest copy the edit did not make.
    fn reapply_pending_edits(&mut self) {
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
    fn edit_query(&mut self, edit: impl FnOnce(&mut TextInput)) {
        let before = self.query.text().to_owned();
        edit(&mut self.query);
        if self.query.text() != before {
            self.after_query_edit();
        }
    }

    fn after_query_edit(&mut self) {
        let selected = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.search_history_index = None;
        self.search_history_draft = self.query.text().to_owned();
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

    pub fn set_table_viewport(&mut self, rows: usize) {
        self.table.set_viewport(rows, self.visible.len());
    }

    /// The scroll bookkeeping for one surface. The table measures its content from
    /// the visible rows, so that length is refreshed on the way out.
    #[must_use]
    pub fn scroll_state(&self, surface: ScrollSurface) -> ScrollState {
        match surface {
            ScrollSurface::Table => ScrollState {
                content: self.visible.len(),
                ..self.table
            },
            ScrollSurface::Details => self.details,
            ScrollSurface::Help => self.help,
            ScrollSurface::Sort => self.sort,
            ScrollSurface::Filter => self.filter_overlay.scroll,
            ScrollSurface::Columns => self.column_overlay.scroll,
            ScrollSurface::Palette => self.palette.scroll,
            ScrollSurface::Views => self.views_overlay.scroll,
            ScrollSurface::FacetMenu => self.facet_bar.scroll,
            ScrollSurface::EditMenu => self.edit_menu.scroll,
            ScrollSurface::StatePicker => self.state_picker.scroll,
            ScrollSurface::PriorityPicker => self.priority_picker.scroll,
            ScrollSurface::AssigneePicker => self.assignee_picker.scroll,
            ScrollSurface::ParentPicker => self.parent_picker.scroll,
            ScrollSurface::NodePicker => self.node_picker.scroll,
            ScrollSurface::TypePicker => self.type_picker.scroll,
            ScrollSurface::Form => self.form_scroll,
        }
    }

    pub fn scroll_state_mut(&mut self, surface: ScrollSurface) -> &mut ScrollState {
        if matches!(surface, ScrollSurface::Table) {
            self.table.content = self.visible.len();
        }
        match surface {
            ScrollSurface::Table => &mut self.table,
            ScrollSurface::Details => &mut self.details,
            ScrollSurface::Help => &mut self.help,
            ScrollSurface::Sort => &mut self.sort,
            ScrollSurface::Filter => &mut self.filter_overlay.scroll,
            ScrollSurface::Columns => &mut self.column_overlay.scroll,
            ScrollSurface::Palette => &mut self.palette.scroll,
            ScrollSurface::Views => &mut self.views_overlay.scroll,
            ScrollSurface::FacetMenu => &mut self.facet_bar.scroll,
            ScrollSurface::EditMenu => &mut self.edit_menu.scroll,
            ScrollSurface::StatePicker => &mut self.state_picker.scroll,
            ScrollSurface::PriorityPicker => &mut self.priority_picker.scroll,
            ScrollSurface::AssigneePicker => &mut self.assignee_picker.scroll,
            ScrollSurface::ParentPicker => &mut self.parent_picker.scroll,
            ScrollSurface::NodePicker => &mut self.node_picker.scroll,
            ScrollSurface::TypePicker => &mut self.type_picker.scroll,
            ScrollSurface::Form => &mut self.form_scroll,
        }
    }

    #[must_use]
    pub fn hovered(&self) -> Option<&PointerTarget> {
        self.pointer.hover.as_ref()
    }

    pub(crate) fn hovered_region(&self) -> Option<&pointer::PointerRegion> {
        let (column, row) = self.pointer.position()?;
        self.hit_regions.resolve(column, row)
    }

    #[must_use]
    pub fn selection(&self) -> Option<TextSelection> {
        self.pointer.selection
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
        // Ctrl-C quits from every mode; other bindings only apply in browse mode.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && command_for_key(key) == Some(CommandId::Quit)
        {
            return self.run_command(CommandId::Quit);
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
            AppMode::Facets => {
                self.handle_facet_key(key);
                AppAction::None
            }
            AppMode::Edit => self.handle_edit_menu_key(key),
            AppMode::StatePicker => self.handle_state_picker_key(key),
            AppMode::PriorityPicker => self.handle_priority_picker_key(key),
            AppMode::Prompt => self.handle_prompt_key(key),
            AppMode::AssigneePicker => self.handle_assignee_picker_key(key),
            AppMode::ParentPicker => self.handle_parent_picker_key(key),
            AppMode::NodePicker => self.handle_node_picker_key(key),
            AppMode::Form => self.handle_form_key(key),
            AppMode::TypePicker => self.handle_type_picker_key(key),
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> PointerUpdate {
        self.pointer.set_position(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp => self.handle_wheel(mouse.column, mouse.row, -3),
            MouseEventKind::ScrollDown => self.handle_wheel(mouse.column, mouse.row, 3),
            MouseEventKind::Down(MouseButton::Left) => self.handle_press(mouse.column, mouse.row),
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved
                if self.pointer.is_pressed() =>
            {
                self.handle_drag(mouse.column, mouse.row)
            }
            MouseEventKind::Moved => self.handle_hover(mouse.column, mouse.row),
            MouseEventKind::Up(MouseButton::Left) => self.handle_release(mouse.column, mouse.row),
            _ => PointerUpdate::none(false),
        }
    }

    pub fn handle_resize(&mut self) {
        self.pointer.clear_selection();
        if matches!(
            self.pointer.drag(),
            DragKind::Text | DragKind::Cancelled | DragKind::Divider
        ) {
            self.pointer.set_drag(DragKind::Cancelled);
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> AppAction {
        // Navigation keys depend on the focused pane; everything else is a command.
        match key.code {
            KeyCode::Char(' ') if self.focus != Focus::Family => self.toggle_row_selection(),
            KeyCode::Tab => self.toggle_focus(),
            KeyCode::Down | KeyCode::Char('j') => self.move_focused(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_focused(-1),
            KeyCode::PageDown => match self.focus {
                Focus::Family => self.move_family_cursor(self.family_page_size()),
                Focus::Tickets | Focus::Details => self.move_focused(10),
            },
            KeyCode::PageUp => match self.focus {
                Focus::Family => self.move_family_cursor(-self.family_page_size()),
                Focus::Tickets | Focus::Details => self.move_focused(-10),
            },
            KeyCode::Home => match self.focus {
                Focus::Tickets => self.select_row(0),
                Focus::Family => self.move_family_cursor_to_edge(false),
                Focus::Details => self.details.scroll_to(0),
            },
            KeyCode::End => match self.focus {
                Focus::Tickets => self.select_row(self.visible.len().saturating_sub(1)),
                Focus::Family => self.move_family_cursor_to_edge(true),
                Focus::Details => self.details.scroll_to(self.details.max_offset()),
            },
            KeyCode::Enter => match self.focus {
                Focus::Tickets => {}
                Focus::Family => {
                    if let Some(key) = self.family_cursor.clone() {
                        self.jump_to_ticket(&key);
                    }
                }
                Focus::Details => {
                    // A field the pointer is resting on opens its editor, the
                    // way clicking it would; anywhere else still opens the
                    // work item in the browser.
                    if let Some(field) = self.pointed_edit_field() {
                        return self.open_field_editor(field);
                    }
                    self.record_history();
                    return self.open_selected();
                }
            },
            KeyCode::Esc if !self.query.is_empty() => self.set_query(String::new()),
            KeyCode::Esc if !self.selected_keys.is_empty() => self.selected_keys.clear(),
            _ => {
                if let Some(id) = command_for_key(key) {
                    return self.run_command(id);
                }
            }
        }
        AppAction::None
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.finish_search(),
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.recall_previous_search();
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.recall_next_search();
            }
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),
            _ => self.edit_query(|query| {
                query.handle_key(key);
            }),
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
                self.help.scroll_by(-1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.help.scroll_by(1);
            }
            KeyCode::PageUp => {
                self.help.scroll_by(-5);
            }
            KeyCode::PageDown => {
                self.help.scroll_by(5);
            }
            KeyCode::Home => self.help.scroll_to(0),
            KeyCode::End => self.help.scroll_to(self.help.max_offset()),
            _ => {}
        }
    }

    fn handle_hover(&mut self, column: u16, row: u16) -> PointerUpdate {
        self.pointer.set_position(column, row);
        PointerUpdate::none(self.refresh_hover())
    }

    pub fn refresh_hover(&mut self) -> bool {
        let hover = self
            .pointer
            .position()
            .and_then(|(column, row)| self.hit_regions.resolve(column, row))
            .map(|region| region.target.clone());
        let changed = hover != self.pointer.hover;
        self.pointer.hover = hover;
        changed
    }

    fn handle_press(&mut self, column: u16, row: u16) -> PointerUpdate {
        let region = self.hit_regions.resolve(column, row).cloned();
        let selectable = self.hit_regions.resolve_selectable(column, row);
        self.pointer.clear_selection();
        if let Some(region) = region {
            let scrollbar = match region.target {
                PointerTarget::ScrollbarThumb { surface } => Some(surface),
                _ => None,
            };
            let selectable = match region.target {
                // Neither drags text: one resizes the panes, and the other is
                // the empty space around a dropdown.
                PointerTarget::PaneDivider | PointerTarget::DismissOverlay => None,
                _ => selectable,
            };
            self.pointer.hover = Some(region.target.clone());
            self.pointer
                .begin_press(region.target, column, row, selectable, scrollbar);
        } else {
            self.pointer.hover = None;
            self.pointer.clear_press();
        }
        PointerUpdate::none(true)
    }

    fn handle_drag(&mut self, column: u16, row: u16) -> PointerUpdate {
        let hover = self
            .hit_regions
            .resolve(column, row)
            .map(|region| region.target.clone());
        let hover_changed = hover != self.pointer.hover;
        self.pointer.hover = hover;
        if !self.pointer.moved_from_origin(column, row)
            && matches!(self.pointer.drag(), DragKind::None)
        {
            return PointerUpdate::none(hover_changed);
        }
        match self.pointer.drag() {
            DragKind::Scrollbar { surface, grab } => {
                self.drag_scrollbar(surface, row, grab);
                PointerUpdate::none(true)
            }
            DragKind::Text => {
                self.update_text_drag(column, row);
                PointerUpdate::none(true)
            }
            DragKind::Divider => {
                self.drag_divider(column, row);
                PointerUpdate::none(true)
            }
            DragKind::Cancelled => PointerUpdate::none(hover_changed),
            DragKind::None => {
                if matches!(
                    self.pointer.press_target(),
                    Some(PointerTarget::PaneDivider)
                ) {
                    self.pointer.set_drag(DragKind::Divider);
                    self.drag_divider(column, row);
                    PointerUpdate::none(true)
                } else if let Some(surface) = self.pointer.press_scrollbar() {
                    let grab = self.scrollbar_grab(surface, self.pointer.press_origin());
                    self.pointer.set_drag(DragKind::Scrollbar { surface, grab });
                    self.drag_scrollbar(surface, row, grab);
                    PointerUpdate::none(true)
                } else if let Some(surface) = self.pointer.press_selectable() {
                    self.pointer.set_drag(DragKind::Text);
                    if let Some(origin) = self.pointer.press_origin()
                        && let Some(snapshot) = self.hit_regions.selectable(surface)
                        && let Some(start) = snapshot.pos_at(origin.0, origin.1)
                    {
                        self.pointer.selection = Some(TextSelection {
                            surface,
                            start,
                            end: start,
                        });
                    }
                    self.update_text_drag(column, row);
                    PointerUpdate::none(true)
                } else {
                    self.pointer.set_drag(DragKind::Cancelled);
                    PointerUpdate::none(hover_changed)
                }
            }
        }
    }

    fn handle_release(&mut self, column: u16, row: u16) -> PointerUpdate {
        let drag = self.pointer.drag();
        let target = self.pointer.press_target().cloned();
        let selection = self.pointer.selection;
        self.pointer.clear_press();
        self.handle_hover(column, row);
        match drag {
            DragKind::Text => {
                if let Some(selection) = selection.filter(|selection| !selection.is_empty())
                    && let Some(snapshot) = self.hit_regions.selectable(selection.surface)
                {
                    let text = pointer::extract_selected_text(snapshot, &selection);
                    if !text.is_empty() {
                        return PointerUpdate::action(AppAction::Copy {
                            text,
                            content: CopiedContent::Text,
                        });
                    }
                }
                PointerUpdate::none(true)
            }
            DragKind::Divider => {
                self.session_dirty = true;
                PointerUpdate::none(true)
            }
            DragKind::Scrollbar { .. } | DragKind::Cancelled => PointerUpdate::none(true),
            DragKind::None => {
                if let Some(target) = target {
                    PointerUpdate::action(self.activate_target(target, column, row))
                } else {
                    PointerUpdate::none(true)
                }
            }
        }
    }

    fn handle_wheel(&mut self, column: u16, row: u16, delta: i32) -> PointerUpdate {
        let hover_changed = self.refresh_hover();
        let Some(surface) = self.hit_regions.resolve_scroll(column, row) else {
            return PointerUpdate::none(hover_changed);
        };
        let changed = self.scroll_surface(surface, delta);
        PointerUpdate::none(changed || hover_changed)
    }

    fn activate_target(&mut self, target: PointerTarget, column: u16, row: u16) -> AppAction {
        match target {
            PointerTarget::SearchField => {
                self.begin_search();
                self.place_caret(TextEditor::Search, column, row);
            }
            PointerTarget::ClearQuery => self.set_query(String::new()),
            PointerTarget::OpenPalette => return self.run_command(CommandId::Palette),
            PointerTarget::OpenHelp => return self.run_command(CommandId::Help),
            PointerTarget::CopyActions => self.open_copy_actions(),
            PointerTarget::CloseOverlay => self.close_overlay(),
            PointerTarget::NarrowTickets => {
                self.narrow_details = false;
                self.focus = Focus::Tickets;
            }
            PointerTarget::NarrowDetails => {
                self.narrow_details = true;
                if !self.focus.is_details_pane() {
                    self.focus = Focus::Details;
                }
            }
            PointerTarget::FocusTickets => {
                self.focus = Focus::Tickets;
                self.narrow_details = false;
            }
            PointerTarget::FocusDetails => {
                self.focus = Focus::Details;
            }
            PointerTarget::TableRow { index } => {
                self.focus = Focus::Tickets;
                self.narrow_details = false;
                if index < self.visible.len() {
                    self.select_row(index);
                    self.record_history();
                }
            }
            PointerTarget::OpenTicket { index } => {
                self.focus = Focus::Tickets;
                self.narrow_details = false;
                if index < self.visible.len() {
                    self.select_row(index);
                    self.record_history();
                    return self.open_selected();
                }
            }
            PointerTarget::ToggleBookmark { index } => {
                if index < self.visible.len() {
                    self.select_row(index);
                    self.toggle_bookmark();
                }
            }
            PointerTarget::ToggleRowSelect { index } => {
                if index < self.visible.len() {
                    self.select_row(index);
                    self.toggle_row_selection();
                }
            }
            PointerTarget::SortHeader(field) => self.toggle_sort(field),
            PointerTarget::OpenSelectedUrl => {
                self.focus = Focus::Details;
                self.narrow_details = true;
                return self.open_selected();
            }
            PointerTarget::JumpToTicket(key) => {
                if self
                    .visible_family_tree()
                    .iter()
                    .any(|entry| entry.key == key)
                {
                    self.focus = Focus::Family;
                    self.family_cursor = Some(key.clone());
                    self.ensure_family_cursor_visible();
                } else if self
                    .selected_family()
                    .is_some_and(|family| family.extra_parents.iter().any(|parent| parent == &key))
                {
                    self.focus = Focus::Family;
                } else {
                    self.focus = Focus::Details;
                }
                self.jump_to_ticket(&key);
            }
            PointerTarget::FacetPill(target) => match target {
                FacetTarget::More => self.open_filters(),
                FacetTarget::Field(field) => {
                    let index = FilterField::BAR
                        .iter()
                        .position(|entry| *entry == field)
                        .unwrap_or_default();
                    self.open_facets(index);
                }
            },
            PointerTarget::FacetValue { index } => {
                self.facet_bar.value_index = index;
                self.toggle_current_bar_facet();
            }
            PointerTarget::DismissFacet => {
                if self.mode == AppMode::Facets {
                    self.mode = AppMode::Browse;
                }
            }
            PointerTarget::RemoveChip(token) => self.remove_filter_token(token),
            PointerTarget::SortChoose(field) => {
                self.toggle_sort(field);
                self.mode = AppMode::Browse;
            }
            PointerTarget::SortSetDirection(direction) => {
                self.sort_draft.direction = direction;
            }
            PointerTarget::FilterRow { index } => {
                if self.filter_overlay.showing_values {
                    self.filter_overlay.value_index = index;
                    self.toggle_current_facet();
                } else {
                    self.filter_overlay.field_index = index;
                    self.filter_overlay.showing_values = true;
                    self.filter_overlay.value_index = 0;
                    self.filter_overlay.scroll.scroll_to(0);
                }
            }
            PointerTarget::ColumnToggle { index } => {
                self.column_overlay.index = index;
                self.layout.toggle_visible(index);
                self.session_dirty = true;
            }
            PointerTarget::ColumnMove { index, delta } => {
                self.column_overlay.index = self.layout.move_column(index, delta);
                self.session_dirty = true;
            }
            PointerTarget::ColumnResize { index, delta } => {
                self.column_overlay.index = index;
                self.layout.resize(index, delta);
                self.session_dirty = true;
            }
            PointerTarget::PaletteCommand { index } => {
                self.palette.selected = index;
                return self.run_selected_command();
            }
            PointerTarget::PaletteQuery => {
                self.place_caret(TextEditor::Palette, column, row);
            }
            PointerTarget::EditMenuRow { index } => {
                self.edit_menu.index = index;
                return self.run_edit_menu_entry(index);
            }
            PointerTarget::StateOption { index } => {
                self.state_picker.index = index;
                return self.choose_state(index);
            }
            PointerTarget::PriorityOption { index } => {
                self.priority_picker.index = index;
                return self.choose_priority(index);
            }
            PointerTarget::AssigneeOption { index } => {
                self.assignee_picker.index = index;
                return self.choose_assignee(index);
            }
            PointerTarget::AssigneeQuery => {
                self.place_caret(TextEditor::Assignee, column, row);
            }
            PointerTarget::ParentOption { index } => {
                self.parent_picker.index = index;
                return self.choose_parent(index);
            }
            PointerTarget::ParentQuery => {
                self.place_caret(TextEditor::Parent, column, row);
            }
            PointerTarget::NodeOption { index } => {
                self.node_picker.index = index;
                return self.choose_node(index);
            }
            PointerTarget::NodeQuery => {
                self.place_caret(TextEditor::Node, column, row);
            }
            PointerTarget::FormField { index } => {
                self.focus_form_field(index);
                self.place_caret(TextEditor::Form, column, row);
            }
            PointerTarget::SubmitForm => return self.submit_form(),
            PointerTarget::CancelForm => self.cancel_form(),
            PointerTarget::TypeOption { index } => {
                self.type_picker.index = index;
                self.choose_work_item_type(index);
            }
            PointerTarget::EditField { field } => return self.open_field_editor(field),
            PointerTarget::DismissOverlay => self.close_overlay(),
            PointerTarget::PromptInput => {
                self.place_caret(TextEditor::Prompt, column, row);
            }
            PointerTarget::SubmitPrompt => return self.submit_prompt(),
            PointerTarget::CancelPrompt => self.close_prompt(),
            PointerTarget::ViewRow { index } => {
                if self
                    .view_rows()
                    .get(index)
                    .is_some_and(|row| !row.is_heading())
                {
                    self.views_overlay.index = index;
                    self.apply_view_at(index);
                }
            }
            PointerTarget::SaveView => {
                if self.views_overlay.naming.is_some() {
                    if let Some(name) = self
                        .views_overlay
                        .naming
                        .take()
                        .map(|name| name.text().trim().to_owned())
                        .filter(|name| !name.is_empty())
                    {
                        self.save_view(name);
                    }
                } else {
                    self.views_overlay.naming =
                        Some(TextInput::new(self.active_view.clone().unwrap_or_default()));
                }
            }
            PointerTarget::DeleteView => self.delete_view_at(self.views_overlay.index),
            PointerTarget::ViewName => {
                self.place_caret(TextEditor::ViewName, column, row);
            }
            PointerTarget::CancelNaming => self.views_overlay.naming = None,
            PointerTarget::OverlayBody => {}
            PointerTarget::ScrollbarTrack { surface, page_down } => {
                let step =
                    i32::try_from(self.scroll_state(surface).page_step()).unwrap_or(i32::MAX);
                self.scroll_surface(surface, if page_down { step } else { -step });
            }
            PointerTarget::ScrollbarThumb { .. } => {}
            PointerTarget::PaneDivider => {}
        }
        AppAction::None
    }

    /// The details-pane field the pointer is resting on, which is what `Enter`
    /// opens an editor for while that pane is focused.
    #[must_use]
    fn pointed_edit_field(&self) -> Option<EditableField> {
        match self.hovered_region().map(|region| &region.target) {
            Some(PointerTarget::EditField { field }) => Some(*field),
            _ => None,
        }
    }

    /// Opens the editor one details-pane field owns, as a dropdown hung under
    /// the value on screen. It runs the same command the Edit menu and the
    /// palette run, so both paths open the same picker and write the same
    /// edit; only where the overlay lands differs.
    fn open_field_editor(&mut self, field: EditableField) -> AppAction {
        let anchor = self
            .hit_regions
            .edit_field(field)
            .map_or(OverlayAnchor::Centered, OverlayAnchor::Below);
        let action = self.run_command(command_for_field(field));
        self.overlay_anchor = anchor;
        action
    }

    fn close_overlay(&mut self) {
        match self.mode {
            AppMode::Views if self.views_overlay.naming.is_some() => {
                self.views_overlay.naming = None;
            }
            AppMode::Prompt => self.close_prompt(),
            AppMode::Form => self.cancel_form(),
            AppMode::AssigneePicker => self.close_picker(self.assignee_picker.scope),
            AppMode::NodePicker => self.close_picker(self.node_picker.scope),
            AppMode::TypePicker => self.close_picker(EditScope::Form(self.type_picker.field)),
            AppMode::Facets => self.mode = AppMode::Browse,
            AppMode::Filter if self.filter_overlay.showing_values => {
                self.filter_overlay.showing_values = false;
                self.filter_overlay.value_index = 0;
                self.filter_overlay.scroll.scroll_to(0);
            }
            AppMode::Browse | AppMode::Search => {}
            _ => self.mode = AppMode::Browse,
        }
        self.pointer.clear_selection();
    }

    fn open_copy_actions(&mut self) {
        self.run_command(CommandId::Palette);
        self.palette.query = TextInput::new("copy");
    }

    fn place_caret(&mut self, editor: TextEditor, column: u16, row: u16) {
        let Some(snapshot) = self
            .hit_regions
            .selectable(match editor {
                TextEditor::Search => SelectableSurface::Search,
                TextEditor::Palette
                | TextEditor::ViewName
                | TextEditor::Prompt
                | TextEditor::Assignee
                | TextEditor::Node
                | TextEditor::Parent
                | TextEditor::Form => SelectableSurface::Overlay,
            })
            .and_then(|snapshot| snapshot.pos_at(column, row))
            .or_else(|| {
                self.hit_regions.resolve(column, row).map(|region| TextPos {
                    line: 0,
                    col: usize::from(column.saturating_sub(region.rect.x)),
                })
            })
        else {
            return;
        };
        let index = snapshot.col;
        match editor {
            TextEditor::Search => self.query.set_cursor(index),
            TextEditor::Palette => self.palette.query.set_cursor(index),
            TextEditor::ViewName => {
                if let Some(name) = self.views_overlay.naming.as_mut() {
                    name.set_cursor(index);
                }
            }
            TextEditor::Prompt => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.input.set_cursor(index);
                }
            }
            TextEditor::Assignee => self.assignee_picker.query.set_cursor(index),
            TextEditor::Parent => self.parent_picker.query.set_cursor(index),
            TextEditor::Node => self.node_picker.query.set_cursor(index),
            TextEditor::Form => {
                if let Some(field) = self.focused_form_field_mut() {
                    field.input.set_cursor(index);
                }
            }
        }
    }

    fn update_text_drag(&mut self, column: u16, row: u16) {
        let Some(surface) = self
            .pointer
            .selection
            .map(|selection| selection.surface)
            .or_else(|| self.pointer.press_selectable())
        else {
            return;
        };
        let Some(snapshot) = self.hit_regions.selectable(surface) else {
            return;
        };
        let Some(end) = snapshot
            .pos_at(column, row)
            .or_else(|| clamp_pos_to_snapshot(snapshot, column, row))
        else {
            return;
        };
        if let Some(selection) = self.pointer.selection.as_mut() {
            selection.end = end;
        } else if let Some(origin) = self.pointer.press_origin()
            && let Some(start) = snapshot.pos_at(origin.0, origin.1)
        {
            self.pointer.selection = Some(TextSelection {
                surface,
                start,
                end,
            });
        }
    }

    fn scrollbar_grab(&self, surface: ScrollSurface, origin: Option<(u16, u16)>) -> i16 {
        let Some((_, row)) = origin else {
            return 0;
        };
        let Some(metrics) = self.hit_regions.scroll(surface) else {
            return 0;
        };
        let Some(thumb) = metrics.thumb() else {
            return 0;
        };
        i16::try_from(row).unwrap_or(0)
            - i16::try_from(metrics.track.y.saturating_add(thumb.y)).unwrap_or(0)
    }

    fn drag_scrollbar(&mut self, surface: ScrollSurface, row: u16, grab: i16) {
        let Some(metrics) = self.hit_regions.scroll(surface) else {
            return;
        };
        let Some(thumb) = metrics.thumb() else {
            return;
        };
        let pointer = i32::from(row) - i32::from(grab);
        let track_y = i32::from(metrics.track.y);
        let rel = pointer.saturating_sub(track_y).max(0) as usize;
        let offset =
            pointer::offset_from_thumb(rel.min(thumb.travel), thumb.travel, thumb.max_offset);
        self.scroll_state_mut(surface).scroll_to(offset);
    }

    fn scroll_surface(&mut self, surface: ScrollSurface, delta: i32) -> bool {
        self.scroll_state_mut(surface).scroll_by(delta)
    }

    /// Records the workspace the panes were last split inside, and which way the
    /// divider runs there. The narrow layout passes `None`: it has no divider.
    pub const fn set_content_layout(&mut self, area: Rect, divider: Option<DividerOrientation>) {
        self.content_area = area;
        self.divider = divider;
    }

    #[must_use]
    pub const fn content_area(&self) -> Rect {
        self.content_area
    }

    #[must_use]
    pub const fn divider_orientation(&self) -> Option<DividerOrientation> {
        self.divider
    }

    /// Moves the divider under the pointer: the tickets pane keeps everything up
    /// to the pointer, the details pane the rest.
    fn drag_divider(&mut self, column: u16, row: u16) {
        match self.divider {
            Some(DividerOrientation::Vertical) => {
                let span = self.content_area.width;
                let cells = column.saturating_sub(self.content_area.x);
                self.pane_split_wide =
                    split_percent(cells, span, MIN_TICKETS_COLUMNS, MIN_DETAILS_COLUMNS);
            }
            Some(DividerOrientation::Horizontal) => {
                let span = self.content_area.height;
                let cells = row.saturating_sub(self.content_area.y);
                self.pane_split_stacked = split_percent(cells, span, MIN_PANE_ROWS, MIN_PANE_ROWS);
            }
            None => {}
        }
    }

    /// Restores the built-in split for both layouts.
    fn reset_pane_split(&mut self) {
        self.pane_split_wide = DEFAULT_PANE_SPLIT_WIDE;
        self.pane_split_stacked = DEFAULT_PANE_SPLIT_STACKED;
        self.session_dirty = true;
        self.set_status("Reset pane split");
    }

    fn move_focused(&mut self, delta: isize) {
        match self.focus {
            Focus::Tickets => self.move_selection(delta),
            Focus::Family => self.move_family_cursor(delta),
            Focus::Details => self.scroll_details(delta),
        }
    }

    fn scroll_details(&mut self, delta: isize) {
        let delta = i32::try_from(delta).unwrap_or(if delta.is_negative() {
            i32::MIN
        } else {
            i32::MAX
        });
        self.details.scroll_by(delta);
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Tickets => Focus::Details,
            Focus::Family => Focus::Details,
            Focus::Details => Focus::Tickets,
        };
        self.narrow_details = self.focus.is_details_pane();
    }

    fn toggle_narrow_details(&mut self) {
        self.narrow_details = !self.narrow_details;
        if self.narrow_details {
            if !self.focus.is_details_pane() {
                self.focus = Focus::Details;
            }
        } else {
            self.focus = Focus::Tickets;
        }
    }

    fn begin_search(&mut self) {
        self.query.move_end();
        self.search_history_index = None;
        self.search_history_draft = self.query.text().to_owned();
        self.mode = AppMode::Search;
    }

    fn finish_search(&mut self) {
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

    fn recall_previous_search(&mut self) {
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
            self.table.offset = 0;
        } else {
            let row = row.min(self.visible.len() - 1);
            self.table_state.select(Some(row));
            self.table.ensure_visible(row);
        }
        self.details.scroll_to(0);
        self.sync_family_state();
    }

    fn visible_row(&self, key: &TicketKey) -> Option<usize> {
        self.visible
            .iter()
            .position(|entry| self.tickets[entry.ticket_index].key == *key)
    }

    fn jump_to_ticket(&mut self, key: &TicketKey) {
        if self
            .selected_ticket()
            .is_some_and(|ticket| ticket.key == *key)
        {
            return;
        }
        let Some(row) = self.visible_row(key) else {
            if self.ticket_by_key(key).is_some() {
                self.set_status(format!("{id} is hidden by the current search", id = key.id));
            } else {
                self.set_error(format!("{id} is not in this database", id = key.id));
            }
            return;
        };
        self.record_history();
        self.select_row(row);
        self.record_history();
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

    fn sort_visible(&mut self) {
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

    fn restore_selection(&mut self, selected: Option<&TicketKey>) {
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

    fn sync_family_state(&mut self) {
        self.reset_family_cursor();
        if self.focus == Focus::Family && !self.selected_has_family() {
            self.focus = Focus::Details;
        }
    }

    fn reset_family_cursor(&mut self) {
        self.family_cursor = self.selected_ticket().map(|ticket| ticket.key.clone());
        self.clamp_family_cursor();
    }

    fn family_page_size(&self) -> isize {
        let visible = self.visible_family_tree().len().max(1);
        let viewport = self.details.viewport.max(1);
        isize::try_from(viewport.min(visible)).unwrap_or(1)
    }

    fn move_family_cursor(&mut self, delta: isize) {
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

    fn move_family_cursor_to_edge(&mut self, last: bool) {
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

    fn ensure_family_cursor_visible(&mut self) {
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

    fn open_filters(&mut self) {
        self.filter_overlay = FilterOverlay::default();
        self.mode = AppMode::Filter;
    }

    fn open_facets(&mut self, field_index: usize) {
        self.facet_bar.field_index = field_index.min(FilterField::BAR.len());
        self.facet_bar.value_index = 0;
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
        self.mode = AppMode::Columns;
    }

    fn open_palette(&mut self) {
        self.palette = PaletteState::default();
        self.mode = AppMode::Palette;
    }

    fn open_views(&mut self) {
        self.views_overlay = ViewsOverlay::default();
        self.focus_view(0);
        self.mode = AppMode::Views;
    }

    /// `e`: the list of field editors. Every editor is one row of
    /// [`EDIT_MENU`], so a new one appears here by being added there.
    fn open_edit_menu(&mut self) {
        self.edit_menu = EditMenu::default();
        self.mode = AppMode::Edit;
    }

    fn handle_edit_menu_key(&mut self, key: KeyEvent) -> AppAction {
        let last = self.edit_menu_entries().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('e') => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_edit_entry(self.edit_menu.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_edit_entry((self.edit_menu.index + 1).min(last));
            }
            KeyCode::Home => self.focus_edit_entry(0),
            KeyCode::End => self.focus_edit_entry(last),
            KeyCode::Enter => return self.run_edit_menu_entry(self.edit_menu.index),
            _ => {}
        }
        AppAction::None
    }

    fn focus_edit_entry(&mut self, index: usize) {
        self.edit_menu.index = index;
        self.edit_menu.scroll.ensure_visible(index);
    }

    /// Runs one Edit menu entry, which is the command it names. Each editor
    /// opens itself, so nothing here knows what a state or a title is.
    fn run_edit_menu_entry(&mut self, index: usize) -> AppAction {
        let Some(entry) = self.edit_menu_entries().get(index).copied() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        self.mode = AppMode::Browse;
        self.run_command(entry.command)
    }

    /// `S`, and the Edit menu's State row: the states this work item's type
    /// allows, with the one it is in already under the cursor. The list is
    /// whatever is cached or already in the database, so this never waits.
    ///
    /// With two or more rows checked the picker moves all of them, and says so
    /// in its title. The states it offers are still the selected row's type's,
    /// which is the only type it could ask about; a state another checked work
    /// item's type does not allow is refused by Azure DevOps and named in the
    /// summary.
    fn open_state_picker(&mut self) {
        let scope = self.edit_scope();
        let Some(ticket) = self.selected_ticket() else {
            self.set_error("No work item is selected");
            return;
        };
        let current = ticket.state.clone();
        let work_item_type = ticket.work_item_type.clone();
        let options = self.states_for(&work_item_type);
        if options.is_empty() {
            self.set_error(format!("No states are known for {work_item_type}"));
            return;
        }
        let index = options
            .iter()
            .position(|option| option.name == current)
            .unwrap_or_default();
        self.state_picker = StatePicker {
            options,
            index,
            scroll: ScrollState::default(),
            current,
            scope,
        };
        self.state_picker.scroll.ensure_visible(index);
        self.mode = AppMode::StatePicker;
    }

    fn handle_state_picker_key(&mut self, key: KeyEvent) -> AppAction {
        let last = self.state_picker.options.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('S') => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_state(self.state_picker.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_state((self.state_picker.index + 1).min(last));
            }
            KeyCode::PageUp => self.focus_state(self.state_picker.index.saturating_sub(5)),
            KeyCode::PageDown => self.focus_state((self.state_picker.index + 5).min(last)),
            KeyCode::Home => self.focus_state(0),
            KeyCode::End => self.focus_state(last),
            KeyCode::Enter => return self.choose_state(self.state_picker.index),
            _ => {}
        }
        AppAction::None
    }

    fn focus_state(&mut self, index: usize) {
        self.state_picker.index = index;
        self.state_picker.scroll.ensure_visible(index);
    }

    /// Confirms one state. Choosing the state the work item is already in
    /// closes the picker and writes nothing; anything else takes the ordinary
    /// write-through path, so the row changes at once and reverts if Azure
    /// DevOps refuses the transition. A picker opened over the checked rows
    /// moves every one of them, so the state the row under the cursor is in is
    /// a change to make there rather than a no-op.
    fn choose_state(&mut self, index: usize) -> AppAction {
        let Some(option) = self.state_picker.options.get(index).cloned() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        self.mode = AppMode::Browse;
        if !self.state_picker.scope.is_bulk() && option.name == self.state_picker.current {
            return AppAction::None;
        }
        self.edit_checked(FieldEdit::state(&option.name))
    }

    /// The Edit menu's Priority row: 1 to 4 and a `Clear` row, with the
    /// priority the work item already has under the cursor.
    fn open_priority_picker(&mut self) {
        let Some(ticket) = self.selected_ticket() else {
            self.set_error("No work item is selected");
            return;
        };
        let current = ticket.priority;
        let id = ticket.key.id;
        let index = PRIORITY_CHOICES
            .iter()
            .position(|choice| *choice == current)
            .unwrap_or_default();
        self.priority_picker = PriorityPicker {
            index,
            scroll: ScrollState::default(),
            current,
            id,
        };
        self.priority_picker.scroll.ensure_visible(index);
        self.mode = AppMode::PriorityPicker;
    }

    fn handle_priority_picker_key(&mut self, key: KeyEvent) -> AppAction {
        let last = PRIORITY_CHOICES.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => self.mode = AppMode::Browse,
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_priority(self.priority_picker.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_priority((self.priority_picker.index + 1).min(last));
            }
            KeyCode::Home => self.focus_priority(0),
            KeyCode::End => self.focus_priority(last),
            KeyCode::Enter => return self.choose_priority(self.priority_picker.index),
            _ => {}
        }
        AppAction::None
    }

    fn focus_priority(&mut self, index: usize) {
        self.priority_picker.index = index;
        self.priority_picker.scroll.ensure_visible(index);
    }

    /// Confirms one priority. The priority the work item already has is a
    /// no-op, and `Clear` takes the field off it rather than writing an empty
    /// value, so the Pri cell empties.
    fn choose_priority(&mut self, index: usize) -> AppAction {
        let Some(choice) = PRIORITY_CHOICES.get(index).copied() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        self.mode = AppMode::Browse;
        if choice == self.priority_picker.current {
            return AppAction::None;
        }
        match choice {
            Some(priority) => self.edit_selected(FieldEdit::priority(priority)),
            None => self.edit_selected(FieldEdit::clear_priority()),
        }
    }

    /// Who the assignee picker offers, in the order it lists them: nobody, the
    /// signed-in user, everybody the database has ever seen a work item
    /// assigned to, and then the rest of the project's teams. Nobody appears
    /// twice, so a team member already holding work keeps their earlier place.
    #[must_use]
    fn assignee_candidates(&self) -> Vec<AssigneeCandidate> {
        let mut candidates = vec![AssigneeCandidate {
            display: UNASSIGNED_LABEL.to_owned(),
            unique: None,
            unassigned: true,
            me: false,
        }];
        if let Some(me) = self
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
                .find(|identity| same_name(&identity.display_name, display))
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

    /// `a`, and the Edit menu's Assignee row: everybody worth offering, with
    /// whoever holds the work item under the cursor. The list is built from
    /// what is already in memory, so the picker opens at once; the project's
    /// teams are asked for the first time it is opened and merged in when they
    /// arrive. With two or more rows checked it reassigns all of them, and
    /// says so in its title.
    fn open_assignee_picker(&mut self) -> AppAction {
        let scope = self.edit_scope();
        let Some(ticket) = self.selected_ticket() else {
            self.set_error("No work item is selected");
            return AppAction::None;
        };
        let current = ticket.assigned_to.clone();
        self.show_assignee_picker(current, scope)
    }

    /// The assignee picker itself, over whoever holds the work item now — or
    /// over the name a form field carries — and whatever it was opened for.
    /// Both the Edit menu and a form's Assignee field come through here, so
    /// the list, its cursor, and the one fetch a session are the same either
    /// way.
    fn show_assignee_picker(&mut self, current: Option<String>, scope: EditScope) -> AppAction {
        let candidates = self.assignee_candidates();
        let index = candidates
            .iter()
            .position(|candidate| candidate.is_current(current.as_deref()))
            .unwrap_or_default();
        self.assignee_picker = AssigneePicker {
            candidates,
            query: TextInput::default(),
            index,
            scroll: ScrollState::default(),
            current,
            scope,
        };
        self.assignee_picker.scroll.ensure_visible(index);
        self.mode = AppMode::AssigneePicker;
        if self.identities_requested {
            AppAction::None
        } else {
            self.identities_requested = true;
            AppAction::FetchIdentities
        }
    }

    fn handle_assignee_picker_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.close_picker(self.assignee_picker.scope),
            KeyCode::Up => self.move_assignee_selection(-1),
            KeyCode::Down => self.move_assignee_selection(1),
            KeyCode::PageUp => self.move_assignee_selection(-5),
            KeyCode::PageDown => self.move_assignee_selection(5),
            KeyCode::Enter => return self.choose_assignee(self.assignee_picker.index),
            // Everything else is typing: Home, End, and the editing keys all
            // belong to the filter field, the way they do in the palette.
            _ => {
                let before = self.assignee_picker.query.text().to_owned();
                self.assignee_picker.query.handle_key(key);
                if self.assignee_picker.query.text() != before {
                    self.assignee_picker.index = 0;
                    self.assignee_picker.scroll.scroll_to(0);
                }
            }
        }
        AppAction::None
    }

    fn move_assignee_selection(&mut self, delta: isize) {
        let count = self.assignee_matches().len();
        if count == 0 {
            self.assignee_picker.index = 0;
            return;
        }
        let index = self
            .assignee_picker
            .index
            .saturating_add_signed(delta)
            .min(count - 1);
        self.focus_assignee(index);
    }

    fn focus_assignee(&mut self, index: usize) {
        self.assignee_picker.index = index;
        self.assignee_picker.scroll.ensure_visible(index);
    }

    /// Confirms one candidate. Whoever holds the work item already is a no-op,
    /// and `Unassigned` takes the field off it rather than writing an empty
    /// identity, so the Assignee cell empties. A picker opened over the checked
    /// rows reassigns every one of them, so whoever holds the row under the
    /// cursor is a change to make to the rest rather than a no-op.
    fn choose_assignee(&mut self, index: usize) -> AppAction {
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
        self.mode = AppMode::Browse;
        if !self.assignee_picker.scope.is_bulk()
            && candidate.is_current(self.assignee_picker.current.as_deref())
        {
            return AppAction::None;
        }
        if candidate.unassigned {
            return self.edit_checked(FieldEdit::unassign());
        }
        self.edit_checked(FieldEdit::assignee(
            &candidate.display,
            candidate.unique.as_deref(),
        ))
    }

    /// The parent one work item hangs under now, as the graph holds it. Azure
    /// DevOps allows a work item only one, so the first is the one.
    #[must_use]
    pub fn parent_of(&self, key: &TicketKey) -> Option<TicketKey> {
        self.graph.parents_of(key).into_iter().next()
    }

    /// Whether the work item under the cursor has a parent to take off, which
    /// is what puts `Remove parent` in the Edit menu.
    #[must_use]
    pub fn selected_has_parent(&self) -> bool {
        self.selected_ticket()
            .is_some_and(|ticket| self.parent_of(&ticket.key).is_some())
    }

    /// The Edit menu as it stands for the row under the cursor: [`EDIT_MENU`],
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

    /// The Edit menu's `Set parent…` row: every work item this one could hang
    /// under, with the one it hangs under now under the cursor. The list is
    /// built from the rows already loaded, so the picker opens at once.
    fn open_parent_picker(&mut self) {
        let Some(child) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            self.set_error("No work item is selected");
            return;
        };
        let candidates = self.parent_candidates(&child);
        if candidates.is_empty() {
            self.set_error("No other work item is loaded to file this one under");
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
            index,
            scroll: ScrollState::default(),
            child,
            current,
        };
        self.parent_picker.scroll.ensure_visible(index);
        self.mode = AppMode::ParentPicker;
    }

    fn handle_parent_picker_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.mode = AppMode::Browse,
            KeyCode::Up => self.move_parent_selection(-1),
            KeyCode::Down => self.move_parent_selection(1),
            KeyCode::PageUp => self.move_parent_selection(-5),
            KeyCode::PageDown => self.move_parent_selection(5),
            KeyCode::Enter => return self.choose_parent(self.parent_picker.index),
            // Everything else is typing, the way it is in the assignee picker.
            _ => {
                let before = self.parent_picker.query.text().to_owned();
                self.parent_picker.query.handle_key(key);
                if self.parent_picker.query.text() != before {
                    self.parent_picker.index = 0;
                    self.parent_picker.scroll.scroll_to(0);
                }
            }
        }
        AppAction::None
    }

    fn move_parent_selection(&mut self, delta: isize) {
        let last = self.parent_matches().len().saturating_sub(1);
        let index = self
            .parent_picker
            .index
            .saturating_add_signed(delta)
            .min(last);
        self.parent_picker.index = index;
        self.parent_picker.scroll.ensure_visible(index);
    }

    /// `Enter` in the parent picker: the work item moves under whatever the
    /// cursor is on. Choosing the parent it already has writes nothing.
    fn choose_parent(&mut self, index: usize) -> AppAction {
        let Some(candidate) = self.parent_matches().get(index).cloned() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        self.mode = AppMode::Browse;
        if self.parent_picker.current.as_ref() == Some(&candidate.key) {
            return AppAction::None;
        }
        let child = self.parent_picker.child.clone();
        self.begin_reparent(&child, Some(candidate.key))
    }

    /// The Edit menu's `Remove parent` row: the work item comes out of its
    /// family and hangs under nothing.
    fn remove_parent(&mut self) -> AppAction {
        let Some(child) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            self.set_error("No work item is selected");
            return AppAction::None;
        };
        if self.parent_of(&child).is_none() {
            self.set_error(format!("#{} has no parent to remove", child.id));
            return AppAction::None;
        }
        self.begin_reparent(&child, None)
    }

    /// Starts one move: the graph takes it at once in both directions, the
    /// parent it had is kept for a refusal, and the action that sends it comes
    /// back. The child progress of the parent it left and the parent it joined
    /// are both rebuilt here, so neither ratio is stale for a frame.
    fn begin_reparent(&mut self, child: &TicketKey, new_parent: Option<TicketKey>) -> AppAction {
        if !self.sync_enabled {
            let reason = self
                .offline_reason
                .clone()
                .unwrap_or_else(|| "no Azure DevOps organization is configured".to_owned());
            self.set_error(format!("#{} not moved: {reason}", child.id));
            return AppAction::None;
        }
        if self.pending_reparents.contains_key(child) {
            self.set_error(format!("#{}: an earlier move is still in flight", child.id));
            return AppAction::None;
        }
        let previous = self.parent_of(child);
        self.graph.reparent(child, new_parent.as_ref());
        self.refresh_child_progress();
        self.pending_reparents.insert(child.clone(), previous);
        self.set_status(match new_parent.as_ref() {
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
    pub fn apply_reparent(&mut self, applied: ReparentApplied) {
        let key = applied.ticket.key.clone();
        self.pending_reparents.remove(&key);
        let parent = applied.parent.clone();
        self.graph.replace_relations_from(&key, applied.relations);
        self.graph.reparent(&key, parent.as_ref());
        if let Some(index) = self.index_of(&key) {
            self.set_ticket(index, applied.ticket);
            self.resettle_rows();
        }
        self.refresh_child_progress();
        self.set_status(match parent {
            Some(parent) => format!("Moved #{} under #{}", key.id, parent.id),
            None => format!("Detached #{}", key.id),
        });
    }

    /// A move that did not land, so the graph goes back the way it was — both
    /// halves of the link, and the child progress of both parents with them.
    pub fn reject_reparent(&mut self, rejection: &ReparentRejection) {
        if let Some(previous) = self.pending_reparents.remove(&rejection.key) {
            self.graph.reparent(&rejection.key, previous.as_ref());
            self.refresh_child_progress();
        }
        let tail = if rejection.conflict {
            " \u{b7} it changed in Azure DevOps; syncing"
        } else {
            ""
        };
        self.set_error(format!(
            "#{} not moved: {}{tail}",
            rejection.key.id, rejection.message
        ));
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
        if self.mode != AppMode::NodePicker {
            return;
        }
        let focused = self
            .node_matches()
            .get(self.node_picker.index)
            .map(|row| row.path.clone());
        let kind = self.node_picker.kind;
        let current = self.node_picker.current.clone();
        self.node_picker.rows = self.node_rows(kind);
        let matches = self.node_matches();
        let index = focused
            .and_then(|path| matches.iter().position(|row| row.path == path))
            .or_else(|| matches.iter().position(|row| row.path == current))
            .unwrap_or(self.node_picker.index)
            .min(matches.len().saturating_sub(1));
        self.focus_node(index);
    }

    /// The sprint the project is in: the deepest iteration whose dates contain
    /// today in UTC. `None` when no iteration is scheduled around today, which
    /// includes every project whose trees have never been fetched.
    #[must_use]
    pub fn current_iteration(&self) -> Option<String> {
        classification::current_iteration(&self.classification_nodes, Timestamp::now().date())
            .map(|node| node.path.clone())
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

    /// The Edit menu's Iteration and Area rows: the project's tree, indented,
    /// with the node the work item sits in already under the cursor. The rows
    /// come out of what is already in memory, so the picker opens at once; the
    /// trees are asked for the first time either picker is opened on a cache
    /// that is empty or over an hour old, and merged in when they arrive.
    ///
    /// Iteration is the one of the two worth making in bulk — a sprint ends
    /// and its leftovers move on together — so with two or more rows checked
    /// it moves all of them and says so in its title. Area stays on the row
    /// under the cursor.
    fn open_node_picker(&mut self, kind: NodeKind) -> AppAction {
        let scope = match kind {
            NodeKind::Iteration => self.edit_scope(),
            NodeKind::Area => {
                EditScope::Ticket(self.selected_ticket().map_or(0, |ticket| ticket.key.id))
            }
        };
        let Some(ticket) = self.selected_ticket() else {
            self.set_error("No work item is selected");
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
    fn show_node_picker(&mut self, kind: NodeKind, current: String, scope: EditScope) -> AppAction {
        let rows = self.node_rows(kind);
        let index = rows
            .iter()
            .position(|row| row.path == current)
            .unwrap_or_default();
        self.node_picker = NodePicker {
            kind,
            rows,
            query: TextInput::default(),
            index,
            scroll: ScrollState::default(),
            current,
            scope,
        };
        self.node_picker.scroll.ensure_visible(index);
        self.mode = AppMode::NodePicker;
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

    fn handle_node_picker_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.close_picker(self.node_picker.scope),
            KeyCode::Up => self.move_node_selection(-1),
            KeyCode::Down => self.move_node_selection(1),
            KeyCode::PageUp => self.move_node_selection(-5),
            KeyCode::PageDown => self.move_node_selection(5),
            KeyCode::Enter => return self.choose_node(self.node_picker.index),
            // Everything else is typing, the way it is in the assignee picker.
            _ => {
                let before = self.node_picker.query.text().to_owned();
                self.node_picker.query.handle_key(key);
                if self.node_picker.query.text() != before {
                    self.node_picker.index = 0;
                    self.node_picker.scroll.scroll_to(0);
                }
            }
        }
        AppAction::None
    }

    fn move_node_selection(&mut self, delta: isize) {
        let count = self.node_matches().len();
        if count == 0 {
            self.node_picker.index = 0;
            return;
        }
        let index = self
            .node_picker
            .index
            .saturating_add_signed(delta)
            .min(count - 1);
        self.focus_node(index);
    }

    fn focus_node(&mut self, index: usize) {
        self.node_picker.index = index;
        self.node_picker.scroll.ensure_visible(index);
    }

    /// Confirms one node. The node the work item already sits in is a no-op;
    /// anything else writes the full backslash path to `System.IterationPath`
    /// or `System.AreaPath`, and the table column goes on showing the leaf. An
    /// iteration picker opened over the checked rows moves every one of them,
    /// so the sprint the row under the cursor is in is a change to make to the
    /// rest rather than a no-op.
    fn choose_node(&mut self, index: usize) -> AppAction {
        let scope = self.node_picker.scope;
        let Some(row) = self.node_matches().get(index).cloned() else {
            self.close_picker(scope);
            return AppAction::None;
        };
        if let EditScope::Form(field) = scope {
            self.fill_form_field(field, row.path);
            return AppAction::None;
        }
        self.mode = AppMode::Browse;
        if !self.node_picker.scope.is_bulk() && row.path == self.node_picker.current {
            return AppAction::None;
        }
        match self.node_picker.kind {
            NodeKind::Iteration => self.edit_checked(FieldEdit::iteration(&row.path)),
            NodeKind::Area => self.edit_selected(FieldEdit::area(&row.path)),
        }
    }

    /// The Edit menu's Title and Tags rows: a single-line field prefilled with
    /// what the work item says now, edited with the same keys as the
    /// named-view editor.
    fn open_prompt(&mut self, field: PromptField) {
        let Some(ticket) = self.selected_ticket() else {
            self.set_error("No work item is selected");
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
        self.mode = AppMode::Prompt;
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.close_prompt(),
            KeyCode::Enter => return self.submit_prompt(),
            _ => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.input.handle_key(key);
                }
            }
        }
        AppAction::None
    }

    fn close_prompt(&mut self) {
        self.prompt = None;
        self.mode = AppMode::Browse;
    }

    /// Saves what the prompt holds. A title is trimmed, and one that is empty
    /// or only whitespace is refused here rather than sent, with the prompt
    /// left open on it. A tag list is normalised. Text that comes back to what
    /// the work item already says closes the prompt without a write.
    fn submit_prompt(&mut self) -> AppAction {
        let Some(prompt) = self.prompt.as_ref() else {
            self.mode = AppMode::Browse;
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
                    self.set_error(format!("#{} title cannot be empty", prompt.id));
                    return AppAction::None;
                }
                PromptField::Comment => {
                    self.set_error(format!("#{} comment cannot be empty", prompt.id));
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
            PromptField::Title => self.edit_selected(FieldEdit::title(&edited)),
            PromptField::Tags => self.edit_selected(FieldEdit::tags(&edited)),
            PromptField::Comment => self.comment_selected(edited),
        }
    }

    /// Asks for the selected work item's description to be opened in the
    /// user's editor. Nothing is written here: the action carries the markup
    /// out to the editor hand-off, which brings back whatever was saved and
    /// sends it down [`Self::edit_ticket`] like any other field. Only the
    /// refusals worth making before somebody spends minutes typing are made
    /// here.
    fn edit_description(&mut self) -> AppAction {
        let Some(ticket) = self.selected_ticket() else {
            self.set_error("No work item is selected");
            return AppAction::None;
        };
        let key = ticket.key.clone();
        let html = ticket.description_html.clone();
        if !self.sync_enabled {
            let reason = self
                .offline_reason
                .clone()
                .unwrap_or_else(|| "no Azure DevOps organization is configured".to_owned());
            self.set_error(format!("#{} description not saved: {reason}", key.id));
            return AppAction::None;
        }
        AppAction::EditDescription { key, html }
    }

    /// Asks for a comment to be left on the selected work item. Unlike a field
    /// edit nothing is shown until Azure DevOps has stored it: a comment has no
    /// id, date, or author until the server gives it one, and a line that
    /// turned out never to have been posted is worse than a moment's wait.
    pub fn comment_selected(&mut self, text: String) -> AppAction {
        let Some(key) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            self.set_error("No work item is selected");
            return AppAction::None;
        };
        let refusal = |reason: &str| format!("#{} comment not posted: {reason}", key.id);
        if !self.sync_enabled {
            let reason = self
                .offline_reason
                .clone()
                .unwrap_or_else(|| "no Azure DevOps organization is configured".to_owned());
            let message = refusal(&reason);
            self.set_error(message);
            return AppAction::None;
        }
        if self.pending_comments.contains(&key) {
            let message = refusal("an earlier comment is still in flight");
            self.set_error(message);
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
    pub fn apply_comment(&mut self, comment: CommentRecord) {
        self.pending_comments.remove(&comment.ticket);
        let id = comment.ticket.id;
        self.graph.add_comment(comment);
        self.set_status(format!("Commented on #{id}"));
    }

    /// A comment that never landed. Nothing was shown for it and nothing is
    /// stored, so only the notification is left to say so.
    pub fn reject_comment(&mut self, key: &TicketKey, message: &str) {
        self.pending_comments.remove(key);
        self.set_error(format!("#{} comment not posted: {message}", key.id));
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
        if self.mode != AppMode::TypePicker {
            return;
        }
        let focused = self
            .type_picker
            .options
            .get(self.type_picker.index)
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
            .unwrap_or(self.type_picker.index)
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

    /// Opens the new work item form: `n`. A draft `Esc` left behind comes back
    /// exactly as it was, cursor and all, so a form closed to go and read
    /// something is not a form retyped.
    pub fn open_create_form(&mut self) -> AppAction {
        if self.pending_create.is_some() {
            self.set_error("A work item is already being created");
            return AppAction::None;
        }
        let form = self.take_draft(FormKind::NewWorkItem).unwrap_or_else(|| {
            FormOverlay::new(
                FormKind::NewWorkItem,
                "New work item",
                self.create_form_fields(None),
            )
        });
        self.open_form(form)
    }

    /// Shows one form and asks for whatever it needs that is not in memory yet.
    /// Every form opens this way, so the placement, the cursor, and the single
    /// types fetch a session are the same for all of them.
    fn open_form(&mut self, form: FormOverlay) -> AppAction {
        self.form = Some(form);
        self.mode = AppMode::Form;
        self.overlay_anchor = OverlayAnchor::Centered;
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
    /// The iteration starts where the selected work item sits, falling back to
    /// the sprint the project is in, because new work almost always joins the
    /// work beside it. `parent` is filled in by whoever opened the form.
    #[must_use]
    fn create_form_fields(&self, parent: Option<i64>) -> Vec<FormField> {
        let iteration = self
            .selected_ticket()
            .map(|ticket| ticket.iteration_path.clone())
            .or_else(|| self.current_iteration())
            .unwrap_or_default();
        let parent_field = FormField::text(FormFieldId::Parent, "Parent")
            .with_placeholder("none — a work item id");
        vec![
            FormField::picker(FormFieldId::Type, "Type", FormPicker::WorkItemType)
                .required()
                .with_value(DEFAULT_WORK_ITEM_TYPE),
            FormField::text(FormFieldId::Title, "Title")
                .required()
                .with_placeholder("what needs doing"),
            match parent {
                Some(id) => parent_field.with_value(id.to_string()).read_only(),
                None => parent_field,
            },
            FormField::picker(FormFieldId::Iteration, "Iteration", FormPicker::Iteration)
                .with_value(iteration)
                .with_placeholder("the project root"),
            FormField::picker(FormFieldId::Assignee, "Assignee", FormPicker::Assignee)
                .with_placeholder("nobody"),
            FormField::text(FormFieldId::Priority, "Priority").with_placeholder("unset — 1 to 4"),
            FormField::text(FormFieldId::Tags, "Tags").with_placeholder("semicolon separated"),
        ]
    }

    fn handle_form_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.cancel_form(),
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.submit_form();
            }
            KeyCode::Up | KeyCode::BackTab => self.move_form_focus(-1),
            KeyCode::Down | KeyCode::Tab => self.move_form_focus(1),
            KeyCode::Enter => return self.activate_form_field(),
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

    fn focused_form_field_mut(&mut self) -> Option<&mut FormField> {
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

    fn focus_form_field(&mut self, index: usize) {
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
    fn activate_form_field(&mut self) -> AppAction {
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
            Some(FormPicker::Assignee) => {
                let current = (!current.trim().is_empty()).then_some(current);
                self.show_assignee_picker(current, EditScope::Form(id))
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
    fn fill_form_field(&mut self, id: FormFieldId, value: impl Into<String>) {
        if let Some(form) = self.form.as_mut() {
            form.set_value(id, value);
            if let Some(index) = form.index_of(id) {
                form.focus(index);
            }
        }
        self.mode = if self.form.is_some() {
            AppMode::Form
        } else {
            AppMode::Browse
        };
    }

    /// Where a picker goes when it closes with nothing chosen: back to the form
    /// that opened it, or to the table.
    fn close_picker(&mut self, scope: EditScope) {
        self.mode = if matches!(scope, EditScope::Form(_)) && self.form.is_some() {
            AppMode::Form
        } else {
            AppMode::Browse
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
    fn cancel_form(&mut self) {
        self.form_draft = self.form.take();
        self.mode = AppMode::Browse;
    }

    /// Files the form: `Ctrl-S`, or `[Create]`. Everything that can be refused
    /// before the network is refused here — a required field left empty, a
    /// parent or a priority that is not a number — with the form left open on
    /// the field at fault rather than a document of nonsense sent out.
    fn submit_form(&mut self) -> AppAction {
        let Some(form) = self.form.as_ref() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        if let Some(missing) = form.first_blank_required() {
            let (label, id) = (missing.label, missing.id);
            let index = form.index_of(id).unwrap_or_default();
            self.focus_form_field(index);
            self.set_error(format!("{label} is required"));
            return AppAction::None;
        }
        let parent = match form_number(form, FormFieldId::Parent) {
            Ok(parent) => parent,
            Err(message) => {
                self.refuse_form(FormFieldId::Parent, message);
                return AppAction::None;
            }
        };
        let priority = match form_number(form, FormFieldId::Priority) {
            Ok(priority) => priority,
            Err(message) => {
                self.refuse_form(FormFieldId::Priority, message);
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
        let tags = normalize_tags(form.value(FormFieldId::Tags));
        if !tags.is_empty() {
            edits.push(FieldEdit::tags(&tags));
        }
        if !self.sync_enabled {
            let reason = self
                .offline_reason
                .clone()
                .unwrap_or_else(|| "no Azure DevOps organization is configured".to_owned());
            self.set_error(format!("Work item not created: {reason}"));
            return AppAction::None;
        }
        let patch: Vec<Value> = edits.iter().flat_map(FieldEdit::patch).collect();
        // The form is held rather than dropped: a refusal has to put it back
        // with everything still in it.
        self.pending_create = self.form.take();
        self.mode = AppMode::Browse;
        self.set_status(format!("Creating {work_item_type}\u{2026}"));
        AppAction::Create {
            work_item_type,
            patch,
            parent,
        }
    }

    /// Refuses a submit and puts the cursor on the field that caused it, so the
    /// message and the caret name the same thing.
    fn refuse_form(&mut self, id: FormFieldId, message: String) {
        if let Some(index) = self.form.as_ref().and_then(|form| form.index_of(id)) {
            self.focus_form_field(index);
        }
        self.set_error(message);
    }

    /// Who a typed assignee names. A name the database already knows is written
    /// by the address the assignee picker would have used, and anything else
    /// goes out as it was typed for Azure DevOps to resolve — the same rule
    /// `ticket-tui create --assignee` follows.
    #[must_use]
    fn assignee_edit(&self, name: &str) -> FieldEdit {
        self.identities
            .iter()
            .find(|identity| {
                same_name(&identity.display_name, name)
                    || identity
                        .unique_name
                        .as_deref()
                        .is_some_and(|unique| same_name(unique, name))
            })
            .map_or_else(
                || FieldEdit::assignee(name, None),
                |identity| {
                    FieldEdit::assignee(&identity.display_name, identity.unique_name.as_deref())
                },
            )
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
    pub fn apply_created(&mut self, ticket: Ticket, relations: Vec<RelationRecord>) {
        self.pending_create = None;
        let key = ticket.key.clone();
        let headline = format!("Created {} #{}", ticket.work_item_type, key.id);
        let hidden = self.query_would_hide(&ticket);
        let index = self.tickets.len();
        Arc::make_mut(&mut self.tickets).push(ticket);
        self.search.push_document(index, &self.tickets[index]);
        self.graph.replace_relations_from(&key, relations);
        self.refresh_child_progress();
        if hidden {
            self.set_query(String::new());
        }
        self.show_all(Some(&key));
        self.details.scroll_to(0);
        self.set_status(if hidden {
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
    pub fn reject_create(&mut self, message: &str) {
        if let Some(form) = self.pending_create.take() {
            self.form = Some(form);
            self.mode = AppMode::Form;
            self.overlay_anchor = OverlayAnchor::Centered;
        }
        self.set_error(format!("Work item not created: {message}"));
    }

    /// The work item type picker, over the type the form names now.
    fn open_type_picker(&mut self, field: FormFieldId, current: String) {
        let options = self.work_item_type_options();
        let index = options
            .iter()
            .position(|name| *name == current)
            .unwrap_or_default();
        self.type_picker = TypePicker {
            options,
            index,
            scroll: ScrollState::default(),
            current,
            field,
        };
        self.type_picker.scroll.ensure_visible(index);
        self.mode = AppMode::TypePicker;
    }

    fn handle_type_picker_key(&mut self, key: KeyEvent) -> AppAction {
        let last = self.type_picker.options.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => self.close_picker(EditScope::Form(self.type_picker.field)),
            KeyCode::Up | KeyCode::Char('k') => {
                self.focus_type(self.type_picker.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_type((self.type_picker.index + 1).min(last));
            }
            KeyCode::PageUp => self.focus_type(self.type_picker.index.saturating_sub(5)),
            KeyCode::PageDown => self.focus_type((self.type_picker.index + 5).min(last)),
            KeyCode::Home => self.focus_type(0),
            KeyCode::End => self.focus_type(last),
            KeyCode::Enter => self.choose_work_item_type(self.type_picker.index),
            _ => {}
        }
        AppAction::None
    }

    fn focus_type(&mut self, index: usize) {
        self.type_picker.index = index;
        self.type_picker.scroll.ensure_visible(index);
    }

    /// Confirms one type, which writes it back into the form field that opened
    /// the picker. Nothing is sent anywhere: a form is not a work item yet.
    fn choose_work_item_type(&mut self, index: usize) {
        let field = self.type_picker.field;
        let Some(name) = self.type_picker.options.get(index).cloned() else {
            self.close_picker(EditScope::Form(field));
            return;
        };
        self.fill_form_field(field, name);
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
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
                self.focus_column(self.column_overlay.index.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.focus_column((self.column_overlay.index + 1).min(last));
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

    fn focus_column(&mut self, index: usize) {
        self.column_overlay.index = index;
        self.column_overlay.scroll.ensure_visible(index);
    }

    fn handle_palette_key(&mut self, key: KeyEvent) -> AppAction {
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

    fn run_selected_command(&mut self) -> AppAction {
        let Some(command) = self.palette_commands().get(self.palette.selected).copied() else {
            self.mode = AppMode::Browse;
            return AppAction::None;
        };
        self.mode = AppMode::Browse;
        self.run_command(command.id)
    }

    fn run_command(&mut self, id: CommandId) -> AppAction {
        // Every command opens its overlay centred; clicking a field sets its
        // anchor afterwards, so a picker never inherits the last one's.
        self.overlay_anchor = OverlayAnchor::Centered;
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
                self.toggle_narrow_details();
                AppAction::None
            }
            CommandId::ToggleSearchOrder => {
                if !self.fuzzy_query().is_empty() {
                    self.toggle_search_order();
                }
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
            CommandId::DatabaseInfo => {
                self.mode = AppMode::Info;
                AppAction::None
            }
            CommandId::Quit => {
                self.should_quit = true;
                AppAction::None
            }
            CommandId::ResetPaneSplit => {
                self.reset_pane_split();
                AppAction::None
            }
            CommandId::SetStaleThreshold => {
                self.cycle_stale_days();
                AppAction::None
            }
        }
    }

    fn handle_views_key(&mut self, key: KeyEvent) -> AppAction {
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

    fn save_view(&mut self, name: String) {
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
    fn apply_view_at(&mut self, index: usize) {
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

    fn delete_view_at(&mut self, index: usize) {
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

    fn copy_with(&self, content: CopiedContent, formatter: fn(&[&Ticket]) -> String) -> AppAction {
        let tickets = self.export_targets();
        if tickets.is_empty() {
            return AppAction::None;
        }
        AppAction::Copy {
            text: formatter(&tickets),
            content,
        }
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
            query: self.query.text().to_owned(),
            sort_field: self.sort_field,
            sort_direction: self.sort_direction,
            search_order: self.search_order,
            row_density: self.row_density,
            columns: self.layout.to_session_columns(),
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
            pane_split_wide: self.pane_split_wide,
            pane_split_stacked: self.pane_split_stacked,
            stale_days: self.stale_days,
        }
    }

    pub fn restore_session(&mut self, session: Session) {
        self.sort_field = session.sort_field;
        self.sort_direction = session.sort_direction;
        self.search_order = session.search_order;
        self.row_density = session.row_density;
        self.layout = TableLayout::from_session_columns(&session.columns, session.auto_hide);
        self.pane_split_wide = session
            .pane_split_wide
            .clamp(MIN_SPLIT_PERCENT, MAX_SPLIT_PERCENT);
        self.pane_split_stacked = session
            .pane_split_stacked
            .clamp(MIN_SPLIT_PERCENT, MAX_SPLIT_PERCENT);
        self.stale_days = clamp_stale_days(session.stale_days);
        self.bookmarks = session.bookmarks.iter().map(TicketKey::from).collect();
        self.recent = session.recent.iter().map(TicketKey::from).collect();
        self.views = session
            .views
            .into_iter()
            .filter(|view| builtin_named(&view.name).is_none())
            .collect();
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

const fn mode_name(mode: AppMode) -> &'static str {
    match mode {
        AppMode::Browse => "browse",
        AppMode::Search => "search",
        AppMode::Sort => "sort",
        AppMode::Help => "help",
        AppMode::Filter => "filter",
        AppMode::Columns => "columns",
        AppMode::Palette => "palette",
        AppMode::Views => "views",
        AppMode::Info => "info",
        AppMode::Facets => "facets",
        AppMode::Edit => "edit",
        AppMode::StatePicker => "state-picker",
        AppMode::PriorityPicker => "priority-picker",
        AppMode::Prompt => "prompt",
        AppMode::AssigneePicker => "assignee-picker",
        AppMode::NodePicker => "node-picker",
        AppMode::Form => "form",
        AppMode::TypePicker => "type-picker",
        AppMode::ParentPicker => "parent-picker",
    }
}

const fn focus_name(focus: Focus) -> &'static str {
    match focus {
        Focus::Tickets => "tickets",
        Focus::Family => "family",
        Focus::Details => "details",
    }
}

/// Holds a threshold at or above the one-day floor, wherever it came from: a
/// flag, a variable, or a session file written by hand.
const fn clamp_stale_days(days: u16) -> u16 {
    if days < MIN_STALE_DAYS {
        MIN_STALE_DAYS
    } else {
        days
    }
}

/// Turns a divider position, measured in cells from the start of the workspace,
/// into a percentage for the first pane. The clamp keeps `first_min` cells for
/// that pane and `second_min` cells plus the one-cell divider for the other,
/// then holds the result inside the 20..=80 safety rails.
fn split_percent(cells: u16, span: u16, first_min: u16, second_min: u16) -> u16 {
    if span == 0 {
        return MIN_SPLIT_PERCENT;
    }
    let span = u32::from(span);
    let low = (u32::from(first_min) * 100)
        .div_ceil(span)
        .clamp(u32::from(MIN_SPLIT_PERCENT), u32::from(MAX_SPLIT_PERCENT));
    let high = (span.saturating_sub(u32::from(second_min) + 1) * 100 / span)
        .min(u32::from(MAX_SPLIT_PERCENT))
        .max(low);
    let percent = u32::from(cells) * 100 / span;
    u16::try_from(percent.clamp(low, high)).unwrap_or(MIN_SPLIT_PERCENT)
}

fn clamp_pos_to_snapshot(
    snapshot: &pointer::SelectableSnapshot,
    column: u16,
    row: u16,
) -> Option<TextPos> {
    if snapshot.cells.is_empty() {
        return None;
    }
    let line = if row < snapshot.rect.y {
        0
    } else {
        usize::from(row.saturating_sub(snapshot.rect.y)).min(snapshot.cells.len() - 1)
    };
    let width = snapshot.cells[line].len();
    let col = if column < snapshot.rect.x {
        0
    } else {
        usize::from(column.saturating_sub(snapshot.rect.x)).min(width)
    };
    Some(TextPos { line, col })
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::model::StateCategory;

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
            description_html: String::new(),
            created_at: crate::timestamp::ts("2026-01-01T00:00:00Z"),
            changed_at: crate::timestamp::ts(changed_at),
            web_url: format!("https://dev.azure.com/demo/atlas/_workitems/edit/{id}"),
            details_rev: 0,
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

    /// Types one line into the focused form field.
    fn type_text(app: &mut App, text: &str) {
        for character in text.chars() {
            press(app, KeyCode::Char(character));
        }
    }

    /// Moves the form cursor onto one field by name.
    fn focus_field(app: &mut App, id: FormFieldId) {
        let index = app
            .form
            .as_ref()
            .and_then(|form| form.index_of(id))
            .expect("the form has that field");
        app.focus_form_field(index);
    }

    /// An app that can write, with one work item to hang new work under.
    fn creating_app() -> App {
        let mut app = App::new(vec![ticket(10, "Sync timer", "2026-01-01T00:00:00Z")]);
        app.enable_sync();
        app
    }

    /// A work item as Azure DevOps hands one back from a create: an id, a
    /// revision, and a URL only the server could have given it.
    fn created(id: i64, work_item_type: &str, title: &str) -> Ticket {
        Ticket {
            work_item_type: work_item_type.into(),
            title: title.into(),
            state: "To Do".into(),
            assigned_to: None,
            priority: None,
            ..ticket(id, title, "2026-08-29T12:00:00Z")
        }
    }

    #[test]
    fn walking_a_form_moves_a_field_at_a_time_and_wraps_at_both_ends() {
        let mut app = creating_app();
        press(&mut app, KeyCode::Char('n'));

        assert_eq!(app.mode, AppMode::Form);
        let fields = app.form.as_ref().expect("the form is open").fields.len();
        assert_eq!(
            fields, 7,
            "type, title, parent, iteration, assignee, priority, tags"
        );
        assert_eq!(app.form.as_ref().unwrap().index, 0);

        press(&mut app, KeyCode::Down);
        assert_eq!(app.form.as_ref().unwrap().index, 1, "down moves on");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.form.as_ref().unwrap().index, 2, "tab moves on too");
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.form.as_ref().unwrap().index, 1, "shift-tab moves back");
        press(&mut app, KeyCode::Up);
        assert_eq!(app.form.as_ref().unwrap().index, 0);

        press(&mut app, KeyCode::Up);
        assert_eq!(
            app.form.as_ref().unwrap().index,
            fields - 1,
            "up from the first field wraps to the last"
        );
        press(&mut app, KeyCode::Down);
        assert_eq!(
            app.form.as_ref().unwrap().index,
            0,
            "and down from the last comes back to the first"
        );
    }

    #[test]
    fn enter_on_a_picker_field_opens_that_picker_and_the_choice_lands_in_the_form() {
        let mut app = creating_app();
        app.set_work_item_types(vec!["Epic".into(), "Issue".into(), "Task".into()]);
        app.set_identities(vec![Identity::new(
            "Avery Chen",
            Some("avery@example.com".into()),
        )]);
        app.set_classification_nodes(
            vec![ClassificationNode {
                kind: NodeKind::Iteration,
                path: "Atlas\\Sprint 2".into(),
                depth: 1,
                start_date: None,
                finish_date: None,
            }],
            Some(Timestamp::now()),
        );
        press(&mut app, KeyCode::Char('n'));

        focus_field(&mut app, FormFieldId::Type);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, AppMode::TypePicker);
        assert_eq!(app.type_picker.options, ["Epic", "Issue", "Task"]);
        press(&mut app, KeyCode::Home);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, AppMode::Form, "the picker hands back to the form");
        assert_eq!(app.form.as_ref().unwrap().value(FormFieldId::Type), "Epic");

        focus_field(&mut app, FormFieldId::Iteration);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, AppMode::NodePicker);
        assert_eq!(
            app.node_picker.scope,
            EditScope::Form(FormFieldId::Iteration),
            "the picker knows which field it is filling in"
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, AppMode::Form);
        assert_eq!(
            app.form.as_ref().unwrap().value(FormFieldId::Iteration),
            "Atlas\\Sprint 2"
        );

        focus_field(&mut app, FormFieldId::Assignee);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, AppMode::AssigneePicker);
        let row = app
            .assignee_matches()
            .iter()
            .position(|candidate| candidate.display == "Avery Chen")
            .expect("the picker offers the person the project knows");
        app.choose_assignee(row);
        assert_eq!(app.mode, AppMode::Form);
        assert_eq!(
            app.form.as_ref().unwrap().value(FormFieldId::Assignee),
            "Avery Chen"
        );

        focus_field(&mut app, FormFieldId::Title);
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.mode,
            AppMode::Form,
            "enter on a typed field moves on rather than filing the form"
        );
    }

    #[test]
    fn escaping_a_picker_a_form_opened_goes_back_to_the_form_rather_than_the_table() {
        let mut app = creating_app();
        press(&mut app, KeyCode::Char('n'));
        focus_field(&mut app, FormFieldId::Iteration);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, AppMode::NodePicker);

        press(&mut app, KeyCode::Esc);

        assert_eq!(app.mode, AppMode::Form);
        assert!(app.form.is_some(), "the form is still open behind it");
    }

    #[test]
    fn submitting_a_form_sends_the_fields_it_holds_and_the_parent_as_a_link() {
        let mut app = creating_app();
        app.set_identities(vec![Identity::new(
            "Avery Chen",
            Some("avery@example.com".into()),
        )]);
        press(&mut app, KeyCode::Char('n'));

        focus_field(&mut app, FormFieldId::Title);
        type_text(&mut app, "Back off on throttling");
        focus_field(&mut app, FormFieldId::Parent);
        type_text(&mut app, "10");
        focus_field(&mut app, FormFieldId::Assignee);
        app.form
            .as_mut()
            .unwrap()
            .set_value(FormFieldId::Assignee, "Avery Chen");
        focus_field(&mut app, FormFieldId::Priority);
        type_text(&mut app, "2");
        focus_field(&mut app, FormFieldId::Tags);
        type_text(&mut app, "sync;  infra");

        let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

        let AppAction::Create {
            work_item_type,
            patch,
            parent,
        } = action
        else {
            panic!("Ctrl-S files the form");
        };
        assert_eq!(work_item_type, "Issue", "the default type is filed as is");
        assert_eq!(parent, Some(10));
        assert_eq!(
            patch,
            vec![
                crate::edit::set_field(crate::edit::TITLE_FIELD, "Back off on throttling"),
                crate::edit::set_field(crate::edit::ASSIGNED_TO_FIELD, "avery@example.com"),
                crate::edit::set_field(crate::edit::PRIORITY_FIELD, 2),
                crate::edit::set_field(crate::edit::ITERATION_PATH_FIELD, "Atlas\\Sprint 1"),
                crate::edit::set_field(crate::edit::TAGS_FIELD, "sync; infra"),
            ],
            "the fields travel in the order the form holds them, and only the ones filled in"
        );

        let config = crate::azure::AzureConfig {
            organization: "demo".into(),
            project: "atlas".into(),
            scope: None,
        };
        let document = crate::azure::create_document(&patch, parent, &config);
        assert_eq!(&document[..patch.len()], &patch[..], "the fields lead");
        assert_eq!(
            document[patch.len()],
            serde_json::json!({
                "op": "add",
                "path": "/relations/-",
                "value": {
                    "rel": "System.LinkTypes.Hierarchy-Reverse",
                    "url": "https://dev.azure.com/demo/_apis/wit/workItems/10",
                },
            }),
            "the parent travels as a link rather than as a field"
        );
        assert!(app.creates_pending(), "the form is held until it answers");
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Creating Issue\u{2026}")
        );
    }

    #[test]
    fn a_form_missing_a_required_field_or_holding_nonsense_refuses_to_be_sent() {
        let mut app = creating_app();
        press(&mut app, KeyCode::Char('n'));

        let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(action, AppAction::None, "nothing goes out without a title");
        assert_eq!(app.mode, AppMode::Form, "the form stays open on it");
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Title is required")
        );
        assert_eq!(
            app.form.as_ref().unwrap().focused().unwrap().id,
            FormFieldId::Title,
            "the cursor lands on the field the refusal names"
        );

        focus_field(&mut app, FormFieldId::Title);
        type_text(&mut app, "Something to do");
        app.form
            .as_mut()
            .unwrap()
            .set_value(FormFieldId::Type, "   ");
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(action, AppAction::None);
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Type is required")
        );

        app.form
            .as_mut()
            .unwrap()
            .set_value(FormFieldId::Type, "Issue");
        focus_field(&mut app, FormFieldId::Priority);
        type_text(&mut app, "high");
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(action, AppAction::None, "garbage is refused, not sent");
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Priority must be a whole number, not \"high\"")
        );
        assert!(!app.creates_pending());
    }

    #[test]
    fn escape_keeps_the_draft_and_opening_the_form_again_brings_it_back() {
        let mut app = creating_app();
        press(&mut app, KeyCode::Char('n'));
        focus_field(&mut app, FormFieldId::Title);
        type_text(&mut app, "Half a thought");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, AppMode::Browse);
        assert!(app.form.is_none(), "the form is closed");

        press(&mut app, KeyCode::Char('n'));

        assert_eq!(app.mode, AppMode::Form);
        let form = app.form.as_ref().expect("the draft came back");
        assert_eq!(form.value(FormFieldId::Title), "Half a thought");
        assert_eq!(
            form.focused().unwrap().id,
            FormFieldId::Title,
            "and the cursor came back with it"
        );
    }

    #[test]
    fn a_created_work_item_joins_the_rows_with_its_family_and_the_selection_follows_it() {
        let mut app = creating_app();
        let parent = app.tickets()[0].key.clone();
        press(&mut app, KeyCode::Char('n'));
        app.form
            .as_mut()
            .unwrap()
            .set_value(FormFieldId::Title, "Honour Retry-After");
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

        let child = created(42, "Issue", "Honour Retry-After");
        let key = child.key.clone();
        app.apply_created(
            child,
            vec![RelationRecord {
                from: key.clone(),
                to: parent.clone(),
                kind: RelationKind::Parent,
            }],
        );

        assert!(!app.creates_pending(), "the create has answered");
        assert_eq!(app.tickets().len(), 2, "the new row joined the table");
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.key.id),
            Some(42),
            "and the selection moved onto it"
        );
        assert_eq!(
            app.family_of(&key).ancestors,
            vec![parent.clone()],
            "the child knows its parent"
        );
        assert_eq!(
            app.family_of(&parent).children,
            vec![key],
            "and the parent knows its child"
        );
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Created Issue #42")
        );
    }

    #[test]
    fn a_created_work_item_the_query_would_hide_clears_it_and_says_so() {
        let mut app = creating_app();
        app.set_query("type:Task".into());
        assert_eq!(app.visible_count(), 1);

        app.apply_created(created(42, "Issue", "Honour Retry-After"), Vec::new());

        assert_eq!(app.query(), "", "a row nobody could see is worth no filter");
        assert_eq!(app.visible_count(), 2);
        assert_eq!(app.selected_ticket().map(|ticket| ticket.key.id), Some(42));
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Created Issue #42 \u{b7} search cleared so it is visible")
        );
    }

    #[test]
    fn a_created_work_item_the_query_already_admits_leaves_it_alone() {
        let mut app = creating_app();
        app.set_query("type:Issue".into());

        app.apply_created(created(42, "Issue", "Honour Retry-After"), Vec::new());

        assert_eq!(app.query(), "type:Issue", "the filter still holds");
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Created Issue #42")
        );
    }

    #[test]
    fn a_refused_create_reopens_the_form_with_everything_still_in_it() {
        let mut app = creating_app();
        press(&mut app, KeyCode::Char('n'));
        focus_field(&mut app, FormFieldId::Title);
        type_text(&mut app, "Honour Retry-After");
        focus_field(&mut app, FormFieldId::Tags);
        type_text(&mut app, "sync");
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.form.is_none(), "the form is out with the request");

        app.reject_create("the work item type Issue is not in this project");

        assert_eq!(app.mode, AppMode::Form, "the form comes straight back");
        let form = app.form.as_ref().expect("with the draft in it");
        assert_eq!(form.value(FormFieldId::Title), "Honour Retry-After");
        assert_eq!(form.value(FormFieldId::Tags), "sync");
        assert!(!app.creates_pending(), "nothing is in flight any more");
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Work item not created: the work item type Issue is not in this project")
        );
        assert_eq!(app.tickets().len(), 1, "and no row was ever shown for it");
    }

    #[test]
    fn agent_context_describes_the_live_ticket_workspace() {
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            ticket(3, "Gamma", "2026-03-01T00:00:00Z"),
        ]);
        app.configure_database(PathBuf::from("/tmp/tickets.sqlite3"), 0);
        app.set_table_viewport(2);
        app.set_query("state:Active".into());
        app.toggle_row_selection();
        app.focus = Focus::Details;
        app.mode = AppMode::Filter;
        app.active_view = Some("Active work".into());

        let context = app.agent_context();

        assert_eq!(context.database_path, "/tmp/tickets.sqlite3");
        assert_eq!(context.mode, "filter");
        assert_eq!(context.focus, "details");
        assert_eq!(context.active_view.as_deref(), Some("Active work"));
        assert_eq!(context.search.filters, vec!["state:Active"]);
        assert_eq!(context.tickets.total_count, 3);
        assert_eq!(context.tickets.matching_count, 3);
        assert_eq!(context.tickets.visible_rows.len(), 2);
        assert_eq!(context.selected_ticket.as_ref().unwrap().id, 3);
        assert!(context.selected_ticket.as_ref().unwrap().checked);
        assert_eq!(context.checked_tickets.len(), 1);
        assert_eq!(context.checked_tickets[0].id, 3);

        let mut mine = app.tickets()[0].clone();
        mine.assigned_to = Some("  avery CHEN ".into());
        let mut theirs = app.tickets()[1].clone();
        theirs.assigned_to = Some("Jordan Patel".into());
        let mut unassigned = app.tickets()[1].clone();
        unassigned.assigned_to = None;
        assert!(!app.is_mine(&mine), "nobody is \"me\" until a name is set");

        app.set_me(Some("Avery Chen".into()));

        assert_eq!(app.me(), Some("Avery Chen"));
        assert!(app.is_mine(&mine), "casing and padding do not matter");
        assert!(!app.is_mine(&theirs));
        assert!(!app.is_mine(&unassigned));
        assert_eq!(app.agent_context().me.as_deref(), Some("Avery Chen"));
    }

    #[test]
    fn the_agent_context_says_where_the_rows_come_from_and_how_the_last_pull_went() {
        let mut app = App::new(vec![ticket(1, "Alpha", "2026-01-01T00:00:00Z")]);

        let offline = app.agent_context().sync;
        assert!(offline.offline, "a run with no organization cannot sync");
        assert_eq!(offline.organization, None);
        assert_eq!(offline.project, None);
        assert_eq!(offline.refresh_seconds, 0);
        assert_eq!(offline.last_success_at, None);
        assert_eq!(offline.last_error, None);

        app.enable_sync();
        app.set_sync_target(Some(SyncTarget {
            organization: "example-org".into(),
            project: "atlas".into(),
            refresh_seconds: 60,
        }));
        app.begin_sync();

        let running = app.agent_context().sync;
        assert!(!running.offline);
        assert_eq!(running.organization.as_deref(), Some("example-org"));
        assert_eq!(running.project.as_deref(), Some("atlas"));
        assert_eq!(running.refresh_seconds, 60);
        assert!(running.in_progress, "a pull is out");

        app.finish_sync();

        let succeeded = app.agent_context().sync;
        assert!(!succeeded.in_progress);
        assert_eq!(succeeded.last_error, None);
        let landed = succeeded.last_success_at.expect("a pull landed");
        assert!(
            Timestamp::parse(&landed).is_ok(),
            "the last sync is RFC 3339: {landed}"
        );

        app.begin_sync();
        app.fail_sync("network unreachable", true);

        let failed = app.agent_context().sync;
        assert!(!failed.in_progress);
        assert_eq!(failed.last_error.as_deref(), Some("network unreachable"));
        assert_eq!(
            failed.last_success_at.as_deref(),
            Some(landed.as_str()),
            "a failure does not erase when the rows last arrived"
        );

        app.finish_sync();
        assert_eq!(
            app.agent_context().sync.last_error,
            None,
            "the next success clears the error"
        );
    }

    #[test]
    fn pending_edits_are_published_while_in_flight_and_gone_once_answered() {
        let mut app = editing_app();
        let request = edit_request(&mut app, FieldEdit::state("Doing"));

        let pending = app.agent_context().pending_edits;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, request.key.id);
        assert_eq!(pending[0].field, "State");
        assert_eq!(pending[0].value, "Doing");
        assert!(
            Timestamp::parse(&pending[0].since).is_ok(),
            "the dispatch time is RFC 3339: {}",
            pending[0].since
        );

        app.apply_edit(EditApplied {
            ticket: stored_copy(&app, &request.key, "Doing"),
            relations: Vec::new(),
            edit: request.edit,
        });
        assert!(
            app.agent_context().pending_edits.is_empty(),
            "an edit that landed is no longer in flight"
        );

        let refused = edit_request(&mut app, FieldEdit::priority(1));
        assert_eq!(app.agent_context().pending_edits.len(), 1);

        app.reject_edit(&EditRejection {
            key: refused.key,
            label: "Priority".into(),
            conflict: false,
            message: "field is read only".into(),
        });
        assert!(
            app.agent_context().pending_edits.is_empty(),
            "a refused edit is no longer in flight either"
        );
    }

    #[test]
    fn search_order_switches_between_relevance_and_field_sorting_and_keeps_the_selection() {
        let mut app = App::new(vec![
            ticket(1, "Search alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Search beta", "2026-02-01T00:00:00Z"),
        ]);
        app.select_row(1);
        let selected = app.selected_ticket().unwrap().key.clone();
        assert_eq!(selected.id, 1, "the newest ticket leads by default");

        app.set_query("search".into());
        await_search(&mut app);
        app.set_sort(SortField::Title, SortDirection::Ascending);
        assert_eq!(app.selected_ticket().unwrap().key, selected);

        app.visible = vec![
            SearchMatch {
                ticket_index: 1,
                score: 100,
            },
            SearchMatch {
                ticket_index: 0,
                score: 1,
            },
        ];
        app.sort_visible();
        assert_eq!(app.search_order, SearchOrder::Relevance);
        assert_eq!(
            app.visible_tickets().next().unwrap().key.id,
            2,
            "relevance leads with the best scoring match"
        );

        app.toggle_search_order();

        assert_eq!(app.search_order, SearchOrder::Field);
        assert_eq!(
            app.visible_tickets().next().unwrap().key.id,
            1,
            "field order falls back to the sort column"
        );

        let mut without_fuzzy = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        let order = without_fuzzy.search_order;
        without_fuzzy.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        assert_eq!(
            without_fuzzy.search_order, order,
            "there is nothing to re-rank without a fuzzy query"
        );
    }

    #[test]
    fn pasting_fills_the_search_editor_and_escape_clears_the_query() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.mode = AppMode::Search;
        app.handle_paste("search\n");
        assert_eq!(app.query(), "search ");
        assert_eq!(app.query_cursor(), 7);
        app.mode = AppMode::Browse;

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(app.query().is_empty());
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
    fn sorting_and_reload_keep_the_view_context_unless_the_selection_is_gone() {
        let original = vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            ticket(3, "Gamma", "2026-03-01T00:00:00Z"),
        ];
        let mut app = App::new(original.clone());
        assert_eq!(
            app.visible_tickets().next().unwrap().key.id,
            3,
            "tickets start sorted by most recently changed"
        );
        app.select_row(1);
        let selected = app.selected_ticket().unwrap().key.clone();
        app.details.set_viewport(0, 5);
        app.details.scroll_to(3);
        app.table.offset = 1;
        app.table.viewport = 2;

        app.set_sort(SortField::Title, SortDirection::Descending);
        assert_eq!(app.selected_ticket().unwrap().key, selected);
        assert_eq!(app.details.offset, 3);
        assert_eq!(app.table.offset, 1);

        app.replace_tickets(original);
        assert_eq!(app.selected_ticket().unwrap().key, selected);
        assert_eq!(app.details.offset, 3);
        assert_eq!(app.table.offset, 1);

        app.replace_tickets(vec![ticket(9, "Delta", "2026-03-01T00:00:00Z")]);
        assert_eq!(app.selected_ticket().unwrap().key.id, 9);
        assert_eq!(app.details.offset, 0, "a lost selection resets the details");
        assert_eq!(app.table.offset, 0, "a lost selection resets the table");
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
    fn a_facet_toggle_rewrites_the_query_and_removing_the_chip_clears_it() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.open_filters();
        app.filter_overlay.showing_values = true;
        app.filter_overlay.field_index = 0;
        app.toggle_current_facet();

        assert!(app.query().contains("state:"));
        let token = app.filter_tokens().pop().unwrap();
        app.remove_filter_token(token);
        assert!(app.query().is_empty());
    }

    #[test]
    fn named_views_restore_query_and_sort() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.set_query("state:active".into());
        app.set_sort(SortField::Title, SortDirection::Ascending);
        app.save_view("Active".into());
        app.set_query(String::new());
        app.set_sort(SortField::Changed, SortDirection::Descending);

        app.apply_view_at(view_row(&app, "Active"));

        assert_eq!(app.query(), "state:active");
        assert_eq!(app.sort_field, SortField::Title);
        assert_eq!(app.active_view.as_deref(), Some("Active"));
    }

    /// A workspace holding one work item for each daily question: mine and
    /// moving, nobody's, two that have gone quiet, one finished long ago, and
    /// one more planned into the sprint running today.
    fn views_app() -> App {
        let today = Timestamp::now().calendar_date();
        let row = |id: i64,
                   title: &str,
                   state: &str,
                   assignee: Option<&str>,
                   iteration: &str,
                   changed: &str| Ticket {
            state: state.into(),
            assigned_to: assignee.map(str::to_owned),
            iteration_path: iteration.into(),
            ..ticket(id, title, changed)
        };
        let sprint = "development\\Sprint 1";
        let quarter = "development\\Q3";
        let mut app = App::new(vec![
            row(
                1,
                "Mine and moving",
                "Doing",
                Some("Avery Chen"),
                sprint,
                &format!("{today}T09:00:00Z"),
            ),
            row(
                2,
                "Nobody has this",
                "To Do",
                None,
                quarter,
                &format!("{today}T08:00:00Z"),
            ),
            row(
                3,
                "Gone quiet",
                "To Do",
                Some("Jordan Patel"),
                quarter,
                "2020-01-01T00:00:00Z",
            ),
            row(
                4,
                "Quieter still",
                "To Do",
                Some("Jordan Patel"),
                quarter,
                "2019-01-01T00:00:00Z",
            ),
            row(
                5,
                "Finished long ago",
                "Done",
                Some("Avery Chen"),
                quarter,
                "2018-01-01T00:00:00Z",
            ),
            row(
                6,
                "Also this sprint",
                "To Do",
                Some("Jordan Patel"),
                sprint,
                &format!("{today}T07:00:00Z"),
            ),
        ]);
        app.set_me(Some("Avery Chen".into()));
        app.set_classification_nodes(classification_trees(), None);
        app
    }

    /// Where a view sits in the overlay, which is not its position among the
    /// user's own views: the built-ins and the headings are counted too.
    fn view_row(app: &App, name: &str) -> usize {
        app.view_rows()
            .iter()
            .position(|row| !row.is_heading() && row.label == name)
            .unwrap_or_else(|| panic!("no view named {name}"))
    }

    fn visible_ids(app: &App) -> Vec<i64> {
        app.visible_tickets().map(|ticket| ticket.key.id).collect()
    }

    #[test]
    fn the_views_overlay_lists_the_built_ins_above_whatever_the_user_saved() {
        let mut app = views_app();

        let rows = app.view_rows();
        let listed: Vec<(&str, &str)> = rows
            .iter()
            .map(|row| (row.label.as_str(), row.query.as_str()))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("Built-in", ""),
                ("Mine", "assignee:@me"),
                ("Unassigned", "assignee:@none"),
                ("Doing", "state:doing"),
                ("Stale", "changed:>14d state:@open"),
                ("Current sprint", "iteration:@current"),
            ]
        );
        assert!(rows[0].is_heading());
        assert!(
            rows[1..].iter().all(|row| !row.is_heading()),
            "with nothing saved there is no second heading to show"
        );

        app.set_query("tag:rust".into());
        app.save_view("Rust work".into());

        let rows = app.view_rows();
        assert_eq!(rows.len(), 8);
        assert!(rows[6].is_heading());
        assert_eq!(rows[6].label, "Saved");
        assert_eq!(rows[7].label, "Rust work");
        assert!(rows[7].active, "the view just saved is the one on screen");
    }

    #[test]
    fn each_built_in_view_yields_the_rows_its_question_asks_for() {
        let mut app = views_app();
        let load = |app: &mut App, name: &str| {
            app.apply_view_at(view_row(app, name));
            visible_ids(app)
        };

        assert_eq!(load(&mut app, "Mine"), vec![1, 5]);
        assert_eq!(load(&mut app, "Unassigned"), vec![2]);
        assert_eq!(load(&mut app, "Doing"), vec![1]);
        assert_eq!(load(&mut app, "Current sprint"), vec![1, 6]);

        assert_eq!(app.active_view.as_deref(), Some("Current sprint"));
        assert_eq!(app.query(), "iteration:@current");
        assert_eq!(
            app.mode,
            AppMode::Browse,
            "loading a view closes the overlay"
        );
    }

    #[test]
    fn the_stale_view_leaves_out_finished_work_and_puts_the_quietest_row_first() {
        let mut app = views_app();

        app.apply_view_at(view_row(&app, "Stale"));

        assert_eq!(app.query(), "changed:>14d state:@open");
        assert_eq!(
            (app.sort_field, app.sort_direction),
            (SortField::Changed, SortDirection::Ascending),
            "the one built-in that turns the default order around"
        );
        assert_eq!(
            visible_ids(&app),
            vec![4, 3],
            "the longest untouched row leads, and the Done row nobody has \
             touched since 2018 is not waiting on anybody"
        );
    }

    #[test]
    fn a_built_in_view_cannot_be_saved_over_or_deleted() {
        let mut app = views_app();
        app.set_query("tag:rust".into());

        app.save_view("mine".into());

        assert!(app.views().is_empty(), "a built-in owns its name");
        assert_eq!(app.view_rows().len(), 6, "and no second Mine is listed");
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("'Mine' is a built-in view; choose another name")
        );

        app.delete_view_at(view_row(&app, "Mine"));

        assert_eq!(app.view_rows().len(), 6);
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("'Mine' is a built-in view and cannot be deleted")
        );
    }

    #[test]
    fn the_views_cursor_opens_on_the_first_built_in_and_steps_over_the_headings() {
        let mut app = views_app();
        app.set_query("tag:rust".into());
        app.save_view("Rust work".into());

        app.open_views();
        assert_eq!(
            app.views_overlay.index, 1,
            "row zero is the Built-in heading"
        );

        for _ in 0..4 {
            press(&mut app, KeyCode::Down);
        }
        assert_eq!(app.views_overlay.index, 5, "the last built-in");
        press(&mut app, KeyCode::Down);
        assert_eq!(
            app.views_overlay.index, 7,
            "the Saved heading is stepped over"
        );
        press(&mut app, KeyCode::Down);
        assert_eq!(app.views_overlay.index, 7, "and the list stops at its end");
        assert!(app.can_delete_focused_view(), "a saved view can be deleted");

        press(&mut app, KeyCode::Up);
        assert_eq!(app.views_overlay.index, 5);
        assert!(!app.can_delete_focused_view(), "a built-in cannot");
    }

    /// `TICKET_TUI_ME` is resolved against the last sync's display name by
    /// `resolve_me` before the app is told who it is, so a different name here
    /// is exactly what the override produces.
    #[test]
    fn the_mine_view_follows_the_name_the_session_is_signed_in_under() {
        let mut app = views_app();
        app.apply_view_at(view_row(&app, "Mine"));
        assert_eq!(visible_ids(&app), vec![1, 5]);

        app.set_me(Some("Jordan Patel".into()));
        app.show_all(None);
        assert_eq!(
            visible_ids(&app),
            vec![6, 3, 4],
            "the saved query is unchanged; the name under it is not"
        );

        app.set_me(None);
        app.show_all(None);
        assert!(
            visible_ids(&app).is_empty(),
            "with nobody signed in @me is nobody rather than everybody"
        );
    }

    #[test]
    fn the_current_sprint_view_follows_the_iteration_dates_rather_than_a_written_path() {
        let mut app = views_app();
        app.apply_view_at(view_row(&app, "Current sprint"));

        assert_eq!(
            app.current_iteration(),
            Some("development\\Sprint 1".to_owned())
        );
        assert_eq!(visible_ids(&app), vec![1, 6]);

        let today =
            Timestamp::parse(&format!("{}T00:00:00Z", Timestamp::now().calendar_date())).unwrap();
        let rolled_over: Vec<ClassificationNode> = classification_trees()
            .into_iter()
            .map(|node| {
                let current = node.path == "development\\Q3";
                ClassificationNode {
                    start_date: current.then_some(today),
                    finish_date: current.then_some(today),
                    ..node
                }
            })
            .collect();
        app.set_classification_nodes(rolled_over, None);
        app.show_all(None);
        assert_eq!(
            visible_ids(&app),
            vec![2, 3, 4, 5],
            "the same saved query follows the sprint over its rollover"
        );

        app.set_classification_nodes(Vec::new(), None);
        app.show_all(None);
        assert!(
            visible_ids(&app).is_empty(),
            "with no sprint scheduled @current is no sprint at all"
        );
    }

    #[test]
    fn sentinels_come_back_from_the_session_file_as_the_chips_they_were_typed_as() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tickets.session.json");
        let mut app = views_app();
        app.set_query("assignee:@me iteration:@current".into());
        app.save_view("My sprint".into());
        session::save(&path, &app.snapshot_session()).unwrap();

        let mut restored = views_app();
        restored.restore_session(session::load(&path).unwrap());

        assert_eq!(restored.query(), "assignee:@me iteration:@current");
        assert_eq!(restored.views()[0].name, "My sprint");
        assert_eq!(restored.views()[0].query, "assignee:@me iteration:@current");
        let labels: Vec<String> = restored
            .filter_tokens()
            .iter()
            .map(FilterToken::chip_label)
            .collect();
        assert_eq!(labels, vec!["assignee:@me", "iteration:@current"]);

        let context = restored.agent_context();
        assert_eq!(context.search.query, "assignee:@me iteration:@current");
        assert_eq!(
            context.search.filters,
            vec!["assignee:@me", "iteration:@current"],
            "an agent reads the sentinels as typed and the me field beside them"
        );
        assert_eq!(context.me.as_deref(), Some("Avery Chen"));
        assert_eq!(
            visible_ids(&restored),
            vec![1],
            "and the query still means me, in this sprint"
        );
    }

    #[test]
    fn a_stored_view_never_takes_a_name_a_built_in_owns() {
        let mut app = views_app();

        app.restore_session(Session {
            views: vec![NamedView {
                name: "Mine".into(),
                query: "tag:rust".into(),
                sort_field: SortField::Changed,
                sort_direction: SortDirection::Descending,
                search_order: SearchOrder::Relevance,
                row_density: RowDensity::Compact,
                columns: Vec::new(),
                auto_hide: true,
            }],
            ..Session::default()
        });

        assert!(app.views().is_empty());
        assert_eq!(
            app.view_rows()
                .iter()
                .filter(|row| row.label == "Mine")
                .count(),
            1,
            "a session written before the built-ins existed lists Mine once"
        );
        assert_eq!(app.view_rows()[1].query, "assignee:@me");
    }

    #[test]
    fn date_filters_come_back_from_the_session_file_as_the_chips_they_were_typed_as() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tickets.session.json");
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.set_query("changed:<7d created:>2026-08-01 rust".into());
        session::save(&path, &app.snapshot_session()).unwrap();

        let mut restored = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        restored.restore_session(session::load(&path).unwrap());

        assert_eq!(restored.query(), "changed:<7d created:>2026-08-01 rust");
        let labels: Vec<String> = restored
            .filter_tokens()
            .iter()
            .map(FilterToken::chip_label)
            .collect();
        assert_eq!(labels, vec!["changed:<7d", "created:>2026-08-01"]);
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
        let action = app.copy_with(CopiedContent::Id, export::copy_ids);
        assert_eq!(
            action,
            AppAction::Copy {
                text: "1\n2\n".into(),
                content: CopiedContent::Id,
            }
        );
    }

    #[test]
    fn command_palette_runs_density_toggle() {
        let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
        app.open_palette();
        app.palette.query = TextInput::new("density");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.row_density, RowDensity::Comfortable);
        assert_eq!(app.mode, AppMode::Browse);
    }

    #[test]
    fn every_bound_key_runs_its_command_from_browse_mode() {
        for command in crate::command::COMMANDS
            .iter()
            .filter(|command| !command.keys.is_empty())
        {
            for key in command.keys {
                let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
                app.handle_key(KeyEvent::new(key.code, key.modifiers));
                let expected = match command.id {
                    CommandId::Sort => Some(AppMode::Sort),
                    CommandId::Help => Some(AppMode::Help),
                    CommandId::Views => Some(AppMode::Views),
                    CommandId::Columns => Some(AppMode::Columns),
                    CommandId::Palette => Some(AppMode::Palette),
                    CommandId::DatabaseInfo => Some(AppMode::Info),
                    CommandId::Search => Some(AppMode::Search),
                    CommandId::Filters => Some(AppMode::Facets),
                    CommandId::MoreFilters => Some(AppMode::Filter),
                    CommandId::EditMenu => Some(AppMode::Edit),
                    CommandId::ChangeState => Some(AppMode::StatePicker),
                    _ => None,
                };
                if let Some(mode) = expected {
                    assert_eq!(app.mode, mode, "{:?} via {}", command.id, key.label());
                }
            }
        }
    }

    fn family_key(id: i64) -> TicketKey {
        TicketKey {
            organization: "demo".into(),
            id,
        }
    }

    fn family_app() -> App {
        let mut parent = ticket(1, "Parent", "2026-01-01T00:00:00Z");
        parent.work_item_type = "Feature".into();
        let mut child = ticket(2, "Child", "2026-02-01T00:00:00Z");
        child.work_item_type = "Task".into();
        let grandchild = ticket(3, "Grandchild", "2026-01-15T00:00:00Z");
        let mut app = App::new(vec![parent, child, grandchild]);
        app.set_workspace_graph(TicketGraph {
            relations: vec![
                RelationRecord {
                    from: family_key(2),
                    to: family_key(1),
                    kind: crate::model::RelationKind::Parent,
                },
                RelationRecord {
                    from: family_key(3),
                    to: family_key(2),
                    kind: crate::model::RelationKind::Parent,
                },
            ],
            ..TicketGraph::default()
        });
        app
    }

    fn press(app: &mut App, code: KeyCode) -> AppAction {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn child_of(child: i64, parent: i64) -> RelationRecord {
        RelationRecord {
            from: family_key(child),
            to: family_key(parent),
            kind: RelationKind::Parent,
        }
    }

    /// An Epic over three issues — one closed, one removed, one still open —
    /// with a task hanging off the open issue.
    fn epic_tickets() -> Vec<Ticket> {
        let mut epic = ticket(1, "Auth rewrite", "2026-01-05T00:00:00Z");
        epic.work_item_type = "Epic".into();
        let mut closed = ticket(2, "Login form", "2026-01-04T00:00:00Z");
        closed.state = "Closed".into();
        let mut removed = ticket(3, "Logout", "2026-01-03T00:00:00Z");
        removed.state = "Removed".into();
        let open = ticket(4, "Session notes", "2026-01-02T00:00:00Z");
        let mut task = ticket(5, "Validate email", "2026-01-01T00:00:00Z");
        task.state = "New".into();
        vec![epic, closed, removed, open, task]
    }

    fn epic_graph() -> TicketGraph {
        TicketGraph {
            relations: vec![
                child_of(2, 1),
                child_of(3, 1),
                child_of(4, 1),
                child_of(5, 4),
            ],
            ..TicketGraph::default()
        }
    }

    /// Two epics, two issues under the first of them, and a task under one of
    /// those issues: enough family to move a work item out of one epic and into
    /// another, and enough depth to have a descendant the picker must hide.
    fn reparent_app() -> App {
        let mut epic = ticket(1, "Auth rewrite", "2026-01-05T00:00:00Z");
        epic.work_item_type = "Epic".into();
        let mut other = ticket(2, "Payments", "2026-01-04T00:00:00Z");
        other.work_item_type = "Epic".into();
        let mut issue = ticket(3, "Login form", "2026-01-03T00:00:00Z");
        issue.work_item_type = "Issue".into();
        let mut closed = ticket(4, "Logout", "2026-01-02T00:00:00Z");
        closed.work_item_type = "Issue".into();
        closed.state = "Closed".into();
        let task = ticket(5, "Validate email", "2026-01-01T00:00:00Z");
        let mut app = App::new(vec![epic, other, issue, closed, task]);
        app.set_workspace_graph(TicketGraph {
            relations: vec![
                child_of(3, 1),
                child_of(4, 1),
                child_of(5, 3),
                RelationRecord {
                    from: family_key(1),
                    to: family_key(3),
                    kind: RelationKind::Child,
                },
            ],
            ..TicketGraph::default()
        });
        app.enable_sync();
        app.set_table_viewport(5);
        app.jump_to_ticket(&family_key(3));
        app
    }

    fn candidate_ids(candidates: &[ParentCandidate]) -> Vec<i64> {
        candidates
            .iter()
            .map(|candidate| candidate.key.id)
            .collect()
    }

    fn menu_labels(app: &App) -> Vec<&'static str> {
        app.edit_menu_entries()
            .into_iter()
            .map(|entry| entry.label)
            .collect()
    }

    fn progress_of(app: &App, id: i64) -> Option<(usize, usize)> {
        app.child_progress(&family_key(id))
            .map(|progress| (progress.done, progress.total))
    }

    #[test]
    fn the_parent_picker_leaves_out_the_work_item_itself_and_everything_below_it() {
        let mut app = reparent_app();

        app.run_command(CommandId::SetParent);

        assert_eq!(app.mode, AppMode::ParentPicker);
        assert_eq!(
            candidate_ids(&app.parent_picker.candidates),
            [1, 2, 4],
            "#3 cannot be its own parent and #5 is already under it, so neither is offered"
        );
        assert_eq!(
            app.parent_picker.current,
            Some(family_key(1)),
            "the epic it hangs under now opens under the cursor"
        );
        assert_eq!(app.parent_picker.index, 0);
    }

    #[test]
    fn the_parent_picker_filters_on_the_id_as_well_as_the_title() {
        let mut app = reparent_app();
        app.run_command(CommandId::SetParent);

        for ch in "pay".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        assert_eq!(
            candidate_ids(&app.parent_matches()),
            [2],
            "the title matches"
        );

        for _ in 0..3 {
            press(&mut app, KeyCode::Backspace);
        }
        press(&mut app, KeyCode::Char('4'));
        assert_eq!(
            candidate_ids(&app.parent_matches()),
            [4],
            "and so does the id"
        );
    }

    #[test]
    fn remove_parent_is_offered_only_when_the_work_item_has_one_to_remove() {
        let mut app = reparent_app();

        assert!(
            menu_labels(&app).contains(&"Remove parent"),
            "#3 hangs under an epic, so it can be detached: {:?}",
            menu_labels(&app)
        );
        assert_eq!(
            app.edit_menu_entries()[7].command,
            CommandId::SetParent,
            "the removal follows the row that sets a parent"
        );
        assert_eq!(app.edit_menu_entries()[8].command, CommandId::RemoveParent);

        app.jump_to_ticket(&family_key(2));
        assert!(
            !menu_labels(&app).contains(&"Remove parent"),
            "#2 hangs under nothing, so there is nothing to take off: {:?}",
            menu_labels(&app)
        );
        assert_eq!(
            app.run_command(CommandId::RemoveParent),
            AppAction::None,
            "and asking for it anyway writes nothing"
        );
    }

    #[test]
    fn choosing_a_new_parent_moves_the_work_item_in_the_graph_and_in_both_ratios() {
        let mut app = reparent_app();
        assert_eq!(progress_of(&app, 1), Some((1, 2)));
        assert_eq!(progress_of(&app, 2), None);
        app.run_command(CommandId::SetParent);

        let index = app
            .parent_matches()
            .iter()
            .position(|candidate| candidate.key == family_key(2))
            .expect("the other epic is on offer");
        let action = app.choose_parent(index);

        assert_eq!(
            action,
            AppAction::Reparent {
                key: family_key(3),
                new_parent: Some(2),
            }
        );
        assert_eq!(app.mode, AppMode::Browse);
        assert_eq!(
            app.parent_of(&family_key(3)),
            Some(family_key(2)),
            "the work item names its new epic at once"
        );
        assert_eq!(
            app.family_of(&family_key(1)).children,
            vec![family_key(4)],
            "and the epic it left no longer names it, which is the other half of the link"
        );
        assert_eq!(app.family_of(&family_key(2)).children, vec![family_key(3)]);
        assert_eq!(
            progress_of(&app, 1),
            Some((1, 1)),
            "the epic it left has one issue fewer to finish"
        );
        assert_eq!(
            progress_of(&app, 2),
            Some((0, 1)),
            "and the epic it joined has one more"
        );
        assert_eq!(
            app.visible_family_tree().first().map(|entry| entry.key.id),
            Some(2),
            "the family tree redraws from the graph, so the new epic is the root"
        );
    }

    #[test]
    fn removing_a_parent_detaches_the_work_item_in_both_directions() {
        let mut app = reparent_app();

        let action = app.run_command(CommandId::RemoveParent);

        assert_eq!(
            action,
            AppAction::Reparent {
                key: family_key(3),
                new_parent: None,
            }
        );
        assert_eq!(app.parent_of(&family_key(3)), None);
        assert_eq!(
            app.family_of(&family_key(1)).children,
            vec![family_key(4)],
            "the epic keeps the issue it still has and loses the one that left"
        );
        assert_eq!(progress_of(&app, 1), Some((1, 1)));
        assert_eq!(
            app.family_of(&family_key(3)).children,
            vec![family_key(5)],
            "what hangs under the detached work item comes with it"
        );
    }

    #[test]
    fn a_refused_move_puts_both_halves_of_the_link_and_both_ratios_back() {
        let mut app = reparent_app();
        app.run_command(CommandId::SetParent);
        let index = app
            .parent_matches()
            .iter()
            .position(|candidate| candidate.key == family_key(2))
            .expect("the other epic is on offer");
        app.choose_parent(index);
        assert_eq!(app.parent_of(&family_key(3)), Some(family_key(2)));

        app.reject_reparent(&ReparentRejection {
            key: family_key(3),
            conflict: true,
            message: "it changed in Azure DevOps".into(),
        });

        assert_eq!(
            app.parent_of(&family_key(3)),
            Some(family_key(1)),
            "the work item is back under the epic it was under"
        );
        assert_eq!(
            app.family_of(&family_key(1)).children,
            vec![family_key(3), family_key(4)],
            "and that epic names it again"
        );
        assert_eq!(
            app.family_of(&family_key(2)).children,
            Vec::new(),
            "the epic it never joined is empty again"
        );
        assert_eq!(progress_of(&app, 1), Some((1, 2)));
        assert_eq!(progress_of(&app, 2), None);
        let (message, level) = app.notification().expect("a refused move is never silent");
        assert_eq!(level, NotificationLevel::Error);
        assert!(
            message.contains("#3 not moved") && message.contains("syncing"),
            "{message}"
        );
    }

    #[test]
    fn a_cycle_the_stale_graph_could_not_see_is_refused_and_put_back_in_its_own_words() {
        // The picker cannot offer a descendant, so a cycle only ever comes from
        // a graph the project has already moved on from: #2 became a child of
        // #3 in Azure DevOps, and nothing here has read that yet.
        let mut app = reparent_app();
        app.run_command(CommandId::SetParent);
        let index = app
            .parent_matches()
            .iter()
            .position(|candidate| candidate.key == family_key(2))
            .expect("the other epic still looks like a candidate");
        app.choose_parent(index);

        app.reject_reparent(&ReparentRejection {
            key: family_key(3),
            conflict: false,
            message: "TF201036: adding this link would create a circular relationship".into(),
        });

        assert_eq!(
            app.parent_of(&family_key(3)),
            Some(family_key(1)),
            "the move is undone whole, not left half applied"
        );
        assert_eq!(
            app.family_of(&family_key(1)).children,
            vec![family_key(3), family_key(4)]
        );
        assert_eq!(app.family_of(&family_key(2)).children, Vec::new());
        assert_eq!(progress_of(&app, 1), Some((1, 2)));
        assert!(!app.reparents_pending());
        let (message, _) = app.notification().expect("a refused move is never silent");
        assert!(
            message.contains("circular relationship") && !message.contains("syncing"),
            "Azure DevOps's own reason is reported, and a rule refusal is not a conflict: {message}"
        );
    }

    #[test]
    fn an_accepted_move_settles_on_the_links_azure_devops_sent_back() {
        let mut app = reparent_app();
        app.run_command(CommandId::RemoveParent);
        let mut stored = app.ticket_by_key(&family_key(3)).unwrap().clone();
        stored.revision += 1;

        // The server filed it under the other epic after all, which is what the
        // graph has to settle on rather than the detachment that was asked for.
        app.apply_reparent(ReparentApplied {
            ticket: stored,
            relations: vec![child_of(3, 2)],
            parent: Some(family_key(2)),
        });

        assert!(!app.reparents_pending());
        assert_eq!(app.parent_of(&family_key(3)), Some(family_key(2)));
        assert_eq!(app.family_of(&family_key(2)).children, vec![family_key(3)]);
        assert_eq!(app.family_of(&family_key(1)).children, vec![family_key(4)]);
        assert_eq!(progress_of(&app, 2), Some((0, 1)));
        assert_eq!(
            app.ticket_by_key(&family_key(3)).unwrap().revision,
            2,
            "the row takes the revision the server settled on"
        );
    }

    #[test]
    fn a_second_move_is_refused_while_the_first_is_still_in_flight() {
        let mut app = reparent_app();
        assert!(matches!(
            app.run_command(CommandId::RemoveParent),
            AppAction::Reparent { .. }
        ));
        assert!(app.reparents_pending());

        app.run_command(CommandId::SetParent);
        let index = app
            .parent_matches()
            .iter()
            .position(|candidate| candidate.key == family_key(2))
            .expect("the other epic is on offer");

        assert_eq!(
            app.choose_parent(index),
            AppAction::None,
            "the second move would be tested against a revision that is already stale"
        );
        assert_eq!(
            app.parent_of(&family_key(3)),
            None,
            "and the graph still shows only the move that is in flight"
        );
    }

    #[test]
    fn child_progress_counts_direct_children_and_closes_on_completed_or_removed() {
        let mut app = App::new(epic_tickets());
        app.set_workspace_graph(epic_graph());

        let epic = app
            .child_progress(&family_key(1))
            .expect("an epic with children has a ratio");
        assert_eq!(
            (epic.done, epic.total),
            (2, 3),
            "Closed and Removed both count as done, and the grandchild is not the epic's own child"
        );
        assert!(!epic.is_complete());
        let issue = app
            .child_progress(&family_key(4))
            .expect("the issue has a task under it");
        assert_eq!((issue.done, issue.total), (0, 1));
        assert_eq!(
            app.child_progress(&family_key(5)),
            None,
            "a work item nobody broke down has no ratio at all, not 0/0"
        );
    }

    #[test]
    fn an_epic_reads_as_complete_once_its_last_child_closes() {
        let mut app = App::new(epic_tickets());
        app.set_workspace_graph(epic_graph());
        assert!(!app.child_progress(&family_key(1)).unwrap().is_complete());

        let mut closing = epic_tickets();
        closing[3].state = "Closed".into();
        app.replace_prepared_tickets(PreparedTickets::with_graph(closing, epic_graph()));

        let epic = app.child_progress(&family_key(1)).unwrap();
        assert_eq!((epic.done, epic.total), (3, 3));
        assert!(
            epic.is_complete(),
            "the last issue closing finishes the epic without anything recounting it by hand"
        );
    }

    #[test]
    fn sorting_by_progress_runs_least_finished_first_and_leaves_childless_rows_last() {
        let mut app = App::new(epic_tickets());
        app.set_workspace_graph(epic_graph());

        app.set_sort(SortField::Progress, SortDirection::Ascending);

        let order: Vec<i64> = app.visible_tickets().map(|ticket| ticket.key.id).collect();
        assert_eq!(
            order,
            vec![4, 1, 2, 3, 5],
            "0/1 before 2/3, then the work items with no children in id order"
        );
    }

    #[test]
    fn a_bar_fills_only_on_a_whole_ratio_and_never_reads_empty_once_work_has_landed() {
        let bar = |done, total| ChildProgress { done, total }.filled_cells(PROGRESS_BAR_CELLS);

        assert_eq!(bar(0, 7), 0);
        assert_eq!(bar(1, 40), 1, "a single closed child still shows one cell");
        assert_eq!(bar(3, 7), 2);
        assert_eq!(bar(39, 40), 5, "an unfinished parent never fills the bar");
        assert_eq!(bar(7, 7), PROGRESS_BAR_CELLS);
    }

    #[test]
    fn pane_keys_move_focus_and_only_the_details_pane_opens_on_enter() {
        let mut app = family_app();
        assert_eq!(app.focus, Focus::Tickets);

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Details);
        assert!(app.narrow_details, "the narrow layout follows the focus");

        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.focus, Focus::Tickets);
        assert!(!app.narrow_details);

        app.focus = Focus::Family;
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Details);

        app.focus = Focus::Tickets;
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            AppAction::None,
            "Enter must not open a browser from the tickets pane"
        );
        assert!(matches!(
            press(&mut app, KeyCode::Char('o')),
            AppAction::OpenUrl(_)
        ));
        app.focus = Focus::Details;
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            AppAction::OpenUrl(_)
        ));
    }

    #[test]
    fn family_cursor_movement_clamps_and_scrolls_the_details_viewport() {
        let mut app = family_app();
        app.focus = Focus::Family;
        app.details.set_viewport(2, 20);

        press(&mut app, KeyCode::Home);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.family_cursor.as_ref().map(|key| key.id), Some(1));
        assert_eq!(app.details.offset, 0);

        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.family_cursor.as_ref().map(|key| key.id), Some(3));
        assert!(
            app.details.offset > 0,
            "the details pane scrolls to keep the cursor visible"
        );
    }

    #[test]
    fn family_enter_selects_visible_tickets_records_history_once_and_explains_hidden_ones() {
        let mut app = family_app();
        assert_eq!(app.selected_ticket().unwrap().key.id, 2);
        app.focus = Focus::Family;

        let opened = press(&mut app, KeyCode::Char('o'));
        assert!(matches!(opened, AppAction::OpenUrl(_)));
        assert_eq!(app.selected_ticket().unwrap().key.id, 2);

        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.selected_ticket().unwrap().key.id, 3);
        assert_eq!(app.focus, Focus::Family);
        assert_eq!(
            app.recent.iter().map(|key| key.id).collect::<Vec<_>>(),
            vec![2, 3]
        );

        press(&mut app, KeyCode::Char('['));
        assert_eq!(app.selected_ticket().unwrap().key.id, 2);

        app.visible
            .retain(|entry| app.tickets[entry.ticket_index].key.id != 3);
        app.family_cursor = Some(family_key(3));
        let query = app.query().to_owned();
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.selected_ticket().unwrap().key.id, 2);
        assert_eq!(app.query(), query, "a hidden target changes no search");
        assert_eq!(
            app.notification(),
            Some(("3 is hidden by the current search", NotificationLevel::Info))
        );
    }

    #[test]
    fn a_background_sync_leaves_the_search_box_and_the_selection_alone() {
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
        ]);
        press(&mut app, KeyCode::Char('/'));
        for character in "alp".chars() {
            press(&mut app, KeyCode::Char(character));
        }
        await_search(&mut app);
        let selected = app.selected_ticket().unwrap().key.clone();

        // The sync worker's rows land while the user is still typing.
        let mut refreshed = app.tickets().to_vec();
        refreshed.push(ticket(3, "Gamma", "2026-03-01T00:00:00Z"));
        app.replace_prepared_tickets(PreparedTickets::new(refreshed));
        await_search(&mut app);

        assert_eq!(app.mode, AppMode::Search);
        assert_eq!(app.query(), "alp");
        assert_eq!(app.tickets().len(), 3);
        assert_eq!(app.selected_ticket().unwrap().key, selected);

        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.query(), "alph", "the caret stayed where it was");
    }

    #[test]
    fn family_selection_and_cursor_restore_after_reload() {
        let mut app = family_app();
        app.focus = Focus::Family;
        press(&mut app, KeyCode::Down);
        assert_eq!(app.family_cursor.as_ref().map(|key| key.id), Some(3));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.selected_ticket().unwrap().key.id, 3);

        let graph = app.graph.clone();
        let tickets = app.tickets().to_vec();
        app.replace_prepared_tickets(PreparedTickets::with_graph(tickets, graph));

        assert_eq!(app.selected_ticket().unwrap().key.id, 3);
        assert_eq!(app.family_cursor.as_ref().map(|key| key.id), Some(3));
        assert_eq!(
            app.visible_family_tree()
                .iter()
                .map(|entry| entry.key.id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn clicking_the_pane_divider_neither_acts_nor_selects_text() {
        let mut app = App::new(vec![ticket(1, "One", "2026-01-02T00:00:00Z")]);
        let rect = Rect {
            x: 60,
            y: 5,
            width: 1,
            height: 10,
        };
        app.set_content_layout(
            Rect {
                x: 0,
                y: 4,
                width: 130,
                height: 20,
            },
            Some(DividerOrientation::Vertical),
        );
        // A selectable pane sits under the divider; pressing the divider must
        // still not start a selection in it.
        app.hit_regions.push(pointer::region(
            Rect {
                x: 0,
                y: 4,
                width: 130,
                height: 20,
            },
            PointerTarget::FocusDetails,
            pointer::PointerLayer::Base,
            Some(SelectableSurface::Details),
            None,
        ));
        app.hit_regions.push(pointer::region(
            rect,
            PointerTarget::PaneDivider,
            pointer::PointerLayer::Base,
            None,
            None,
        ));
        app.session_dirty = false;

        let point = |kind| MouseEvent {
            kind,
            column: rect.x,
            row: rect.y + 3,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(point(MouseEventKind::Down(MouseButton::Left)));
        let update = app.handle_mouse(point(MouseEventKind::Up(MouseButton::Left)));

        assert!(matches!(update.action, AppAction::None));
        assert!(app.selection().is_none(), "a divider press selects no text");
        assert_eq!(app.pane_split_wide, DEFAULT_PANE_SPLIT_WIDE);
        assert!(!app.session_dirty, "a press with no drag changes nothing");

        app.pane_split_wide = 71;
        app.pane_split_stacked = 45;
        let session = app.snapshot_session();
        let mut restored = App::new(vec![ticket(1, "One", "2026-01-02T00:00:00Z")]);
        restored.restore_session(session);
        assert_eq!(restored.pane_split_wide, 71, "the split is remembered");
        assert_eq!(restored.pane_split_stacked, 45);

        restored.session_dirty = false;
        restored.run_command(CommandId::ResetPaneSplit);
        assert_eq!(restored.pane_split_wide, DEFAULT_PANE_SPLIT_WIDE);
        assert_eq!(restored.pane_split_stacked, DEFAULT_PANE_SPLIT_STACKED);
        assert!(restored.session_dirty);
    }

    /// Three work items over a configured Azure DevOps project, which is what
    /// an edit needs to go anywhere.
    fn editing_app() -> App {
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            ticket(3, "Gamma", "2026-03-01T00:00:00Z"),
        ]);
        app.enable_sync();
        app.set_table_viewport(3);
        app
    }

    fn edit_request(app: &mut App, edit: FieldEdit) -> EditRequest {
        match app.edit_selected(edit) {
            AppAction::Edit(requests) => only(requests),
            other => panic!("expected an edit to be dispatched, got {other:?}"),
        }
    }

    /// The one request an edit of a single work item dispatches.
    fn only(requests: Vec<EditRequest>) -> EditRequest {
        assert_eq!(requests.len(), 1, "one work item, one request");
        requests.into_iter().next().expect("the request is there")
    }

    /// Checks every row the app holds, which is what turns the pickers into
    /// bulk changes.
    fn check_all(app: &mut App) {
        for key in app
            .tickets()
            .iter()
            .map(|ticket| ticket.key.clone())
            .collect::<Vec<_>>()
        {
            app.selected_keys.insert(key);
        }
    }

    /// The work item as Azure DevOps hands it back: the field written, and the
    /// revision and changed date it decided on.
    fn stored_copy(app: &App, key: &TicketKey, state: &str) -> Ticket {
        let mut ticket = app.ticket_by_key(key).expect("the row is loaded").clone();
        ticket.state = state.to_owned();
        ticket.revision += 1;
        ticket.changed_at = crate::timestamp::ts("2026-04-01T00:00:00Z");
        ticket
    }

    /// Azure DevOps accepting whatever a request asked for. The optimistic row
    /// already carries the field it wrote, so the stored copy is that row on
    /// the next revision.
    fn accept_edit(app: &mut App, request: &EditRequest) {
        let mut ticket = app
            .ticket_by_key(&request.key)
            .expect("the row is loaded")
            .clone();
        ticket.revision += 1;
        app.apply_edit(EditApplied {
            ticket,
            relations: Vec::new(),
            edit: request.edit.clone(),
        });
    }

    /// One press of `u`, and the requests it dispatched.
    fn undo(app: &mut App) -> Vec<EditRequest> {
        match press(app, KeyCode::Char('u')) {
            AppAction::Edit(requests) => requests,
            other => panic!("an undo should be dispatched like any other edit, got {other:?}"),
        }
    }

    #[test]
    fn an_edit_shows_at_once_and_the_stored_copy_replaces_it() {
        let mut app = editing_app();
        let request = edit_request(&mut app, FieldEdit::state("Doing"));
        let key = request.key.clone();

        assert_eq!(request.expected_revision, 1, "the row's revision is tested");
        assert_eq!(request.edit.summary(), "State → Doing");
        assert!(app.edits_pending());
        assert_eq!(
            app.ticket_by_key(&key).unwrap().state,
            "Doing",
            "the row does not wait for the network"
        );

        app.set_query("Doing".into());
        await_search(&mut app);
        assert_eq!(
            app.visible_count(),
            1,
            "the search index follows the optimistic value"
        );
        app.set_query(String::new());
        await_search(&mut app);

        let stored = stored_copy(&app, &key, "Doing");
        app.apply_edit(EditApplied {
            ticket: stored.clone(),
            relations: Vec::new(),
            edit: FieldEdit::state("Doing"),
        });

        assert!(!app.edits_pending());
        assert_eq!(app.ticket_by_key(&key), Some(&stored), "the server wins");
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Updated #3 · State → Doing")
        );
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.key.id),
            Some(key.id),
            "the selection stays on the work item it was on"
        );
    }

    #[test]
    fn a_refused_edit_puts_the_row_back_and_names_the_field() {
        let mut app = editing_app();
        let request = edit_request(&mut app, FieldEdit::state("Doing"));
        let before = app.tickets().to_vec();

        app.reject_edit(&EditRejection {
            key: request.key.clone(),
            label: "State".into(),
            conflict: true,
            message: "the test operation on /rev failed".into(),
        });

        assert!(!app.edits_pending());
        assert_eq!(
            app.ticket_by_key(&request.key).unwrap().state,
            "Active",
            "a refused write leaves nothing of itself behind"
        );
        assert_ne!(before, app.tickets());
        let (message, level) = app.notification().expect("a refusal is always reported");
        assert!(message.contains("#3 changed in Azure DevOps"), "{message}");
        assert!(message.contains("State not saved"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
    }

    #[test]
    fn a_pull_that_lands_during_an_edit_keeps_the_optimistic_value() {
        let mut app = editing_app();
        let request = edit_request(&mut app, FieldEdit::state("Doing"));
        let key = request.key.clone();

        // A pull that was already in flight when the edit went out: it cannot
        // know about the edit, but it must not undo it on screen either.
        let mut pulled = ticket(3, "Gamma renamed", "2026-03-02T00:00:00Z");
        pulled.revision = 4;
        app.replace_prepared_tickets(PreparedTickets::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            pulled.clone(),
        ]));

        let row = app.ticket_by_key(&key).expect("the row survived the pull");
        assert_eq!(row.state, "Doing", "the edit is still showing");
        assert_eq!(row.title, "Gamma renamed", "everything else is the pull's");
        assert!(app.edits_pending());

        app.reject_edit(&EditRejection {
            key: key.clone(),
            label: "State".into(),
            conflict: false,
            message: "field is read only".into(),
        });
        assert_eq!(
            app.ticket_by_key(&key),
            Some(&pulled),
            "a refusal restores the freshest copy the edit did not make"
        );
    }

    #[test]
    fn an_edit_leaves_the_filtered_view_only_once_it_lands() {
        let mut app = editing_app();
        app.set_query("state:Active".into());
        assert_eq!(app.visible_count(), 3);

        let request = edit_request(&mut app, FieldEdit::state("Done"));
        assert_eq!(
            app.visible_count(),
            3,
            "the row stays where it is while the write is in flight"
        );

        let stored = stored_copy(&app, &request.key, "Done");
        app.apply_edit(EditApplied {
            ticket: stored,
            relations: Vec::new(),
            edit: request.edit.clone(),
        });

        assert_eq!(
            app.visible_count(),
            2,
            "the filter drops the row when the change lands"
        );
        assert_eq!(app.query(), "state:Active", "the query is left alone");
    }

    #[test]
    fn an_offline_app_refuses_an_edit_and_changes_nothing() {
        let mut app = App::new(vec![ticket(1, "Alpha", "2026-01-01T00:00:00Z")]);
        app.set_offline_reason(Some("no Azure DevOps organization; pass --org".into()));

        assert_eq!(
            app.edit_selected(FieldEdit::state("Doing")),
            AppAction::None
        );

        assert_eq!(app.tickets()[0].state, "Active");
        assert!(!app.edits_pending());
        let (message, level) = app.notification().expect("the refusal is reported");
        assert!(message.contains("State not saved"), "{message}");
        assert!(message.contains("--org"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
    }

    #[test]
    fn a_second_edit_of_the_same_row_waits_for_the_first_to_answer() {
        let mut app = editing_app();
        let request = edit_request(&mut app, FieldEdit::state("Doing"));

        assert_eq!(app.edit_selected(FieldEdit::state("Done")), AppAction::None);
        assert_eq!(app.ticket_by_key(&request.key).unwrap().state, "Doing");
        let (message, _) = app.notification().unwrap();
        assert!(
            message.contains("an earlier edit is still in flight"),
            "{message}"
        );

        app.apply_edit(EditApplied {
            ticket: stored_copy(&app, &request.key, "Doing"),
            relations: Vec::new(),
            edit: request.edit,
        });
        assert!(
            matches!(
                app.edit_selected(FieldEdit::state("Done")),
                AppAction::Edit(_)
            ),
            "the next edit goes out once the first has answered"
        );
    }

    /// The states a Basic-process Task moves through, as a sync would have
    /// cached them.
    fn task_states() -> Vec<StateOption> {
        vec![
            StateOption::new("To Do", StateCategory::Proposed),
            StateOption::new("Doing", StateCategory::InProgress),
            StateOption::new("Done", StateCategory::Completed),
        ]
    }

    /// An editable app whose rows are all in the first state, with the states
    /// their type allows already cached.
    fn picker_app() -> App {
        let mut tickets = vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            ticket(3, "Gamma", "2026-03-01T00:00:00Z"),
        ];
        for ticket in &mut tickets {
            ticket.state = "To Do".into();
        }
        let mut app = App::new(tickets);
        app.enable_sync();
        app.set_table_viewport(3);
        let mut catalog = StateCatalog::default();
        catalog.insert("Task", task_states());
        app.set_state_catalog(catalog);
        app
    }

    fn state_names(options: &[StateOption]) -> Vec<&str> {
        options.iter().map(|option| option.name.as_str()).collect()
    }

    fn shift(app: &mut App, ch: char) -> AppAction {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::SHIFT))
    }

    #[test]
    fn the_state_picker_opens_on_the_current_state_and_enter_writes_the_one_chosen() {
        let mut app = picker_app();

        assert_eq!(shift(&mut app, 'S'), AppAction::None);
        assert_eq!(app.mode, AppMode::StatePicker);
        assert_eq!(
            state_names(&app.state_picker.options),
            ["To Do", "Doing", "Done"]
        );
        assert_eq!(app.state_picker.current, "To Do");
        assert_eq!(
            app.state_picker.index, 0,
            "the state the work item is in starts under the cursor"
        );
        assert_eq!(
            app.state_picker.scope,
            EditScope::Ticket(3),
            "the picker names the selected row"
        );

        press(&mut app, KeyCode::Down);
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing another state should dispatch an edit");
        };
        let request = only(requests);

        assert_eq!(app.mode, AppMode::Browse);
        assert_eq!(request.key.id, 3);
        assert_eq!(
            request.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                serde_json::json!({"op": "add", "path": "/fields/System.State", "value": "Doing"}),
            ]
        );
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.state.as_str()),
            Some("Doing"),
            "the row shows the new state without waiting for Azure DevOps"
        );
        assert!(app.edits_pending());
    }

    #[test]
    fn choosing_the_current_state_or_pressing_escape_writes_nothing() {
        let mut app = picker_app();

        shift(&mut app, 'S');
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            AppAction::None,
            "the state it is already in is a no-op"
        );
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None, "a no-op closes silently");

        shift(&mut app, 'S');
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Esc), AppAction::None);
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None);
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.state.as_str()),
            Some("To Do"),
            "cancelling leaves the row exactly as it was"
        );
    }

    /// The states every row is showing, in the order the table holds them.
    fn states_of(app: &App) -> Vec<&str> {
        app.tickets()
            .iter()
            .map(|ticket| ticket.state.as_str())
            .collect()
    }

    /// The work item as Azure DevOps hands it back after a bulk change: the
    /// state written, on the revision it settled on.
    fn accept(app: &mut App, request: &EditRequest) {
        let ticket = stored_copy(app, &request.key, request.edit.value_text().as_str());
        app.apply_edit(EditApplied {
            ticket,
            relations: Vec::new(),
            edit: request.edit.clone(),
        });
    }

    /// Checks all three rows and moves them to `Doing`, which is the bulk
    /// change the other tests here take apart.
    fn bulk_state_change(app: &mut App) -> Vec<EditRequest> {
        check_all(app);
        shift(app, 'S');
        press(app, KeyCode::Down);
        match press(app, KeyCode::Enter) {
            AppAction::Edit(requests) => requests,
            other => panic!("a checked picker should dispatch a bulk edit, got {other:?}"),
        }
    }

    #[test]
    fn a_picker_over_checked_rows_dispatches_one_request_for_each_of_them() {
        let mut app = picker_app();
        check_all(&mut app);

        shift(&mut app, 'S');
        assert_eq!(
            app.state_picker.scope,
            EditScope::Checked(3),
            "the picker says how many work items it is about to move"
        );
        assert_eq!(
            app.state_picker.scope.label(),
            "3 tickets",
            "which is what its title reads"
        );

        press(&mut app, KeyCode::Down);
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing a state should dispatch an edit for every checked row");
        };

        assert_eq!(
            requests
                .iter()
                .map(|request| request.key.id)
                .collect::<Vec<_>>(),
            [1, 2, 3],
            "one request a work item, in the order the table holds them"
        );
        for request in &requests {
            assert_eq!(
                request.document(),
                vec![
                    serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                    serde_json::json!({
                        "op": "add",
                        "path": "/fields/System.State",
                        "value": "Doing",
                    }),
                ],
                "each carries its own revision test",
            );
        }
        assert_eq!(
            states_of(&app),
            ["Doing", "Doing", "Doing"],
            "every row shows the new state without waiting for Azure DevOps"
        );
        assert!(app.edits_pending());
        assert_eq!(
            app.notification(),
            None,
            "nothing is said until they answer"
        );
    }

    #[test]
    fn a_bulk_change_reports_itself_once_the_last_work_item_has_answered() {
        let mut app = picker_app();
        let requests = bulk_state_change(&mut app);

        accept(&mut app, &requests[0]);
        assert_eq!(
            app.notification(),
            None,
            "a bulk change speaks once, not once a row"
        );
        accept(&mut app, &requests[1]);
        assert_eq!(app.notification(), None);

        accept(&mut app, &requests[2]);
        let (message, level) = app.notification().expect("the tally goes up at the end");
        assert_eq!(message, "Updated 3 tickets \u{b7} State \u{2192} Doing");
        assert_eq!(level, NotificationLevel::Info);
        assert!(!app.edits_pending());
        assert_eq!(
            states_of(&app),
            ["Doing", "Doing", "Doing"],
            "the copies Azure DevOps stored replace the optimistic rows"
        );
        assert_eq!(
            app.tickets()
                .iter()
                .filter(|ticket| app.is_row_selected(&ticket.key))
                .count(),
            3,
            "the checked set survives the change, ready for the next one"
        );
    }

    #[test]
    fn one_refusal_in_a_bulk_change_reverts_only_its_own_row_and_is_named() {
        let mut app = picker_app();
        let requests = bulk_state_change(&mut app);

        accept(&mut app, &requests[0]);
        accept(&mut app, &requests[1]);
        app.reject_edit(&EditRejection {
            key: requests[2].key.clone(),
            label: "State".into(),
            conflict: false,
            message: "the transition is not allowed".into(),
        });

        let (message, level) = app.notification().expect("a refusal is never dropped");
        assert_eq!(
            message,
            "Updated 2 of 3 \u{b7} #3 failed: the transition is not allowed"
        );
        assert_eq!(level, NotificationLevel::Error);
        assert_eq!(
            states_of(&app),
            ["Doing", "Doing", "To Do"],
            "only the work item that was refused goes back"
        );
        assert!(!app.edits_pending());
        assert!(
            app.is_row_selected(&requests[2].key),
            "a refused row stays checked, so it can be tried again"
        );
    }

    #[test]
    fn a_bulk_change_passes_over_the_work_items_already_carrying_the_value() {
        let mut app = picker_app();
        let key = app.tickets()[1].key.clone();
        let AppAction::Edit(first) = app.edit_ticket(&key, FieldEdit::state("Doing")) else {
            panic!("one work item moves on its own");
        };
        let first = only(first);
        accept(&mut app, &first);

        check_all(&mut app);
        shift(&mut app, 'S');
        press(&mut app, KeyCode::Down);
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("the rows that are not there yet should still be moved");
        };
        assert_eq!(
            requests
                .iter()
                .map(|request| request.key.id)
                .collect::<Vec<_>>(),
            [1, 3],
            "the work item already in the state is left alone rather than rewritten"
        );

        accept(&mut app, &requests[0]);
        accept(&mut app, &requests[1]);
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Updated 2 tickets \u{b7} State \u{2192} Doing")
        );
    }

    #[test]
    fn a_bulk_change_with_nothing_left_to_do_says_so_and_writes_nothing() {
        let mut app = picker_app();
        check_all(&mut app);

        shift(&mut app, 'S');
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            AppAction::None,
            "the state they are all already in is a no-op"
        );
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Nothing to change \u{b7} State \u{2192} To Do")
        );
        assert!(!app.edits_pending());
        assert_eq!(states_of(&app), ["To Do", "To Do", "To Do"]);
    }

    #[test]
    fn the_editors_that_are_not_worth_making_in_bulk_stay_on_the_row_under_the_cursor() {
        let mut app = picker_app();
        check_all(&mut app);

        app.run_command(CommandId::EditTitle);
        type_query(&mut app, "!");
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("the title prompt should still write one work item");
        };
        assert_eq!(
            only(requests).key.id,
            3,
            "the same title on three work items is never what was meant"
        );
        assert_eq!(
            app.tickets()
                .iter()
                .filter(|ticket| ticket.title.ends_with('!'))
                .count(),
            1,
            "and only that row is renamed"
        );
    }

    #[test]
    fn an_undo_puts_the_value_back_and_writes_it_to_azure_devops_to_do_it() {
        let mut app = editing_app();
        let request = edit_request(&mut app, FieldEdit::state("Doing"));
        let key = request.key.clone();
        accept_edit(&mut app, &request);

        let undone = only(undo(&mut app));
        assert_eq!(undone.key, key, "the work item the edit was made on");
        assert_eq!(
            undone.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 2}),
                serde_json::json!({
                    "op": "add",
                    "path": "/fields/System.State",
                    "value": "Active",
                }),
            ],
            "an undo is an ordinary edit, guarded by the revision the write settled on"
        );
        assert_eq!(
            app.ticket_by_key(&key).unwrap().state,
            "Active",
            "the row goes back without waiting for the network"
        );

        accept_edit(&mut app, &undone);
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Undid State on #3 (Doing \u{2192} Active)")
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('u')),
            AppAction::None,
            "an undo is not itself undoable, or u would only ever toggle"
        );
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Nothing to undo")
        );
    }

    #[test]
    fn undoing_an_edit_of_a_field_that_was_empty_clears_it_rather_than_emptying_it() {
        let mut unset = ticket(1, "Alpha", "2026-01-01T00:00:00Z");
        unset.priority = None;
        let mut app = App::new(vec![unset]);
        app.enable_sync();
        app.set_table_viewport(1);

        let request = edit_request(&mut app, FieldEdit::priority(1));
        accept_edit(&mut app, &request);
        assert_eq!(app.tickets()[0].priority, Some(1));

        let undone = only(undo(&mut app));
        assert_eq!(
            undone.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 2}),
                serde_json::json!({
                    "op": "remove",
                    "path": "/fields/Microsoft.VSTS.Common.Priority",
                }),
            ],
            "a field that was unset goes back to unset, not to an empty value"
        );

        accept_edit(&mut app, &undone);
        assert_eq!(app.tickets()[0].priority, None);
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Undid Priority on #1 (1 \u{2192} (none))")
        );
    }

    #[test]
    fn pressing_undo_with_nothing_to_take_back_says_so() {
        let mut app = editing_app();

        assert_eq!(press(&mut app, KeyCode::Char('u')), AppAction::None);
        let (message, level) = app.notification().expect("a key that did nothing says why");
        assert_eq!(message, "Nothing to undo");
        assert_eq!(level, NotificationLevel::Info);
        assert!(!app.edits_pending(), "and nothing went out");
    }

    #[test]
    fn a_refused_edit_never_reaches_the_undo_stack() {
        let mut app = editing_app();
        let request = edit_request(&mut app, FieldEdit::state("Doing"));

        app.reject_edit(&EditRejection {
            key: request.key.clone(),
            label: "State".into(),
            conflict: true,
            message: "the test operation on /rev failed".into(),
        });

        assert_eq!(
            press(&mut app, KeyCode::Char('u')),
            AppAction::None,
            "an edit that left nothing behind has nothing to take back"
        );
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Nothing to undo")
        );
    }

    #[test]
    fn a_refused_undo_is_reported_like_any_other_conflict() {
        let mut app = editing_app();
        let request = edit_request(&mut app, FieldEdit::state("Doing"));
        let key = request.key.clone();
        accept_edit(&mut app, &request);

        let undone = only(undo(&mut app));
        app.reject_edit(&EditRejection {
            key: undone.key.clone(),
            label: "State".into(),
            conflict: true,
            message: "the test operation on /rev failed".into(),
        });

        let (message, level) = app.notification().expect("a refused undo is never dropped");
        assert!(message.contains("#3 changed in Azure DevOps"), "{message}");
        assert!(message.contains("State not saved"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
        assert_eq!(
            app.ticket_by_key(&key).unwrap().state,
            "Doing",
            "the row stays where the edit left it"
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('u')),
            AppAction::None,
            "and the value is not offered again on a copy that has moved on"
        );
    }

    #[test]
    fn the_undo_stack_remembers_twenty_edits_and_forgets_the_ones_before_them() {
        let mut app = editing_app();
        let key = app
            .selected_ticket()
            .expect("a row is selected")
            .key
            .clone();

        for round in 1..=UNDO_DEPTH + 1 {
            let title = FieldEdit::title(&format!("Alpha {round}"));
            let AppAction::Edit(requests) = app.edit_ticket(&key, title) else {
                panic!("a rename should be dispatched");
            };
            let request = only(requests);
            accept_edit(&mut app, &request);
        }

        for _ in 0..UNDO_DEPTH {
            let request = only(undo(&mut app));
            accept_edit(&mut app, &request);
        }

        assert_eq!(
            app.ticket_by_key(&key).unwrap().title,
            "Alpha 1",
            "twenty edits back is as far as it goes; the title before them is forgotten"
        );
        assert_eq!(press(&mut app, KeyCode::Char('u')), AppAction::None);
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Nothing to undo")
        );
    }

    #[test]
    fn one_press_takes_a_whole_bulk_change_back() {
        let mut app = picker_app();
        let requests = bulk_state_change(&mut app);
        for request in &requests {
            accept(&mut app, request);
        }
        assert_eq!(states_of(&app), ["Doing", "Doing", "Doing"]);

        let undone = undo(&mut app);
        assert_eq!(
            undone
                .iter()
                .map(|request| request.key.id)
                .collect::<Vec<_>>(),
            [1, 2, 3],
            "every work item the change touched, under one press"
        );
        assert_eq!(
            states_of(&app),
            ["To Do", "To Do", "To Do"],
            "and every row goes back at once"
        );

        for request in &undone {
            assert_eq!(
                request.expected_revision, 2,
                "each carries the revision its own write settled on"
            );
            accept(&mut app, request);
        }
        let (message, level) = app.notification().expect("the tally goes up at the end");
        assert_eq!(message, "Undid State on 3 tickets");
        assert_eq!(level, NotificationLevel::Info);
        assert_eq!(
            press(&mut app, KeyCode::Char('u')),
            AppAction::None,
            "the whole change went back as one, so there is nothing left of it"
        );
    }

    #[test]
    fn a_bulk_undo_that_only_partly_lands_names_the_rows_left_where_they_were() {
        let mut app = picker_app();
        let requests = bulk_state_change(&mut app);
        for request in &requests {
            accept(&mut app, request);
        }

        let undone = undo(&mut app);
        accept(&mut app, &undone[0]);
        accept(&mut app, &undone[1]);
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Updated 3 tickets \u{b7} State \u{2192} Doing"),
            "the change's own summary still stands: an undo speaks once, not once a row"
        );

        app.reject_edit(&EditRejection {
            key: undone[2].key.clone(),
            label: "State".into(),
            conflict: true,
            message: "the test operation on /rev failed".into(),
        });

        let (message, level) = app
            .notification()
            .expect("a half-done undo is never silent");
        assert_eq!(
            message,
            "Undid 2 of 3 \u{b7} #3 failed: it changed in Azure DevOps"
        );
        assert_eq!(level, NotificationLevel::Error);
        assert_eq!(
            states_of(&app),
            ["To Do", "To Do", "Doing"],
            "only the work item that was refused is left where the change put it"
        );
    }

    #[test]
    fn the_edit_menu_lists_the_field_editors_and_opens_the_one_chosen() {
        let mut app = picker_app();

        assert_eq!(press(&mut app, KeyCode::Char('e')), AppAction::None);
        assert_eq!(app.mode, AppMode::Edit);
        assert_eq!(
            EDIT_MENU
                .iter()
                .map(|entry| entry.label)
                .collect::<Vec<_>>(),
            [
                "State",
                "Title",
                "Priority",
                "Tags",
                "Assignee",
                "Iteration",
                "Area",
                "Set parent\u{2026}",
                "Description",
                "Add comment"
            ],
            "later field editors append their own row"
        );
        assert_eq!(app.edit_menu.index, 0);

        assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
        assert_eq!(app.mode, AppMode::StatePicker);
        assert_eq!(
            state_names(&app.state_picker.options),
            ["To Do", "Doing", "Done"]
        );

        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.mode, AppMode::Edit);
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.mode, AppMode::Browse, "e closes the menu it opened");
    }

    /// An editable app whose selected row — the most recently changed one — has
    /// a priority and a tag to open the field editors on.
    fn edit_app() -> App {
        let mut gamma = ticket(3, "Gamma", "2026-03-01T00:00:00Z");
        gamma.priority = Some(1);
        gamma.tags = vec!["rust".into()];
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            gamma,
        ]);
        app.enable_sync();
        app.set_table_viewport(3);
        app
    }

    /// Opens the Edit menu and runs the row at `index`, the way a hand does.
    fn open_editor(app: &mut App, index: usize) {
        press(app, KeyCode::Char('e'));
        for _ in 0..index {
            press(app, KeyCode::Down);
        }
        press(app, KeyCode::Enter);
    }

    fn prompt_text(app: &App) -> String {
        app.prompt
            .as_ref()
            .expect("a prompt should be open")
            .input
            .text()
            .to_owned()
    }

    /// Clears the prompt and types `text` into it, one key at a time.
    fn type_over(app: &mut App, text: &str) {
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        for character in text.chars() {
            press(app, KeyCode::Char(character));
        }
    }

    #[test]
    fn the_title_prompt_opens_on_the_current_title_and_saves_a_trimmed_one() {
        let mut app = edit_app();

        open_editor(&mut app, 1);
        assert_eq!(app.mode, AppMode::Prompt);
        assert_eq!(prompt_text(&app), "Gamma", "the prompt opens prefilled");

        type_over(&mut app, "  Renamed gamma  ");
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("a new title should dispatch an edit");
        };
        let request = only(requests);

        assert_eq!(app.mode, AppMode::Browse);
        assert!(app.prompt.is_none());
        assert_eq!(request.key.id, 3);
        assert_eq!(
            request.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                serde_json::json!({
                    "op": "add",
                    "path": "/fields/System.Title",
                    "value": "Renamed gamma",
                }),
            ],
            "the title is trimmed before it is sent"
        );
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.title.as_str()),
            Some("Renamed gamma"),
            "the row shows the new title without waiting for Azure DevOps"
        );
    }

    #[test]
    fn an_empty_title_is_refused_locally_and_an_unchanged_one_writes_nothing() {
        let mut app = edit_app();

        open_editor(&mut app, 1);
        type_over(&mut app, "   ");
        assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
        assert_eq!(
            app.mode,
            AppMode::Prompt,
            "a blank title leaves the prompt open to fix"
        );
        assert!(!app.edits_pending(), "nothing was sent");
        let (message, level) = app.notification().expect("a refusal is reported");
        assert!(message.contains("title cannot be empty"), "{message}");
        assert_eq!(level, NotificationLevel::Error);

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, AppMode::Browse);
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.title.as_str()),
            Some("Gamma"),
            "cancelling leaves the row exactly as it was"
        );

        let mut app = edit_app();
        open_editor(&mut app, 1);
        assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
        assert_eq!(
            app.notification(),
            None,
            "an unchanged title closes silently"
        );
    }

    #[test]
    fn the_priority_picker_opens_on_the_current_value_and_writes_the_one_chosen() {
        let mut app = edit_app();

        open_editor(&mut app, 2);
        assert_eq!(app.mode, AppMode::PriorityPicker);
        assert_eq!(app.priority_picker.current, Some(1));
        assert_eq!(
            app.priority_picker.index, 0,
            "the priority the work item has starts under the cursor"
        );
        assert_eq!(app.priority_picker.id, 3);

        assert_eq!(
            press(&mut app, KeyCode::Enter),
            AppAction::None,
            "the priority it already has is a no-op"
        );
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None);

        open_editor(&mut app, 2);
        press(&mut app, KeyCode::Down);
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("another priority should dispatch an edit");
        };
        let request = only(requests);
        assert_eq!(
            request.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                serde_json::json!({
                    "op": "add",
                    "path": "/fields/Microsoft.VSTS.Common.Priority",
                    "value": 2,
                }),
            ]
        );
        assert_eq!(
            app.selected_ticket().and_then(|ticket| ticket.priority),
            Some(2),
            "the Pri cell shows the new priority at once"
        );
    }

    #[test]
    fn clearing_the_priority_removes_the_field_and_empties_the_cell() {
        let mut app = edit_app();

        open_editor(&mut app, 2);
        press(&mut app, KeyCode::End);
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("Clear should dispatch an edit");
        };
        let request = only(requests);
        assert_eq!(
            request.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                serde_json::json!({
                    "op": "remove",
                    "path": "/fields/Microsoft.VSTS.Common.Priority",
                }),
            ],
            "a priority goes back to unset by being removed"
        );
        assert_eq!(
            app.selected_ticket().and_then(|ticket| ticket.priority),
            None
        );
    }

    #[test]
    fn the_tags_prompt_trims_deduplicates_and_rejoins_what_it_saves() {
        let mut app = edit_app();

        open_editor(&mut app, 3);
        assert_eq!(app.mode, AppMode::Prompt);
        assert_eq!(
            prompt_text(&app),
            "rust",
            "the prompt opens on the tags held"
        );

        type_over(&mut app, "rust; Rust ;; tui");
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("a new tag list should dispatch an edit");
        };
        let request = only(requests);
        assert_eq!(
            request.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                serde_json::json!({
                    "op": "add",
                    "path": "/fields/System.Tags",
                    "value": "rust; tui",
                }),
            ]
        );
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.tags.clone()),
            Some(vec!["rust".to_owned(), "tui".to_owned()]),
            "the Tags cell shows the normalised list at once"
        );
    }

    #[test]
    fn a_tag_list_that_normalises_to_what_is_there_writes_nothing() {
        let mut app = edit_app();

        open_editor(&mut app, 3);
        type_over(&mut app, "  rust ;; RUST ");
        assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None);
    }

    /// The Edit menu row for one command, found by the command itself so a new
    /// field editor above it moves nothing here.
    fn menu_row(command: CommandId) -> usize {
        EDIT_MENU
            .iter()
            .position(|entry| entry.command == command)
            .expect("the Edit menu offers the row")
    }

    #[test]
    fn the_description_row_hands_the_raw_html_to_the_editor() {
        let mut gamma = ticket(3, "Gamma", "2026-03-01T00:00:00Z");
        gamma.description_html = "<p>Hand it to <code>$EDITOR</code>.</p>".into();
        gamma.description = "Hand it to `$EDITOR`.".into();
        let mut app = App::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            gamma,
        ]);
        app.enable_sync();
        app.set_table_viewport(3);
        let key = app.selected_ticket().unwrap().key.clone();

        press(&mut app, KeyCode::Char('e'));
        for _ in 0..menu_row(CommandId::EditDescription) {
            press(&mut app, KeyCode::Down);
        }
        let action = press(&mut app, KeyCode::Enter);

        assert_eq!(
            action,
            AppAction::EditDescription {
                key,
                html: "<p>Hand it to <code>$EDITOR</code>.</p>".into(),
            },
            "the editor is opened on the markup Azure DevOps stores, not the reading of it"
        );
        assert_eq!(app.mode, AppMode::Browse, "the TUI is on its way out");
        assert!(
            !app.edits_pending(),
            "nothing is written until the editor is"
        );
        assert_eq!(app.notification(), None);
    }

    #[test]
    fn an_offline_run_refuses_the_description_before_the_editor_opens() {
        let mut app = App::new(vec![ticket(3, "Gamma", "2026-03-01T00:00:00Z")]);
        app.set_table_viewport(3);

        open_editor(&mut app, menu_row(CommandId::EditDescription));

        let (message, level) = app.notification().expect("an offline run says so");
        assert!(message.contains("#3 description not saved"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
        assert!(!app.edits_pending());
    }

    /// The Edit menu row that opens the comment box, found by the command it
    /// runs so adding a field editor above it moves nothing here.
    fn comment_row() -> usize {
        EDIT_MENU
            .iter()
            .position(|entry| entry.command == CommandId::AddComment)
            .expect("the Edit menu offers a comment row")
    }

    /// One comment as Azure DevOps hands it back, already carrying the id,
    /// date, and author only the server can give it.
    fn comment(id: i64, at: &str, text: &str) -> CommentRecord {
        CommentRecord {
            ticket: TicketKey {
                organization: "demo".into(),
                id: 3,
            },
            comment_id: id,
            created_at: crate::timestamp::ts(at),
            author: Some("Jacob Ragsdale".into()),
            text: text.into(),
        }
    }

    #[test]
    fn the_comment_prompt_opens_empty_and_posts_what_was_typed() {
        let mut app = edit_app();

        open_editor(&mut app, comment_row());
        assert_eq!(app.mode, AppMode::Prompt);
        assert_eq!(
            prompt_text(&app),
            "",
            "there is nothing to edit, only to say"
        );
        let prompt = app.prompt.as_ref().expect("a prompt should be open");
        assert_eq!(prompt.field, PromptField::Comment);
        assert_eq!(
            prompt.field.title(prompt.id),
            "Comment on #3",
            "the prompt names the work item it is about"
        );

        type_over(&mut app, "  Merged into main  ");
        let action = press(&mut app, KeyCode::Enter);
        assert_eq!(
            action,
            AppAction::Comment {
                key: app.selected_ticket().unwrap().key.clone(),
                text: "Merged into main".into(),
            },
            "the comment is trimmed before it is sent"
        );
        assert_eq!(app.mode, AppMode::Browse);
        assert!(app.prompt.is_none());
        assert!(
            app.comments_pending(),
            "the post is waiting on Azure DevOps"
        );
        assert!(
            app.comments_for(&app.selected_ticket().unwrap().key)
                .is_empty(),
            "nothing is shown until the server has stored it"
        );

        assert_eq!(
            app.comment_selected("And again".into()),
            AppAction::None,
            "one comment at a time"
        );
        let (message, level) = app.notification().expect("the second attempt says so");
        assert!(message.contains("still in flight"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
    }

    #[test]
    fn a_blank_comment_is_refused_locally_and_leaves_the_prompt_open() {
        let mut app = edit_app();

        open_editor(&mut app, comment_row());
        type_over(&mut app, "   ");
        assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
        assert_eq!(
            app.mode,
            AppMode::Prompt,
            "a blank comment leaves the prompt open to fix"
        );
        assert!(!app.comments_pending(), "nothing was sent");
        let (message, level) = app.notification().expect("a refusal is reported");
        assert!(message.contains("comment cannot be empty"), "{message}");
        assert_eq!(level, NotificationLevel::Error);

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, AppMode::Browse);
        assert!(app.prompt.is_none());
    }

    #[test]
    fn a_stored_comment_joins_the_discussion_in_date_order() {
        let mut app = edit_app();
        let key = app.selected_ticket().unwrap().key.clone();

        app.comment_selected("Merged into main".into());
        app.apply_comment(comment(9, "2026-03-04T00:00:00Z", "Merged into main"));

        assert!(!app.comments_pending(), "the post was answered");
        assert_eq!(
            app.comments_for(&key)
                .iter()
                .map(|held| held.text.as_str())
                .collect::<Vec<_>>(),
            ["Merged into main"]
        );
        let (message, level) = app.notification().expect("the post reports itself");
        assert_eq!(message, "Commented on #3");
        assert_eq!(level, NotificationLevel::Info);

        // A details fetch that lands afterwards brings the same comment back;
        // it replaces the one already held rather than doubling it, and an
        // older comment files ahead of it.
        app.apply_comment(comment(5, "2026-03-01T00:00:00Z", "Blocked on the API"));
        app.apply_comment(comment(9, "2026-03-04T00:00:00Z", "Merged into main"));
        assert_eq!(
            app.comments_for(&key)
                .iter()
                .map(|held| held.text.as_str())
                .collect::<Vec<_>>(),
            ["Blocked on the API", "Merged into main"]
        );
    }

    #[test]
    fn a_refused_comment_changes_nothing_and_says_why() {
        let mut app = edit_app();
        let key = app.selected_ticket().unwrap().key.clone();

        app.comment_selected("Merged into main".into());
        app.reject_comment(&key, "HTTP 403: the work item is read only");

        assert!(app.comments_for(&key).is_empty(), "nothing was filed");
        assert!(!app.comments_pending(), "the row is free to try again");
        let (message, level) = app.notification().expect("a refusal is reported");
        assert_eq!(
            message,
            "#3 comment not posted: HTTP 403: the work item is read only"
        );
        assert_eq!(level, NotificationLevel::Error);

        assert!(
            matches!(
                app.comment_selected("Merged into main".into()),
                AppAction::Comment { .. }
            ),
            "a refusal does not block the next attempt"
        );
    }

    #[test]
    fn a_prompt_takes_a_paste_at_its_caret() {
        let mut app = edit_app();

        open_editor(&mut app, 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.handle_paste("Pasted\ttitle");
        assert_eq!(prompt_text(&app), "Pastedtitle");
    }

    #[test]
    fn the_picker_lists_cached_states_and_otherwise_the_ones_already_in_the_database() {
        let typed = |id: i64, work_item_type: &str, state: &str| {
            let mut ticket = ticket(id, "Row", "2026-01-01T00:00:00Z");
            ticket.work_item_type = work_item_type.to_owned();
            ticket.state = state.to_owned();
            ticket
        };
        let mut app = App::new(vec![
            typed(1, "Bug", "Done"),
            typed(2, "Bug", "New"),
            typed(3, "Bug", "Active"),
            typed(4, "Bug", "New"),
            typed(5, "Bug", "Approved"),
            typed(6, "Task", "Doing"),
        ]);

        assert_eq!(
            state_names(&app.states_for("Bug")),
            ["Approved", "New", "Active", "Done"],
            "the fallback runs Proposed, InProgress, Resolved, Completed, Removed, then name"
        );
        assert_eq!(state_names(&app.states_for("Task")), ["Doing"]);
        assert!(
            app.states_for("Epic").is_empty(),
            "a type with no rows and nothing cached has no states"
        );

        let mut catalog = StateCatalog::default();
        catalog.insert(
            "Bug",
            vec![
                StateOption::new("New", StateCategory::Proposed),
                StateOption::new("Active", StateCategory::InProgress),
                StateOption::new("Resolved", StateCategory::Resolved),
                StateOption::new("Closed", StateCategory::Completed),
            ],
        );
        app.set_state_catalog(catalog);

        assert_eq!(
            state_names(&app.states_for("Bug")),
            ["New", "Active", "Resolved", "Closed"],
            "cached states win, in the order the process template runs them"
        );
        assert_eq!(
            state_names(&app.states_for("Task")),
            ["Doing"],
            "a type without cached states still falls back"
        );
    }

    /// An editable app whose rows name three different people, with the
    /// signed-in user holding none of them.
    fn assignee_app() -> App {
        let mut alpha = ticket(1, "Alpha", "2026-01-01T00:00:00Z");
        alpha.assigned_to = Some("Priya Nair".into());
        let mut beta = ticket(2, "Beta", "2026-02-01T00:00:00Z");
        beta.assigned_to = None;
        let mut gamma = ticket(3, "Gamma", "2026-03-01T00:00:00Z");
        gamma.assigned_to = Some("Avery Chen".into());
        let mut app = App::new(vec![alpha, beta, gamma]);
        app.enable_sync();
        app.set_table_viewport(3);
        app.set_me(Some("Jacob Ragsdale".into()));
        app
    }

    fn candidate_names(app: &App) -> Vec<String> {
        app.assignee_matches()
            .into_iter()
            .map(|candidate| candidate.display)
            .collect()
    }

    fn type_query(app: &mut App, text: &str) {
        for character in text.chars() {
            press(app, KeyCode::Char(character));
        }
    }

    #[test]
    fn the_assignee_picker_lists_nobody_then_me_then_the_database_and_starts_on_the_current_one() {
        let mut app = assignee_app();

        assert_eq!(
            press(&mut app, KeyCode::Char('a')),
            AppAction::FetchIdentities,
            "the first open asks for the project's teams"
        );
        assert_eq!(app.mode, AppMode::AssigneePicker);
        assert_eq!(
            candidate_names(&app),
            ["Unassigned", "Jacob Ragsdale", "Avery Chen", "Priya Nair"],
            "nobody, then me, then everybody the rows name, sorted"
        );
        assert!(
            app.assignee_matches()[1].me,
            "the signed-in user is marked as such"
        );
        assert_eq!(
            app.assignee_picker.index, 2,
            "the picker opens on whoever holds the work item"
        );
        assert_eq!(
            app.assignee_picker.scope,
            EditScope::Ticket(3),
            "it names the selected row"
        );

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, AppMode::Browse);
        assert_eq!(
            press(&mut app, KeyCode::Char('a')),
            AppAction::None,
            "the teams are asked for once a session"
        );
    }

    #[test]
    fn checking_several_rows_hands_all_of_them_to_whoever_the_picker_names() {
        let mut app = assignee_app();
        check_all(&mut app);

        press(&mut app, KeyCode::Char('a'));
        assert_eq!(
            app.assignee_picker.scope,
            EditScope::Checked(3),
            "reassigning a departing engineer's work is one change, not three"
        );
        press(&mut app, KeyCode::Up);
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing somebody should reassign every checked row");
        };

        assert_eq!(
            requests
                .iter()
                .map(|request| request.key.id)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        for request in &requests {
            assert_eq!(request.edit.summary(), "Assignee \u{2192} Jacob Ragsdale");
        }
        assert!(
            app.tickets()
                .iter()
                .all(|ticket| ticket.assigned_to.as_deref() == Some("Jacob Ragsdale")),
            "every row shows its new owner at once"
        );

        // Whoever holds the row under the cursor is a change worth making to
        // the others, so it is no longer the no-op it is for a single row.
        let mut app = assignee_app();
        check_all(&mut app);
        press(&mut app, KeyCode::Char('a'));
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("the other checked rows are held by somebody else");
        };
        assert_eq!(
            requests
                .iter()
                .map(|request| request.key.id)
                .collect::<Vec<_>>(),
            [1, 2],
            "#3 already holds it, so it is passed over rather than rewritten"
        );
    }

    #[test]
    fn typing_filters_the_assignee_picker_and_enter_assigns_who_is_left() {
        let mut app = assignee_app();
        app.set_identities(vec![Identity::new(
            "Jacob Ragsdale",
            Some("jacob@example.com".into()),
        )]);

        press(&mut app, KeyCode::Char('a'));
        type_query(&mut app, "jr");
        assert_eq!(
            candidate_names(&app),
            ["Jacob Ragsdale"],
            "the filter matches characters in order, not only whole words"
        );
        assert_eq!(
            app.assignee_picker.index, 0,
            "typing moves to the first hit"
        );

        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing somebody else should dispatch an edit");
        };
        let request = only(requests);
        assert_eq!(app.mode, AppMode::Browse);
        assert_eq!(request.key.id, 3);
        assert_eq!(
            request.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                serde_json::json!({
                    "op": "add",
                    "path": "/fields/System.AssignedTo",
                    "value": "jacob@example.com",
                }),
            ],
            "the write carries the address when the picker knows one"
        );
        assert_eq!(
            app.selected_ticket()
                .and_then(|ticket| ticket.assigned_to.clone()),
            Some("Jacob Ragsdale".to_owned()),
            "the cell reads as the display name, not the address"
        );
        assert!(app.is_mine(app.selected_ticket().unwrap()));
    }

    #[test]
    fn a_person_with_no_address_is_written_by_name_and_unassigned_removes_the_field() {
        let mut app = assignee_app();

        press(&mut app, KeyCode::Char('a'));
        type_query(&mut app, "priya");
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing somebody else should dispatch an edit");
        };
        let request = only(requests);
        assert_eq!(
            request.edit.patch(),
            vec![serde_json::json!({
                "op": "add",
                "path": "/fields/System.AssignedTo",
                "value": "Priya Nair",
            })],
            "a name the database only ever saw is sent as itself"
        );

        let mut app = assignee_app();
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("Unassigned should dispatch an edit");
        };
        let request = only(requests);
        assert_eq!(
            request.edit.patch(),
            vec![serde_json::json!({"op": "remove", "path": "/fields/System.AssignedTo"})],
            "nobody is written by taking the field off the work item"
        );
        assert_eq!(
            app.selected_ticket()
                .and_then(|ticket| ticket.assigned_to.clone()),
            None,
            "the Assignee cell empties at once"
        );
    }

    #[test]
    fn choosing_the_current_assignee_or_pressing_escape_writes_nothing() {
        let mut app = assignee_app();

        press(&mut app, KeyCode::Char('a'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            AppAction::None,
            "whoever holds it already is a no-op"
        );
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None, "a no-op closes silently");

        // The same again for a work item nobody holds, where Unassigned is the
        // row the picker opens on.
        app.select_row(1);
        assert_eq!(
            app.selected_ticket()
                .and_then(|ticket| ticket.assigned_to.clone()),
            None
        );
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.assignee_picker.index, 0);
        assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None);

        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Esc), AppAction::None);
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
    }

    #[test]
    fn team_members_land_in_an_open_picker_without_moving_the_cursor() {
        let mut app = assignee_app();

        press(&mut app, KeyCode::Char('a'));
        let focused = app.assignee_matches()[app.assignee_picker.index]
            .display
            .clone();
        assert_eq!(focused, "Avery Chen");

        app.merge_identities(vec![
            Identity::new("Avery Chen", Some("avery@example.com".into())),
            Identity::new("Dana Okafor", Some("dana@example.com".into())),
        ]);

        assert_eq!(
            candidate_names(&app),
            [
                "Unassigned",
                "Jacob Ragsdale",
                "Avery Chen",
                "Priya Nair",
                "Dana Okafor"
            ],
            "a team member nobody holds work for is appended after the database's"
        );
        assert_eq!(
            app.assignee_matches()[app.assignee_picker.index].display,
            focused,
            "the cursor stays on the person it was on"
        );
        assert_eq!(
            app.assignee_matches()[2].unique.as_deref(),
            Some("avery@example.com"),
            "somebody already listed gains the address the teams knew"
        );

        type_query(&mut app, "dana");
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("a merged-in team member should be choosable");
        };
        let request = only(requests);
        assert_eq!(
            request.edit.patch(),
            vec![serde_json::json!({
                "op": "add",
                "path": "/fields/System.AssignedTo",
                "value": "dana@example.com",
            })]
        );
    }

    /// The two trees a project with a nested quarter has, as a fetch flattens
    /// them. Sprint 1 is the one running today, whenever today is.
    fn classification_trees() -> Vec<ClassificationNode> {
        let today = Timestamp::now().calendar_date();
        let day = || Timestamp::parse(&format!("{today}T00:00:00Z")).ok();
        vec![
            ClassificationNode::new(NodeKind::Area, "development", 0),
            ClassificationNode::new(NodeKind::Area, "development\\Platform", 1),
            ClassificationNode::new(NodeKind::Iteration, "development", 0),
            ClassificationNode {
                start_date: day(),
                finish_date: day(),
                ..ClassificationNode::new(NodeKind::Iteration, "development\\Sprint 1", 1)
            },
            ClassificationNode::new(NodeKind::Iteration, "development\\Q3", 1),
            ClassificationNode::new(NodeKind::Iteration, "development\\Q3\\Sprint 7", 2),
        ]
    }

    /// An editable app whose selected row is planned into `development\Q3` and
    /// `development\Platform`, both nodes of the trees above.
    fn planned_app() -> App {
        let mut app = edit_app();
        let planned: Vec<Ticket> = app
            .tickets()
            .iter()
            .map(|ticket| Ticket {
                iteration_path: "development\\Q3".into(),
                area_path: "development\\Platform".into(),
                ..ticket.clone()
            })
            .collect();
        app.replace_prepared_tickets(PreparedTickets::new(planned));
        app.set_table_viewport(3);
        app
    }

    /// The same app with the project's trees already cached.
    fn node_app() -> App {
        let mut app = planned_app();
        app.set_classification_nodes(classification_trees(), None);
        app
    }

    /// The rows the open picker is showing, as they are drawn: the indent, the
    /// leaf, and whether the row is marked as running today.
    fn node_rows(app: &App) -> Vec<String> {
        app.node_matches()
            .into_iter()
            .map(|row| {
                let current = if row.current_period { " current" } else { "" };
                format!("{}{}{current}", row.indent(), row.leaf())
            })
            .collect()
    }

    /// Runs the Edit menu's Iteration or Area row.
    fn open_nodes(app: &mut App, kind: NodeKind) -> AppAction {
        app.run_command(match kind {
            NodeKind::Iteration => CommandId::EditIteration,
            NodeKind::Area => CommandId::EditArea,
        })
    }

    #[test]
    fn the_iteration_picker_draws_the_tree_indented_and_opens_on_the_current_node() {
        let mut app = node_app();

        assert_eq!(
            open_nodes(&mut app, NodeKind::Iteration),
            AppAction::FetchClassificationNodes,
            "the first open asks for the project's trees"
        );
        assert_eq!(app.mode, AppMode::NodePicker);
        assert_eq!(
            node_rows(&app),
            ["development", "  Sprint 1 current", "  Q3", "    Sprint 7"],
            "two spaces a level, the leaf named, and the sprint running today marked"
        );
        assert!(
            app.node_matches()[1].dates.is_some(),
            "a scheduled iteration carries its date range"
        );
        assert_eq!(
            app.node_picker.index, 2,
            "the picker opens on the node the work item sits in"
        );
        assert_eq!(app.node_picker.current, "development\\Q3");
        assert_eq!(
            app.node_picker.scope,
            EditScope::Ticket(3),
            "it names the selected row"
        );

        press(&mut app, KeyCode::Esc);
        assert_eq!(
            open_nodes(&mut app, NodeKind::Iteration),
            AppAction::None,
            "the trees are asked for once a session, so the second open is instant"
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(
            open_nodes(&mut app, NodeKind::Area),
            AppAction::None,
            "and the other picker shares that one fetch"
        );
        assert_eq!(
            node_rows(&app),
            ["development", "  Platform"],
            "the area picker draws the other tree, with no dates on it"
        );
        assert_eq!(app.node_picker.index, 1);
    }

    #[test]
    fn enter_on_another_node_writes_the_full_path_and_the_row_shows_its_leaf() {
        let mut app = node_app();

        open_nodes(&mut app, NodeKind::Iteration);
        type_query(&mut app, "sprint 1");
        assert_eq!(
            node_rows(&app),
            ["  Sprint 1 current"],
            "the filter matches characters in order over the whole path"
        );
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing another node should dispatch an edit");
        };
        let request = only(requests);

        assert_eq!(app.mode, AppMode::Browse);
        assert_eq!(request.key.id, 3);
        assert_eq!(
            request.edit.patch(),
            vec![serde_json::json!({
                "op": "add",
                "path": "/fields/System.IterationPath",
                "value": "development\\Sprint 1",
            })],
            "the write carries the full backslash path, not the leaf"
        );
        let moved = app.selected_ticket().expect("a selected row");
        assert_eq!(moved.iteration_path, "development\\Sprint 1");
        assert_eq!(
            path_leaf(&moved.iteration_path),
            "Sprint 1",
            "the Iteration column goes on showing the leaf"
        );

        let mut app = node_app();
        open_nodes(&mut app, NodeKind::Area);
        press(&mut app, KeyCode::Up);
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing another area should dispatch an edit");
        };
        let request = only(requests);
        assert_eq!(
            request.edit.patch(),
            vec![serde_json::json!({
                "op": "add",
                "path": "/fields/System.AreaPath",
                "value": "development",
            })]
        );
        assert_eq!(
            app.selected_ticket().map(|ticket| ticket.area_path.clone()),
            Some("development".to_owned())
        );
    }

    #[test]
    fn choosing_the_node_the_work_item_is_already_in_writes_nothing() {
        let mut app = node_app();

        open_nodes(&mut app, NodeKind::Iteration);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            AppAction::None,
            "the node it sits in already is a no-op"
        );
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
        assert_eq!(app.notification(), None, "a no-op closes silently");

        open_nodes(&mut app, NodeKind::Iteration);
        press(&mut app, KeyCode::Up);
        assert_eq!(press(&mut app, KeyCode::Esc), AppAction::None);
        assert_eq!(app.mode, AppMode::Browse);
        assert!(!app.edits_pending());
    }

    #[test]
    fn checking_several_rows_moves_them_all_to_the_sprint_chosen_but_not_to_an_area() {
        let mut app = node_app();
        check_all(&mut app);

        open_nodes(&mut app, NodeKind::Iteration);
        assert_eq!(
            app.node_picker.scope,
            EditScope::Checked(3),
            "a sprint's leftovers move on together"
        );
        press(&mut app, KeyCode::Up);
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing a sprint should move every checked row");
        };
        assert_eq!(
            requests
                .iter()
                .map(|request| request.key.id)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(
            app.tickets()
                .iter()
                .all(|ticket| ticket.iteration_path == "development\\Sprint 1"),
            "every row carries the full path at once"
        );

        let mut app = node_app();
        check_all(&mut app);
        open_nodes(&mut app, NodeKind::Area);
        assert_eq!(
            app.node_picker.scope,
            EditScope::Ticket(3),
            "the area tree stays on the row under the cursor"
        );
        press(&mut app, KeyCode::Up);
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("choosing another area should dispatch an edit");
        };
        assert_eq!(only(requests).key.id, 3);
    }

    #[test]
    fn a_picker_with_nothing_cached_lists_the_paths_the_database_holds() {
        let mut app = planned_app();

        open_nodes(&mut app, NodeKind::Iteration);
        assert_eq!(
            node_rows(&app),
            ["  Q3"],
            "every work item is in development\\Q3, indented by its own depth"
        );
        assert_eq!(app.node_picker.index, 0, "which is where the cursor starts");

        press(&mut app, KeyCode::Esc);
        open_nodes(&mut app, NodeKind::Area);
        assert_eq!(node_rows(&app), ["  Platform"]);
    }

    #[test]
    fn fetched_trees_land_in_an_open_picker_without_moving_the_cursor() {
        let mut app = planned_app();

        assert_eq!(
            open_nodes(&mut app, NodeKind::Iteration),
            AppAction::FetchClassificationNodes
        );
        assert_eq!(node_rows(&app), ["  Q3"]);
        let focused = app.node_matches()[app.node_picker.index].path.clone();

        app.merge_classification_nodes(classification_trees());
        assert_eq!(
            node_rows(&app),
            ["development", "  Sprint 1 current", "  Q3", "    Sprint 7"],
            "the fetched tree replaces the fallback in the open picker"
        );
        assert_eq!(
            app.node_matches()[app.node_picker.index].path,
            focused,
            "the cursor stays on the node it was on"
        );

        type_query(&mut app, "q3s7");
        let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
            panic!("a merged-in node should be choosable");
        };
        let request = only(requests);
        assert_eq!(
            request.edit.patch(),
            vec![serde_json::json!({
                "op": "add",
                "path": "/fields/System.IterationPath",
                "value": "development\\Q3\\Sprint 7",
            })]
        );
    }

    #[test]
    fn the_current_iteration_is_the_scheduled_one_containing_today() {
        let mut app = planned_app();
        assert_eq!(
            app.current_iteration(),
            None,
            "a project whose trees were never fetched has no current sprint"
        );

        app.set_classification_nodes(classification_trees(), None);
        assert_eq!(
            app.current_iteration(),
            Some("development\\Sprint 1".to_owned())
        );

        let undated: Vec<ClassificationNode> = classification_trees()
            .into_iter()
            .map(|node| ClassificationNode::new(node.kind, node.path, node.depth))
            .collect();
        app.set_classification_nodes(undated, None);
        assert_eq!(
            app.current_iteration(),
            None,
            "an iteration nobody scheduled is never the current one"
        );
    }

    /// A work item in `state`, last touched `changed_at`.
    fn aged(id: i64, state: &str, changed_at: &str) -> Ticket {
        Ticket {
            state: state.into(),
            ..ticket(id, "Neglected", changed_at)
        }
    }

    #[test]
    fn the_stale_threshold_flags_open_work_past_it_and_never_finished_work() {
        let now = crate::timestamp::ts("2026-08-29T12:00:00Z");
        let app = App::new(vec![]);

        assert_eq!(app.stale_days(), DEFAULT_STALE_DAYS);
        assert_eq!(
            BUILTIN_VIEWS
                .iter()
                .find(|view| view.name == "Stale")
                .map(|view| view.query),
            Some(stale_query(DEFAULT_STALE_DAYS).as_str()),
            "the built-in view asks the question the highlight answers"
        );
        assert_eq!(
            app.stale_age_days_at(&aged(1, "To Do", "2026-08-08T12:00:00Z"), now),
            Some(21),
            "three weeks untouched is flagged, and the pane says how long"
        );
        assert_eq!(
            app.stale_age_days_at(&aged(2, "To Do", "2026-08-15T12:00:00Z"), now),
            None,
            "exactly fourteen days has not crossed the threshold yet"
        );
        assert_eq!(
            app.stale_age_days_at(&aged(3, "To Do", "2026-08-15T11:59:59Z"), now),
            Some(14),
            "a second past it has"
        );
        for finished in ["Done", "Closed", "Removed"] {
            assert_eq!(
                app.stale_age_days_at(&aged(4, finished, "2025-01-01T00:00:00Z"), now),
                None,
                "{finished} work is never stale, whatever its age"
            );
        }
    }

    #[test]
    fn the_stale_threshold_takes_the_flag_over_the_session_and_the_palette_over_both() {
        let now = crate::timestamp::ts("2026-08-29T12:00:00Z");
        let three_weeks_old = aged(1, "To Do", "2026-08-08T12:00:00Z");
        let mut app = App::new(vec![]);

        app.set_stale_days(30);
        let session = app.snapshot_session();
        let mut restored = App::new(vec![]);
        restored.restore_session(session.clone());
        assert_eq!(restored.stale_days(), 30, "the session remembers it");
        assert_eq!(restored.stale_age_days_at(&three_weeks_old, now), None);

        // `--stale-days`, or TICKET_TUI_STALE_DAYS, is applied after the
        // session has been restored, and beats what it carried.
        restored.override_stale_days(7);
        assert_eq!(restored.stale_days(), 7);
        assert_eq!(restored.stale_age_days_at(&three_weeks_old, now), Some(21));
        assert_eq!(
            restored.snapshot_session().stale_days,
            30,
            "a flag passed once does not quietly become the setting"
        );

        restored.run_command(CommandId::SetStaleThreshold);
        assert_eq!(
            restored.stale_days(),
            14,
            "the palette steps up from the seven days in force"
        );
        assert_eq!(
            restored.snapshot_session().stale_days,
            14,
            "and the palette is what gets remembered"
        );
    }

    #[test]
    fn setting_the_stale_threshold_steps_through_the_choices_and_names_the_query() {
        let mut app = App::new(vec![]);

        let steps: Vec<u16> = (0..5)
            .map(|_| {
                app.run_command(CommandId::SetStaleThreshold);
                app.stale_days()
            })
            .collect();
        assert_eq!(
            steps,
            vec![21, 30, 7, 14, 21],
            "the choices step upward and wrap round"
        );
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("Stale after 21 days · changed:>21d state:@open"),
            "the status names the query the highlight stands for"
        );
        assert!(app.session_dirty, "moving the setting is worth saving");
    }

    #[test]
    fn a_threshold_of_no_days_at_all_is_held_at_the_one_day_floor() {
        let mut app = App::new(vec![]);

        app.override_stale_days(0);
        assert_eq!(app.stale_days(), 1);

        app.set_stale_days(0);
        assert_eq!(app.stale_days(), 1);

        let mut restored = App::new(vec![]);
        restored.restore_session(Session {
            stale_days: 0,
            ..Session::default()
        });
        assert_eq!(restored.stale_days(), 1, "including one edited by hand");
    }

    #[test]
    fn a_pull_without_cached_states_keeps_the_ones_an_earlier_pull_brought() {
        let mut app = picker_app();
        let tickets = app.tickets().to_vec();

        app.replace_prepared_tickets(PreparedTickets::new(tickets.clone()));
        assert_eq!(
            state_names(&app.states_for("Task")),
            ["To Do", "Doing", "Done"],
            "a pull that has not read the states endpoint must not empty the picker"
        );

        let mut catalog = StateCatalog::default();
        catalog.insert(
            "Task",
            vec![StateOption::new("Cut", StateCategory::Removed)],
        );
        app.replace_prepared_tickets(PreparedTickets::new(tickets).with_states(catalog));
        assert_eq!(state_names(&app.states_for("Task")), ["Cut"]);
    }
}
