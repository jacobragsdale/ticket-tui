# Where to pick up

Last updated 2026-08-29. The backlog itself lives in Azure DevOps
(`jacobragsdale/development`); this file is only the pointer into it. Run
`ticket-tui` and browse the epics for the current state.

## State of `main`

- Every implementable ticket in the roadmap is merged and Done. `cargo fmt
  --check`, `cargo clippy --all-targets --all-features -D warnings`, `cargo test
  --all-targets` (413 tests, and again under `NO_COLOR=1`) and `cargo build
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

Agents (Epic 624, all but #643): context JSON v2 with a `sync` block and
`pending_edits` (#625), CLI subcommands `sync|show|list|edit|comment|create`
(#626), the rewritten `ticket-tui-context` skill (#627).

Manager workflows (Epic 628, closed): built-in views with `@me` / `@none` /
`@current` / `@open` sentinels (#629), sprint summary overlay (#630), stale
highlighting sharing #649's predicate (#631), child progress (#648), `changed:`
and `created:` date filters (#649), finished tickets hidden by default (#652).

Also: the CLI now resolves query sentinels through the same `MatchContext` the
TUI uses, and refuses one it cannot resolve rather than returning an empty list.
`ticket-tui sync` caches the classification trees, which only opening a picker
used to do.

## What is left

1. **#643 in-TUI agent chat pane** — parked pending a decision. It is the only
   thing holding Epic 624 open; build it, or move it out and close the epic.
2. **#654 backslashes do not survive `format_query`/`parse_query`** — found
   while building #630. `quote_if_needed` quotes but does not escape `\`, and
   `take_quoted` treats `\` as an escape, so `iteration:"development\Sprint 1"`
   parses back as `developmentSprint 1`. Every iteration and area facet toggle
   hits this; it is masked because a bare leaf also matches.

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
