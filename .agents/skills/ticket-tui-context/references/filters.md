# The filter grammars

One shape, nine vocabularies. Work items, repositories, pull requests, runs,
pods, registries, repositories-in-a-registry, vaults and vault items each have
their own fields; everything else — how values combine, how they are
quoted, how sentinels resolve — is the same everywhere, and the same in the
TUI's search boxes and in the `search.query` field of the context JSON. None of
it is WIQL: the global `--query WIQL` flag that narrows a *pull* is a different
thing entirely.

## Contents

- [Work items — `ticket-tui list --query`](#work-items--ticket-tui-list---query)
- [Sentinels](#sentinels)
- [`changed:` and `created:`](#changed-and-created)
- [Ordering](#ordering)
- [Repositories — `ticket-tui repos list --query`](#repositories--ticket-tui-repos-list---query)
- [Pull requests — `ticket-tui prs list --query`](#pull-requests--ticket-tui-prs-list---query)
- [Runs — `ticket-tui runs list --query`](#runs--ticket-tui-runs-list---query)
- [Pods — `ticket-tui pods`](#pods--ticket-tui-pods)
- [Registries and repositories — the ACR tab](#registries-and-repositories--the-acr-tab)
- [Vaults and their items — the Key Vault tab](#vaults-and-their-items--the-key-vault-tab)

This page describes the work-item grammar first, because it is the largest, and
then the shorter ones. Each is used by the subcommand named beside it, except
the last four: the ACR and Key Vault tabs have search boxes but no `--query` on
the command line, so those grammars are the TUI's alone.

A query is a sequence of `field:value` pairs and free text. Values in one field
are ORed, different fields are ANDed, and everything left over is matched
fuzzily and orders the rows by how well it matched.

```console
ticket-tui list --query 'state:doing state:"to do" assignee:@me'
ticket-tui list --query 'type:Issue tag:agents changed:<7d'
ticket-tui list --query 'type:Epic sync'          # `sync` is the fuzzy term
```

## Work items — `ticket-tui list --query`

| Field | Aliases | Matches |
|---|---|---|
| `state:` | | `To Do`, `Doing`, `Done` in this project |
| `type:` | | `Epic`, `Issue`, `Task`, … |
| `assignee:` | `assigned:` | Display name, or the literal `Unassigned` |
| `priority:` | `pri:` | The number, or `—` for none |
| `project:` | | Azure DevOps project |
| `area:` | | Full area path or its last segment |
| `iteration:` | `sprint:` | Full iteration path or its last segment |
| `tag:` | `tags:` | One tag; a work item matches if any tag matches |
| `changed:` | `updated:` | A date comparison — see below |
| `created:` | | A date comparison — see below |

Plus `id:` — `id:613 id:614` is those two work items and nothing else, which is
what a jump from another tab writes into the search box — and
`is:bookmarked` (or `is:bookmark`), which is a TUI-only filter: bookmarks
live in the session file, so it matches nothing from `ticket-tui list`.

Comparisons are case-insensitive. `area:` and `iteration:` also match a bare
last path segment, so `iteration:"Sprint 1"` finds work items whose iteration is
`development\Sprint 1`. An unrecognized field name is not an error — it falls
through and becomes fuzzy text.

Values containing spaces are quoted with `"` or `'`, and `\` escapes the next
character:

```console
ticket-tui list --query 'state:"To Do" area:"development\\Platform"'
```

## Sentinels

Four values are written with a leading `@` and stand for something resolved
when the query runs, not when it was written — so a saved query follows the
person and the calendar instead of freezing them.

| Value | Stands for |
|---|---|
| `assignee:@me` | The display name the last sync recorded, or `TICKET_TUI_ME` |
| `assignee:@none` | Unassigned work |
| `iteration:@current` | The iteration whose dates contain today |
| `state:@open` | Any state that is not `Done` or `Removed` |

A sentinel written on a field that has none — `state:@me` — is not an error; it
is an ordinary value that matches nothing.

`@me` and `@current` need something the run may not have. In the TUI they match
nothing and the chips still show what you typed. From the command line that
would read exactly like an empty backlog, so `list` refuses instead:

```console
$ ticket-tui list --query 'assignee:@me'
no signed-in name to resolve @me; run `ticket-tui sync` once or set TICKET_TUI_ME
```

`@current` reads the cached iteration tree, which a sync fills. A project with
no sprint scheduled around today is refused the same way.

## `changed:` and `created:`

These take a comparison rather than a value, because a date has no enumerable
list. Anything without an operator is not a comparison and falls through to
fuzzy text — `changed:7d` matches nothing useful; `changed:<7d` is what you
meant.

| Form | Meaning |
|---|---|
| `changed:<7d` | Changed within the last 7 days |
| `changed:>14d` | Not touched for more than 14 days |
| `changed:<=2h` | Changed within the last 2 hours |
| `created:>=2026-08-01` | Created on or after 2026-08-01 UTC midnight |
| `created:<2026-08-01T12:00:00Z` | Created before that instant |

Operators: `<`, `<=`, `>`, `>=`. Relative units: `m` minutes, `h` hours, `d`
days, `w` weeks — a whole number and one unit, such as `30m`, `48h`, `2w`.

**The operator reads against the value written after it, which turns its meaning
around between the two forms.** `<7d` is an *age* below seven days, so it keeps
the recently touched items; `<2026-08-01` is an *instant* before that date, so
it keeps the old ones. Relative bounds are measured when the query runs, not
when it was written, so a saved `changed:<7d` still means the last seven days
tomorrow.

Absolute values accept RFC 3339, a bare `YYYY-MM-DD` (read as UTC midnight), and
a space-separated `YYYY-MM-DD HH:MM:SS` if the whole value is quoted:

```console
ticket-tui list --query 'changed:">2026-08-29 06:00:00" type:Issue'
```

Quote the whole query in a shell regardless: `<` and `>` are redirections.

The TUI's filter overlay offers `<24h`, `<7d`, `<14d`, and `<30d` as presets for
these two fields; anything else is typed into the search box.

## Ordering

Without a fuzzy term, `list` returns rows newest change first, ties broken by
descending id — the same order the TUI's table opens on. With a fuzzy term the
rows come back in relevance order. Fuzzy matching covers id, title, assignee,
state, type, area, iteration, and tags; it deliberately excludes descriptions,
so searching for a phrase from a ticket body will not find it.

## Repositories — `ticket-tui repos list --query`

| Field | Matches |
|---|---|
| `name:` | The repository's name |
| `branch:` | Its default branch, with or without `refs/heads/` |
| `local:` | `cloned`, `missing`, `dirty`, `ahead`, `behind` — what the clone on this machine is |
| `disabled:` | `yes` or `no` |

Fuzzy text matches the name and the branch.

```console
ticket-tui repos list --query 'local:dirty'
ticket-tui repos list --query 'local:missing disabled:no'
```

`local:` is answered from `git status` in the workspace, so it says nothing at
all when the workspace does not exist — every repository then reads
`local:missing`.

## Pull requests — `ticket-tui prs list --query`

| Field | Aliases | Matches |
|---|---|---|
| `repo:` | `repository:` | The repository's name |
| `author:` | `by:` | Who raised it; `@me` resolves |
| `reviewer:` | | Somebody asked to review it; `@me` resolves |
| `vote:` | | *Your own* vote: `approved`, `suggestions`, `waiting`, `rejected`, `none` |
| `status:` | | `active`, `completed`, `abandoned` |
| `target:` | | The target branch, short or full |
| `source:` | | The source branch |
| `draft:` | | `yes` or `no` |
| `build:` | | What the branch policy's build says: `succeeded`, `running`, `failed`, `none` |

Fuzzy text matches the title, the repository and both branches.

```console
ticket-tui prs list --query 'reviewer:@me vote:none'     # the review queue
ticket-tui prs list --query 'author:@me draft:no build:failed'
ticket-tui prs list --query 'status:completed target:main'
```

`vote:` is about you, not about the pull request: `vote:none` is one you have
not voted on, whoever else has. Closed pull requests are in the database only as
far back as the pull's window reaches, so `status:completed` is recent history
rather than the whole project's.

## Runs — `ticket-tui runs list --query`

| Field | Matches |
|---|---|
| `pipeline:` | The definition's name |
| `branch:` | The branch it built, short or full |
| `status:` | `inProgress`, `completed`, `cancelling`, `notStarted`, `postponed` |
| `result:` | `succeeded`, `failed`, `canceled`, `partiallySucceeded` |
| `reason:` | Why it ran: `manual`, `individualCI`, `pullRequest`, `schedule` |
| `by:` | Who set it going; `@me` resolves |

`--pipeline NAME` is the same as `pipeline:NAME` and is there because it is the
one everybody wants.

```console
ticket-tui runs list --pipeline 'ticket-tui CI' --query 'result:failed'
ticket-tui runs list --query 'branch:main by:@me'
```

A run still going has no result, so `result:failed` never matches one; use
`status:inProgress` for those.

## Pods — `ticket-tui pods`

The only grammar whose query is a positional argument rather than `--query`:
`ticket-tui pods 'status:running'`.

| Field | Aliases | Matches |
|---|---|---|
| `cluster:` | | The cluster's name **in `config.toml`**, not its kubeconfig context |
| `ns:` | `namespace:` | The namespace |
| `status:` | `phase:` | The STATUS word: `Running`, `CrashLoopBackOff`, `Pending`, `Completed`, `Init:1/2` |
| `owner:` | `deployment:` | What made it, by name: `orders-api` for a `Deployment/orders-api` |
| `node:` | | The node it landed on |
| `app:` | | The `app` or `app.kubernetes.io/name` label |
| `repo:` | `repository:` | The repository on file its image or app label names |

Fuzzy text matches the pod name, the namespace, the owner and the repository.

```console
ticket-tui pods 'status:running'
ticket-tui pods --cluster qa 'status:crashloopbackoff'
ticket-tui pods 'app:orders-api ns:orders'
ticket-tui pods 'orders-api'                     # fuzzy: name, namespace, owner, repo
```

`--cluster` and `--namespace` are not the same as `cluster:` and `ns:`: the
flags narrow what `kubectl` is *asked for*, the fields narrow what is printed
from what came back. `--cluster qa` is one read; `cluster:qa` is every cluster
read and most of the rows thrown away.

`repo:` never matches from the CLI — the repository lookup wants the project's
repositories and `pods` does not open the database — but it works in the TUI's
own search box and in the columns.

## Registries and repositories — the ACR tab

Two grammars, one per level, typed into the tab's own search box with `/`. There
is no `--query` on `ticket-tui acr`: the listings are short enough that `grep`
or `--json` and `jq` do the same job from a script.

At the registries level:

| Field | Aliases | Matches |
|---|---|---|
| `name:` | `registry:` | The registry's name |
| `rg:` | `group:`, `resourcegroup:` | Its resource group |
| `sku:` | | `Basic`, `Standard`, `Premium` |
| `location:` | `region:` | The Azure region: `westeurope` |

The facet bar offers `rg:`, `sku:` and `location:`. Anything left over matches
the registry's name or its login server.

Inside one registry, a catalog has one field worth naming:

| Field | Aliases | Matches |
|---|---|---|
| `name:` | `repo:`, `repository:` | The repository's path, `team/orders-api` |

The bar is empty at that level — the one field there is is already the thing you
are typing. Leftover text matches the repository's name or the registry's.

## Vaults and their items — the Key Vault tab

The same shape again, and the same rule: the search box only, no `--query` on
`ticket-tui vaults`, `secrets`, `keys` or `certs`.

At the vaults level:

| Field | Aliases | Matches |
|---|---|---|
| `name:` | `vault:` | The vault's name |
| `rg:` | `group:`, `resourcegroup:` | Its resource group |
| `location:` | `region:` | The Azure region |

Inside one vault, over all three kinds at once, because that is how a person
looks for one — by name, not by which listing it came out of:

| Field | Aliases | Matches |
|---|---|---|
| `name:` | `item:` | The item's name |
| `kind:` | `type:` | `secret`, `key`, or `cert` |
| `enabled:` | | `yes`/`no`, and `true`/`false` — both spellings, so neither quietly matches nothing |
| `expires:` | `expiry:` | A date comparison, the same one `changed:` and `created:` take |

The facet bar offers `kind:` and `enabled:`. Leftover text matches the item's
name or its kind; at the vaults level it matches the vault's name or its
resource group.

### `expires:` looks forward

`expires:` is the one date field in this app that is usually about the future,
so it takes the `+` form of the comparison:

| Form | Meaning |
|---|---|
| `expires:<+30d` | Expires before the instant 30 days from now — everything falling due inside a month, plus everything already lapsed |
| `expires:<+7d` | The same, inside a week |
| `expires:>+90d` | Not due for more than 90 days |
| `expires:<2026-09-01` | Before that date, absolutely |
| `expires:>30d` | An *age*: lapsed more than 30 days ago |

The `+` is what turns the bound around. `<+30d` compares against *now plus
thirty days*, so it keeps what is about to run out; `<30d` with no `+` is the
age form every other date field uses and would keep only what lapsed recently.
Bounds are measured when the query runs, so the query stays true tomorrow.

```console
kind:cert expires:<+30d          # what the tab's ◇N badge counts
kind:cert expires:<+30d enabled:yes
kind:secret expires:<+7d
```

An item with no expiry never lapses and matches no comparison at all, which is
why `kind:cert expires:<+30d` does not sweep up the certificates nobody dated.

`◇N` on the tab bar is the same question asked without a query: certificates
already lapsed or within thirty days of it, across every vault whose items have
been read. It is in the live context as `key_vault.expiring_certificates`.
