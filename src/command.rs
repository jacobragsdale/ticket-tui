use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
    EditDescription,
    AddComment,
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
    SaveView,
    DatabaseInfo,
    Quit,
    ResetPaneSplit,
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

/// One action, defined once: its palette title, its bindings, and its help text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Command {
    pub id: CommandId,
    pub title: &'static str,
    pub keys: &'static [Key],
    pub help: &'static str,
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
    },
    Command {
        id: CommandId::Filters,
        title: "Open filter bar",
        keys: &[key('f')],
        help: "Space toggles values",
    },
    Command {
        id: CommandId::MoreFilters,
        title: "More filters",
        keys: &[key('+')],
        help: "Priority, project, area…",
    },
    Command {
        id: CommandId::Columns,
        title: "Configure columns",
        keys: &[key('w')],
        help: "Show, hide, reorder",
    },
    Command {
        id: CommandId::Views,
        title: "Named views",
        keys: &[key('V')],
        help: "Save and restore",
    },
    Command {
        id: CommandId::EditMenu,
        title: "Edit\u{2026}",
        keys: &[key('e')],
        help: "Change a field",
    },
    Command {
        id: CommandId::ChangeState,
        title: "Change state",
        keys: &[key('S')],
        help: "Move the work item",
    },
    Command {
        id: CommandId::EditTitle,
        title: "Edit title",
        keys: &[],
        help: "Rename the work item",
    },
    Command {
        id: CommandId::EditPriority,
        title: "Edit priority",
        keys: &[],
        help: "1\u{2013}4, or clear it",
    },
    Command {
        id: CommandId::EditTags,
        title: "Edit tags",
        keys: &[],
        help: "Semicolon separated",
    },
    Command {
        id: CommandId::EditAssignee,
        title: "Change assignee",
        keys: &[key('a')],
        help: "Pick who owns it",
    },
    Command {
        id: CommandId::EditIteration,
        title: "Change iteration",
        keys: &[],
        help: "Move it to a sprint",
    },
    Command {
        id: CommandId::EditArea,
        title: "Change area",
        keys: &[],
        help: "Move it in the area tree",
    },
    Command {
        id: CommandId::EditDescription,
        title: "Edit description",
        keys: &[],
        help: "Opens your $EDITOR",
    },
    Command {
        id: CommandId::AddComment,
        title: "Add comment",
        keys: &[],
        help: "One line on the discussion",
    },
    Command {
        id: CommandId::UndoEdit,
        title: "Undo last edit",
        keys: &[key('u')],
        help: "Put the value back",
    },
    Command {
        id: CommandId::SaveView,
        title: "Save current view",
        keys: &[],
        help: "",
    },
    Command {
        id: CommandId::Sort,
        title: "Sort tickets",
        keys: &[key('s')],
        help: "Choose field and direction",
    },
    Command {
        id: CommandId::ToggleDensity,
        title: "Toggle row density",
        keys: &[key('c')],
        help: "",
    },
    Command {
        id: CommandId::ToggleDetails,
        title: "Toggle details pane",
        keys: &[key('d')],
        help: "Below 70 columns",
    },
    Command {
        id: CommandId::ToggleSearchOrder,
        title: "Toggle search order",
        keys: &[key('v')],
        help: "Relevance or fields",
    },
    Command {
        id: CommandId::ToggleFinished,
        // Reworded as it runs by `finished_title`, so the row always names the
        // change it makes rather than the state it is in.
        title: "Show finished tickets",
        keys: &[],
        help: "Done and Removed rows",
    },
    Command {
        id: CommandId::Open,
        title: "Open ticket in browser",
        keys: &[key('o')],
        help: "",
    },
    Command {
        id: CommandId::ToggleBookmark,
        title: "Toggle bookmark",
        keys: &[key('m')],
        help: "",
    },
    Command {
        id: CommandId::HistoryBack,
        title: "Back to previous ticket",
        keys: &[key('[')],
        help: "",
    },
    Command {
        id: CommandId::HistoryForward,
        title: "Forward to next ticket",
        keys: &[key(']')],
        help: "",
    },
    Command {
        id: CommandId::CopyId,
        title: "Copy ID",
        keys: &[key('y')],
        help: "Selected or current tickets",
    },
    Command {
        id: CommandId::CopyUrl,
        title: "Copy URL",
        keys: &[],
        help: "",
    },
    Command {
        id: CommandId::CopyTitle,
        title: "Copy title",
        keys: &[],
        help: "",
    },
    Command {
        id: CommandId::CopyMarkdown,
        title: "Copy Markdown link",
        keys: &[],
        help: "",
    },
    Command {
        id: CommandId::CopySummary,
        title: "Copy summary",
        keys: &[],
        help: "",
    },
    Command {
        id: CommandId::ExportJson,
        title: "Export selected as JSON",
        keys: &[],
        help: "",
    },
    Command {
        id: CommandId::ExportCsv,
        title: "Export selected as CSV",
        keys: &[],
        help: "",
    },
    Command {
        id: CommandId::SelectAll,
        title: "Select all visible",
        keys: &[],
        help: "",
    },
    Command {
        id: CommandId::ClearSelection,
        title: "Clear selection",
        keys: &[],
        help: "",
    },
    Command {
        id: CommandId::DatabaseInfo,
        title: "Database info",
        keys: &[key('i')],
        help: "Path, counts, freshness",
    },
    Command {
        id: CommandId::Sync,
        title: "Sync from Azure DevOps",
        keys: &[key('r')],
        help: "Pull work items now",
    },
    Command {
        id: CommandId::Help,
        title: "Help",
        keys: &[key('?')],
        help: "",
    },
    Command {
        id: CommandId::Palette,
        title: "Command palette",
        keys: &[key('p'), key(':')],
        help: "",
    },
    Command {
        id: CommandId::Quit,
        title: "Quit",
        keys: &[key('q'), ctrl('c')],
        help: "",
    },
    Command {
        id: CommandId::ResetPaneSplit,
        title: "Reset pane split",
        keys: &[],
        help: "Restore the 62/56 layout",
    },
];

/// One row of the Edit menu: the field it changes, and the command that opens
/// its editor. Every field editor is one row here, so adding an editor is
/// adding its command to [`COMMANDS`] and its field to this table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditMenuEntry {
    /// The field's name, as the menu lists it.
    pub label: &'static str,
    pub command: CommandId,
}

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
        label: "Description",
        command: CommandId::EditDescription,
    },
    EditMenuEntry {
        label: "Add comment",
        command: CommandId::AddComment,
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
pub fn command_for_key(event: KeyEvent) -> Option<CommandId> {
    let modifiers = normalized_modifiers(event);
    COMMANDS
        .iter()
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
fn worded(command: Command, finished_hidden: bool) -> Command {
    if command.id == CommandId::ToggleFinished {
        Command {
            title: finished_title(finished_hidden),
            ..command
        }
    } else {
        command
    }
}

#[must_use]
pub fn matching_commands(query: &str, finished_hidden: bool) -> Vec<Command> {
    let query = query.trim().to_ascii_lowercase();
    COMMANDS
        .iter()
        .copied()
        .filter(Command::in_palette)
        .map(|command| worded(command, finished_hidden))
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
        let matches = matching_commands("copy", true);
        assert!(
            matches
                .iter()
                .any(|command| command.id == CommandId::CopyId)
        );
        assert!(!matches.iter().any(|command| command.id == CommandId::Sync));
        assert!(
            !matching_commands("", true)
                .iter()
                .any(|command| command.id == CommandId::Search),
            "commands that only work from browse mode stay out of the palette"
        );
    }

    #[test]
    fn the_finished_toggle_is_worded_as_the_change_it_would_make() {
        let hidden = matching_commands("show finished", true);
        assert_eq!(
            hidden.first().map(|command| command.id),
            Some(CommandId::ToggleFinished),
            "while they are hidden the palette offers to show them"
        );
        assert_eq!(hidden[0].title, "Show finished tickets");

        let shown = matching_commands("hide finished", false);
        assert_eq!(
            shown.first().map(|command| command.id),
            Some(CommandId::ToggleFinished)
        );
        assert_eq!(shown[0].title, "Hide finished tickets");
        assert!(
            matching_commands("show finished", false).is_empty(),
            "a command is only found under the words it is showing"
        );
    }

    #[test]
    fn keys_resolve_to_their_command() {
        assert_eq!(
            command_for_key(press(KeyCode::Char('s'), KeyModifiers::NONE)),
            Some(CommandId::Sort)
        );
        assert_eq!(
            command_for_key(press(KeyCode::Char(':'), KeyModifiers::NONE)),
            Some(CommandId::Palette)
        );
        assert_eq!(
            command_for_key(press(KeyCode::Char('p'), KeyModifiers::NONE)),
            Some(CommandId::Palette)
        );
        assert_eq!(
            command_for_key(press(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(CommandId::Quit)
        );
        assert_eq!(
            command_for_key(press(KeyCode::Char('c'), KeyModifiers::NONE)),
            Some(CommandId::ToggleDensity)
        );
        assert_eq!(
            command_for_key(press(KeyCode::Char('V'), KeyModifiers::SHIFT)),
            Some(CommandId::Views)
        );
        assert_eq!(
            command_for_key(press(KeyCode::Char('S'), KeyModifiers::SHIFT)),
            Some(CommandId::ChangeState),
            "capital S is the state picker; lowercase s stays the sort menu"
        );
        assert_eq!(
            command_for_key(press(KeyCode::Char('e'), KeyModifiers::NONE)),
            Some(CommandId::EditMenu)
        );
        assert_eq!(
            command_for_key(press(KeyCode::Tab, KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn bound_commands_have_labels_and_unique_keys() {
        let mut bound: Vec<Key> = Vec::new();
        for command in COMMANDS {
            if command.keys.is_empty() {
                assert!(command.key_label().is_empty(), "{:?}", command.id);
                continue;
            }
            assert!(!command.key_label().is_empty(), "{:?}", command.id);
            for key in command.keys {
                assert!(!key.label().is_empty(), "{:?}", command.id);
                assert!(!bound.contains(key), "duplicate binding {}", key.label());
                bound.push(*key);
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
}
