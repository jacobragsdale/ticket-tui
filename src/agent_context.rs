use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::model::{RowDensity, SearchOrder, SortDirection, SortField};

pub const SCHEMA_VERSION: u8 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AgentContext {
    pub database_path: String,
    pub read_only: bool,
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
            read_only: false,
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
    fn context_path_sits_beside_the_database() {
        assert_eq!(
            path_for(Path::new("/tmp/demo.sqlite3")),
            PathBuf::from("/tmp/demo.context.json")
        );
    }

    #[test]
    fn save_replaces_a_complete_json_document_and_remove_is_idempotent() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.context.json");
        let first = context("first.sqlite3".into());
        save(&path, &first).unwrap();
        let first_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(first_json["schema_version"], SCHEMA_VERSION);
        assert_eq!(first_json["database_path"], "first.sqlite3");
        assert_eq!(first_json["search"]["order"], "relevance");
        assert_eq!(first_json["sort"]["field"], "changed");
        assert_eq!(first_json["sort"]["direction"], "desc");
        assert_eq!(first_json["sort"]["row_density"], "compact");
        assert!(first_json["process_id"].as_u64().is_some());
        assert!(first_json["updated_at"].as_str().is_some());

        let second = context("second.sqlite3".into());
        save(&path, &second).unwrap();
        let second_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(second_json["database_path"], "second.sqlite3");
        assert!(!temporary_path(&path).exists());

        remove(&path).unwrap();
        remove(&path).unwrap();
        assert!(!path.exists());
    }
}
