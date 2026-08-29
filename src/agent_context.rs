use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::model::{RowDensity, SearchOrder, SortDirection, SortField};

pub const SCHEMA_VERSION: u8 = 2;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AgentContext {
    pub database_path: String,
    /// Display name of the signed-in Azure DevOps user, or `null` when the
    /// last sync could not read a profile.
    pub me: Option<String>,
    /// How fresh the rows are: where they are pulled from, whether a pull is
    /// running, and how the last one went.
    pub sync: SyncContext,
    /// Edits sent to Azure DevOps and not answered yet. The rows already show
    /// them, so a value named here is optimistic until the edit leaves this
    /// list.
    pub pending_edits: Vec<PendingEditContext>,
    pub mode: String,
    pub focus: String,
    pub screen: String,
    pub active_view: Option<String>,
    pub search: SearchContext,
    pub sort: SortContext,
    pub tickets: TicketsContext,
    pub selected_ticket: Option<TicketContext>,
    pub checked_tickets: Vec<TicketContext>,
    pub family_cursor: Option<TicketReference>,
    pub details_scroll_line: u16,
}

/// What the run knows about its own sync, so an agent can tell data that is a
/// minute old from data that stopped arriving an hour ago.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SyncContext {
    /// The Azure DevOps organization and project the rows are pulled from, or
    /// `null` on a run with no project resolved.
    pub organization: Option<String>,
    pub project: Option<String>,
    /// Seconds between timer pulls, `0` when the timer is off and the sync key
    /// is the only thing that pulls.
    pub refresh_seconds: u64,
    /// Whether a pull is in flight right now.
    pub in_progress: bool,
    /// When the last pull that reached Azure DevOps finished, RFC 3339, or
    /// `null` when none has this run. A pull that found nothing new still
    /// moves this: it says when the rows were last confirmed, not when they
    /// last changed.
    pub last_success_at: Option<String>,
    /// What the last failed pull said, cleared by the next one that succeeds.
    pub last_error: Option<String>,
    /// Whether the run has an Azure DevOps project to pull from at all. An
    /// offline run browses whatever the database already holds and never
    /// refreshes it. A run whose worker died later stays `false` and says so
    /// through `last_error` instead.
    pub offline: bool,
}

/// One edit the table is already showing and Azure DevOps has not answered yet.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PendingEditContext {
    pub id: i64,
    /// The field as the Edit menu names it, such as `State` or `Tags`.
    pub field: String,
    /// The value being written, as a notification spells it; a cleared field
    /// reads `(none)`.
    pub value: String,
    /// When the edit was sent, RFC 3339.
    pub since: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SearchContext {
    pub query: String,
    pub fuzzy_text: String,
    pub filters: Vec<String>,
    pub pending: bool,
    pub order: SearchOrder,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SortContext {
    pub field: SortField,
    pub direction: SortDirection,
    pub row_density: RowDensity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TicketsContext {
    pub total_count: usize,
    pub matching_count: usize,
    /// Whether the table is leaving finished work out, so the rows counted
    /// here are the open backlog rather than everything the query matches.
    /// The details pane and the family tree still reach a hidden work item.
    pub finished_hidden: bool,
    pub viewport_start: usize,
    pub viewport_size: usize,
    pub visible_rows: Vec<TicketContext>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TicketContext {
    pub organization: String,
    pub project: String,
    pub id: i64,
    pub work_item_type: String,
    pub title: String,
    pub state: String,
    pub assigned_to: Option<String>,
    pub priority: Option<i64>,
    pub tags: Vec<String>,
    pub web_url: String,
    pub bookmarked: bool,
    pub checked: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TicketReference {
    pub organization: String,
    pub id: i64,
}

#[derive(Serialize)]
struct ContextDocument<'a> {
    schema_version: u8,
    process_id: u32,
    updated_at: String,
    #[serde(flatten)]
    context: &'a AgentContext,
}

#[must_use]
pub fn path_for(database: &Path) -> PathBuf {
    let mut file_name = database
        .file_stem()
        .map_or_else(|| "tickets".into(), |stem| stem.to_os_string());
    file_name.push(".context.json");
    database.with_file_name(file_name)
}

pub fn save(path: &Path, context: &AgentContext) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let document = ContextDocument {
        schema_version: SCHEMA_VERSION,
        process_id: std::process::id(),
        updated_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .context("failed to format agent context time")?,
        context,
    };
    let mut raw =
        serde_json::to_string_pretty(&document).context("failed to serialize agent context")?;
    raw.push('\n');
    let temporary = temporary_path(path);
    fs::write(&temporary, raw)
        .with_context(|| format!("failed to write agent context {}", temporary.display()))?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error)
            .with_context(|| format!("failed to publish agent context {}", path.display()));
    }
    Ok(())
}

pub fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to remove agent context {}", path.display()))
        }
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map_or_else(|| "tickets.context.json".into(), |name| name.to_os_string());
    file_name.push(format!(".tmp.{}", std::process::id()));
    path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn context(database_path: String) -> AgentContext {
        AgentContext {
            database_path,
            me: Some("Jacob Ragsdale".into()),
            sync: SyncContext {
                organization: Some("example-org".into()),
                project: Some("atlas".into()),
                refresh_seconds: 60,
                in_progress: false,
                last_success_at: Some("2026-08-29T12:00:00Z".into()),
                last_error: None,
                offline: false,
            },
            pending_edits: Vec::new(),
            mode: "browse".into(),
            focus: "tickets".into(),
            screen: "workspace".into(),
            active_view: None,
            search: SearchContext {
                query: String::new(),
                fuzzy_text: String::new(),
                filters: Vec::new(),
                pending: false,
                order: SearchOrder::Relevance,
            },
            sort: SortContext {
                field: SortField::Changed,
                direction: SortDirection::Descending,
                row_density: RowDensity::Compact,
            },
            tickets: TicketsContext {
                total_count: 0,
                matching_count: 0,
                finished_hidden: true,
                viewport_start: 0,
                viewport_size: 0,
                visible_rows: Vec::new(),
            },
            selected_ticket: None,
            checked_tickets: Vec::new(),
            family_cursor: None,
            details_scroll_line: 0,
        }
    }

    #[test]
    fn save_replaces_a_complete_json_document_and_remove_is_idempotent() {
        let directory = tempdir().unwrap();
        let path = path_for(&directory.path().join("tickets.sqlite3"));
        assert_eq!(path, directory.path().join("tickets.context.json"));
        let mut first = context("first.sqlite3".into());
        first.pending_edits.push(PendingEditContext {
            id: 625,
            field: "State".into(),
            value: "Doing".into(),
            since: "2026-08-29T12:00:01Z".into(),
        });
        save(&path, &first).unwrap();
        let first_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(first_json["schema_version"], SCHEMA_VERSION);
        assert_eq!(first_json["database_path"], "first.sqlite3");
        assert_eq!(first_json["me"], "Jacob Ragsdale");
        assert_eq!(first_json["search"]["order"], "relevance");
        assert_eq!(first_json["sort"]["field"], "changed");
        assert_eq!(first_json["sort"]["direction"], "desc");
        assert_eq!(first_json["sort"]["row_density"], "compact");
        assert_eq!(first_json["tickets"]["finished_hidden"], true);
        assert_eq!(first_json["sync"]["organization"], "example-org");
        assert_eq!(first_json["sync"]["project"], "atlas");
        assert_eq!(first_json["sync"]["refresh_seconds"], 60);
        assert_eq!(first_json["sync"]["in_progress"], false);
        assert_eq!(
            first_json["sync"]["last_success_at"],
            "2026-08-29T12:00:00Z"
        );
        assert!(first_json["sync"]["last_error"].is_null());
        assert_eq!(first_json["sync"]["offline"], false);
        assert_eq!(first_json["pending_edits"][0]["id"], 625);
        assert_eq!(first_json["pending_edits"][0]["field"], "State");
        assert_eq!(first_json["pending_edits"][0]["value"], "Doing");
        assert_eq!(
            first_json["pending_edits"][0]["since"],
            "2026-08-29T12:00:01Z"
        );
        assert!(first_json["process_id"].as_u64().is_some());
        assert!(first_json["updated_at"].as_str().is_some());

        let mut second = context("second.sqlite3".into());
        second.me = None;
        save(&path, &second).unwrap();
        let second_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(second_json["database_path"], "second.sqlite3");
        assert!(second_json["me"].is_null());
        assert!(second_json["pending_edits"].as_array().unwrap().is_empty());
        assert!(!temporary_path(&path).exists());

        remove(&path).unwrap();
        remove(&path).unwrap();
        assert!(!path.exists());
    }
}
