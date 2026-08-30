use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::TabId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandId {
    Search,
    Palette,
    Filters,
    MoreFilters,
    Columns,
    Views,
    EditMenu,
    ChangeState,
    EditTitle,
    EditPriority,
    EditTags,
    EditAssignee,
    EditIteration,
    EditArea,
    SetParent,
    RemoveParent,
    EditDescription,
    AddComment,
    NewWorkItem,
    NewChild,
    DeleteWorkItem,
    UndoEdit,
    Sort,
    Help,
    Sync,
    Open,
    ToggleDensity,
    ToggleDetails,
    ToggleSearchOrder,
    ToggleFinished,
    ToggleBookmark,
    CopyId,
    CopyUrl,
    CopyTitle,
    CopyMarkdown,
    CopySummary,
    ExportJson,
    ExportCsv,
    SelectAll,
    ClearSelection,
    HistoryBack,
    HistoryForward,
    CloneRepo,
    FetchRepo,
    PullRepo,
    /// The Pull requests tab's verbs: the four votes and their undo, and
    /// the three ways a pull request is closed or set to close itself.
    ApprovePr,
    SuggestPr,
    WaitPr,
    RejectPr,
    UndoVote,
    CompletePr,
    AbandonPr,
    AutoCompletePr,
    CommentPr,
    ToggleClosedPrs,
    /// The Pipelines tab's verbs.
    RunPipeline,
    CancelRun,
    RetryRun,
    WatchRun,
    Approvals,
    SaveView,
    SprintSummary,
    DatabaseInfo,
    Quit,
    ResetPaneSplit,
    SetStaleThreshold,
}

/// A single binding: the key code plus the modifiers crossterm reports with it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Key {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl Key {
    /// Human readable binding, e.g. `s`, `Ctrl-C`, or `Space`.
    #[must_use]
    pub fn label(self) -> String {
        let base = match self.code {
            KeyCode::Char(' ') => "Space".to_owned(),
            KeyCode::Char(ch) => ch.to_string(),
            KeyCode::Enter => "Enter".to_owned(),
            KeyCode::Esc => "Esc".to_owned(),
            KeyCode::Tab => "Tab".to_owned(),
            other => format!("{other:?}"),
        };
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            format!("Ctrl-{}", base.to_ascii_uppercase())
        } else {
            base
        }
    }
}

#[must_use]
pub const fn key(ch: char) -> Key {
    Key {
        code: KeyCode::Char(ch),
        modifiers: KeyModifiers::NONE,
    }
}

#[must_use]
pub const fn ctrl(ch: char) -> Key {
    Key {
        code: KeyCode::Char(ch),
        modifiers: KeyModifiers::CONTROL,
    }
}

/// Where a command applies: everywhere, or on the one tab that owns it. The
/// palette lists the global commands and the active tab's, and a key bound to
/// another tab's command does nothing here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    Global,
    /// The tabs whose `run_command` answers this command. More than one when
    /// two screens read the same verb their own way — `Copy ID` is the work
    /// item's number on one tab and the repository's clone URL on another.
    Tabs(&'static [TabId]),
}

/// One action, defined once: its palette title, its bindings, its help text,
/// and the tabs it belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Command {
    pub id: CommandId,
    pub title: &'static str,
    pub keys: &'static [Key],
    pub help: &'static str,
    pub scope: Scope,
}

impl Scope {
    /// Whether a command in this scope is offered on `tab`.
    #[must_use]
    pub fn covers(self, tab: TabId) -> bool {
        match self {
            Self::Global => true,
            Self::Tabs(owners) => owners.contains(&tab),
        }
    }
}

impl Command {
    /// Bindings joined for display, e.g. `p / :`. Empty when the command has none.
    #[must_use]
    pub fn key_label(&self) -> String {
        self.keys
            .iter()
            .map(|key| key.label())
            .collect::<Vec<_>>()
            .join(" / ")
    }

    /// Whether the command palette offers this command.
    #[must_use]
    pub const fn in_palette(&self) -> bool {
        !matches!(self.id, CommandId::Search)
    }
}

pub const COMMANDS: &[Command] = &[
    Command {
        id: CommandId::Search,
        title: "Search tickets",
        keys: &[key('/')],
        help: "Core ticket fields",
        scope: Scope::Global,
    },
    Command {
        id: CommandId::Filters,
        title: "Open filter bar",
        keys: &[key('f')],
        help: "Space toggles values",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::MoreFilters,
        title: "More filters",
        keys: &[key('F')],
        help: "Priority, project, area…",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::Columns,
        title: "Configure columns",
        keys: &[key('c')],
        help: "Show, hide, reorder",
        scope: Scope::Global,
    },
    Command {
        id: CommandId::Views,
        title: "Named views",
        keys: &[key('v')],
        help: "Save and restore",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::EditMenu,
        title: "Actions\u{2026}",
        keys: &[key('e')],
        help: "Change a field",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::ChangeState,
        title: "Change state",
        keys: &[key('S')],
        help: "Move the work item",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::CloneRepo,
        title: "Clone repository",
        keys: &[key('C')],
        help: "Into the workspace",
        scope: Scope::Tabs(&[TabId::Repos]),
    },
    Command {
        id: CommandId::FetchRepo,
        title: "Fetch",
        keys: &[key('G')],
        help: "git fetch --prune",
        scope: Scope::Tabs(&[TabId::Repos]),
    },
    Command {
        id: CommandId::PullRepo,
        title: "Pull",
        keys: &[key('P')],
        help: "Fast-forward only",
        scope: Scope::Tabs(&[TabId::Repos]),
    },
    Command {
        id: CommandId::ApprovePr,
        title: "Approve",
        keys: &[key('a')],
        help: "Vote on the pull request",
        scope: Scope::Tabs(&[TabId::PullRequests]),
    },
    Command {
        id: CommandId::SuggestPr,
        title: "Approve with suggestions",
        keys: &[key('A')],
        help: "",
        scope: Scope::Tabs(&[TabId::PullRequests]),
    },
    Command {
        id: CommandId::WaitPr,
        title: "Wait for author",
        keys: &[key('w')],
        help: "",
        scope: Scope::Tabs(&[TabId::PullRequests]),
    },
    Command {
        id: CommandId::RejectPr,
        title: "Reject",
        keys: &[key('x')],
        help: "",
        scope: Scope::Tabs(&[TabId::PullRequests]),
    },
    Command {
        id: CommandId::UndoVote,
        title: "Undo last vote",
        keys: &[key('u')],
        help: "Put the previous vote back",
        scope: Scope::Tabs(&[TabId::PullRequests]),
    },
    Command {
        id: CommandId::CompletePr,
        title: "Complete",
        keys: &[key('C')],
        help: "Merge it, after a short form",
        scope: Scope::Tabs(&[TabId::PullRequests]),
    },
    Command {
        id: CommandId::AbandonPr,
        title: "Abandon",
        keys: &[key('X')],
        help: "Asks once more",
        scope: Scope::Tabs(&[TabId::PullRequests]),
    },
    Command {
        id: CommandId::AutoCompletePr,
        title: "Toggle auto-complete",
        keys: &[key('t')],
        help: "Complete when the policies pass",
        scope: Scope::Tabs(&[TabId::PullRequests]),
    },
    Command {
        id: CommandId::CommentPr,
        title: "Comment",
        keys: &[key('n')],
        help: "One line, as a new thread",
        scope: Scope::Tabs(&[TabId::PullRequests]),
    },
    Command {
        id: CommandId::ToggleClosedPrs,
        title: "Show or hide closed pull requests",
        keys: &[],
        help: "Completed and abandoned",
        scope: Scope::Tabs(&[TabId::PullRequests]),
    },
    Command {
        id: CommandId::RunPipeline,
        title: "Run pipeline",
        keys: &[key('t')],
        help: "Pick a branch and start it",
        scope: Scope::Tabs(&[TabId::Pipelines]),
    },
    Command {
        id: CommandId::CancelRun,
        title: "Cancel run",
        keys: &[key('x')],
        help: "Asks once more",
        scope: Scope::Tabs(&[TabId::Pipelines]),
    },
    Command {
        id: CommandId::RetryRun,
        title: "Retry failed jobs",
        keys: &[key('R')],
        help: "On a run that failed or was canceled",
        scope: Scope::Tabs(&[TabId::Pipelines]),
    },
    Command {
        id: CommandId::WatchRun,
        title: "Watch run",
        keys: &[key('W')],
        help: "Say when it stops, on any tab",
        scope: Scope::Tabs(&[TabId::Pipelines]),
    },
    Command {
        id: CommandId::Approvals,
        title: "Approvals",
        keys: &[key('A')],
        help: "What is waiting on a person",
        scope: Scope::Tabs(&[TabId::Pipelines]),
    },
    Command {
        id: CommandId::EditTitle,
        title: "Edit title",
        keys: &[],
        help: "Rename the work item",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::EditPriority,
        title: "Edit priority",
        keys: &[],
        help: "1\u{2013}4, or clear it",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::EditTags,
        title: "Edit tags",
        keys: &[],
        help: "Semicolon separated",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::EditAssignee,
        title: "Change assignee",
        keys: &[key('a')],
        help: "Pick who owns it",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::EditIteration,
        title: "Change iteration",
        keys: &[],
        help: "Move it to a sprint",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::EditArea,
        title: "Change area",
        keys: &[],
        help: "Move it in the area tree",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::SetParent,
        title: "Set parent\u{2026}",
        keys: &[],
        help: "File it under another work item",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::RemoveParent,
        title: "Remove parent",
        keys: &[],
        help: "Detach it from its family",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::EditDescription,
        title: "Edit description",
        keys: &[],
        help: "Opens your $EDITOR",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::AddComment,
        title: "Add comment",
        keys: &[],
        help: "One line on the discussion",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::NewWorkItem,
        title: "New work item",
        keys: &[key('n')],
        help: "Ctrl-S creates it",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::NewChild,
        title: "New child",
        keys: &[key('N')],
        help: "Under the selected one",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    // Deliberately unbound. Every other editor is a keypress away because the
    // worst it can do is a wrong value somebody types over; this one takes the
    // work item off the board, so it is reached by name and confirmed.
    Command {
        id: CommandId::DeleteWorkItem,
        title: "Delete work item…",
        keys: &[],
        help: "To the recycle bin",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::UndoEdit,
        title: "Undo last edit",
        keys: &[key('u')],
        help: "Put the value back",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::SaveView,
        title: "Save current view",
        keys: &[key('V')],
        help: "",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::Sort,
        title: "Sort tickets",
        keys: &[key('s')],
        help: "Choose field and direction",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::ToggleDensity,
        title: "Toggle row density",
        keys: &[],
        help: "",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::ToggleDetails,
        title: "Toggle details pane",
        keys: &[key('d')],
        help: "Below 70 columns",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::ToggleSearchOrder,
        title: "Toggle search order",
        keys: &[],
        help: "Relevance or fields",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::ToggleFinished,
        // Reworded as it runs by `finished_title`, so the row always names the
        // change it makes rather than the state it is in.
        title: "Show finished tickets",
        keys: &[],
        help: "Done and Removed rows",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::Open,
        title: "Open ticket in browser",
        keys: &[key('o')],
        help: "",
        scope: Scope::Global,
    },
    Command {
        id: CommandId::ToggleBookmark,
        title: "Toggle bookmark",
        keys: &[key('m')],
        help: "",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::HistoryBack,
        title: "Back to previous ticket",
        keys: &[key('[')],
        help: "",
        scope: Scope::Global,
    },
    Command {
        id: CommandId::HistoryForward,
        title: "Forward to next ticket",
        keys: &[key(']')],
        help: "",
        scope: Scope::Global,
    },
    Command {
        id: CommandId::CopyId,
        title: "Copy ID",
        keys: &[key('y')],
        help: "Selected or current tickets",
        scope: Scope::Tabs(&[TabId::WorkItems, TabId::Repos]),
    },
    Command {
        id: CommandId::CopyUrl,
        title: "Copy URL",
        keys: &[],
        help: "",
        scope: Scope::Tabs(&[TabId::WorkItems, TabId::Repos]),
    },
    Command {
        id: CommandId::CopyTitle,
        title: "Copy title",
        keys: &[],
        help: "",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::CopyMarkdown,
        title: "Copy Markdown link",
        keys: &[],
        help: "",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::CopySummary,
        title: "Copy summary",
        keys: &[],
        help: "",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::ExportJson,
        title: "Export selected as JSON",
        keys: &[],
        help: "",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::ExportCsv,
        title: "Export selected as CSV",
        keys: &[],
        help: "",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::SelectAll,
        title: "Select all visible",
        keys: &[],
        help: "",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::ClearSelection,
        title: "Clear selection",
        keys: &[],
        help: "",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::SprintSummary,
        title: "Sprint summary",
        keys: &[],
        help: "Who has what this iteration",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::DatabaseInfo,
        title: "Database info",
        keys: &[key('i')],
        help: "Path, counts, freshness",
        scope: Scope::Global,
    },
    Command {
        id: CommandId::Sync,
        title: "Sync from Azure DevOps",
        keys: &[key('r')],
        help: "Pull work items now",
        scope: Scope::Global,
    },
    Command {
        id: CommandId::Help,
        title: "Help",
        keys: &[key('?')],
        help: "",
        scope: Scope::Global,
    },
    Command {
        id: CommandId::Palette,
        title: "Command palette",
        keys: &[key('p'), key(':')],
        help: "",
        scope: Scope::Global,
    },
    Command {
        id: CommandId::Quit,
        title: "Quit",
        keys: &[key('q'), ctrl('c')],
        help: "",
        scope: Scope::Global,
    },
    Command {
        id: CommandId::ResetPaneSplit,
        title: "Reset pane split",
        keys: &[],
        help: "Restore the 62/56 layout",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
    Command {
        id: CommandId::SetStaleThreshold,
        title: "Set stale threshold",
        keys: &[],
        help: "7, 14, 21, or 30 days",
        scope: Scope::Tabs(&[TabId::WorkItems]),
    },
];

/// One row of the Actions menu: what it changes about the work item, and the
/// command that does it. Every field editor is one row here, so adding an
/// editor is adding its command to [`COMMANDS`] and its field to this table;
/// the rows under the fields act on the work item as a whole rather than on
/// one of its fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditMenuEntry {
    /// The field's name, as the menu lists it.
    pub label: &'static str,
    pub command: CommandId,
}

/// The one row the Actions menu does not always offer: taking a work item out of
/// its family only reads as a choice when it is in one, so this follows
/// `Set parent\u{2026}` when the work item has a parent and is left out when it
/// has none.
pub const REMOVE_PARENT_ROW: EditMenuEntry = EditMenuEntry {
    label: "Remove parent",
    command: CommandId::RemoveParent,
};

pub const EDIT_MENU: &[EditMenuEntry] = &[
    EditMenuEntry {
        label: "State",
        command: CommandId::ChangeState,
    },
    EditMenuEntry {
        label: "Title",
        command: CommandId::EditTitle,
    },
    EditMenuEntry {
        label: "Priority",
        command: CommandId::EditPriority,
    },
    EditMenuEntry {
        label: "Tags",
        command: CommandId::EditTags,
    },
    EditMenuEntry {
        label: "Assignee",
        command: CommandId::EditAssignee,
    },
    EditMenuEntry {
        label: "Iteration",
        command: CommandId::EditIteration,
    },
    EditMenuEntry {
        label: "Area",
        command: CommandId::EditArea,
    },
    EditMenuEntry {
        label: "Set parent\u{2026}",
        command: CommandId::SetParent,
    },
    EditMenuEntry {
        label: "Description",
        command: CommandId::EditDescription,
    },
    EditMenuEntry {
        label: "Add comment",
        command: CommandId::AddComment,
    },
    EditMenuEntry {
        label: "New child",
        command: CommandId::NewChild,
    },
    EditMenuEntry {
        label: "Delete work item\u{2026}",
        command: CommandId::DeleteWorkItem,
    },
];

/// The bindings one command answers to, for menus that name a command rather
/// than a key.
#[must_use]
pub fn key_label_for(id: CommandId) -> String {
    COMMANDS
        .iter()
        .find(|command| command.id == id)
        .map(Command::key_label)
        .unwrap_or_default()
}

/// crossterm reports SHIFT alongside the uppercase character it produced, so drop
/// it before comparing a pressed key against the bindings.
fn normalized_modifiers(event: KeyEvent) -> KeyModifiers {
    if matches!(event.code, KeyCode::Char(ch) if ch.is_uppercase()) {
        event.modifiers.difference(KeyModifiers::SHIFT)
    } else {
        event.modifiers
    }
}

#[must_use]
pub fn command_for_key(event: KeyEvent, tab: TabId) -> Option<CommandId> {
    let modifiers = normalized_modifiers(event);
    COMMANDS
        .iter()
        .filter(|command| command.scope.covers(tab))
        .find(|command| {
            command
                .keys
                .iter()
                .any(|bound| bound.code == event.code && bound.modifiers == modifiers)
        })
        .map(|command| command.id)
}

/// How the finished-tickets toggle reads while finished work is hidden or
/// listed. The row names the change it makes rather than the state it is in,
/// which is how every other verb in the palette reads.
#[must_use]
pub const fn finished_title(finished_hidden: bool) -> &'static str {
    if finished_hidden {
        "Show finished tickets"
    } else {
        "Hide finished tickets"
    }
}

/// The command as the palette should list it right now. Only the finished
/// toggle differs from its entry in [`COMMANDS`], and it is reworded before
/// the query is matched so a command is found under the words it is showing.
fn worded(command: Command, finished_hidden: bool, tab: TabId) -> Command {
    let title = match command.id {
        CommandId::ToggleFinished => finished_title(finished_hidden),
        // Four screens share one verb; each opens a different thing, and the
        // palette should say which.
        CommandId::Open => match tab {
            TabId::WorkItems => "Open ticket in browser",
            TabId::Repos => "Open repository in browser",
            TabId::PullRequests => "Open pull request in browser",
            TabId::Pipelines => "Open run in browser",
        },
        _ => return command,
    };
    Command { title, ..command }
}

#[must_use]
pub fn matching_commands(query: &str, finished_hidden: bool, tab: TabId) -> Vec<Command> {
    let query = query.trim().to_ascii_lowercase();
    COMMANDS
        .iter()
        .copied()
        .filter(|command| command.scope.covers(tab))
        .filter(Command::in_palette)
        .map(|command| worded(command, finished_hidden, tab))
        .filter(|command| {
            query.is_empty()
                || command.title.to_ascii_lowercase().contains(&query)
                || command.key_label().to_ascii_lowercase().contains(&query)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn palette_filters_commands_by_title() {
        let matches = matching_commands("copy", true, TabId::WorkItems);
        assert!(
            matches
                .iter()
                .any(|command| command.id == CommandId::CopyId)
        );
        assert!(!matches.iter().any(|command| command.id == CommandId::Sync));
        assert!(
            !matching_commands("", true, TabId::WorkItems)
                .iter()
                .any(|command| command.id == CommandId::Search),
            "commands that only work from browse mode stay out of the palette"
        );
    }

    #[test]
    fn the_finished_toggle_is_worded_as_the_change_it_would_make() {
        let hidden = matching_commands("show finished", true, TabId::WorkItems);
        assert_eq!(
            hidden.first().map(|command| command.id),
            Some(CommandId::ToggleFinished),
            "while they are hidden the palette offers to show them"
        );
        assert_eq!(hidden[0].title, "Show finished tickets");

        let shown = matching_commands("hide finished", false, TabId::WorkItems);
        assert_eq!(
            shown.first().map(|command| command.id),
            Some(CommandId::ToggleFinished)
        );
        assert_eq!(shown[0].title, "Hide finished tickets");
        assert!(
            matching_commands("show finished", false, TabId::WorkItems).is_empty(),
            "a command is only found under the words it is showing"
        );
    }

    #[test]
    fn keys_resolve_to_their_command() {
        assert_eq!(
            command_for_key(
                press(KeyCode::Char('s'), KeyModifiers::NONE),
                TabId::WorkItems
            ),
            Some(CommandId::Sort)
        );
        assert_eq!(
            command_for_key(
                press(KeyCode::Char(':'), KeyModifiers::NONE),
                TabId::WorkItems
            ),
            Some(CommandId::Palette)
        );
        assert_eq!(
            command_for_key(
                press(KeyCode::Char('p'), KeyModifiers::NONE),
                TabId::WorkItems
            ),
            Some(CommandId::Palette)
        );
        assert_eq!(
            command_for_key(
                press(KeyCode::Char('c'), KeyModifiers::CONTROL),
                TabId::WorkItems
            ),
            Some(CommandId::Quit)
        );
        assert_eq!(
            command_for_key(
                press(KeyCode::Char('c'), KeyModifiers::NONE),
                TabId::WorkItems
            ),
            Some(CommandId::Columns)
        );
        assert_eq!(
            command_for_key(
                press(KeyCode::Char('v'), KeyModifiers::NONE),
                TabId::WorkItems
            ),
            Some(CommandId::Views)
        );
        assert_eq!(
            command_for_key(
                press(KeyCode::Char('V'), KeyModifiers::SHIFT),
                TabId::WorkItems
            ),
            Some(CommandId::SaveView),
            "lowercase v opens the views, capital V saves one"
        );
        assert_eq!(
            command_for_key(press(KeyCode::Char('S'), KeyModifiers::SHIFT), TabId::Repos),
            None,
            "a work-item key does nothing on another tab"
        );
        assert_eq!(
            command_for_key(
                press(KeyCode::Char('S'), KeyModifiers::SHIFT),
                TabId::WorkItems
            ),
            Some(CommandId::ChangeState),
            "capital S is the state picker; lowercase s stays the sort menu"
        );
        assert_eq!(
            command_for_key(
                press(KeyCode::Char('N'), KeyModifiers::SHIFT),
                TabId::WorkItems
            ),
            Some(CommandId::NewChild),
            "capital N files a child; lowercase n stays the new work item form"
        );
        assert_eq!(
            command_for_key(
                press(KeyCode::Char('n'), KeyModifiers::NONE),
                TabId::WorkItems
            ),
            Some(CommandId::NewWorkItem)
        );
        assert_eq!(
            command_for_key(
                press(KeyCode::Char('e'), KeyModifiers::NONE),
                TabId::WorkItems
            ),
            Some(CommandId::EditMenu)
        );
        assert_eq!(
            command_for_key(press(KeyCode::Tab, KeyModifiers::NONE), TabId::WorkItems),
            None
        );
    }

    #[test]
    fn bound_commands_have_labels_and_unique_keys() {
        // A key may mean one thing on one tab and another on the next; it
        // may not mean two things anywhere a person could press it.
        let overlaps = |left: Scope, right: Scope| match (left, right) {
            (Scope::Global, _) | (_, Scope::Global) => true,
            (Scope::Tabs(a), Scope::Tabs(b)) => a.iter().any(|tab| b.contains(tab)),
        };
        let mut bound: Vec<(Key, Scope)> = Vec::new();
        for command in COMMANDS {
            if command.keys.is_empty() {
                assert!(command.key_label().is_empty(), "{:?}", command.id);
                continue;
            }
            assert!(!command.key_label().is_empty(), "{:?}", command.id);
            for key in command.keys {
                assert!(!key.label().is_empty(), "{:?}", command.id);
                assert!(
                    !bound
                        .iter()
                        .any(|(held, scope)| held == key && overlaps(*scope, command.scope)),
                    "duplicate binding {} for {:?}",
                    key.label(),
                    command.id
                );
                bound.push((*key, command.scope));
            }
        }

        assert_eq!(key('s').label(), "s");
        assert_eq!(ctrl('c').label(), "Ctrl-C");
        let palette = COMMANDS
            .iter()
            .find(|command| command.id == CommandId::Palette)
            .expect("palette command");
        assert_eq!(palette.key_label(), "p / :");
    }

    /// Every command a screen answers, mirroring the `run_command` match in
    /// `app/<tab>/mod.rs`. The four shared overlays are left out: `App` opens
    /// each of those over whichever tab is showing, so every tab answers them.
    fn answered_by(tab: TabId) -> &'static [CommandId] {
        match tab {
            TabId::Repos => &[
                CommandId::Search,
                CommandId::Open,
                CommandId::CopyUrl,
                CommandId::CopyId,
                CommandId::CloneRepo,
                CommandId::FetchRepo,
                CommandId::PullRepo,
                CommandId::Sync,
                CommandId::HistoryBack,
                CommandId::HistoryForward,
                CommandId::Quit,
            ],
            TabId::PullRequests => &[
                CommandId::Search,
                CommandId::Open,
                CommandId::Sync,
                CommandId::HistoryBack,
                CommandId::HistoryForward,
                CommandId::ApprovePr,
                CommandId::SuggestPr,
                CommandId::WaitPr,
                CommandId::RejectPr,
                CommandId::UndoVote,
                CommandId::CompletePr,
                CommandId::AbandonPr,
                CommandId::AutoCompletePr,
                CommandId::CommentPr,
                CommandId::ToggleClosedPrs,
                CommandId::Quit,
            ],
            TabId::Pipelines => &[
                CommandId::Search,
                CommandId::Open,
                CommandId::Sync,
                CommandId::HistoryBack,
                CommandId::HistoryForward,
                CommandId::RunPipeline,
                CommandId::Approvals,
                CommandId::CancelRun,
                CommandId::RetryRun,
                CommandId::WatchRun,
                CommandId::Quit,
            ],
            // The work items screen matches exhaustively and answers
            // everything except the other tabs' verbs, which are exactly the
            // commands no other tab shares with it.
            TabId::WorkItems => &[],
        }
    }

    /// The overlays `App` opens over any tab before the screen sees them.
    const SHARED: [CommandId; 4] = [
        CommandId::Help,
        CommandId::Palette,
        CommandId::Columns,
        CommandId::DatabaseInfo,
    ];

    #[test]
    fn no_tab_offers_a_command_its_screen_does_not_answer() {
        // A palette entry that does nothing reads as broken rather than as out
        // of scope, so a command's scope may only name a tab whose
        // `run_command` has an arm for it. Widening a scope without writing
        // that arm fails here.
        for tab in TabId::ALL {
            if tab == TabId::WorkItems {
                continue;
            }
            let answered = answered_by(tab);
            for command in matching_commands("", true, tab) {
                assert!(
                    answered.contains(&command.id) || SHARED.contains(&command.id),
                    "the {} palette offers {:?}, which that screen does not answer",
                    tab.label(),
                    command.id
                );
            }
        }
    }

    #[test]
    fn a_scope_names_every_tab_that_answers_the_command() {
        // The other direction: a screen that answers a command must be offered
        // it. Catches a scope narrowed past a tab that still has the arm.
        for tab in TabId::ALL {
            if tab == TabId::WorkItems {
                continue;
            }
            for id in answered_by(tab) {
                let command = COMMANDS
                    .iter()
                    .find(|command| command.id == *id)
                    .expect("every answered command is in the table");
                assert!(
                    command.scope.covers(tab),
                    "the {} screen answers {id:?}, but its scope does not reach that tab",
                    tab.label()
                );
            }
        }
    }
}
