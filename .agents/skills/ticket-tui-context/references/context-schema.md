# Live context JSON reference

A **running** ticket-tui publishes a versioned JSON document beside its SQLite
database after each meaningful rendered-state change. For `tickets.sqlite3` the
context path is `tickets.context.json`. The file is written atomically and
removed on a clean exit, so its absence means no TUI is running — read the
backlog with `ticket-tui list` instead.

The file can survive a crash or forced termination. `process_id` is a
best-effort liveness check, and `updated_at` is the UTC RFC 3339 time of the
last state change. Neither field is a heartbeat guarantee.

One live publisher per database is supported. If several ticket-tui processes
open the same database they share one context path and the last rendered write
wins.

This document is the live *view*. It is not the backlog: it carries only the
rendered viewport and the current selection. For work item data use
[cli.md](cli.md), and for records the CLI does not print use
[database.md](database.md).

## Top-level fields

Current contract: **schema version 2**. A reader that finds another version
should refuse rather than guess.

| Field | Type | Meaning |
|---|---|---|
| `schema_version` | integer | Context contract version; currently `2` |
| `process_id` | integer | PID of the ticket-tui process that wrote the file |
| `updated_at` | string | UTC RFC 3339 time of the published state change |
| `database_path` | string | SQLite database backing the view |
| `me` | string or null | Signed-in display name recorded by sync, including the background refresh, overridden by `TICKET_TUI_ME`; null when neither is set |
| `sync` | object | Where the rows are pulled from and how the last pull went |
| `pending_edits` | pending edit array | Edits sent to Azure DevOps and not answered yet |
| `mode` | string | `browse`, `search`, or the active overlay name |
| `focus` | string | `tickets`, `family`, or `details` |
| `screen` | string | `workspace` or the narrow-layout `details` screen |
| `active_view` | string or null | Loaded named view, if any |
| `search` | object | Query, parsed filters, pending state, and ordering |
| `sort` | object | Sort field, direction, and row density |
| `tickets` | object | Result counts, viewport position, and rendered rows |
| `selected_ticket` | ticket or null | Ticket driving the details pane |
| `checked_tickets` | ticket array | Multi-select set used for bulk actions |
| `family_cursor` | ticket reference or null | Keyboard cursor within the family tree |
| `details_scroll_line` | integer | Zero-based details-pane scroll line |

## Sync fields

`sync` describes the freshness of every row in the document. Read it before
treating a work item field as current.

| Field | Type | Meaning |
|---|---|---|
| `sync.organization` | string or null | Azure DevOps organization the rows are pulled from; null when no project is resolved |
| `sync.project` | string or null | Azure DevOps project the rows are pulled from |
| `sync.refresh_seconds` | integer | Seconds between timer pulls; `0` when the timer is off |
| `sync.in_progress` | boolean | A pull is in flight right now |
| `sync.last_success_at` | string or null | UTC RFC 3339 time the last pull that reached Azure DevOps finished; null when none has this run |
| `sync.last_error` | string or null | What the last failed pull said; cleared by the next pull that succeeds |
| `sync.offline` | boolean | No Azure DevOps project was resolved for this run, so it never refreshes |

Precisely:

- **`refresh_seconds: 0`** means the timer is off and the sync key — `r` in the
  TUI — is the only thing that pulls. It is also `0` on an offline run, where
  nothing pulls at all, so read it together with `offline`. The default is 60,
  changed by `--refresh SECONDS` or `TICKET_TUI_REFRESH`.
- **`last_success_at`** moves on *any* pull that reaches Azure DevOps, including
  one that finds nothing new: it says when the rows were last confirmed, not
  when they last changed. It is null until the first pull of the run lands, so
  an older `updated_at` with no `last_success_at` means nothing has been
  confirmed since the process started.
- **`offline: true`** means no project was resolved for the run — no
  organization configured, or a database holding a different project. The TUI
  browses whatever the database already holds and never refreshes it. A run
  whose sync worker started and then died keeps `offline` false and reports
  itself through `last_error` instead, so read both.

When `offline` is true, or `last_error` is set, or `last_success_at` is far
behind `updated_at`, the rows are last-synced values rather than live ones. Say
so instead of reporting them as current, or run `ticket-tui sync` and re-read.

## Pending edit fields

`pending_edits` lists write-through edits the TUI has sent and Azure DevOps has
not answered. The rows already show these values optimistically, so a field
named here is a request, not a stored value: it can still be refused and put
back. An empty array means every visible value is one the server returned.

| Field | Type | Meaning |
|---|---|---|
| `id` | integer | Work item the edit is on |
| `field` | string | Field as the Edit menu names it, such as `State` or `Tags` |
| `value` | string | Value being written; a cleared field reads `(none)` |
| `since` | string | UTC RFC 3339 time the edit was sent |

Entries are ordered by work item id, and one work item carries at most one
pending edit: a second edit of the same row is refused while the first is in
flight. Do not send a `ticket-tui edit` for a work item listed here: both writes
lead with the same stored revision, so whichever lands second is refused. Wait
for the entry to clear.

## Search fields

`search.query` preserves the complete user input. `search.fuzzy_text` contains
only free-text terms. `search.filters` contains canonical labels such as
`state:Active`, `tag:rust`, `changed:<7d`, and `is:bookmarked` — the same
grammar `ticket-tui list --query` takes, described in [filters.md](filters.md).
Values in one field are ORed; different fields are ANDed. A value written with a
leading `@` — `assignee:@me`, `assignee:@none`, `iteration:@current`,
`state:@open` — is reported as typed and resolved as the filter runs, so read
`me` at the top level to know who `assignee:@me` currently means.
`search.pending` means the fuzzy result worker has not yet published its latest
matches, so the counts below are a moment behind the query above.

`search.order` is `relevance` or `field`. `sort.field` is the column the table
is ordered by (`changed`, `priority`, `id`, `title`, `state`, `type`,
`assignee`, `organization`, `project`, `area`, `iteration`, `created`, `tags`,
`progress`), `sort.direction` is `asc` or `desc`, and `sort.row_density` is
`compact` or `comfortable`.

## Ticket viewport fields

`tickets.total_count` is the number of work items loaded from SQLite.
`tickets.matching_count` is the complete filtered/search result count.
`tickets.viewport_start` is the zero-based first rendered result index, and
`tickets.viewport_size` is the current row capacity. `tickets.visible_rows`
contains only rows rendered in that viewport — never answer "there are N work
items" from its length.

Ticket objects carry `organization`, `project`, `id`, `work_item_type`, `title`,
`state`, `assigned_to`, `priority`, `tags`, `web_url`, `bookmarked`, and
`checked`. A family cursor is only a ticket reference, with `organization` and
`id`. For the description, comments, history, or the parent/child graph, take
the `id` to `ticket-tui show <id>` or to SQLite.

## Reading it

`scripts/read_context.py` prints a compact interpretation, validates the
document against this schema, and refuses a version it does not know:

```console
uv run .agents/skills/ticket-tui-context/scripts/read_context.py [--database PATH]
uv run .agents/skills/ticket-tui-context/scripts/read_context.py --json
uv run .agents/skills/ticket-tui-context/scripts/read_context.py --details
```

Exit 2 means there is no context file at that path — pass `--database` if the
TUI was started with one, or `--context` to point at the file directly. Exit 1
means the file is unreadable, a schema the script does not support, or malformed
against it. `--details` joins the selected work item to its full SQLite records
using `database_path` from the document itself.

Keep `SCHEMA_VERSION` in the script equal to `SCHEMA_VERSION` in
`src/agent_context.rs` whenever the contract changes.
