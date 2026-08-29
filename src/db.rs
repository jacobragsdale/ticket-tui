use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::slice;
use std::time::Duration as StdDuration;

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::classification::{ClassificationNode, NodeKind};
use crate::model::{
    CommentRecord, DetailsUpdate, HistoryRecord, Identity, Pipeline, PrBuild, PrReviewer, PrStatus,
    PullRequest, RelationKind, RelationRecord, Repo, Run, RunResult, RunStatus, StateCatalog,
    StateCategory, StateOption, Ticket, TicketGraph, TicketKey,
};
use crate::timestamp::Timestamp;

const SCHEMA_VERSION: i64 = 15;

/// `sync_meta` key holding the display name of the signed-in Azure DevOps user.
pub const ME_DISPLAY_NAME_KEY: &str = "me_display_name";

/// `sync_meta` key holding the signed-in user's own id, which is what a vote
/// on a pull request is written under. Read once and kept: the work-item
/// endpoints never report it.
pub const ME_ID_KEY: &str = "me_id";

/// `sync_meta` key holding the greatest `System.ChangedDate` the last
/// successful pull brought back, which is where the next incremental pull
/// starts asking. It is never a wall clock reading: client clocks skew, Azure
/// DevOps's own timestamps do not.
pub const WATERMARK_KEY: &str = "watermark_changed_at";

/// `sync_meta` key holding the Azure DevOps organization the stored work items
/// were pulled from, written by every successful pull. A run resolving another
/// organization refuses to sync into the database rather than replacing rows it
/// did not fetch.
pub const ORGANIZATION_KEY: &str = "organization";

/// `sync_meta` key holding the Azure DevOps project the stored work items were
/// pulled from, the other half of [`ORGANIZATION_KEY`].
pub const PROJECT_KEY: &str = "project";

/// `sync_meta` key holding the extra WIQL condition the last pull narrowed the
/// project with, empty for a database holding the whole project. A pull whose
/// scope no longer matches this runs in full, because widening it has to bring
/// work items in and narrowing it has to drop the ones now outside.
pub const SYNC_SCOPE_KEY: &str = "sync_scope";

/// `sync_meta` key holding the project's GUID, which the pull request policy
/// and artifact-link endpoints ask for by id rather than by name.
pub const PROJECT_ID_KEY: &str = "project_id";

/// `sync_meta` key holding when the project's classification nodes were last
/// read. The iteration and area pickers open from the cached trees and only ask
/// Azure DevOps again once this is an hour old, so a run that follows another
/// closely never touches the network for them at all.
pub const CLASSIFICATION_FETCHED_KEY: &str = "classification_nodes_fetched_at";

/// Azure DevOps is the source of truth and a full pull rebuilds every row, so a
/// database at another schema version is dropped and recreated rather than
/// migrated; the pull that follows refills it. The `sync_meta` settings go
/// with it, which is what makes that pull a full one.
const RESET_SCHEMA: &str = r"
DROP TABLE IF EXISTS work_items;
DROP TABLE IF EXISTS work_item_relations;
DROP TABLE IF EXISTS work_item_comments;
DROP TABLE IF EXISTS work_item_history;
DROP TABLE IF EXISTS work_item_type_states;
DROP TABLE IF EXISTS work_item_types;
DROP TABLE IF EXISTS identities;
DROP TABLE IF EXISTS classification_nodes;
DROP TABLE IF EXISTS repos;
DROP TABLE IF EXISTS pipelines;
DROP TABLE IF EXISTS runs;
DROP TABLE IF EXISTS pull_requests;
DROP TABLE IF EXISTS pr_reviewers;
DROP TABLE IF EXISTS pr_work_items;
DROP TABLE IF EXISTS sync_meta;
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
    description_html TEXT NOT NULL DEFAULT '',
    created_at     TEXT NOT NULL,
    changed_at     TEXT NOT NULL,
    web_url        TEXT NOT NULL,
    details_rev    INTEGER NOT NULL DEFAULT 0,
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
CREATE TABLE work_item_type_states (
    work_item_type TEXT NOT NULL,
    name           TEXT NOT NULL,
    category       TEXT NOT NULL,
    position       INTEGER NOT NULL,
    PRIMARY KEY (work_item_type, name)
);
CREATE TABLE work_item_types (
    name     TEXT PRIMARY KEY,
    position INTEGER NOT NULL
);
CREATE TABLE identities (
    display_name TEXT PRIMARY KEY,
    unique_name  TEXT
);
CREATE TABLE classification_nodes (
    kind        TEXT NOT NULL,
    path        TEXT NOT NULL,
    depth       INTEGER NOT NULL,
    start_date  TEXT,
    finish_date TEXT,
    position    INTEGER NOT NULL,
    PRIMARY KEY (kind, path)
);
CREATE TABLE repos (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    project        TEXT NOT NULL,
    default_branch TEXT,
    remote_url     TEXT NOT NULL,
    ssh_url        TEXT NOT NULL,
    web_url        TEXT NOT NULL,
    is_disabled    INTEGER NOT NULL,
    size           INTEGER
);
CREATE TABLE pipelines (
    id             INTEGER PRIMARY KEY,
    name           TEXT NOT NULL,
    folder         TEXT NOT NULL,
    repo_id        TEXT,
    default_branch TEXT,
    url            TEXT NOT NULL,
    queue_status   TEXT NOT NULL
);
CREATE TABLE runs (
    id             INTEGER PRIMARY KEY,
    pipeline_id    INTEGER NOT NULL,
    build_number   TEXT NOT NULL,
    status         TEXT NOT NULL,
    result         TEXT,
    source_branch  TEXT NOT NULL,
    source_version TEXT NOT NULL,
    requested_for  TEXT,
    reason         TEXT NOT NULL,
    pr_id          INTEGER,
    queue_time     TEXT,
    start_time     TEXT,
    finish_time    TEXT,
    url            TEXT NOT NULL
);
CREATE INDEX runs_by_pipeline ON runs (pipeline_id, queue_time DESC);
CREATE TABLE pull_requests (
    id                       INTEGER PRIMARY KEY,
    repo_id                  TEXT NOT NULL,
    title                    TEXT NOT NULL,
    description              TEXT NOT NULL,
    status                   TEXT NOT NULL,
    is_draft                 INTEGER NOT NULL,
    created_by               TEXT NOT NULL,
    created_by_unique        TEXT,
    created_at               TEXT,
    closed_at                TEXT,
    source_ref               TEXT NOT NULL,
    target_ref               TEXT NOT NULL,
    merge_status             TEXT NOT NULL,
    last_merge_source_commit TEXT NOT NULL,
    auto_complete_set_by     TEXT,
    url                      TEXT NOT NULL,
    build_status             TEXT,
    build_run_id             INTEGER
);
CREATE TABLE pr_reviewers (
    pull_request_id INTEGER NOT NULL,
    reviewer_id     TEXT NOT NULL,
    display_name    TEXT NOT NULL,
    unique_name     TEXT,
    vote            INTEGER NOT NULL,
    is_required     INTEGER NOT NULL,
    position        INTEGER NOT NULL,
    PRIMARY KEY (pull_request_id, reviewer_id)
);
CREATE TABLE pr_work_items (
    pull_request_id INTEGER NOT NULL,
    work_item_id    INTEGER NOT NULL,
    PRIMARY KEY (pull_request_id, work_item_id)
);
CREATE TABLE sync_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// `sync_meta`, `work_item_type_states`, `work_item_types`, `identities`, and
/// `classification_nodes` are deliberately absent: they describe the sync, the
/// project's process, the people in it, and the trees its work is planned into,
/// not the work items a pull replaces.
const CLEAR_CACHE: &str = "DELETE FROM work_items;
DELETE FROM work_item_relations;
DELETE FROM work_item_comments;
DELETE FROM work_item_history;";

#[derive(Debug)]
pub struct SqliteTicketRepository {
    connection: Connection,
    path: PathBuf,
    schema_rebuilt: bool,
}

impl SqliteTicketRepository {
    /// Opens the database, creating it — directory and all — when it is not
    /// there, and rebuilding its schema when it is at another version.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let connection = connect(&path)?;
        let schema_rebuilt = ensure_current_schema(&connection)?;
        Ok(Self {
            connection,
            path,
            schema_rebuilt,
        })
    }

    /// Opens an existing database without touching its schema. Background
    /// reloads, the sync worker, and the subcommands use this so an older
    /// running instance can never rebuild (and empty) a database that a newer
    /// build owns; a version mismatch is an error instead.
    pub fn open_existing(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let connection = connect(&path)?;
        let version = schema_version(&connection)?;
        if version != SCHEMA_VERSION {
            bail!(
                "ticket cache schema is version {version} but this build expects {SCHEMA_VERSION}; restart ticket-tui"
            );
        }
        Ok(Self {
            connection,
            path,
            schema_rebuilt: false,
        })
    }

    /// Whether [`Self::open`] dropped and recreated the tables because the file
    /// was at another schema version (a fresh file counts). The rows are gone,
    /// so the caller pulls from Azure DevOps straight away.
    #[must_use]
    pub const fn schema_was_rebuilt(&self) -> bool {
        self.schema_rebuilt
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_all(&self) -> Result<Vec<Ticket>> {
        let mut statement = self.connection.prepare(
            "SELECT organization, project, work_item_id, revision, work_item_type,
                    title, state, reason, assigned_to, priority, area_path,
                    iteration_path, tags, description, created_at, changed_at, web_url,
                    details_rev, description_html
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
                description_html: row.get(18)?,
                created_at: parse_row_timestamp(created_raw, "created_at", &organization, id)?,
                changed_at: parse_row_timestamp(changed_raw, "changed_at", &organization, id)?,
                web_url: row.get(16)?,
                details_rev: row.get(17)?,
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

    /// Records one fact about the cache, such as who ran the last sync.
    pub fn set_meta(&mut self, key: &str, value: &str) -> Result<()> {
        self.connection
            .execute(
                "INSERT OR REPLACE INTO sync_meta (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .with_context(|| format!("failed to store the {key} cache setting"))?;
        Ok(())
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT value FROM sync_meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .with_context(|| format!("failed to read the {key} cache setting"))
    }

    /// Records the states one work item type allows, in the order Azure DevOps
    /// listed them. A type is written whole, so a state retired in the process
    /// template stops being offered.
    pub fn replace_type_states(
        &mut self,
        work_item_type: &str,
        states: &[StateOption],
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM work_item_type_states WHERE work_item_type = ?1",
            params![work_item_type],
        )?;
        for (position, state) in states.iter().enumerate() {
            transaction.execute(
                "INSERT OR REPLACE INTO work_item_type_states
                    (work_item_type, name, category, position)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    work_item_type,
                    state.name,
                    state.category.as_str(),
                    i64::try_from(position).unwrap_or(i64::MAX)
                ],
            )?;
        }
        transaction
            .commit()
            .with_context(|| format!("failed to store the {work_item_type} states"))
    }

    /// Every work item type's states, each in the order they were stored.
    pub fn load_type_states(&self) -> Result<StateCatalog> {
        let mut statement = self.connection.prepare(
            "SELECT work_item_type, name, category FROM work_item_type_states
             ORDER BY work_item_type, position",
        )?;
        let rows = statement.query_map([], |row| {
            let work_item_type: String = row.get(0)?;
            let name: String = row.get(1)?;
            let category: String = row.get(2)?;
            Ok((work_item_type, name, category))
        })?;
        let mut grouped: Vec<(String, Vec<StateOption>)> = Vec::new();
        for row in rows {
            let (work_item_type, name, category) = row.context("failed to load type states")?;
            let option = StateOption::new(name, StateCategory::parse(&category));
            match grouped.last_mut() {
                Some((current, states)) if *current == work_item_type => states.push(option),
                _ => grouped.push((work_item_type, vec![option])),
            }
        }
        let mut catalog = StateCatalog::default();
        for (work_item_type, states) in grouped {
            catalog.insert(work_item_type, states);
        }
        Ok(catalog)
    }

    /// Records the work item types the project's process offers, in the order
    /// Azure DevOps listed them. The list is written whole, so a type retired
    /// from the process stops being offered.
    pub fn replace_work_item_types(&mut self, types: &[String]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM work_item_types", [])?;
        for (position, name) in types.iter().enumerate() {
            transaction.execute(
                "INSERT OR REPLACE INTO work_item_types (name, position) VALUES (?1, ?2)",
                params![name, i64::try_from(position).unwrap_or(i64::MAX)],
            )?;
        }
        transaction
            .commit()
            .context("failed to store the project's work item types")
    }

    /// Every work item type the last fetch found, in the order it found them.
    pub fn load_work_item_types(&self) -> Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT name FROM work_item_types ORDER BY position")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load work item types")
    }

    /// Records the people the project's teams hold, so the assignee picker can
    /// offer somebody who has never been assigned a work item. The list is
    /// written whole, so somebody who left the project stops being offered.
    pub fn replace_identities(&mut self, identities: &[Identity]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM identities", [])?;
        for identity in identities {
            transaction.execute(
                "INSERT OR REPLACE INTO identities (display_name, unique_name)
                 VALUES (?1, ?2)",
                params![identity.display_name, identity.unique_name],
            )?;
        }
        transaction
            .commit()
            .context("failed to store the project's identities")
    }

    /// Replaces the project's repositories with what the pull found. Answers
    /// whether anything changed, so an idle project writes nothing.
    pub fn replace_repos(&mut self, repos: &[Repo]) -> Result<bool> {
        // Compared in the order they are read back in, so a source that lists
        // them another way is still recognised as the same set.
        let mut repos = repos.to_vec();
        repos.sort_by_key(|repo| repo.name.to_lowercase());
        if self.load_repos()? == repos {
            return Ok(false);
        }
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM repos", [])?;
        for repo in &repos {
            transaction.execute(
                "INSERT OR REPLACE INTO repos
                 (id, name, project, default_branch, remote_url, ssh_url, web_url, is_disabled, size)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    repo.id,
                    repo.name,
                    repo.project,
                    repo.default_branch,
                    repo.remote_url,
                    repo.ssh_url,
                    repo.web_url,
                    i64::from(repo.is_disabled),
                    repo.size,
                ],
            )?;
        }
        transaction
            .commit()
            .context("failed to store the project's repositories")?;
        Ok(true)
    }

    /// The project's repositories, by name.
    pub fn load_repos(&self) -> Result<Vec<Repo>> {
        let mut statement = self.connection.prepare(
            "SELECT id, name, project, default_branch, remote_url, ssh_url, web_url,
                    is_disabled, size
             FROM repos ORDER BY name COLLATE NOCASE",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Repo {
                id: row.get(0)?,
                name: row.get(1)?,
                project: row.get(2)?,
                default_branch: row.get(3)?,
                remote_url: row.get(4)?,
                ssh_url: row.get(5)?,
                web_url: row.get(6)?,
                is_disabled: row.get::<_, i64>(7)? != 0,
                size: row.get(8)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load repositories")
    }

    /// Replaces the project's pipelines with what the pull found, and answers
    /// whether anything changed.
    pub fn replace_pipelines(&mut self, pipelines: &[Pipeline]) -> Result<bool> {
        let mut pipelines = pipelines.to_vec();
        pipelines.sort_by_key(|pipeline| pipeline.id);
        if self.load_pipelines()? == pipelines {
            return Ok(false);
        }
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM pipelines", [])?;
        for pipeline in &pipelines {
            transaction.execute(
                "INSERT OR REPLACE INTO pipelines
                 (id, name, folder, repo_id, default_branch, url, queue_status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    pipeline.id,
                    pipeline.name,
                    pipeline.folder,
                    pipeline.repo_id,
                    pipeline.default_branch,
                    pipeline.url,
                    pipeline.queue_status,
                ],
            )?;
        }
        transaction
            .commit()
            .context("failed to store the project's pipelines")?;
        Ok(true)
    }

    /// The project's pipelines, by id.
    pub fn load_pipelines(&self) -> Result<Vec<Pipeline>> {
        let mut statement = self.connection.prepare(
            "SELECT id, name, folder, repo_id, default_branch, url, queue_status
             FROM pipelines ORDER BY id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Pipeline {
                id: row.get(0)?,
                name: row.get(1)?,
                folder: row.get(2)?,
                repo_id: row.get(3)?,
                default_branch: row.get(4)?,
                url: row.get(5)?,
                queue_status: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load pipelines")
    }

    /// Replaces the stored runs with the window the pull brought back, which
    /// is what prunes everything older than it: the table never grows past
    /// what one query answers with. Answers whether anything changed.
    pub fn replace_runs(&mut self, runs: &[Run]) -> Result<bool> {
        let mut runs = runs.to_vec();
        runs.sort_by_key(|run| std::cmp::Reverse(run.id));
        if self.load_runs()? == runs {
            return Ok(false);
        }
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM runs", [])?;
        for run in &runs {
            transaction.execute(
                "INSERT OR REPLACE INTO runs
                 (id, pipeline_id, build_number, status, result, source_branch, source_version,
                  requested_for, reason, pr_id, queue_time, start_time, finish_time, url)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    run.id,
                    run.pipeline_id,
                    run.build_number,
                    run.status.as_str(),
                    run.result.map(RunResult::as_str),
                    run.source_branch,
                    run.source_version,
                    run.requested_for,
                    run.reason,
                    run.pr_id,
                    run.queue_time.map(|time| time.to_rfc3339()),
                    run.start_time.map(|time| time.to_rfc3339()),
                    run.finish_time.map(|time| time.to_rfc3339()),
                    run.url,
                ],
            )?;
        }
        transaction
            .commit()
            .context("failed to store the project's pipeline runs")?;
        Ok(true)
    }

    /// The stored runs, newest first.
    pub fn load_runs(&self) -> Result<Vec<Run>> {
        let mut statement = self.connection.prepare(
            "SELECT id, pipeline_id, build_number, status, result, source_branch, source_version,
                    requested_for, reason, pr_id, queue_time, start_time, finish_time, url
             FROM runs ORDER BY id DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Run {
                id: row.get(0)?,
                pipeline_id: row.get(1)?,
                build_number: row.get(2)?,
                status: RunStatus::parse(&row.get::<_, String>(3)?),
                result: row
                    .get::<_, Option<String>>(4)?
                    .as_deref()
                    .and_then(RunResult::parse),
                source_branch: row.get(5)?,
                source_version: row.get(6)?,
                requested_for: row.get(7)?,
                reason: row.get(8)?,
                pr_id: row.get(9)?,
                queue_time: row
                    .get::<_, Option<String>>(10)?
                    .as_deref()
                    .and_then(|raw| Timestamp::parse(raw).ok()),
                start_time: row
                    .get::<_, Option<String>>(11)?
                    .as_deref()
                    .and_then(|raw| Timestamp::parse(raw).ok()),
                finish_time: row
                    .get::<_, Option<String>>(12)?
                    .as_deref()
                    .and_then(|raw| Timestamp::parse(raw).ok()),
                url: row.get(13)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load pipeline runs")
    }

    /// Replaces the stored pull requests, their reviewers and their work-item
    /// links with what the pull found. Answers whether anything changed.
    pub fn replace_pull_requests(&mut self, requests: &[PullRequest]) -> Result<bool> {
        let mut requests = requests.to_vec();
        requests.sort_by_key(|request| std::cmp::Reverse(request.id));
        if self.load_pull_requests()? == requests {
            return Ok(false);
        }
        let transaction = self.connection.transaction()?;
        for table in ["pull_requests", "pr_reviewers", "pr_work_items"] {
            transaction.execute(&format!("DELETE FROM {table}"), [])?;
        }
        for request in &requests {
            transaction.execute(
                "INSERT OR REPLACE INTO pull_requests
                 (id, repo_id, title, description, status, is_draft, created_by,
                  created_by_unique, created_at, closed_at, source_ref, target_ref,
                  merge_status, last_merge_source_commit, auto_complete_set_by, url,
                  build_status, build_run_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16, ?17, ?18)",
                params![
                    request.id,
                    request.repo_id,
                    request.title,
                    request.description,
                    request.status.as_str(),
                    i64::from(request.is_draft),
                    request.created_by.display_name,
                    request.created_by.unique_name,
                    request.created_at.map(|at| at.to_rfc3339()),
                    request.closed_at.map(|at| at.to_rfc3339()),
                    request.source_ref,
                    request.target_ref,
                    request.merge_status,
                    request.last_merge_source_commit,
                    request.auto_complete_set_by,
                    request.url,
                    request.build.as_ref().map(|build| build.status.clone()),
                    request.build.as_ref().and_then(|build| build.run_id),
                ],
            )?;
            for (position, reviewer) in request.reviewers.iter().enumerate() {
                transaction.execute(
                    "INSERT OR REPLACE INTO pr_reviewers
                     (pull_request_id, reviewer_id, display_name, unique_name, vote,
                      is_required, position)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        request.id,
                        reviewer.id,
                        reviewer.display_name,
                        reviewer.unique_name,
                        i64::from(reviewer.vote),
                        i64::from(reviewer.is_required),
                        i64::try_from(position).unwrap_or_default(),
                    ],
                )?;
            }
            for work_item in &request.work_items {
                transaction.execute(
                    "INSERT OR REPLACE INTO pr_work_items (pull_request_id, work_item_id)
                     VALUES (?1, ?2)",
                    params![request.id, work_item],
                )?;
            }
        }
        transaction
            .commit()
            .context("failed to store the project's pull requests")?;
        Ok(true)
    }

    /// The stored pull requests, newest first, with their reviewers and links.
    pub fn load_pull_requests(&self) -> Result<Vec<PullRequest>> {
        let mut statement = self.connection.prepare(
            "SELECT id, repo_id, title, description, status, is_draft, created_by,
                    created_by_unique, created_at, closed_at, source_ref, target_ref,
                    merge_status, last_merge_source_commit, auto_complete_set_by, url,
                    build_status, build_run_id
             FROM pull_requests ORDER BY id DESC",
        )?;
        let rows = statement.query_map([], |row| {
            let build_status: Option<String> = row.get(16)?;
            Ok(PullRequest {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                title: row.get(2)?,
                description: row.get(3)?,
                status: PrStatus::parse(&row.get::<_, String>(4)?),
                is_draft: row.get::<_, i64>(5)? != 0,
                created_by: Identity::new(row.get::<_, String>(6)?, row.get(7)?),
                created_at: row
                    .get::<_, Option<String>>(8)?
                    .as_deref()
                    .and_then(|raw| Timestamp::parse(raw).ok()),
                closed_at: row
                    .get::<_, Option<String>>(9)?
                    .as_deref()
                    .and_then(|raw| Timestamp::parse(raw).ok()),
                source_ref: row.get(10)?,
                target_ref: row.get(11)?,
                merge_status: row.get(12)?,
                last_merge_source_commit: row.get(13)?,
                auto_complete_set_by: row.get(14)?,
                url: row.get(15)?,
                build: build_status.map(|status| PrBuild {
                    status,
                    run_id: row.get(17).ok().flatten(),
                }),
                reviewers: Vec::new(),
                work_items: Vec::new(),
            })
        })?;
        let mut requests = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        for request in &mut requests {
            request.reviewers = self.load_pr_reviewers(request.id)?;
            request.work_items = self.load_pr_work_items(request.id)?;
        }
        Ok(requests)
    }

    fn load_pr_reviewers(&self, pull_request: i64) -> Result<Vec<PrReviewer>> {
        let mut statement = self.connection.prepare(
            "SELECT reviewer_id, display_name, unique_name, vote, is_required
             FROM pr_reviewers WHERE pull_request_id = ?1 ORDER BY position",
        )?;
        let rows = statement.query_map(params![pull_request], |row| {
            Ok(PrReviewer {
                id: row.get(0)?,
                display_name: row.get(1)?,
                unique_name: row.get(2)?,
                vote: i8::try_from(row.get::<_, i64>(3)?).unwrap_or_default(),
                is_required: row.get::<_, i64>(4)? != 0,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load pull request reviewers")
    }

    fn load_pr_work_items(&self, pull_request: i64) -> Result<Vec<i64>> {
        let mut statement = self.connection.prepare(
            "SELECT work_item_id FROM pr_work_items WHERE pull_request_id = ?1
             ORDER BY work_item_id",
        )?;
        let rows = statement.query_map(params![pull_request], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load pull request work items")
    }

    /// Everybody the last identity fetch found, by display name.
    pub fn load_identities(&self) -> Result<Vec<Identity>> {
        let mut statement = self
            .connection
            .prepare("SELECT display_name, unique_name FROM identities ORDER BY display_name")?;
        let rows = statement.query_map([], |row| {
            Ok(Identity::new(row.get::<_, String>(0)?, row.get(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load identities")
    }

    /// Records both classification trees as the last fetch flattened them, in
    /// that order, so the pickers redraw the tree without walking it again.
    /// They are written whole, so a sprint deleted from the process stops being
    /// offered.
    pub fn replace_classification_nodes(&mut self, nodes: &[ClassificationNode]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM classification_nodes", [])?;
        for (position, node) in nodes.iter().enumerate() {
            transaction.execute(
                "INSERT OR REPLACE INTO classification_nodes
                    (kind, path, depth, start_date, finish_date, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    node.kind.as_str(),
                    node.path,
                    i64::try_from(node.depth).unwrap_or(i64::MAX),
                    node.start_date.map(Timestamp::to_rfc3339),
                    node.finish_date.map(Timestamp::to_rfc3339),
                    i64::try_from(position).unwrap_or(i64::MAX)
                ],
            )?;
        }
        transaction
            .commit()
            .context("failed to store the project's classification nodes")
    }

    /// Both trees as the last fetch flattened them, in that order.
    pub fn load_classification_nodes(&self) -> Result<Vec<ClassificationNode>> {
        let mut statement = self.connection.prepare(
            "SELECT kind, path, depth, start_date, finish_date FROM classification_nodes
             ORDER BY position",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut nodes = Vec::new();
        for row in rows {
            let (kind, path, depth, start, finish) =
                row.context("failed to load the classification nodes")?;
            // A kind the current build does not know is a row from another
            // schema, which is no use to either picker.
            let Some(kind) = NodeKind::parse(&kind) else {
                continue;
            };
            nodes.push(ClassificationNode {
                kind,
                path,
                depth: usize::try_from(depth).unwrap_or_default(),
                start_date: start.as_deref().and_then(|raw| Timestamp::parse(raw).ok()),
                finish_date: finish.as_deref().and_then(|raw| Timestamp::parse(raw).ok()),
            });
        }
        Ok(nodes)
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
            insert_relation(&transaction, relation)?;
        }
        for comment in &graph.comments {
            insert_comment(&transaction, comment)?;
        }
        for entry in &graph.history {
            insert_history(&transaction, entry)?;
        }
        transaction.commit()?;
        Ok(tickets.len())
    }

    /// Writes one work item and the links leading out of it, leaving every
    /// other row alone. An edit that Azure DevOps accepted lands this way: it
    /// changed one record, so replacing the whole database would throw away
    /// everything else the last pull brought.
    pub fn upsert(&mut self, ticket: &Ticket, relations: &[RelationRecord]) -> Result<()> {
        self.write_upserts(slice::from_ref(ticket), relations, &[])
            .with_context(|| format!("failed to store work item {}", ticket.key.id))
    }

    /// Writes one work item that has just been moved between parents, and the
    /// links leading out of it, leaving every other row alone.
    ///
    /// A hierarchy link is stored twice — once from the child, once from the
    /// parent — and Azure DevOps answers a move with the child alone, so the
    /// parent it left is never mentioned in `relations`. Clearing the child
    /// links pointing at it is what takes it out of the old family; the child's
    /// own parent link, which `relations` carries, is what puts it in the new
    /// one. Both happen in the same transaction, so no reader ever sees the
    /// work item in two families or in none.
    pub fn reparent(&mut self, ticket: &Ticket, relations: &[RelationRecord]) -> Result<()> {
        self.write_reparent(ticket, relations)
            .with_context(|| format!("failed to move work item {}", ticket.key.id))
    }

    fn write_reparent(&mut self, ticket: &Ticket, relations: &[RelationRecord]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        insert_ticket(&transaction, ticket)?;
        transaction
            .execute(
                "DELETE FROM work_item_relations
                 WHERE organization = ?1 AND (from_id = ?2 OR (to_id = ?2 AND kind = ?3))",
                params![
                    ticket.key.organization,
                    ticket.key.id,
                    RelationKind::Child.as_str()
                ],
            )
            .context("failed to clear the work item's hierarchy links")?;
        for relation in relations {
            insert_relation(&transaction, relation)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Writes a batch of work items, the links leading out of them, and the
    /// comments and history read for them, in one transaction, leaving every
    /// other row alone. This is how an incremental pull lands: only what
    /// changed is rewritten, so the rows nobody touched keep the bytes they
    /// already had, and a work item's rows and its discussion never disagree
    /// about which revision they came from.
    pub fn upsert_all(
        &mut self,
        tickets: &[Ticket],
        relations: &[RelationRecord],
        details: &[DetailsUpdate],
    ) -> Result<()> {
        self.write_upserts(tickets, relations, details)
            .with_context(|| format!("failed to store {} changed work items", tickets.len()))
    }

    /// Replaces the comments and revision history of work items whose rows are
    /// already stored, which is how a details fetch the selection asked for
    /// lands. Nothing at all is written for an empty batch.
    pub fn replace_details(&mut self, details: &[DetailsUpdate]) -> Result<()> {
        if details.is_empty() {
            return Ok(());
        }
        let transaction = self.connection.transaction()?;
        write_details(&transaction, details)?;
        transaction
            .commit()
            .context("failed to store work item comments and history")
    }

    /// Writes one comment somebody just left, on its own, leaving every other
    /// row alone. `details_rev` is deliberately not moved: the stored details
    /// are still the ones the last fetch read, so the next fetch is free to
    /// read the discussion again and settle it against the server.
    pub fn insert_comment(&mut self, comment: &CommentRecord) -> Result<()> {
        let transaction = self.connection.transaction()?;
        insert_comment(&transaction, comment)?;
        transaction.commit().with_context(|| {
            format!(
                "failed to store the comment on work item {}",
                comment.ticket.id
            )
        })
    }

    /// The stored revision of one work item, which is the revision a details
    /// fetch records against it. `None` when the row is gone, which is what a
    /// fetch racing a deletion sees.
    pub fn revision_of(&self, key: &TicketKey) -> Result<Option<i64>> {
        self.connection
            .query_row(
                "SELECT revision FROM work_items WHERE organization = ?1 AND work_item_id = ?2",
                params![key.organization, key.id],
                |row| row.get(0),
            )
            .optional()
            .with_context(|| format!("failed to read the revision of work item {}", key.id))
    }

    /// Every work item type the database already knows the states of, so a pull
    /// can skip asking Azure DevOps for them again.
    pub fn cached_state_types(&self) -> Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT DISTINCT work_item_type FROM work_item_type_states")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load the cached work item types")
    }

    /// Removes every work item the project no longer lists, along with the
    /// links, comments, and history hanging off it. `live_ids` is the complete
    /// id list the project's own query answered with, so a work item moved to
    /// the recycle bin — which vanishes from WIQL rather than being marked —
    /// is caught here. Nothing at all is written when nothing is missing, so an
    /// idle pull leaves the file's signature where it was.
    pub fn delete_missing(&mut self, live_ids: &[i64]) -> Result<usize> {
        let live: HashSet<i64> = live_ids.iter().copied().collect();
        let mut statement = self
            .connection
            .prepare("SELECT organization, work_item_id FROM work_items")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let missing: Vec<(String, i64)> = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to list the cached work items")?
            .into_iter()
            .filter(|(_, id)| !live.contains(id))
            .collect();
        drop(statement);
        if missing.is_empty() {
            return Ok(0);
        }

        let transaction = self.connection.transaction()?;
        for (organization, id) in &missing {
            forget_work_item(&transaction, organization, *id)?;
        }
        transaction
            .commit()
            .context("failed to remove the work items the project no longer has")?;
        Ok(missing.len())
    }

    /// Removes one work item and everything hanging off it, for a work item the
    /// user has just sent to the recycle bin. The next pull would catch it
    /// anyway — a deleted work item vanishes from WIQL, which is what
    /// [`Self::delete_missing`] reads — but the row has to leave the table now,
    /// and the file has to agree with the table.
    ///
    /// Links are dropped in both directions, so the children left behind stop
    /// claiming a parent that is gone. They are not deleted with it: a soft
    /// delete takes the one work item, and its children become work items
    /// nobody has broken down from.
    pub fn delete_work_item(&mut self, key: &TicketKey) -> Result<()> {
        let transaction = self.connection.transaction()?;
        forget_work_item(&transaction, &key.organization, key.id)?;
        transaction
            .commit()
            .with_context(|| format!("failed to remove #{} from the database", key.id))
    }

    /// One transaction holding every work item in `tickets`, the links leading
    /// out of each of them, and the details read for any of them, replacing
    /// whatever those work items linked to and said before.
    fn write_upserts(
        &mut self,
        tickets: &[Ticket],
        relations: &[RelationRecord],
        details: &[DetailsUpdate],
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;
        for ticket in tickets {
            insert_ticket(&transaction, ticket)?;
            transaction
                .execute(
                    "DELETE FROM work_item_relations WHERE organization = ?1 AND from_id = ?2",
                    params![ticket.key.organization, ticket.key.id],
                )
                .context("failed to clear the work item's relations")?;
        }
        for relation in relations {
            insert_relation(&transaction, relation)?;
        }
        // After the work items: a fresh row carries `details_rev = 0`, and it
        // is this that lifts it to the revision the details were read at.
        write_details(&transaction, details)?;
        transaction.commit()?;
        Ok(())
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

/// One connection to the file, configured the way every reader and writer
/// wants it: a short wait on a lock rather than an immediate refusal, and the
/// write-ahead log so a reload never blocks a pull.
fn connect(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)
        .with_context(|| format!("failed to open database at {}", path.display()))?;
    connection
        .busy_timeout(StdDuration::from_secs(3))
        .context("failed to configure SQLite busy timeout")?;
    connection
        .execute_batch("PRAGMA journal_mode = WAL;")
        .context("failed to configure SQLite")?;
    Ok(connection)
}

fn schema_version(connection: &Connection) -> Result<i64> {
    Ok(connection.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Returns whether the schema had to be rebuilt.
fn ensure_current_schema(connection: &Connection) -> Result<bool> {
    if schema_version(connection)? == SCHEMA_VERSION {
        return Ok(false);
    }
    connection
        .execute_batch(RESET_SCHEMA)
        .context("failed to rebuild the ticket cache schema")?;
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(true)
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

/// Drops one work item and everything filed under it: the row, its links in
/// both directions — a relation pointing at a work item that no longer exists
/// is no more meaningful than one leading out of it — its comments, and its
/// history. Both a pull that noticed a work item has gone and a delete the user
/// asked for leave the file this way.
fn forget_work_item(transaction: &Transaction<'_>, organization: &str, id: i64) -> Result<()> {
    transaction.execute(
        "DELETE FROM work_items WHERE organization = ?1 AND work_item_id = ?2",
        params![organization, id],
    )?;
    transaction.execute(
        "DELETE FROM work_item_relations
         WHERE organization = ?1 AND (from_id = ?2 OR to_id = ?2)",
        params![organization, id],
    )?;
    transaction.execute(
        "DELETE FROM work_item_comments WHERE organization = ?1 AND work_item_id = ?2",
        params![organization, id],
    )?;
    transaction.execute(
        "DELETE FROM work_item_history WHERE organization = ?1 AND work_item_id = ?2",
        params![organization, id],
    )?;
    Ok(())
}

fn insert_ticket(transaction: &Transaction<'_>, ticket: &Ticket) -> Result<()> {
    transaction.execute(
        "INSERT OR REPLACE INTO work_items (
            organization, project, work_item_id, revision, work_item_type,
            title, state, reason, assigned_to, priority, area_path,
            iteration_path, tags, description, created_at, changed_at, web_url,
            details_rev, description_html
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                   ?18, ?19)",
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
            ticket.details_rev,
            ticket.description_html,
        ],
    )?;
    Ok(())
}

fn insert_relation(transaction: &Transaction<'_>, relation: &RelationRecord) -> Result<()> {
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
    Ok(())
}

fn insert_comment(transaction: &Transaction<'_>, comment: &CommentRecord) -> Result<()> {
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
    Ok(())
}

fn insert_history(transaction: &Transaction<'_>, entry: &HistoryRecord) -> Result<()> {
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
    Ok(())
}

/// Replaces the comments and revision history of every work item in `updates`
/// and records the revision they were read at. A work item with neither is
/// still written: its `details_rev` moves, which is what stops it being asked
/// about again.
fn write_details(transaction: &Transaction<'_>, updates: &[DetailsUpdate]) -> Result<()> {
    for update in updates {
        transaction.execute(
            "DELETE FROM work_item_comments WHERE organization = ?1 AND work_item_id = ?2",
            params![update.key.organization, update.key.id],
        )?;
        transaction.execute(
            "DELETE FROM work_item_history WHERE organization = ?1 AND work_item_id = ?2",
            params![update.key.organization, update.key.id],
        )?;
        for comment in &update.details.comments {
            insert_comment(transaction, comment)?;
        }
        for entry in &update.details.history {
            insert_history(transaction, entry)?;
        }
        transaction.execute(
            "UPDATE work_items SET details_rev = ?3
             WHERE organization = ?1 AND work_item_id = ?2",
            params![update.key.organization, update.key.id, update.revision],
        )?;
    }
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
            description_html: "<p>Cached from <b>Azure DevOps</b>.</p>".into(),
            created_at: ts("2026-01-01T00:00:00Z"),
            changed_at: ts("2026-02-01T00:00:00Z"),
            web_url: format!("https://dev.azure.com/example-org/atlas/_workitems/edit/{id}"),
            details_rev: 0,
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
        assert!(
            repository.schema_was_rebuilt(),
            "a brand new file starts without the tables"
        );
        drop(repository);
        assert!(
            !SqliteTicketRepository::open(&path)
                .unwrap()
                .schema_was_rebuilt(),
            "reopening a current database leaves its rows alone"
        );
        assert!(
            !SqliteTicketRepository::open_existing(&path)
                .unwrap()
                .schema_was_rebuilt()
        );
    }

    #[test]
    fn replace_all_round_trips_tickets_and_relations_without_touching_sync_meta() {
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
        assert_eq!(
            loaded[0].description_html, "<p>Cached from <b>Azure DevOps</b>.</p>",
            "the raw description survives the round trip beside its flattened reading"
        );
        assert_eq!(repository.load_graph().unwrap(), graph);

        let survivor = vec![ticket(3)];
        repository
            .replace_all(&survivor, &TicketGraph::default())
            .unwrap();
        assert_eq!(repository.load_all().unwrap(), survivor);
        assert_eq!(repository.load_graph().unwrap(), TicketGraph::default());

        assert_eq!(repository.meta(ME_DISPLAY_NAME_KEY).unwrap(), None);
        repository
            .set_meta(ME_DISPLAY_NAME_KEY, "Jacob Ragsdale")
            .unwrap();
        repository
            .replace_all(&survivor, &TicketGraph::default())
            .unwrap();
        assert_eq!(
            repository.meta(ME_DISPLAY_NAME_KEY).unwrap().as_deref(),
            Some("Jacob Ragsdale"),
            "a pull replaces work items, not who is signed in"
        );
        repository
            .set_meta(ME_DISPLAY_NAME_KEY, "Avery Chen")
            .unwrap();
        drop(repository);
        assert_eq!(
            SqliteTicketRepository::open(&path)
                .unwrap()
                .meta(ME_DISPLAY_NAME_KEY)
                .unwrap()
                .as_deref(),
            Some("Avery Chen"),
            "the signed-in name outlives a reopen"
        );
    }

    #[test]
    fn reparent_writes_the_new_link_and_clears_the_one_the_old_parent_held() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let mut repository = SqliteTicketRepository::open(&path).unwrap();
        let relation = |from: &Ticket, to: &Ticket, kind| RelationRecord {
            from: from.key.clone(),
            to: to.key.clone(),
            kind,
        };
        // The two halves a pull writes for one hierarchy link, and a related
        // link on the old parent that has nothing to do with the family.
        let graph = TicketGraph {
            relations: vec![
                relation(&ticket(3), &ticket(1), RelationKind::Parent),
                relation(&ticket(1), &ticket(3), RelationKind::Child),
                relation(&ticket(1), &ticket(2), RelationKind::Related),
            ],
            ..TicketGraph::default()
        };
        repository
            .replace_all(&[ticket(1), ticket(2), ticket(3)], &graph)
            .unwrap();

        let mut moved = ticket(3);
        moved.revision = 7;
        repository
            .reparent(
                &moved,
                &[relation(&ticket(3), &ticket(2), RelationKind::Parent)],
            )
            .unwrap();

        let stored = repository.load_graph().unwrap();
        assert_eq!(stored.parents_of(&ticket(3).key), vec![ticket(2).key]);
        assert!(
            stored.children_of(&ticket(1).key).is_empty(),
            "the child link the old parent held is gone, not merely overwritten on the child"
        );
        assert!(
            stored
                .relations
                .contains(&relation(&ticket(1), &ticket(2), RelationKind::Related)),
            "a link that is not a hierarchy link is left alone"
        );
        assert_eq!(
            repository
                .load_all()
                .unwrap()
                .iter()
                .find(|held| held.key.id == 3)
                .map(|held| held.revision),
            Some(7),
            "the row takes the revision the move came back with"
        );
    }

    #[test]
    fn upsert_writes_one_work_item_and_its_links_without_disturbing_the_rest() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let mut repository = SqliteTicketRepository::open(&path).unwrap();
        let relation = |from: &Ticket, to: &Ticket, kind| RelationRecord {
            from: from.key.clone(),
            to: to.key.clone(),
            kind,
        };
        let graph = TicketGraph {
            relations: vec![
                relation(&ticket(1), &ticket(2), RelationKind::Parent),
                relation(&ticket(2), &ticket(1), RelationKind::Child),
            ],
            ..TicketGraph::default()
        };
        repository
            .replace_all(&[ticket(1), ticket(2)], &graph)
            .unwrap();
        let before = data_signature(&path);

        let mut edited = ticket(1);
        edited.state = "Done".into();
        edited.revision = 4;
        repository
            .upsert(
                &edited,
                &[relation(&ticket(1), &ticket(3), RelationKind::Related)],
            )
            .unwrap();

        let mut stored = repository.load_all().unwrap();
        stored.sort_by_key(|ticket| ticket.key.id);
        assert_eq!(stored, vec![edited.clone(), ticket(2)]);
        let relations = repository.load_graph().unwrap().relations;
        assert_eq!(
            relations.len(),
            2,
            "the work item's own links are replaced and nobody else's are"
        );
        assert!(relations.contains(&relation(&ticket(2), &ticket(1), RelationKind::Child)));
        assert!(relations.contains(&relation(&ticket(1), &ticket(3), RelationKind::Related)));
        assert_ne!(
            data_signature(&path),
            before,
            "the watcher can tell the file was written"
        );

        let mut fresh = ticket(9);
        fresh.title = "Written by an edit".into();
        repository.upsert(&fresh, &[]).unwrap();
        assert_eq!(repository.load_all().unwrap().len(), 3);
        assert_eq!(
            repository
                .load_all()
                .unwrap()
                .iter()
                .find(|ticket| ticket.key.id == 9)
                .map(|ticket| ticket.title.clone()),
            Some("Written by an edit".to_owned())
        );
    }

    #[test]
    fn a_stale_schema_is_rebuilt_by_open_but_refused_by_open_existing() {
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
                PRAGMA user_version = 5;",
            )
            .unwrap();
        drop(connection);

        let mut repository = SqliteTicketRepository::open(&path).unwrap();
        assert!(repository.load_all().unwrap().is_empty());
        assert!(repository.schema_was_rebuilt());
        assert_eq!(
            schema_version(&repository.connection).unwrap(),
            SCHEMA_VERSION
        );

        repository
            .replace_all(&[ticket(1)], &TicketGraph::default())
            .unwrap();
        drop(repository);
        assert_eq!(
            SqliteTicketRepository::open_existing(&path)
                .unwrap()
                .load_all()
                .unwrap()
                .len(),
            1
        );

        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        let error = format!(
            "{:#}",
            SqliteTicketRepository::open_existing(&path).unwrap_err()
        );
        assert!(error.contains("restart ticket-tui"), "{error}");
        let rows: i64 = Connection::open(&path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM work_items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1, "a version mismatch must not drop the cached rows");
    }

    #[test]
    fn type_states_round_trip_in_order_and_survive_a_pull() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("states.sqlite3");
        let mut repository = SqliteTicketRepository::open(&path).unwrap();
        assert_eq!(
            repository.load_type_states().unwrap(),
            StateCatalog::default(),
            "a fresh database knows no states, which is what the picker falls back for"
        );

        let issue = vec![
            StateOption::new("To Do", StateCategory::Proposed),
            StateOption::new("Doing", StateCategory::InProgress),
            StateOption::new("Done", StateCategory::Completed),
        ];
        repository.replace_type_states("Issue", &issue).unwrap();
        repository
            .replace_type_states("Task", &[StateOption::new("Cut", StateCategory::Removed)])
            .unwrap();

        let stored = repository.load_type_states().unwrap();
        assert_eq!(
            stored.states_for("Issue"),
            issue,
            "the process template's own order is kept"
        );
        assert_eq!(
            stored.states_for("Task"),
            [StateOption::new("Cut", StateCategory::Removed)]
        );
        assert!(stored.states_for("Epic").is_empty());

        repository
            .replace_all(&[ticket(1)], &TicketGraph::default())
            .unwrap();
        assert_eq!(
            repository.load_type_states().unwrap(),
            stored,
            "a pull replaces work items, not the states their type allows"
        );

        let renamed = vec![StateOption::new("Open", StateCategory::Proposed)];
        repository.replace_type_states("Issue", &renamed).unwrap();
        assert_eq!(
            repository.load_type_states().unwrap().states_for("Issue"),
            renamed,
            "a type is rewritten whole, so a retired state stops being offered"
        );
    }

    #[test]
    fn the_work_item_types_survive_a_pull_and_are_rewritten_whole() {
        let directory = tempdir().unwrap();
        let mut repository =
            SqliteTicketRepository::open(directory.path().join("t.sqlite3")).unwrap();
        assert!(
            repository.load_work_item_types().unwrap().is_empty(),
            "a fresh database knows no types, which is what the form falls back for"
        );

        let types = vec!["Epic".to_owned(), "Issue".to_owned(), "Task".to_owned()];
        repository.replace_work_item_types(&types).unwrap();
        assert_eq!(
            repository.load_work_item_types().unwrap(),
            types,
            "the process template's own order is kept"
        );

        repository
            .replace_all(&[ticket(1)], &TicketGraph::default())
            .unwrap();
        assert_eq!(
            repository.load_work_item_types().unwrap(),
            types,
            "a pull replaces work items, not the types the process offers"
        );

        repository
            .replace_work_item_types(&["Issue".to_owned()])
            .unwrap();
        assert_eq!(
            repository.load_work_item_types().unwrap(),
            ["Issue"],
            "the list is rewritten whole, so a retired type stops being offered"
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
