//! Bookmarks, the checked set, copy and export, the visit history and the
//! session file.

use super::*;
use crate::columns::ColumnId;

impl WorkItemsScreen {
    #[must_use]
    pub fn is_bookmarked(&self, key: &TicketKey) -> bool {
        self.bookmarks.contains(key)
    }

    #[must_use]
    pub fn is_row_selected(&self, key: &TicketKey) -> bool {
        self.selected_keys.contains(key)
    }

    pub(super) fn open_copy_actions(&mut self, shell: &mut Shell) {
        self.run_command(shell, CommandId::Palette);
        self.palette.query = TextInput::new("copy");
    }

    pub(super) fn toggle_bookmark(&mut self, shell: &mut Shell) {
        let Some(key) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            return;
        };
        if self.bookmarks.remove(&key) {
            shell.set_status(format!("Removed bookmark {}", key.id));
        } else {
            self.bookmarks.insert(key.clone());
            shell.set_status(format!("Bookmarked {}", key.id));
        }
        shell.session_dirty = true;
        if self.parsed_query().filters.bookmarked {
            let selected = Some(key);
            if self.fuzzy_query().is_empty() {
                self.show_all(shell, selected.as_ref());
            } else {
                self.pending_selection = selected;
                self.submit_search();
            }
        }
    }

    pub(super) fn toggle_row_selection(&mut self) {
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

    pub(super) fn copy_with(
        &self,
        content: CopiedContent,
        formatter: fn(&[&Ticket]) -> String,
    ) -> AppAction {
        let tickets = self.export_targets();
        if tickets.is_empty() {
            return AppAction::None;
        }
        AppAction::Copy {
            text: formatter(&tickets),
            content,
        }
    }

    pub(super) fn export_with(
        &self,
        extension: &str,
        formatter: fn(&[&Ticket]) -> String,
    ) -> AppAction {
        let tickets = self.export_targets();
        if tickets.is_empty() {
            return AppAction::None;
        }
        AppAction::WriteFile {
            path: PathBuf::from(format!("ticket-tui-export.{extension}")),
            contents: formatter(&tickets),
        }
    }

    /// Records the row under the cursor as somewhere this run has been.
    pub fn record_visit(&mut self, shell: &mut Shell) {
        self.record_history(shell);
    }

    pub(super) fn record_history(&mut self, shell: &mut Shell) {
        let Some(key) = self.selected_ticket().map(|ticket| ticket.key.clone()) else {
            return;
        };
        shell.record_jump(Jump::WorkItem(key));
    }

    /// The bookmarks, for the shell to save.
    #[must_use]
    pub fn bookmark_keys(&self) -> Vec<TicketKey> {
        self.bookmarks.iter().cloned().collect()
    }

    /// What the shell keeps for this screen, put back before its own slice is:
    /// the stale threshold and whether finished work is on the table have to
    /// be in place before the rows are worked out.
    pub fn restore_shared(&mut self, stale_days: u16, show_finished: bool, session: &Session) {
        self.stale_days = clamp_stale_days(stale_days);
        self.show_finished = show_finished;
        self.bookmarks = session.bookmarks.iter().cloned().collect();
    }

    /// This screen's slice of the session file. What the shell keeps —
    /// bookmarks, the visit history, the pane splits — is [`App`]'s to save.
    pub fn snapshot(&self) -> TabSession {
        TabSession {
            query: self.query.text().to_owned(),
            sort_field: self.sort_field.key().to_owned(),
            sort_direction: self.sort_direction,
            search_order: self.search_order,
            row_density: self.row_density,
            columns: self.layout.to_session_columns(),
            auto_hide: Some(self.layout.auto_hide),
            views: self.views.clone(),
            active_view: self.active_view.clone(),
        }
    }

    /// The same, coming back. `selected` is the row to settle on once the
    /// query has been applied, and belongs to the shell's half of the file.
    pub fn restore(&mut self, shell: &mut Shell, session: TabSession, selected: Option<TicketKey>) {
        self.sort_field = SortField::from_key(&session.sort_field).unwrap_or_default();
        self.sort_direction = session.sort_direction;
        self.search_order = session.search_order;
        self.row_density = session.row_density;
        self.layout = TableLayout::from_session_columns(&session.columns, session.auto_hide);
        self.views = session
            .views
            .into_iter()
            .filter(|view| builtin_named(&view.name).is_none())
            .collect();
        self.active_view = session.active_view;
        if session.query.is_empty() {
            self.show_all(shell, selected.as_ref());
        } else {
            self.set_query(shell, session.query);
            if let Some(selected) = selected {
                self.pending_selection = Some(selected);
            }
        }
    }
}
