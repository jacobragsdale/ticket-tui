use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::columns::{ColumnConfig, TableLayout};
use crate::model::{RowDensity, SearchOrder, SortDirection, SortField, TicketKey};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub sort_field: String,
    #[serde(default)]
    pub sort_direction: String,
    #[serde(default)]
    pub search_order: String,
    #[serde(default)]
    pub row_density: String,
    #[serde(default)]
    pub columns: Vec<SessionColumn>,
    #[serde(default)]
    pub auto_hide: Option<bool>,
    #[serde(default)]
    pub bookmarks: Vec<SessionKey>,
    #[serde(default)]
    pub recent: Vec<SessionKey>,
    #[serde(default)]
    pub views: Vec<NamedView>,
    #[serde(default)]
    pub active_view: Option<String>,
    #[serde(default)]
    pub selected: Option<SessionKey>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionColumn {
    pub id: String,
    pub visible: bool,
    pub width: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionKey {
    pub organization: String,
    pub id: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NamedView {
    pub name: String,
    pub query: String,
    pub sort_field: String,
    pub sort_direction: String,
    pub search_order: String,
    pub row_density: String,
    pub columns: Vec<SessionColumn>,
    pub auto_hide: bool,
}

impl From<&TicketKey> for SessionKey {
    fn from(key: &TicketKey) -> Self {
        Self {
            organization: key.organization.clone(),
            id: key.id,
        }
    }
}

impl From<&SessionKey> for TicketKey {
    fn from(key: &SessionKey) -> Self {
        Self {
            organization: key.organization.clone(),
            id: key.id,
        }
    }
}

#[must_use]
pub fn path_for(database: &Path) -> PathBuf {
    let mut file_name = database
        .file_stem()
        .map_or_else(|| "tickets".into(), |stem| stem.to_os_string());
    file_name.push(".session.json");
    database.with_file_name(file_name)
}

pub fn load(path: &Path) -> Result<Session> {
    if !path.exists() {
        return Ok(Session::default());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read session {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse session {}", path.display()))
}

pub fn save(path: &Path, session: &Session) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw = serde_json::to_string_pretty(session).context("failed to serialize session")?;
    fs::write(path, raw).with_context(|| format!("failed to write session {}", path.display()))
}

#[must_use]
pub fn encode_sort_field(field: SortField) -> &'static str {
    match field {
        SortField::Changed => "changed",
        SortField::Priority => "priority",
        SortField::Id => "id",
        SortField::Title => "title",
        SortField::State => "state",
        SortField::Type => "type",
        SortField::Assignee => "assignee",
        SortField::Organization => "organization",
        SortField::Project => "project",
        SortField::Area => "area",
        SortField::Iteration => "iteration",
        SortField::Created => "created",
        SortField::Tags => "tags",
    }
}

#[must_use]
pub fn decode_sort_field(value: &str) -> Option<SortField> {
    Some(match value {
        "changed" => SortField::Changed,
        "priority" => SortField::Priority,
        "id" => SortField::Id,
        "title" => SortField::Title,
        "state" => SortField::State,
        "type" => SortField::Type,
        "assignee" => SortField::Assignee,
        "organization" => SortField::Organization,
        "project" => SortField::Project,
        "area" => SortField::Area,
        "iteration" => SortField::Iteration,
        "created" => SortField::Created,
        "tags" => SortField::Tags,
        _ => return None,
    })
}

#[must_use]
pub fn encode_layout(layout: &TableLayout) -> Vec<SessionColumn> {
    layout
        .columns
        .iter()
        .map(|column| SessionColumn {
            id: encode_sort_field(column.id).to_owned(),
            visible: column.visible,
            width: column.width,
        })
        .collect()
}

#[must_use]
pub fn decode_layout(columns: &[SessionColumn], auto_hide: Option<bool>) -> TableLayout {
    let mut layout = TableLayout::default();
    if columns.is_empty() {
        if let Some(auto_hide) = auto_hide {
            layout.auto_hide = auto_hide;
        }
        return layout;
    }
    let mut decoded = Vec::new();
    for column in columns {
        let Some(id) = decode_sort_field(&column.id) else {
            continue;
        };
        decoded.push(ColumnConfig {
            id,
            visible: column.visible,
            width: column.width,
        });
    }
    for default in &layout.columns {
        if !decoded.iter().any(|column| column.id == default.id) {
            decoded.push(*default);
        }
    }
    layout.columns = decoded;
    layout.auto_hide = auto_hide.unwrap_or(false);
    layout
}

#[must_use]
pub fn encode_density(density: RowDensity) -> &'static str {
    match density {
        RowDensity::Compact => "compact",
        RowDensity::Comfortable => "comfortable",
    }
}

#[must_use]
pub fn decode_density(value: &str) -> RowDensity {
    if value == "comfortable" {
        RowDensity::Comfortable
    } else {
        RowDensity::Compact
    }
}

#[must_use]
pub fn encode_search_order(order: SearchOrder) -> &'static str {
    match order {
        SearchOrder::Relevance => "relevance",
        SearchOrder::Field => "field",
    }
}

#[must_use]
pub fn decode_search_order(value: &str) -> SearchOrder {
    if value == "field" {
        SearchOrder::Field
    } else {
        SearchOrder::Relevance
    }
}

#[must_use]
pub fn encode_direction(direction: SortDirection) -> &'static str {
    match direction {
        SortDirection::Ascending => "asc",
        SortDirection::Descending => "desc",
    }
}

#[must_use]
pub fn decode_direction(value: &str) -> SortDirection {
    if value == "asc" {
        SortDirection::Ascending
    } else {
        SortDirection::Descending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn session_round_trips_through_json() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.session.json");
        let mut session = Session {
            query: "state:active".into(),
            sort_field: "title".into(),
            sort_direction: "asc".into(),
            ..Session::default()
        };
        session.bookmarks.push(SessionKey {
            organization: "demo".into(),
            id: 7,
        });
        session.views.push(NamedView {
            name: "Active".into(),
            query: "state:active".into(),
            sort_field: "changed".into(),
            sort_direction: "desc".into(),
            search_order: "relevance".into(),
            row_density: "compact".into(),
            columns: encode_layout(&TableLayout::default()),
            auto_hide: true,
        });

        save(&path, &session).unwrap();
        let loaded = load(&path).unwrap();

        assert_eq!(loaded.query, "state:active");
        assert_eq!(loaded.bookmarks[0].id, 7);
        assert_eq!(loaded.views[0].name, "Active");
    }

    #[test]
    fn missing_session_file_is_an_empty_session() {
        let session = load(Path::new("/tmp/does-not-exist-ticket-tui.json")).unwrap();
        assert!(session.query.is_empty());
        assert!(session.views.is_empty());
    }
}
