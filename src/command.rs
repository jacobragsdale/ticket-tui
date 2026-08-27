#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandId {
    Palette,
    Filters,
    Columns,
    Views,
    Sort,
    Help,
    Reload,
    Open,
    ToggleDensity,
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
    ImportJson,
    ImportCsv,
    DatabaseInfo,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Command {
    pub id: CommandId,
    pub title: &'static str,
    pub hint: &'static str,
}

pub const COMMANDS: [Command; 27] = [
    Command {
        id: CommandId::Filters,
        title: "Open filter bar",
        hint: "f",
    },
    Command {
        id: CommandId::Columns,
        title: "Configure columns",
        hint: "w",
    },
    Command {
        id: CommandId::Views,
        title: "Named views",
        hint: "V",
    },
    Command {
        id: CommandId::SaveView,
        title: "Save current view",
        hint: "",
    },
    Command {
        id: CommandId::Sort,
        title: "Sort tickets",
        hint: "s",
    },
    Command {
        id: CommandId::ToggleDensity,
        title: "Toggle row density",
        hint: "c",
    },
    Command {
        id: CommandId::ToggleSearchOrder,
        title: "Toggle search order",
        hint: "v",
    },
    Command {
        id: CommandId::Open,
        title: "Open ticket in browser",
        hint: "Enter",
    },
    Command {
        id: CommandId::ToggleBookmark,
        title: "Toggle bookmark",
        hint: "m",
    },
    Command {
        id: CommandId::HistoryBack,
        title: "Back to previous ticket",
        hint: "[",
    },
    Command {
        id: CommandId::HistoryForward,
        title: "Forward to next ticket",
        hint: "]",
    },
    Command {
        id: CommandId::CopyId,
        title: "Copy ID",
        hint: "y",
    },
    Command {
        id: CommandId::CopyUrl,
        title: "Copy URL",
        hint: "",
    },
    Command {
        id: CommandId::CopyTitle,
        title: "Copy title",
        hint: "",
    },
    Command {
        id: CommandId::CopyMarkdown,
        title: "Copy Markdown link",
        hint: "",
    },
    Command {
        id: CommandId::CopySummary,
        title: "Copy summary",
        hint: "",
    },
    Command {
        id: CommandId::ExportJson,
        title: "Export selected as JSON",
        hint: "",
    },
    Command {
        id: CommandId::ExportCsv,
        title: "Export selected as CSV",
        hint: "",
    },
    Command {
        id: CommandId::SelectAll,
        title: "Select all visible",
        hint: "",
    },
    Command {
        id: CommandId::ClearSelection,
        title: "Clear selection",
        hint: "",
    },
    Command {
        id: CommandId::ImportJson,
        title: "Import JSON file",
        hint: "",
    },
    Command {
        id: CommandId::ImportCsv,
        title: "Import CSV file",
        hint: "",
    },
    Command {
        id: CommandId::DatabaseInfo,
        title: "Database info",
        hint: "i",
    },
    Command {
        id: CommandId::Reload,
        title: "Reload tickets",
        hint: "r",
    },
    Command {
        id: CommandId::Help,
        title: "Help",
        hint: "?",
    },
    Command {
        id: CommandId::Palette,
        title: "Command palette",
        hint: "p",
    },
    Command {
        id: CommandId::Quit,
        title: "Quit",
        hint: "q",
    },
];

#[must_use]
pub fn matching_commands(query: &str) -> Vec<Command> {
    let query = query.trim().to_ascii_lowercase();
    COMMANDS
        .into_iter()
        .filter(|command| {
            query.is_empty()
                || command.title.to_ascii_lowercase().contains(&query)
                || command.hint.to_ascii_lowercase().contains(&query)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_filters_commands_by_title() {
        let matches = matching_commands("copy");
        assert!(
            matches
                .iter()
                .any(|command| command.id == CommandId::CopyId)
        );
        assert!(
            !matches
                .iter()
                .any(|command| command.id == CommandId::Reload)
        );
    }
}
