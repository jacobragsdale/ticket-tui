use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandId {
    Search,
    Palette,
    Filters,
    MoreFilters,
    Columns,
    Views,
    Sort,
    Help,
    Sync,
    Open,
    ToggleDensity,
    ToggleDetails,
    ToggleSearchOrder,
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

#[must_use]
pub fn matching_commands(query: &str) -> Vec<Command> {
    let query = query.trim().to_ascii_lowercase();
    COMMANDS
        .iter()
        .copied()
        .filter(Command::in_palette)
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
        let matches = matching_commands("copy");
        assert!(
            matches
                .iter()
                .any(|command| command.id == CommandId::CopyId)
        );
        assert!(!matches.iter().any(|command| command.id == CommandId::Sync));
        assert!(
            !matching_commands("")
                .iter()
                .any(|command| command.id == CommandId::Search),
            "commands that only work from browse mode stay out of the palette"
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
            None
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
