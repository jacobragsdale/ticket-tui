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

3. Press `/`, type part of a ticket title or other core field, and watch the
   table update. Press `Esc` to leave the search box while retaining the filter.

4. Select a ticket and press `Enter` to open its fake ADO-shaped HTTPS URL.
   These demo URLs exercise the launcher but do not resolve to real work items.

5. Press `q` to exit.

To use another database path:

```console
cargo run --release -- --database ./tickets.sqlite3
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
| `Esc` | Leave search, or clear an active filter from browse mode |
| `s` | Open the sort menu; use arrows and `Enter` to apply |
| `v` | Toggle relevance-first or strict field ordering during search |
| `Tab` | Switch focus between tickets and details |
| `d` | Toggle the details screen when the terminal is under 70 columns |
| `Enter`, `o` | Open the selected ticket in the system browser |
| `r` | Reload and rebuild the search index in the background |
| `?` | Show the in-app help; use arrows or page keys to scroll it |
| `q`, `Ctrl-C` | Quit |

Mouse input can select rows, scroll either pane, sort by visible headers, and
open an underlined ticket ID or detail URL.

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
semicolon separation. Fuzzy search covers ID, title, assignee, state, type,
area, iteration, and tags; it intentionally excludes descriptions.

The application uses WAL mode and a busy timeout so a future synchronizer can
update the database between reloads. There is no Azure DevOps authentication,
network client, import command, or ticket editing in this release.

## Develop and verify

Run the same checks used by CI:

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```

CI exercises these checks on current macOS and Linux runners.
