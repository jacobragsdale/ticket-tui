use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::model::{Ticket, TicketKey};

const SCHEMA_VERSION: i64 = 1;
pub const DEMO_TICKET_COUNT: usize = 500;

const CREATE_SCHEMA: &str = r#"
CREATE TABLE work_items (
    organization   TEXT NOT NULL,
    project        TEXT NOT NULL,
    work_item_id   INTEGER NOT NULL,
    revision       INTEGER NOT NULL,
    work_item_type TEXT NOT NULL,
    title          TEXT NOT NULL,
    state          TEXT NOT NULL,
    reason         TEXT,
    assigned_to    TEXT,
    priority       INTEGER,
    area_path      TEXT NOT NULL,
    iteration_path TEXT NOT NULL,
    tags           TEXT NOT NULL DEFAULT '',
    description    TEXT NOT NULL DEFAULT '',
    created_at     TEXT NOT NULL,
    changed_at     TEXT NOT NULL,
    web_url        TEXT NOT NULL,
    PRIMARY KEY (organization, work_item_id)
);
CREATE INDEX work_items_changed_idx ON work_items(changed_at);
CREATE INDEX work_items_priority_idx ON work_items(priority);
CREATE INDEX work_items_state_idx ON work_items(state);
CREATE INDEX work_items_type_idx ON work_items(work_item_type);
"#;

#[derive(Debug)]
pub struct OpenedRepository {
    pub repository: SqliteTicketRepository,
    pub seeded_demo_data: bool,
}

#[derive(Debug)]
pub struct SqliteTicketRepository {
    connection: Connection,
    path: PathBuf,
}

impl SqliteTicketRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<OpenedRepository> {
        let path = path.as_ref().to_path_buf();
        let is_new = !path.exists();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let mut connection = Connection::open(&path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;
        connection
            .busy_timeout(StdDuration::from_secs(3))
            .context("failed to configure SQLite busy timeout")?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
            .context("failed to configure SQLite")?;

        migrate(&connection)?;
        if is_new {
            seed_demo_data(&mut connection, DEMO_TICKET_COUNT)?;
        }

        Ok(OpenedRepository {
            repository: Self { connection, path },
            seeded_demo_data: is_new,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_all(&self) -> Result<Vec<Ticket>> {
        let mut statement = self.connection.prepare(
            "SELECT organization, project, work_item_id, revision, work_item_type,
                    title, state, reason, assigned_to, priority, area_path,
                    iteration_path, tags, description, created_at, changed_at, web_url
             FROM work_items",
        )?;
        let rows = statement.query_map([], |row| {
            let raw_tags: String = row.get(12)?;
            Ok(Ticket {
                key: TicketKey {
                    organization: row.get(0)?,
                    id: row.get(2)?,
                },
                project: row.get(1)?,
                revision: row.get(3)?,
                work_item_type: row.get(4)?,
                title: row.get(5)?,
                state: row.get(6)?,
                reason: row.get(7)?,
                assigned_to: row.get(8)?,
                priority: row.get(9)?,
                area_path: row.get(10)?,
                iteration_path: row.get(11)?,
                tags: raw_tags
                    .split(';')
                    .map(str::trim)
                    .filter(|tag| !tag.is_empty())
                    .map(str::to_owned)
                    .collect(),
                description: row.get(13)?,
                created_at: row.get(14)?,
                changed_at: row.get(15)?,
                web_url: row.get(16)?,
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load work items")
    }
}

#[must_use]
pub fn default_database_path() -> PathBuf {
    ProjectDirs::from("", "", "ticket-tui").map_or_else(
        || PathBuf::from("tickets.sqlite3"),
        |dirs| dirs.data_dir().join("tickets.sqlite3"),
    )
}

fn migrate(connection: &Connection) -> Result<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        bail!("database schema version {version} is newer than supported version {SCHEMA_VERSION}");
    }
    if version == 0 {
        let existing_table: Option<String> = connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'work_items'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if existing_table.is_some() {
            bail!("database contains an unversioned work_items table; refusing to overwrite it");
        }
        connection.execute_batch(CREATE_SCHEMA)?;
        connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(())
}

fn seed_demo_data(connection: &mut Connection, count: usize) -> Result<()> {
    let transaction = connection.transaction()?;
    let anchor = OffsetDateTime::now_utc();
    for index in 0..count {
        insert_demo_ticket(&transaction, index, anchor)?;
    }
    transaction.commit()?;
    Ok(())
}

#[allow(clippy::cast_possible_wrap)]
fn insert_demo_ticket(
    transaction: &Transaction<'_>,
    index: usize,
    anchor: OffsetDateTime,
) -> Result<()> {
    const TYPES: [&str; 5] = ["Epic", "Feature", "User Story", "Bug", "Task"];
    const STATES: [(&str, &str); 5] = [
        ("New", "New work item"),
        ("Active", "Implementation started"),
        ("Resolved", "Ready for validation"),
        ("Closed", "Work completed"),
        ("Removed", "Removed from backlog"),
    ];
    const ASSIGNEES: [Option<&str>; 7] = [
        Some("Avery Chen"),
        Some("Jordan Patel"),
        Some("Morgan Lee"),
        Some("Riley Smith"),
        Some("Taylor Garcia"),
        Some("Casey Nguyen"),
        None,
    ];
    const AREAS: [&str; 5] = [
        "Atlas\\Platform",
        "Atlas\\Developer Experience",
        "Atlas\\Billing",
        "Atlas\\Identity",
        "Atlas\\Observability",
    ];
    const TITLE_VERBS: [&str; 10] = [
        "Improve",
        "Fix",
        "Add",
        "Investigate",
        "Document",
        "Harden",
        "Migrate",
        "Simplify",
        "Measure",
        "Automate",
    ];
    const TITLE_OBJECTS: [&str; 10] = [
        "deployment health checks",
        "ticket search relevance",
        "session timeout handling",
        "billing export pipeline",
        "developer onboarding",
        "audit log retention",
        "service ownership metadata",
        "release notifications",
        "API retry behavior",
        "dashboard accessibility",
    ];
    const TAG_SETS: [&str; 8] = [
        "backend;rust",
        "frontend;accessibility",
        "customer;priority",
        "platform;reliability",
        "security;identity",
        "technical-debt",
        "documentation;developer-experience",
        "observability;performance",
    ];

    let id = 10_001 + index as i64;
    let (state, reason) = STATES[index % STATES.len()];
    let created = anchor - Duration::days((index % 365) as i64 + 7);
    let changed = created + Duration::days((index % 30) as i64 + 1);
    let priority = (!index.is_multiple_of(11)).then_some((index % 4 + 1) as i64);
    let assignee = ASSIGNEES[index % ASSIGNEES.len()];
    let title = format!(
        "{} {}",
        TITLE_VERBS[index % TITLE_VERBS.len()],
        TITLE_OBJECTS[(index * 7) % TITLE_OBJECTS.len()]
    );
    let iteration = format!("Atlas\\2026\\Sprint {}", index % 12 + 1);
    let description = format!(
        "Demo work item {id}. This locally generated description exercises wrapping and scrolling in the ticket detail pane."
    );
    let created_at = created.format(&Rfc3339)?;
    let changed_at = changed.format(&Rfc3339)?;
    let web_url = format!("https://dev.azure.com/example-org/atlas/_workitems/edit/{id}");

    transaction.execute(
        "INSERT INTO work_items (
            organization, project, work_item_id, revision, work_item_type,
            title, state, reason, assigned_to, priority, area_path,
            iteration_path, tags, description, created_at, changed_at, web_url
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            "example-org",
            "atlas",
            id,
            index as i64 % 9 + 1,
            TYPES[index % TYPES.len()],
            title,
            state,
            reason,
            assignee,
            priority,
            AREAS[index % AREAS.len()],
            iteration,
            TAG_SETS[index % TAG_SETS.len()],
            description,
            created_at,
            changed_at,
            web_url,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn new_database_is_migrated_and_seeded_once() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");

        let opened = SqliteTicketRepository::open(&path).unwrap();
        assert!(opened.seeded_demo_data);
        assert_eq!(
            opened.repository.load_all().unwrap().len(),
            DEMO_TICKET_COUNT
        );
        drop(opened);

        let reopened = SqliteTicketRepository::open(&path).unwrap();
        assert!(!reopened.seeded_demo_data);
        assert_eq!(
            reopened.repository.load_all().unwrap().len(),
            DEMO_TICKET_COUNT
        );
    }

    #[test]
    fn existing_empty_database_is_migrated_but_not_seeded() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("empty.sqlite3");
        Connection::open(&path).unwrap();

        let opened = SqliteTicketRepository::open(&path).unwrap();

        assert!(!opened.seeded_demo_data);
        assert!(opened.repository.load_all().unwrap().is_empty());
    }

    #[test]
    fn newer_schema_version_is_rejected() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("future.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
        drop(connection);

        let error = SqliteTicketRepository::open(&path).unwrap_err();

        assert!(error.to_string().contains("newer than supported"));
    }
}
