# Where to pick up

Last updated 2026-08-29. The backlog itself lives in Azure DevOps
(`jacobragsdale/development`); this file is only the pointer into it. Run
`ticket-tui` and browse the epics for the current state.

## State of `main`

- **The four-tab roadmap is done**: Epics 656 (#661–#667), 657 (#669–#673),
  658 (#674–#679), 659 (#680–#689) and 660 (#690–#694) are all closed, as is
  #668. `cargo fmt --check`, `cargo clippy --all-targets --all-features -D
  warnings`, `cargo test --all-targets` (516 lib + 28 bin tests, and again under
  `NO_COLOR=1`) and `cargo build --release` are clean.
- **A review round followed the roadmap** (evening of 2026-08-29; see "Review
  round" below): the four tabs were read end to end as a QA pass, and what it
  found is fixed and on `main`.
- Database schema is `PRAGMA user_version = 17`. A schema bump drops and
  rebuilds rather than migrating, so the first launch of a build that raises it
  does one full pull automatically — and a running `ticket-tui` binary from
  before the bump refuses to open the file until it is rebuilt.

## The module tree (#661, #662, #698)

`src/app.rs`, `src/ui.rs` and `src/run.rs` are directory modules; every file is
under 1,500 lines. `App` is `{ shell, tab, work_items, repos, pull_requests, pipelines }`:
`Shell` is the state every screen shares and each tab is a `Screen`. A screen
method that needs the shell takes `shell: &mut Shell` (or `&Shell`) as its
first argument after the receiver; nothing else reaches across.

    src/app/mod.rs      App, AppAction, and the event entry points
        screen.rs       the Screen trait every tab implements
        shell.rs        focus, the pointer, the notification, the layout, sync
        work_items/     the work items screen
            mod.rs      WorkItemsScreen, WorkItemMode, the key map, the footer
            context.rs  the JSON context file agents read
            edits.rs    edits, undo, bulk, comments, reparenting, deletion
            family.rs   the family tree, its cursor, child progress
            forms.rs    the new-work-item form
            history.rs  bookmarks, the checked set, copy, export, the session
            pickers.rs  state, priority, assignee, parent, node, type pickers
            pointer.rs  hover, press, drag, release, the divider
            query.rs    the search box, filters, facets, columns, commands
            views.rs    saved and built-in views, the sprint summary, staleness
            tests/      the same split, plus tests/deletes.rs
        repos/          the Repos tab: the table, the workspace, the git keys
        pull_requests/  the Pull requests tab: votes, complete, comment
        pipelines/      the Pipelines tab: two levels, the timeline, the log
    src/watch.rs        the pipeline watcher thread: live runs, timelines, logs,
                        approvals — cadences that stretch, and no SQLite at all
    src/local.rs        the local-repos thread: the workspace scan and the three
                        git commands the Repos tab runs — also no SQLite
    src/main.rs         opens the module below and reports what it returns
    src/run/mod.rs      run: the database, the workers, the terminal, and back
        engines.rs      the sync, details, local and reload workers, and the
                        agent context file they publish through
        events.rs       the event loop and the actions it hands back
        dispatch.rs     every request that leaves the run, and its timer
        polling.rs      one non-blocking pass over each worker's channel
        editor.rs       the description round trip through $VISUAL/$EDITOR/vi
        desktop.rs      the clipboard and the browser
        pointer.rs      the mouse pointer shape, and what the hover means
        tests/          the same split, over one fake Azure DevOps
    src/ui/mod.rs       render, render_screen, the layout, the theme, anchoring
        details.rs      the details pane and the family tree it draws
        overlays.rs     the list overlays, chips and facet bar
        pickers.rs      pickers, the prompt, the form, the delete confirmation
        table.rs        the table, its cells and their colours
        widgets.rs      the modal frame, query field, controls, scrollbar, paint
        repos.rs        the Repos tab's own renderer
        pull_requests.rs  the Pull requests tab's own renderer
        pipelines.rs    the Pipelines tab's own renderer
        tests/          the same split

Every ui function takes `screen` and `shell` rather than `app`, and
`ui::render` paints the tab bar itself and then goes through `App::screen()`
and `Screen::render` for the rest of the frame.

Shared pieces the later tabs are meant to reuse: `columns::ColumnId` and
`TableLayout<C>` with `ui::table::render_list_table` (#663), `app::ListCursor`
(#663), `filter::FilterSchema` with `FilterSet<S>` and `parse_query::<S>`
(#664), and `PlaceholderScreen` (#665), which is what tabs 2–4 show until
their own tickets land. Pointer targets never name one screen's types:
`SortHeader` carries a column key, `FacetPill` a field key, `RemoveChip` and
`SelectTab` an index.

**The key map changed in #665**: `F` more filters (was `+`), `c` columns (was
`w`), `v` views (was `V`), `V` save view, `e` opens the Actions menu (the Edit
menu renamed), and row density and search order are palette-only. `1`–`4`
switch tabs. Commands carry a `Scope` — `Global` or `Tab(TabId)` — which is
what keeps another tab's keys out of the palette and groups the help.

## Shipped 2026-08-29

Read path (Epic 603, closed): throttling backoff honouring `Retry-After`
(#645). **A pre-existing bug was fixed along the way**: the WIQL endpoint was
called without `timePrecision=true`, so every incremental pull whose watermark
carried a time failed with HTTP 400 — the 60-second background refresh had been
silently failing and only full pulls worked.

Editing (Epic 611, closed): bulk edit over the checked set with one summary
notification (#619), `u` undo — a bulk change undoes as one unit (#646),
`Delete work item…` with a confirm overlay that names the orphaned children
(#647).

Creating (Epic 620, closed): a generic `FormOverlay` behind `n` (#621), `N` for
a child with the type, area and iteration inherited (#622), Set parent /
Remove parent through a cycle-safe picker, one PATCH with `test /rev` (#623).

Agents (Epic 624, closed): context JSON v2 with a `sync` block and
`pending_edits` (#625), CLI subcommands `sync|show|list|edit|comment|create`
(#626), the rewritten `ticket-tui-context` skill (#627). #643, the in-TUI agent
chat pane, was closed on 2026-08-29 as decided against: an agent in its own
terminal reading `tickets.context.json` through the skill and writing through
the CLI covers the need, so the TUI stays a viewer and grows no chat UI.

Manager workflows (Epic 628, closed): built-in views with `@me` / `@none` /
`@current` / `@open` sentinels (#629), sprint summary overlay (#630), stale
highlighting sharing #649's predicate (#631), child progress (#648), `changed:`
and `created:` date filters (#649), finished tickets hidden by default (#652).

The four tabs (Epics 656–660, all closed): the shell split and its shared
scaffolding (#661–#667), repos in the pull (#668), Pipelines with the live log
tail and approvals (#680–#689), Pull requests with votes, complete, abandon,
auto-complete and one-line comments (#674–#679), and Repos with the workspace
scan, clone, fetch, pull and cross-tab jumps (#669–#673). Two background
threads carry the live parts: `watch.rs` for pipelines and `local.rs` for git,
and neither writes SQLite. Then the cross-links and the agent surface
(#690–#694): a work item's artifact links, context schema 3 describing all four
tabs, `repos`/`prs`/`pipelines`/`runs`/`approvals` on the command line with
`runs wait` and `runs logs --follow` exiting on the build's own result, and the
skill rewritten around all of it.

Also: the CLI now resolves query sentinels through the same `MatchContext` the
TUI uses, and refuses one it cannot resolve rather than returning an empty list.
`ticket-tui sync` caches the classification trees, which only opening a picker
used to do.

## Code review pass, 2026-08-29

A cleanup slice ahead of the Repos and Pipelines tabs, with no behaviour change
except one: the deprecated `--sync` flag is gone (`ticket-tui sync --full` is
the replacement, and the wrong-project notification now names it). What went:
the `blocking_sync` path that duplicated the worker's full pull; `SessionKey`
(`TicketKey` derives serde and the file shape is unchanged); the shortcut
fields on `HitRegions` (`table_body`, `headers`, `detail_links`… — every reader
now goes through `find_target`); `FamilySnapshot::tree_entries`/`jump_keys`
and the `other_links` half of the snapshot, which nothing drew; the `UrlOpener`
trait (a `Fn(&Url)`); and a dozen accessors with no callers. What was folded:
`model::same_text` replaces four case-insensitive comparisons, `App::write_refusal`
the six copies of the offline check, `SyncRuntime::send` the five copies of
"hand it to the worker or explain", `stamp_database` the nine
`configure_database` calls, `AzureClient::wit_url` the five hand-built URLs,
`ui::render_query_field` the four picker filter fields, and
`sync::pull_summary` the wording the TUI and the CLI both had.

## The four-tab roadmap (Epics 656–660, created 2026-08-29)

ticket-tui grows into a terminal front-end for the whole project: Work items ·
Repos · Pull requests · Pipelines, keys `1`–`4`, with the browser (`o`) for
anything complicated. The design lives in the epics; every issue carries
Problem / Approach / Done-when, and **Epic 656's description is the working
agreement** for all of them (worker and watcher rules, the shared glyph
language, keyboard–mouse parity, the test gates). Read it before any ticket.

Build order: **656 Tab-ready shell** (661 split files → 662 extract the
screen → 663 list scaffolding → 664 filter schema → 665 tab bar and key map →
666 per-tab session → 667 jumps and `id:`), then **668** repos sync (pulled
forward: pull requests and runs name repos), then **659 Pipelines** (680–689;
the live log tail, 684, is the critical feature of the roadmap), **658 Pull
requests** (674–679), the rest of **657 Repos** (669–673), and **660
Cross-links and agents** (690–694).

Decisions behind the tickets: no terminal hand-off and no Herdr integration
(Repos is detect / clone / status / fetch / pull, not a git client); pull
request actions in the TUI are vote, complete, abandon, auto-complete and a
one-line comment, never the diff; pipelines get trigger, cancel, retry,
approvals, and live status and logs through a second `PipelineWatcher` thread
that never writes SQLite; agents are first-class through `ticket-tui`
subcommands and context v3.

## What is left

Nothing on the four-tab roadmap: Epics 656–660 are all closed. The last slice
(Epic 660) added the artifact links a work item carries, context schema 3, the
`repos`/`prs`/`pipelines`/`runs`/`approvals` subcommands, and the rewritten
`ticket-tui-context` skill in `.agents/skills/`, which is the place to start for
anything an agent should know about this project.

Three things the finished tabs left behind, all noted on tickets:

- The facet bar and chip row are still written against the work items screen,
  so the other three tabs filter through their search box but draw no pills
  (#681).
- The Pipelines details pane does not scroll as a whole: the log has its own
  scroll, the header, Related links and timeline above it do not (#689).
- The Actions overlay is work-items-only. (The command palette, the help, the
  columns editor and the database overlay are not any more: the review round
  below opened them on every tab.)

Four bugs the CLI work turned up in code that had already shipped, all fixed —
worth knowing about because they share a shape: **the fake source in the tests
implemented what the real client did not**.

- `AzureClient` never overrode `pull_request_action`, `comment_on_pull_request`
  or `pull_request_threads`, so completing, abandoning, auto-completing and
  commenting on a pull request all failed against the real API and the
  discussion was always empty (#677, #678).
- A completion never carried `lastMergeSourceCommit`, though the worker's doc
  comment promised it (#677).
- The pull-request list endpoint does not always answer with `_links`, so every
  stored pull request had an empty URL and `o` opened nothing (#674).
- `parse_timeline` sorted by an `order` field that only ranks siblings, and a
  job's log lives on the Phase record that gets flattened away — so the tree
  read out of order and no job had a log (#693).

## Known data issue, not a code bug

`development\Sprint 1` has **no start or finish dates** in Azure DevOps, so
`current_iteration()` returns `None`. The `Current sprint` built-in view
therefore lists nothing, and the sprint summary falls back to the selected row's
iteration. Setting the dates on the iteration makes both work.

## Working agreement

Each ticket carries Problem / Approach / Done-when. Flow: set the ticket to
Doing (`ticket-tui edit N --state Doing`), implement it on `main`, keep
fmt/clippy/test/release green, commit and push at each working checkpoint, then
comment on the ticket saying what shipped and set it Done. One ticket at a time,
in the build order; no worktrees and no feature branches.

## Review round, 2026-08-29 (evening)

Jacob reported two bugs off the bat — a clone that sat on `cloning…` for ever,
and "hovering selects the element" — and asked for the whole four-tab delivery
to be reviewed as a QA pass. Everything below is fixed and on `main`.

**The clone.** `C` cloned over ssh, and the machine's key is not registered
with Azure DevOps, so ssh fell back to a *password prompt on /dev/tty* — the
terminal the TUI owns — and git waited for ever. `local.rs` now runs every
remote git command with prompts off (`GIT_TERMINAL_PROMPT=0`, `ssh -o
BatchMode=yes -o ConnectTimeout=20`, stdin closed), a 30-second stall timeout
(`http.lowSpeedLimit/Time`), and — the part that makes it *work* rather than
merely fail fast — signs an Azure DevOps https remote with the same
`authorization_header()` the sync uses, plus `X-VSS-ForceMsaPassThrough: true`,
which git needs for an MSA-backed org exactly as the REST calls do. The header
is scoped to the host (`http.https://dev.azure.com.extraheader`) and passed as
`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` so it is never offered to GitHub and
never shows in `ps`. https is the default clone URL now;
`TICKET_TUI_CLONE_PROTOCOL=ssh` asks for ssh. Fetch and pull of a clone whose
origin is an Azure DevOps https remote are signed the same way.

**The hover.** Not reproduced: driving the release binary under a pty with
Ghostty-style SGR motion (`ESC [ < 35 ; x ; y M`) hovers rows (the
`Indexed(237)` tint from #642) and never moves the selection, and grok-night's
palette makes the selection (`#6c6c6c`) brighter than the tint. One real defect
was found on the way and fixed: a click on the tab bar recorded its press on
the screen but the release was swallowed by `App::handle_mouse`, so the pointer
believed the button was still down and every later move read as a drag. If
the report stands after that, the next thing to capture is the raw byte stream
Ghostty sends while hovering.

**Everything else the pass found**, each with a test:

- The Repos tab was empty until the first pull (`App::relate_repos` now runs at
  start-up too).
- `?`, `p`/`:`, `c` and `i` did nothing on tabs 2–4 while their footers said
  `? help`: the four shared overlays now open on every tab, drawn and driven
  by the work items screen on the other tab's behalf (`App::overlay_for`,
  `AppAction::RunCommand`); the palette lists that tab's commands and the
  columns editor edits that tab's layout.
- The Pull requests and Pipelines verbs were raw keys with no command behind
  them, so the help listed none of them and the `[Approve]`… and
  `[Cancel] [Retry]` buttons had no click regions. They are scoped commands
  now (`CommandId::ApprovePr`… `WatchRun`), and the buttons are the keys.
- The Repos, Pull request and run details panes placed their click targets by
  line index while wrapping long lines, so once a URL or title wrapped every
  target under it was off by a row or more (`ui::widgets::wrapped_rows`). The
  Repos and Pull request panes scroll now, with a `Details` scroll region.
- A timer pull replaced the pull request and run lists and kept only the row
  *number*, so the hand moved to a different row when something new arrived;
  the watcher's `merge_live_runs` did the same. Both keep the selection by id,
  and `set_pipelines` keeps what the watcher already saw (a run newer than the
  pull's window, or one it has already seen finish).
- A jump to a pull request, pipeline, run or repository the tab's query hid
  said "not in this database"; the query is cleared instead.
- `Enter` on the runs level re-opened the runs and reset the cursor; `R`
  retried runs that had succeeded; `W` on a finished run produced an instant
  "finished" toast rather than a refusal.
- A pull request was only re-read when its head or reviewers moved, so the
  Build column never left `queued`; one whose policy build is undecided is
  read again (`sync::build_undecided`).
- `runs trigger --follow` failed at once ("has written no log yet") because a
  just-queued run has no timeline, and `--follow` stopped when the first node
  finished; it waits, and follows the run node by node to the end.
- `runs trigger` sent `refs/heads/refs/heads/<branch>` (`start_run` takes
  either spelling now); `prs vote` by a non-reviewer left the stored copy
  without the vote.
- The help popup wrapped its longest rows; the empty table said "No tickets
  in this database" when all 100 were merely finished and hidden.
