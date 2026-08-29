# The SQLite database

## What it is

## Contents

- [What it is](#what-it-is)
- [What a pull touches](#what-a-pull-touches)
- [Tables](#tables)

Tables: `work_items`, `work_item_relations`, `work_item_comments`, `work_item_history`, `sync_meta`, `work_item_type_states`, `identities`, `classification_nodes`, `work_item_artifact_links`, `repos`, `pipelines` and `runs`, `pull_requests` and its three side tables`

A durable local copy of one Azure DevOps project. Azure DevOps stays the record
of truth, but the file persists across runs, is what the TUI and every
`ticket-tui list`/`show` answers from, and is what makes reads instant and
offline. It is not a scratch cache: a pull replaces the work item rows inside
it, it is not thrown away and rebuilt.

Default path:

- macOS: `~/Library/Application Support/ticket-tui/tickets.sqlite3`
- Linux: `$XDG_DATA_HOME/ticket-tui/tickets.sqlite3`, normally
  `~/.local/share/ticket-tui/tickets.sqlite3`

`--database PATH` on any invocation points elsewhere. WAL mode and a busy
timeout are on, so external readers can query it while the TUI runs.

**Read it, never write it.** Every change goes through `ticket-tui edit`,
`comment`, or `create`, which write to Azure DevOps first and store the copy
that comes back — so these rows only ever hold what the server accepted. A row
you write by hand is a lie the next pull erases.

Open it read-only:

```console
sqlite3 "file:$HOME/Library/Application Support/ticket-tui/tickets.sqlite3?mode=ro" \
  "SELECT work_item_id, state, title FROM work_items WHERE state = 'Doing'"
```

Prefer the subcommands for anything they can answer — `list`, `show`, `repos
list`, `prs list`, `runs list`, all with `--json`. Reach for SQL only for what
none of them prints: the parent/child graph, comments, revision history, and the
artifact links between a work item and its pull requests, commits and builds.

## What a pull touches

An incremental pull upserts the work items changed since the stored watermark
and drops the ones the project no longer lists. A full pull (`ticket-tui sync
--full`) clears and refills `work_items`, `work_item_relations`,
`work_item_artifact_links`, `work_item_comments`, `work_item_history`, `repos`,
`pipelines`, `runs`, `pull_requests`, `pr_reviewers`, `pr_work_items` and
`pr_threads`. It deliberately leaves
`sync_meta`, `work_item_type_states`, `identities`, and `classification_nodes`
alone: those describe the sync, the process, the people, and the trees the work
is planned into, not the work items themselves.

Timelines, logs and a run's live progress are **not** stored at all: the
Pipelines tab and the `runs show`/`runs logs`/`runs wait` commands read them
from Azure DevOps every time. Nothing in SQLite answers "is the build finished".

The file carries `PRAGMA user_version = 17`. There are no migrations — a
database at another version has its tables dropped, recreated, and refilled by
an immediate pull on the next TUI launch. That is the one case rows vanish
wholesale, and it is a version upgrade, not routine operation. After upgrading
the binary, restart any running ticket-tui.

## Tables

### `work_items`

Primary key `(organization, work_item_id)`.

| Column | Meaning |
|---|---|
| `organization`, `project` | Azure DevOps location |
| `work_item_id`, `revision` | Identity, and the revision the row was read at |
| `work_item_type`, `title`, `state`, `reason` | Core fields |
| `assigned_to`, `priority` | Ownership and priority; nullable |
| `area_path`, `iteration_path`, `tags` | Planning metadata; tags are `;`-separated |
| `description` | The description flattened to plain text — the readable one |
| `description_html` | The same field as Azure DevOps stores it |
| `created_at`, `changed_at` | UTC RFC 3339 timestamps |
| `web_url` | Browser URL for the work item |
| `details_rev` | Revision whose comments and history are stored, `0` for none |

Indexed on `changed_at`, `priority`, `state`, and `work_item_type`.

### `work_item_relations`

The graph around each work item, from every pull. Primary key
`(organization, from_id, to_id, kind)`; `kind` is one of `parent`, `child`,
`related`, `predecessor`, `successor`, `duplicate`. Links are stored in both
directions, so a parent/child pair appears as two rows.

The children of an Epic — which no `--query` can reach, because parentage is not
a field on the work item:

```sql
SELECT w.work_item_id, w.state, w.title
FROM work_item_relations r
JOIN work_items w
  ON w.organization = r.organization AND w.work_item_id = r.to_id
WHERE r.organization = 'jacobragsdale' AND r.from_id = 624 AND r.kind = 'child'
ORDER BY w.work_item_id;
```

### `work_item_comments`

`(organization, work_item_id, comment_id)`, with `created_at`, `author`, and
`body` as flattened text. Only present for work items whose `details_rev` says
their details have been read — the TUI reads them eagerly for changed items and
lazily on selection, so a work item nobody has opened has none stored. That is
absence of data, not absence of comments.

### `work_item_history`

`(organization, work_item_id, revision, field_name)`, with `changed_at`,
`changed_by`, `old_value`, and `new_value` — one row per field a revision
touched. Same `details_rev` caveat as comments.

### `sync_meta`

Key/value, describing the sync rather than the work items.

| Key | Meaning |
|---|---|
| `me_display_name` | The signed-in display name `@me` resolves to |
| `watermark_changed_at` | The greatest `System.ChangedDate` the last successful pull saw |
| `organization`, `project` | Where the stored work items were pulled from |
| `sync_scope` | The extra WIQL condition that pull narrowed the project with, empty for a whole project |
| `classification_nodes_fetched_at` | When the area/iteration trees were last read |

A run that resolves a different organization/project refuses to sync into a
database already holding work items; `--full` is how the replacement is asked
for.

### `work_item_type_states`

What states each type offers: `(work_item_type, name)`, plus `category`
(`Proposed`, `InProgress`, `Resolved`, `Completed`, `Removed`) and `position`,
the order the process template lists them in. Check here before
`ticket-tui edit --state`; in this project it is `To Do` → `Doing` → `Done` for
both `Epic` and `Issue`.

### `identities`

`display_name` (primary key) and `unique_name`, the sign-in address a write is
addressed to. Filled from the project's teams the first time the assignee picker
opens. `--assignee` matches either column, case-insensitively.

### `classification_nodes`

What `--iteration` and `--area` take: `kind` (`area` or `iteration`), `path`
(the value a work item's field carries, such as `development\Sprint 1`), `depth`,
`start_date`, `finish_date`, and `position`. Keyed on `(kind, path)`.

```sql
SELECT path, start_date, finish_date FROM classification_nodes
WHERE kind = 'iteration' ORDER BY position;
```

### `work_item_artifact_links`

What a work item was worked on with, from the `ArtifactLink` relations Azure
DevOps stores: `(organization, work_item_id, kind, repo_id, target)`, plus
`name` — the label Azure DevOps gives the link, such as `Pull Request` or
`Integrated in build`.

`kind` is `pull_request`, `commit` or `build`. `target` is the pull request id,
the build id, or the commit sha; `repo_id` is the repository GUID for the first
two kinds and empty for a build. Nothing here promises the far end is in this
database — a pull request older than the stored window is still linked.

```sql
SELECT a.kind, a.target, r.name
FROM work_item_artifact_links a
LEFT JOIN repos r ON r.id = a.repo_id
WHERE a.work_item_id = 690;
```

### `repos`

One row per Git repository: `id` (the GUID every other table names it by),
`name`, `project`, `default_branch` (the full `refs/heads/…`), `remote_url`,
`ssh_url`, `web_url`, `is_disabled`, and `size` in bytes. Nothing about clones
on this machine is stored — that is read from the workspace when it is needed.

### `pipelines` and `runs`

`pipelines`: `id`, `name`, `folder`, `repo_id`, `default_branch`, `url`,
`queue_status`. `runs`: `id`, `pipeline_id`, `build_number`, `status`, `result`,
`source_branch`, `source_version`, `requested_for`, `reason`, `pr_id`,
`queue_time`, `start_time`, `finish_time`, `url`.

A pull stores the newest window of runs, so `runs` is recent history rather than
everything the project has ever built, and a run still going is only as current
as the last pull. Ask Azure DevOps through `ticket-tui runs show` for anything
live.

### `pull_requests` and its three side tables

`pull_requests`: `id`, `repo_id`, `title`, `description`, `status`, `is_draft`,
`created_by`, `created_by_unique`, `created_at`, `closed_at`, `source_ref`,
`target_ref`, `merge_status`, `last_merge_source_commit`, `auto_complete_set_by`,
`url`, `build_status`, `build_run_id`.

- `pr_reviewers` — `(pull_request_id, reviewer_id)`, `display_name`,
  `unique_name`, `vote`, `is_required`. Vote is the API's scale: `10` approved,
  `5` approved with suggestions, `0` none, `-5` waiting, `-10` rejected.
- `pr_work_items` — `(pull_request_id, work_item_id)`, the work items it closes.
- `pr_threads` — the first comment of each thread: `id`, `author`, `text`,
  `published_at`, `status`.

Every active pull request is stored, plus a window of recently closed ones, so
`status = 'completed'` is recent history rather than the whole project's.

```sql
SELECT p.id, p.title, COUNT(r.reviewer_id) AS reviewers,
       SUM(CASE WHEN r.vote > 0 THEN 1 ELSE 0 END) AS approvals
FROM pull_requests p
LEFT JOIN pr_reviewers r ON r.pull_request_id = p.id
WHERE p.status = 'active'
GROUP BY p.id ORDER BY p.id DESC;
```
