# `ticket-tui` subcommand reference

A bare `ticket-tui` opens the TUI. Every subcommand does one thing and exits,
which is what lets an agent read and change work items without a terminal to
drive.

```console
ticket-tui sync [--full]
ticket-tui show <id> [--json]
ticket-tui list [--query '<filter>'] [--json]
ticket-tui edit <id> [--state S] [--assignee A] [--priority N] [--iteration I]
                     [--area A] [--title T] [--tags a,b] [--description-file F]
ticket-tui comment <id> "text"
ticket-tui create --type TYPE --title TITLE [--parent ID] [--iteration I]
                  [--assignee A] [--priority N] [--tags a,b]
```

## Global options

These three are global: valid on every subcommand, either side of it.

| Flag | Meaning |
|---|---|
| `--database PATH` | SQLite database to use instead of the platform data-directory default |
| `--org ORG` | Azure DevOps organization, as a slug or a URL; else `TICKET_TUI_ORG`, else `az devops configure` defaults |
| `--project PROJECT` | Azure DevOps project; else `TICKET_TUI_PROJECT`, else `az devops configure` defaults |

`--query WIQL`, `--refresh SECONDS`, `--stale-days DAYS`, and the deprecated
`--sync` are **not** global. They must be written before the subcommand, and `ticket-tui sync
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
`changed:>Nd state:@open` the **Stale** view asks for. `--sync`
prints a deprecation note and pulls before opening the TUI; `ticket-tui sync` is
the subcommand that pulls and exits.

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
