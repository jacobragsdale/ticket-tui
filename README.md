# ticket-tui

`ticket-tui` is a fast terminal browser for Azure DevOps work items. It keeps a
local SQLite database synced from one Azure DevOps project and reads from that
database, so navigation, sorting, filtering, and fuzzy search stay instant. It provides mouse
navigation, responsive ticket details, field sorting, live fuzzy search, and
links that open in the system browser.

Azure DevOps is the source of truth. A background worker pulls the project's
work items over the REST API every minute and replaces the local rows, so a
state changed in the browser shows up in a running TUI without restarting it;
the database file itself is durable, lives in the platform data directory, and
is the interface other tools and agent skills read. A field changed in the TUI
is written straight back over the same API; everything else it does is local.

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
[Organization and project](#organization-and-project).

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

ticket-tui stores no secrets. It reads the token from the CLI or the environment
on each sync and keeps nothing but work-item data in SQLite.

## Organization and project

Both values are resolved in this order:

1. the `--org` and `--project` flags;
2. the `TICKET_TUI_ORG` and `TICKET_TUI_PROJECT` environment variables;
3. the `[defaults]` entries in `~/.azure/azuredevops/config`, written by
   `az devops configure --defaults organization=... project=...`
   (`AZURE_CONFIG_DIR` moves that file).

`--org` accepts a bare slug, `https://dev.azure.com/<slug>`, or
`https://<slug>.visualstudio.com`; all three reduce to the slug. Without both
values the TUI browses the database offline and never syncs; with `--sync` an
unresolved value fails with the missing flag, variable, and command spelled out.

### The database remembers which project it holds

Every successful pull records the organization and project it ran under in the
`sync_meta` table. A run that resolves a different pair will not sync into a
database that already holds work items: the sync worker never starts, the TUI
opens offline over the rows that are there, and the notification — and the `i`
overlay's sync line — says

```text
Database holds other-org/borealis; pass --database for another project or --sync to replace it
```

so a typo in `--project`, or a `TICKET_TUI_ORG` left over from yesterday, cannot
quietly replace a database. `--sync` is how the replacement is asked for: it
pulls the new project in full and re-stamps the database with it. A database
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
without opening the TUI at all; the deprecated `--sync` flag runs one before the
TUI opens and blocks until it finishes, and that pull failing is a notification
over the existing database rather than a reason to refuse to start. Only one
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
schema this build rebuilt, or `ticket-tui --sync`, which is the way to rebuild
one deliberately. A full pull leaves a watermark behind, so the pulls that
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
it is reached from the Edit menu or the palette instead.

`e` opens the Edit menu, which lists the fields that can be changed; `S`
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

The Edit menu's remaining rows are the edits that would otherwise mean opening a
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

**Assignee** has a key of its own, `a`, as well as its Edit menu row and
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
under nothing. It is the one Edit menu row that is not always there: it appears
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

**Add comment** is the last Edit menu row, and `Add comment` in the palette. It
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
`Enter` on one opens the same picker the Edit menu opens over a work item: the
work item types the project's process offers, the iteration and area trees, and
the assignee list, each writing its choice back into the field and returning to
the form. `Enter` on a typed field moves on to the next one; submitting is
deliberately not bound to it, so a stray `Enter` halfway down the form never
files a half-typed work item. `Ctrl-S` or `[Create]` files it, `Esc` or
`[Cancel]` closes it. Clicking a field focuses it and puts the caret where the
click landed, and the two buttons are clickable.

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
anybody files, and none of it is worth retyping, so `N` — or the Edit menu's
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

**Type** is the type the parent's own type breaks down into. The project's
process answers that where it can — the types come back in the order the process
lists them, so the one after the parent's is what sits under it — and where the
list has not been read yet, the Basic process's own breakdown does: an Epic into
Issues, an Issue into Tasks, and a Task into more Tasks. A type with nothing
obvious under it keeps its own, because a child of the same type is always
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

## Controls

| Input | Action |
|---|---|
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
| `v` | Toggle relevance-first or strict field ordering during search |
| `c` | Toggle compact or comfortable table rows |
| `f` | Focus the filter bar; `h`/`l` change field, `j`/`k` values, `Space` toggles |
| `+` | Open the full filter overlay for extra fields |
| `w` | Show or hide (`Space`), reorder (`J`/`K`), and resize (`<`/`>`) columns, Progress among them |
| `p` / `:` | Open the command palette |
| `V` | Open views: five built-in ones and your own; `n` saves, `Enter` loads, `d` deletes |
| `e` | Open the Edit menu of field editors; `Enter` opens the one chosen |
| `S` | Change the selected work item's state, or every checked one; `Enter` applies, `Esc` cancels |
| `a` | Change who the selected work item is assigned to, or every checked one; type to filter, `Enter` assigns |
| `e` → Title/Priority/Tags/Iteration/Area | Edit the title, priority, tags, iteration, or area; also `Edit title`, `Edit priority`, `Edit tags`, `Change iteration`, `Change area`, and `Change assignee` in the palette |
| `e` → Description | Edit the description in `$VISUAL`/`$EDITOR`/`vi` as Markdown; also `Edit description` in the palette |
| `e` → Add comment | Leave a one-line comment on the selected work item; also `Add comment` in the palette |
| `n` | Open the new work item form; `↑`/`↓` or `Tab` moves between fields, `Enter` opens a field's picker, `Ctrl-S` creates, `Esc` keeps the draft |
| `N` | Open the same form as a child of the selected work item: the type it breaks down into, the parent fixed, the area and iteration inherited; also `New child` in the Edit menu and the palette |
| `u` | Undo the last edit, putting the value back; a bulk change goes back under one press |
| `m` | Bookmark or unbookmark the selected ticket |
| `Space` | Toggle ticket multi-select; two or more make `S`, `a`, and Iteration bulk edits |
| `y` | Copy selected (or current) ticket IDs |
| `[` / `]` | Jump to the previous or next recently viewed ticket |
| `Tab` | Toggle focus between tickets and details |
| `d` | Toggle the details screen when the terminal is under 70 columns |
| `Enter` | Select the family cursor ticket; with details focused, edit the field under the pointer, or open the work item |
| `o` | Open the selected ticket in the system browser |
| `r` | Sync from Azure DevOps now, without waiting for the timer |
| `i` | Show database path, row counts, hidden finished rows, and sync freshness |
| `?` | Show the in-app help; use arrows or page keys to scroll it |
| `q`, `Ctrl-C` | Quit |

The help overlay's Actions section and the palette's key labels are generated
from the same command table these keys are bound in, so a binding reads the same
way everywhere.

The details pane is one scrolling document rather than a pinned heading over a
scrolling body. Its heading — title, ID / Type / State, the family breadcrumb,
assignee and priority, tags, project and revision, child progress, and the
work-item URL — scrolls away with everything under it, in this order: the
family tree, Planning, Description, History, and Comments. The scrollbar
therefore measures the whole pane and its thumb reaches the bottom, `End` lands
on the last comment, and a field value stays clickable wherever the scroll has
carried it. Moving the family cursor scrolls the tree back into view when the
heading has pushed it below the fold.

A work item with children reports how far they have got. The heading reads
`Children: 3/7 done` with a six-cell bar beside it, every family-tree row that
is itself a parent trails its own ` 3/7`, and the optional Progress table
column shows the same ratio. All three count direct children only — an Epic
reads over its Features rather than over every Task underneath them — and both
Completed and Removed states count as done, so a cut work item stops holding
its parent back. A work item nobody broke down shows nothing at all in any of
the three places, never `0/0`. The bar is drawn from two different glyphs
rather than two colours, so it reads the same under `NO_COLOR`.

Mouse input stays captured so the TUI can provide its own pointer controls
without restoring terminal drag-select. Wheel scrolling moves the hovered
table, details pane, help, or overlay by three rows or lines and does not
change keyboard focus or the selected ticket. Left-click activates the
visible control under the pointer on release: search, filter pills, sort
headers, ticket rows, checkboxes, bookmark markers,
underlined IDs and URLs, details-pane field values, tabs, overlay rows, and
close/action buttons. A field value in the details pane — the title, the state,
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
dragged. Dragging the divider between the Tickets and Details panes resizes
them, both side by side and stacked; the tickets pane keeps at least 40 columns
and details at least 30 side by side, and each keeps six rows when stacked.
`Reset pane split` in the command palette restores the built-in layout.
Right-click, double-click, and horizontal wheel gestures are not
used. Terminals supporting OSC 22 show a browser-style pointer over external
URL targets and over the details-pane values that can be edited, which underline
under the pointer whether or not the terminal has colours.

Search accepts a compact grammar such as `state:active type:bug
assignee:"Avery Chen" priority:1 tag:rust`, plus `project:`, `area:`, and
`iteration:`. Values in the same field are combined with OR; different fields
are combined with AND. `is:bookmarked` limits the table to locally bookmarked
tickets. `changed:` and `created:` take a comparison rather than a value, so
`changed:<7d` is what moved in the last seven days and `changed:>14d` what has
gone untouched for longer, over units of minutes, hours, days, and weeks
(`m`, `h`, `d`, `w`), with `<=` and `>=` taking the edge and `updated:` reading
as `changed:`. An ISO date compares against the instant instead, at UTC
midnight, so `created:>2026-08-01` is everything raised since the start of
August. Relative windows are measured as the filter runs, so a view saved as
`changed:<7d` still means the last seven days tomorrow, and the `+` overlay
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
or child that is finished is still shown there.

The command palette copies
IDs, URLs, titles, Markdown links, or summaries, edits the title, priority,
tags, iteration, area, or description, leaves a comment, shows or hides finished
tickets, and exports the
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
path segment while details keeps the full path, family-tree rows carry a
one-character state glyph (`○ ◐ ● ✓ ✗`), and matched search characters are
underlined in visible results. ID, Title, State, Type, Pri, Changed, and
Assignee are the columns a fresh session shows; Org, Project, Area, Iteration,
Created, Tags, and Progress are there under `w` and stay hidden until they are
switched on, after which the choice is saved with the rest of the layout. A
parent whose children have all finished goes green and bold, in the Progress
column and in the details heading alike. A hovered row is tinted with a
256-colour background rather than repainted, so its coloured cells keep their own
foregrounds; hovered controls reverse instead. Setting the standard `NO_COLOR`
environment variable selects the monochrome theme, where weight carries the same
distinctions: badges keep their brackets, finished rows dim instead of fading,
state glyphs and your own work items go bold, and a hovered row reverses.

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
stale threshold** in the palette, which remembers what it was given.

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
semicolon separation. The `work_item_relations`, `work_item_comments`, and
`work_item_history` tables hold the graph around each work item: links from
every pull, and comments and revision history for every work item whose
`details_rev` says they have been read. A work item the project stops listing
takes all three with it. The `sync_meta`
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

The database carries `PRAGMA user_version = 12`. Because Azure DevOps is the
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
a terminal to drive. `--database`, `--org`, and `--project` apply to all of
them and may be written either side of the subcommand.

```console
ticket-tui sync [--full]
ticket-tui show <id> [--json]
ticket-tui list [--query '<filter>'] [--json]
ticket-tui edit <id> [--state S] [--assignee A] [--priority N] [--iteration I] [--area A] [--title T] [--tags a,b] [--description-file F]
ticket-tui comment <id> "text"
ticket-tui create --type Issue --title T [--parent ID] [--iteration I] [--assignee A] [--priority N] [--tags a,b]
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
the same conversion the Edit menu's description editor makes.

`create` adds a work item of any type the process template offers, and
`--parent` links it under an existing one. A refusal prints what Azure DevOps
said and exits 1.

## Live agent context

While ticket-tui is running, it atomically publishes a compact JSON snapshot
beside the cache. For `tickets.sqlite3`, the file is `tickets.context.json`.
This is the supported interface for an LLM agent to understand the current view
without scraping terminal cells or causing SQLite reloads; the
[subcommands](#subcommands) above are how one acts on what it reads.

The versioned snapshot includes:

- selected and checked tickets;
- the rows currently rendered in the ticket viewport, plus matching and total
  counts and whether finished work is being left off the table;
- the complete query, fuzzy text, and parsed filters;
- sort order, named view, mode, focused pane, family cursor, and details scroll;
- cache path, the signed-in display name that marks your own work items,
  process ID, and last-change timestamp.

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

Planned work — editing work items and creating them from the TUI — is tracked
as work items in the same Azure DevOps project
ticket-tui is pointed at. Sync the cache and browse it for the current list.

## Develop and verify

Run the same checks used by CI:

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```

CI exercises these checks on current macOS and Linux runners.
