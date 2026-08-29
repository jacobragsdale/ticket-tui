# Where to pick up

Last updated 2026-08-29. The backlog itself lives in Azure DevOps
(`jacobragsdale/development`); this file is only the pointer into it. Run
`ticket-tui` and browse the epics for the current state.

## State of `main`

- Every implementable ticket in the roadmap is merged and Done. `cargo fmt
  --check`, `cargo clippy --all-targets --all-features -D warnings`, `cargo test
  --all-targets` (415 tests, and again under `NO_COLOR=1`) and `cargo build
  --release` are clean.
- Database schema is `PRAGMA user_version = 12`. The first launch of this build
  rebuilds any older database and does one full pull automatically.
- Nothing is in Doing. No worktree branches carry unmerged work.

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

## Next: make `App` tab-ready before the first new tab

`App` (src/app.rs, 12.5k lines) is the app shell and the work-items screen in
one struct, and `AppMode`, `ScrollSurface`, `PointerTarget`, `TextEditor`, the
footer hints, `mode_name`, and `ui::render_pass` each enumerate every overlay.
A Repos tab added as-is would grow all of them. The proposed slice, to discuss
before starting: extract the work-items screen (tickets, search, filters,
sort, pickers, forms, edits, family cursor) into its own module with its own
mode enum, leaving `App` as the shell (tab, focus, pointer, notifications,
session, sync flags) that dispatches keys, mouse, and rendering to the active
tab. Split `app.rs` into a directory module at the same time. The six
near-identical picker cursors (`focus_*`/`move_*_selection`) would collapse
into one list-cursor type on the way through.

## What is left

Nothing on the backlog. The tab-ready split above is the next slice, once its
shape is agreed. Query round-tripping was the last
loose end and is fixed: `quote_if_needed` now escapes `\` and `take_quoted`
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
Doing, implement in an isolated worktree starting from `git merge main`, keep
fmt/clippy/test/release green, merge to `main`, verify there, set Done. Two or
three concurrent tickets are fine when they touch disjoint regions.

One lesson from this round: an agent's `git merge main` can report "already up
to date" against a stale `main` and still merge textually clean. Have each agent
verify the merge landed by grepping for a symbol the previous ticket added, and
always re-run the full suite after merging rather than trusting a clean
auto-merge.
