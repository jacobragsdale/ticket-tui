# `ticket-tui` subcommand reference

A bare `ticket-tui` opens the TUI. Every subcommand does one thing and exits,
which is what lets an agent read and change work items without a terminal to
drive.

## Contents

- [Global options](#global-options)
- [`sync`](#sync)
- [`show`](#show)
- [`list`](#list)
- [JSON output](#json-output)
- [`edit`](#edit)
- [`comment`](#comment)
- [`create`](#create)
- [Which surface to use](#which-surface-to-use)
- [`repos list`, `repos show`](#repos-list-repos-show)
- [`prs list`, `prs show`](#prs-list-prs-show)
- [The pull-request writes](#the-pull-request-writes)
- [`pipelines`, `runs`](#pipelines-runs)
- [Waiting, and exit codes](#waiting-and-exit-codes)
- [`approvals`](#approvals)
- [`pods`](#pods)
- [`acr`](#acr)
- [`vaults`](#vaults)
- [`secrets`, `keys`, `certs`](#secrets-keys-certs)

```console
ticket-tui sync [--full]
ticket-tui show <id> [--json]
ticket-tui list [--query '<filter>'] [--json]
ticket-tui edit <id> [--state S] [--assignee A] [--priority N] [--iteration I]
                     [--area A] [--title T] [--tags a,b] [--description-file F]
ticket-tui comment <id> "text"
ticket-tui create --type TYPE --title TITLE [--parent ID] [--iteration I]
                  [--assignee A] [--priority N] [--tags a,b]
ticket-tui repos list [--query '<filter>'] [--json]
ticket-tui repos show <name> [--json]
ticket-tui prs list [--query '<filter>'] [--json]
ticket-tui prs show <id> [--json]
ticket-tui prs vote <id> approve|suggest|wait|reject|none
ticket-tui prs complete <id> [--strategy squash|merge|rebase] [--keep-source]
                             [--no-transition]
ticket-tui prs abandon <id>
ticket-tui prs autocomplete <id> on|off
ticket-tui prs comment <id> "text"
ticket-tui pipelines [--json]
ticket-tui runs list [--pipeline NAME] [--query '<filter>'] [--json]
ticket-tui runs show <id> [--json]
ticket-tui runs logs <id> [--job NAME | --task NAME] [--follow]
ticket-tui runs trigger <pipeline> --branch NAME [--follow]
ticket-tui runs cancel <id>
ticket-tui runs retry <id>
ticket-tui runs wait <id>
ticket-tui approvals list [--json]
ticket-tui approvals approve <id> [--comment TEXT]
ticket-tui approvals reject <id> [--comment TEXT]
ticket-tui pods [--cluster NAME] [--namespace NAME] ['<filter>'] [--json]
ticket-tui acr list [--json]
ticket-tui acr show <registry> [--json]
ticket-tui acr repos list --registry NAME [--json]
ticket-tui acr tags list --registry NAME --repo NAME [--json]
ticket-tui acr tags show --registry NAME --repo NAME <tag> [--json]
ticket-tui vaults list [--json]
ticket-tui vaults show <vault> [--json]
ticket-tui secrets list --vault NAME [--json]
ticket-tui secrets show --vault NAME <name> [--json | --value]
ticket-tui keys list --vault NAME [--json]
ticket-tui certs list --vault NAME [--json]
```

## Global options

These are global: valid on every subcommand, either side of it.

| Flag | Meaning |
|---|---|
| `--database PATH` | SQLite database to use instead of the platform data-directory default |
| `--org ORG` | Azure DevOps organization, as a slug or a URL; else `TICKET_TUI_ORG`, else `az devops configure` defaults |
| `--project PROJECT` | Azure DevOps project; else `TICKET_TUI_PROJECT`, else `az devops configure` defaults |
| `--workspace PATH` | Where `repos` looks for clones; else `TICKET_TUI_WORKSPACE`, else `~/Development` |
| `--subscription ID` | Azure subscription the `acr` and vault commands read; else `TICKET_TUI_SUBSCRIPTION`, else whichever one `az account set` left the CLI on |

`--query WIQL`, `--refresh SECONDS`, and `--stale-days DAYS` are **not** global. They must be written before the subcommand, and `ticket-tui sync
--query …` is rejected as an unexpected argument:

```console
ticket-tui --query "[System.Tags] CONTAINS 'agents'" sync   # narrows the pull
```

`--query WIQL` is one extra WIQL condition ANDed into every pull, for a project
too large to hold whole; `TICKET_TUI_QUERY` sets the same thing and is read
whatever the flag placement, which is usually the easier way to reach it from a
subcommand. Two different flags share the name `--query`: this one takes WIQL
and narrows what a **pull** asks Azure DevOps for, while `list --query` takes
the filter grammar and narrows what is **printed** from rows already stored.

`--refresh SECONDS` and `--stale-days DAYS` only mean anything to a run that
opens the TUI; `--stale-days` (or `TICKET_TUI_STALE_DAYS`) sets how long a work
item may sit untouched before the Changed column flags it, which is the
`changed:>Nd state:@open` the **Stale** view asks for. `ticket-tui sync` is the
subcommand that pulls and exits.

Everything a subcommand reports goes to stdout. A failure prints
`error: <message>` on stderr and exits 1.

Environment: `TICKET_TUI_ME` overrides the display name `@me` resolves to,
`AZURE_DEVOPS_EXT_PAT` switches authentication to a personal access token, and
`AZURE_CONFIG_DIR` moves the `az` config the org/project defaults are read from.

## `sync`

Pulls work items into the database and exits.

```console
$ ticket-tui sync
Synced 6 changes from jacobragsdale/development
```

- Incremental by default, starting from the `watermark_changed_at` the last pull
  left behind. `--full` replaces every stored work item.
- A pull that reached Azure DevOps and found nothing prints `Synced 0 changes
  from …` and exits 0 — that is a success, not a failure.
- A database already holding another organization/project refuses an incremental
  pull: `database holds other-org/borealis; pass --database for another project
  or --full to replace it`. `--full` is how the swap is asked for.
- Throttling exits non-zero with `Azure DevOps is throttling requests; try again
  in Ns`.
- A running TUI notices the new rows through its file watcher, within a second.

## `show`

Prints one work item from the database. Never touches the network, so it works
with no organization configured.

```console
$ ticket-tui show 627
#627 Issue · Doing · rev 2
Update the ticket-tui-context skill

Assignee  Jacob Ragsdale
Priority  3
Area      development
Iteration development
Tags      agents
Created   2026-08-29T01:05:43.49Z
Changed   2026-08-29T02:55:20.78Z
URL       https://dev.azure.com/jacobragsdale/development/_workitems/edit/627

Problem. …
```

Empty fields are left out of the block. The description is the flattened plain
text, printed last. A work item that is not stored is an error naming the
database and suggesting `ticket-tui sync`.

## `list`

Prints work items from the database. Also read-only and offline.

```console
$ ticket-tui list --query 'state:doing'
#627  Doing  Issue  Jacob Ragsdale  Update the ticket-tui-context skill
#621  Doing  Issue  Jacob Ragsdale  New work item form
#629  Doing  Issue  Jacob Ragsdale  Built-in saved views
```

Columns are `#id`, state, type, assignee, title, padded to the widest value; an
unassigned work item shows `—`. No match prints `no matching work items` and
exits 0.

- `--query` is the filter grammar in [filters.md](filters.md), not WIQL.
- Without a fuzzy term the rows come back newest change first, ties broken by
  descending id. With one they come back in relevance order.
- `assignee:@me` resolves to `me_display_name` from the last sync, overridden by
  `TICKET_TUI_ME`. With neither it is an error, not a silent match-everything.
- `is:bookmarked` matches nothing here: bookmarks live in the TUI's session
  file, which a one-shot read does not open.

## JSON output

`--json` on `list` prints an array, on `show` a single object. Field names are
the ones the filter grammar uses, not the Azure DevOps reference names. An unset
field is `null`.

```json
{
  "id": 629,
  "organization": "jacobragsdale",
  "project": "development",
  "rev": 3,
  "type": "Issue",
  "title": "Built-in saved views",
  "state": "Doing",
  "assignee": "Jacob Ragsdale",
  "priority": 3,
  "area": "development",
  "iteration": "development",
  "tags": ["manager"],
  "created": "2026-08-29T01:05:44.08Z",
  "changed": "2026-08-29T06:45:29.72Z",
  "url": "https://dev.azure.com/jacobragsdale/development/_workitems/edit/629"
}
```

`show --json` adds `"description"`, the flattened plain text. `list --json`
never carries a description: a list of five hundred rows is no place for five
hundred descriptions.

## `edit`

Changes fields in Azure DevOps and stores the copy that comes back.

| Flag | Notes |
|---|---|
| `--state STATE` | Must be a state the process template offers for that type |
| `--assignee A` | Display name, sign-in address, or `@me`. `--assignee ''` unassigns |
| `--priority N` | As the process template numbers them |
| `--iteration I` | Full iteration path, such as `development\Sprint 1` |
| `--area A` | Full area path |
| `--title T` | An empty title is refused |
| `--tags a,b` | **Replaces** the whole tag list. Commas on the command line, stored as Azure DevOps semicolons |
| `--description-file PATH` | Reads Markdown and writes the HTML Azure DevOps stores |

```console
$ ticket-tui edit 627 --state Doing --tags agents,docs
#627 rev 5: State → Doing, Tags → agents; docs
```

Every field named in one invocation travels in one JSON Patch document, led by
a test on the revision the database holds, so a work item somebody else moved on
is refused rather than overwritten:

```text
error: #627 changed in Azure DevOps since the last sync; run `ticket-tui sync` and try again
```

Run `ticket-tui sync`, re-read the work item, and retry. A work item the
database has never seen goes out without a revision test. Passing no field flag
at all is an error listing the flags.

## `comment`

```console
$ ticket-tui comment 627 "Rewrote the skill; gates green."
#627 comment 41 posted
```

Plain text, wrapped in one paragraph and HTML-escaped. Markdown is not rendered.
An empty or whitespace-only comment is refused. Nothing is written locally
unless the post landed.

## `create`

```console
$ ticket-tui create --type Issue --title "Stale ticket highlighting" --parent 624
#654 rev 1: Issue Stale ticket highlighting
```

`--type` is any type the process template offers (`Epic`, `Issue`, `Task`, …).
`--parent ID` links the new work item under an existing one, and the stored copy
carries the link, so the family tree shows it without a pull. `--iteration`,
`--assignee`, `--priority`, and `--tags` behave as they do on `edit`; there is
no `--description-file` on `create`, so write the body with a following
`ticket-tui edit <new-id> --description-file PATH`. An empty title is refused.

## Which surface to use

- Reading one or many work items → `show` / `list`. No network, no rate limit.
- Changing anything → `edit` / `comment` / `create`. They carry the revision
  guard and keep the database and any running TUI in step; `az boards` and hand-
  rolled REST do neither.
- What the user currently has selected → the context JSON, not the database.
- Anything with no subcommand → [rest-fallback.md](rest-fallback.md).

## `repos list`, `repos show`

Both read the database and the workspace, and neither touches the network. The
workspace is `--workspace PATH`, else `TICKET_TUI_WORKSPACE`, else
`~/Development`: its immediate subdirectories that are git repositories are
matched to the project by their `origin` remote, or by name when no remote
matches — a project mirrored on GitHub is still the code you have — and each is
measured with `git status`. A workspace that is not there simply finds nothing.

```console
$ ticket-tui repos list
ado-helper         main  0  1  —
rust-game          main  0  0  main dirty -2
```

The columns are `name  branch  prs  pipelines  local`, where `prs` counts the
active pull requests, `pipelines` the definitions that build it, and `local` is
the clone: its branch, then `dirty`, `+n` ahead and `-n` behind, or `—` for a
repository that is not here. `--query` takes the Repos grammar (`name:`,
`branch:`, `local:`, `disabled:`).

`repos show NAME` prints the same facts as a block, plus the three URLs and,
for a clone matched by name rather than by remote, the origin it actually
points at. `--json` keys: `id, name, project, default_branch, is_disabled,
pull_requests, pipelines, web_url, remote_url, ssh_url, local` — where `local`
is `null` or `{path, origin, branch, dirty, ahead, behind}`.

Cloning, fetching and pulling are the TUI's keys `C`, `G` and `P`, not
subcommands. An agent that wants a clone should run `git` itself.

## `prs list`, `prs show`

```console
$ ticket-tui prs list
!7  pr-checkout-smoke  Jacob Ragsdale  0/0  —          Checks test PR
!1  development        Jacob Ragsdale  1/1  succeeded  ado-cli PR smoke test
```

Columns: `!id  repo  author  votes  build  title`, where `votes` is how many of
the reviewers asked have approved. `--query` takes the Pull requests grammar
(`repo:`, `author:`, `reviewer:`, `vote:`, `status:`, `target:`, `source:`,
`draft:`, `build:`), so `--query 'reviewer:@me vote:none'` is the review queue.

`--json` keys on a row: `id, repo, title, author, status, is_draft, source,
target, merge_status, auto_complete, created, closed, url`, plus `build` when
one gates it. `prs show --json` adds `reviewers` (`name`, `vote`,
`is_required`), `work_items`, `threads` (`id`, `author`, `text`, `status`,
`published`) and `description`.

Votes are the API's own scale: `10` approved, `5` approved with suggestions,
`0` no vote, `-5` waiting for the author, `-10` rejected.

## The pull-request writes

```console
ticket-tui prs vote 11 approve
ticket-tui prs comment 11 "One nit, otherwise good."
ticket-tui prs complete 11 --strategy squash
ticket-tui prs abandon 12
ticket-tui prs autocomplete 11 on
```

Each writes to Azure DevOps and stores the copy it answers with, so a running
TUI shows the change without a pull. `vote` writes as whoever is signed in,
read once from Azure DevOps and kept in the database — no work-item endpoint
ever reports it. `complete` defaults to `squash` and to deleting the source
branch and transitioning the linked work items; `--keep-source` and
`--no-transition` turn those off. It carries the head commit the stored copy
was read at, so a merge that raced somebody else's push is refused:

```text
error: Azure DevOps returned HTTP 409 … the source branch has been updated
```

Run `ticket-tui sync` and look again. A pull request that cannot fast-forward,
or a policy that is not satisfied, is Azure DevOps's refusal in its own words.

## `pipelines`, `runs`

`pipelines` and `runs list` read the database. `runs show`, `runs logs`,
`runs wait` and everything that writes read or write Azure DevOps: a timeline,
a log and a run's progress are not things a pull stores.

```console
$ ticket-tui pipelines
2  \  succeeded  ado-helper-smoke

$ ticket-tui runs list
4  20260516.2  succeeded  main  ado-helper-smoke
```

`pipelines` prints `id  folder  last-run  name`; `runs list` prints
`id  build-number  result  branch  pipeline` and takes `--pipeline NAME` and
the run grammar (`pipeline:`, `status:`, `result:`, `branch:`, `by:`,
`reason:`).

`runs show ID` prints the header and the timeline as a tree, in the glyphs the
TUI uses — `✓` succeeded, `✗` failed, `◐` running, `◑` partly succeeded, `⊘`
canceled, `○` not started. `--json` prints the run plus a flat `timeline` array
where each node names its `parent_id`, which is what a tree is on the wire.

`runs logs ID` prints one node's log. `--job NAME` or `--task NAME` names it;
with neither it takes the deepest node still running, which is what the TUI's
log pane shows. `--follow` keeps printing what is new, sending the line count it
already holds so each poll fetches only the tail, and returns when the node
finishes.

## Waiting, and exit codes

```console
ticket-tui runs wait 14
ticket-tui runs trigger 'ticket-tui CI' --branch main --follow
```

Both exit with the run's result rather than making you parse anything:

| Exit | Meaning |
|---|---|
| 0 | succeeded |
| 1 | failed — also every other error this CLI reports |
| 2 | canceled |
| 3 | partially succeeded |

`--branch` takes `main` or `refs/heads/main`; the short form is expanded.
`runs cancel` and `runs retry` print the run and its new status.

## `approvals`

```console
$ ticket-tui approvals list
abc-123  20260829.4  Deploy to production  ticket-tui CI

ticket-tui approvals approve abc-123 --comment 'checked the staging smoke'
ticket-tui approvals reject abc-123 --comment 'waiting on the migration'
```

Columns: `id  build-number  stage  pipeline`. `--json` adds `run_id`,
`instructions` and `requested_at`. Both answers take an optional `--comment`,
which Azure DevOps records with the decision.

## `pods`

The AKS tab without the tab, and the one read that has no database behind it:
it reads the clusters `config.toml` names through `kubectl`, live, every time.

```console
$ ticket-tui pods
qa    orders   orders-api-7d9f5b-abc12   1/1  Running           0  4h
qa    orders   orders-api-7d9f5b-def34   0/1  CrashLoopBackOff  9  12m
prod  billing  billing-worker-6c4a1-x9z  1/1  Running           0  3d

$ ticket-tui pods --cluster qa "status:crashloopbackoff"
qa  orders  orders-api-7d9f5b-def34  0/1  CrashLoopBackOff  9  12m

$ ticket-tui pods --cluster qa --namespace orders "orders-api-7d9f5b-def34" --json
[
  {
    "cluster": "qa",
    "namespace": "orders",
    "name": "orders-api-7d9f5b-def34",
    "status": "CrashLoopBackOff",
    "ready": "0/1",
    "restarts": 9,
    "created": "2026-08-30T10:00:00Z",
    "age": "12m",
    "node": "aks-nodepool1-0",
    "ip": "10.0.0.7",
    "owner": "Deployment/orders-api",
    "containers": [
      {
        "name": "api",
        "image": "myacr.azurecr.io/team/orders-api:1.2.3",
        "ready": false,
        "restarts": 9,
        "state": "CrashLoopBackOff"
      }
    ],
    "labels": { "app": "orders-api", "pod-template-hash": "7d9f5b" }
  }
]
```

Columns: `cluster  namespace  pod  ready  status  restarts  age`, or
`no matching pods`. The positional argument is the AKS tab's grammar —
`cluster:`, `ns:`, `status:`, `owner:`, `node:`, `app:`, `repo:`, and anything
else matched fuzzily against the name, namespace, owner and repository
([filters.md](filters.md)). `--cluster` and `--namespace` narrow what is *read*
rather than what is printed, which is the difference between one `kubectl` call
and six.

`repo` is always null here: matching a pod to a repository wants the project's
repositories, and this command does not open the database. Read it from the
live context (`aks.selected.repo`) when a TUI is running.

Failures are per cluster, and partial answers are still printed. A cluster that
will not answer puts one line on stderr —

```console
$ ticket-tui pods; echo "exit $?"
prod: Unable to connect to the server: dial tcp: i/o timeout
qa  orders  orders-api-7d9f5b-abc12  1/1  Running  0  4h
error: 1 cluster(s) could not be read
exit 1
```

— so a non-zero exit means *some* cluster is missing from the table, not that
nothing was read. A namespace the server refuses (`Error from server
(Forbidden)`) does not stop that cluster's other namespaces; anything else
does.

`config.toml` with no `[[clusters]]` is an error: `no clusters in config.toml;
add a [[clusters]] table`. So is `--cluster` naming one the file does not have.
`kubectl` must be on `PATH` and already logged in — the TUI and the CLI both
shell out to it and neither can answer a prompt.

## `acr`

The ACR tab without the tab, and — like `pods` — a read with no database behind
it. The subscription is asked for its registries on every invocation through
Resource Graph, and a registry's own data plane answers for the repositories,
tags and manifests inside it. The subscription is `--subscription`, else
`TICKET_TUI_SUBSCRIPTION`, else whichever one `az account set` left the CLI on;
with none of the three the command is an error rather than an empty listing:

```text
error: no Azure subscription: pass --subscription, set TICKET_TUI_SUBSCRIPTION, or run `az account set`
```

`az login` must have happened. A personal access token opens Azure DevOps and
nothing else, so `AZURE_DEVOPS_EXT_PAT` does not reach ARM.

```console
$ ticket-tui acr list
acr  rg  Premium  westeurope  acr.azurecr.io

$ ticket-tui acr show acr
acr
acr.azurecr.io

Group         rg
Location      westeurope
SKU           Premium
Repositories  2

Portal        https://portal.azure.com/#resource/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.ContainerRegistry/registries/acr

$ ticket-tui acr repos list --registry acr
team/orders-api  7  9  2026-08-29 09:00:00 UTC (1d)
team/billing     —  —  —

$ ticket-tui acr tags list --registry acr --repo team/orders-api
1.2.3  0123456789ab  2026-08-29 09:00:00 UTC (1d)

$ ticket-tui acr tags show --registry acr --repo team/orders-api 1.2.3
1.2.3  0123456789ab

Digest        sha256:0123456789abcdef
Tagged        2026-08-29 09:00:00 UTC (1d)
Created       2026-08-29 09:00:00 UTC (1d)
Platform      linux/amd64
Size          12.5 MB

Pull          acr.azurecr.io/team/orders-api:1.2.3
```

Columns: `acr list` prints `name  resource-group  sku  location  login-server`;
`repos list` prints `repository  tags  manifests  updated`; `tags list` prints
`tag  digest  created`, newest first, because that is the order the registry
lists them in. A count nobody has read yet is `—` rather than `0` — a catalog
listing is names and nothing else, and the counts come from one attributes call
per repository.

`--json`:

```console
$ ticket-tui acr list --json
[
  {
    "name": "acr",
    "resource_group": "rg",
    "sku": "Premium",
    "location": "westeurope",
    "login_server": "acr.azurecr.io",
    "id": "/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.ContainerRegistry/registries/acr",
    "portal_url": "https://portal.azure.com/#resource/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.ContainerRegistry/registries/acr"
  }
]

$ ticket-tui acr repos list --registry acr --json
[
  { "name": "team/orders-api", "tags": 7, "manifests": 9, "updated": "2026-08-29T09:00:00Z" },
  { "name": "team/billing", "tags": null, "manifests": null, "updated": null }
]

$ ticket-tui acr tags show --registry acr --repo team/orders-api 1.2.3 --json
{
  "tag": {
    "name": "1.2.3",
    "digest": "sha256:0123456789abcdef",
    "short_digest": "0123456789ab",
    "created": "2026-08-29T09:00:00Z",
    "updated": "2026-08-29T09:00:00Z"
  },
  "pull": "acr.azurecr.io/team/orders-api:1.2.3",
  "manifest": {
    "digest": "sha256:0123456789abcdef",
    "platform": "linux/amd64",
    "architecture": "amd64",
    "os": "linux",
    "size": 13107200,
    "size_label": "12.5 MB",
    "created": "2026-08-29T09:00:00Z"
  }
}
```

`acr show --json` adds `repositories` (the catalog's size); `acr list --json`
leaves it out, because a listing has read no catalog. `tags list --json` is an
array of the `tag` object above.

Names are matched ignoring case, and one the subscription does not hold is
refused rather than guessed at: `no registry called ghcr in subscription
sub-1`, `no tag called 9.9.9 on team/orders-api in acr`. An attributes call that
refuses is one line on stderr and a non-zero exit *after* the rows that did
answer have been printed — `2 repository(s) could not be read` — so a partial
listing is still on stdout.

Everything is read-only. There is no untag and no delete here, and there is not
going to be: the portal link in `show` is the way to anything destructive.

## `vaults`

The Key Vault tab without the tab, read the same way and from the same
inventory. A listing never carries a value.

```console
$ ticket-tui vaults list
atlas-kv  rg  westeurope  standard  https://atlas-kv.vault.azure.net/

$ ticket-tui vaults show atlas-kv
atlas-kv
https://atlas-kv.vault.azure.net/

Group         rg
Location      westeurope
SKU           standard
Secrets       2
Keys          1
Certificates  2

Portal        https://portal.azure.com/#resource/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.KeyVault/vaults/atlas-kv
```

Columns: `name  resource-group  location  sku  uri`. `--json`:

```console
$ ticket-tui vaults show atlas-kv --json
{
  "name": "atlas-kv",
  "resource_group": "rg",
  "location": "westeurope",
  "sku": "standard",
  "uri": "https://atlas-kv.vault.azure.net/",
  "id": "/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.KeyVault/vaults/atlas-kv",
  "portal_url": "https://portal.azure.com/#resource/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.KeyVault/vaults/atlas-kv",
  "items": { "secrets": 2, "keys": 1, "certs": 2 }
}
```

`items` is only on `show`, which is the one form that reads the listing;
`vaults list --json` leaves it out. A vault the subscription does not hold is
`no vault called billing-kv in this subscription`.

## `secrets`, `keys`, `certs`

Three top-level groups over one listing: a vault answers for its secrets, keys
and certificates in one read, and each command is that listing filtered to one
kind. They are three commands because that is how you reach for them.

```console
$ ticket-tui secrets list --vault atlas-kv
orders-db      yes  2026-08-29 09:00:00 UTC (1d)    —                               text/plain
retired-token  no   2026-01-05 09:00:00 UTC (237d)  2026-08-20 09:00:00 UTC (10d)   —

$ ticket-tui keys list --vault atlas-kv
signing  yes  2026-08-28 09:00:00 UTC (2d)  —

$ ticket-tui certs list --vault atlas-kv
wildcard  yes  2026-08-01 09:00:00 UTC (29d)  2026-09-29 09:00:00 UTC (0s) expires in 30 days
legacy    yes  2026-01-05 09:00:00 UTC (237d) 2026-08-20 09:00:00 UTC (10d) expired 10 days ago
```

Columns: `name  enabled  updated  expires`, with a fifth `content-type` column
for secrets — the one kind that carries one — and the expiry said in plain words
as well as a stamp for certificates, which is what a reader is usually after. An
empty listing is `no secrets in this vault`.

`secrets show` prints what the listing says about one, and then says what it is
deliberately not printing:

```console
$ ticket-tui secrets show --vault atlas-kv orders-db
orders-db  secret

Enabled       yes
Created       2026-07-01 09:00:00 UTC (60d)
Updated       2026-08-29 09:00:00 UTC (1d)
Expires       —
Content type  text/plain
Recovery      Recoverable+Purgeable

value: not shown; pass --value to print it
```

`--json` for any of them prints the listing's own fields and nothing else.
**There is no field for a value, on purpose:**

```console
$ ticket-tui secrets show --vault atlas-kv orders-db --json
{
  "kind": "secret",
  "name": "orders-db",
  "enabled": true,
  "created": "2026-07-01T09:00:00Z",
  "updated": "2026-08-29T09:00:00Z",
  "expires": null,
  "content_type": "text/plain",
  "recovery_level": "Recoverable+Purgeable"
}
```

`kind` is `secret`, `key` or `cert`. A key is not a secret however the vault
lists them: `secrets show --vault atlas-kv signing` is `no secret called signing
in atlas-kv`.

### `--value`

`ticket-tui secrets show --vault V NAME --value` prints the secret's value,
raw, on stdout and nothing else — so `$(…)` around it is the value and only the
value. It is the one read in this CLI that fetches one, it conflicts with
`--json` at the command line rather than quietly preferring one, and reading a
value is an audited operation: the vault records who read what.

**Never run it unless the user has asked to see that specific secret, and never
paste a value into a ticket, a commit message, a comment, a log, or the context
file.** Every other form here — including `secrets show` without the flag —
answers "does this secret exist, is it enabled, when does it expire" without
reading a value at all, and that is almost always the question.
