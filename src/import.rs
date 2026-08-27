use serde_json::Value;

use crate::model::{CommentRecord, HistoryRecord, RelationKind, RelationRecord, Ticket, TicketKey};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportFormat {
    Json,
    Csv,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportDiagnostic {
    pub location: String,
    pub message: String,
}

impl std::fmt::Display for ImportDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.location, self.message)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportBatch {
    pub tickets: Vec<Ticket>,
    pub relations: Vec<RelationRecord>,
    pub comments: Vec<CommentRecord>,
    pub history: Vec<HistoryRecord>,
    pub diagnostics: Vec<ImportDiagnostic>,
}

impl ImportBatch {
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{} tickets", self.tickets.len())];
        if !self.relations.is_empty() {
            parts.push(format!("{} relations", self.relations.len()));
        }
        if !self.comments.is_empty() {
            parts.push(format!("{} comments", self.comments.len()));
        }
        if !self.history.is_empty() {
            parts.push(format!("{} history rows", self.history.len()));
        }
        if !self.diagnostics.is_empty() {
            parts.push(format!("{} issues", self.diagnostics.len()));
        }
        parts.join(", ")
    }
}

#[must_use]
pub fn parse_json(raw: &str) -> ImportBatch {
    let mut batch = ImportBatch::default();
    let parsed = match serde_json::from_str::<Value>(raw) {
        Ok(value) => value,
        Err(error) => {
            batch.diagnostics.push(ImportDiagnostic {
                location: "json".into(),
                message: error.to_string(),
            });
            return batch;
        }
    };
    let rows = match parsed {
        Value::Array(rows) => rows,
        Value::Object(object) => object
            .get("tickets")
            .or_else(|| object.get("items"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                batch.diagnostics.push(ImportDiagnostic {
                    location: "json".into(),
                    message: "expected an array or an object with a tickets array".into(),
                });
                Vec::new()
            }),
        _ => {
            batch.diagnostics.push(ImportDiagnostic {
                location: "json".into(),
                message: "expected a JSON array of tickets".into(),
            });
            Vec::new()
        }
    };
    for (index, row) in rows.iter().enumerate() {
        let location = format!("item {}", index + 1);
        match draft_from_json(row, &location) {
            Ok(draft) => append_draft(&mut batch, draft),
            Err(diagnostic) => batch.diagnostics.push(diagnostic),
        }
    }
    batch
}

#[must_use]
pub fn parse_csv(raw: &str) -> ImportBatch {
    let mut batch = ImportBatch::default();
    let mut lines = parse_csv_rows(raw).into_iter();
    let Some(headers) = lines.next() else {
        batch.diagnostics.push(ImportDiagnostic {
            location: "csv".into(),
            message: "file is empty".into(),
        });
        return batch;
    };
    if headers.is_empty() {
        batch.diagnostics.push(ImportDiagnostic {
            location: "csv".into(),
            message: "header row is empty".into(),
        });
        return batch;
    }
    let header_map: Vec<String> = headers
        .iter()
        .map(|header| header.to_ascii_lowercase())
        .collect();
    for (index, row) in lines.enumerate() {
        let location = format!("row {}", index + 2);
        if row.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        match draft_from_csv(&header_map, &row, &location) {
            Ok(draft) => append_draft(&mut batch, draft),
            Err(diagnostic) => batch.diagnostics.push(diagnostic),
        }
    }
    batch
}

struct Draft {
    ticket: Ticket,
    relations: Vec<RelationRecord>,
    comments: Vec<CommentRecord>,
    history: Vec<HistoryRecord>,
}

fn append_draft(batch: &mut ImportBatch, draft: Draft) {
    batch.relations.extend(draft.relations);
    batch.comments.extend(draft.comments);
    batch.history.extend(draft.history);
    batch.tickets.push(draft.ticket);
}

fn draft_from_json(value: &Value, location: &str) -> Result<Draft, ImportDiagnostic> {
    let object = value.as_object().ok_or_else(|| ImportDiagnostic {
        location: location.into(),
        message: "ticket must be an object".into(),
    })?;
    let id = int_field(
        object.get("id").or_else(|| object.get("work_item_id")),
        location,
        "id",
    )?;
    let title = required_text(object.get("title"), location, "title")?;
    let organization = text_or(object.get("organization"), "imported");
    let ticket = Ticket {
        key: TicketKey {
            organization: organization.clone(),
            id,
        },
        project: text_or(object.get("project"), "imported"),
        revision: int_or(object.get("revision"), 1),
        work_item_type: text_or(
            object.get("type").or_else(|| object.get("work_item_type")),
            "Issue",
        ),
        title,
        state: text_or(object.get("state"), "New"),
        reason: optional_text(object.get("reason")),
        assigned_to: optional_text(object.get("assignee").or_else(|| object.get("assigned_to"))),
        priority: optional_int(object.get("priority")),
        area_path: text_or(object.get("area").or_else(|| object.get("area_path")), ""),
        iteration_path: text_or(
            object
                .get("iteration")
                .or_else(|| object.get("iteration_path")),
            "",
        ),
        tags: tags_from_json(object.get("tags")),
        description: text_or(object.get("description"), ""),
        created_at: text_or(object.get("created_at"), "1970-01-01T00:00:00Z"),
        changed_at: text_or(
            object.get("changed_at"),
            text_or(object.get("created_at"), "1970-01-01T00:00:00Z").as_str(),
        ),
        web_url: text_or(object.get("url").or_else(|| object.get("web_url")), ""),
    };
    let relations = relations_from_json(object.get("relations"), &ticket.key, location)?;
    Ok(Draft {
        ticket,
        relations,
        comments: Vec::new(),
        history: Vec::new(),
    })
}

fn relations_from_json(
    value: Option<&Value>,
    from: &TicketKey,
    location: &str,
) -> Result<Vec<RelationRecord>, ImportDiagnostic> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let rows = value.as_array().ok_or_else(|| ImportDiagnostic {
        location: location.into(),
        message: "relations must be an array".into(),
    })?;
    let mut relations = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let object = row.as_object().ok_or_else(|| ImportDiagnostic {
            location: format!("{location} relation {}", index + 1),
            message: "relation must be an object".into(),
        })?;
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .and_then(RelationKind::parse)
            .ok_or_else(|| ImportDiagnostic {
                location: format!("{location} relation {}", index + 1),
                message: "missing or unknown relation kind".into(),
            })?;
        let to_id = int_field(
            object.get("id").or_else(|| object.get("to")),
            location,
            "relation id",
        )?;
        let organization = object
            .get("organization")
            .and_then(Value::as_str)
            .unwrap_or(from.organization.as_str())
            .to_owned();
        relations.push(RelationRecord {
            from: from.clone(),
            to: TicketKey {
                organization,
                id: to_id,
            },
            kind,
        });
    }
    Ok(relations)
}

fn draft_from_csv(
    headers: &[String],
    row: &[String],
    location: &str,
) -> Result<Draft, ImportDiagnostic> {
    let get = |name: &str| -> Option<&str> {
        headers
            .iter()
            .position(|header| header == name)
            .and_then(|index| row.get(index))
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let id = get("id")
        .or_else(|| get("work_item_id"))
        .ok_or_else(|| ImportDiagnostic {
            location: location.into(),
            message: "missing id".into(),
        })?
        .parse::<i64>()
        .map_err(|_| ImportDiagnostic {
            location: location.into(),
            message: "id must be an integer".into(),
        })?;
    let title = get("title").ok_or_else(|| ImportDiagnostic {
        location: location.into(),
        message: "missing title".into(),
    })?;
    let tags = get("tags")
        .unwrap_or_default()
        .split([';', ','])
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect();
    let priority = get("priority")
        .map(|value| {
            value.parse::<i64>().map_err(|_| ImportDiagnostic {
                location: location.into(),
                message: "priority must be an integer".into(),
            })
        })
        .transpose()?;
    Ok(Draft {
        ticket: Ticket {
            key: TicketKey {
                organization: get("organization").unwrap_or("imported").to_owned(),
                id,
            },
            project: get("project").unwrap_or("imported").to_owned(),
            revision: 1,
            work_item_type: get("type")
                .or_else(|| get("work_item_type"))
                .unwrap_or("Issue")
                .to_owned(),
            title: title.to_owned(),
            state: get("state").unwrap_or("New").to_owned(),
            reason: None,
            assigned_to: get("assignee")
                .or_else(|| get("assigned_to"))
                .map(str::to_owned),
            priority,
            area_path: get("area")
                .or_else(|| get("area_path"))
                .unwrap_or("")
                .to_owned(),
            iteration_path: get("iteration")
                .or_else(|| get("iteration_path"))
                .unwrap_or("")
                .to_owned(),
            tags,
            description: get("description").unwrap_or("").to_owned(),
            created_at: get("created_at")
                .unwrap_or("1970-01-01T00:00:00Z")
                .to_owned(),
            changed_at: get("changed_at")
                .or_else(|| get("created_at"))
                .unwrap_or("1970-01-01T00:00:00Z")
                .to_owned(),
            web_url: get("url")
                .or_else(|| get("web_url"))
                .unwrap_or("")
                .to_owned(),
        },
        relations: Vec::new(),
        comments: Vec::new(),
        history: Vec::new(),
    })
}

fn parse_csv_rows(raw: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut current = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = raw.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '"' if in_quotes => {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            }
            '"' => in_quotes = true,
            ',' if !in_quotes => {
                current.push(std::mem::take(&mut field));
            }
            '\n' if !in_quotes => {
                current.push(std::mem::take(&mut field));
                if !current.iter().all(String::is_empty) {
                    rows.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
            }
            '\r' => {}
            _ => field.push(character),
        }
    }
    if in_quotes || !field.is_empty() || !current.is_empty() {
        current.push(field);
        if !current.iter().all(String::is_empty) {
            rows.push(current);
        }
    }
    rows
}

fn required_text(
    value: Option<&Value>,
    location: &str,
    field: &str,
) -> Result<String, ImportDiagnostic> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ImportDiagnostic {
            location: location.into(),
            message: format!("missing {field}"),
        })
}

fn text_or(value: Option<&Value>, default: &str) -> String {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map_or_else(|| default.to_owned(), str::to_owned)
}

fn optional_text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn int_field(value: Option<&Value>, location: &str, field: &str) -> Result<i64, ImportDiagnostic> {
    match value {
        Some(Value::Number(number)) => number.as_i64().ok_or_else(|| ImportDiagnostic {
            location: location.into(),
            message: format!("{field} must be an integer"),
        }),
        Some(Value::String(text)) => text.parse().map_err(|_| ImportDiagnostic {
            location: location.into(),
            message: format!("{field} must be an integer"),
        }),
        _ => Err(ImportDiagnostic {
            location: location.into(),
            message: format!("missing {field}"),
        }),
    }
}

fn int_or(value: Option<&Value>, default: i64) -> i64 {
    match value {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(default),
        Some(Value::String(text)) => text.parse().unwrap_or(default),
        _ => default,
    }
}

fn optional_int(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(number)) => number.as_i64(),
        Some(Value::String(text)) => text.parse().ok(),
        _ => None,
    }
}

fn tags_from_json(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect(),
        Some(Value::String(text)) => text
            .split([';', ','])
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_import_requires_id_and_title_and_keeps_valid_rows() {
        let batch = parse_json(
            r#"[{"id":1,"title":"One"},{"title":"Missing id"},{"id":2,"title":"Two","type":"Bug","relations":[{"kind":"parent","id":1}]}]"#,
        );
        assert_eq!(batch.tickets.len(), 2);
        assert_eq!(batch.relations.len(), 1);
        assert_eq!(batch.diagnostics.len(), 1);
        assert!(batch.diagnostics[0].message.contains("missing id"));
    }

    #[test]
    fn csv_import_reports_bad_priority_and_quoted_titles() {
        let batch = parse_csv("id,title,priority\n1,\"Fix, please\",1\n2,Broken,nope\n");
        assert_eq!(batch.tickets.len(), 1);
        assert_eq!(batch.tickets[0].title, "Fix, please");
        assert_eq!(batch.diagnostics.len(), 1);
        assert!(batch.diagnostics[0].message.contains("priority"));
    }

    #[test]
    fn json_object_wrapper_is_accepted() {
        let batch = parse_json(r#"{"tickets":[{"id":9,"title":"Wrapped"}]}"#);
        assert_eq!(batch.tickets[0].key.id, 9);
    }
}
