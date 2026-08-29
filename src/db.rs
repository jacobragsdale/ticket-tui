use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use rusqlite::{Connection, Transaction, params};

use crate::model::{
    CommentRecord, HistoryRecord, RelationKind, RelationRecord, Ticket, TicketGraph, TicketKey,
};
use crate::timestamp::Timestamp;

const SCHEMA_VERSION: i64 = 5;

/// SQLite is a disposable cache of Azure DevOps, so any database that is not at
/// the current schema version is dropped and recreated instead of migrated.
const RESET_SCHEMA: &str = r"
DROP TABLE IF EXISTS work_items;
DROP TABLE IF EXISTS work_item_relations;
DROP TABLE IF EXISTS work_item_comments;
DROP TABLE IF EXISTS work_item_history;
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
CREATE TABLE work_item_relations (
    organization TEXT NOT NULL,
    from_id      INTEGER NOT NULL,
    to_id        INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    PRIMARY KEY (organization, from_id, to_id, kind)
);
CREATE TABLE work_item_comments (
    organization TEXT NOT NULL,
    work_item_id INTEGER NOT NULL,
    comment_id   INTEGER NOT NULL,
    created_at   TEXT NOT NULL,
    author       TEXT,
    body         TEXT NOT NULL,
    PRIMARY KEY (organization, work_item_id, comment_id)
);
CREATE TABLE work_item_history (
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
";

const CLEAR_CACHE: &str = "DELETE FROM work_items;
DELETE FROM work_item_relations;
DELETE FROM work_item_comments;
DELETE FROM work_item_history;";

#[derive(Debug)]
pub struct SqliteTicketRepository {
    connection: Connection,
    path: PathBuf,
}

impl SqliteTicketRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let connection = Connection::open(&path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;
        connection
            .busy_timeout(StdDuration::from_secs(3))
            .context("failed to configure SQLite busy timeout")?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
            .context("failed to configure SQLite")?;

        ensure_current_schema(&connection)?;

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
            let organization: String = row.get(0)?;
            let id: i64 = row.get(2)?;
            let raw_tags: String = row.get(12)?;
            let created_raw: String = row.get(14)?;
            let changed_raw: String = row.get(15)?;
            Ok(Ticket {
                key: TicketKey {
                    organization: organization.clone(),
                    id,
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
                created_at: parse_row_timestamp(created_raw, "created_at", &organization, id)?,
                changed_at: parse_row_timestamp(changed_raw, "changed_at", &organization, id)?,
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

    /// Replaces the cached work items and their graph with a freshly pulled set.
    pub fn replace_all(&mut self, tickets: &[Ticket], graph: &TicketGraph) -> Result<usize> {
        let transaction = self.connection.transaction()?;
        transaction
            .execute_batch(CLEAR_CACHE)
            .context("failed to clear the ticket cache")?;
        for ticket in tickets {
            insert_ticket(&transaction, ticket)?;
        }
        for relation in &graph.relations {
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
        for comment in &graph.comments {
            transaction.execute(
                "INSERT OR REPLACE INTO work_item_comments
                    (organization, work_item_id, comment_id, created_at, author, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    comment.ticket.organization,
                    comment.ticket.id,
                    comment.comment_id,
                    comment.created_at.to_rfc3339(),
                    comment.author,
                    comment.text
                ],
            )?;
        }
        for entry in &graph.history {
            transaction.execute(
                "INSERT OR REPLACE INTO work_item_history
                    (organization, work_item_id, revision, changed_at, changed_by,
                     field_name, old_value, new_value)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    entry.ticket.organization,
                    entry.ticket.id,
                    entry.revision,
                    entry.changed_at.to_rfc3339(),
                    entry.changed_by,
                    entry.field_name,
                    entry.old_value,
                    entry.new_value
                ],
            )?;
        }
        transaction.commit()?;
        Ok(tickets.len())
    }

    fn load_relations(&self) -> Result<Vec<RelationRecord>> {
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
        let mut statement = self.connection.prepare(
            "SELECT organization, work_item_id, comment_id, created_at, author, body
             FROM work_item_comments",
        )?;
        let rows = statement.query_map([], |row| {
            let organization: String = row.get(0)?;
            let id: i64 = row.get(1)?;
            let created_raw: String = row.get(3)?;
            Ok(CommentRecord {
                ticket: TicketKey {
                    organization: organization.clone(),
                    id,
                },
                comment_id: row.get(2)?,
                created_at: parse_row_timestamp(created_raw, "created_at", &organization, id)?,
                author: row.get(4)?,
                text: row.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load comments")
    }

    fn load_history(&self) -> Result<Vec<HistoryRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT organization, work_item_id, revision, changed_at, changed_by,
                    field_name, old_value, new_value
             FROM work_item_history",
        )?;
        let rows = statement.query_map([], |row| {
            let organization: String = row.get(0)?;
            let id: i64 = row.get(1)?;
            let changed_raw: String = row.get(3)?;
            Ok(HistoryRecord {
                ticket: TicketKey {
                    organization: organization.clone(),
                    id,
                },
                revision: row.get(2)?,
                changed_at: parse_row_timestamp(changed_raw, "changed_at", &organization, id)?,
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

fn schema_version(connection: &Connection) -> Result<i64> {
    Ok(connection.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

fn ensure_current_schema(connection: &Connection) -> Result<()> {
    if schema_version(connection)? == SCHEMA_VERSION {
        return Ok(());
    }
    connection
        .execute_batch(RESET_SCHEMA)
        .context("failed to rebuild the ticket cache schema")?;
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn parse_row_timestamp(
    raw: String,
    field: &'static str,
    organization: &str,
    id: i64,
) -> rusqlite::Result<Timestamp> {
    Timestamp::parse(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(InvalidTimestamp {
                organization: organization.to_owned(),
                id,
                field,
                source: error,
            }),
        )
    })
}

#[derive(Debug)]
struct InvalidTimestamp {
    organization: String,
    id: i64,
    field: &'static str,
    source: crate::timestamp::TimestampError,
}

impl std::fmt::Display for InvalidTimestamp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "work item {}/{} has an invalid {field} value: {source}",
            self.organization,
            self.id,
            field = self.field,
            source = self.source
        )
    }
}

impl std::error::Error for InvalidTimestamp {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn insert_ticket(transaction: &Transaction<'_>, ticket: &Ticket) -> Result<()> {
    transaction.execute(
        "INSERT OR REPLACE INTO work_items (
            organization, project, work_item_id, revision, work_item_type,
            title, state, reason, assigned_to, priority, area_path,
            iteration_path, tags, description, created_at, changed_at, web_url
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
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
            ticket.created_at.to_rfc3339(),
            ticket.changed_at.to_rfc3339(),
            ticket.web_url,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timestamp::ts;
    use tempfile::tempdir;

    fn ticket(id: i64) -> Ticket {
        Ticket {
            key: TicketKey {
                organization: "example-org".into(),
                id,
            },
            project: "atlas".into(),
            revision: 3,
            work_item_type: "Bug".into(),
            title: format!("Ticket {id}"),
            state: "Active".into(),
            reason: Some("Investigating".into()),
            assigned_to: Some("Avery Chen".into()),
            priority: Some(2),
            area_path: "Atlas\\Platform".into(),
            iteration_path: "Atlas\\2026\\Sprint 1".into(),
            tags: vec!["backend".into(), "rust".into()],
            description: "Cached from Azure DevOps.".into(),
            created_at: ts("2026-01-01T00:00:00Z"),
            changed_at: ts("2026-02-01T00:00:00Z"),
            web_url: format!("https://dev.azure.com/example-org/atlas/_workitems/edit/{id}"),
        }
    }

    #[test]
    fn open_creates_an_empty_cache_at_the_current_schema_version() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("nested").join("tickets.sqlite3");

        let repository = SqliteTicketRepository::open(&path).unwrap();

        assert!(repository.load_all().unwrap().is_empty());
        assert_eq!(repository.load_graph().unwrap(), TicketGraph::default());
        assert_eq!(
            schema_version(&repository.connection).unwrap(),
            SCHEMA_VERSION
        );
    }

    #[test]
    fn replace_all_round_trips_tickets_and_relations() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let mut repository = SqliteTicketRepository::open(&path).unwrap();
        let tickets = vec![ticket(1), ticket(2)];
        let graph = TicketGraph {
            relations: vec![RelationRecord {
                from: tickets[1].key.clone(),
                to: tickets[0].key.clone(),
                kind: RelationKind::Parent,
            }],
            comments: vec![CommentRecord {
                ticket: tickets[0].key.clone(),
                comment_id: 7,
                created_at: ts("2026-02-02T00:00:00Z"),
                author: Some("Jordan Patel".into()),
                text: "Looks good".into(),
            }],
            history: vec![HistoryRecord {
                ticket: tickets[0].key.clone(),
                revision: 2,
                changed_at: ts("2026-02-03T00:00:00Z"),
                changed_by: Some("Morgan Lee".into()),
                field_name: "State".into(),
                old_value: Some("New".into()),
                new_value: Some("Active".into()),
            }],
        };

        assert_eq!(repository.replace_all(&tickets, &graph).unwrap(), 2);

        let mut loaded = repository.load_all().unwrap();
        loaded.sort_by_key(|ticket| ticket.key.id);
        assert_eq!(loaded, tickets);
        assert_eq!(repository.load_graph().unwrap(), graph);

        let survivor = vec![ticket(3)];
        repository
            .replace_all(&survivor, &TicketGraph::default())
            .unwrap();
        assert_eq!(repository.load_all().unwrap(), survivor);
        assert_eq!(repository.load_graph().unwrap(), TicketGraph::default());
    }

    #[test]
    fn stale_schema_version_rebuilds_the_cache_instead_of_migrating() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stale.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE work_items (
                    organization TEXT NOT NULL,
                    work_item_id INTEGER NOT NULL,
                    title TEXT NOT NULL
                );
                INSERT INTO work_items VALUES ('example-org', 10001, 'Stale');
                PRAGMA user_version = 4;",
            )
            .unwrap();
        drop(connection);

        let repository = SqliteTicketRepository::open(&path).unwrap();

        assert!(repository.load_all().unwrap().is_empty());
        assert_eq!(
            schema_version(&repository.connection).unwrap(),
            SCHEMA_VERSION
        );
    }

    #[test]
    fn load_reports_invalid_timestamps_with_the_row_id() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("timestamps.sqlite3");
        let mut repository = SqliteTicketRepository::open(&path).unwrap();
        repository
            .replace_all(&[ticket(10_001)], &TicketGraph::default())
            .unwrap();
        repository
            .connection
            .execute(
                "UPDATE work_items SET changed_at = '2026-08-26T13:00:00-05:00'
                 WHERE work_item_id = 10001",
                [],
            )
            .unwrap();
        let tickets = repository.load_all().unwrap();
        assert_eq!(tickets[0].changed_at, ts("2026-08-26T18:00:00Z"));

        repository
            .connection
            .execute(
                "UPDATE work_items SET created_at = 'not-a-date' WHERE work_item_id = 10001",
                [],
            )
            .unwrap();

        let error = format!("{:#}", repository.load_all().unwrap_err());
        assert!(error.contains("10001"), "{error}");
        assert!(error.contains("created_at"), "{error}");
    }
}
