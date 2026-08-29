# The filter grammar

One grammar serves `ticket-tui list --query`, the TUI's search box, and the
`search.query` field in the context JSON. It is not WIQL — the global
`--query WIQL` flag that narrows a *pull* is a different thing entirely.

A query is a sequence of `field:value` pairs and free text. Values in one field
are ORed, different fields are ANDed, and everything left over is matched
fuzzily and orders the rows by how well it matched.

```console
ticket-tui list --query 'state:doing state:"to do" assignee:@me'
ticket-tui list --query 'type:Issue tag:agents changed:<7d'
ticket-tui list --query 'type:Epic sync'          # `sync` is the fuzzy term
```

## Fields

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

Plus `is:bookmarked` (or `is:bookmark`), which is a TUI-only filter: bookmarks
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
