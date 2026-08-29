# ticket-tui

`ticket-tui` is a fast terminal browser for Azure DevOps work items. It keeps a
local SQLite cache of one Azure DevOps project and reads from that cache, so
navigation, sorting, filtering, and fuzzy search stay instant. It provides mouse
navigation, responsive ticket details, field sorting, live fuzzy search, and
links that open in the system browser.

The cache is disposable. `--sync` pulls the project's work items over the Azure
DevOps REST API and replaces the cache contents; everything else the TUI does is
local and never edits work items.

## Run it

You need Rust 1.88 or newer, a macOS or Linux terminal, and access to an Azure
DevOps project.

1. Sign in with the Azure CLI:

   ```console
   az login
   ```

2. Point ticket-tui at an organization and project, then pull the work items:

   ```console
   cargo run --release -- --sync --org my-org --project my-project
   ```

3. Later runs open the cache without touching the network:

   ```console
   cargo run --release
   ```

4. Press `/`, type part of a ticket title or a filter such as `state:active`,
   and watch the table update. Press `Esc` to leave the search box while
   retaining the filter.

5. Select a ticket and press `o` to open it on `dev.azure.com`.

6. Press `q` to exit.

Without `--sync`, an empty cache opens to the status line
`Cache is empty; run with --sync to pull work items from Azure DevOps`.

To use another cache file:

```console
cargo run --release -- --database ./tickets.sqlite3
```

## Authentication

ticket-tui borrows the Azure CLI's login. It runs

```console
az account get-access-token --resource 499b84ac-1321-427f-aa17-267ca6975798
```

and sends the result as an `Authorization: Bearer` header together with
`X-VSS-ForceMsaPassThrough: true`, which organizations backed by a Microsoft
personal account require.

Setting `AZURE_DEVOPS_EXT_PAT` to a personal access token takes precedence and
switches to Basic authentication, for environments without the Azure CLI. A
`401` or `302` response is reported as rejected credentials with a reminder to
run `az login`.

ticket-tui stores no secrets. It reads the token from the CLI or the environment
on each sync and keeps nothing but work-item data in SQLite.

## Organization and project

Both values are resolved in this order:

1. the `--org` and `--project` flags;
2. the `TICKET_TUI_ORG` and `TICKET_TUI_PROJECT` environment variables;
3. the `[defaults]` entries in `~/.azure/azuredevops/config`, written by
   `az devops configure --defaults organization=... project=...`
   (`AZURE_CONFIG_DIR` moves that file).

`--org` accepts a bare slug, `https://dev.azure.com/<slug>`, or
`https://<slug>.visualstudio.com`; all three reduce to the slug. Neither value
is needed unless `--sync` is used, and an unresolved value fails with the
missing flag, variable, and command spelled out.

## Sync

`--sync` runs before the TUI opens and blocks until it finishes. It queries
every id in the project with WIQL:

```sql
SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project ORDER BY [System.Id]
```

then reads those ids in batches of 200 from `/_apis/wit/workitems` with
`$expand=relations`, and replaces the cached work items and relations in one
transaction. The status line reports how many work items were synced.

Fields map onto the cache as follows:

| Cache | Azure DevOps |
|---|---|
| `organization` | the org slug, not the URL |
| `project`, `work_item_type`, `title`, `state`, `reason` | the matching `System.*` fields |
| `assigned_to` | `System.AssignedTo` display name, falling back to the unique name |
| `priority` | `Microsoft.VSTS.Common.Priority` |
| `tags` | `System.Tags`, split on `;` |
| `description` | `System.Description` HTML flattened to plain text |
| `created_at`, `changed_at` | `System.CreatedDate` and `System.ChangedDate` |
| `web_url` | `https://dev.azure.com/<org>/<project>/_workitems/edit/<id>` |

Hierarchy links become parent and child relations; related, predecessor,
successor, and duplicate links are stored as themselves. Other link types, such
as attachments, are ignored.

Sync is currently a full pull at startup. Periodic background refresh is planned
(#608). Comments and revision history are not synced yet: their tables exist and
the details pane renders them when present, but nothing fills them. `r` reloads
from SQLite only and never contacts Azure DevOps.

## Controls

| Input | Action |
|---|---|
| `↑`/`↓`, `j`/`k` | Move the ticket selection, family row, or focused details pane |
| `Page Up`/`Page Down` | Move ten tickets or one family page |
| `Home`/`End` | Select the first/last ticket, family row, or details line |
| `/` | Focus live fuzzy search |
| `←`/`→`, `Home`/`End` | Move the search cursor while searching |
| `↑`/`↓` | Move the ticket selection while searching |
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
| `w` | Show or hide (`Space`), reorder (`J`/`K`), and resize (`<`/`>`) columns |
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
| `r` | Reload from the cache and rebuild the search index in the background |
| `i` | Show cache path, row counts, and data freshness |
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
the caret in search, the command palette, and the named-view editor.
Scrollbar tracks page by a viewport-minus-one step; thumbs can be
dragged. Right-click, double-click, and horizontal wheel gestures are not
used. Terminals supporting OSC 22 show a browser-style pointer over external
URL targets.

Search accepts a compact grammar such as `state:active type:bug
assignee:"Avery Chen" priority:1 tag:rust`, plus `project:`, `area:`, and
`iteration:`. Values in the same field are combined with OR; different fields
are combined with AND. `is:bookmarked` limits the table to locally bookmarked
tickets. Active filters appear as removable chips. The command palette copies
IDs, URLs, titles, Markdown links, or summaries and exports the selection as
JSON or CSV. Press `i` for cache path, row count, and freshness. Local SQLite
changes reload automatically; the table title shows `Stale` until the reload
finishes.

Ticket states and priorities use restrained semantic colors, work-item types
and tags render as compact badges, and matched search characters are
underlined in visible results. Changed dates use compact relative labels, and
exact UTC timestamps remain available in details. Press `c` to switch between
compact and comfortable row density. Named views, column layout, bookmarks,
and the last query are saved beside the cache as `*.session.json`. Set the
standard `NO_COLOR` environment variable to use the monochrome theme.

## Database reference

The default cache is `ticket-tui/tickets.sqlite3` under the platform data
directory:

- macOS: `~/Library/Application Support/ticket-tui/tickets.sqlite3`
- Linux: `$XDG_DATA_HOME/ticket-tui/tickets.sqlite3`, normally
  `~/.local/share/ticket-tui/tickets.sqlite3`

The `work_items` table stores these columns:

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
semicolon separation. The `work_item_relations`, `work_item_comments`, and
`work_item_history` tables hold the graph around each work item.

The cache carries `PRAGMA user_version = 5`. Because it is a cache rather than a
record of truth, there are no migrations: a database at any other version has
its tables dropped and recreated, and the next `--sync` refills it. Deleting the
file has the same effect.

The TUI displays cached records but never edits them. Parent and child links
render as an always-expanded family tree in the details pane. Click a family
row, or press `Enter` on the family cursor, to select that ticket in the table.
Fuzzy search covers ID, title, assignee, state, type, area, iteration, and tags;
it intentionally excludes descriptions. Structured `field:value` tokens are
parsed out of the query before fuzzy matching.

The application uses WAL mode and a busy timeout so external SQLite readers can
query the cache while the TUI is running.

## Live agent context

While ticket-tui is running, it atomically publishes a compact JSON snapshot
beside the cache. For `tickets.sqlite3`, the file is `tickets.context.json`.
This is the supported interface for an LLM agent to understand the current view
without scraping terminal cells or causing SQLite reloads.

The versioned snapshot includes:

- selected and checked tickets;
- the rows currently rendered in the ticket viewport, plus matching and total
  counts;
- the complete query, fuzzy text, and parsed filters;
- sort order, named view, mode, focused pane, family cursor, and details scroll;
- cache path, process ID, and last-change timestamp.

The file is replaced after meaningful rendered-state changes and removed on a
clean exit. A crash or forced termination can leave a stale file, so consumers
must check its process ID and treat stale data as the last observed view.

This repository includes the `ticket-tui-context` agent skill and a compact
reader:

```console
uv run .agents/skills/ticket-tui-context/scripts/read_context.py
```

Pass `--database PATH` for a custom cache, `--json` for the exact snapshot,
or `--details` to join the selected ticket to its full SQLite records. See the
skill's [context schema reference](.agents/skills/ticket-tui-context/references/context-schema.md)
for field-level semantics.

## Roadmap

Planned work — periodic background refresh, editing work items, and creating
them from the TUI — is tracked as work items in the same Azure DevOps project
ticket-tui is pointed at. Sync the cache and browse it for the current list.

## Develop and verify

Run the same checks used by CI:

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```

CI exercises these checks on current macOS and Linux runners.
