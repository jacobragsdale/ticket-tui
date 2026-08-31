use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize};

use crate::app::TabId;
use crate::model::{Jump, RowDensity, SearchOrder, SortDirection, TicketKey};

/// One tab's slice of the session file: what it was showing, how it was
/// arranged, and the views saved on it. Sort field and columns are key strings
/// so one shape serves every tab.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TabSession {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub sort_field: String,
    #[serde(default)]
    pub sort_direction: SortDirection,
    #[serde(default)]
    pub search_order: SearchOrder,
    #[serde(default)]
    pub row_density: RowDensity,
    #[serde(default, deserialize_with = "known_columns")]
    pub columns: Vec<SessionColumn>,
    #[serde(default)]
    pub views: Vec<NamedView>,
    #[serde(default)]
    pub active_view: Option<String>,
}

/// The flat shape written before the tabs existed. A file carrying these loads
/// them into the work items tab and writes the new shape back; nothing written
/// by this build fills them in.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct FlatSession {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sort_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sort_direction: Option<SortDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    search_order: Option<SearchOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    row_density: Option<RowDensity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    columns: Option<Vec<SessionColumn>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    views: Option<Vec<NamedView>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_view: Option<String>,
    /// The work items the run had visited, before the history could cross
    /// tabs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recent: Option<Vec<TicketKey>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    /// The tab the run was left on, which is the one it reopens on.
    #[serde(default)]
    pub active_tab: TabId,
    #[serde(default)]
    pub work_items: TabSession,
    #[serde(default)]
    pub repos: TabSession,
    #[serde(default)]
    pub pull_requests: TabSession,
    #[serde(default)]
    pub pipelines: TabSession,
    #[serde(default)]
    pub aks: TabSession,
    #[serde(default)]
    pub bookmarks: Vec<TicketKey>,
    /// Everywhere the run had been, oldest last, across every tab.
    #[serde(default)]
    pub history: Vec<Jump>,
    /// Whether the table lists finished work. Absent from every session
    /// written before the toggle existed, so `false` is what those load as and
    /// an old session opens on the open backlog like a new one.
    #[serde(default)]
    pub show_finished: bool,
    /// The work item the cursor was left on. Rows belong to the work items tab
    /// for now; the tabs that grow rows of their own bring their own key.
    #[serde(default)]
    pub selected: Option<TicketKey>,
    #[serde(default = "wide_split")]
    pub pane_split_wide: u16,
    #[serde(default = "stacked_split")]
    pub pane_split_stacked: u16,
    #[serde(default = "details_split")]
    pub pane_split_details: u16,
    #[serde(default = "stale_days")]
    pub stale_days: u16,
    /// The flat shape a pre-tabs file carries, folded into the work items tab
    /// by [`load`] and never written back.
    #[serde(flatten)]
    pub(crate) flat: FlatSession,
}

impl Session {
    /// Folds a pre-tabs file into the work items tab, so nobody loses their
    /// query, columns or views on upgrade.
    fn fold_flat(&mut self) {
        let flat = std::mem::take(&mut self.flat);
        let tab = &mut self.work_items;
        if let Some(query) = flat.query {
            tab.query = query;
        }
        if let Some(field) = flat.sort_field {
            tab.sort_field = field;
        }
        if let Some(direction) = flat.sort_direction {
            tab.sort_direction = direction;
        }
        if let Some(order) = flat.search_order {
            tab.search_order = order;
        }
        if let Some(density) = flat.row_density {
            tab.row_density = density;
        }
        if let Some(columns) = flat.columns {
            tab.columns = columns;
        }
        if let Some(views) = flat.views {
            tab.views = views;
        }
        if flat.active_view.is_some() {
            tab.active_view = flat.active_view;
        }
        if let Some(recent) = flat.recent {
            self.history = recent.into_iter().map(Jump::WorkItem).collect();
        }
    }

    /// One tab's slice.
    #[must_use]
    pub fn tab(&self, tab: TabId) -> &TabSession {
        match tab {
            TabId::WorkItems => &self.work_items,
            TabId::Repos => &self.repos,
            TabId::PullRequests => &self.pull_requests,
            TabId::Pipelines => &self.pipelines,
            TabId::Aks => &self.aks,
        }
    }

    pub fn set_tab(&mut self, tab: TabId, session: TabSession) {
        match tab {
            TabId::WorkItems => self.work_items = session,
            TabId::Repos => self.repos = session,
            TabId::PullRequests => self.pull_requests = session,
            TabId::Pipelines => self.pipelines = session,
            TabId::Aks => self.aks = session,
        }
    }
}

/// Sessions written before the divider was draggable carry no split, so both
/// fields fall back to the built-in layout.
const fn wide_split() -> u16 {
    crate::app::DEFAULT_PANE_SPLIT_WIDE
}

const fn stacked_split() -> u16 {
    crate::app::DEFAULT_PANE_SPLIT_STACKED
}

/// The same for the seam inside the details pane, which sessions written
/// before that pane had one carry nothing for.
const fn details_split() -> u16 {
    crate::app::DEFAULT_PANE_SPLIT_DETAILS
}

/// Sessions written before the Changed column flagged neglected work carry no
/// threshold, so they fall back to the built-in fortnight.
const fn stale_days() -> u16 {
    crate::app::DEFAULT_STALE_DAYS
}

impl Default for Session {
    fn default() -> Self {
        Self {
            active_tab: TabId::default(),
            work_items: TabSession::default(),
            repos: TabSession::default(),
            pull_requests: TabSession::default(),
            pipelines: TabSession::default(),
            aks: TabSession::default(),
            bookmarks: Vec::new(),
            history: Vec::new(),
            show_finished: false,
            selected: None,
            pane_split_wide: wide_split(),
            pane_split_stacked: stacked_split(),
            pane_split_details: details_split(),
            stale_days: stale_days(),
            flat: FlatSession::default(),
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
    /// The column the view sorts by, by key — the same spelling the columns
    /// use, so one shape serves every tab's views.
    #[serde(default)]
    pub sort_field: String,
    #[serde(default)]
    pub sort_direction: SortDirection,
    #[serde(default)]
    pub search_order: SearchOrder,
    #[serde(default)]
    pub row_density: RowDensity,
    #[serde(default, deserialize_with = "known_columns")]
    pub columns: Vec<SessionColumn>,
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
    let mut session: Session = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse session {}", path.display()))?;
    session.fold_flat();
    Ok(session)
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
    use crate::columns::ColumnLayout;
    use crate::columns::TableLayout;
    use crate::filter::{FilterField, FilterSet, WorkItemSchema, format_query, parse_query};
    use tempfile::tempdir;

    #[test]
    fn a_query_holding_a_full_iteration_path_survives_a_restart() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.session.json");
        let mut filters = FilterSet::<WorkItemSchema>::default();
        filters.insert(FilterField::Iteration, "Atlas\\Sprint 1");
        let query = format_query(&filters, "");
        let mut session = Session::default();
        session.work_items.query = query.clone();
        session.work_items.views.push(NamedView {
            name: "Sprint 1".into(),
            query: query.clone(),
            sort_field: "changed".to_owned(),
            sort_direction: SortDirection::Descending,
            search_order: SearchOrder::Relevance,
            row_density: RowDensity::Compact,
            columns: TableLayout::<crate::model::SortField>::default().to_session_columns(),
        });

        save(&path, &session).unwrap();
        let loaded = load(&path).unwrap();

        for stored in [&loaded.work_items.query, &loaded.work_items.views[0].query] {
            assert_eq!(stored, &query, "the query text came back as it was written");
            assert!(
                parse_query::<WorkItemSchema>(stored)
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
            pane_split_wide: 70,
            pane_split_stacked: 44,
            stale_days: 21,
            ..Session::default()
        };
        session.work_items.query = "state:active".into();
        session.work_items.sort_field = "title".to_owned();
        session.work_items.sort_direction = SortDirection::Ascending;
        session.repos.query = "status:dirty".into();
        session.pull_requests.query = "author:@me".into();
        session.pipelines.query = "result:failed".into();
        session.active_tab = crate::app::TabId::Pipelines;
        session.bookmarks.push(TicketKey {
            organization: "demo".into(),
            id: 7,
        });
        session.history = vec![
            Jump::WorkItem(TicketKey {
                organization: "demo".into(),
                id: 7,
            }),
            Jump::Repo("ticket-tui".to_owned()),
        ];
        session.work_items.views.push(NamedView {
            name: "Active".into(),
            query: "state:active".into(),
            sort_field: "changed".to_owned(),
            sort_direction: SortDirection::Descending,
            search_order: SearchOrder::Relevance,
            row_density: RowDensity::Compact,
            columns: TableLayout::<crate::model::SortField>::default().to_session_columns(),
        });

        save(&path, &session).unwrap();
        let stored: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let loaded = load(&path).unwrap();

        assert_eq!(stored["active_tab"], "pipelines");
        assert_eq!(stored["work_items"]["sort_field"], "title");
        assert_eq!(stored["work_items"]["sort_direction"], "asc");
        assert_eq!(stored["work_items"]["search_order"], "relevance");
        assert_eq!(stored["work_items"]["row_density"], "compact");
        assert_eq!(stored["work_items"]["views"][0]["columns"][0]["id"], "id");
        assert_eq!(stored["repos"]["query"], "status:dirty");
        assert!(
            stored.get("query").is_none(),
            "the flat shape is not written any more: {stored}"
        );
        assert_eq!(stored["pane_split_wide"], 70);
        assert_eq!(stored["pane_split_stacked"], 44);
        assert_eq!(stored["stale_days"], 21);

        assert_eq!(loaded.active_tab, crate::app::TabId::Pipelines);
        assert_eq!(loaded.work_items.query, "state:active");
        assert_eq!(loaded.work_items.sort_field, "title");
        assert_eq!(loaded.work_items.sort_direction, SortDirection::Ascending);
        assert_eq!(loaded.repos.query, "status:dirty");
        assert_eq!(loaded.pull_requests.query, "author:@me");
        assert_eq!(loaded.pipelines.query, "result:failed");
        assert_eq!(loaded.bookmarks[0].id, 7);
        assert_eq!(
            loaded.history[1],
            Jump::Repo("ticket-tui".to_owned()),
            "the history remembers other tabs too"
        );
        assert_eq!(loaded.work_items.views[0].name, "Active");
        assert_eq!(loaded.work_items.views[0].sort_field, "changed");
        assert_eq!(
            loaded.work_items.views[0].columns.len(),
            TableLayout::<crate::model::SortField>::default()
                .columns
                .len()
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
        let mut layout = TableLayout::<crate::model::SortField>::default();
        let index = layout
            .columns
            .iter()
            .position(|column| column.id == crate::model::SortField::Progress)
            .expect("the layout offers a Progress column");
        ColumnLayout::toggle_visible(&mut layout, index);
        let mut session = Session::default();
        session.work_items.columns = layout.to_session_columns();

        save(&path, &session).unwrap();
        let loaded = load(&path).unwrap();
        let restored = TableLayout::<crate::model::SortField>::from_session_columns(
            &loaded.work_items.columns,
        );

        assert_eq!(
            restored.columns[index].id,
            crate::model::SortField::Progress
        );
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
                "auto_hide": false,
                "recent": [{ "organization": "demo", "id": 613 }],
                "views": [
                    { "name": "Mine", "query": "assignee:@me", "sort_field": "changed",
                      "sort_direction": "desc", "search_order": "relevance",
                      "row_density": "compact", "columns": [], "auto_hide": true }
                ],
                "active_view": "Mine"
            }"#,
        )
        .unwrap();

        let loaded = load(&path).unwrap();

        let work_items = &loaded.work_items;
        assert_eq!(
            work_items.query, "state:active",
            "the flat shape loads into the work items tab"
        );
        assert_eq!(work_items.sort_field, "priority");
        assert_eq!(work_items.sort_direction, SortDirection::Ascending);
        assert_eq!(work_items.search_order, SearchOrder::Field);
        assert_eq!(work_items.row_density, RowDensity::Comfortable);
        assert_eq!(
            loaded.active_tab,
            crate::app::TabId::WorkItems,
            "and a file written before the tabs opens on the first of them"
        );
        assert_eq!(
            work_items.views[0].name, "Mine",
            "the views written before the tabs are the work items tab's"
        );
        assert_eq!(work_items.active_view.as_deref(), Some("Mine"));
        assert!(
            loaded.repos.query.is_empty()
                && loaded.pipelines.views.is_empty()
                && loaded.aks.query.is_empty(),
            "and the tabs that did not exist start empty"
        );
        assert_eq!(
            loaded.history,
            vec![Jump::WorkItem(TicketKey {
                organization: "demo".into(),
                id: 613
            })],
            "the work items it had visited become the first cross-tab history"
        );
        assert_eq!(
            (loaded.pane_split_wide, loaded.pane_split_stacked),
            (62, 56),
            "a session written before the divider moved keeps the built-in split"
        );
        assert_eq!(
            loaded.stale_days, 14,
            "and one written before the Changed column flagged anything keeps the fortnight"
        );
        let ids: Vec<_> = work_items
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
            TableLayout::<crate::model::SortField>::from_session_columns(&work_items.columns);
        let restored: Vec<_> = layout
            .columns
            .iter()
            .take(2)
            .map(|column| column.id)
            .collect();
        assert_eq!(
            restored,
            [crate::model::SortField::Id, crate::model::SortField::Title],
            "and the work-item layout drops a column it has no name for"
        );
    }
}
