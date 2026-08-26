# ticket-tui product plan

## Goal

Turn the working SQLite ticket browser into a polished, fast, local-first
terminal workspace for finding, reviewing, grouping, and sharing ticket data.
Preserve the current read-only ticket model while making navigation and repeated
triage workflows substantially more capable.

## Product principles

- Keep SQLite as the runtime data boundary.
- Keep ticket records read-only inside the TUI.
- Prefer fast keyboard workflows without sacrificing mouse support.
- Preserve useful context across reloads, searches, view changes, and restarts.
- Remain usable in narrow, short, monochrome, and low-color terminals.
- Make expensive database and search work non-blocking.
- Add complexity in independently useful, tested increments.

## Out of scope

- Network service integration or synchronization.
- Authentication or credential storage.
- Remote ticket creation, editing, assignment, or state transitions.
- A built-in ticket-system API client.

The application may later import local, user-provided files, but it will not
connect to a remote ticket service.

## Status legend

- [ ] Planned
- [~] In progress
- [x] Complete

## Phase 1: interaction foundation

### Input and navigation

- [x] Replace permanent status text with transient, severity-aware
  notifications and restore contextual shortcuts after notifications expire.
- [x] Keep focus and the visible screen synchronized in narrow layouts.
- [x] Add full search-field editing: cursor movement, Home/End, Delete,
  word deletion, paste, and long-query horizontal scrolling.
- [x] Add search history with previous/next recall.
- [x] Bound detail scrolling to the rendered content and expose scroll position.
- [x] Add first/last and page navigation to long detail content.
- [x] Make help and future overlays scrollable in short terminals.
- [x] Replace continuous idle redraws with event-driven rendering.
- [ ] Move database reload and search-document rebuilding off the UI thread.

### Behavior clarity

- [x] Make the relationship between relevance ranking and field sorting
  explicit, and allow strict field sorting while a search is active.
- [x] Preserve ticket selection, table offset, detail position, and active pane
  whenever the underlying operation permits it.
- [ ] Make empty, loading, stale, and error states visually distinct.
- [ ] Add explicit confirmation or feedback for every mouse-only affordance.

## Phase 2: visual system and responsive layout

- [ ] Introduce a centralized theme with default, monochrome, and `NO_COLOR`
  behavior.
- [ ] Apply restrained semantic styling to ticket state and priority.
- [ ] Highlight matched portions of visible search results.
- [ ] Render work-item types and tags as compact badges.
- [ ] Right-align numeric columns and improve date formatting.
- [ ] Show relative changed times in the table and exact timestamps in details.
- [ ] Reorganize details into clear overview, description, relationships, and
  history sections when those data are available.
- [x] Add a detail scrollbar and bounded position indicator.
- [ ] Add a table scrollbar or equivalent position indicator.
- [x] Replace the narrow-layout `d` convention with a visible Tickets/Details
  tab treatment while retaining a fast shortcut.
- [x] Make the footer contextual to the current mode and focused pane.
- [ ] Add compact and comfortable row-density options.

## Phase 3: power browsing

### Filtering and views

- [ ] Add structured filters for state, type, assignee, priority, project, area,
  iteration, and tags.
- [ ] Support a concise query grammar such as `state:active priority:1`.
- [ ] Display active filters as removable filter chips.
- [ ] Add a filter/facet overlay with value counts.
- [ ] Save and restore named views containing filters, search, sorting, columns,
  and layout preferences.
- [ ] Persist the last active view and session state.

### Table configuration

- [ ] Let users show, hide, reorder, and resize columns.
- [ ] Add columns for organization, project, area, iteration, created date, and
  tags.
- [ ] Persist table configuration per saved view.
- [ ] Add a command palette for actions that do not merit global shortcuts.

### Personal workflow

- [ ] Add local bookmarks independent of ticket data.
- [ ] Add recently viewed tickets and quick navigation back/forward.
- [ ] Add copy actions for ID, URL, title, Markdown link, and ticket summary.
- [ ] Add multi-select and export selected tickets as IDs, URLs, Markdown, JSON,
  or CSV.

## Phase 4: richer local data

- [ ] Add optional local JSON and CSV import commands with validation and clear
  diagnostics.
- [ ] Add a true `--read-only` database mode that performs no migration,
  journaling change, directory creation, or seeding.
- [ ] Detect local SQLite changes and reload automatically.
- [ ] Show database path, last load time, row count, and data freshness.
- [ ] Extend the versioned schema for parent, child, related, predecessor,
  successor, and duplicate relationships.
- [ ] Display ticket relationships as a compact tree or linked list.
- [ ] Support locally supplied history and comment records without editing them.

## Phase 5: scale, accessibility, and distribution

- [ ] Benchmark search, reload, sorting, and rendering with 10,000, 50,000, and
  100,000 tickets.
- [ ] Move filtering or pagination into SQLite where measurements justify it.
- [ ] Parse and normalize timestamps rather than relying on lexical ordering and
  fixed string slices.
- [ ] Add render snapshots for narrow, stacked, wide, short, long-text, Unicode,
  monochrome, empty, loading, and error states.
- [ ] Add end-to-end keyboard and mouse workflow tests.
- [ ] Add diagnostic file logging outside the alternate terminal screen.
- [ ] Validate schema compatibility before loading rows and report actionable
  migration errors.
- [ ] Publish versioned macOS and Linux binaries with checksums and release
  notes.

## Implementation order

Work through the phases in order, but ship coherent checkpoints within each
phase. A checkpoint must leave formatting, Clippy, tests, and the release build
passing. Update this document as items move from planned to in progress or
complete; do not mark an item complete based only on a partial UI stub.
