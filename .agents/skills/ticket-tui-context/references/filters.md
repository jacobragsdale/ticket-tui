# The filter grammars

One shape, four vocabularies. Work items, repositories, pull requests and runs
each have their own fields; everything else — how values combine, how they are
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

This page describes the work-item grammar first, because it is the largest, and
then the three shorter ones. Each is used by the subcommand named beside it.

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
