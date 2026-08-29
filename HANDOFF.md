# Where to pick up

Last updated 2026-08-29. The backlog itself lives in Azure DevOps
(`jacobragsdale/development`, Epics 603–628); this file is only the pointer
into it. Run `ticket-tui` and browse the epics for the current state.

## State of `main`

- Everything merged through #645; `cargo fmt --check`, `cargo clippy
  --all-targets --all-features -D warnings`, `cargo test --all-targets`
  (230 tests) and `cargo build --release` are clean.
- Database schema is `PRAGMA user_version = 11`. The first launch of this build
  rebuilds any older database and does one full pull automatically; no `--sync`
  needed.
- Nothing is in the Doing state. No open worktree branches carry unmerged work
  (the `.claude/worktrees/agent-*` directories left on disk are stale copies of
  commits already on `main` and can be removed with `git worktree remove`).

## Shipped since the Sprint 1 start (2026-08-28 → 29)

Read path (Epic 603, complete): background refresh with `--refresh` and
`r` = sync (#608), incremental pulls with a ChangedDate watermark (#606),
comments and history — eager for changed items, lazy on selection (#607),
structured HTML rendering plus the raw `description_html` column (#610),
org/project recorded and guarded, `--query` sync scope, `TICKET_TUI_REFRESH`
(#609), throttling backoff honouring `Retry-After` (#645).

Editing (Epic 611): write-through dispatcher with `test /rev` guard and
revert-on-rejection (#612), `e` Edit menu and `S` state picker (#613), title /
priority / tags (#614), `a` assignee picker (#615), iteration and area pickers
with `current_iteration()` (#616), `$EDITOR` description editor with a Markdown
round-trip (#617), add a comment (#618), click any field value in the details
pane to open its editor as an anchored dropdown (#650).

Layout: scrollbar thumb painted from the hit-test geometry (#651); the details
pane scrolls as one paragraph with Description before History (#653).

## Next, in the order the epics list them

1. **#619 Bulk edit** for the checked set (state / assignee / iteration, one
   summary notification).
2. **#646 Undo the last edit** (`u`).
3. **#621 New work item form** → #622 new child (`n` / `N`) → #623 reparent.
4. **#625 context JSON v2** (sync status, pending edits) → **#626 CLI
   subcommands** (`ticket-tui sync|show|list|edit|comment|create`) → #627 skill
   rewrite.
5. **#649 date filters** (`changed:` / `created:`) → #629 built-in views →
   #631 stale highlighting → #648 child progress → #630 sprint summary.
6. Low priority: #652 hide finished tickets by default, #647 delete/recycle.
7. Parked pending a decision: #643 in-TUI agent chat.

Reserved keys for the above: `u` undo, `n` new, `N` new child. Everything else
editing-related goes through the `e` Edit menu or the palette.

## Working agreement

Each ticket carries Problem / Approach / Done-when. Flow: set the ticket to
Doing, implement in an isolated worktree starting from `git merge main`, keep
fmt/clippy/test/release green, merge to `main`, verify there, set Done. Two or
three concurrent tickets are fine when they touch disjoint regions; whoever
merges second re-merges `main` first and takes the higher `SCHEMA_VERSION`.
