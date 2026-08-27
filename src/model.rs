use std::cmp::Ordering;
use std::fmt;

use crate::timestamp::Timestamp;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TicketKey {
    pub organization: String,
    pub id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ticket {
    pub key: TicketKey,
    pub project: String,
    pub revision: i64,
    pub work_item_type: String,
    pub title: String,
    pub state: String,
    pub reason: Option<String>,
    pub assigned_to: Option<String>,
    pub priority: Option<i64>,
    pub area_path: String,
    pub iteration_path: String,
    pub tags: Vec<String>,
    pub description: String,
    pub created_at: Timestamp,
    pub changed_at: Timestamp,
    pub web_url: String,
}

impl Ticket {
    #[must_use]
    pub fn searchable_text(&self) -> String {
        format!(
            "{} {} {} {} {} {} {} {}",
            self.key.id,
            self.title,
            self.assigned_to.as_deref().unwrap_or_default(),
            self.state,
            self.work_item_type,
            self.area_path,
            self.iteration_path,
            self.tags.join(" ")
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortField {
    #[default]
    Changed,
    Priority,
    Id,
    Title,
    State,
    Type,
    Assignee,
    Organization,
    Project,
    Area,
    Iteration,
    Created,
    Tags,
}

impl SortField {
    pub const ALL: [Self; 13] = [
        Self::Changed,
        Self::Priority,
        Self::Id,
        Self::Title,
        Self::State,
        Self::Type,
        Self::Assignee,
        Self::Organization,
        Self::Project,
        Self::Area,
        Self::Iteration,
        Self::Created,
        Self::Tags,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Changed => "Changed",
            Self::Priority => "Priority",
            Self::Id => "ID",
            Self::Title => "Title",
            Self::State => "State",
            Self::Type => "Type",
            Self::Assignee => "Assignee",
            Self::Organization => "Org",
            Self::Project => "Project",
            Self::Area => "Area",
            Self::Iteration => "Iteration",
            Self::Created => "Created",
            Self::Tags => "Tags",
        }
    }

    #[must_use]
    pub const fn is_numeric(self) -> bool {
        matches!(self, Self::Priority)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelationKind {
    Parent,
    Child,
    Related,
    Predecessor,
    Successor,
    Duplicate,
}

impl RelationKind {
    pub const ALL: [Self; 6] = [
        Self::Parent,
        Self::Child,
        Self::Related,
        Self::Predecessor,
        Self::Successor,
        Self::Duplicate,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Parent => "parent",
            Self::Child => "child",
            Self::Related => "related",
            Self::Predecessor => "predecessor",
            Self::Successor => "successor",
            Self::Duplicate => "duplicate",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Parent => "Parent",
            Self::Child => "Child",
            Self::Related => "Related",
            Self::Predecessor => "Predecessor",
            Self::Successor => "Successor",
            Self::Duplicate => "Duplicate",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "parent" => Self::Parent,
            "child" => Self::Child,
            "related" | "relates" => Self::Related,
            "predecessor" | "predecessorof" => Self::Predecessor,
            "successor" | "successorof" => Self::Successor,
            "duplicate" | "duplicateof" => Self::Duplicate,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationRecord {
    pub from: TicketKey,
    pub to: TicketKey,
    pub kind: RelationKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommentRecord {
    pub ticket: TicketKey,
    pub comment_id: i64,
    pub created_at: Timestamp,
    pub author: Option<String>,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryRecord {
    pub ticket: TicketKey,
    pub revision: i64,
    pub changed_at: Timestamp,
    pub changed_by: Option<String>,
    pub field_name: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TicketGraph {
    pub relations: Vec<RelationRecord>,
    pub comments: Vec<CommentRecord>,
    pub history: Vec<HistoryRecord>,
}

impl TicketGraph {
    #[must_use]
    pub fn relations_from(&self, key: &TicketKey) -> Vec<&RelationRecord> {
        self.relations
            .iter()
            .filter(|relation| relation.from == *key)
            .collect()
    }

    #[must_use]
    pub fn comments_for(&self, key: &TicketKey) -> Vec<&CommentRecord> {
        let mut comments: Vec<_> = self
            .comments
            .iter()
            .filter(|comment| comment.ticket == *key)
            .collect();
        comments.sort_by_key(|left| left.created_at);
        comments
    }

    #[must_use]
    pub fn history_for(&self, key: &TicketKey) -> Vec<&HistoryRecord> {
        let mut history: Vec<_> = self
            .history
            .iter()
            .filter(|entry| entry.ticket == *key)
            .collect();
        history.sort_by(|left, right| {
            left.revision
                .cmp(&right.revision)
                .then_with(|| left.changed_at.cmp(&right.changed_at))
                .then_with(|| left.field_name.cmp(&right.field_name))
        });
        history
    }
}

impl fmt::Display for SortField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SearchOrder {
    #[default]
    Relevance,
    Field,
}

impl SearchOrder {
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Relevance => Self::Field,
            Self::Field => Self::Relevance,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Relevance => "Relevance",
            Self::Field => "Field",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RowDensity {
    #[default]
    Compact,
    Comfortable,
}

impl RowDensity {
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Compact => Self::Comfortable,
            Self::Comfortable => Self::Compact,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Comfortable => "Comfortable",
        }
    }

    #[must_use]
    pub const fn row_height(self) -> u16 {
        match self {
            Self::Compact => 1,
            Self::Comfortable => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortDirection {
    Ascending,
    #[default]
    Descending,
}

impl SortDirection {
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }

    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Ascending => "↑",
            Self::Descending => "↓",
        }
    }
}

#[must_use]
pub fn compare_tickets(
    left: &Ticket,
    right: &Ticket,
    field: SortField,
    direction: SortDirection,
) -> Ordering {
    let primary = match field {
        SortField::Changed => left.changed_at.cmp(&right.changed_at),
        SortField::Created => left.created_at.cmp(&right.created_at),
        SortField::Priority => compare_optional_last(left.priority, right.priority, direction),
        SortField::Id => left.key.id.cmp(&right.key.id),
        SortField::Title => compare_text(&left.title, &right.title),
        SortField::State => compare_text(&left.state, &right.state),
        SortField::Type => compare_text(&left.work_item_type, &right.work_item_type),
        SortField::Assignee => compare_optional_text_last(
            left.assigned_to.as_deref(),
            right.assigned_to.as_deref(),
            direction,
        ),
        SortField::Organization => compare_text(&left.key.organization, &right.key.organization),
        SortField::Project => compare_text(&left.project, &right.project),
        SortField::Area => compare_text(&left.area_path, &right.area_path),
        SortField::Iteration => compare_text(&left.iteration_path, &right.iteration_path),
        SortField::Tags => compare_text(&left.tags.join(";"), &right.tags.join(";")),
    };

    let directed = if matches!(field, SortField::Priority | SortField::Assignee) {
        primary
    } else {
        apply_direction(primary, direction)
    };

    directed
        .then_with(|| left.key.id.cmp(&right.key.id))
        .then_with(|| left.key.organization.cmp(&right.key.organization))
}

fn compare_text(left: &str, right: &str) -> Ordering {
    left.to_lowercase().cmp(&right.to_lowercase())
}

fn compare_optional_last<T: Ord>(
    left: Option<T>,
    right: Option<T>,
    direction: SortDirection,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => apply_direction(left.cmp(&right), direction),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_optional_text_last(
    left: Option<&str>,
    right: Option<&str>,
    direction: SortDirection,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => apply_direction(compare_text(left, right), direction),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn apply_direction(ordering: Ordering, direction: SortDirection) -> Ordering {
    match direction {
        SortDirection::Ascending => ordering,
        SortDirection::Descending => ordering.reverse(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timestamp::ts;

    fn ticket(id: i64, title: &str, priority: Option<i64>) -> Ticket {
        Ticket {
            key: TicketKey {
                organization: "demo-org".into(),
                id,
            },
            project: "demo".into(),
            revision: 1,
            work_item_type: "Task".into(),
            title: title.into(),
            state: "Active".into(),
            reason: None,
            assigned_to: None,
            priority,
            area_path: "demo".into(),
            iteration_path: "demo\\Sprint 1".into(),
            tags: vec!["rust".into()],
            description: "not searchable sentinel".into(),
            created_at: ts("2026-01-01T00:00:00Z"),
            changed_at: Timestamp::from_offset_date_time(
                time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(id),
            ),
            web_url: format!("https://dev.azure.com/demo/demo/_workitems/edit/{id}"),
        }
    }

    #[test]
    fn searchable_text_includes_core_fields_but_not_description() {
        let ticket = ticket(42, "Fix search", Some(1));
        let text = ticket.searchable_text();

        assert!(text.contains("42 Fix search"));
        assert!(text.contains("Active Task"));
        assert!(text.contains("Sprint 1"));
        assert!(text.contains("rust"));
        assert!(!text.contains("sentinel"));
    }

    #[test]
    fn priority_sorts_missing_values_last_in_both_directions() {
        let present = ticket(1, "Present", Some(2));
        let missing = ticket(2, "Missing", None);

        assert_eq!(
            compare_tickets(
                &present,
                &missing,
                SortField::Priority,
                SortDirection::Ascending
            ),
            Ordering::Less
        );
        assert_eq!(
            compare_tickets(
                &present,
                &missing,
                SortField::Priority,
                SortDirection::Descending
            ),
            Ordering::Less
        );
    }

    #[test]
    fn title_sort_is_case_insensitive_and_uses_id_as_tie_breaker() {
        let left = ticket(1, "alpha", Some(1));
        let right = ticket(2, "ALPHA", Some(1));

        assert_eq!(
            compare_tickets(&left, &right, SortField::Title, SortDirection::Descending),
            Ordering::Less
        );
    }

    #[test]
    fn changed_sort_uses_normalized_instants() {
        let mut earlier = ticket(1, "Earlier", Some(1));
        let mut later = ticket(2, "Later", Some(1));
        earlier.changed_at = ts("2026-08-26T16:00:00Z");
        later.changed_at = ts("2026-08-26T13:00:00-05:00");

        assert_eq!(
            compare_tickets(
                &later,
                &earlier,
                SortField::Changed,
                SortDirection::Descending
            ),
            Ordering::Less
        );
    }
}
