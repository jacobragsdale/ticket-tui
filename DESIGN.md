# ticket-tui design

How ticket-tui works and why, in full: the flags, the sync protocol, the
revision rules an edit obeys, every screen and key, the database schema, and
the context file agents read. [README.md](README.md) is the short way in.

- [Run it](#run-it)
- [Authentication](#authentication)
- [Organization and projects](#organization-and-projects)
- [Sync](#sync)
- [Editing](#editing)
- [Creating work items](#creating-work-items)
- [Deleting work items](#deleting-work-items)
- [Sprint summary](#sprint-summary)
- [Tabs](#tabs)
- [Repos](#repos)
- [Pull requests](#pull-requests)
- [Pipelines](#pipelines)
- [Controls](#controls)
- [Database reference](#database-reference)
- [Subcommands](#subcommands)
- [Live agent context](#live-agent-context)
- [Roadmap](#roadmap)
- [Develop and verify](#develop-and-verify)

## Run it

You need Rust 1.88 or newer, a macOS or Linux terminal, and access to an Azure
DevOps project.

1. Sign in with the Azure CLI:

   ```console
   az login
   ```

2. Point ticket-tui at an organization and project, then pull the work items:

   ```console
   cargo run --release -- sync --org my-org --project my-project
   ```

3. Later runs open the database immediately and pull in the background:

   ```console
   cargo run --release
   ```

4. Press `/`, type part of a ticket title or a filter such as `state:active`,
   and watch the table update. Press `Esc` to leave the search box while
   retaining the filter.

5. Select a ticket and press `o` to open it on `dev.azure.com`.

6. Press `q` to exit.

Press `r` at any time to pull immediately.

`--refresh SECONDS` changes how often the background pull runs, and `--refresh
0` turns the timer off, leaving `r` as the only way to pull:

```console
cargo run --release -- --refresh 300
```

`TICKET_TUI_REFRESH` sets the same interval, and `--query` narrows what a pull
asks Azure DevOps for; both are under
[Organization and projects](#organization-and-projects).

Without a configured organization the TUI runs offline: it browses the database,
never contacts the network, and `r` reports the missing organization. An empty
database then opens to the status line ``Database is empty and offline; run
`ticket-tui sync --org ORG --project PROJECT` to pull work items``.

To use another database file:

```console
cargo run --release -- --database ./tickets.sqlite3
```

## Authentication

ticket-tui borrows the Azure CLI's login. It runs

```console
az account get-access-token --resource 499b84ac-1321-427f-aa17-267ca6975798
```

and sends the result as an `Authorization: Bearer` header together with
`X-VSS-ForceMsaPassThrough: true`, which organizations backed by a Microsoft
personal account require.

Setting `AZURE_DEVOPS_EXT_PAT` to a personal access token takes precedence and
switches to Basic authentication, for environments without the Azure CLI. A
`401` or `302` response is reported as rejected credentials with a reminder to
run `az login`.

An access token expires in about an hour, well within one session, so a request
Azure DevOps refuses is retried once with a freshly minted token before it is
reported.

All of that is Azure DevOps. The ACR and Key Vault tabs address a subscription
instead, which wants tokens for other audiences and has never accepted a
personal access token: see [Azure Resource Manager](#azure-resource-manager).

ticket-tui stores no secrets. It reads the token from the CLI or the environment
on each sync and keeps nothing but work-item data in SQLite. A secret's value
read out of a key vault is held in memory for one minute and written nowhere at
all — not the database, not the session file, not the agent context.

## Organization and projects

Every value is resolved in this order:

1. the `--org`, `--project` and `--code-project` flags;
2. the `TICKET_TUI_ORG`, `TICKET_TUI_PROJECT` and `TICKET_TUI_CODE_PROJECT`
   environment variables;
3. the `[devops]` table in `config.toml`;
4. the `[defaults]` entries in `~/.azure/azuredevops/config`, written by
   `az devops configure --defaults organization=... project=...`
   (`AZURE_CONFIG_DIR` moves that file).

`--org` accepts a bare slug, `https://dev.azure.com/<slug>`, or
`https://<slug>.visualstudio.com`; all three reduce to the slug. Without an
organization and a project the TUI browses the database offline and never
syncs; `ticket-tui sync` with an unresolved value fails with the missing flag,
variable, and command spelled out.

### Two projects, one organization

A shop whose board lives in one project and whose code lives in another says so
once: `project` is where the work items are, `code_project` is where the
repositories, pull requests and pipelines are. Left unsaid, the code project is
the project, which is what one project in one place has always meant.

The split is a property of the URL and nothing else. Every `wit` and `wiql`
call, the teams behind the assignee picker, and the address a work item opens
at use `project`; every `git`, `build`, `pipelines`, `approvals` and
`pullrequests` call goes through `AzureClient::code_url` and uses
`code_project`. One client, one token, one organization: `X-VSS-ForceMsaPassThrough`
still travels on all of them, exactly as `az devops` sends it. The database
overlay's sync line adds `· code <code_project>` when the two differ and says
nothing when they do not.

### The database remembers which project it holds

Every successful pull records the organization and project it ran under in the
`sync_meta` table. A run that resolves a different pair will not sync into a
database that already holds work items: the sync worker never starts, the TUI
opens offline over the rows that are there, and the notification — and the `i`
overlay's sync line — says

```text
Database holds other-org/borealis; pass --database for another project or run `ticket-tui sync --full` to replace it
```

so a typo in `--project`, or a `TICKET_TUI_ORG` left over from yesterday, cannot
quietly replace a database. `ticket-tui sync --full` is how the replacement is
asked for: it pulls the new project in full and re-stamps the database with it. A database
with nothing in it, or one written before this was recorded, adopts whatever the
next pull brings.

### `TICKET_TUI_REFRESH`

`TICKET_TUI_REFRESH` mirrors `--refresh`, so a shell profile can set the pull
interval once. The flag wins when both are given, and a value that is not a
number of seconds is a startup error naming the variable rather than a silent
fall back to the default.

### `--stale-days` and `TICKET_TUI_STALE_DAYS`

How long a work item may sit untouched before the Changed column flags it:

```console
cargo run --release -- --stale-days 21
```

`TICKET_TUI_STALE_DAYS` sets the same threshold, so a shell profile can set it
once. The flag wins when both are given, a value that is not a number of days
is a startup error naming the variable, and either one beats whatever the last
session remembered. **Set stale threshold** in the command palette steps
through 7, 14, 21, and 30 days, has the last word for the rest of the run, and
is what gets saved. See [Stale-item
highlighting](#stale-item-highlighting).

### `config.toml`

One file holds everything a workplace does not change from run to run:

```toml
[devops]                      # tabs 1 to 4, and every subcommand
org = "myorg"                 # slug or https://dev.azure.com/myorg
project = "ISTO"              # where the work items live
code_project = "Fiquants"     # repos, pull requests and pipelines; left out = project
# query = "[System.AreaPath] UNDER 'ISTO\\Team'"   # optional WIQL scope on every pull
# workspace = "~/Development" # where clones live; a leading ~ is the home directory

[azure]                       # tabs 6 ACR and 7 Key Vault
subscriptions = ["<dev-guid>", "<qa-guid>"]   # left out: whatever `az account show` says
registries = ["acrdev", "acrqa"]              # optional: only these, in this order
vaults = ["kv-dev", "kv-qa"]                  # optional: only these, in this order
```

`config.example.toml` at the root of the repository is the whole file,
commented, `[[clusters]]` and `[theme]` included; copying it to
`~/.config/ticket-tui/config.toml` and editing it is the setup.

Every key is optional and every one of them is a default: the flag wins, then
the `TICKET_TUI_*` variable, then this file, then whatever the Azure CLI is
set to — `az devops configure` for the organization and the project,
`az account show` for the subscription. Keys a build does not know are ignored,
so the file can grow without an older binary refusing it. A blank string is
refused where a blank cluster name is, because it would otherwise mask the flag
and the variable behind it: `devops.project is blank; give it a value or leave
it out`. The file is read once, straight after the command line is parsed, and
a file that will not parse stops the TUI and every subcommand alike; the
mid-run reload keeps reporting a broken file in the footer instead, because by
then there is a footer to report it in.

`[[clusters]]` names the kubeconfig contexts the AKS tab reads — the ones
`az aks get-credentials --resource-group RG --name CLUSTER` writes, which
`kubectl config get-contexts` lists. Signing in to a tenant other than the
login's default wants `az login --tenant <tenant>` first.

### `--theme`, `TICKET_TUI_THEME` and `config.toml`

Which palette the TUI paints with:

```console
cargo run --release -- --theme terminal-light
```

There are four. `terminal` is the default: the sixteen ANSI colours, so
whatever palette the terminal itself is set to shows through, over SSH too.
`terminal-light` swaps the few of those that vanish on a white ground.
`mono` is what the standard `NO_COLOR` variable selects, and `NO_COLOR` beats
everything else: every colour reset, so weight and glyphs carry each
distinction alone. `custom` is built from a palette in
`$XDG_CONFIG_HOME/ticket-tui/config.toml` (`~/.config/ticket-tui/config.toml`
by default, on macOS as well):

```toml
[theme]
preset = "custom"          # optional: terminal · terminal-light · mono · custom

[theme.custom]             # what `theme apply` writes, in its own words
name = "neon-void"
appearance = "dark"
bg = "#05060a"
bg_deep = "#000000"
surface = "#0b0d14"
overlay = "#171b28"
fg = "#dfe6ff"
subtle = "#aab4dd"
muted = "#626c9c"
accent = "#c07cff"
red = "#ff5f87"
green = "#4ef5a4"
yellow = "#ffd75f"
blue = "#61a8ff"
cyan = "#4fe8ff"
orange = "#ff9e5e"
teal = "#29e0c8"
```

The table is the vocabulary of the `theme` tool, which applies one palette to
every program on the machine and has ticket-tui as one of its targets; a
palette written by hand works the same way. `preset` may be left out: a file
with a `[theme.custom]` table paints with it, and a file without one paints
with `terminal`. The flag wins over `TICKET_TUI_THEME`, both win over the
file, and a name that is none of the four is a startup error naming them.
The file is optional; a file that does not parse, or a `custom` preset with no
palette, is reported in the footer and the theme stays as it was.

A running ticket-tui reads the file again whenever it changes — it looks at
the file's clock once a second, on wake-ups the event loop makes anyway — so
`theme pick` repaints the app live, and the footer says which palette it is
now showing. A theme chosen by the flag or the variable keeps winning over the
file for the whole run.

### `--workspace` and `TICKET_TUI_WORKSPACE`

Where the Repos tab looks for clones and makes new ones:

```console
cargo run --release -- --workspace ~/src
```

`TICKET_TUI_WORKSPACE` sets the same directory and `workspace` under `[devops]`
in `config.toml` is the third way to say it, in that order of precedence; a
leading `~/` in the file is the home directory. Without any of the three it is
`~/Development`. See [The workspace](#the-workspace).

### `--query`: how much of the project to sync

A project too large to hold whole is narrowed with `--query`, or
`TICKET_TUI_QUERY`, which takes one extra WIQL condition:

```console
cargo run --release -- --query "[System.ChangedDate] > @today-180"
cargo run --release -- --query "[System.WorkItemType] <> 'Test Case'"
```

The condition is ANDed into both the full and the incremental query, in
parentheses so its own `OR` cannot swallow the clauses around it, and is passed
to Azure DevOps verbatim: WIQL is its dialect to parse, so a mistake comes back
as the usual sync failure notification rather than as a local guess about what
is legal.

The scope is stored in `sync_meta` as `sync_scope`. Changing it — or dropping it
— makes the next pull a full one, because a watermark says what changed, not
what the old condition kept out: only a full pull can bring in what a widened
scope now admits and drop what a narrowed one now excludes.

The `i` overlay's sync line names all of it: the organization and project, the
refresh interval or `on request` when the timer is off, the scope when one is
configured, and how the last pull went.

## Sync

A sync worker pulls in the background on a timer, every 60 seconds by default,
and whenever `r` asks it to. The TUI opens from the database straight away and
the first pull runs behind it, so a state flipped in Azure DevOps appears within
one interval without a keypress. `ticket-tui sync` runs one pull and exits,
without opening the TUI at all. Only one
pull runs at a time: `r` during one reports `Sync already in progress`.

The table title carries the sync state — `Syncing…`, `Synced just now`,
`Synced 2m ago`, or `Sync failed` until the next success — and `i` shows the
same in the database overlay. A timer pull that keeps failing the same way says
so only in the title; a pull `r` asked for always reports itself.

Pulls are incremental. Each one asks for the work items edited since the last
successful pull, using the watermark that pull left behind:

```sql
SELECT [System.Id] FROM WorkItems
WHERE [System.TeamProject] = @project AND [System.ChangedDate] >= '2026-08-28T20:15:03Z'
ORDER BY [System.Id]
```

The watermark is the greatest `System.ChangedDate` the last pull actually saw,
never a wall clock reading: a client whose clock runs fast would otherwise step
straight past edits it never read. It is stored in the database's `sync_meta`
table as `watermark_changed_at` and written down to the second, so the
comparison is inclusive and the work item it came from is read once more rather
than an edit made in the same second being missed.

Every pull also runs the plain id query:

```sql
SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = @project ORDER BY [System.Id]
```

Deleting a work item is not an edit — it stops being listed — so this is what
catches one moved to the recycle bin, and the rows it no longer names are
removed along with their links, comments, and history.

A configured [sync scope](#--query-how-much-of-the-project-to-sync) is ANDed
into both queries in parentheses, so the same reconciliation drops a work item
an edit has moved outside the scope:

```sql
SELECT [System.Id] FROM WorkItems
WHERE [System.TeamProject] = @project AND ([System.WorkItemType] <> 'Test Case')
ORDER BY [System.Id]
```

Whatever the changed-since query names is read in batches of 200 from
`/_apis/wit/workitems` with `$expand=relations` and written in one transaction,
each work item's own row and outgoing links replaced and everyone else's left
untouched. The watermark advances only after that batch is committed. When
nothing changed and nothing was deleted, nothing at all is written: an idle
project costs exactly two queries a minute, the database's timestamp does not
move, and no other ticket-tui or agent reading the file reloads for nothing.

A pull runs in full — every work item, replacing the stored rows wholesale —
when there is no watermark to start from: a fresh database, a database whose
schema this build rebuilt, or `ticket-tui sync --full`, which is the way to
rebuild one deliberately. A full pull leaves a watermark behind, so the pulls that
follow it are incremental again.

Azure DevOps throttles a client that asks too often, and a poller that ignores
it only makes the throttling worse. A pull refused with `429` or `503` is not a
failed sync: the wait its `Retry-After` header names — thirty seconds when it
names none — pushes the next pull out that far, the table title reads
`Sync paused 2m` instead of `Sync failed`, and nothing is announced. Every
throttle in a row after the first doubles the refresh interval as well, up to
ten minutes, and the first pull that gets through puts it back to the configured
one. `--refresh 0` stays off throughout: a throttle is a reason to pull later,
never a reason to start pulling. Successful responses are read the same way —
`X-RateLimit-Remaining` at zero holds the next pull until the
`X-RateLimit-Reset` the response names, while the work items it carried are
stored as usual.

Edits, comments, and details fetches are somebody waiting at the keyboard, so
they are not simply postponed: a throttled one waits out the delay on the sync
thread, capped at a minute, and goes out once more. A second refusal is reported
as the rejection it is — `Azure DevOps is throttling requests; try again in 45s`
— and the row it belongs to is put back.

A pull that `r` asked for reports itself in the status line:
`Synced 3 changes from <org>/<project>`, `Synced 52 work items from
<org>/<project>` after a full pull, or `Nothing changed`. A timer pull only
updates the table title, which still moves to `Synced just now` when the pull
found nothing.

Fields map onto the cache as follows:

| Cache | Azure DevOps |
|---|---|
| `organization` | the org slug, not the URL |
| `project`, `work_item_type`, `title`, `state`, `reason` | the matching `System.*` fields |
| `assigned_to` | `System.AssignedTo` display name, falling back to the unique name |
| `priority` | `Microsoft.VSTS.Common.Priority` |
| `tags` | `System.Tags`, split on `;` |
| `description` | `System.Description` rendered as plain text |
| `description_html` | `System.Description` exactly as Azure DevOps stores it |
| `created_at`, `changed_at` | `System.CreatedDate` and `System.ChangedDate` |
| `web_url` | `https://dev.azure.com/<org>/<project>/_workitems/edit/<id>` |

Descriptions are written in a browser rich-text editor, so both readings are
kept: `description` is what the details pane draws, and `description_html` is
the document an edit hands back. The rendering keeps the structure a reader
needs and drops the rest. `<ul>` items keep their bullet and `<ol>` items are
numbered, nested lists indent two spaces per level, headings and paragraphs are
separated by one blank line at most, `<a href>` renders as `text (url)`,
`<code>` outside a `<pre>` is wrapped in backticks, a `<pre>` keeps its
whitespace, a table renders one row per line with cells joined by ` | `, an
image becomes `[image: alt]`, and `<hr>` becomes `───`. Bold, italics, spans,
and every unrecognised tag are transparent: the text survives, the markup does
not. Numeric entities and the names the editor emits are decoded, and markup
that never closes is rendered as the text it looks like rather than dropped.

Hierarchy links become parent and child relations; related, predecessor,
successor, and duplicate links are stored as themselves. Other link types, such
as attachments, are ignored.

The first pull a work item type appears in also reads
`/_apis/wit/workitemtypes/<type>/states` and stores that type's states in
`work_item_type_states`, which is what the state picker offers. A type the
database already holds states for is not asked about again, so a run that opens
a filled database makes no states requests at all; a request that fails is
retried on the next pull and never sinks it.

The first pull also reads `/_apis/profile/profiles/me` for the signed-in display
name and stores it in the cache's `sync_meta` table. The profile host is
separate from the work-item host, so a failure there is skipped rather than
sinking the sync. Work items assigned to that name render bold in the accent
colour in the Assignee column and in the details pane. Set `TICKET_TUI_ME` to
override the stored name, for anyone whose profile name differs from the name
their work items are assigned to.

Comments and revision history come down per work item rather than with the
project, from two more endpoints:

```
GET <org>/<project>/_apis/wit/workItems/<id>/comments?api-version=7.1-preview.4
GET <org>/_apis/wit/workItems/<id>/updates?api-version=7.1
```

Comment bodies are rendered from HTML the same way descriptions are, though
only the text is stored: a comment is read, not edited. Revisions
keep only the changes a person reads a history for — state, assignee, title,
iteration, area, priority, tags, and reason — so a revision that moved nothing
but the revision number, the changed date, the comment count, or the watermark
is dropped whole, and an identity is stored as the name it displays under.
Azure DevOps stamps the newest revision's `revisedDate` with `9999-01-01`
instead of a date, because nothing has revised it yet; that revision's own
`System.ChangedDate` stands in.

An incremental pull reads both for every work item it found changed, in the
same transaction that stores the work item itself: something that just moved is
something somebody is about to look at. A full pull does not — two more
requests per work item is a price only a handful of changes can pay — so
whatever it left unread is read lazily instead.

Each row carries `details_rev`, the revision its stored comments and history
belong to. When the selection rests for 300 ms on a work item whose
`details_rev` is behind its `revision`, that one work item is read. Scrolling
reads nothing, because the trigger is the selection settling rather than the
selection changing; one request is out at a time; and a work item that could
not be read is reported once and never asked about again. The details pane
shows `Loading comments and history…` where the history is about to appear, and
when the answer arrives only that work item's comments and history change —
nothing else is reloaded and no other row moves. An accepted edit sets
`details_rev` back to zero, because the copy Azure DevOps sent back is a new
revision whose history has not been read.

The history renders one line per change — `2h ago · Jacob Ragsdale · State: To
Do → Doing`, with the exact UTC instant beside it — oldest revision first.

## Editing

Work items are written back to Azure DevOps as they change, one field at a
time, and every edit takes the same path. The row changes in the table straight
away, the sync worker sends a JSON Patch document to
`PATCH /_apis/wit/workitems/<id>`, and what happens next depends on the answer.

The document always leads with a revision test:

```json
[
  {"op": "test", "path": "/rev", "value": 12},
  {"op": "add", "path": "/fields/System.State", "value": "Doing"}
]
```

so Azure DevOps refuses the whole write if the work item changed after it was
loaded. On success the copy Azure DevOps stored — its new revision and changed
date included — replaces the row, is written to SQLite on its own without
touching any other record, and the status line reports
`Updated #613 · State → Doing`. The row then re-sorts and re-filters if the
change moved it, and the selection follows its work item rather than its row
number.

A refusal puts the row back exactly as it was and names the field, so a change
is never dropped quietly. A conflict reads `#613 changed in Azure DevOps since
it was loaded; State not saved — syncing the latest copy` and asks for a pull at
once, so the value somebody else wrote appears. Anything else Azure DevOps
refused is reported as it came.

Edits ride the same worker as pulls and are handled in the order they arrive, so
typing is never blocked and an edit queued before a pull is written before that
pull reads. If a pull finishes while an edit is still in flight, the edit stays
on screen over the rows the pull brought. There is no offline queue: without a
configured organization an edit is refused before anything changes, and an edit
that cannot be sent is reverted rather than saved for later.

Sprint hygiene means flipping ten work items at once, so the state picker, the
assignee picker, and the Iteration tree act on every checked row — see `Space`
below — when two or more are checked, rather than on the row under the cursor.
The overlay title says which it is: `State · #613` for one work item and
`State · 5 tickets` for a bulk change, so the scope is unmistakable before
`Enter` is pressed. Every row changes on screen at once, and one edit goes out
a work item — no `$batch` endpoint, just sequential writes, each with its own
revision test — which the worker takes in the order the table holds them. A
work item already carrying the value chosen is passed over rather than
rewritten; a change nothing is left to do closes with `Nothing to change · State
→ Doing`.

A bulk change speaks once, when the last work item has answered:
`Updated 5 tickets · State → Doing`, or `Updated 5 of 6 · #612 failed: the
transition is not allowed` when something did not land, naming the first three
refusals and counting the rest. A refusal reverts only the row it names — the
others stay changed — and the checked set survives the whole thing, ready for
the next change. Every other editor stays on the row under the cursor: the same
title or the same description on ten work items is never what was meant.

`u` takes the last edit back. Every change is immediate, so a mis-click on the
state picker needs a way out that is just as quick: `u` reads the value the work
item carried before the edit and writes it back down the path above — an
ordinary JSON Patch with its own fresh revision test, so a work item somebody
else has moved since refuses the undo exactly as it would refuse any other edit,
and reports it the same way. When it lands the status line reads `Undid State on
#613 (Doing → To Do)`. A field that was empty before the edit goes back to
*empty* rather than to an empty value: an undone priority or assignee is the
same `remove` the `Clear` and `Unassigned` rows send.

A bulk change is one entry on the stack however many work items it touched, so
one `u` puts all of them back and one summary reports it — `Undid State on 3
tickets`, or `Undid 2 of 3 · #613 failed: it changed in Azure DevOps` when part
of it did not land, so an undo is never left half done in silence. The stack
holds the last twenty edits and is kept in memory only, so it starts empty every
run and `u` with nothing on it says `Nothing to undo`. An undo is not itself
undoable — taking one back would make `u` a toggle between the last two values,
and the edit under it would never be reached — and a comment cannot be undone at
all, being a post rather than a field.

Every editor is reachable two ways. Clicking a field's value in the details
pane opens that field's editor where the value is, as a dropdown anchored under
it — one click, not two — and `Enter` does the same for the value under the
pointer while the details pane is focused, with the link line still opening the
work item. The keyboard opens the same editors centred, and both paths run the
same command and write the same edit; only the placement differs. The
description is the exception: it is long-form, so no dropdown could hold it and
it is reached from the Actions menu or the palette instead.

`e` opens the Actions menu, which lists the fields that can be changed; `S`
(capital, because `s` is the sort menu) skips it and opens the state picker
directly. The picker lists the states the selected work item's type allows,
coloured by category and with the state it is in already under the cursor.
`Enter` writes the state chosen down the path above, `Esc` changes nothing, and
choosing the state it is already in closes without a write. A transition Azure
DevOps refuses puts the row back and says why. Opened over checked rows it moves
all of them; the states it offers are still the selected work item's type's,
which is the only type it could ask about, and a state another checked work
item's type does not allow is refused by Azure DevOps and named in the summary.

The picker never waits for the network. It offers the states cached in
`work_item_type_states` when a pull has fetched them, and otherwise the distinct
states already in the database for that type, ordered by category — Proposed,
In Progress, Resolved, Completed, Removed — then by name, so it opens instantly
on a database that has never reached Azure DevOps.

The Actions menu's remaining rows are the edits that would otherwise mean opening a
browser. Title, Priority, Tags, Iteration, Area, Set parent, Remove parent,
Description, and Add comment have no key of their own; they are reached through
`e`, or by name in the command palette as `Edit title`, `Edit priority`,
`Edit tags`, `Change iteration`, `Change area`, `Set parent…`,
`Remove parent`, `Edit description`, and `Add comment`. Assignee is reached the
same way, and also directly with `a`.

**Title** opens a one-line field prefilled with the title, edited with the same
keys as the named-view editor — `←`/`→`, `Home`/`End`, `Ctrl-W`, `Ctrl-U`, and
paste. `Enter` writes `System.Title`, `Esc` changes nothing. Surrounding
whitespace is trimmed before the write; a title that is empty or only whitespace
is refused here rather than sent, and the prompt stays open on it; a title
saved back unchanged closes without a request.

**Priority** opens a five-row list — 1, 2, 3, 4, and `Clear` — in the colours
the Pri column uses, with the priority the work item already has under the
cursor. Choosing a number writes `Microsoft.VSTS.Common.Priority`; choosing
`Clear` sends a JSON Patch `remove` for the field, because a priority goes back
to unset by being taken off the work item rather than set to an empty value, and
the Pri cell empties. Choosing the priority it already has closes without a
write.

**Tags** opens a one-line field prefilled with the tags as `a; b; c`. On save
the text is split on `;`, each tag trimmed, empties dropped, and a repeat of a
tag already listed dropped whatever its case — the first spelling is the one
kept — then rejoined with `; ` for `System.Tags`. So `rust; Rust ;; tui` saves
as `rust; tui`. An empty result clears the tags, which `System.Tags` accepts as
an empty string rather than a `remove`. A list that normalises to what is
already there closes without a write.

**Assignee** has a key of its own, `a`, as well as its Actions menu row and
`Change assignee` in the palette, because assigning work is the edit worth
reaching for. It opens a filterable list: type to narrow it, `↑`/`↓` to move,
`Enter` to assign, `Esc` to change nothing. Whoever holds the work item is
marked and under the cursor, and choosing them closes without a write — unless
the picker was opened over checked rows, when it reassigns all of them and
whoever holds the row under the cursor is a change worth making to the rest.
The list runs `Unassigned` first, then you — marked `(me)` — then everybody the
database has ever seen a work item assigned to, sorted, and finally the rest of
the project's teams. Nobody is offered twice, however their name is spelled.

The write goes out as the person's unique name — their sign-in address — when
one is known and as their display name when it is not; Azure DevOps resolves
either. The Assignee cell reads as the display name straight away whichever was
sent, and the copy that comes back from the write settles the spelling.
`Unassigned` sends a JSON Patch `remove` for `System.AssignedTo`, because a work
item goes back to nobody by having the field taken off it, and the cell empties.

The picker never waits for the network. The people it lists come out of the
database, which on a small project is already the whole team. The project's
teams — `GET /_apis/projects/<project>/teams`, then the members of each — are
read once a run, the first time the picker is opened, and merged into the open
list where it stands. They are cached in the `identities` table, so from the
next run the picker is complete the moment it opens. Teams that cannot be read
are passed over in silence: nothing is logged and nothing is said, because the
list the database gave is enough on its own.

**Iteration** and **Area** open the project's classification trees as indented
rows, two spaces a level, each row naming the leaf with the rest of the path
implied by the indent. The node the work item sits in is marked and under the
cursor; type to narrow the tree, `↑`/`↓` to move, `Enter` to move the
work item, `Esc` to change nothing. Choosing the node it is already in closes
without a write. Iteration is the one of the two worth choosing for several
work items at once — a sprint ends and its leftovers move on together — so it
moves every checked row; Area stays on the row under the cursor. An iteration
row also carries the days it runs between — `Aug 25 – Sep 5` — and the one
containing today (UTC) is marked `current`.

`Enter` writes the full backslash path — `development\Sprint 1`, not
`Sprint 1` — to `System.IterationPath` or `System.AreaPath`. The Iteration and
Area table columns go on showing only the leaf, and the `iteration:` and `area:`
search filters go on matching it.

These pickers never wait for the network either. Both trees come from one
request — `GET /<project>/_apis/wit/classificationnodes?$depth=10` — read once a
run, the first time either picker is opened, and merged into the open list where
it stands. The value a work item's field carries is not the `path` each node
reports (`\development\Iteration\Sprint 1`), so it is rebuilt from the names
on the way down: the project root, then the descendants, without the `Iteration`
or `Area` segment that only separates the two trees. The result is cached in the
`classification_nodes` table, and a cache under an hour old is used as it
stands, so a picker opened soon after a previous run touches the network for
nothing at all. Before anything is cached, and whenever the trees cannot be
read, both pickers list the distinct iteration and area paths the work items in
the database already carry, which is every sprint actually in use.

**Description** is the one long-form field, so it is the one edit that does not
happen in the TUI at all: it hands the description to your own editor and takes
the terminal back when you are done. The editor is `$VISUAL`, then `$EDITOR`,
then `vi`; the variable is split on whitespace, so `EDITOR="code --wait"` runs
`code --wait <file>`. The TUI leaves the alternate screen, gives mouse capture
and bracketed paste back, and writes the description to `ticket-613.md` in a
fresh temporary directory that is deleted afterwards. On the way back it takes
raw mode, the alternate screen, and both input modes again and repaints from
scratch — the editor drew over the screen, so nothing on it is trusted — and it
does that whether the editor saved, exited non-zero, or was never there to run.
An editor that failed is reported as `#613 description not saved: vi exited
with exit status: 1`, and nothing is written.

The file is Markdown, not HTML: paragraphs separated by a blank line, `- `
bullets and `1.` numbers indented two spaces a level, `[text](url)` links,
backtick `code`, triple-backtick fenced blocks for `<pre>`, `#` headings,
`**bold**`, and `---` rules. Saving converts it back the way it came, so a description that
goes out and comes back untouched reads exactly as it did. A file that comes
back byte for byte as it was written is not an edit at all: nothing is sent and
the status line says `#613 description unchanged`. An emptied file clears the
description.

Some formatting has no Markdown here — tables, images, colours and inline
styles, spans carrying attributes of their own. A description holding any of it
opens with

```
<!-- rich formatting in this description will be replaced on save -->
```

as its first line, so you can quit without saving rather than flatten it. The
notice is taken off again before anything is compared or converted, so leaving
it in place changes nothing. Everything else goes down the usual path:
`System.Description` is written with a revision test like any other field, and
the details pane shows the new description before the network answers.

**Set parent** files the work item under another one, and is how a ticket filed
under the wrong Epic is moved without opening a browser. It opens a picker over
every work item the database holds, each row reading `#613 Issue Fix ticket
search`; typing filters on the id and the title at once, so `613` and
`dispatcher` both find the same row, and the parent it hangs under now opens
under the cursor and is a no-op if chosen again.

The work item itself and everything below it are left out of the list. A work
item cannot be its own ancestor, so offering a descendant could only ever earn a
refusal; leaving them out makes a cycle something the picker cannot ask for
rather than something Azure DevOps has to catch. Azure DevOps still catches the
one case the picker cannot see — a family that moved in the browser since the
last pull — and that refusal is reported in its own words with the move put
back.

**Remove parent** takes the work item out of its family and leaves it hanging
under nothing. It is the one Actions menu row that is not always there: it appears
directly under `Set parent…` when the work item has a parent, and is absent when
it has none, so the menu never offers a removal there is nothing to remove.

A parent is not a field, so neither is written like one. It is an entry in the
work item's `relations` array, and Azure DevOps removes a relation by the
position it holds in that array — which only a copy read now knows. So the
worker reads the work item with `GET /_apis/wit/workitems/<id>?$expand=relations`
and builds one patch document against exactly what came back:

```json
[
  {"op": "test",   "path": "/rev", "value": 9},
  {"op": "remove", "path": "/relations/1"},
  {"op": "add",    "path": "/relations/-",
   "value": {"rel": "System.LinkTypes.Hierarchy-Reverse",
             "url": "https://dev.azure.com/<org>/_apis/wit/workItems/22"}}
]
```

The revision test leads, so a work item somebody else changed between the read
and the write is refused rather than patched against indices that have moved.
The removal names the index the parent link sits at in the copy just read. The
append comes last, because appending cannot shift an index the removal above it
still needs. Setting a first parent sends only the test and the append; removing
one sends only the test and the removal. All of it is one `PATCH`, so there is
no moment in which the work item has been detached but not re-filed.

The move is optimistic in both directions. A hierarchy link is held twice — the
child names its parent, the parent names its child — so both halves are
rewritten the moment the choice is made: the family tree redraws under the new
parent, the epic the work item left stops listing it, and the child progress of
both parents is recounted before anything reaches the network. The status line
reads `Moving #613 under #22…` while it is out and `Moved #613 under #22` when
it lands, at which point the graph settles on the links Azure DevOps sent back
rather than on the guess. A refusal puts both halves and both ratios back and
says why: `#613 not moved: TF201036: adding this link would create a circular
relationship`. One move per work item is in flight at a time.

SQLite is written the same way. `work_item_relations` holds both halves, and
Azure DevOps answers a move with the moved work item alone, so the child link
the old parent still held is cleared in the same transaction that writes the new
parent link — no reader ever sees the work item in two families or in none.

**Add comment** is the last Actions menu row, and `Add comment` in the palette. It
opens a one-line box — empty, because there is nothing to edit, only something
to say — titled `Comment on #613` and edited with the same keys as the other
prompts. `Enter` posts, `Esc` changes nothing, and a comment that is empty or
only whitespace is refused here rather than sent, with the box left open on it.
One line is the whole of it: long-form comments still belong in the browser.

A comment is not a field, so it is not a JSON Patch and carries no revision
test. What was typed is escaped — `&`, `<`, and `>`, the three characters that
would otherwise be read as markup — wrapped in a paragraph, and sent as
`POST /<project>/_apis/wit/workItems/<id>/comments` with a body of
`{"text": "<p>merged into main</p>"}`.

It is also the one write that is not optimistic. A comment has no id, date, or
author until Azure DevOps gives it one, and a line that turned out never to have
been posted is worse than a moment's wait, so nothing appears until the server
answers. The status line reads `Posting comment on #613…` while it is out and
`Commented on #613` when it lands, at which point the comment is written to
`work_item_comments` on its own and appears at the foot of the details pane's
Comments section. A refusal writes nothing at all and says so:
`#613 comment not posted: HTTP 403: the work item is read only`. One comment
per work item is in flight at a time; a second says `an earlier comment is
still in flight`.

The comment survives the next refresh either way. Posting one moves the work
item's `System.CommentCount`, and with it its revision and `System.ChangedDate`,
so the next incremental pull sees the work item as changed, refetches it, and
replaces its comments with the server's list — which includes the new one. If a
process template ever does not move the changed date, the pull does not name
that work item at all, so nothing replaces its comments and the row inserted
locally stays exactly where it was. The insert deliberately leaves `details_rev`
alone, so the next lazy details fetch still reads the discussion and settles it
against the server.

## Creating work items

New work starts in the terminal too. `n` opens the new work item form — the
first multi-field overlay in the app — over whatever is on screen:

```
┌ New work item ────────────────────────────────────────────── [×] ┐
│ › Type *      Issue                                            ▾ │
│   Title *     Back off on throttling                             │
│   Parent      none — a work item id                              │
│   Iteration   development\Sprint 1                             ▾ │
│   Area        development\Platform                             ▾ │
│   Assignee    Jacob Ragsdale                                   ▾ │
│   Priority    2                                                  │
│   Tags        sync; infra                                        │
│                                                                  │
│ [Create]  [Cancel]                                               │
└──────────────────────────────────────────────────────────────────┘
```

`↑`/`↓` and `Tab`/`Shift-Tab` move between fields, wrapping at both ends. Four
of them are chosen from a list rather than typed — the `▾` says which — and
`Enter` on one opens the same picker the Actions menu opens over a work item: the
work item types the project's process offers, the iteration and area trees, and
the assignee list, each writing its choice back into the field and returning to
the form. `Enter` on a typed field moves on to the next one; submitting is
deliberately not bound to it, so a stray `Enter` halfway down the form never
files a half-typed work item. `Ctrl-S` or `[Create]` files it, `Esc` or
`[Cancel]` closes it. Clicking a typed field focuses it and puts the caret where
the click landed; clicking a `▾` row drops that picker down, because a chevron
that answers only the keyboard is a chevron that lies. The two buttons are
clickable.

The defaults are the ones that are usually right. **Type** starts at `Issue`,
the Basic process's everyday unit of work, and the picker lists the project's
own types read from `GET /<project>/_apis/wit/workitemtypes` once a session and
cached in `work_item_types` for the next run — less the ones the process has
disabled and the ones it keeps in its hidden category, which are the code review
and feedback requests nobody files by hand. Before that fetch lands the picker
offers every type the database already holds a work item of, so the form never
waits for the network. **Iteration** and **Area** start where the selected work
item sits, the iteration falling back to the sprint the project is in, because
new work almost always joins the work beside it. **Parent** is a work item id,
left empty for work that hangs under nothing.

**Type** and **Title** are required; the rest are optional and are left off the
document when empty. A refusal that can be made before the network is made
before the network, with the cursor moved to the field at fault: `Title is
required`, or `Priority must be a whole number, not "high"`. Tags are typed the
way the Tags prompt takes them — semicolon separated — and normalised the same
way.

Submitting sends the fields as a JSON Patch document to
`POST /<project>/_apis/wit/workitems/$<type>?$expand=relations`. A parent
travels as a link rather than as a field:

```json
[
  {"op": "add", "path": "/fields/System.Title", "value": "Back off on throttling"},
  {"op": "add", "path": "/fields/Microsoft.VSTS.Common.Priority", "value": 2},
  {"op": "add", "path": "/relations/-", "value": {
    "rel": "System.LinkTypes.Hierarchy-Reverse",
    "url": "https://dev.azure.com/<org>/_apis/wit/workItems/613"
  }}
]
```

Like a comment, this is not an optimistic write. A work item has no id,
revision, or URL until Azure DevOps gives it one, so nothing appears in the
table until the server answers: the status line reads `Creating Issue…` while it
is out. When it lands the copy Azure DevOps stored joins the rows with the links
it came back carrying — so the family tree shows it under its parent, and the
parent's child progress counts it, at once — and the selection moves onto it.
The status line reads `Created Issue #613`. The sync worker writes it to SQLite
itself on the way through, so nothing is reloaded behind it.

A row nobody could see is worth no filter, so a new work item the query on
screen would hide clears that query and says so:
`Created Issue #613 · search cleared so it is visible`. A search term counts as
hiding it whatever it says, because the matching runs on the search thread and
answers a frame or two later.

A refusal writes nothing at all and reopens the form with everything still in
it, so the reason can be answered where it was caused:
`Work item not created: TF401320: rule error`. One create is in flight at a
time; `n` or `N` while one is out says `A work item is already being created`.

`Esc` keeps the draft. The form closes, the table comes back, and the next `n`
brings the form back exactly as it was — every field and the cursor with them —
so closing it to go and read something is not the same as retyping it. The draft
lives in memory for the session only: the session file records how the table is
arranged, not a half-typed work item, so it is gone at quit.

Breaking an Epic into Issues or an Issue into Tasks is the commonest thing
anybody files, and none of it is worth retyping, so `N` — or the Actions menu's
**New child** row, or `New child` in the palette — opens the same form already
knowing what it is filing:

```
┌ New child of #595 ────────────────────────────────────────── [×] ┐
│ › Type *      Issue                                            ▾ │
│   Title *     Cache the work item types                          │
│   Parent      #595 Tech debt and architecture foundation         │
│   Iteration   development\Sprint 1                             ▾ │
│   Area        development\Platform                             ▾ │
│   Assignee    nobody                                           ▾ │
│   Priority    unset — 1 to 4                                     │
│   Tags        semicolon separated                                │
│                                                                  │
│ [Create]  [Cancel]                                               │
└──────────────────────────────────────────────────────────────────┘
```

**Type** is the type the parent's own type breaks down into: an Epic into
Issues, an Issue into Tasks. Which process the project works to answers that,
and the app works it out from the types the project offers rather than from the
order they arrived in — `GET /<project>/_apis/wit/workitemtypes` answers in an
order of its own, and an org whose list reads `Issue, Epic, Task` had an Epic
filed under an Issue for it. The four stock breakdowns are held in the app —
Basic `Epic → Issue → Task`, Agile `Epic → Feature → User Story → Task`, Scrum
`Epic → Feature → Product Backlog Item → Task`, CMMI `Epic → Feature →
Requirement → Task` — and the project gets the one naming most of the types it
offers. An Agile project has an Issue as well, but four of its own chain's names
beat three of Basic's; a tie, and a project whose types have not been read yet,
goes to Basic. A type with nothing under it keeps its own, and so does one whose
child the project does not offer, because a child of the same type is always
defensible and an empty Type field never is. **Parent** is fixed: it reads as
the work item rather than as its id, takes nothing typed at it, and opens no
picker. **Iteration** and **Area** are the parent's, because a child is planned
beside its parent unless somebody moves it — both are still pickers, so moving
it is one `Enter` away.

Everything else about the form is the same: the same required fields, the same
refusals, the same `Ctrl-S`. The draft `Esc` keeps is kept per parent, so a
child half typed under one work item is never offered under another, and neither
`n` nor `N` ever opens what the other was left holding. When the create lands
the child appears under its parent in the family tree at once, because the copy
Azure DevOps stored comes back carrying the link.

## Deleting work items

Occasionally a ticket is filed by mistake. The Actions menu's **Delete work item…**
row — or `Delete work item…` in the palette — takes it back out again. There is
no key bound to it: every other editor is a keypress away because the worst it
can do is a value somebody types over, and this one takes the work item off the
board.

Nothing goes anywhere until a confirmation says what is about to happen:

```
┌ Delete ────────────────────────────────────────────────────── [×] ┐
│ Delete #595 Tech debt and architecture foundation?                │
│                                                                   │
│ Its 8 children are not deleted — left with no parent.             │
│ It goes to the Azure DevOps recycle bin and can be restored from  │
│ there.                                                            │
│                                                                   │
│ [Delete]  [Cancel]                                                │
└───────────────────────────────────────────────────────────────────┘
```

The child count is the point of the whole overlay. A delete takes the one work
item and nothing under it, so an Epic over eight issues leaves eight issues
hanging under nothing — which is the moment somebody wants telling, before the
delete rather than after it. A work item nobody broke down says nothing about
children at all.

`d` confirms and `Esc` cancels; `Enter` is deliberately not it, so a stray
`Enter` never deletes anything. Both buttons are clickable, and cancelling
closes silently: nothing was written, so there is nothing to report.

Confirming sends `DELETE /_apis/wit/workitems/<id>` with no `destroy`
parameter, which is the **soft** delete: Azure DevOps takes the work item out of
every query and every board and keeps it in the project's recycle bin, where
somebody who deleted the wrong thing restores it. The overlay says so, because a
confirmation more frightening than the action warrants is its own kind of wrong.

Like a comment and a create, this is not an optimistic write. The row stays on
the table while the delete is out — a row dropped for a delete that was refused
is a lie the next pull undoes — and leaves when Azure DevOps answers. When it
does, the row, its links in both directions, its discussion, and its history all
go from memory and from SQLite, its parent's child-progress ratio drops by one,
and the status line reads
`Deleted #613 · restore it from the Azure DevOps recycle bin`. The cursor takes
the row that moved up into its place, so deleting a run of rows reads as working
down the list, and the details pane reads that work item from the top rather
than pointing at one that is gone. A refusal changes nothing at all and says
`#613 not deleted: TF401232: read only`.

A delete is not undoable. It never reaches the `u` stack, and an edit already on
the stack for the work item is dropped with it, because there is no longer a row
to put anything back on. The way back is the recycle bin, in the browser.

With two or more rows checked the confirmation covers all of them —
`Delete 2 tickets?`, with their children counted together — and one `d` sends
them, one request each, in the order the table holds them. A child going the
same way is not an orphan, so deleting a parent and its children together warns
about neither. The whole thing speaks once, when the last answer is in:
`Deleted 5 tickets`, or `Deleted 4 of 5 · #612 failed: read only` when something
did not land, naming the first three refusals and counting the rest. A work item
that was refused stays exactly where it was.

## Sprint summary

**Sprint summary** in the command palette — no default key — opens a read-only
board for one iteration, worked out from the work items already in memory, so it
opens instantly and reads the same offline as it does after a pull:

```
┌ Sprint summary · Sprint 1 ──────────────[×]┐
│  Assignee      To Do  Doing   Done  Total  │
│› Avery Chen        3      2      4      9  │
│  Blake Ford        2      1      6      9  │
│  Unassigned        4      0      1      5  │
│  Total             9      3     11     23  │
│                                            │
│  By type                                   │
│  Task                                  14  │
│  Bug                                    6  │
│  User Story                             3  │
│                                            │
│  23 items · 11 done (48%) · 4 stale        │
└────────────────────────────────────────────┘
```

The grid has one row per person, a row for the work nobody owns, and a Total
row. Its columns are the three stations a board has, and work is put under them
by Azure DevOps state *category* rather than by state name, so `To Do`/`Doing`/
`Done`, `New`/`Active`/`Closed`, and every other process template's spelling
land in the same three columns. Resolved counts as in flight: somebody is still
waiting on work that is only ready for test.

**The counts are taken over every work item on file, not over the rows the
table is showing.** The table hides finished work by default, and a sprint
summary that inherited that would report a Done column that never filled up.
The `4 stale` figure is the same rule the Changed column paints — see
[Stale-item highlighting](#stale-item-highlighting) — rather than a second
definition of old.

`↑`/`↓` (or `j`/`k`) move between the grid rows, stepping over the headings and
the tallies. `Enter` filters the table to that row and closes the overlay:
`assignee:"Avery Chen" iteration:"Sprint 1"` for a person, and the iteration
alone for the Total row. The table's own finished rule still applies to what
comes back, so the `Finished hidden ×` chip may say the summary counted more
than the table is listing; its `×` puts them back.

`←`/`→` (or `h`/`l`) step to the previous or next iteration once the
classification trees have been fetched, stopping at either end rather than
wrapping, and skipping the project root — somewhere to file work, not a sprint.

Which iteration the overlay opens on is the sprint the project is in: the
deepest iteration whose start and finish dates contain today. A project whose
sprints carry no dates has no current iteration at all, so the overlay falls
back to the one the selected work item is planned into. With neither — nothing
scheduled and no row selected — it says so rather than painting an empty grid.

## Tabs

A one-row bar across the top names the seven screens:

    1 Work items   2 Repos   3 Pull requests   4 Pipelines   5 AKS   6 ACR   7 Key Vault

`1`–`7` switch between them from anywhere a digit is not being typed — an
overlay comes down on the way out — and clicking a tab does the same. The names
shorten, and then go altogether leaving the digits, as the terminal narrows, so
every tab stays on the bar and stays clickable at any width. Each
screen keeps its own query, cursor and scroll while another is showing, and a
tab wears a badge after its name when it has something waiting.

The first four read the database the sync worker fills; the last three read
nothing of the sort. AKS reads clusters through `kubectl`, and ACR and Key Vault
read one Azure **subscription** through Resource Manager — a different service
from Azure DevOps, reached with a different login. See
[Azure Resource Manager](#azure-resource-manager) for what that costs and what
it buys.

All seven are drawn by one pane system: the same two panes, the same three
arrangements as the terminal narrows, and the same draggable seam between them,
described under [Controls](#controls). The split, which
pane is showing below 70 columns, and the focus are the shell's rather than any
screen's, so switching tabs keeps the layout you were working in.

`[` and `]` walk back and forward through everywhere the run has been,
switching tabs as they go: a work item, then its repository, then back. A
reference in a details pane — the family tree's rows today — is underlined and
follows on `Enter` or a click; one this database does not hold says so rather
than opening an empty screen. The walk is remembered in the session file, and a
file written before the tabs becomes the first cross-tab history.

## Repos

Tab `2` lists the project's repositories: `Name · Default branch · PRs ·
Pipelines · Local`. The counts are the active pull requests and the pipelines
that build each one; a repository the project has disabled stays on the table,
faded, so a link naming it still resolves. The Local column says what is on
this machine — `main ✓` clean, `feat/x *` dirty, `main ↑2 ↓1` ahead and behind,
`—` not cloned.

The details pane carries the name and project, the default branch and size, the
local copy — its path, its status, and when the workspace was last read — and
what is open against the repository: its active
pull requests and the pipelines that build it, each a jump: `Tab` moves the
focus to the pane, `j`/`k` walk the references, `Enter` follows the one they are
on, and a click does both at once. `[` comes back, here as everywhere. `[Clone]`, or
`[Fetch]` and `[Pull]` where there is a clone, run what they say on a click.
`o` opens the repository's page, and so does clicking its name.

Nothing on the pane prints a URL: they are long, they wrap, and they are wanted
on the clipboard rather than read. The URLs section is a row of chips instead —
` Copy web `, ` Copy HTTPS `, ` Copy SSH ` — and the path of the clone copies
itself on a click, over just the text it covers. `y` still copies the ssh URL,
and the status line names what went to the clipboard: a url, or a path.

Its grammar: `name:`, `branch:`, `local:` (`cloned`, `dirty`, `ahead`,
`behind`, `missing`) and `disabled:`.

### The workspace

Clones are looked for, and made, in one directory: `--workspace PATH`, else
`TICKET_TUI_WORKSPACE`, else `~/Development`. The `i` overlay says which. While
the tab is showing, that directory is read on arrival and every 60 seconds
after: its immediate subdirectories that are git repositories are matched to the
project by their `origin` remote — both
`https://…@dev.azure.com/org/project/_git/name` and
`git@ssh.dev.azure.com:v3/org/project/name` read as the same repository — and
each match is measured with `git status`. A directory no remote claimed is then
offered to the repository of the same name, because a project whose
repositories are mirrored somewhere else is still the code you have here; such
a clone's details say `origin … — matched by name`, since a fetch in it goes
wherever that points. A workspace that is not there is not an error: nothing is
cloned, and the details pane says where it looked. The read never fetches, so
what the column calls behind is what your last fetch knew.

It runs on a thread of its own, so a clone that takes a minute holds up neither
the pull nor an edit.

| Key | Action |
|---|---|
| `C` | Clone the selected repository into the workspace |
| `G` | `git fetch --prune` in the clone |
| `P` | `git pull --ff-only` in the clone |

While one runs, the row reads `⠮ cloning…`, `fetching…` or `pulling…` where its
status goes, with the same spinner every other wait uses; the status is re-read
when it finishes, and a notification says what happened — `Cloned ticket-tui`,
or git's own last line when it failed.

`C` clones over https, signing the request with the same login the sync uses
(`az login` or `AZURE_DEVOPS_EXT_PAT`), so it works before any SSH key is
registered with Azure DevOps; `TICKET_TUI_CLONE_PROTOCOL=ssh` asks for the ssh
URL instead. Fetches and pulls of a clone whose `origin` is an Azure DevOps
https remote are signed the same way. Neither git nor ssh is allowed to ask a
question — the terminal is the TUI's — so a missing login or key fails at once
with the reason, and a transfer that stalls for thirty seconds ends itself.
Each is refused, with a word about why, when it
cannot be what you meant: cloning what is already here, fetching or pulling what
is not, pulling a tree with uncommitted changes, or asking for a second command
while one is still running. A pull that cannot fast-forward is refused by git
itself and the refusal is shown — this is not a git client, and a divergence
wants one.

## Pull requests

Tab `3` is the review queue: `ID · Title · Repo · Source → Target · Author ·
Votes · Build · Age`. A draft says `[draft]` after its title, the Votes column
is a run of reviewer glyphs — `✓✓·` is two approved and one not voted, `✗` is
red — and the Build column carries the branch policy's build, or `⚠ conflicts`
in red when the merge is blocked, which is the thing to know first. Closed pull
requests are left off the table behind the same chip finished work items use.

The details pane holds the title and status, the author and both branches, the
description as text, the Reviewers with each vote and whether it is required, a
Related section, the Discussion, the Completion settings and the buttons
`[Approve] [Suggest] [Wait] [Reject] [Complete] [Abandon]`. Related follows the
repository, one line per work item the pull request closes — named as the work
items tab has them when the database holds the row — and the run that gates it,
each a click away and `[` back again.

`C` opens a small form — merge strategy (squash by default, merge commit, or
rebase), delete the source branch, complete the linked work items — and `Enter`
merges it. It is refused before the request goes out, naming what is wrong and
suggesting `o`, when the merge has conflicts or the build policy is failing.
`X` asks `Abandon !123?` and a second `X` abandons it; reactivating one is a
job for the browser. `t` toggles auto-complete, taking the same form the first
time it is turned on, since that is what auto-complete will do when it fires.
`n` opens a one-line prompt and posts it as a thread of its own; the Discussion
section lists the first comment of each thread — author, age, status and the
text — and replies and line comments are `o`. All four are non-optimistic: the
row changes when Azure DevOps answers.

`a` approves, `A` approves with suggestions, `w` waits for the author and `x`
rejects; `u` puts the last vote back. The glyph changes at once and a refusal
reverts it and says why. Voting on a pull request you were not asked to review
adds you, which is what Azure DevOps does. Your own id — which a vote is
written under, and which the work-item endpoints never report — is read once
from `_apis/connectionData` and kept with the sync settings.

Its grammar: `repo:`, `author:` and `reviewer:` (both take `@me`), `vote:`
(`approved`, `suggestions`, `waiting`, `rejected`, `none`), `status:`,
`target:`, `source:`, `draft:` and `build:`. Four built-in views open on the
questions worth asking — **To review** (`reviewer:@me vote:none status:active`),
**Mine**, **Active** and **Recently closed** — and the tab badge is the To
review count.

## Pipelines

Tab `4` lists the project's build definitions: `Pipeline · Folder · Last run ·
Branch · Age`, with the Last run cell carrying the run's glyph and build
number — `◐ 20260829.4 · 3m 12s` for one that is going, its elapsed time
recomputed every frame, `✓ 20260829.3` for one that finished. `Enter` opens a
pipeline's runs — `Run · Result · Branch · Reason · By · Duration · Age`, newest
first — and `Backspace` or `h` goes back up. The details pane heads the run
under the cursor with its build number and result, the pipeline, the branch,
the short commit, who set it going and why, when it was queued, started and
finished, and how long it took. `o` opens the run, or the pipeline, in the
browser; column headers sort; the tab wears a `◐2` badge while anything runs.

`t` on a pipeline opens a branch picker — a filter field over the repository's
branches, opening at once on the default branch and filling in when Azure
DevOps answers, cached for ten minutes — and `Enter` starts it there. The run
that comes back is inserted at the top of that pipeline's runs, selected,
focused and watched, so its timeline and log start straight away. `x` on a run
that is going asks `Cancel 20260829.4?` and a second `x` stops it; `R` on one
that has stopped retries the jobs that failed, in place. A refusal is a toast
and nothing changes.

A run's details carry a Related section: the repository it built, the pull
request it was raised for when there is one, and the work items it says it
carried. Each is underlined and follows on a click — to the Repos tab, the Pull
requests tab, or the work items table filtered to `id:613 id:614` — and `[`
comes back the way it does anywhere else.

`A` opens the approvals the project is waiting on — pipeline, run, stage, what
the approver is asked to check, and how long it has waited. `a` approves and
`x` rejects, each through a prompt for a word about why, which may be left
empty. The watcher reads them once a minute and again the moment the overlay
opens, and the tab badge adds `◇1` beside the `◐2` of anything running.

`W` on a run — or on a pipeline, meaning the run it is having now — follows it:
the row wears a `◉` in its gutter, the watcher keeps polling it whatever tab is
showing, and when it stops the footer says so for eight seconds, `✓ Build
20260829.4 succeeded · 4m 12s` or `✗ Build 20260829.4 failed`. Clicking the
marker does the same. A run that has already finished is refused rather than
announced. Watches live for the session only.

Each level has its own search box and its own grammar. Pipelines filter on
`name:`, `folder:`, `repo:` and `result:` — the result of the run they last
had, so `result:failed` is every pipeline that is currently red. Runs filter on
`pipeline:`, `branch:`, `result:`, `status:`, `reason:` and `by:`, where
`by:@me` is whoever the last sync signed in as. Going down into a pipeline's
runs and back up again puts each list back the way it was left.

A second worker — the pipeline watcher — keeps the list live. It runs on its
own thread with its own client, so a poll never queues behind the 60-second
pull and an edit never queues behind a poll, and it writes nothing to the
database: what it learns is merged into what the tab is showing, and the next
pull is what persists it. While the tab is showing it reads the project's live
runs every 15 seconds; while it is hidden and nothing is being followed it
costs nothing at all. Every cadence doubles, up to a minute, when Azure DevOps
reports a thin rate-limit budget or turns a request away, and goes back to 15
seconds on the next clean response. The `i` overlay says what it is doing.

Under the run's header the details pane draws its timeline: the stages, the
jobs in them and the tasks in those, as a tree with the same connectors the
work-item family tree uses. Each node carries its glyph, its name, `✗ 2` in red
where it reported errors, `42%` where a running task says how far it has got,
and its duration on the right — recomputed each frame while it runs, `—` for
one that has not started. `Tab` moves the focus to the tree, `j`/`k` and a
click move between nodes, and while the run on screen is going the watcher
reads its timeline every five seconds; a finished run's is read once and kept.

Under the timeline is the log of the node the tree cursor is on — or, with
nobody chosen, of the deepest task still running, which moves on as tasks
finish. `l` gives the log the whole pane and gives it back. The title reads
`Log · Build and test · 1,204 lines · following`. The ISO timestamp every line
carries is dimmed rather than dropped, because a slow step is easiest to spot
by its clock, and the `##[…]` markers are painted rather than printed:
`##[section]` bold in the accent colour, `##[group]` and `##[endgroup]` bold
with a `▸`, `##[warning]` yellow, `##[error]` red, `##[debug]` muted,
`##[command]` accent.

While the node is being written the watcher reads the log every two seconds,
sending the number of lines already held so each poll fetches only what is new;
two empty polls in a row and it drops to five seconds until there is something
to read again. A finished node's log is read once, whole. Follow mode keeps the
tail in view; scrolling up with `k`, `PgUp`, the wheel or the scrollbar thumb
leaves it and the title says `scrolled`, and `End` follows again. The pane is a
selectable surface, so dragging across it copies lines. Twenty thousand lines
are kept per log, with a line at the top saying how many earlier ones went.

## AKS

Tab `5` lists the pods of every cluster `config.toml` names: `Pod · Cluster ·
Namespace · Ready · Status · Restarts · Age`, with Node and Repository off the
table by default and one press of `c` away. A `[[clusters]]` table is a name —
what the tab calls it — the kubeconfig context `kubectl` reaches it by, and the
namespaces to read; a cluster that lists none is read `--all-namespaces` in one
call. The file is re-read whenever it changes, so a cluster added while the TUI
is running is read at once, and one taken out takes its pods with it.

    [[clusters]]
    name = "qa"
    context = "aks-qa"
    namespaces = ["orders", "billing"]

Nothing here touches SQLite. A pod is read live, the way local git state and
live runs are: what a cluster holds is not the project's business, and a read is
cheap. `src/aks.rs` is a third worker on a thread of its own — `PodWatcher`,
with its own `kubectl` processes and its own channel — so a slow cluster queues
behind neither a pull nor an edit. While the tab is showing, each cluster is
read every 15 seconds on a `Cadence` of its own, the same one the pipeline
watcher stretches to a minute when a cluster will not answer and puts back on
the next clean read; while the tab is hidden nothing is read at all. One
`kubectl logs -f` child runs at a time — whichever pod the details pane is on —
and it is killed when the pane leaves it and when the run ends, so a quit leaves
no `kubectl` behind. Every one-shot call carries `--request-timeout=10s`; the
follow is the one call without a bound, because a stream is meant to last.

Each `(cluster, namespace)` is a read of its own, and the pods it answers with
replace the ones held for that pair and nothing else — so a cluster that cannot
be reached blanks no other, and the cursor stays on the pod it was on. A read
that fails leaves one line under the table's status and in the details pane —
`qa/orders: Unable to connect to the server` — replaced by the next read of the
same pair and dropped by one that succeeds. A namespace the server refused,
`Error from server (Forbidden)`, does not stop the cluster's other namespaces;
anything else does, since a server that could not be reached will not answer the
next call either. The tab wears a `✗N` badge counting the pods somebody has to
look at.

Its grammar: `cluster:`, `ns:`, `status:`, `owner:`, `node:`, `app:` — the `app`
and `app.kubernetes.io/name` labels — and `repo:`; anything left over matches
the name, the namespace, the owner or the repository. Column headers sort, and a
list re-read every fifteen seconds orders rows a column cannot tell apart by
where they live, so nothing shuffles under the cursor.

The details pane heads the pod with its glyph and status, then the cluster and
namespace, Owner, Node, IP, Created, Ready and Restarts, the buttons
`[Logs] [Describe] [Shell] [Restart]`, the repository the image or app label
names when one is on file, and the containers — name, image, whether each is
ready, its restarts and its state, with a `›` on the one the log is following.

Under that is a text pane showing one of two things. `L` is the pod's log,
tailed with `kubectl logs -f --timestamps --tail=500`, its title reading `Log ·
orders-api-7d9f5b-abc12 · api · 1,204 lines · following`. Follow mode keeps the
tail in view; scrolling up leaves it and the title says `scrolled`, `End`
follows again, and a stream that stopped — the pod went, or `kubectl` refused —
says `ended` rather than spinning forever. `P` turns on `-p`, the run before the
last restart, which is where a crash loop says why; `C` moves to the pod's next
container and round to the first again. `D` puts what `kubectl describe pod`
said in the log's place, and `L` brings the log back. `l` gives the text pane
the whole details pane and gives it back. Twenty thousand lines are kept, as
everywhere else.

The verbs: `x` deletes the pod and lets its controller put it back, asking once
more first — the confirmation names the owner, a second `x` sends it, `Esc` does
not — and it is refused outright on a pod with no controller, which deleting
would take away for good rather than restart. `s` runs `kubectl exec -it … --
sh` in this terminal, suspending the TUI and repainting it on the way out. `g`
goes to the Repos tab, on the repository the pod's image or app label names —
`repo_candidates` reads `myacr.azurecr.io/team/orders-api:1.2.3` as
`orders-api` — and `[` comes back the way it does anywhere else; a pod nothing
on file matches says what it offered. `y` copies `namespace/pod`. `r` re-reads
the clusters rather than pulling from Azure DevOps: nothing on this tab comes
from there. `o` has no page to open and says so.

## Azure Resource Manager

Tabs `6` and `7` read a subscription, not the project, so they get a second
client and a second worker thread. `src/arm.rs` is the client; `src/arm_watch.rs`
is the thread. Neither touches SQLite: what a subscription holds is not the
project's business and it changes without anyone editing a work item, so it is
read live the way a cluster's pods are and the next read replaces it.

The subscriptions are `--subscription <id>`, repeatable, else
`TICKET_TUI_SUBSCRIPTION` naming one, else `subscriptions` under `[azure]` in
`config.toml` naming as many as a shop has, else whatever `az account show`
says the Azure CLI is set to — and that last step happens on the worker thread
rather than on the startup path, so no run pays for a shell-out it may never
need. One Resource Graph query covers all of them at once, so dev, qa and prod
arrive in one listing. With none of the four, both tabs draw empty and say so:

    no Azure subscription: pass --subscription, name one under [azure] in config.toml, set TICKET_TUI_SUBSCRIPTION, or run `az account set`

`registries` and `vaults` under `[azure]` narrow what comes back: an allowlist
keeps only the resources it names, in the order it names them, matched without
regard to case, and a name the subscriptions do not hold is simply absent
rather than an error — `ticket-tui acr list` and `ticket-tui vaults list` show
what matched. An empty allowlist is no opinion at all and keeps everything,
which is what fifty subscriptions' worth of registries need narrowing from.
The status bar names the subscriptions being read, joined with `, `.

`az login` is what all of this borrows. A tenant other than the login's default
wants `az login --tenant <tenant>` before any of it will answer.

Three token audiences, all borrowed from the Azure CLI's login. `management.azure.com`
signs the Resource Graph query that lists the subscription's registries and
vaults. `vault.azure.net` signs every Key Vault data-plane call. A registry's own
data plane wants a third: the ARM token is exchanged at
`https://<login server>/oauth2/exchange` for a refresh token and then for an
access token scoped to what is being read — `registry:catalog:*` for a catalog,
`repository:<name>:metadata_read` for one repository's tags and manifests. There
is no personal-access-token path anywhere here: a PAT is an Azure DevOps
credential and ARM has never accepted one, so a PAT-only run has a working
work-items tab and two empty ones, and every refusal says `az login`.

The reads are edge-triggered. The Resource Graph inventory — one query for both
resource types rather than one list call per provider — runs every 60 seconds
while either ARM tab is showing, and not at all while neither is. Everything
under it is read once per focus: drilling into a registry reads its catalog and
then one attributes call per repository, moving the details cursor onto a tag
reads that manifest, opening a vault reads its secrets, keys and certificates in
one listing. Drilling in and back out again costs nothing: the screen holds what
it read and stops asking. `r` clears the worker's memory of what it has
answered and re-reads the inventory at once — but a registry or vault already
open and already read is not asked for again, because the screen still holds it
and so reports no focus. Whether it should is #733's to settle. A `429` or `503`
honours `Retry-After` in either spelling — seconds or an IMF-fixdate — clamped to
an hour, the same shape the pipeline watcher uses.

A secret's value is the exception to all of it. It is read when somebody presses
the key that asks for one and not a moment besides, answered once, and never held
by the worker. In the app it lives in `Secret`, a newtype whose
`Debug` and `Display` both print `[redacted]` so it cannot reach a log line, an
error or a panic message by accident; `Secret::expose` is the one way to read it
and it is meant to be conspicuous at the call site. There are exactly two callers:
the line the details pane draws, and `secrets show --value`.

## ACR

Tab `6` lists the container registries the subscription holds: `Registry ·
Resource group · SKU · Location`, with Login server off the table by default and
one press of `c` away. `Enter` goes down into one registry's catalog —
`Repository · Tags · Updated` — and `Backspace` or `h` comes back up. A count
that has not landed yet is a muted `—` rather than a nought: a catalog listing is
names and nothing else, and the counts arrive one attributes call at a time, with
the table's bottom border reading `12 repositories · 5 of 12 read` while they do.

The details pane shows what the cursor is on. A registry: its login server under
its name, then Group, Location, SKU and Repos, and the portal link, which is a
click target. A repository: its counts and stamp, the chips
`[Copy pull] [Copy digest] [Open]`, then its tags newest first — name, short
digest, age, and the size once the manifest has been read — and under them the
Manifest section for the tag the pane's own cursor is on: Created, Platform
(`linux/amd64`), Size. `Tab` moves the focus to the pane so `j`/`k` walk the
tags; a click picks one directly. Whatever refused last is a `Problem` section at
the bottom of either pane, in the error colour, cleared by the next read that
works.

Its two grammars, one per level: `name:`/`registry:`, `rg:`, `sku:`, `location:`
on registries, with the facet bar offering the last three; `name:`/`repo:` alone
inside one, where the bar stays empty because the one field there is is the one
already in the search box. Leftover text matches the registry's name or login
server, or the repository's name.

The verbs: `y` copies the pull reference — `atlas.azurecr.io/team/api:1.2.3`, the
thing `docker pull` wants, not a number. `D` copies the tag's full digest. `o`
opens the registry in the Azure portal from either level — a repository and a
tag have no page of their own. `r` re-reads the subscription rather than pulling
from Azure DevOps: nothing on this tab comes from there.

Deliberately out: untag and delete. This tab and `ticket-tui acr` are read-only,
and the portal link is the way to anything destructive — a mistake here is not
undoable and not worth a keystroke.

## Key Vault

Tab `7` lists the key vaults the subscription holds: `Vault · Resource group ·
Location · SKU`. `Enter` goes down into one, and the level under it is **one
table over all three kinds** — `Kind · Name · Enabled · Updated · Expires` —
because that is how a person looks for one: by name, not by which listing it came
out of. A disabled item is faded whole; an expiry reads amber inside the month
before it and red once the clock has passed it. The table opens soonest-expiry
first, and an item that never expires sorts last that way round.

The details pane heads the item with its kind, then Enabled, Content type,
Created, Updated, Expires (coloured the way the table's cell is) and Recovery,
then the chips — `[Reveal]` only on a secret, since only a secret has a value,
then `[Copy name] [Open]`. The vault pane above it is the same shape as the
registry pane: name, URI, Group, Location, SKU, Items, and the portal link.

Its grammars: `name:`/`vault:`, `rg:`, `location:` on vaults; `name:`/`item:`,
`kind:` (`secret`, `key`, `cert`), `enabled:` (`yes`/`no` and `true`/`false`,
both, so neither spelling quietly matches nothing) and `expires:` inside one. The
facet bar offers `kind:` and `enabled:`. `expires:` is a date, and the one in this
app usually asked about the future, so it takes the `+` form: `expires:<+30d` is
everything falling due before the instant thirty days from now, which is the
question this tab exists for. The tab wears a `◇N` badge counting the
certificates already lapsed or within thirty days of it, across every vault whose
items have been read.

`R` reveals a secret's value. The worker reads it then and there, hands it back
once, and keeps nothing; the pane shows it in the accent colour with `clears in
43s` beside it, and it goes when the minute runs out, when the cursor moves, when
the level changes, when `r` refreshes, and when the tab is left — `close_overlay`
is what leaving runs, which is why the value goes with it. `Y` copies it, and
only while it is showing. `y` copies the name. `o` opens the vault in the portal
from either level, an item having no page of its own. `r` re-reads the
subscription.

The value is nowhere else. It is not in the session file, not in the agent
context — which carries `revealed: true` and no field for a value — not in a
notification, and not in a `Debug` line: `AppAction::CopySecret` prints
`[redacted]` like everything else holding a `Secret`. Two tests hold that from
both ends.

Deliberately out: create, set and delete; access policies and IAM; and a secret's
version history, which would be one more data-plane read per item for a question
nobody has asked yet. The portal link is the way to all of it.

## Controls

Everything above the tabs line is global; the work-item keys under it only do
anything on tab `1`.

| Input | Action |
|---|---|
| `1`–`7` | Switch to Work items, Repos, Pull requests, Pipelines, AKS, ACR, or Key Vault |
| `↑`/`↓`, `j`/`k` | Move the ticket selection, family row, or focused details pane |
| `Page Up`/`Page Down` | Move ten tickets or one family page |
| `Home`/`End` | Select the first/last ticket, family row, or details line |
| `/` | Focus live fuzzy search |
| `←`/`→`, `Home`/`End` | Move the search cursor while searching |
| `↑`/`↓` | Move the ticket selection while searching |
| `Backspace`/`Delete`, `Ctrl-W` | Edit the query while searching |
| `Ctrl-U` | Clear the query while searching |
| `Ctrl-P`/`Ctrl-N` | Recall previous/next completed searches |
| Paste | Insert sanitized pasted text into the search query |
| `Esc` | Leave search, clear the query, or clear a multi-selection |
| `s` | Open the sort menu; use arrows and `Enter` to apply |
| `f` | Focus the filter bar; `h`/`l` change field, `j`/`k` values, `Space` toggles |
| `id:613 id:614` | List exactly those work items — exact, ORed like any field, and a chip like any other. It is what a jump from another tab writes |
| `F` | Open the full filter overlay for extra fields |
| `c` | Show or hide (`Space`), reorder (`J`/`K`), and resize (`<`/`>`) columns, Progress among them |
| `p` / `:` | Open the command palette |
| `v` | Open views: five built-in ones and your own; `n` saves, `Enter` loads, `d` deletes |
| `V` | Save the current query, sort and columns as a view |
| `e` | Open the Actions menu of field editors; `Enter` opens the one chosen |
| `S` | Change the selected work item's state, or every checked one; `Enter` applies, `Esc` cancels |
| `a` | Change who the selected work item is assigned to, or every checked one; type to filter, `Enter` assigns |
| Palette → Toggle row density | Compact or comfortable table rows; no key of its own |
| Palette → Toggle search order | Relevance-first or strict field ordering during search |
| `e` → Title/Priority/Tags/Iteration/Area | Edit the title, priority, tags, iteration, or area; also `Edit title`, `Edit priority`, `Edit tags`, `Change iteration`, `Change area`, and `Change assignee` in the palette |
| `e` → Description | Edit the description in `$VISUAL`/`$EDITOR`/`vi` as Markdown; also `Edit description` in the palette |
| `e` → Add comment | Leave a one-line comment on the selected work item; also `Add comment` in the palette |
| `n` | Open the new work item form; `↑`/`↓` or `Tab` moves between fields, `Enter` opens a field's picker, `Ctrl-S` creates, `Esc` keeps the draft |
| `N` | Open the same form as a child of the selected work item: the type it breaks down into, the parent fixed, the area and iteration inherited; also `New child` in the Actions menu and the palette |
| `e` → Delete work item… | Send the selected work item, or every checked one, to the Azure DevOps recycle bin; `d` confirms, `Esc` cancels. No key of its own; also `Delete work item…` in the palette |
| `u` | Undo the last edit, putting the value back; a bulk change goes back under one press |
| `m` | Bookmark or unbookmark the selected ticket |
| `Space` | Toggle ticket multi-select; two or more make `S`, `a`, Iteration, and Delete act on all of them |
| `y` | Copy selected (or current) ticket IDs |
| `[` / `]` | Jump to the previous or next recently viewed ticket |
| `Tab` | Toggle focus between tickets and details |
| `d` | Toggle the details pane when the terminal is under 70 columns; on every tab |
| `Enter` | Select the family cursor ticket; with details focused, edit the field under the pointer, or open the work item |
| `o` | Open the selected ticket in the system browser |
| `r` | Sync from Azure DevOps now, without waiting for the timer |
| `L` / `D` (AKS) | Tail the selected pod's log, or put `kubectl describe` in its place |
| `P` / `C` (AKS) | Follow the run before the last restart, or the pod's next container |
| `l` / `End` (AKS) | Give the text pane the whole details pane; follow the tail again |
| `x` (AKS) | Delete the pod so its controller puts it back; a second `x` confirms |
| `s` (AKS) | `kubectl exec -it … -- sh` in this terminal |
| `g` (AKS) | Go to the repository the pod's image or app label names |
| `y` (AKS) | Copy `namespace/pod` |
| `Enter` (ACR, Key Vault) | Into the registry's repositories, or the vault's items |
| `Backspace` / `h` (ACR, Key Vault) | Back up to the registries or the vaults |
| `Tab` (ACR) | Focus the details pane, where `j`/`k` walk the tags and the manifest follows |
| `y` (ACR) | Copy the pull reference — `atlas.azurecr.io/team/api:1.2.3` |
| `D` (ACR) | Copy the digest of the tag the details pane is on |
| `R` (Key Vault) | Reveal the selected secret's value: on this screen, for one minute, nowhere else |
| `Y` (Key Vault) | Copy the revealed value, and only while it is showing |
| `y` (Key Vault) | Copy the item's name |
| `o` (ACR, Key Vault) | Open the registry or the vault in the Azure portal — the resource is what the portal has a page for, whichever level you are on |
| `r` (AKS, ACR, Key Vault) | Re-read that tab's own source rather than pulling from Azure DevOps |
| `i` | Show database path, row counts, hidden finished rows, and sync freshness |
| `?` | Show the in-app help; use arrows or page keys to scroll it |
| `q`, `Ctrl-C` | Quit |

The help overlay's key sections and the palette's key labels are generated from
the same command table these keys are bound in, so a binding reads the same way
everywhere. Each command carries the scope it belongs to — global, or the tabs
that answer it — which is what groups them under headings in the help and keeps
another tab's keys out of the palette. A scope names a tab only when that tab's
screen has an arm for the command, so the palette never offers an entry that
would do nothing; two tests hold that in both directions, and a command two
tabs share is listed under both help headings because it does a different thing
on each. `Open` is worded for what it opens — a ticket, a repository, a pull
request, a run, or the Azure portal — and so are the three verbs that mean
something different on a tab with no Azure DevOps behind it: `Sync` reads
`Refresh pods`, `Refresh registries` or `Refresh vaults`, and `Copy ID` reads
`Copy pull reference` on ACR and `Copy name` on Key Vault. `?`, `p`/`:`, `c` and `i` open on every tab: the
palette lists the commands of the tab that is showing and runs its choice
there, and the columns editor edits that tab's columns.

The details pane is one scrolling document rather than a pinned heading over a
scrolling body. Its heading — the title, then a badge row reading
`#600 · [Issue] · ✓ Done · P1 · Jacob Ragsdale` in the colours the table's own
cells use, then the family breadcrumb, tags, project and revision, child
progress, and the work-item URL — scrolls away with everything under it, in
this order: the family tree, Related, Planning, Description, History, and
Comments. Every field below the badge row reads as a muted label in a column of
its own with the value beside it, so the values line up down the pane; a label
too wide for that column — `Default branch` on the Repos pane — pushes its
value along instead, behind one space rather than into it. Each
section is headed by a rule (`── Planning ────`) rather than by another bold
line, which is what tells a heading from a field name without colour. The scrollbar
therefore measures the whole pane and its thumb reaches the bottom, `End` lands
on the last comment, and a field value stays clickable wherever the scroll has
carried it. Moving the family cursor scrolls the tree back into view when the
heading has pushed it below the fold.

Related lists what the work item was worked on with, from the `ArtifactLink`
relations Azure DevOps stores: its pull requests, the commits that named it, and
the builds it went out in. A pull request or build this database holds is named
and follows on `Enter` or a click — `!42  Split the files · completed`,
`Build 20260829.4 ✓` — and one it does not hold says so rather than pretending,
because there is nothing here to open. A commit reads as its short sha and the
repository it is in: nothing in this app shows a diff, and `o` still opens the
work item in the browser. The section is left out entirely for a work item with
no such links, and the agent context lists the same links on the selected work
item, each saying whether the database holds it.

A work item with children reports how far they have got. The heading reads
`Children   3/7 done` with a six-cell bar beside it, drawn in fractional
blocks so `3/7` and `4/7` are different pictures, every family-tree row that
is itself a parent trails its own ` 3/7`, and the optional Progress table
column shows the same ratio. All three count direct children only — an Epic
reads over its Features rather than over every Task underneath them — and both
Completed and Removed states count as done, so a cut work item stops holding
its parent back. A work item nobody broke down shows nothing at all in any of
the three places, never `0/0`. The bar is drawn from filled and hollow glyphs
rather than two colours, so it reads the same under `NO_COLOR`.

One braille spinner (`⠋⠛⠻⠺⠾⠼⠮⠯⠇⠏`, ten frames off the wall clock at 100 ms)
stands for every wait: the search prompt while a search is still running, the
status bar while a pull is in flight, the details pane while it is fetching
comments and history, a Repos row while git is working, and a log that is
following a run still going. Nothing turns while nothing is running: the loop
wakes ten times a second and repaints only while one of those is true, and an
idle app draws no frames at all. When an edit lands, or is taken back, the
row's gutter goes accent and reversed for a fifth of a second — long enough to
catch a row several away from the cursor, and gone on the next frame.

An overlay is a layer in front of the screen, not more of it: while one is
open every cell behind it gives up its colour and its weight, and gets them
back when it closes — unless the theme says otherwise, which is what `mono`
says. The help and the command palette take about 70% of the terminal each
way, clamped to what they hold and never smaller than they were before there
was room to grow; the pickers keep the anchored geometry that hangs them off
the field they edit. A modal's close button is a muted `×` in the top-right
corner of its frame, its title is bold, and its buttons are chips: ` Cancel `
on the surface ground, and the action the overlay exists for — ` Save `,
` Delete `, ` Create ` — filled with the accent, or reversed where the palette
has no colour to fill with. A list overlay marks the row under the cursor with
an accent `›` on the surface ground and puts the key that runs it against
the right-hand edge in the muted colour.

The bottom row is a two-segment status bar. On the left, what the keys do on
this tab and in this mode, cut where one hint ends rather than in the middle of
a key when the row is narrow — the `?` overlay carries the rest. On the right,
how the sync is going: `⠮ Syncing…` while a pull is in flight, `● Synced 2m`
after one, `! Sync failed`, `◌ Stale` when another writer has changed the
database, `◌ Sync paused 2m` while Azure DevOps is throttling, and `⊘ Offline`
for a run with no project to pull from — in the success, error, warning and
muted colours, with the project's name beside it when the row is wide enough.
A notification takes the left segment over with a leading `✓` or `✗` and
expires as it always did; it never covers the sync, which is on screen
whatever else is being said. The tickets pane's title says nothing about the
sync any more.

The search is one row under the tab bar, not a box three rows tall: a `/`
prompt in the muted colour with the placeholder beside it, a bold `›` and the
surface ground while it has the keyboard, a braille spinner in the prompt cell
while a search is still running, and a `[×]` at the right end once there is a
query to clear. Where a tab is searching inside something — a saved pull
request view, the runs of one pipeline — that name reads muted at the right end
of the row. `Actions` and `?` are chips at the right end of the tab bar rather
than titles on the search box, because both open over every tab; the shell
answers them, as it does a click on a tab. On the bar itself the tab showing
reads in the accent on the surface ground, bold — reversed under `NO_COLOR`,
where there is no ground — the others are muted, and a badge saying what is
waiting on a tab reads in the warning colour wherever that tab sits. The facet pills below are filled
chips: the one the cursor is on reverses, one carrying a filter reads in the
accent, and the rest are muted — distinctions that survive `NO_COLOR`, where
the first two become reverse and bold.

Every pane wears the same frame: the theme's corners — rounded in `terminal`,
`terminal-light` and `custom`, plain under `NO_COLOR` — the accent and a bold
name while it has the focus, and the neutral border colour when it has not.
Two panes side by side, or stacked, share one border rather than leaving a gap
between them: that column is the seam, drawn as `┬` and `┴` where it meets the
frame, painted in the neutral colour because it belongs to neither pane, and it
is the divider you drag. A list pane says what it is on its top border and what
it holds on the bottom one — `╰ 106/106 · Changed ↑ ─` — except where it is stacked
over another pane, which paints that row last: there the count joins the name
above. A rule under the table header separates the column names from the rows,
and the details pane is padded a column in from each side, with the scrollbar
in a column of its own at the edge.

Every tab is arranged by the same pane system, so all four look and behave
alike. Each shows a list and the details of whatever its cursor is on, and the
width decides how: side by side from 110 columns, stacked between 70 and 110,
and one at a time below 70. The one-pane layout wears the two chips that swap
them on its top border where the pane's name would go — ` [Repos] [Repository] `,
` [Pipelines] [Run] ` — the pane on screen in the accent and the other waiting
to be clicked; `d` switches between them from the keyboard on every tab, and
`Tab` brings the pane it moves the focus to on screen with it. The
seam between the panes is draggable on every tab, and the split is the shell's
rather than a screen's, so a tab opens arranged the way the last one was left,
and the session file remembers it. A pane divided again inside itself follows
the same rules: the pipelines log is stacked under its run in a tall pane, sits
beside it in a short wide one, and stands down altogether where there is room
for neither, leaving `l` to bring it up whole.

Mouse input stays captured so the TUI can provide its own pointer controls
without restoring terminal drag-select. Wheel scrolling moves the hovered
table, details pane, help, or overlay by three rows or lines and does not
change keyboard focus or the selected ticket. Left-click activates the
visible control under the pointer on release: search, filter pills, sort
headers, ticket rows, checkboxes, bookmark markers,
underlined IDs and URLs, details-pane field values, tabs, overlay rows, form
rows, and close/action buttons. A field value in the details pane — the title, the state,
the assignee, the priority, the tags, the iteration, or the area — opens its
editor as a dropdown hung under the value that was clicked, left edge on it and
width fitted to the longest entry. A field with too little room below opens its
dropdown above itself, and one with room neither way falls back to the middle of
the screen. Clicking anywhere outside an open dropdown closes it without a
change, and that click reaches nothing underneath. Assignee and priority share a
line and are two separate targets on it. Dragging over visible text
selects it and copies the plain text on release, a field value included. Bracketed paste inserts at
the caret in search, the command palette, the named-view editor, and the title,
tags, and comment prompts.
Scrollbar tracks page by a viewport-minus-one step; thumbs can be
dragged. Dragging the border two panes share resizes them, on every tab and in
both arrangements; the list keeps at least 40 columns and the details pane at
least 30 side by side, and each keeps six rows when stacked. A seam inside a
pane — the pipelines log under its run — drags the same way, and keeps less
either side because it is dividing one pane's worth of room. `Reset pane split`
in the command palette, on any tab, restores the built-in layout.
Right-click, double-click, and horizontal wheel gestures are not
used. Terminals supporting OSC 22 show a browser-style pointer over external
URL targets and over the details-pane values that can be edited, which underline
under the pointer whether or not the terminal has colours.

Search accepts a compact grammar such as `state:active type:bug
assignee:"Avery Chen" priority:1 tag:rust`, plus `project:`, `area:`, and
`iteration:`. Values in the same field are combined with OR; different fields
are combined with AND. `id:` names work items by number — `id:613 id:614` is
those two and nothing else — which is what a jump from another tab writes into
the search box. It is the one field the `F` overlay does not offer a list of
values for: that list would be as long as the database. `is:bookmarked` limits the table to locally bookmarked
tickets. `changed:` and `created:` take a comparison rather than a value, so
`changed:<7d` is what moved in the last seven days and `changed:>14d` what has
gone untouched for longer, over units of minutes, hours, days, and weeks
(`m`, `h`, `d`, `w`), with `<=` and `>=` taking the edge and `updated:` reading
as `changed:`. An ISO date compares against the instant instead, at UTC
midnight, so `created:>2026-08-01` is everything raised since the start of
August. Relative windows are measured as the filter runs, so a view saved as
`changed:<7d` still means the last seven days tomorrow, and the `F` overlay
offers 24h, 7d, 14d, and 30d presets for both fields, where a date has no list
of values to check off.

Four values are written with a leading `@` and stand for something the app
works out as the filter runs rather than when the query is typed:
`assignee:@me` is whoever the last sync signed in as, `assignee:@none` is the
work nobody owns, `iteration:@current` is the sprint whose dates contain today,
and `state:@open` is anything the workflow has not finished with, read by state
category so it holds whatever the process template calls its states. They can
be typed straight into the search box, they are stored and shown as written,
and a sentinel the app cannot fill in — nobody signed in, no sprint scheduled —
matches nothing rather than everything. `TICKET_TUI_ME` sets who `@me` is for
anyone whose profile name differs from the name their work items are assigned
to. Active filters appear as removable chips.

Finished work — Done, Closed, Removed, Cut, and whatever else the process
template puts in the Completed and Removed categories — is left off the table,
so the view you open on is the open backlog. A `Finished hidden ×` chip says so
whenever it applies, and its `×` puts the rows back; `Show finished tickets` and
`Hide finished tickets` in the command palette do the same thing, and the choice
is saved with the rest of the session. A query that names a state of its own
takes over from the rule, so `state:done` lists the Done work whether or not the
toggle is on, and `state:@open` — including the **Stale** built-in view written
with it — means exactly what it says rather than being applied twice. The table
title counts what is on the table over the whole database, so the rows left out
are the difference between the two; `i` says how many they are. The details
pane and the family tree reach a hidden work item as they always did: a parent
or child that is finished is still shown there, and the
[sprint summary](#sprint-summary) counts it.

The command palette copies
IDs, URLs, titles, Markdown links, or summaries, edits the title, priority,
tags, iteration, area, or description, leaves a comment, shows or hides finished
tickets, opens the [sprint summary](#sprint-summary), and exports the
selection as JSON or CSV. Press `i` for database path, row counts, how many
finished rows are hidden, freshness, and the last
sync. A database another process writes reloads automatically; the table title
shows `Stale` until that reload finishes, and `Syncing…`, `Synced 2m ago`, or
`Sync failed` for the pulls from Azure DevOps.

States are coloured by category: New, To Do, and Proposed blue; Active, Doing,
and In Progress yellow; Resolved magenta; Done and Closed green; Removed grey;
a state outside those groups stays plain. Work-item types carry fixed badge
colours — Epic yellow, Feature magenta, Issue, User Story, and Product Backlog
Item blue, Task cyan, Bug and Impediment red, Test Case green — priority 1 is
red, 2 yellow, 3 and 4 blue, and each tag is hashed onto a stable badge colour
so one tag reads the same everywhere. Completed and removed rows are dimmed
wherever they are shown — once the toggle above lists them, and in the family
tree, which never leaves them out — the Area and Iteration table columns show only the last
path segment while details keeps the full path, the State cell and the
family-tree rows carry the same one-character state glyph
(`○ ◐ ● ✓ ✗`) in front of the word, the Pri cell reads `P1`–`P4`, and matched
search characters are underlined in visible results. ID, Title, State, Type,
Pri, Changed, and Assignee are the columns a fresh session shows; Org, Project,
Area, Iteration, Created, Tags, and Progress are there under `c` and stay hidden
until they are switched on, after which the choice is saved with the rest of the
layout. A table that cannot fit the columns it has been given drops the
right-most optional one for as long as the Title would otherwise fall under 24
characters — so a narrow pane loses Assignee, then Changed, rather than the
title, and each comes back as soon as there is room for it again. ID and Title
are pinned and never go. The scrollbar has a column of its own at the right
edge, reserved whether or not the list overflows, so the last cell keeps every
character it was given. A
parent whose children have all finished goes green and bold, in the Progress
column and in the details heading alike. A hovered row is tinted with a
256-colour background rather than repainted, so its coloured cells keep their own
foregrounds; hovered controls reverse instead. Those are the `terminal` theme's
colours; the `custom` theme maps a palette from `config.toml` onto the same
roles — state, type, priority and tag colours included — and `terminal-light`
keeps them readable on a white ground (see [`--theme`](#--theme-ticket_tui_theme-and-configtoml)).
Setting the standard `NO_COLOR` environment variable selects the `mono` theme,
where weight carries the same distinctions: badges keep their brackets,
finished rows dim instead of fading, state glyphs and your own work items go
bold, and a hovered row reverses.

`V` opens the views. Five built-in ones are listed under a **Built-in**
heading, above whatever you have saved: **Mine** (`assignee:@me`),
**Unassigned** (`assignee:@none`), **Doing** (`state:doing`), **Stale**
(`changed:>14d state:@open`, oldest first, leaving out work that is finished),
and **Current sprint** (`iteration:@current`). A built-in sets the query and
the sort and leaves the columns and the row density as you have them; it is
never written to the session file, cannot be deleted, and owns its name, so
saving a view called `Mine` is refused. `n` saves the current query, sort,
columns, and density under a name of your own, `Enter` loads the view under the
cursor, and `d` deletes one you saved.

Changed dates use compact relative labels, and exact UTC timestamps remain
available in details. Press `c` to switch between compact and comfortable row
density. Named views, column layout, bookmarks, the pane split, the stale
threshold, whether finished tickets are listed, and the last query are saved
beside the cache as `*.session.json`.

### Stale-item highlighting

Work that nobody has touched for a fortnight, and that the workflow has not
finished with, is flagged in the Changed column: the age goes warning-coloured,
and bold where `NO_COLOR` leaves no palette to colour it. The details pane says
how long, as `Changed: 2026-08-08 12:00:00 UTC (stale 21d)`.

Nothing dims — dim already means finished — and finished work is never flagged
however long it has sat: nobody is waiting on a work item that is done or
removed. Whether a state counts as finished is read from its Azure DevOps state
category, not from its name, so every process template's spelling of Done,
Closed, Removed, or Cut is understood.

The threshold is exclusive, and it is exactly the `changed:` comparison the
query language already takes, so a flagged row is precisely a row `changed:>14d
state:@open` lists — the built-in **Stale** view. An item last touched exactly
fourteen days ago has not crossed the threshold; one touched a moment before
that has. Change it with `--stale-days`, `TICKET_TUI_STALE_DAYS`, or **Set
stale threshold** in the palette, which remembers what it was given. The
[sprint summary](#sprint-summary)'s stale figure counts by the same rule and
moves with the same threshold.

## Database reference

The default database is `ticket-tui/tickets.sqlite3` under the platform data
directory:

- macOS: `~/Library/Application Support/ticket-tui/tickets.sqlite3`
- Linux: `$XDG_DATA_HOME/ticket-tui/tickets.sqlite3`, normally
  `~/.local/share/ticket-tui/tickets.sqlite3`

The `work_items` table stores these columns:

| Column | Meaning |
|---|---|
| `organization`, `project` | Azure DevOps location |
| `work_item_id`, `revision` | Work-item identity and revision |
| `work_item_type`, `title`, `state`, `reason` | Core work-item fields |
| `assigned_to`, `priority` | Ownership and priority |
| `area_path`, `iteration_path`, `tags` | Planning metadata |
| `description` | Detail content, rendered as plain text |
| `description_html` | The same field as Azure DevOps stores it |
| `created_at`, `changed_at` | UTC RFC 3339 timestamps |
| `web_url` | HTTPS browser URL for the work item |
| `details_rev` | Revision whose comments and history are stored, `0` for none |

The primary key is `(organization, work_item_id)`. Tags use Azure DevOps-style
semicolon separation. The `work_item_relations`, `work_item_artifact_links`,
`work_item_comments`, and `work_item_history` tables hold the graph around each
work item: links to other work items and to the pull requests, commits and
builds it was worked on with, both from every pull, and comments and revision
history for every work item whose `details_rev` says they have been read. A work
item the project stops listing takes all four with it. The `sync_meta`
key/value table describes the sync itself rather than the work items, so a full
pull clears the other tables but leaves it alone. These keys live there, beside
the `classification_nodes_fetched_at` below:

| Key | Meaning |
|---|---|
| `me_display_name` | The signed-in display name that marks your own work items |
| `watermark_changed_at` | The greatest `System.ChangedDate` the last successful pull saw, as an RFC 3339 UTC timestamp |
| `organization`, `project` | Where the stored work items were pulled from |
| `sync_scope` | The extra WIQL condition that pull narrowed the project with, empty for a project pulled whole |

The watermark is where the next incremental pull starts asking; a database
without one is pulled in full and left with one. The organization and project
are what a run resolving a different pair refuses to sync over, and the scope is
what a pull compares its own against: a scope that has moved forces one full
pull. Those three are written by every successful pull and only when they
change, so an idle project's pull still leaves the file untouched.

The `identities` table holds what the assignee picker offers beyond the people
the rows already name: `display_name`, the primary key, and `unique_name`, the
sign-in address a write is addressed to when one is known. It is filled from the
project's teams the first time that picker is opened in a run, and read back at
startup so the next run's picker is complete before any network call. Like
`sync_meta` it describes the project rather than its work items, so a pull
leaves it alone; it is rewritten whole when the teams are read.

The `classification_nodes` table holds what the iteration and area pickers
offer: `kind` (`area` or `iteration`), `path` — the value a work item's field
carries, such as `development\Sprint 1` — `depth`, the level the row is
indented to, `start_date` and `finish_date`, an iteration's schedule as RFC 3339
UTC timestamps or null, and `position`, the order the trees were flattened in,
keyed on `(kind, path)`. `sync_meta` carries `classification_nodes_fetched_at`
beside it, the RFC 3339 UTC time the trees were last read; a cache younger than
an hour is used as it stands rather than fetched again. Like `sync_meta` the
table describes the project's plan rather than its work items, so a pull leaves
it alone; both trees are rewritten whole when they are read, so a deleted sprint
stops being offered.

The `work_item_type_states` table holds what the state picker offers:
`work_item_type`, `name`, `category` (`Proposed`, `InProgress`, `Resolved`,
`Completed`, or `Removed`), and `position`, the order the process template lists
the state in, keyed on `(work_item_type, name)`. Like `sync_meta` it describes
the project's process rather than its work items, so a pull leaves it alone; a
type is rewritten whole when its states are fetched, so a retired state stops
being offered.

The `work_item_types` table holds what the new work item form's Type field
offers: `name`, the primary key, and `position`, the order the process listed
the type in. It is filled the first time a form is opened in a run and read back
at startup, so the next run's picker is complete before any network call, and it
holds only the types worth filing by hand — the disabled and hidden ones are
dropped on the way in. Like `sync_meta` it describes the project's process
rather than its work items, so a pull leaves it alone; it is rewritten whole
when the types are read, so a retired type stops being offered.

The database carries `PRAGMA user_version = 17`. Because Azure DevOps is the
record of truth, there are no migrations: a database at any other version has
its tables dropped and recreated at startup, and a pull runs immediately to
refill it, whatever `--refresh` says. Deleting the file has the same effect. The
sync worker and background reloads instead open the database without touching
its schema and report the version mismatch, ending in `restart ticket-tui`, so a
running instance can never empty a database a newer build owns. After upgrading
the binary, restart any running ticket-tui.

An edited work item is written to Azure DevOps first and stored from the copy
that comes back, so these records only ever hold what the server accepted.
Parent and child links render as an always-expanded family tree in the details
pane. Click a family row, or press `Enter` on the family cursor, to select that
ticket in the table. Fuzzy search covers ID, title, assignee, state, type, area,
iteration, and tags; it intentionally excludes descriptions. Structured
`field:value` tokens are parsed out of the query before fuzzy matching.

The application uses WAL mode and a busy timeout so external SQLite readers can
query the cache while the TUI is running.

## Subcommands

A bare `ticket-tui` opens the TUI. Every subcommand does one thing and exits,
which is what lets an agent — or a script — read and change work items without
a terminal to drive. `--database`, `--org`, `--project`, `--code-project`,
`--workspace` and `--subscription` — the last repeatable — apply to all of them
and may be written either side of the subcommand. Every one of them falls back
to `config.toml`, so a shop that has edited that file writes none of them.

```console
ticket-tui sync [--full]
ticket-tui show <id> [--json]
ticket-tui list [--query '<filter>'] [--json]
ticket-tui edit <id> [--state S] [--assignee A] [--priority N] [--iteration I] [--area A] [--title T] [--tags a,b] [--description-file F]
ticket-tui comment <id> ["text" | -]
ticket-tui create --type Issue --title T [--parent ID] [--iteration I] [--assignee A] [--priority N] [--tags a,b]
ticket-tui repos list [--query '<filter>'] [--json]
ticket-tui repos show <name> [--json]
ticket-tui prs list [--query '<filter>'] [--json]
ticket-tui prs show <id> [--json]
ticket-tui prs vote <id> approve|suggest|wait|reject|none
ticket-tui prs complete <id> [--strategy squash|merge|rebase] [--keep-source] [--no-transition]
ticket-tui prs abandon <id>
ticket-tui prs autocomplete <id> on|off
ticket-tui prs comment <id> ["text" | -]
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

`sync` pulls and exits, printing what moved — `Synced 3 changes from
my-org/my-project`. It is incremental by default, starting from the watermark
the last pull left behind; `--full` replaces every stored work item, which is
also how a database is pointed at another project. A running TUI notices the
new rows through the same file watcher it uses for any other writer, within a
second. Anything that stopped the pull is an error and exits non-zero.

`show` and `list` read the database and never touch the network, so they work
without an Azure DevOps organization configured at all. `--query` takes the
TUI's own [filter grammar](#controls): `field:value` pairs narrow,
`assignee:@me` means whoever the last sync signed in as, and whatever is left
over is matched fuzzily and orders the rows. Without a fuzzy term the
rows come back newest change first. `is:bookmarked` matches nothing out here:
bookmarks live in the TUI's session file, which a one-shot read does not open.

```console
ticket-tui list --query 'state:doing assignee:@me' --json
```

`--json` prints one object per work item, with the fields under the names the
filter grammar uses — `id`, `organization`, `project`, `rev`, `type`, `title`,
`state`, `assignee`, `priority`, `area`, `iteration`, `tags`, `created`,
`changed`, `url` — and `show --json` adds `description`. An unset field is
`null`.

`edit`, `comment`, and `create` write over the same REST API the TUI writes
over, and store the copy Azure DevOps answers with straight into the database,
so a running TUI shows the change without a pull. They print the work item and
its new revision:

```console
$ ticket-tui edit 613 --state Doing --tags cli,agents
#613 rev 5: State → Doing, Tags → cli; agents
```

Every field named in one `edit` travels in one document, led by the revision the
database holds for that work item, so a work item somebody else moved on is
refused rather than overwritten — run `ticket-tui sync` and try again.
`--assignee` takes a display name, a sign-in address, or `@me`, and an empty
value takes the work item off whoever holds it; `--tags` replaces the tag list;
`--description-file` reads Markdown and writes the HTML Azure DevOps stores,
the same conversion the Actions menu's description editor makes.

`comment` takes its body as an argument or down a pipe, and `prs comment` the
same way. `-` reads standard input to end of file, which is also what leaving
the body out does when standard input is not a terminal; leaving it out **at** a
terminal is a usage error naming both forms rather than a wait for input nobody
is going to type.

```console
$ cargo test 2>&1 | tail -30 | ticket-tui comment 642 -
#642 comment 91 posted
```

A body that came down a pipe is program output, so it is posted as a fenced
code block — `<pre>` on a work item, a ` ``` ` fence on a pull-request thread,
which is what each API stores — and its columns line up in the portal and in
the TUI's own comment view. A body typed as an argument is a sentence and stays
plain text, as it always has. The trailing newline a pipe carries comes off; the
tabs a test runner lines its output up with do not. Over 64 kB the post stops
and says the size — `that is a log, not a comment: 212 kB. Pipe it through
tail` — because truncating silently would keep the half that does not matter.

`repos` and `prs` are the Repos and Pull requests tabs without the tab. Both
reads answer from the database and take the tab's own filter grammar —
`repos list --query 'local:dirty'`, `prs list --query 'reviewer:@me vote:none'`,
which is what the To review count counts. `repos` also reads the workspace, so
the Local column says what `git status` says; that is the only thing either read
touches beyond SQLite, and it never fetches.

```console
$ ticket-tui prs list
!7  pr-checkout-smoke  Jacob Ragsdale  0/0  —          Checks test PR
!1  development        Jacob Ragsdale  1/1  succeeded  ado-cli PR smoke test
```

The columns are `!id repo author votes build title`, where votes counts the
reviewers who have approved out of those asked. `repos list` prints
`name branch prs pipelines local`. `prs show` adds the reviewers and their
votes, the work items it carries, the build and the discussion; `--json` prints
the same, with `list` leaving out the reviewers, work items, threads and
description the way `list` leaves out a work item's body.

The five writes go out over the same REST API the TUI uses and store the copy
Azure DevOps answers with, so a running TUI shows the change without a pull. A
completion carries the head commit the stored copy was read at, so a merge that
raced somebody else's push is refused rather than landing over it; a refusal
exits non-zero in Azure DevOps's own words. `prs vote` writes as whoever is
signed in — read once from Azure DevOps and kept in the database, since no
work-item endpoint ever reports it.

```console
$ ticket-tui prs vote 7 approve
!7 vote: approve
$ ticket-tui prs complete 7 --strategy squash
!7 completed
```

`pipelines` and `runs list` read the database; everything else under `runs` and
`approvals` reads or writes Azure DevOps, because a timeline, a log and a run's
own progress are not things a pull stores. `runs list` takes `--pipeline` and
the tab's run grammar (`status:`, `result:`, `branch:`, `by:`, `reason:`).
`runs show` prints the header and the timeline as a tree, with the same glyphs
the tab draws.

```console
$ ticket-tui runs show 4
✓ run 4 · succeeded · 20260516.2
ado-helper-smoke on main
…
Timeline
  ✓ __default  1m 00s
    ✓ Job  1m 00s
      ✓ Wait briefly  59s
```

`runs logs` prints one node's log — `--job` or `--task` names it, and with
neither it takes the deepest node still running, which is what the tab's log
pane shows. `--follow` keeps printing as the node writes, at the watcher's own
cadence and honouring the same throttling, and returns when the node finishes.

The blocking pair is what an agent wants: `runs trigger <pipeline> --branch main
--follow` starts a build, tails its log, and exits when it stops; `runs wait
<id>` just waits. Both exit with the result rather than making you parse
anything — **0** succeeded, **1** failed, **2** canceled, **3** partially
succeeded — so a script can branch on `$?`.

```console
$ ticket-tui runs trigger 'ticket-tui CI' --branch main --follow
run 51 queued: ticket-tui CI on main
…
run 51 succeeded · 20260829.4
```

`create` adds a work item of any type the process template offers, and
`--parent` links it under an existing one. A refusal prints what Azure DevOps
said and exits 1.

`pods` is the one read with no database to answer from: it reads the clusters
`config.toml` names through `kubectl`, every time. It prints `cluster namespace
pod ready status restarts age`, or `no matching pods`; `--json` adds the node,
the IP, the owner as `Deployment/orders-api`, every container with its image
and state, and the labels. The positional argument is the AKS tab's own
[grammar](#aks), and `--cluster` and `--namespace` narrow what is read rather
than what is printed. No repository is matched to a pod here — that lookup wants
the project's repositories, and this command does not open the database. A
cluster that could not be read is one line on stderr and a non-zero exit after
the pods that did answer have been printed, because a partial answer is still
worth having; a `config.toml` with no `[[clusters]]` is an error saying to add
one.

`acr` and the four vault groups are the [ACR](#acr) and [Key Vault](#key-vault)
tabs without the tab, and the other reads with no database behind them: the
subscription is asked for its registries and vaults on every invocation, and a
registry's or a vault's own data plane answers for what is inside it. They
resolve the subscription the way the tabs do — `--subscription`, then
`TICKET_TUI_SUBSCRIPTION`, then the Azure CLI — and an unresolved one or a
refused token is an error rather than an empty listing, because there is nothing
stored here to fall back on. A name the subscription does not hold is refused by
name (`no registry called ghcr in subscription sub-1`, `no vault called
billing-kv in this subscription`), matched ignoring case like every other name on
this command line.

```console
$ ticket-tui acr list
acr  rg  Premium  westeurope  acr.azurecr.io

$ ticket-tui acr repos list --registry acr
team/orders-api  7  9  2026-08-29 09:00:00 UTC (1d)
team/billing     —  —  —

$ ticket-tui certs list --vault atlas-kv
wildcard  yes  2026-08-01 09:00:00 UTC (29d)  2026-09-29 09:00:00 UTC (0s) expires in 30 days
```

The tables are the tabs' own with their hidden columns shown: `acr list` prints
`name resource-group sku location login-server`, `repos list` prints
`repository tags manifests updated`, `tags list` prints `tag digest created`
newest first, `vaults list` prints `name resource-group location sku uri`, and
the three item listings print `name enabled updated expires` with a
`content-type` column after it for secrets and the expiry in words as well as a
stamp for certificates. A count nobody could read is a dash rather than a nought,
the way the tab draws one that has not landed; `acr repos list` puts one line per
unreadable repository on stderr and exits non-zero after printing the rows that
did answer. `show` on either resource is the block of text its details pane
draws, portal link and all. `--json` carries the same fields plus the resource's
`id` and a `portal_url`, so an agent need not build one; the counts (`repositories`
on a registry, `items` on a vault) are only on `show`, which is the one form that
reads them.

Nothing under `acr` or the vault groups writes. Everything is read-only for the
same reason the tabs are, and a listing never carries a value.

`ticket-tui secrets show --vault V NAME --value` is the one command in this file
that reads a secret's value. It prints it raw on stdout and prints nothing else,
so `$(…)` around it is the value and only the value; it conflicts with `--json`
at the command line rather than resolving into a quiet preference for one of
them; and it is the only caller of `Secret::expose` outside the details pane. The
metadata form of the same command reads the listing every other form reads, never
the value, and ends by saying so — `value: not shown; pass --value to print it`.

## Live agent context

While ticket-tui is running, it atomically publishes a compact JSON snapshot
beside the cache. For `tickets.sqlite3`, the file is `tickets.context.json`.
This is the supported interface for an LLM agent to understand the current view
without scraping terminal cells or causing SQLite reloads; the
[subcommands](#subcommands) above are how one acts on what it reads.

Schema 3 describes the whole workspace rather than one tab. At the top level:
the cache path, the signed-in display name that marks your own work items, the
process ID and last-change timestamp, a `sync` block, the `pending_edits` not
yet answered, and `active_tab` — `work_items`, `repos`, `pull_requests`,
`pipelines`, `aks`, `acr` or `key_vault`. Under those, one block per tab, **all
of them filled whether or not the tab is showing**, so an agent asked about a
pull request need not ask the user to press `3`:

- `work_items` — everything schema 2 kept at the top level, unchanged and one
  level down: selected and checked tickets, the rows in the viewport with the
  matching and total counts and whether finished work is off the table, the
  complete query with its fuzzy text and parsed filters, and the sort order,
  named view, mode, focused pane, family cursor and details scroll. The selected
  ticket also carries `related`, its artifact links.
- `repos` — the rows on the table and the selected one, each with its default
  branch, its pull-request and pipeline counts and the clone on this machine
  (path, branch, dirty, ahead, behind, and what git is doing to it), plus the
  `workspace` they live in.
- `pull_requests` — the queue and the selected request with its reviewers and
  their votes, the work items it carries, its build, whether auto-complete is
  set and how many threads are unresolved, plus `to_review_count` and whether
  closed ones are shown. Every row carries `my_vote`.
- `pipelines` — the `level` (`pipelines` or `runs`), the selected pipeline and
  run — the run with its stages, each stage's state and result — the log being
  tailed (`run_id`, `log_id`, `node`, `line_count`, `following`), how many runs
  are going, which are watched, and how many approvals are pending.
- `aks` — the `clusters` `config.toml` names, the pod under the cursor with its
  containers, how many rows the table is showing and how many of them are
  unhealthy, the log being tailed (`pod`, `container`, `previous`,
  `line_count`, `following`), and one `errors` line per `(cluster, namespace)`
  that could not be read. Pods are read live rather than stored, so the block is
  only as current as the last read.
- `acr` — the `level` (`registries` or `repositories`), the registry under the
  cursor with its group, SKU, location, login server and portal link, the
  repository under it with its tag count and stamp, the tag the details pane's
  own cursor is on with its full digest, and how many rows the query leaves on
  the table.
- `key_vault` — the `level` (`vaults` or `items`), the vault under the cursor
  with its group, location, SKU, URI and portal link, the item under it —
  `kind`, `name`, `enabled`, `updated`, `expires` — the row count, and
  `expiring_certificates`, the same number the tab's `◇N` badge shows. The item
  also carries `revealed`, which says whether its value is on the screen this
  minute. **There is no field for the value and there is not meant to be one:**
  a value is read for the screen alone and this file is written to disk.
- `arm` — whether those two have anything to read at all: `subscription`,
  `offline`, and `last_error`, the one line that says why an offline run reads
  nothing. Both tabs are described on every run; `arm.offline` is what tells an
  agent that an empty `acr` means "no subscription", not "no registries".

The last three blocks, like `aks`, are as current as the last read rather than
as current as a pull: 60 seconds for the inventory while an ARM tab is showing,
and once per focus for everything under it.

Schema 2 consumers read the work-item fields at the top level and will find them
under `work_items` instead. A block added within schema 3 — `aks`, `acr`,
`key_vault` and `arm` all were — is additive, and a reader should ignore fields
it does not know rather than refuse them.

The file is replaced after meaningful rendered-state changes and removed on a
clean exit. A crash or forced termination can leave a stale file, so consumers
must check its process ID and treat stale data as the last observed view.

This repository includes the `ticket-tui-context` agent skill and a compact
reader:

```console
uv run .agents/skills/ticket-tui-context/scripts/read_context.py
```

Pass `--database PATH` for a custom cache, `--json` for the exact snapshot,
or `--details` to join the selected ticket to its full SQLite records. See the
skill's [context schema reference](.agents/skills/ticket-tui-context/references/context-schema.md)
for field-level semantics.

## Roadmap

Planned work is tracked as work items in the same Azure DevOps project
ticket-tui is pointed at, so the tool browses its own backlog. Sync the
database and read it there for the current list; `HANDOFF.md` names where the
last round of work stopped.

## Develop and verify

Run the same checks used by CI:

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```

CI exercises these checks on current macOS and Linux runners.

The renderer tests assert through the theme's tokens rather than through
colour names, so the suite is run again in each palette that changes what a
token holds:

```console
NO_COLOR=1 cargo test --all-targets
TICKET_TUI_THEME=terminal-light cargo test --all-targets
TICKET_TUI_THEME=mono cargo test --all-targets
```

`Theme::from_env` reads both variables, so a run under one of them paints in
that palette from end to end — which is what makes the matrix worth running:
a colour that reads on one ground and vanishes on another fails there rather
than on somebody's screen.
