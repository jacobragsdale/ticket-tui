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
pub const ITERATION_PATH_FIELD: &str = "System.IterationPath";
pub const AREA_PATH_FIELD: &str = "System.AreaPath";
pub const DESCRIPTION_FIELD: &str = "System.Description";

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
    /// What the row and the notification read while the write is in flight,
    /// when that is not the value sent. An assignee is written by unique name —
    /// the sign-in address Azure DevOps resolves — but the cell says the
    /// display name, which is what the server copy comes back carrying.
    shown: Option<String>,
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
            shown: None,
        }
    }

    /// An edit that takes the field off the work item entirely.
    #[must_use]
    pub fn clearing(field: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            label: label.into(),
            value: FieldValue::Clear,
            shown: None,
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

    /// Assign a work item to somebody. Azure DevOps resolves either spelling of
    /// a person, so the write goes out as the unique name when the picker knows
    /// one and as the display name when it does not; the cell reads as the
    /// display name whichever was sent.
    #[must_use]
    pub fn assignee(display_name: &str, unique_name: Option<&str>) -> Self {
        Self {
            shown: Some(display_name.to_owned()),
            ..Self::new(
                ASSIGNED_TO_FIELD,
                "Assignee",
                unique_name.unwrap_or(display_name),
            )
        }
    }

    /// Move a work item to another iteration. `path` is the full backslash
    /// path the field holds — `development\Sprint 1` — not the leaf the
    /// table column shows.
    #[must_use]
    pub fn iteration(path: &str) -> Self {
        Self::new(ITERATION_PATH_FIELD, "Iteration", path)
    }

    /// Move a work item to another area, by the same full backslash path.
    #[must_use]
    pub fn area(path: &str) -> Self {
        Self::new(AREA_PATH_FIELD, "Area", path)
    }

    /// Rewrite the description, which is the one field written as HTML rather
    /// than as a value. A notification cannot say a whole document out loud, so
    /// it says the field changed and leaves the reading of it to the details
    /// pane; an empty document clears the field.
    #[must_use]
    pub fn description(html: &str) -> Self {
        Self {
            shown: Some(if html.trim().is_empty() {
                String::new()
            } else {
                "updated".to_owned()
            }),
            ..Self::new(DESCRIPTION_FIELD, "Description", html)
        }
    }

    /// Take a work item off whoever holds it. `System.AssignedTo` goes back to
    /// nobody by being removed, not by being set to an empty identity.
    #[must_use]
    pub fn unassign() -> Self {
        Self::clearing(ASSIGNED_TO_FIELD, "Assignee")
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

    /// The text the row and the notification read, which is the value sent
    /// unless the edit carries a friendlier spelling of it.
    fn shown_text(&self) -> Option<&str> {
        self.shown.as_deref().or_else(|| self.value.as_str())
    }

    /// The value as a notification shows it.
    #[must_use]
    pub fn value_text(&self) -> String {
        if let Some(shown) = self.shown.as_deref() {
            return if shown.trim().is_empty() {
                "(none)".to_owned()
            } else {
                shown.to_owned()
            };
        }
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
        let text = self.shown_text().map(str::trim);
        match self.field.as_str() {
            // A state or a title is never cleared, so a clear leaves it alone.
            STATE_FIELD if !self.value.is_clear() => ticket.state = self.value_string(),
            TITLE_FIELD if !self.value.is_clear() => ticket.title = self.value_string(),
            ASSIGNED_TO_FIELD => {
                ticket.assigned_to = text.filter(|name| !name.is_empty()).map(str::to_owned);
            }
            // A work item always sits somewhere in both trees, so neither is
            // ever cleared; a clear leaves the path alone.
            ITERATION_PATH_FIELD if !self.value.is_clear() => {
                ticket.iteration_path = self.value_string();
            }
            AREA_PATH_FIELD if !self.value.is_clear() => ticket.area_path = self.value_string(),
            // The description is held twice: as Azure DevOps stores it, and as
            // the details pane draws it. An edit writes the markup and renders
            // the reading of it, so the pane changes with the row rather than
            // waiting for the copy the server sends back.
            DESCRIPTION_FIELD => {
                let html = self.value_string();
                ticket.description = crate::html::html_to_text(&html);
                ticket.description_html = html;
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

    /// The same refusal as one line of a bulk change's summary, where the field
    /// is already named once for the whole change and several of these have to
    /// fit in one notification.
    #[must_use]
    pub fn failure(&self) -> String {
        let id = self.key.id;
        if self.conflict {
            format!("#{id} failed: it changed in Azure DevOps")
        } else {
            format!("#{id} failed: {}", self.message)
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
            description_html: String::new(),
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

        let unknown = FieldEdit::new("System.History", "History", "Later");
        let before = edited.clone();
        unknown.apply(&mut edited);
        assert_eq!(
            edited, before,
            "a field the row does not model waits for the server copy"
        );
        assert_eq!(unknown.summary(), "History → Later");
    }

    #[test]
    fn a_description_writes_the_markup_and_shows_the_reading_of_it() {
        let edit = FieldEdit::description("<p>Hand it to <code>$EDITOR</code>.</p>");
        assert_eq!(
            edit.patch(),
            vec![json!({
                "op": "add",
                "path": "/fields/System.Description",
                "value": "<p>Hand it to <code>$EDITOR</code>.</p>",
            })],
            "Azure DevOps is handed the document it stores"
        );
        assert_eq!(
            edit.summary(),
            "Description → updated",
            "a notification cannot say a whole document out loud"
        );

        let mut described = ticket();
        edit.apply(&mut described);
        assert_eq!(
            described.description_html,
            "<p>Hand it to <code>$EDITOR</code>.</p>"
        );
        assert_eq!(
            described.description, "Hand it to `$EDITOR`.",
            "the details pane reads the new description at once"
        );
        assert_eq!(described.title, ticket().title, "only the field it names");

        let cleared = FieldEdit::description("");
        assert_eq!(cleared.summary(), "Description → (none)");
        cleared.apply(&mut described);
        assert!(described.description.is_empty());
        assert!(described.description_html.is_empty());
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
        assert_eq!(
            rejection.failure(),
            "#613 failed: it changed in Azure DevOps",
            "a bulk summary says it shorter, because several of them share a line"
        );

        let rejection = EditRejection {
            conflict: false,
            message: "HTTP 400: field is read only".into(),
            ..rejection
        };
        assert_eq!(
            rejection.notification(),
            "#613 State not saved: HTTP 400: field is read only"
        );
        assert_eq!(
            rejection.failure(),
            "#613 failed: HTTP 400: field is read only"
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
    fn an_assignee_is_written_by_address_and_read_back_by_name() {
        let edit = FieldEdit::assignee("Jacob Ragsdale", Some("jacob@example.com"));
        assert_eq!(
            edit.patch(),
            vec![json!({
                "op": "add",
                "path": "/fields/System.AssignedTo",
                "value": "jacob@example.com",
            })],
            "Azure DevOps resolves the sign-in address"
        );
        assert_eq!(
            edit.summary(),
            "Assignee → Jacob Ragsdale",
            "the notification says the name, not the address"
        );

        let mut assigned = ticket();
        edit.apply(&mut assigned);
        assert_eq!(
            assigned.assigned_to.as_deref(),
            Some("Jacob Ragsdale"),
            "the cell shows the name while the write is in flight"
        );

        let by_name = FieldEdit::assignee("Jordan Patel", None);
        assert_eq!(
            by_name.patch(),
            vec![json!({
                "op": "add",
                "path": "/fields/System.AssignedTo",
                "value": "Jordan Patel",
            })],
            "a name with no address known is sent as itself"
        );

        let unassign = FieldEdit::unassign();
        assert_eq!(
            unassign.patch(),
            vec![json!({"op": "remove", "path": "/fields/System.AssignedTo"})]
        );
        assert_eq!(unassign.summary(), "Assignee → (none)");
        unassign.apply(&mut assigned);
        assert_eq!(assigned.assigned_to, None);
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
