//! The JSON context file agents read.

use super::*;
use crate::agent_context::ArtifactContext;
use crate::model::ArtifactKind;

impl WorkItemsScreen {
    /// What the shell knows whichever tab is showing: where the rows come
    /// from, how the last pull went, and what is still in flight.
    #[must_use]
    pub fn sync_context(&self, shell: &Shell) -> SyncContext {
        SyncContext {
            organization: shell
                .sync_target
                .as_ref()
                .map(|target| target.organization.clone()),
            project: shell
                .sync_target
                .as_ref()
                .map(|target| target.project.clone()),
            refresh_seconds: shell
                .sync_target
                .as_ref()
                .map_or(0, |target| target.refresh_seconds),
            in_progress: shell.sync_pending,
            last_success_at: shell.synced_wall_clock.map(Timestamp::to_rfc3339),
            last_error: shell.sync_error.clone(),
            offline: !shell.sync_enabled,
        }
    }

    /// This tab's slice of the context file.
    #[must_use]
    pub fn agent_context(&self, shell: &Shell) -> WorkItemsContext {
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
        WorkItemsContext {
            // Where `g` goes from here is `App`'s to work out.
            follow: None,
            mode: mode_name(self.mode).into(),
            focus: focus_name(shell.focus).into(),
            screen: if shell.narrow_details {
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
            selected_ticket: self.selected_ticket().map(|ticket| TicketContext {
                related: self.artifact_contexts(&ticket.key, shell),
                ..self.ticket_context(ticket)
            }),
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
    pub(crate) fn pending_edit_contexts(&self) -> Vec<PendingEditContext> {
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
            related: Vec::new(),
        }
    }

    /// One work item's artifact links, saying for each whether this database
    /// holds the thing it points at.
    fn artifact_contexts(&self, key: &TicketKey, shell: &Shell) -> Vec<ArtifactContext> {
        self.artifacts_for(key)
            .into_iter()
            .map(|artifact| ArtifactContext {
                kind: artifact.kind.as_str().to_owned(),
                name: artifact.name.clone(),
                repo: artifact.kind.repo_id().map(|id| shell.repo_name(id)),
                target: artifact.kind.target(),
                in_database: match &artifact.kind {
                    ArtifactKind::PullRequest { id, .. } => shell.pull_request_label(*id).is_some(),
                    ArtifactKind::Build(id) => shell.run_label(*id).is_some(),
                    // Nothing in this app shows a commit.
                    ArtifactKind::Commit { .. } => false,
                },
            })
            .collect()
    }
}
