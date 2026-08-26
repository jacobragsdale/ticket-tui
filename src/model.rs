use std::cmp::Ordering;
use std::fmt;

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
    pub created_at: String,
    pub changed_at: String,
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
}

impl SortField {
    pub const ALL: [Self; 7] = [
        Self::Changed,
        Self::Priority,
        Self::Id,
        Self::Title,
        Self::State,
        Self::Type,
        Self::Assignee,
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
        }
    }
}

impl fmt::Display for SortField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
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
            created_at: "2026-01-01T00:00:00Z".into(),
            changed_at: format!("2026-01-{id:02}T00:00:00Z"),
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
}
