# Where to pick up

Last updated 2026-08-29. The backlog itself lives in Azure DevOps
(`jacobragsdale/development`); this file is only the pointer into it. Run
`ticket-tui` and browse the epics for the current state.

## State of `main`

- The four-tab roadmap is under way: Epic 656 is finished (#661–#667) and
  #668 (repos sync) is Done. Epic 659 (Pipelines, #680–#689) is next. `cargo fmt --check`, `cargo clippy --all-targets
  --all-features -D warnings`, `cargo test --all-targets` (415 tests, and again
  under `NO_COLOR=1`) and `cargo build --release` are clean.
- Database schema is `PRAGMA user_version = 13`. The first launch of this build
  rebuilds any older database and does one full pull automatically.

## The module tree (#661, #662)

`src/app.rs` and `src/ui.rs` are directory modules; every file is under 1,500
lines. `App` is now only `{ shell, work_items }`: `Shell` is the state every
screen shares and `WorkItemsScreen` is the first `Screen`. A screen method that
needs the shell takes `shell: &mut Shell` (or `&Shell`) as its first argument
after the receiver; nothing else reaches across.

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
    src/ui/mod.rs       render, render_screen, the layout, the theme, anchoring
        details.rs      the details pane and the family tree it draws
        overlays.rs     the list overlays, chips and facet bar
        pickers.rs      pickers, the prompt, the form, the delete confirmation
        table.rs        the table, its cells and their colours
        widgets.rs      the modal frame, query field, controls, scrollbar, paint
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

The roadmap above. #661–#668 are Done; start at **Epic 659** (#680–#689, the
live log tail in #684 is the critical one), then Epic 658, the rest of Epic
657, then Epic 660. then #668 before anything in Epic 659. Query round-tripping was an
earlier loose end and is fixed: `quote_if_needed` now escapes `\` and `take_quoted`
unescapes it, so `iteration:"development\Sprint 1"` survives
`format_query`/`parse_query` (#654), and a backslash typed once inside a quoted
filter value stays a single backslash (#655).

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
