---
name: ticket-tui-context
description: Read and change an Azure DevOps project with ticket-tui. Use when the task touches this project's work items, epics, backlog, pull requests, reviews, repositories or CI — reading what is there, flipping a state, voting on a pull request, or starting a build and waiting for it — reading the subscription's container registries and key vaults — and use it before falling back to `az boards`, `az acr`, `az keyvault` or the Azure DevOps REST API.
---

# ticket-tui

ticket-tui is a terminal front-end for one Azure DevOps project: work items,
repositories, pull requests and pipelines, on the first four of eight tabs. It
keeps a local SQLite database of what the project holds and writes changes
straight back to Azure DevOps. Azure DevOps is the record of truth; the database
is a durable local copy of it that survives across runs, not a scratch cache.
Tabs `5`–`8` have no database behind them at all: AKS reads clusters through
`kubectl`, ACR and Key Vault read one Azure subscription through Resource
Manager, both live, and Environments renders the deployment repository's
kustomize overlays off this machine.

Eight surfaces, each with a read, a write and a live view:

| Surface | Read (from SQLite) | Write (to Azure DevOps) | In the live context |
|---|---|---|---|
| Work items | `list`, `show` | `edit`, `comment`, `create` | `work_items` |
| Repositories | `repos list`, `repos show` | — (clone/fetch/pull are the TUI's own keys) | `repos` |
| Pull requests | `prs list`, `prs show` | `prs vote`, `complete`, `abandon`, `autocomplete`, `comment` | `pull_requests` |
| Pipelines | `pipelines`, `runs list` | `runs trigger`, `cancel`, `retry`; `approvals approve`/`reject` | `pipelines` |
| AKS pods | `pods` (live, through `kubectl`) | — (restart and shell are the TUI's keys `x` and `s`) | `aks` |
| Registries | `acr list`, `acr show`, `acr repos list`, `acr tags list`, `acr tags show` (live, through ARM) | — read-only, no untag and no delete | `acr`, `arm` |
| Key vaults | `vaults list`, `vaults show`, `secrets list`, `secrets show`, `keys list`, `certs list` (live, through ARM) | — read-only, no create, set or delete | `key_vault`, `arm` |
| Environments | `env list`, `env show`, `env check`, `env diff` (live, through `kubectl kustomize`) | — read-only; nothing here applies anything | `environments` |

`runs show`, `runs logs`, `runs wait` and `approvals list` read Azure DevOps
rather than the database: a timeline, a log and a run's own progress are not
things a pull stores.

## How to invoke it

`ticket-tui` if it is installed, else `cargo run -q --release --` from the root
of a checkout of this repository — which is also where the `uv run .agents/…`
paths below resolve from. Every example writes `ticket-tui`. `--database`,
`--org`, `--project`, `--code-project`, `--workspace` and `--subscription` —
the last repeatable — are global and may be written either side of the
subcommand. Each falls back to `TICKET_TUI_*` and then to
`~/.config/ticket-tui/config.toml`, which is where a machine says its
organization, its projects and its subscriptions once, so none of them normally
needs writing. Errors go to stderr as `error: …` and exit 1.

The work items may live in one project and the code in another: `project` in
that file is the board, `code_project` is the repositories, pull requests and
pipelines, and left out they are the same project.

## Read the backlog

```console
ticket-tui list                                   # every work item, newest change first
ticket-tui list --query 'state:doing assignee:@me'
ticket-tui list --query 'type:Epic' --json
ticket-tui show 627                               # one work item, description included
ticket-tui show 627 --json
```

`list` prints `#id  state  type  assignee  title`, or `no matching work items`;
`--json` prints one object per work item and `show --json` adds `description`.

`--query` takes the TUI's own grammar: `field:value` pairs narrow — values in
one field ORed, different fields ANDed — and whatever is left over is matched
fuzzily and orders the rows. `state`, `type`, `assignee`, `priority`, `project`,
`area`, `iteration`, `tag`, `id`, and the date comparisons `changed:<7d` and
`created:>=2026-08-01`. `assignee:@me` is whoever the last sync signed in as.
Every tab has its own vocabulary in the same shape:
[references/filters.md](references/filters.md).

`list` and `show` need no Azure DevOps organization configured: they only read
the file. Read [references/database.md](references/database.md) before querying
SQLite directly — rarely necessary, but comments, revision history, the
parent/child graph and the artifact links are only there.

## Change a work item

```console
ticket-tui edit 627 --state Doing --tags agents,docs
ticket-tui comment 627 "Rewrote the skill; gates green."
ticket-tui create --type Issue --title "Stale ticket highlighting" --parent 624
```

Prefer these over `az boards` or raw REST. They speak the same API the TUI does,
they lead each write with the revision the database holds — so a work item
somebody else moved on is refused instead of silently overwritten — and they
store the copy Azure DevOps answers with, so a TUI the user has open shows the
change within a second. `az boards` does none of that, and hand-rolled REST
against this organization needs the MSA header below.

`edit` prints `#627 rev 5: State → Doing, Tags → agents; docs`; every named
field travels in one document. A write refused because the work item moved on
says `run \`ticket-tui sync\` and try again` — do exactly that, re-read it, and
retry. Posting a comment bumps the revision too, so **comment first, then set
the state**: the other order costs a sync.

Full flag list and JSON shapes: [references/cli.md](references/cli.md).

## Repositories, pull requests and pipelines

```console
ticket-tui repos list                              # name branch prs pipelines local
ticket-tui prs list                                # !id repo author votes build title
ticket-tui prs list --query 'reviewer:@me vote:none'   # what is waiting on you
ticket-tui prs show 11 --json
ticket-tui pipelines
ticket-tui runs list --pipeline 'ticket-tui CI'
ticket-tui runs show 14                            # header and timeline tree
ticket-tui runs logs 14 --job Build --follow
```

`repos show NAME` prints one. `repos` also reads the workspace —
`--workspace PATH`, else `TICKET_TUI_WORKSPACE`, else `~/Development` — so the
Local column says what `git status` says; it never fetches. Cloning, fetching
and pulling are the TUI's keys `C`, `G` and `P` rather than subcommands: an
agent that wants a clone should run `git` itself.

The writes:

```console
ticket-tui prs vote 11 approve                     # approve | suggest | wait | reject | none
ticket-tui prs comment 11 "One nit, otherwise good."
ticket-tui prs complete 11 --strategy squash
ticket-tui prs abandon 12
ticket-tui prs autocomplete 11 on
ticket-tui runs trigger 'ticket-tui CI' --branch main
ticket-tui runs cancel 14
ticket-tui runs retry 14
ticket-tui approvals list
ticket-tui approvals approve abc-123 --comment 'ship it'
```

A completion carries the head commit the stored copy was read at, so a merge
that raced somebody else's push is refused rather than landing over it. Run
`ticket-tui sync` and try again.

## Waiting for a build

```console
ticket-tui runs wait 14
ticket-tui runs trigger 'ticket-tui CI' --branch main --follow
```

`wait` polls at the watcher's own cadence until the run stops; `--follow` tails
the deepest running node's log as it goes. Both exit with the result rather than
making you parse anything — **0** succeeded, **1** failed, **2** canceled,
**3** partially succeeded — so branch on `$?` and read the log only when it is
not 0.

## Registries and key vaults

Tabs `6` and `7`, and the five command groups behind them, read one **Azure
subscription** rather than the Azure DevOps project — a different service, a
different login. Nothing is stored: every invocation asks the subscription
again.

```console
ticket-tui acr list                                    # name rg sku location login-server
ticket-tui acr show myacr
ticket-tui acr repos list --registry myacr             # repository tags manifests updated
ticket-tui acr tags list --registry myacr --repo team/orders-api
ticket-tui acr tags show --registry myacr --repo team/orders-api 1.2.3 --json
ticket-tui vaults list                                 # name rg location sku uri
ticket-tui vaults show atlas-kv
ticket-tui secrets list --vault atlas-kv               # names, dates, never values
ticket-tui secrets show --vault atlas-kv orders-db
ticket-tui keys list --vault atlas-kv
ticket-tui certs list --vault atlas-kv                 # with how far off each expiry is
```

The subscriptions are `--subscription ID`, repeatable, else
`TICKET_TUI_SUBSCRIPTION` naming one, else `subscriptions` under `[azure]` in
`~/.config/ticket-tui/config.toml` naming several, else whichever one
`az account set` left the CLI on; with none of the four every one of these
commands is an error rather than an empty listing. `registries` and `vaults` in
that same table narrow the listings to the ones worth seeing, in the order they
are written, so `acr list` and `vaults list` show what matched rather than
everything fifty subscriptions hold. `az login` must have happened — a personal
access token opens Azure DevOps and nothing else, so a PAT-only run reads no
subscription at all and the live context says so in `arm.offline` and
`arm.last_error`; a tenant other than the login's default wants
`az login --tenant TENANT`.

All eleven forms are read-only. There is no untag, no delete, no create or set,
no IAM and no replication here; the `portal_url` that `acr show` and `vaults
show` print is the way to anything destructive.

**Never run `secrets show --value` unless the user has asked to see that
specific secret; never paste a value into a ticket, a commit, a comment, or the
context.** Reading a value is an audited operation, it is the one command here
that fetches one, and every other form answers "does it exist, is it enabled,
when does it expire" without touching it. The live context has no field for a
value either — `key_vault.selected_item.revealed` says only that one is on the
user's screen this minute.

Flags, columns and JSON shapes: [references/cli.md](references/cli.md).

## Environments: what an overlay declares, and what it forgot

Tab `8` and the four `env` commands read one **deployment repository** — the
kustomize overlays on this machine — rather than Azure DevOps or a
subscription. `[deployment]` in `~/.config/ticket-tui/config.toml` names the
repository and `[[environments]]` names what each environment is made of;
without them every form here says where it looked and does nothing else.

```console
ticket-tui env list                       # name overlays vault registry cluster counts
ticket-tui env show prod                  # every workload, its images and its references
ticket-tui env check prod                 # the pre-deploy gate; exit 1 on any finding
ticket-tui env check                      # every environment the file declares
ticket-tui env check prod --offline       # skip the vault half, which is the half needing a token
ticket-tui env diff qa prod orders        # the promotion, read before it is made
```

`env check` is the one worth wiring into a pipeline: it renders the
environment's own overlays and reports every ConfigMap and Secret key a
workload reads that the overlay never defines, every vault object a provider
pulls that the vault does not hold, every provider pulling from another
environment's vault, and every object in use inside thirty days of its expiry.
It exits **1** for any finding, **0** for clean, and **2** when an overlay
would not render — a gate that could not look must not read as clean.
Everything it says is that a name is **absent**; no value is read, here or
anywhere else in this feature.

`env diff <from> <to> [service]` is the same two environments read against each
other: the keys, vault objects and variables one has that the other has not,
and the image gap read back through the runs to the pull requests and work
items between them. That last half needs the service's own clone; the TUI's
board does not read it, so `env diff` is where the pull request list lives.

When a TUI is running and the user says "is prod ready", "what is prod
missing", or "this service" while on tab `8`, read `environments` from the live
context instead of asking them: it carries every environment, what the cell
under their cursor is missing, and the promotion the details pane is showing.

## How fresh the data is

A running ticket-tui pulls every 60 seconds by default (`--refresh SECONDS`,
`TICKET_TUI_REFRESH`; `0` turns the timer off), and picks up another process's
database writes within a second. Nothing guarantees a TUI is running, so force a
pull yourself before trusting the rows for anything time-sensitive:

```console
ticket-tui sync            # incremental, from the stored watermark
ticket-tui sync --full     # replace everything stored
```

It prints `Synced 100 work items, 4 repos, 1 pipeline, 1 run, 3 pull requests
from jacobragsdale/development`. Anything that stopped the pull is an error and
exits non-zero. Runs, timelines and logs are the exception to all of this: they
are read live by the commands that need them, never stored.

## What the user is looking at

While the TUI runs it publishes `tickets.context.json` beside the database,
describing **every tab whether or not it is showing**, plus `active_tab` — so
you never have to ask the user to press a key. Read it whenever they say
"this ticket", "the selected one", "my pull request", or "the build":

```console
uv run .agents/skills/ticket-tui-context/scripts/read_context.py
uv run .agents/skills/ticket-tui-context/scripts/read_context.py --json
uv run .agents/skills/ticket-tui-context/scripts/read_context.py --details
```

`--database PATH` if the TUI was started with one; `--details` joins the
selected work item to its full SQLite records (description, relations,
comments, history). If the file is missing, no TUI is running — say so and use
the subcommands instead of guessing.

Interpreting what comes back:

- **Freshness first.** `sync.offline` means the run never refreshes;
  `sync.last_error` means the last pull failed; `sync.last_success_at` is when
  the rows were last confirmed against Azure DevOps. When any of those says the
  rows are old, describe them as last-synced values, not live ones.
- **`pending_edits`** are writes sent and not answered. The rows already show
  them optimistically, so report them as in flight, not as stored.
- **`active_tab`** is where the user is. Every other tab is still described, so
  "my pull request" is answerable from the Work items tab.
- **`Selected`** drives the details pane; **`Checked`** is the independent
  multi-select set used by bulk actions and can hold different work items.
- **Visible rows** are only the rendered viewport — compare against the matching
  and total counts before saying "there are N".
- **`work_items.tickets.finished_hidden`** means the table is leaving Done and
  Removed work out, which it does by default and the query does not say. The
  matching count is then the open backlog; `ticket-tui list` has no such rule.
- **A stale-process warning** means the file survived an unclean exit. It is the
  last observed view, not a live one.
- **`arm.offline`** means tabs `6` and `7` have no subscription to read, so
  `acr` and `key_vault` are empty for a reason `arm.last_error` names — not
  because the subscription holds nothing.
- **`key_vault.selected_item.revealed`** says a secret's value is on the
  user's screen right now. The value is not in the file, and asking for it is
  not implied by their looking at it.
- **`environments.reason`** means tab `8` has nothing to draw and says where it
  looked: no `[deployment]`, no `[[environments]]`, or no clone here. The board
  is rendered when the tab is first opened, on `r`, and after a `git pull` of that clone, never on
  a timer, so it is as current as the last render.

Field-level semantics, including exactly when each `sync` field moves:
[references/context-schema.md](references/context-schema.md).

## Working agreement on this project

The backlog lives in Azure DevOps (`jacobragsdale/development`, Basic process:
`To Do` → `Doing` → `Done`), Epics holding Issues. Write the ticket first — one
work item with `Problem` / `Approach` / `Done when:` under the right Epic — set
it to `Doing` when you start so a concurrent agent does not pick it up,
implement it on `main` (no worktrees, no feature branches) with `cargo fmt`,
`clippy -D warnings`, `cargo test --all-targets` and `cargo build --release`
green and a commit and push at every working checkpoint, then comment what
shipped and set it `Done`. Comment first: a comment bumps the revision, so the
state edit after it is the one that needs the fresh copy. The comment says what
landed, what differed from the ticket, and what was left out.

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
```
Children come from the parent/child graph rather than a field, so no `--query`
reaches them: read `work_item_relations`
([references/database.md](references/database.md)), or the family tree in the
TUI's details pane.

**Is my pull request ready to merge?**

```console
ticket-tui prs show 11 --json
```
Ready means all four: `status` `active`, `is_draft` false, `merge_status`
`succeeded` (`conflicts` is the common blocker), every required reviewer with a
positive `vote`, and `build.status` not failing. Then `prs complete 11
--strategy squash`, or `prs autocomplete 11 on` and let the policies finish it.

**What is waiting on me**

```console
ticket-tui prs list --query 'reviewer:@me vote:none'
ticket-tui approvals list
```

**Trigger the build and watch it**

```console
ticket-tui runs trigger 'ticket-tui CI' --branch main --follow; echo "exit $?"
```
It prints the run id, tails the log, and exits with the result. On anything but
0, `runs show <id>` says which node is `✗` and `runs logs <id> --task NAME`
reads it.

**Approve the gate**

```console
ticket-tui approvals list          # id build stage pipeline
ticket-tui approvals approve <id> --comment 'checked the staging smoke'
```

**Is anything crash-looping in qa?**

```console
ticket-tui pods --cluster qa 'status:crashloopbackoff' --json
```
Reads the cluster live; there is no database to be stale. Exit 1 with a line on
stderr means a cluster could not be read, and the pods that *did* answer are
still on stdout. When a TUI is running and the user has a pod under the cursor —
"this pod", "why is it restarting" — read `aks.selected` from the context
instead, which also carries the repository the image names.

**Which tags exist for a service**

```console
ticket-tui acr tags list --registry myacr --repo team/orders-api --json
```
Newest first, each with its digest and when it was made; `acr repos list
--registry myacr` first if you do not know how the catalog spells the
repository. `acr tags show … <tag>` adds the manifest — platform, size — and the
`docker pull` reference. When a TUI is running and the user is on tab `6` —
"this image", "that tag" — read `acr.selected_registry`, `acr.selected_repository`
and `acr.selected_tag` from the context instead of asking them to read it out.

**Which certificates expire this month**

```console
ticket-tui certs list --vault atlas-kv
```
The Expires column says the date and the words — `expires in 12 days`, `expired
10 days ago`. Run `vaults list` first for the vault names. On tab `7` the same
question is `kind:cert expires:<+30d` in the search box (the `+` compares
against *now plus thirty days*, not against an age), and the answer is already
in the context as `key_vault.expiring_certificates`, which is what the `◇N`
badge on the tab counts.

**Is prod ready for this?**

```console
ticket-tui env check prod
ticket-tui env diff qa prod orders
```
The first says what prod would be short of, the second what promoting qa into
it would change. Both read the clone and neither applies anything. On tab `8`
the same two answers are the cell under the cursor and the details pane beside
it, and `environments.diff.lines` in the live context is the second of them
word for word.

**Never** reach for `secrets show --value` on the way to either of these: a name,
a date and an `enabled` flag answer almost every question about a vault, and a
value read is audited and cannot be un-read.

**What changed in this sprint's pull requests**

```console
ticket-tui sync && ticket-tui prs list --query 'status:completed' --json
```
The database holds every active pull request and only a window of recently
closed ones, so take dates from `created`/`closed` rather than assuming the list
is the whole sprint.

**Create a child under the selected Epic**

```console
uv run .agents/skills/ticket-tui-context/scripts/read_context.py   # read Selected
ticket-tui create --type Issue --title "Stale ticket highlighting" --parent 624 \
  --assignee @me --priority 3 --tags manager
```
Prints `#654 rev 1: Issue Stale ticket highlighting`. To give it a body, write
the Markdown to a file and follow with
`ticket-tui edit 654 --description-file /tmp/654.md`.

## If you must call REST directly

Only when no subcommand covers it — deleting a work item, your own WIQL, an
endpoint ticket-tui does not implement. This organization is backed by a
Microsoft personal account, so **every request needs
`X-VSS-ForceMsaPassThrough: true`**; without it Azure DevOps answers `302` with
a sign-in page rather than the resource, which reads like a broken URL. The
token comes from `az account get-access-token --resource
499b84ac-1321-427f-aa17-267ca6975798`. The working `curl` shape, every endpoint
ticket-tui uses, and what a `401` means:
[references/rest-fallback.md](references/rest-fallback.md).

## References

- [references/cli.md](references/cli.md) — every subcommand, flag, output shape,
  and failure mode.
- [references/filters.md](references/filters.md) — the query grammars, one per
  tab, shared with the TUI's search boxes.
- [references/context-schema.md](references/context-schema.md) — the live
  context JSON, schema version 3.
- [references/database.md](references/database.md) — the SQLite tables an agent
  may read directly.
- [references/rest-fallback.md](references/rest-fallback.md) — authentication
  and the MSA pass-through header.
