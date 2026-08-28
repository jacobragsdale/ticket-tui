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
| `schema_version` | integer | Context contract version; currently `1` |
| `process_id` | integer | PID of the ticket-tui process that wrote the file |
| `updated_at` | string | UTC RFC 3339 time of the published state change |
| `database_path` | string | SQLite database backing the view |
| `read_only` | boolean | Whether ticket-tui opened SQLite with `--read-only` |
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

## Search fields

`search.query` preserves the complete user input. `search.fuzzy_text` contains
only free-text terms. `search.filters` contains canonical labels such as
`state:Active`, `tag:rust`, and `is:bookmarked`. Values in one field are ORed;
different fields are ANDed. `search.pending` means the fuzzy result worker has
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

The JSON file is the live UI-state interface. SQLite remains the durable ticket
data source. Join a ticket identity to `work_items` on `(organization,
work_item_id)` when full fields are needed. Related records use
`work_item_relations`, `work_item_comments`, and `work_item_history`.

The context publisher never requires a writable SQLite connection, so it also
works when ticket-tui runs with `--read-only`.
