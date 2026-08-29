//! The JSON context file agents read.

use super::*;

impl App {
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
                finished_hidden: self.finished_hidden(),
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
}
