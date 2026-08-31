# Live context JSON reference

A **running** ticket-tui publishes a versioned JSON document beside its SQLite
database after each meaningful rendered-state change. For `tickets.sqlite3` the
context path is `tickets.context.json`. The file is written atomically and
removed on a clean exit, so its absence means no TUI is running — read the
backlog with `ticket-tui list` instead.

## Contents

- [Top-level fields](#top-level-fields)
- [Sync fields](#sync-fields)
- [Pending edit fields](#pending-edit-fields)
- [Search fields](#search-fields)
- [Ticket viewport fields](#ticket-viewport-fields)
- [Reading it](#reading-it)

The file can survive a crash or forced termination. `process_id` is a
best-effort liveness check, and `updated_at` is the UTC RFC 3339 time of the
last state change. Neither field is a heartbeat guarantee.

One live publisher per database is supported. If several ticket-tui processes
open the same database they share one context path and the last rendered write
wins.

This document is the live *view*. It is not the backlog: it carries only the
rendered viewport and the current selection of each tab. For the data itself use
[cli.md](cli.md), and for records the CLI does not print use
[database.md](database.md).

## Top-level fields

Current contract: **schema version 3**. A reader that finds another version
should refuse rather than guess. A field may be added to a block within a
version — `tickets.finished_hidden` was — so a reader should ignore fields it
does not know rather than refuse them; only removing or reshaping a field
already documented here bumps the version.

Version 3 describes **every tab**, whether or not the user is looking at them:
`active_tab` says where they are, and the blocks under it say what each tab
holds. Everything version 2 kept at the top level moved under `work_items`
unchanged.

| Field | Type | Meaning |
|---|---|---|
| `schema_version` | integer | Context contract version; currently `3` |
| `process_id` | integer | PID of the ticket-tui process that wrote the file |
| `updated_at` | string | UTC RFC 3339 time of the published state change |
| `database_path` | string | SQLite database backing the view |
| `me` | string or null | Signed-in display name recorded by sync, including the background refresh, overridden by `TICKET_TUI_ME`; null when neither is set |
| `sync` | object | Where the rows are pulled from and how the last pull went |
| `pending_edits` | pending edit array | Edits sent to Azure DevOps and not answered yet |
| `active_tab` | string | `work_items`, `repos`, `pull_requests`, `pipelines`, `aks`, `acr`, or `key_vault` |
| `work_items` | object | The Work items tab |
| `repos` | object | The Repos tab |
| `pull_requests` | object | The Pull requests tab |
| `pipelines` | object | The Pipelines tab |
| `aks` | object | The AKS tab |
| `acr` | object | The ACR tab |
| `key_vault` | object | The Key Vault tab |
| `arm` | object | What the ACR and Key Vault tabs can reach at all |

### `work_items`

| Field | Type | Meaning |
|---|---|---|
| `mode` | string | `browse`, `search`, or the active overlay name |
| `focus` | string | `tickets`, `family`, or `details` |
| `screen` | string | `workspace` or the narrow-layout `details` screen |
| `active_view` | string or null | Loaded named view, if any |
| `search` | object | Query, parsed filters, pending state, and ordering |
| `sort` | object | Sort field, direction, and row density |
| `tickets` | object | Result counts, whether finished work is hidden, viewport position, and rendered rows |
| `selected_ticket` | ticket or null | Ticket driving the details pane |
| `checked_tickets` | ticket array | Multi-select set used for bulk actions |
| `family_cursor` | ticket reference or null | Keyboard cursor within the family tree |
| `details_scroll_line` | integer | Zero-based details-pane scroll line |

### `repos`

| Field | Type | Meaning |
|---|---|---|
| `selected` | repo or null | The row the cursor is on |
| `visible_rows` | repo array | Every row the filter leaves on the table |
| `workspace` | string or null | Where clones are looked for and made |

A repo carries `id`, `name`, `default_branch`, `is_disabled`, `pull_requests`
and `pipelines` (how many are open against it), `web_url`, and `local` — null
for a repository that is not on this machine, otherwise `path`, `branch`,
`dirty`, `ahead`, `behind`, and `busy` (`cloning`, `fetching`, `pulling`, or
null).

### `pull_requests`

| Field | Type | Meaning |
|---|---|---|
| `selected` | pull request or null | The row the cursor is on, with its details |
| `visible_rows` | row array | Every row the filter leaves on the table |
| `to_review_count` | integer | How many are waiting on the signed-in user's vote |
| `closed_shown` | boolean | Whether closed pull requests are on the table |

A row carries `id`, `repo`, `title`, `author`, `status`, `is_draft`,
`source_branch`, `target_branch`, `merge_status`, `my_vote` and `web_url`. The
selected one adds `reviewers` (`name`, `vote`, `is_required`), `work_items`,
`build` (`status`, `run_id`), `auto_complete`, `thread_count` and
`unresolved_threads`. Votes are the API's scale: `10` approved, `5` approved
with suggestions, `0` none, `-5` waiting, `-10` rejected.

### `pipelines`

| Field | Type | Meaning |
|---|---|---|
| `level` | string | `pipelines` or `runs` — which list the tab is showing |
| `selected_pipeline` | pipeline or null | `id`, `name`, `folder`, `repo`, `web_url` |
| `selected_run` | run or null | The run the details pane is on |
| `following_log` | object or null | The log being tailed: `run_id`, `log_id`, `node`, `line_count`, `following` |
| `running` | integer | How many runs are going right now |
| `watched` | integer array | The runs `w` is following |
| `pending_approvals` | integer | How many approvals the project is waiting on |

A run carries `id`, `pipeline_id`, `build_number`, `status`, `result`,
`branch`, `requested_for`, `started_at`, `finished_at`, `web_url`, and `stages`
— the top level of the timeline only, each with `name`, `state` and `result`.
The whole tree would be longer than the rest of the document; read it with
`ticket-tui runs show <id> --json`.

Runs, timelines and logs are live rather than stored, so this block is only as
current as the watcher's last poll, and it is empty on a run that has never
opened the tab.

### `aks`

| Field | Type | Meaning |
|---|---|---|
| `clusters` | string array | The clusters `config.toml` names, in file order; left out when there are none |
| `selected` | pod or null | The pod the cursor is on |
| `visible_rows` | integer | How many rows the query leaves on the table |
| `unhealthy` | integer | How many of those pods somebody has to look at |
| `following_log` | object or null | The log the text pane is tailing |
| `errors` | string array | One line per `(cluster, namespace)` that could not be read; left out when every read succeeded |

A pod carries `cluster` (the name `config.toml` gives it, not the kubeconfig
context), `namespace`, `name`, `status` (the STATUS word — `Running`,
`CrashLoopBackOff`, `Init:1/2`), `ready` (`1/2`, containers ready over
containers in the spec), `restarts`, `node`, `owner` (`Deployment/orders-api`,
or null for a bare pod — a pod with no owner is not restartable), `created_at`
(UTC RFC 3339), `containers`, and `repo` — the repository on file its image or
app label names, null when nothing matches.

A container carries `name`, `image`, `state` (`Running`, or the reason it is
waiting or has stopped: `CrashLoopBackOff`, `Completed`, `ExitCode:137`) and
`restarts`.

`following_log` carries `pod`, `container` (null while the pane follows the
first), `previous` (whether it is the run before the last restart), `line_count`
and `following` (whether the pane is pinned to the tail rather than scrolled
back).

Pods are read live through `kubectl` and never stored, so this block is only as
current as the last read — 15 seconds at most while the tab is showing, and
however long ago it was left when it is not. `errors` is the one place a cluster
that cannot be reached shows up: its pods are simply missing from
`visible_rows`, not zeroed. `ticket-tui pods` is the same read without the TUI.

### `arm`

Whether the two subscription tabs have anything to read. Both are on the bar on
every run; a run with no subscription draws them empty and says why here.

| Field | Type | Meaning |
|---|---|---|
| `subscription` | string or null | The subscription id the two tabs read; null when none was resolved |
| `offline` | boolean | Nothing can be read — no subscription, or the Azure CLI refused |
| `last_error` | string or null | The one line that says why, such as `run \`az login\`` |

The subscription is `--subscription`, else `TICKET_TUI_SUBSCRIPTION`, else
whichever one `az account set` left the CLI on. A personal access token opens
Azure DevOps and nothing else, so a PAT-only run is ARM-offline with the work
items tab working normally.

### `acr`

The ACR tab: the subscription's container registries, and the repositories of
whichever one is open.

| Field | Type | Meaning |
|---|---|---|
| `level` | string | `registries` or `repositories` — which list the tab is showing |
| `selected_registry` | registry or null | The registry the cursor is on, or the one that was drilled into |
| `selected_repository` | repository or null | The repository the cursor is on; null at the registries level |
| `selected_tag` | tag or null | The tag the details pane's own cursor is on |
| `visible_rows` | integer | How many rows the query leaves on the table |

A registry carries `name`, `resource_group`, `sku`, `location`, `login_server`
(the host a pull reference starts with) and `portal_url`.

A repository carries `name`, `tags` and `updated` — the last two null until the
attributes call has landed, because a catalog listing is names and nothing else.

A tag carries `name`, `digest` (the full `sha256:…`) and `created`.

Registries are read from Resource Graph every 60 seconds while either ARM tab is
showing, and a registry's own catalog, tags and manifest are read once per
focus, so this block is only as current as the last read. Nothing here is
stored: `ticket-tui acr list` is the same read without the TUI.

### `key_vault`

The Key Vault tab, the same way round: the subscription's vaults, and what one
of them holds.

| Field | Type | Meaning |
|---|---|---|
| `level` | string | `vaults` or `items` — which list the tab is showing |
| `selected_vault` | vault or null | The vault the cursor is on, or the one that was drilled into |
| `selected_item` | item or null | The secret, key or certificate the cursor is on |
| `visible_rows` | integer | How many rows the query leaves on the table |
| `expiring_certificates` | integer | Certificates already lapsed or within 30 days of it, across every vault whose items have been read — the same count the tab bar badges `◇N` |

A vault carries `name`, `resource_group`, `location`, `sku`, `uri` (the
data-plane host its items are read from) and `portal_url`.

An item carries `kind` (`secret`, `key`, or `cert`), `name`, `enabled`,
`updated`, `expires` (null for one that never lapses), and `revealed`.

**`revealed` is the only thing this file says about a value.** It is true while
the user has pressed `R` on that secret and the minute has not run out; the
value itself is nowhere in the document, and there is no field for it. A value
is read out of the vault for the screen alone, once, and this file is written to
disk. `ticket-tui secrets show --vault V NAME --value` is the one way to read
one from outside the TUI, and it is an audited read — see
[cli.md](cli.md#secrets-keys-certs).

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

These live under `work_items`. `search.query` preserves the complete user input. `search.fuzzy_text` contains
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

All of these live under `work_items`.

`tickets.total_count` is the number of work items loaded from SQLite.
`tickets.matching_count` is the complete filtered/search result count.
`tickets.viewport_start` is the zero-based first rendered result index, and
`tickets.viewport_size` is the current row capacity. `tickets.visible_rows`
contains only rows rendered in that viewport — never answer "there are N work
items" from its length.

`tickets.finished_hidden` is true when the TUI is leaving finished work — the
Completed and Removed state categories — off the table. It is on by default and
turned over in the TUI, and `search.query` does not say so: it is a rule the app
applies beside the query, equivalent to an added `state:@open`. While it is
true, `matching_count` and `visible_rows` are the open backlog rather than
everything the query matches, so `total_count - matching_count` includes the
finished rows as well as the filtered ones. A query naming a state of its own —
`state:done`, `state:@open` — takes over from the rule and reports
`finished_hidden: false`. The details pane and the family tree are unaffected,
so `selected_ticket` and `family_cursor` can name a work item no visible row
does. To read finished work while it is true, use the CLI: `ticket-tui list`
has no such rule and reads the database directly.

Ticket objects carry `organization`, `project`, `id`, `work_item_type`, `title`,
`state`, `assigned_to`, `priority`, `tags`, `web_url`, `bookmarked`, and
`checked`. The selected one also carries `related` when it has artifact links:
one entry per pull request, commit or build the work item was worked on with,
each with `kind` (`pull_request`, `commit`, `build`), `name` as Azure DevOps
labels the link, `repo`, `target` (the id or the commit sha), and `in_database`
— which is what tells you whether `ticket-tui prs show` or `runs show` will find
it. The field is left out entirely when there are none. A family cursor is only a ticket reference, with `organization` and
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
