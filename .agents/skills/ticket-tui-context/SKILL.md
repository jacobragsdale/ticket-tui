---
name: ticket-tui-context
description: Read and change Azure DevOps work items through ticket-tui — the backlog in its SQLite database, the live view the user is looking at, and the `ticket-tui` subcommands that flip a state, leave a comment, or create a work item. Use for any task about this project's tickets, epics, or backlog, and before falling back to `az boards` or the REST API.
---

# ticket-tui

ticket-tui is a terminal browser for one Azure DevOps project. It keeps a local
SQLite database of that project's work items and writes changes straight back
to Azure DevOps. Azure DevOps is the record of truth; the database is a durable
local copy of it that survives across runs, not a scratch cache.

Three surfaces, in the order you will want them:

| Surface | What it is for |
|---|---|
| `ticket-tui list` / `show` | Read the backlog. Answers from SQLite, never touches the network. |
| `ticket-tui edit` / `comment` / `create` | Change work items. Writes to Azure DevOps, then stores the copy it answers with. |
| `tickets.context.json` | What the user is looking at *right now* in a running TUI: selection, checked set, query. Only exists while the TUI runs. |

## How to invoke it

`ticket-tui` if it is installed. From the root of a checkout of this repository
— which is also where the `uv run .agents/…` paths below resolve from — it is:

```console
cargo run -q --release -- list --query 'state:doing'
```

Every example below writes `ticket-tui`; substitute `cargo run -q --release --`
if the binary is not on `PATH`. `--database`, `--org`, and `--project` are
global and may be written either side of the subcommand. Errors go to stderr as
`error: …` and exit 1.

## Read the backlog

```console
ticket-tui list                                   # every work item, newest change first
ticket-tui list --query 'state:doing assignee:@me'
ticket-tui list --query 'type:Epic' --json
ticket-tui show 627                               # one work item, description included
ticket-tui show 627 --json
```

`list` prints `#id  state  type  assignee  title`, one work item per line, or
`no matching work items`. `--json` prints an array of objects keyed
`id, organization, project, rev, type, title, state, assignee, priority, area,
iteration, tags, created, changed, url`; `show --json` adds `description`.

`--query` takes the TUI's own filter grammar. `field:value` pairs narrow —
values in one field are ORed, different fields are ANDed — and whatever is left
over is matched fuzzily and orders the rows. Fields: `state`, `type`,
`assignee`, `priority`, `project`, `area`, `iteration`, `tag`, plus the date
comparisons `changed:` and `created:` (`changed:<7d`, `created:>=2026-08-01`).
`assignee:@me` is whoever the last sync signed in as. Matching is
case-insensitive, and `area:`/`iteration:` also match a bare last path segment.
See [references/filters.md](references/filters.md).

`list` and `show` work with no Azure DevOps organization configured at all: they
only read the file. Read [references/database.md](references/database.md) before
querying SQLite directly — it is rarely necessary, but comments, revision
history, and the parent/child graph are only there.

## Change a work item

```console
ticket-tui edit 627 --state Doing
ticket-tui edit 627 --state Doing --tags agents,docs
ticket-tui comment 627 "Rewrote the skill; gates green."
ticket-tui create --type Issue --title "Stale ticket highlighting" --parent 624
```

Prefer these over `az boards` or raw REST. They speak the same API the TUI
does, they lead each write with the revision the database holds — so a work item
somebody else moved on is refused instead of silently overwritten — and they
store the copy Azure DevOps answers with, so a TUI the user has open shows the
change within a second without a pull. `az boards` does none of that, and hand-
rolled REST against this organization needs the MSA header below.

`edit` prints `#627 rev 5: State → Doing, Tags → agents; docs`. Every named
field travels in one document. A refused write that means the work item moved on
says so and tells you what to do:

```text
error: #627 changed in Azure DevOps since the last sync; run `ticket-tui sync` and try again
```

Do exactly that, re-read the work item, and retry.

Full flag list and JSON shapes: [references/cli.md](references/cli.md).

## How fresh the data is

A running ticket-tui pulls from Azure DevOps every 60 seconds by default
(`--refresh SECONDS`, `TICKET_TUI_REFRESH`; `--refresh 0` turns the timer off
and leaves `r` in the TUI as the only thing that pulls). It also picks up
database writes from any other process within a second.

Nothing guarantees a TUI is running. Force a pull yourself before trusting the
rows for anything time-sensitive:

```console
ticket-tui sync            # incremental, from the stored watermark
ticket-tui sync --full     # replace every stored work item
```

It prints `Synced 6 changes from jacobragsdale/development`, or `Synced 0
changes from …` when it reached Azure DevOps and found nothing new. Anything
that stopped the pull is an error and exits non-zero.

## What the user is looking at

While the TUI runs it publishes `tickets.context.json` beside the database —
the selected work item, the checked set, the query, the visible rows, and a
`sync` block saying how fresh the rows are. Read it whenever the user says
"this ticket", "the selected one", or "what I have checked":

```console
uv run .agents/skills/ticket-tui-context/scripts/read_context.py
uv run .agents/skills/ticket-tui-context/scripts/read_context.py --json
uv run .agents/skills/ticket-tui-context/scripts/read_context.py --details
```

`--database PATH` if the TUI was started with one; `--details` joins the
selected work item to its full SQLite records (description, relations,
comments, history). If the file is missing, no TUI is running — say so and use
`ticket-tui list` instead of guessing.

Interpreting what comes back:

- **Freshness first.** `sync.offline` means the run never refreshes;
  `sync.last_error` means the last pull failed; `sync.last_success_at` is when
  the rows were last confirmed against Azure DevOps. When any of those says the
  rows are old, describe them as last-synced values, not live ones.
- **`pending_edits`** are writes sent and not answered. The rows already show
  them optimistically, so report them as in flight, not as stored.
- **`Selected`** drives the details pane; **`Checked`** is the independent
  multi-select set used by bulk actions and can hold different work items.
- **Visible rows** are only the rendered viewport — compare against the matching
  and total counts before saying "there are N".
- **A stale-process warning** means the file survived an unclean exit. It is the
  last observed view, not a live one.

Field-level semantics, including exactly when each `sync` field moves:
[references/context-schema.md](references/context-schema.md).

## Working agreement on this project

The backlog lives in Azure DevOps (`jacobragsdale/development`, Basic process:
`To Do` → `Doing` → `Done`). Epics hold Issues. The flow for a piece of work is:

1. **Write the ticket first.** One work item, `Problem` / `Approach` /
   `Done when:` in the description, created under the right Epic.
2. **Set it to `Doing` when you start**, so a concurrent agent does not pick it
   up: `ticket-tui edit <id> --state Doing`.
3. **Implement** in an isolated worktree that begins with `git merge main`,
   keeping `cargo fmt --all -- --check`, `cargo clippy --all-targets
   --all-features -- -D warnings`, `cargo test --all-targets`, and `cargo build
   --release` green.
4. **Set it to `Done`** once it is merged and verified:
   `ticket-tui edit <id> --state Done`.

Leave a comment on the work item when the outcome differs from the ticket —
scope dropped, an assumption that turned out wrong, a follow-up worth filing.
`HANDOFF.md` in the repository root points at what is next; the work items
themselves are the backlog.

## Recipes

**List my Doing items**

```console
ticket-tui list --query 'state:doing assignee:@me'
```

**Everything in an Epic**

```console
ticket-tui show 624                      # the Epic, description and all
ticket-tui list --query 'type:Issue tag:agents'
```
Children come from the parent/child graph rather than a field on the work item,
so no `--query` reaches them: read `work_item_relations`
([references/database.md](references/database.md)), or read the always-expanded
family tree in the TUI's details pane.

**Flip a state**

```console
ticket-tui edit 627 --state Doing
ticket-tui edit 627 --state Done
```
The state must be one the process template offers for that type — `To Do`,
`Doing`, `Done` here. A rejected value prints what Azure DevOps said.

**Add a comment**

```console
ticket-tui comment 627 "Merged as 4160277; gates green on macOS and Linux."
```
Plain text, one paragraph. Empty comments are refused.

**Create a child under the selected Epic**

```console
uv run .agents/skills/ticket-tui-context/scripts/read_context.py   # read Selected
ticket-tui create --type Issue --title "Stale ticket highlighting" --parent 624 \
  --assignee @me --priority 3 --tags manager
```
Prints `#654 rev 1: Issue Stale ticket highlighting`. To give it a body, write
the Markdown to a file and follow with
`ticket-tui edit 654 --description-file /tmp/654.md`.

**Take a work item off whoever holds it**

```console
ticket-tui edit 627 --assignee ''
```

**What changed in the last day**

```console
ticket-tui sync
ticket-tui list --query 'changed:<24h'
```

## If you must call REST directly

Only when no subcommand covers it (deleting a work item, querying WIQL
yourself, an endpoint ticket-tui does not implement). This organization is
backed by a Microsoft personal account, so **every request needs
`X-VSS-ForceMsaPassThrough: true`** — without it Azure DevOps answers `302` to
a sign-in page rather than the resource, which reads like a broken URL. The
bearer token comes from the Azure CLI:

```console
az account get-access-token --resource 499b84ac-1321-427f-aa17-267ca6975798 \
  --query accessToken -o tsv
```

Details, including the working `curl` shape and what a `401`/`302` means:
[references/rest-fallback.md](references/rest-fallback.md).

## References

- [references/cli.md](references/cli.md) — every subcommand, flag, output shape,
  and failure mode.
- [references/filters.md](references/filters.md) — the `--query` grammar, shared
  with the TUI search box.
- [references/context-schema.md](references/context-schema.md) — the live
  context JSON, schema version 2.
- [references/database.md](references/database.md) — the SQLite tables an agent
  may read directly.
- [references/rest-fallback.md](references/rest-fallback.md) — authentication
  and the MSA pass-through header.
