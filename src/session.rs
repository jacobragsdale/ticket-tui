use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize};

use crate::model::{RowDensity, SearchOrder, SortDirection, SortField, TicketKey};

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub bookmarks: Vec<TicketKey>,
    #[serde(default)]
    pub recent: Vec<TicketKey>,
    #[serde(default)]
    pub views: Vec<NamedView>,
    #[serde(default)]
    pub active_view: Option<String>,
    /// Whether the table lists finished work. Absent from every session
    /// written before the toggle existed, so `false` is what those load as and
    /// an old session opens on the open backlog like a new one.
    #[serde(default)]
    pub show_finished: bool,
    #[serde(default)]
    pub selected: Option<TicketKey>,
    #[serde(default = "wide_split")]
    pub pane_split_wide: u16,
    #[serde(default = "stacked_split")]
    pub pane_split_stacked: u16,
    #[serde(default = "stale_days")]
    pub stale_days: u16,
}

/// Sessions written before the divider was draggable carry no split, so both
/// fields fall back to the built-in layout.
const fn wide_split() -> u16 {
    crate::app::DEFAULT_PANE_SPLIT_WIDE
}

const fn stacked_split() -> u16 {
    crate::app::DEFAULT_PANE_SPLIT_STACKED
}

/// Sessions written before the Changed column flagged neglected work carry no
/// threshold, so they fall back to the built-in fortnight.
const fn stale_days() -> u16 {
    crate::app::DEFAULT_STALE_DAYS
}

impl Default for Session {
    fn default() -> Self {
        Self {
            query: String::new(),
            sort_field: SortField::default(),
            sort_direction: SortDirection::default(),
            search_order: SearchOrder::default(),
            row_density: RowDensity::default(),
            columns: Vec::new(),
            auto_hide: None,
            bookmarks: Vec::new(),
            recent: Vec::new(),
            views: Vec::new(),
            active_view: None,
            show_finished: false,
            selected: None,
            pane_split_wide: wide_split(),
            pane_split_stacked: stacked_split(),
            stale_days: stale_days(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionColumn {
    /// The column's key, as [`crate::columns::ColumnId::key`] spells it. A
    /// string rather than an enum so every screen's columns share one file
    /// shape; the work-item keys are what earlier builds already wrote.
    #[serde(default)]
    pub id: String,
    pub visible: bool,
    pub width: u16,
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

/// Drops a stored column that is not shaped like one instead of failing the
/// whole session load. A column whose key no name in this screen's set is kept
/// here and dropped by [`crate::columns::TableLayout::from_session_columns`],
/// which is the only place that knows what the keys mean.
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
    use crate::columns::ColumnLayout;
    use super::*;
    use crate::columns::TableLayout;
    use crate::filter::{FilterField, FilterSet, format_query, parse_query};
    use tempfile::tempdir;

    #[test]
    fn a_query_holding_a_full_iteration_path_survives_a_restart() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.session.json");
        let mut filters = FilterSet::default();
        filters.insert(FilterField::Iteration, "Atlas\\Sprint 1");
        let query = format_query(&filters, "");
        let mut session = Session {
            query: query.clone(),
            ..Session::default()
        };
        session.views.push(NamedView {
            name: "Sprint 1".into(),
            query: query.clone(),
            sort_field: SortField::Changed,
            sort_direction: SortDirection::Descending,
            search_order: SearchOrder::Relevance,
            row_density: RowDensity::Compact,
            columns: TableLayout::<SortField>::default().to_session_columns(),
            auto_hide: true,
        });

        save(&path, &session).unwrap();
        let loaded = load(&path).unwrap();

        for stored in [&loaded.query, &loaded.views[0].query] {
            assert_eq!(stored, &query, "the query text came back as it was written");
            assert!(
                parse_query(stored)
                    .filters
                    .contains(FilterField::Iteration, "Atlas\\Sprint 1"),
                "the path still selects its own rows: {stored}"
            );
        }
    }

    #[test]
    fn session_round_trips_through_json() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.session.json");
        let mut session = Session {
            query: "state:active".into(),
            sort_field: SortField::Title,
            sort_direction: SortDirection::Ascending,
            pane_split_wide: 70,
            pane_split_stacked: 44,
            stale_days: 21,
            ..Session::default()
        };
        session.bookmarks.push(TicketKey {
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
            columns: TableLayout::<SortField>::default().to_session_columns(),
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
        assert_eq!(stored["pane_split_wide"], 70);
        assert_eq!(stored["pane_split_stacked"], 44);
        assert_eq!(stored["stale_days"], 21);

        assert_eq!(loaded.query, "state:active");
        assert_eq!(loaded.sort_field, SortField::Title);
        assert_eq!(loaded.sort_direction, SortDirection::Ascending);
        assert_eq!(loaded.bookmarks[0].id, 7);
        assert_eq!(loaded.views[0].name, "Active");
        assert_eq!(loaded.views[0].sort_field, SortField::Changed);
        assert_eq!(
            loaded.views[0].columns.len(),
            TableLayout::<SortField>::default().columns.len()
        );
        assert_eq!(loaded.pane_split_wide, 70);
        assert_eq!(loaded.pane_split_stacked, 44);
        assert_eq!(
            loaded.stale_days, 21,
            "the stale threshold survives a restart"
        );
    }

    #[test]
    fn a_progress_column_switched_on_is_still_on_after_a_restart() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.session.json");
        let mut layout = TableLayout::<SortField>::default();
        let index = layout
            .columns
            .iter()
            .position(|column| column.id == SortField::Progress)
            .expect("the layout offers a Progress column");
        ColumnLayout::toggle_visible(&mut layout, index);
        let session = Session {
            columns: layout.to_session_columns(),
            auto_hide: Some(layout.auto_hide),
            ..Session::default()
        };

        save(&path, &session).unwrap();
        let loaded = load(&path).unwrap();
        let restored =
            TableLayout::<SortField>::from_session_columns(&loaded.columns, loaded.auto_hide);

        assert_eq!(restored.columns[index].id, SortField::Progress);
        assert!(
            restored.columns[index].visible,
            "the column the overlay switched on comes back switched on"
        );
    }

    #[test]
    fn a_session_written_before_the_toggle_kept_finished_tickets_hidden() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.session.json");
        fs::write(&path, r#"{ "query": "" }"#).unwrap();

        let loaded = load(&path).unwrap();

        assert!(
            !loaded.show_finished,
            "a session with no toggle in it opens on the open backlog"
        );

        let session = Session {
            show_finished: true,
            ..Session::default()
        };
        save(&path, &session).unwrap();
        let stored: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

        assert_eq!(stored["show_finished"], true);
        assert!(
            load(&path).unwrap().show_finished,
            "the choice to list them again comes back on the next run"
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
        assert_eq!(
            (loaded.pane_split_wide, loaded.pane_split_stacked),
            (62, 56),
            "a session written before the divider moved keeps the built-in split"
        );
        assert_eq!(
            loaded.stale_days, 14,
            "and one written before the Changed column flagged anything keeps the fortnight"
        );
        let ids: Vec<_> = loaded
            .columns
            .iter()
            .map(|column| column.id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["id", "sprint", "title"],
            "the file keeps every key it holds, whoever they belong to"
        );
        let layout =
            TableLayout::<SortField>::from_session_columns(&loaded.columns, loaded.auto_hide);
        let restored: Vec<_> = layout
            .columns
            .iter()
            .take(2)
            .map(|column| column.id)
            .collect();
        assert_eq!(
            restored,
            [SortField::Id, SortField::Title],
            "and the work-item layout drops a column it has no name for"
        );
    }
}
