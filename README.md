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
 1 Work items  2 Repos  3 Pull requests  4 Pipelines                                  ?
/ Type / to search, or pick a filter from the bar below
 State ▾   Assignee ▾   Iteration ▾   Type ▾   Priority ▾   Tags ▾   +
╭ Tickets 116/116 · Changed ↑ ───────────────────────────────────────────────────────╮
│       ID      Title                       State      Type           Pri Changed↑   │
│────────────────────────────────────────────────────────────────────────────────────│
│  [ ]  599     Serialize session enums wit ✓ Done     [Issue]         P1         1d┃│
│  [ ]  596     Remove demo seed data, sche ✓ Done     [Issue]         P1         1d││
│  [ ]  602     Update README, CI, and the  ✓ Done     [Issue]         P1         1d││
│  [ ]  633     Colour the State column by  ✓ Done     [Issue]         P1         1d││
│  [ ]  634     Colour type badges for ever ✓ Done     [Issue]         P2         1d││
│› [ ]  600     Factor overlay list renderi ✓ Done     [Issue]         P1         1d││
│  [ ]  597     Introduce ScrollState and T ✓ Done     [Issue]         P1         1d││
│  [ ]  635     Dim completed work items in ✓ Done     [Issue]         P2         1d││
│  [ ]  636     Show the leaf of area and i ✓ Done     [Issue]         P3         1d││
├ Details ───────────────────────────────────────────────────────────────────────────┤
│ Factor overlay list rendering into one helper                                     ┃│
│ #600 · [Issue] · ✓ Done · P1 · Jacob Ragsdale                                     ││
│ Family     Epic 595  Tech debt and architecture foundation › this                 ││
│ Tags       [tech-debt]                                                            ││
│ Project    jacobragsdale / development · r5                                       ││
│ https://dev.azure.com/jacobragsdale/development/_workitems/edit/600               ││
│                                                                                   ││
│ ── Family ────────────────────────────────────────────────────────────────────────││
╰────────────────────────────────────────────────────────────────────────────────────╯
 ↑↓/jk move  / search  click/drag copy  wheel scroll   development  ● Synced just now
```

## Run it

You need Rust 1.88 or newer, a macOS or Linux terminal, and access to an Azure
DevOps project. One edited file is the whole setup:

```console
mkdir -p ~/.config/ticket-tui && cp config.example.toml ~/.config/ticket-tui/config.toml
az login                                                  # ticket-tui borrows this login
cargo run --release -- sync                               # fills the database
cargo run --release                                       # later runs open at once
```

`config.toml` names the organization, the project the work items live in, and
the project the code lives in. Every one of them can still be overridden by a
flag — `--org`, `--project`, `--code-project` — or by the matching
`TICKET_TUI_*` variable, and whatever none of the three name is left for the
Azure CLI to answer.

The first `sync` fills the database; every run after it opens immediately and
pulls in the background. The Work items tab opens on **Mine** — the rows
assigned to you — until a session remembers something else; `V` picks another
view. Without a configured organization the TUI runs
offline, browsing whatever the database already holds.

## The keys worth knowing

| Key | Does |
|---|---|
| `1`–`4` | Work items, Repos, Pull requests, Pipelines |
| `/` | Live fuzzy search — `state:active`, `assignee:@me`, `iteration:@past`, `id:642`, `repo:ado-helper` |
| `p` / `:` | The command palette: every action the tab can take |
| `e` | The Actions menu — edit title, state, assignee, tags, description, acceptance criteria, comment |
| `n` / `N` | New work item, or a new child of the selected one |
| `+` | Quick capture, on every tab: one row, a title, `Enter` — an Issue on you, in the current sprint, tagged `inbox` |
| `r` | Sync now, without waiting for the timer |
| `o` | Open the selected row in the system browser |
| `?` | The in-app help, generated from the same table the keys are bound in |
| `L` | On Work items: link the work item to a branch — a repository, then one of its branches or a new name, made at the head of the default branch. On Pull requests: link a work item to the one under the cursor |
| `w` | On Work items: work with a coding agent — Copilot or Cursor in a Herdr pane, in the work item's repository, with the ticket in front of it; or back to the agent already on it. **Start another agent session**, **Copy agent prompt** and **Show agent prompt** are in the Actions menu and the palette |
| `g` | Go to what the row points at: a work item's pull request, build or the repository its branch is in; a pull request's work items; a run's pull request; a repository's open pull request, its linked work items or, with none, the pipeline that builds it |
| `[` / `]` | Back and forward through everywhere you have been, across tabs |
| `q` | Quit |

The mouse works throughout: click a field to edit it, drag the divider, scroll
a pane, click a tab.

## Where things live

The database is durable and lives in the platform data directory —
`~/Library/Application Support/ticket-tui/tickets.sqlite3` on macOS,
`~/.local/share/ticket-tui/tickets.sqlite3` on Linux. It is a documented
interface, not a scratch cache: other tools and agent skills read it directly,
and the TUI publishes a JSON file beside it naming what is on screen.

`~/.config/ticket-tui/config.toml` is optional and is the one file worth
editing; [config.example.toml](config.example.toml) is a commented copy of the
whole of it.

`[devops]` says where the work is. `org` takes a slug or a
`https://dev.azure.com/...` URL, `project` is where the work items live, and
`code_project` is where the repositories, pull requests and pipelines live —
left out, they live in the same project, which is what one project in one place
has always meant. `query` is one WIQL condition ANDed into every pull, and
`workspace` is where the Repos tab looks for clones, with a leading `~/` read
as the home directory; the Pull requests and Pipelines tabs show only the
repositories with a verified clone there — one whose `origin` is the
repository. `team` is one team of a project that is a whole
department's board, or a list of them: their area paths narrow every pull,
their members are what the assignee picker offers, their sprints are what
`@current` means, and a fresh session opens on Current sprint; the Sprint
column then says where every row sits against it — `-2 · Sprint 16` is a
leftover, `now · Sprint 18` this sprint, `backlog` unplanned — and the
Leftovers and Backlog views (`iteration:@past`, `iteration:@backlog`) pick
those out. `ticket-tui
teams` prints the names to choose from. A large team's whole history is still
inside its areas; `query` is what keeps it out:

```toml
[devops]
org = "myorg"
project = "ISTO"
code_project = "Fiquants"
team = ["Payments", "Platform"]
query = "[System.State] <> 'Closed' OR [System.ChangedDate] > @today-90"
```

`[notify]` is one command, run through `sh -c` when something worth
interrupting for happens: a run you pressed `w` on finishes, a vote lands on a
pull request you wrote, one turns up wanting your review, an approval lands on
a run. The status line says the same words
whether or not the table is there, so this is the copy you get when ticket-tui
is in a pane you are not looking at:

```toml
[notify]
command = "notify-send {title} {body}"
```

`{title}` and `{body}` are replaced by the text as one complete single-quoted
shell word, so write them where an argument goes and quote nothing around them;
[config.example.toml](config.example.toml) carries the macOS spelling, which
hands both to `osascript` through `argv`. Nothing is announced for what was
already there when the run started.

And it holds the colour theme: a `[theme.custom]` palette in the vocabulary of
the `theme` tool, which applies one palette to every program on the machine,
writes this file for you, and repaints a running ticket-tui when it changes.
Without one the sixteen ANSI colours of the terminal show through; `--theme
terminal-light` suits a white ground, and `NO_COLOR` turns colour off.

`[agents]` and `[herdr]` are what `w` reads. `w` on a work item launches a
coding CLI in a Herdr pane, in that work item's repository, with the ticket
already in front of it — and the conversation is then the CLI's own. Herdr
workspaces are your groupings of repositories, one tab per repository inside
each, one agent per pane in that tab, all made only when a launch first needs
them; a repository no workspace names is asked about, and `Ctrl-S` in that
picker writes the answer here. Each launch gets a git worktree of its own for
the ticket's branch, and the agent is handed a context file, the workflow
skill and a short opening prompt that asks it to refine the ticket with you
before implementing; when told to go on it implements, verifies, pushes and
opens a linked draft pull request with `ticket-tui prs create`. It needs
ticket-tui to be running inside Herdr; **Copy agent prompt** works anywhere.
The agent's own input box shows the pasted prompt as one truncated line, so
**Show agent prompt** opens the prompt the agent on the work item was sent, and
every launch writes it as `prompt.md` beside `context.md` under
`<database dir>/handoffs/<org>-<id>/`.

```toml
[agents]
default = "cursor"            # or copilot
checkout = "worktree"         # or shared

[herdr]
unmapped_workspace = "Other"

[[herdr.workspaces]]
name = "Payments"
repos = ["payments-api", "settlement-worker"]
```

`ticket-tui` is also a CLI — `list`, `show`, `edit`, `comment`, `create`,
`link`, `repos`, `prs`, `pipelines`, `runs`, `approvals`, `status`, `agent` —
so a script or an agent can do anything the TUI can.

`prs create` opens a pull request linked to the work items it names — at
least one — and is safe to run again: an active pull request between the
same branches is reused untouched and only its missing links are repaired,
and a link Azure DevOps has not confirmed is reported rather than assumed:

```console
ticket-tui prs create --repo payments-api --source 715-fix-duplicate-imports \
  --target main --title "Fix duplicate imports" --description-file pr.md \
  --work-item 715 --draft
```

`link` pins a work item to a repository through a Branch link, making the
branch first when the repository has none of that name:

```console
ticket-tui link 715 ado-helper              # 715-fix-the-thing, from the title
ticket-tui link 715 ado-helper feature/x    # a branch that is already there
```

`comment` takes its body down a pipe, so the tail of a test run reaches a work
item — or a pull request — without going through the clipboard:

```console
cargo test 2>&1 | tail -30 | ticket-tui comment 642 -
```

A piped body is posted as a code block, so its columns line up in the portal
and in the TUI.

`status` prints the numbers the tab bar badges as one line, for a status bar or
a shell prompt in a pane that is not this one — from SQLite alone, in a few
milliseconds, and nothing at all when there is nothing to say:

```console
$ ticket-tui status
doing 4 · stale 2 · review 3 · rejected 1 · ◐ 1 · failed 1
```

## More

- [DESIGN.md](DESIGN.md) — how all of it works, in full: the sync protocol, the
  revision rules an edit obeys, every screen and key, the database schema, and
  the context file agents read.
- [HANDOFF.md](HANDOFF.md) — where the last round of work stopped.
- [LICENSE](LICENSE) — MIT.
