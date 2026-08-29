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
    FacetValue, FilterField, FilterSet, FilterToken, MatchContext, ParsedQuery, Sentinel,
    days_untouched, facet_values, format_query, is_stale, parse_query, stale_query,
};
use crate::model::{
    CommentRecord, DetailsUpdate, FamilySnapshot, FamilyTreeEntry, HistoryRecord, Identity,
    RelationKind, RelationRecord, SortDirection, SortField, StateCatalog, StateCategory,
    StateOption, Ticket, TicketGraph, TicketKey, compare_tickets, path_leaf, same_text,
};
pub use crate::model::{RowDensity, SearchOrder};
use crate::pointer::{
    DragKind, PointerState, ScrollState, ScrollSurface, SelectableSurface, TextEditor, TextPos,
    TextSelection,
};
pub use crate::pointer::{EditableField, HitRegions, OverlayAnchor, PointerTarget};
use crate::search::{SearchDocuments, SearchEngine, SearchMatch};
use crate::session::{NamedView, Session};
use crate::sprint::{self, SprintSummary, SummaryRow, SummaryRowKind};
use crate::sync::{ReparentApplied, ReparentRejection};
use crate::text_input::TextInput;
use crate::timestamp::Timestamp;

pub mod cursor;
mod screen;
pub mod shell;
pub mod work_items;

pub use cursor::ListCursor;
pub use screen::Screen;
pub use shell::{
    DEFAULT_PANE_SPLIT_STACKED, DEFAULT_PANE_SPLIT_WIDE, DividerOrientation, Focus,
    NotificationLevel, PointerUpdate, Shell,
};
pub use work_items::{
    BuiltinView, ChildProgress, ChildProgressIndex, ColumnOverlay, DEFAULT_STALE_DAYS,
    DeleteConfirm, EditMenu, EditScope, FacetBar, FilterOverlay, FormField, FormFieldId,
    FormFieldKind, FormKind, FormOverlay, FormPicker, PRIORITY_CHOICES, PROGRESS_BAR_CELLS,
    PaletteState, PreparedTickets, PriorityPicker, PromptField, SortDraft, SprintOverlay,
    StatePicker, SyncTarget, TextPrompt, TypePicker, UNASSIGNED_LABEL, ViewRow, ViewRowKind,
    ViewsOverlay, WorkItemMode, WorkItemsScreen,
};

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
    /// Send work items to the project's recycle bin, one request each, which
    /// the worker takes in the order they are listed. Nothing leaves the table
    /// until Azure DevOps has taken the delete: a row dropped for a delete that
    /// was refused is a lie the next pull undoes.
    Delete(Vec<TicketKey>),
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

/// The application: the shell every screen shares, and the screens themselves.
/// There is one today; #665 puts a tab bar over them.
pub struct App {
    pub shell: Shell,
    pub work_items: WorkItemsScreen,
}

impl App {
    #[must_use]
    pub fn new(tickets: Vec<Ticket>) -> Self {
        let mut shell = Shell::default();
        let work_items = WorkItemsScreen::new(&mut shell, tickets);
        Self { shell, work_items }
    }

    /// The shell and the screen the keyboard and the mouse are talking to,
    /// handed back apart so an event can be given one with the other. #665
    /// makes which screen this is a matter of the tab bar.
    pub fn screen(&mut self) -> (&mut Shell, &mut dyn Screen) {
        (&mut self.shell, &mut self.work_items)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        let (shell, screen) = self.screen();
        screen.handle_key(shell, key)
    }

    /// The mouse still goes to the screen's own entry point: the pointer state
    /// it answers with is the shell's, not something a screen reports.
    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> PointerUpdate {
        self.work_items.handle_mouse(&mut self.shell, mouse)
    }

    pub fn handle_paste(&mut self, pasted: &str) {
        let (shell, screen) = self.screen();
        screen.handle_paste(shell, pasted);
    }

    pub fn handle_resize(&mut self) {
        self.shell.handle_resize();
    }
}
