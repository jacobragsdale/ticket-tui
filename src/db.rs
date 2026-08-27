use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::import::ImportBatch;
use crate::model::{
    CommentRecord, HistoryRecord, RelationKind, RelationRecord, Ticket, TicketGraph, TicketKey,
};

const SCHEMA_VERSION: i64 = 2;
pub const DEMO_TICKET_COUNT: usize = 500;

const CREATE_WORK_ITEMS: &str = r#"
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

const CREATE_RELATED_TABLES: &str = r#"
CREATE TABLE IF NOT EXISTS work_item_relations (
    organization TEXT NOT NULL,
    from_id      INTEGER NOT NULL,
    to_id        INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    PRIMARY KEY (organization, from_id, to_id, kind)
);
CREATE TABLE IF NOT EXISTS work_item_comments (
    organization TEXT NOT NULL,
    work_item_id INTEGER NOT NULL,
    comment_id   INTEGER NOT NULL,
    created_at   TEXT NOT NULL,
    author       TEXT,
    body         TEXT NOT NULL,
    PRIMARY KEY (organization, work_item_id, comment_id)
);
CREATE TABLE IF NOT EXISTS work_item_history (
    organization TEXT NOT NULL,
    work_item_id INTEGER NOT NULL,
    revision     INTEGER NOT NULL,
    changed_at   TEXT NOT NULL,
    changed_by   TEXT,
    field_name   TEXT NOT NULL,
    old_value    TEXT,
    new_value    TEXT,
    PRIMARY KEY (organization, work_item_id, revision, field_name)
);
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

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            bail!("database {} does not exist", path.display());
        }
        let connection =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| format!("failed to open {} in read-only mode", path.display()))?;
        connection
            .busy_timeout(StdDuration::from_secs(3))
            .context("failed to configure SQLite busy timeout")?;
        validate_readable_schema(&connection)?;
        Ok(Self { connection, path })
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

    pub fn load_graph(&self) -> Result<TicketGraph> {
        Ok(TicketGraph {
            relations: self.load_relations()?,
            comments: self.load_comments()?,
            history: self.load_history()?,
        })
    }

    pub fn import_batch(&mut self, batch: &ImportBatch) -> Result<usize> {
        let transaction = self.connection.transaction()?;
        for ticket in &batch.tickets {
            upsert_ticket(&transaction, ticket)?;
        }
        for relation in &batch.relations {
            transaction.execute(
                "INSERT OR REPLACE INTO work_item_relations (organization, from_id, to_id, kind)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    relation.from.organization,
                    relation.from.id,
                    relation.to.id,
                    relation.kind.as_str()
                ],
            )?;
        }
        for comment in &batch.comments {
            transaction.execute(
                "INSERT OR REPLACE INTO work_item_comments
                    (organization, work_item_id, comment_id, created_at, author, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    comment.ticket.organization,
                    comment.ticket.id,
                    comment.comment_id,
                    comment.created_at,
                    comment.author,
                    comment.text
                ],
            )?;
        }
        for entry in &batch.history {
            transaction.execute(
                "INSERT OR REPLACE INTO work_item_history
                    (organization, work_item_id, revision, changed_at, changed_by,
                     field_name, old_value, new_value)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    entry.ticket.organization,
                    entry.ticket.id,
                    entry.revision,
                    entry.changed_at,
                    entry.changed_by,
                    entry.field_name,
                    entry.old_value,
                    entry.new_value
                ],
            )?;
        }
        transaction.commit()?;
        Ok(batch.tickets.len())
    }

    fn load_relations(&self) -> Result<Vec<RelationRecord>> {
        if !table_exists(&self.connection, "work_item_relations")? {
            return Ok(Vec::new());
        }
        let mut statement = self
            .connection
            .prepare("SELECT organization, from_id, to_id, kind FROM work_item_relations")?;
        let rows = statement.query_map([], |row| {
            let organization: String = row.get(0)?;
            let kind: String = row.get(3)?;
            Ok(RelationRecord {
                from: TicketKey {
                    organization: organization.clone(),
                    id: row.get(1)?,
                },
                to: TicketKey {
                    organization,
                    id: row.get(2)?,
                },
                kind: RelationKind::parse(&kind).unwrap_or(RelationKind::Related),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load relations")
    }

    fn load_comments(&self) -> Result<Vec<CommentRecord>> {
        if !table_exists(&self.connection, "work_item_comments")? {
            return Ok(Vec::new());
        }
        let mut statement = self.connection.prepare(
            "SELECT organization, work_item_id, comment_id, created_at, author, body
             FROM work_item_comments",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(CommentRecord {
                ticket: TicketKey {
                    organization: row.get(0)?,
                    id: row.get(1)?,
                },
                comment_id: row.get(2)?,
                created_at: row.get(3)?,
                author: row.get(4)?,
                text: row.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load comments")
    }

    fn load_history(&self) -> Result<Vec<HistoryRecord>> {
        if !table_exists(&self.connection, "work_item_history")? {
            return Ok(Vec::new());
        }
        let mut statement = self.connection.prepare(
            "SELECT organization, work_item_id, revision, changed_at, changed_by,
                    field_name, old_value, new_value
             FROM work_item_history",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(HistoryRecord {
                ticket: TicketKey {
                    organization: row.get(0)?,
                    id: row.get(1)?,
                },
                revision: row.get(2)?,
                changed_at: row.get(3)?,
                changed_by: row.get(4)?,
                field_name: row.get(5)?,
                old_value: row.get(6)?,
                new_value: row.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load history")
    }
}

#[must_use]
pub fn data_signature(path: &Path) -> u128 {
    let db = file_stamp(path);
    let wal = file_stamp(&wal_path(path));
    (u128::from(db) << 64) | u128::from(wal)
}

fn wal_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map_or_else(|| "tickets.sqlite3".into(), |name| name.to_os_string());
    file_name.push("-wal");
    path.with_file_name(file_name)
}

fn file_stamp(path: &Path) -> u64 {
    fs::metadata(path)
        .ok()
        .and_then(|meta| {
            let modified = meta.modified().ok()?;
            let elapsed = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
            Some(elapsed.as_millis() as u64 ^ meta.len().wrapping_mul(0x9E37_79B9))
        })
        .unwrap_or(0)
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
        connection.execute_batch(CREATE_WORK_ITEMS)?;
        connection.execute_batch(CREATE_RELATED_TABLES)?;
        connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        return Ok(());
    }
    if version == 1 {
        connection.execute_batch(CREATE_RELATED_TABLES)?;
        connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(())
}

fn validate_readable_schema(connection: &Connection) -> Result<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 0 {
        bail!("database has no ticket-tui schema; reopen without --read-only to migrate");
    }
    if version > SCHEMA_VERSION {
        bail!("database schema version {version} is newer than supported version {SCHEMA_VERSION}");
    }
    if version < SCHEMA_VERSION {
        bail!(
            "database schema version {version} needs migration to {SCHEMA_VERSION}; reopen without --read-only"
        );
    }
    if !table_exists(connection, "work_items")? {
        bail!("database is missing the work_items table");
    }
    Ok(())
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool> {
    let found: Option<String> = connection
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

fn upsert_ticket(transaction: &Transaction<'_>, ticket: &Ticket) -> Result<()> {
    transaction.execute(
        "INSERT INTO work_items (
            organization, project, work_item_id, revision, work_item_type,
            title, state, reason, assigned_to, priority, area_path,
            iteration_path, tags, description, created_at, changed_at, web_url
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT(organization, work_item_id) DO UPDATE SET
            project = excluded.project,
            revision = excluded.revision,
            work_item_type = excluded.work_item_type,
            title = excluded.title,
            state = excluded.state,
            reason = excluded.reason,
            assigned_to = excluded.assigned_to,
            priority = excluded.priority,
            area_path = excluded.area_path,
            iteration_path = excluded.iteration_path,
            tags = excluded.tags,
            description = excluded.description,
            created_at = excluded.created_at,
            changed_at = excluded.changed_at,
            web_url = excluded.web_url",
        params![
            ticket.key.organization,
            ticket.project,
            ticket.key.id,
            ticket.revision,
            ticket.work_item_type,
            ticket.title,
            ticket.state,
            ticket.reason,
            ticket.assigned_to,
            ticket.priority,
            ticket.area_path,
            ticket.iteration_path,
            ticket.tags.join(";"),
            ticket.description,
            ticket.created_at,
            ticket.changed_at,
            ticket.web_url,
        ],
    )?;
    Ok(())
}

fn seed_demo_data(connection: &mut Connection, count: usize) -> Result<()> {
    let transaction = connection.transaction()?;
    let anchor = OffsetDateTime::now_utc();
    for index in 0..count {
        insert_demo_ticket(&transaction, index, anchor)?;
    }
    seed_demo_graph(&transaction, count, anchor)?;
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

fn seed_demo_graph(
    transaction: &Transaction<'_>,
    count: usize,
    anchor: OffsetDateTime,
) -> Result<()> {
    for index in 0..count {
        let id = 10_001 + index as i64;
        if !index.is_multiple_of(5) {
            let parent = 10_001 + (index - index % 5) as i64;
            insert_relation(transaction, id, parent, "parent")?;
            insert_relation(transaction, parent, id, "child")?;
        }
        if index.is_multiple_of(11) && index + 1 < count {
            insert_relation(transaction, id, id + 1, "related")?;
            insert_relation(transaction, id + 1, id, "related")?;
        }
        if index.is_multiple_of(7) {
            let created = (anchor - Duration::days((index % 20) as i64 + 1)).format(&Rfc3339)?;
            transaction.execute(
                "INSERT INTO work_item_comments
                    (organization, work_item_id, comment_id, created_at, author, body)
                 VALUES ('example-org', ?1, 1, ?2, 'Avery Chen', ?3)",
                params![
                    id,
                    created,
                    format!(
                        "Demo comment on work item {id}. Stored locally and not edited by the TUI."
                    )
                ],
            )?;
            transaction.execute(
                "INSERT INTO work_item_history
                    (organization, work_item_id, revision, changed_at, changed_by,
                     field_name, old_value, new_value)
                 VALUES ('example-org', ?1, 2, ?2, 'Jordan Patel', 'State', 'New', 'Active')",
                params![id, created],
            )?;
        }
    }
    Ok(())
}

fn insert_relation(
    transaction: &Transaction<'_>,
    from_id: i64,
    to_id: i64,
    kind: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO work_item_relations (organization, from_id, to_id, kind)
         VALUES ('example-org', ?1, ?2, ?3)",
        params![from_id, to_id, kind],
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

    #[test]
    fn schema_v1_migrates_and_gains_relation_tables() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("v1.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE work_items (
                    organization TEXT NOT NULL,
                    project TEXT NOT NULL,
                    work_item_id INTEGER NOT NULL,
                    revision INTEGER NOT NULL,
                    work_item_type TEXT NOT NULL,
                    title TEXT NOT NULL,
                    state TEXT NOT NULL,
                    reason TEXT,
                    assigned_to TEXT,
                    priority INTEGER,
                    area_path TEXT NOT NULL,
                    iteration_path TEXT NOT NULL,
                    tags TEXT NOT NULL DEFAULT '',
                    description TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL,
                    changed_at TEXT NOT NULL,
                    web_url TEXT NOT NULL,
                    PRIMARY KEY (organization, work_item_id)
                );",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        drop(connection);

        let opened = SqliteTicketRepository::open(&path).unwrap();
        let graph = opened.repository.load_graph().unwrap();
        assert!(graph.relations.is_empty());
        assert!(table_exists(&opened.repository.connection, "work_item_relations").unwrap());
    }

    #[test]
    fn read_only_mode_opens_existing_files_and_rejects_missing_ones() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        SqliteTicketRepository::open(&path).unwrap();

        let repository = SqliteTicketRepository::open_read_only(&path).unwrap();
        assert_eq!(repository.load_all().unwrap().len(), DEMO_TICKET_COUNT);
        assert!(!repository.load_graph().unwrap().relations.is_empty());

        let missing = directory.path().join("missing").join("tickets.sqlite3");
        let error = SqliteTicketRepository::open_read_only(&missing).unwrap_err();
        assert!(error.to_string().contains("does not exist"));
        assert!(!missing.parent().unwrap().exists());
    }

    #[test]
    fn import_upserts_tickets_and_relations() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("import.sqlite3");
        let mut opened = SqliteTicketRepository::open(&path).unwrap();
        let batch = crate::import::parse_json(
            r#"[{"id":42,"organization":"example-org","project":"atlas","title":"Imported","type":"Bug","relations":[{"kind":"parent","id":10001}]}]"#,
        );
        assert_eq!(opened.repository.import_batch(&batch).unwrap(), 1);
        let tickets = opened.repository.load_all().unwrap();
        assert!(tickets.iter().any(|ticket| ticket.key.id == 42));
        let graph = opened.repository.load_graph().unwrap();
        assert!(
            graph
                .relations
                .iter()
                .any(|relation| relation.from.id == 42 && relation.to.id == 10_001)
        );
    }
}
