# ticket-tui

A fast terminal browser for Azure DevOps work items — and for the repositories,
pull requests and pipelines beside them.

`ticket-tui` keeps a local SQLite database synced from one Azure DevOps project
and reads from that database, so navigation, sorting, filtering and fuzzy
search stay instant no matter how far away the server is. Azure DevOps stays
the source of truth: a background worker pulls the project every minute, and a
field changed in the TUI is written straight back over the REST API. Everything
else it does is local.

```
 1 Work items  2 Repos  3 Pull requests  4 Pipelines
┌ Search / ───────────────────────────────────────────────────────────[Actions] [?]┐
│Type / to search, or pick State, Type, Tags, or Assignee below                    │
└──────────────────────────────────────────────────────────────────────────────────┘
 State ▾   Type ▾   Tags ▾   Assignee ▾   +
┌ Tickets 105/105 · Title ↑ ───────────────────────────────────────────────────────┐
│       ID      Title↑                     State      Type           Pri Changed   │
│                                                                                  │
│› [ ]  643     [Discuss] In-TUI agent cha Done       [Issue]          4         9┃│
│  [ ]  655     A hand-typed backslash in  Done       [Issue]          2         9││
│  [ ]  618     Add a comment              Done       [Issue]          2        20││
│  [ ]  691     Agent context v3: the acti Done       [Issue]          2         4││
│  [ ]  624     Agent integration          Done       [Epic]           3         9││
└──────────────────────────────────────────────────────────────────────────────────┘
────────────────────────────────────────────────────────────────────────────────────
┌ Details ─────────────────────────────────────────────────────────────────────────┐
│[Discuss] In-TUI agent chat pane about the selected ticket                       ┃│
│ID / Type / State: 643 · [Issue] · Done                                          ││
│Family: Epic 624  Agent integration › this                                       ││
└──────────────────────────────────────────────────────────────────────────────────┘
         ↑↓/jk move  / search  click/drag copy  wheel scroll  ? help  q quit
```

## Run it

You need Rust 1.88 or newer, a macOS or Linux terminal, and access to an Azure
DevOps project.

```console
az login                                                  # ticket-tui borrows this login
cargo run --release -- sync --org my-org --project my-project
cargo run --release                                       # later runs open at once
```

The first `sync` fills the database; every run after it opens immediately and
pulls in the background. Without a configured organization the TUI runs
offline, browsing whatever the database already holds.

## The keys worth knowing

| Key | Does |
|---|---|
| `1`–`4` | Work items, Repos, Pull requests, Pipelines |
| `/` | Live fuzzy search — `state:active`, `assignee:@me`, `id:642` |
| `p` / `:` | The command palette: every action the tab can take |
| `e` | The Actions menu — edit title, state, assignee, tags, description |
| `n` / `N` | New work item, or a new child of the selected one |
| `r` | Sync now, without waiting for the timer |
| `o` | Open the selected row in the system browser |
| `?` | The in-app help, generated from the same table the keys are bound in |
| `q` | Quit |

The mouse works throughout: click a field to edit it, drag the divider, scroll
a pane, click a tab.

## Where things live

The database is durable and lives in the platform data directory —
`~/Library/Application Support/ticket-tui/tickets.sqlite3` on macOS,
`~/.local/share/ticket-tui/tickets.sqlite3` on Linux. It is a documented
interface, not a scratch cache: other tools and agent skills read it directly,
and the TUI publishes a JSON file beside it naming what is on screen.

`ticket-tui` is also a CLI — `list`, `show`, `edit`, `comment`, `create`,
`repos`, `prs`, `pipelines`, `runs`, `approvals` — so a script or an agent can
do anything the TUI can.

## More

- [DESIGN.md](DESIGN.md) — how all of it works, in full: the sync protocol, the
  revision rules an edit obeys, every screen and key, the database schema, and
  the context file agents read.
- [HANDOFF.md](HANDOFF.md) — where the last round of work stopped.
- [LICENSE](LICENSE) — MIT.
