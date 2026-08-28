---
name: ticket-tui-context
description: Inspect and interpret the live ticket-tui screen state when a user asks about the ticket, checked set, search, filters, or rows they are currently viewing. Do not use for unrelated SQLite inspection or for changing TUI state.
---

# Ticket TUI Context

Read the live context before answering claims about what the user currently sees.
Treat it as observational state; do not manipulate the TUI or edit ticket data
unless the user separately requests that action.

## Read the current view

From the ticket-tui repository, run:

```console
uv run .agents/skills/ticket-tui-context/scripts/read_context.py
```

The command prints a compact interpretation of the default database's live
context. If ticket-tui uses `--database PATH`, pass the same path:

```console
uv run .agents/skills/ticket-tui-context/scripts/read_context.py --database PATH
```

If the path is unknown, inspect the running command with
`ps -axo command= | rg '[t]icket-tui'`, then use its `--database` value. Do not
scan unrelated user files.

Use `--details` only when the task needs the selected ticket's description,
relations, comments, or history. Use `--json` when exact machine-readable fields
are more useful than the compact report.

## Interpret the result

- `Selected` is the ticket driving the details pane.
- `Checked` is the independent multi-select set used by copy and export actions;
  it can be empty or contain tickets other than the selected ticket.
- `Visible rows` are only the ticket rows in the rendered viewport. Use the
  matching and total counts to distinguish the viewport from the full result
  set.
- `Query` is the user's complete search text. `Fuzzy` is its free-text portion,
  while `Filters` lists parsed structured filters.
- `Mode`, `focus`, and `screen` identify overlays and whether the user is
  interacting with tickets, family, or details.
- A stale process warning means the file survived an unclean exit. Describe it
  as the last observed view, not a live view.

Read [references/context-schema.md](references/context-schema.md) when writing
an integration, interpreting raw JSON, or handling a schema version change.
