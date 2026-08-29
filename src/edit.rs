//! Write-through edits: one field change, on its way to Azure DevOps and back.
//!
//! Every field the TUI can change travels this way. A [`FieldEdit`] names the
//! Azure DevOps field, carries the value to write and the label a notification
//! says out loud, and knows how to apply itself to a work item held in memory
//! so a row can change before the network answers. An [`EditRequest`] pairs one
//! of those with the work item and the revision it was read at, and turns into
//! the JSON Patch document the work-item endpoint accepts.

use serde_json::{Value, json};

use crate::model::{RelationRecord, Ticket, TicketKey};

/// Azure DevOps reference names for the fields the TUI writes.
pub const STATE_FIELD: &str = "System.State";
pub const TITLE_FIELD: &str = "System.Title";
pub const ASSIGNED_TO_FIELD: &str = "System.AssignedTo";
pub const TAGS_FIELD: &str = "System.Tags";
pub const PRIORITY_FIELD: &str = "Microsoft.VSTS.Common.Priority";

/// One JSON Patch operation setting a work-item field. Azure DevOps takes `add`
/// for a field that is already present as well as for one that is not.
#[must_use]
pub fn set_field(field: &str, value: impl Into<Value>) -> Value {
    json!({
        "op": "add",
        "path": format!("/fields/{field}"),
        "value": value.into(),
    })
}

/// One JSON Patch operation taking a field off a work item, so it has no value
/// at all rather than an empty one. This is how a priority goes back to unset;
/// a field Azure DevOps accepts an empty string for, such as `System.Tags`, is
/// better written with [`set_field`].
#[must_use]
pub fn remove_field(field: &str) -> Value {
    json!({
        "op": "remove",
        "path": format!("/fields/{field}"),
    })
}

/// The operation that makes a write refuse to land on a work item somebody else
/// changed after we read it: Azure DevOps rejects the whole document when the
/// revision no longer matches.
#[must_use]
pub fn revision_test(expected_revision: i64) -> Value {
    json!({"op": "test", "path": "/rev", "value": expected_revision})
}

/// What an edit writes: either a value, or nothing at all. A cleared field is
/// taken off the work item rather than set to an empty value, which is the only
/// way a number such as the priority goes back to unset.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Set(Value),
    Clear,
}

impl FieldValue {
    /// The value as text, for the fields the TUI models as strings. A cleared
    /// field reads as the empty string, which is what removing it leaves.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Set(value) => value.as_str(),
            Self::Clear => Some(""),
        }
    }

    #[must_use]
    pub const fn is_clear(&self) -> bool {
        matches!(self, Self::Clear)
    }
}

/// One field change, with everything needed to write it, undo it, and talk
/// about it.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldEdit {
    field: String,
    label: String,
    value: FieldValue,
}

impl FieldEdit {
    /// `field` is an Azure DevOps reference name such as `System.State`, and
    /// `label` is what a notification calls it, such as `State`.
    pub fn new(
        field: impl Into<String>,
        label: impl Into<String>,
        value: impl Into<Value>,
    ) -> Self {
        Self {
            field: field.into(),
            label: label.into(),
            value: FieldValue::Set(value.into()),
        }
    }

    /// An edit that takes the field off the work item entirely.
    #[must_use]
    pub fn clearing(field: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            label: label.into(),
            value: FieldValue::Clear,
        }
    }

    /// Move a work item to another state, the edit the state picker makes.
    #[must_use]
    pub fn state(state: &str) -> Self {
        Self::new(STATE_FIELD, "State", state)
    }

    /// Rename a work item. The title prompt trims before it gets here, and
    /// refuses an empty one rather than sending it.
    #[must_use]
    pub fn title(title: &str) -> Self {
        Self::new(TITLE_FIELD, "Title", title)
    }

    /// Set the priority to one of the values Azure DevOps offers.
    #[must_use]
    pub fn priority(priority: i64) -> Self {
        Self::new(PRIORITY_FIELD, "Priority", priority)
    }

    /// Put the priority back to unset, which needs the field removed: there is
    /// no empty number.
    #[must_use]
    pub fn clear_priority() -> Self {
        Self::clearing(PRIORITY_FIELD, "Priority")
    }

    /// Replace the tag list. `tags` is the normalised `a; b; c` text
    /// [`normalize_tags`] produces; an empty one clears the tags, which
    /// `System.Tags` accepts as an empty string.
    #[must_use]
    pub fn tags(tags: &str) -> Self {
        Self::new(TAGS_FIELD, "Tags", tags)
    }

    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub const fn value(&self) -> &FieldValue {
        &self.value
    }

    /// The value as a notification shows it.
    #[must_use]
    pub fn value_text(&self) -> String {
        match &self.value {
            FieldValue::Clear | FieldValue::Set(Value::Null) => "(none)".to_owned(),
            FieldValue::Set(Value::String(text)) if text.trim().is_empty() => "(none)".to_owned(),
            FieldValue::Set(Value::String(text)) => text.clone(),
            FieldValue::Set(other) => other.to_string(),
        }
    }

    /// What a status line says about the change, such as `State → Doing`.
    #[must_use]
    pub fn summary(&self) -> String {
        format!("{} → {}", self.label, self.value_text())
    }

    /// The operations that write this edit, without the revision test.
    #[must_use]
    pub fn patch(&self) -> Vec<Value> {
        match &self.value {
            FieldValue::Set(value) => vec![set_field(&self.field, value.clone())],
            FieldValue::Clear => vec![remove_field(&self.field)],
        }
    }

    /// Applies the change to a work item held in memory, so the row can show it
    /// before Azure DevOps answers. A field outside the list below is left to
    /// the copy the server sends back once the write lands.
    pub fn apply(&self, ticket: &mut Ticket) {
        let text = self.value.as_str().map(str::trim);
        match self.field.as_str() {
            // A state or a title is never cleared, so a clear leaves it alone.
            STATE_FIELD if !self.value.is_clear() => ticket.state = self.value_string(),
            TITLE_FIELD if !self.value.is_clear() => ticket.title = self.value_string(),
            ASSIGNED_TO_FIELD => {
                ticket.assigned_to = text.filter(|name| !name.is_empty()).map(str::to_owned);
            }
            TAGS_FIELD => {
                ticket.tags = text
                    .unwrap_or_default()
                    .split(';')
                    .map(str::trim)
                    .filter(|tag| !tag.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            PRIORITY_FIELD => {
                ticket.priority = match &self.value {
                    FieldValue::Clear => None,
                    FieldValue::Set(value) => value
                        .as_i64()
                        .or_else(|| text.and_then(|raw| raw.parse().ok())),
                };
            }
            _ => {}
        }
    }

    fn value_string(&self) -> String {
        match &self.value {
            FieldValue::Clear => String::new(),
            FieldValue::Set(value) => value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned),
        }
    }
}

/// Tidies a semicolon separated tag list the way the tags prompt saves it:
/// split on `;`, trim each tag, drop the empty ones, drop a later repeat of a
/// tag already listed whatever its case, and join what is left with `; `.
/// An empty result clears the tags.
#[must_use]
pub fn normalize_tags(raw: &str) -> String {
    let mut seen: Vec<String> = Vec::new();
    let mut tags: Vec<&str> = Vec::new();
    for tag in raw.split(';').map(str::trim).filter(|tag| !tag.is_empty()) {
        let folded = tag.to_lowercase();
        if seen.contains(&folded) {
            continue;
        }
        seen.push(folded);
        tags.push(tag);
    }
    tags.join("; ")
}

/// One field edit on its way to Azure DevOps.
#[derive(Clone, Debug, PartialEq)]
pub struct EditRequest {
    pub key: TicketKey,
    /// The revision the row carried when the edit was made. Azure DevOps
    /// refuses the write if the work item has moved past it.
    pub expected_revision: i64,
    pub edit: FieldEdit,
}

impl EditRequest {
    /// The JSON Patch document to send: the revision test first, so nothing is
    /// written to a work item that changed under us.
    #[must_use]
    pub fn document(&self) -> Vec<Value> {
        let mut document = Vec::with_capacity(2);
        document.push(revision_test(self.expected_revision));
        document.extend(self.edit.patch());
        document
    }
}

/// A work item as Azure DevOps returned it after accepting an edit, already
/// written to SQLite.
#[derive(Clone, Debug)]
pub struct EditApplied {
    pub ticket: Ticket,
    /// The work item's outgoing links, which replace the ones held for it.
    pub relations: Vec<RelationRecord>,
    pub edit: FieldEdit,
}

/// An edit that did not land, so the row goes back to what it was.
#[derive(Clone, Debug)]
pub struct EditRejection {
    pub key: TicketKey,
    /// What the field is called in a notification, such as `State`.
    pub label: String,
    /// Whether the work item changed under us, which a fresh pull fixes.
    pub conflict: bool,
    pub message: String,
}

impl EditRejection {
    /// The error notification: a conflict says the work item moved on and that
    /// a pull is on its way; anything else reports what Azure DevOps said. Both
    /// name the field, so a change is never dropped quietly.
    #[must_use]
    pub fn notification(&self) -> String {
        let id = self.key.id;
        if self.conflict {
            format!(
                "#{id} changed in Azure DevOps since it was loaded; {} not saved — syncing the latest copy",
                self.label
            )
        } else {
            format!("#{id} {} not saved: {}", self.label, self.message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timestamp::ts;

    fn ticket() -> Ticket {
        Ticket {
            key: TicketKey {
                organization: "demo".into(),
                id: 613,
            },
            project: "atlas".into(),
            revision: 4,
            work_item_type: "Task".into(),
            title: "Edit dispatcher".into(),
            state: "To Do".into(),
            reason: None,
            assigned_to: Some("Avery Chen".into()),
            priority: Some(2),
            area_path: "Atlas".into(),
            iteration_path: "Atlas\\Sprint 1".into(),
            tags: vec!["rust".into()],
            description: String::new(),
            created_at: ts("2026-01-01T00:00:00Z"),
            changed_at: ts("2026-02-01T00:00:00Z"),
            web_url: "https://dev.azure.com/demo/atlas/_workitems/edit/613".into(),
            details_rev: 0,
        }
    }

    #[test]
    fn a_request_sends_the_revision_test_before_the_field_it_writes() {
        let request = EditRequest {
            key: ticket().key,
            expected_revision: 4,
            edit: FieldEdit::state("Doing"),
        };

        assert_eq!(
            request.document(),
            vec![
                json!({"op": "test", "path": "/rev", "value": 4}),
                json!({"op": "add", "path": "/fields/System.State", "value": "Doing"}),
            ]
        );
        assert_eq!(request.edit.summary(), "State → Doing");
        assert_eq!(request.edit.field(), STATE_FIELD);
    }

    #[test]
    fn an_edit_changes_the_field_it_names_and_leaves_the_rest_of_the_row_alone() {
        let mut edited = ticket();
        FieldEdit::state("Doing").apply(&mut edited);
        assert_eq!(edited.state, "Doing");
        assert_eq!(edited.title, ticket().title);
        assert_eq!(
            edited.revision,
            ticket().revision,
            "the server owns the rev"
        );

        FieldEdit::new(TITLE_FIELD, "Title", "Renamed").apply(&mut edited);
        assert_eq!(edited.title, "Renamed");

        FieldEdit::new(ASSIGNED_TO_FIELD, "Assignee", "Jordan Patel").apply(&mut edited);
        assert_eq!(edited.assigned_to.as_deref(), Some("Jordan Patel"));
        FieldEdit::new(ASSIGNED_TO_FIELD, "Assignee", "").apply(&mut edited);
        assert_eq!(edited.assigned_to, None, "an empty identity unassigns");

        FieldEdit::new(TAGS_FIELD, "Tags", "rust; azure ;").apply(&mut edited);
        assert_eq!(edited.tags, ["rust", "azure"]);

        FieldEdit::new(PRIORITY_FIELD, "Priority", 1).apply(&mut edited);
        assert_eq!(edited.priority, Some(1));
        FieldEdit::new(PRIORITY_FIELD, "Priority", "3").apply(&mut edited);
        assert_eq!(edited.priority, Some(3), "Azure DevOps quotes some numbers");

        let unknown = FieldEdit::new("System.Description", "Description", "Later");
        let before = edited.clone();
        unknown.apply(&mut edited);
        assert_eq!(
            edited, before,
            "a field the row does not model waits for the server copy"
        );
        assert_eq!(unknown.summary(), "Description → Later");
    }

    #[test]
    fn a_rejection_names_the_field_and_a_conflict_says_a_pull_is_coming() {
        let rejection = EditRejection {
            key: ticket().key,
            label: "State".into(),
            conflict: true,
            message: "the test operation failed".into(),
        };
        let message = rejection.notification();
        assert!(
            message.starts_with("#613 changed in Azure DevOps"),
            "{message}"
        );
        assert!(message.contains("State not saved"), "{message}");
        assert!(message.contains("syncing the latest copy"), "{message}");

        let rejection = EditRejection {
            conflict: false,
            message: "HTTP 400: field is read only".into(),
            ..rejection
        };
        assert_eq!(
            rejection.notification(),
            "#613 State not saved: HTTP 400: field is read only"
        );
    }

    #[test]
    fn a_cleared_field_is_removed_rather_than_emptied() {
        let edit = FieldEdit::clear_priority();
        assert_eq!(edit.value(), &FieldValue::Clear);
        assert_eq!(
            edit.patch(),
            vec![json!({"op": "remove", "path": "/fields/Microsoft.VSTS.Common.Priority"})]
        );
        assert_eq!(edit.summary(), "Priority → (none)");

        let mut cleared = ticket();
        edit.apply(&mut cleared);
        assert_eq!(cleared.priority, None);
        assert_eq!(cleared.title, ticket().title, "only the field it names");

        FieldEdit::priority(3).apply(&mut cleared);
        assert_eq!(cleared.priority, Some(3));
    }

    #[test]
    fn a_tag_list_is_trimmed_deduplicated_and_rejoined() {
        assert_eq!(normalize_tags("rust; Rust ;; tui"), "rust; tui");
        assert_eq!(normalize_tags("  "), "");
        assert_eq!(normalize_tags(";;"), "");
        assert_eq!(
            normalize_tags("Azure DevOps ; azure devops ; cli"),
            "Azure DevOps; cli",
            "the first spelling of a tag is the one kept"
        );

        let mut tagged = ticket();
        FieldEdit::tags(&normalize_tags("rust; Rust ;; tui")).apply(&mut tagged);
        assert_eq!(tagged.tags, ["rust", "tui"]);
        FieldEdit::tags("").apply(&mut tagged);
        assert!(tagged.tags.is_empty(), "an empty list clears the tags");
        assert_eq!(
            FieldEdit::tags("").patch(),
            vec![json!({"op": "add", "path": "/fields/System.Tags", "value": ""})],
            "System.Tags takes an empty string rather than a remove"
        );
    }

    #[test]
    fn a_blank_value_reads_as_none_in_a_notification() {
        assert_eq!(
            FieldEdit::new(ASSIGNED_TO_FIELD, "Assignee", "").summary(),
            "Assignee → (none)"
        );
        assert_eq!(
            FieldEdit::new(PRIORITY_FIELD, "Priority", 2).summary(),
            "Priority → 2"
        );
    }
}
