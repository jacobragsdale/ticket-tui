# The SQLite database

## What it is

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

Prefer `ticket-tui list --json` for anything it can answer. Reach for SQL only
for what the CLI does not print: the parent/child graph, comments, and revision
history.

## What a pull touches

An incremental pull upserts the work items changed since the stored watermark
and drops the ones the project no longer lists. A full pull (`ticket-tui sync
--full`) clears and refills `work_items`, `work_item_relations`,
`work_item_comments`, and `work_item_history`. It deliberately leaves
`sync_meta`, `work_item_type_states`, `identities`, and `classification_nodes`
alone: those describe the sync, the process, the people, and the trees the work
is planned into, not the work items themselves.

The file carries `PRAGMA user_version = 11`. There are no migrations — a
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
