# ticket-tui

`ticket-tui` is a read-only terminal browser for Azure DevOps work items stored
in SQLite. It provides mouse navigation, responsive ticket details, field
sorting, live fuzzy search, and links that open in the system browser.

The current release is intentionally offline. It creates realistic local demo
data so the interface can be developed and evaluated without Azure DevOps
access.

## Run the demo

You need Rust 1.88 or newer and a macOS or Linux terminal.

1. Build and run the application:

   ```console
   cargo run --release
   ```

2. On first run, the application creates a SQLite database and inserts 500 demo
   tickets. The status line displays the database path.

3. Press `/`, type part of a ticket title or a filter such as `state:active`,
   and watch the table update. Press `Esc` to leave the search box while
   retaining the filter.

4. Select a ticket and press `o` to open its fake ADO-shaped HTTPS URL.
   These demo URLs exercise the launcher but do not resolve to real work items.

5. Press `q` to exit.

To use another database path:

```console
cargo run --release -- --database ./tickets.sqlite3
```

Open an existing database without migrating, seeding, or journal changes:

```console
cargo run --release -- --database ./tickets.sqlite3 --read-only
```

Import a local JSON or CSV file, then open the TUI:

```console
cargo run --release -- --database ./tickets.sqlite3 --import ./tickets.json
```

A nonexistent path is initialized and seeded. An existing empty database is
migrated but deliberately left empty, which prevents demo rows from being added
to a database prepared by another tool.

## Controls

| Input | Action |
|---|---|
| `↑`/`↓`, `j`/`k` | Move through tickets or scroll the focused details pane |
| `Page Up`/`Page Down` | Move ten tickets |
| `Home`/`End` | Select the first/last ticket or jump through focused details |
| `/` | Focus live fuzzy search |
| `←`/`→`, `Home`/`End` | Move the search cursor while searching |
| `Backspace`/`Delete`, `Ctrl-W` | Edit the query while searching |
| `Ctrl-U` | Clear the query while searching |
| `Ctrl-P`/`Ctrl-N` | Recall previous/next completed searches |
| Paste | Insert sanitized pasted text into the search query |
| `Esc` | Leave search, clear the query, or clear a multi-selection |
| `s` | Open the sort menu; use arrows and `Enter` to apply |
| `v` | Toggle relevance-first or strict field ordering during search |
| `c` | Toggle compact or comfortable table rows |
| `f` | Focus the filter bar; `h`/`l` change field, `j`/`k` values, `Space` toggles |
| `+` | Open the full filter overlay for extra fields |
| `w` | Show, hide, reorder (`J`/`K`), and resize (`<`/`>`) columns |
| `p` / `:` | Open the command palette |
| `V` | Open named views; `n` saves, `Enter` loads, `d` deletes |
| `m` | Bookmark or unbookmark the selected ticket |
| `Space` | Toggle ticket multi-select |
| `y` | Copy selected (or current) ticket IDs |
| `[` / `]` | Jump to the previous or next recently viewed ticket |
| `Tab` | Toggle focus between tickets and details |
| `d` | Toggle the details screen when the terminal is under 70 columns |
| `Enter` | Select the family cursor ticket, or open from the details pane |
| `o` | Open the selected ticket in the system browser |
| `r` | Reload and rebuild the search index in the background |
| `i` | Show database path, row counts, and data freshness |
| `?` | Show the in-app help; use arrows or page keys to scroll it |
| `q`, `Ctrl-C` | Quit |

Mouse input stays captured so the TUI can provide its own pointer controls
without restoring terminal drag-select. Wheel scrolling moves the hovered
table, details pane, help, or overlay by three rows or lines and does not
change keyboard focus or the selected ticket. Left-click activates the
visible control under the pointer on release: search, filter pills, sort
headers, ticket rows, checkboxes, bookmark markers,
underlined IDs and URLs, tabs, overlay rows, and close/action buttons. Dragging over visible text
selects it and copies the plain text on release. Bracketed paste inserts at
the caret in search, the command palette, the import prompt, and the named-view
editor. Scrollbar tracks page by a viewport-minus-one step; thumbs can be
dragged. Right-click, double-click, and horizontal wheel gestures are not
used. Terminals supporting OSC 22 show a browser-style pointer over external
URL targets.

Search accepts a compact grammar such as `state:active type:bug
assignee:"Avery Chen" priority:1 tag:rust`. Values in the same field are
combined with OR; different fields are combined with AND. `is:bookmarked`
limits the table to locally bookmarked tickets. Active filters appear as
removable chips. The command palette copies IDs, URLs, titles, Markdown links,
or summaries, exports the selection as JSON or CSV, and imports local JSON or
CSV files with row-level diagnostics. Press `i` for database path, row count,
and freshness. Local SQLite changes reload automatically; the table title
shows `Stale` until the reload finishes. `--read-only` opens an existing
database without migrating, seeding, or changing journal mode.

Ticket states and priorities use restrained semantic colors, work-item types
and tags render as compact badges, and matched search characters are
underlined in visible results. Changed dates use compact relative labels, and
exact UTC timestamps remain available in details. Press `c` to switch between
compact and comfortable row density. Named views, column layout, bookmarks,
and the last query are saved beside the database as `*.session.json`. Set the
standard `NO_COLOR` environment variable to use the monochrome theme.

## Database reference

The default database is `ticket-tui/tickets.sqlite3` under the platform data
directory:

- macOS: `~/Library/Application Support/ticket-tui/tickets.sqlite3`
- Linux: `$XDG_DATA_HOME/ticket-tui/tickets.sqlite3`, normally
  `~/.local/share/ticket-tui/tickets.sqlite3`

The versioned `work_items` table stores these columns:

| Column | Meaning |
|---|---|
| `organization`, `project` | Azure DevOps location |
| `work_item_id`, `revision` | Work-item identity and revision |
| `work_item_type`, `title`, `state`, `reason` | Core work-item fields |
| `assigned_to`, `priority` | Ownership and priority |
| `area_path`, `iteration_path`, `tags` | Planning metadata |
| `description` | Plain-text detail content |
| `created_at`, `changed_at` | UTC RFC 3339 timestamps |
| `web_url` | HTTPS browser URL for the work item |

The primary key is `(organization, work_item_id)`. Tags use Azure DevOps-style
semicolon separation. Schema version 2 adds local `work_item_relations`,
`work_item_comments`, and `work_item_history` tables. The TUI displays those
records but never edits them. Parent and child links render as an always-expanded
family tree in the details pane. Click a family row, or press `Enter` on the
family cursor, to select that ticket in the table. Fuzzy search covers ID,
title, assignee, state, type, area, iteration, and tags; it intentionally
excludes descriptions. Structured `field:value` tokens are parsed out of the
query before fuzzy matching.

Schema version 3 adds the `current_selection` table as a stable interface for
other local programs. It contains zero or one row:

| Column | Meaning |
|---|---|
| `singleton` | Always `1`; enforces the one-row contract |
| `organization`, `work_item_id` | Identity of the ticket currently shown |
| `selected_at` | UTC RFC 3339 time when that ticket became current |

Query the current ticket with:

```sql
SELECT organization, work_item_id, selected_at
FROM current_selection
WHERE singleton = 1;
```

In normal writable mode, ticket-tui replaces this row whenever its selected
ticket changes and deletes it on a clean exit. An empty result means there is
no published selection. `--read-only` mode never publishes or clears this row.
If the process is killed or crashes, its last row can remain, so consumers
should treat the row as the last observed selection rather than proof that the
application is still running.

The application uses WAL mode and a busy timeout so external SQLite readers can
query the database while the TUI is running. Normal browsing does not edit work
items; an explicit import can upsert ticket data. There is no Azure DevOps
authentication or network client in this release.

## Develop and verify

Run the same checks used by CI:

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```

CI exercises these checks on current macOS and Linux runners.
