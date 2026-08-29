use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize};

use crate::model::{RowDensity, SearchOrder, SortDirection, SortField, TicketKey};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub sort_field: SortField,
    #[serde(default)]
    pub sort_direction: SortDirection,
    #[serde(default)]
    pub search_order: SearchOrder,
    #[serde(default)]
    pub row_density: RowDensity,
    #[serde(default, deserialize_with = "known_columns")]
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SessionColumn {
    #[serde(default)]
    pub id: SortField,
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
    #[serde(default)]
    pub sort_field: SortField,
    #[serde(default)]
    pub sort_direction: SortDirection,
    #[serde(default)]
    pub search_order: SearchOrder,
    #[serde(default)]
    pub row_density: RowDensity,
    #[serde(default, deserialize_with = "known_columns")]
    pub columns: Vec<SessionColumn>,
    pub auto_hide: bool,
}

/// Drops stored columns whose identifier is no longer known instead of
/// failing the whole session load.
fn known_columns<'de, D>(deserializer: D) -> Result<Vec<SessionColumn>, D::Error>
where
    D: Deserializer<'de>,
{
    let stored = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(stored
        .into_iter()
        .filter_map(|column| serde_json::from_value(column).ok())
        .collect())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::columns::TableLayout;
    use tempfile::tempdir;

    #[test]
    fn session_round_trips_through_json() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.session.json");
        let mut session = Session {
            query: "state:active".into(),
            sort_field: SortField::Title,
            sort_direction: SortDirection::Ascending,
            ..Session::default()
        };
        session.bookmarks.push(SessionKey {
            organization: "demo".into(),
            id: 7,
        });
        session.views.push(NamedView {
            name: "Active".into(),
            query: "state:active".into(),
            sort_field: SortField::Changed,
            sort_direction: SortDirection::Descending,
            search_order: SearchOrder::Relevance,
            row_density: RowDensity::Compact,
            columns: TableLayout::default().to_session_columns(),
            auto_hide: true,
        });

        save(&path, &session).unwrap();
        let stored: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let loaded = load(&path).unwrap();

        assert_eq!(stored["sort_field"], "title");
        assert_eq!(stored["sort_direction"], "asc");
        assert_eq!(stored["search_order"], "relevance");
        assert_eq!(stored["row_density"], "compact");
        assert_eq!(stored["views"][0]["columns"][0]["id"], "id");

        assert_eq!(loaded.query, "state:active");
        assert_eq!(loaded.sort_field, SortField::Title);
        assert_eq!(loaded.sort_direction, SortDirection::Ascending);
        assert_eq!(loaded.bookmarks[0].id, 7);
        assert_eq!(loaded.views[0].name, "Active");
        assert_eq!(loaded.views[0].sort_field, SortField::Changed);
        assert_eq!(
            loaded.views[0].columns.len(),
            TableLayout::default().columns.len()
        );
    }

    #[test]
    fn existing_string_sessions_load_into_typed_fields() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.session.json");
        fs::write(
            &path,
            r#"{
                "query": "state:active",
                "sort_field": "priority",
                "sort_direction": "asc",
                "search_order": "field",
                "row_density": "comfortable",
                "columns": [
                    { "id": "id", "visible": true, "width": 7 },
                    { "id": "sprint", "visible": true, "width": 9 },
                    { "id": "title", "visible": true, "width": 0 }
                ],
                "auto_hide": false
            }"#,
        )
        .unwrap();

        let loaded = load(&path).unwrap();

        assert_eq!(loaded.sort_field, SortField::Priority);
        assert_eq!(loaded.sort_direction, SortDirection::Ascending);
        assert_eq!(loaded.search_order, SearchOrder::Field);
        assert_eq!(loaded.row_density, RowDensity::Comfortable);
        assert_eq!(loaded.auto_hide, Some(false));
        let ids: Vec<_> = loaded.columns.iter().map(|column| column.id).collect();
        assert_eq!(ids, vec![SortField::Id, SortField::Title]);
    }
}
