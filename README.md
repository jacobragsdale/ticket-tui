# ticket-tui

`ticket-tui` is a fast terminal browser for Azure DevOps work items. It keeps a
local SQLite database synced from one Azure DevOps project and reads from that
database, so navigation, sorting, filtering, and fuzzy search stay instant. It provides mouse
navigation, responsive ticket details, field sorting, live fuzzy search, and
links that open in the system browser.

Azure DevOps is the source of truth. A background worker pulls the project's
work items over the REST API every minute and replaces the local rows, so a
state changed in the browser shows up in a running TUI without restarting it;
the database file itself is durable, lives in the platform data directory, and
is the interface other tools and agent skills read. A field changed in the TUI
is written straight back over the same API; everything else it does is local.

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

3. Later runs open the database immediately and pull in the background:

   ```console
   cargo run --release
   ```

4. Press `/`, type part of a ticket title or a filter such as `state:active`,
   and watch the table update. Press `Esc` to leave the search box while
   retaining the filter.

5. Select a ticket and press `o` to open it on `dev.azure.com`.

6. Press `q` to exit.

Press `r` at any time to pull immediately.

`--refresh SECONDS` changes how often the background pull runs, and `--refresh
0` turns the timer off, leaving `r` as the only way to pull:

```console
cargo run --release -- --refresh 300
```

Without a configured organization the TUI runs offline: it browses the database,
never contacts the network, and `r` reports the missing organization. An empty
database then opens to the status line `Database is empty and offline; run with
--sync --org ORG --project PROJECT to pull work items`.

To use another database file:

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

An access token expires in about an hour, well within one session, so a request
Azure DevOps refuses is retried once with a freshly minted token before it is
reported.

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
`https://<slug>.visualstudio.com`; all three reduce to the slug. Without both
values the TUI browses the database offline and never syncs; with `--sync` an
unresolved value fails with the missing flag, variable, and command spelled out.

## Sync

A sync worker pulls in the background on a timer, every 60 seconds by default,
and whenever `r` asks it to. The TUI opens from the database straight away and
the first pull runs behind it, so a state flipped in Azure DevOps appears within
one interval without a keypress. `--sync` instead runs one pull before the TUI
opens and blocks until it finishes; that pull failing is a notification over the
existing database rather than a reason to refuse to start. Only one pull runs at
a time: `r` during one reports `Sync already in progress`.

The table title carries the sync state — `Syncing…`, `Synced just now`,
`Synced 2m ago`, or `Sync failed` until the next success — and `i` shows the
same in the database overlay. A timer pull that keeps failing the same way says
so only in the title; a pull `r` asked for always reports itself.

Pulls are incremental. Each one asks for the work items edited since the last
successful pull, using the watermark that pull left behind:

```sql
SELECT [System.Id] FROM WorkItems
WHERE [System.TeamProject] = @project AND [System.ChangedDate] >= '2026-08-28T20:15:03Z'
ORDER BY [System.Id]
```

The watermark is the greatest `System.ChangedDate` the last pull actually saw,
never a wall clock reading: a client whose clock runs fast would otherwise step
straight past edits it never read. It is stored in the database's `sync_meta`
table as `watermark_changed_at` and written down to the second, so the
comparison is inclusive and the work item it came from is read once more rather
than an edit made in the same second being missed.

Every pull also runs the plain id query:

```sql
SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project ORDER BY [System.Id]
```

Deleting a work item is not an edit — it stops being listed — so this is what
catches one moved to the recycle bin, and the rows it no longer names are
removed along with their links, comments, and history.

Whatever the changed-since query names is read in batches of 200 from
`/_apis/wit/workitems` with `$expand=relations` and written in one transaction,
each work item's own row and outgoing links replaced and everyone else's left
untouched. The watermark advances only after that batch is committed. When
nothing changed and nothing was deleted, nothing at all is written: an idle
project costs exactly two queries a minute, the database's timestamp does not
move, and no other ticket-tui or agent reading the file reloads for nothing.

A pull runs in full — every work item, replacing the stored rows wholesale —
when there is no watermark to start from: a fresh database, a database whose
schema this build rebuilt, or `ticket-tui --sync`, which is the way to rebuild
one deliberately. A full pull leaves a watermark behind, so the pulls that
follow it are incremental again.

A pull that `r` asked for reports itself in the status line:
`Synced 3 changes from <org>/<project>`, `Synced 52 work items from
<org>/<project>` after a full pull, or `Nothing changed`. A timer pull only
updates the table title, which still moves to `Synced just now` when the pull
found nothing.

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

The first pull a work item type appears in also reads
`/_apis/wit/workitemtypes/<type>/states` and stores that type's states in
`work_item_type_states`, which is what the state picker offers. A type the
database already holds states for is not asked about again, so a run that opens
a filled database makes no states requests at all; a request that fails is
retried on the next pull and never sinks it.

The first pull also reads `/_apis/profile/profiles/me` for the signed-in display
name and stores it in the cache's `sync_meta` table. The profile host is
separate from the work-item host, so a failure there is skipped rather than
sinking the sync. Work items assigned to that name render bold in the accent
colour in the Assignee column and in the details pane. Set `TICKET_TUI_ME` to
override the stored name, for anyone whose profile name differs from the name
their work items are assigned to.

Comments and revision history are not synced yet: their tables exist and the
details pane renders them when present, but nothing fills them.

## Editing

Work items are written back to Azure DevOps as they change, one field at a
time, and every edit takes the same path. The row changes in the table straight
away, the sync worker sends a JSON Patch document to
`PATCH /_apis/wit/workitems/<id>`, and what happens next depends on the answer.

The document always leads with a revision test:

```json
[
  {"op": "test", "path": "/rev", "value": 12},
  {"op": "add", "path": "/fields/System.State", "value": "Doing"}
]
```

so Azure DevOps refuses the whole write if the work item changed after it was
loaded. On success the copy Azure DevOps stored — its new revision and changed
date included — replaces the row, is written to SQLite on its own without
touching any other record, and the status line reports
`Updated #613 · State → Doing`. The row then re-sorts and re-filters if the
change moved it, and the selection follows its work item rather than its row
number.

A refusal puts the row back exactly as it was and names the field, so a change
is never dropped quietly. A conflict reads `#613 changed in Azure DevOps since
it was loaded; State not saved — syncing the latest copy` and asks for a pull at
once, so the value somebody else wrote appears. Anything else Azure DevOps
refused is reported as it came.

Edits ride the same worker as pulls and are handled in the order they arrive, so
typing is never blocked and an edit queued before a pull is written before that
pull reads. If a pull finishes while an edit is still in flight, the edit stays
on screen over the rows the pull brought. There is no offline queue: without a
configured organization an edit is refused before anything changes, and an edit
that cannot be sent is reverted rather than saved for later.

`e` opens the Edit menu, which lists the fields that can be changed; `S`
(capital, because `s` is the sort menu) skips it and opens the state picker
directly. The picker lists the states the selected work item's type allows,
coloured by category and with the state it is in already under the cursor.
`Enter` writes the state chosen down the path above, `Esc` changes nothing, and
choosing the state it is already in closes without a write. A transition Azure
DevOps refuses puts the row back and says why.

The picker never waits for the network. It offers the states cached in
`work_item_type_states` when a pull has fetched them, and otherwise the distinct
states already in the database for that type, ordered by category — Proposed,
In Progress, Resolved, Completed, Removed — then by name, so it opens instantly
on a database that has never reached Azure DevOps.

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
| `e` | Open the Edit menu of field editors; `Enter` opens the one chosen |
| `S` | Change the selected work item's state; `Enter` applies, `Esc` cancels |
| `m` | Bookmark or unbookmark the selected ticket |
| `Space` | Toggle ticket multi-select |
| `y` | Copy selected (or current) ticket IDs |
| `[` / `]` | Jump to the previous or next recently viewed ticket |
| `Tab` | Toggle focus between tickets and details |
| `d` | Toggle the details screen when the terminal is under 70 columns |
| `Enter` | Select the family cursor ticket, or open from the details pane |
| `o` | Open the selected ticket in the system browser |
| `r` | Sync from Azure DevOps now, without waiting for the timer |
| `i` | Show database path, row counts, and sync freshness |
| `?` | Show the in-app help; use arrows or page keys to scroll it |
| `q`, `Ctrl-C` | Quit |

The help overlay's Actions section and the palette's key labels are generated
from the same command table these keys are bound in, so a binding reads the same
way everywhere.

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
dragged. Dragging the divider between the Tickets and Details panes resizes
them, both side by side and stacked; the tickets pane keeps at least 40 columns
and details at least 30 side by side, and each keeps six rows when stacked.
`Reset pane split` in the command palette restores the built-in layout.
Right-click, double-click, and horizontal wheel gestures are not
used. Terminals supporting OSC 22 show a browser-style pointer over external
URL targets.

Search accepts a compact grammar such as `state:active type:bug
assignee:"Avery Chen" priority:1 tag:rust`, plus `project:`, `area:`, and
`iteration:`. Values in the same field are combined with OR; different fields
are combined with AND. `is:bookmarked` limits the table to locally bookmarked
tickets. Active filters appear as removable chips. The command palette copies
IDs, URLs, titles, Markdown links, or summaries and exports the selection as
JSON or CSV. Press `i` for database path, row count, freshness, and the last
sync. A database another process writes reloads automatically; the table title
shows `Stale` until that reload finishes, and `Syncing…`, `Synced 2m ago`, or
`Sync failed` for the pulls from Azure DevOps.

States are coloured by category: New, To Do, and Proposed blue; Active, Doing,
and In Progress yellow; Resolved magenta; Done and Closed green; Removed grey;
a state outside those groups stays plain. Work-item types carry fixed badge
colours — Epic yellow, Feature magenta, Issue, User Story, and Product Backlog
Item blue, Task cyan, Bug and Impediment red, Test Case green — priority 1 is
red, 2 yellow, 3 and 4 blue, and each tag is hashed onto a stable badge colour
so one tag reads the same everywhere. Completed and removed rows are dimmed so
open work stands out, the Area and Iteration table columns show only the last
path segment while details keeps the full path, family-tree rows carry a
one-character state glyph (`○ ◐ ● ✓ ✗`), and matched search characters are
underlined in visible results. A hovered row is tinted with a 256-colour
background rather than repainted, so its coloured cells keep their own
foregrounds; hovered controls reverse instead. Setting the standard `NO_COLOR`
environment variable selects the monochrome theme, where weight carries the same
distinctions: badges keep their brackets, finished rows dim instead of fading,
state glyphs and your own work items go bold, and a hovered row reverses.

Changed dates use compact relative labels, and exact UTC timestamps remain
available in details. Press `c` to switch between compact and comfortable row
density. Named views, column layout, bookmarks, the pane split, and the last
query are saved beside the cache as `*.session.json`.

## Database reference

The default database is `ticket-tui/tickets.sqlite3` under the platform data
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
`work_item_history` tables hold the graph around each work item. The `sync_meta`
key/value table describes the sync itself rather than the work items, so a full
pull clears the other tables but leaves it alone. Two keys live there:
`me_display_name`, the signed-in display name that marks your own work items,
and `watermark_changed_at`, the greatest `System.ChangedDate` the last
successful pull saw, as an RFC 3339 UTC timestamp. That watermark is where the
next incremental pull starts asking; a database without one is pulled in full
and left with one.

The `work_item_type_states` table holds what the state picker offers:
`work_item_type`, `name`, `category` (`Proposed`, `InProgress`, `Resolved`,
`Completed`, or `Removed`), and `position`, the order the process template lists
the state in, keyed on `(work_item_type, name)`. Like `sync_meta` it describes
the project's process rather than its work items, so a pull leaves it alone; a
type is rewritten whole when its states are fetched, so a retired state stops
being offered.

The database carries `PRAGMA user_version = 7`. Because Azure DevOps is the
record of truth, there are no migrations: a database at any other version has
its tables dropped and recreated at startup, and a pull runs immediately to
refill it, whatever `--refresh` says. Deleting the file has the same effect. The
sync worker and background reloads instead open the database without touching
its schema and report the version mismatch, ending in `restart ticket-tui`, so a
running instance can never empty a database a newer build owns. After upgrading
the binary, restart any running ticket-tui.

An edited work item is written to Azure DevOps first and stored from the copy
that comes back, so these records only ever hold what the server accepted.
Parent and child links render as an always-expanded family tree in the details
pane. Click a family row, or press `Enter` on the family cursor, to select that
ticket in the table. Fuzzy search covers ID, title, assignee, state, type, area,
iteration, and tags; it intentionally excludes descriptions. Structured
`field:value` tokens are parsed out of the query before fuzzy matching.

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
- cache path, the signed-in display name that marks your own work items,
  process ID, and last-change timestamp.

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

Planned work — editing work items and creating them from the TUI — is tracked
as work items in the same Azure DevOps project
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
