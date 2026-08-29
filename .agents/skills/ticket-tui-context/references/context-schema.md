# Live context JSON reference

ticket-tui publishes a versioned JSON document beside its SQLite database after
each meaningful rendered-state change. For `tickets.sqlite3`, the context path
is `tickets.context.json`. The file is atomically replaced and removed on a
clean exit.

The file can survive a crash or forced termination. `process_id` is a
best-effort liveness check, and `updated_at` is the UTC RFC 3339 time of the
last state change. Neither field is a heartbeat guarantee.

One live publisher per database is supported. If multiple ticket-tui processes
open the same database, they share one context path and the last rendered write
wins.

## Top-level fields

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
treating a ticket field as current.

| Field | Type | Meaning |
|---|---|---|
| `sync.organization` | string or null | Azure DevOps organization the rows are pulled from; null when no project is resolved |
| `sync.project` | string or null | Azure DevOps project the rows are pulled from |
| `sync.refresh_seconds` | integer | Seconds between timer pulls; `0` when the timer is off and only the sync key pulls |
| `sync.in_progress` | boolean | A pull is in flight right now |
| `sync.last_success_at` | string or null | UTC RFC 3339 time the last pull that reached Azure DevOps finished; null when none has this run |
| `sync.last_error` | string or null | What the last failed pull said; cleared by the next pull that succeeds |
| `sync.offline` | boolean | The run has no Azure DevOps project to pull from and never refreshes |

`sync.last_success_at` moves on every pull that reaches Azure DevOps, including
one that finds nothing new: it says when the rows were last confirmed, not when
they last changed. It is absent until the first pull of the run lands, so an
older `updated_at` with no `last_success_at` means nothing has been confirmed
since the process started.

A run whose sync worker died after starting keeps `sync.offline` false and
reports itself through `sync.last_error`, so read both.

When `sync.offline` is true, or `sync.last_error` is set, or
`sync.last_success_at` is far behind `updated_at`, the rows are the last synced
values rather than live ones. Say so instead of reporting them as current.

## Pending edit fields

`pending_edits` lists the write-through edits the TUI has sent and Azure DevOps
has not answered. The rows already show these values optimistically, so a field
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
flight.

## Search fields

`search.query` preserves the complete user input. `search.fuzzy_text` contains
only free-text terms. `search.filters` contains canonical labels such as
`state:Active`, `tag:rust`, and `is:bookmarked`. Values in one field are ORed;
different fields are ANDed. A value written with a leading `@` — `assignee:@me`,
`assignee:@none`, `iteration:@current`, `state:@open` — is reported as typed and
resolved as the filter runs, so read `me` at the top level to know who
`assignee:@me` currently means. `search.pending` means the fuzzy result worker has
not yet published its latest matches.

## Ticket viewport fields

`tickets.total_count` is the number of tickets loaded from SQLite.
`tickets.matching_count` is the complete filtered/search result count.
`tickets.viewport_start` is the zero-based first rendered result index, and
`tickets.viewport_size` is the current row capacity. `tickets.visible_rows`
contains only rows rendered in that viewport.

Ticket objects contain `organization`, `project`, `id`, `work_item_type`,
`title`, `state`, `assigned_to`, `priority`, `tags`, `web_url`, `bookmarked`,
and `checked`. A family cursor is only a ticket reference with `organization`
and `id`.

## SQLite relationship

The JSON file is the live UI-state interface. SQLite holds the full ticket
fields behind that view. Join a ticket identity to `work_items` on
`(organization, work_item_id)` when full fields are needed. Related records use
`work_item_relations`, `work_item_comments`, and `work_item_history`.

The SQLite database is a durable local copy of one Azure DevOps project, synced
by running ticket-tui with `--sync` and by the background refresh a running
ticket-tui performs every 60 seconds by default; Azure DevOps remains the record
of truth. It can still lag the server by up to one refresh interval, so a work
item changed in Azure DevOps moments ago may read as its last synced values;
`sync.last_success_at` says how far behind it can be.
Read it freely; never write to it, because ticket-tui replaces its rows
wholesale on the next sync.
