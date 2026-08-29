//! Bookmarks, the checked set, copy and export, the visit history and the
//! session file.

use super::*;

impl App {
    #[must_use]
    pub fn is_bookmarked(&self, key: &TicketKey) -> bool {
        self.bookmarks.contains(key)
    }

    #[must_use]
    pub fn is_row_selected(&self, key: &TicketKey) -> bool {
        self.selected_keys.contains(key)
    }

    pub(super) fn open_copy_actions(&mut self) {
        self.run_command(CommandId::Palette);
        self.palette.query = TextInput::new("copy");
    }

    pub(super) fn toggle_bookmark(&mut self) {
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

    pub(super) fn record_history(&mut self) {
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

    pub(super) fn history_back(&mut self) {
        if self.recent.len() < 2 {
            return;
        }
        let current = self.recent.pop().expect("recent ticket exists");
        self.future.push(current);
        let key = self.recent.last().cloned();
        self.restore_selection(key.as_ref());
        self.session_dirty = true;
    }

    pub(super) fn history_forward(&mut self) {
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
            bookmarks: self.bookmarks.iter().cloned().collect(),
            recent: self.recent.clone(),
            views: self.views.clone(),
            active_view: self.active_view.clone(),
            show_finished: self.show_finished,
            selected: self.selected_ticket().map(|ticket| ticket.key.clone()),
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
        self.bookmarks = session.bookmarks.into_iter().collect();
        self.recent = session.recent;
        self.views = session
            .views
            .into_iter()
            .filter(|view| builtin_named(&view.name).is_none())
            .collect();
        self.active_view = session.active_view;
        // Set before the rows are worked out, so the first pass over them
        // already knows whether finished work belongs on the table.
        self.show_finished = session.show_finished;
        let selected = session.selected;
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
