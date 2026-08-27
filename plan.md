# Mouse-first polish plan

## Goal

Make `ticket-tui` feel predictable and complete when used primarily with a
mouse, while preserving every existing keyboard binding and keeping the
application a focused, read-only ticket browser.

This work is interaction polish, not a product expansion. The finished app
should let a user point, click, scroll, select, copy, and paste without first
learning terminal-specific workarounds. Keyboard and mouse routes must invoke
the same application commands so neither path becomes second class.

## Scope guardrails

- Keep mouse capture enabled. Do not restore the reverted environment-specific
  `--mouse`/`--no-mouse` behavior or make users choose between in-app mouse
  controls and copying text.
- Preserve all current shortcuts, focus behavior available through `Tab`, and
  command-palette actions. Mouse support is additive.
- Keep the existing responsive layouts and minimum terminal size of `36 x 10`.
- Keep macOS and Linux support and avoid terminal-emulator-specific behavior as
  the primary interaction path.
- Reuse the current `AppAction`, command palette, clipboard commands, rendering
  theme, and test backend where practical.
- Prefer a small typed interaction layer over scattered new coordinate checks.
- Do not add ticket editing, remote service integration, context menus,
  tooltips, dashboards, new persistence formats, or other unrelated features.
- Do not make right-click, double-click timing, or horizontal wheel gestures
  required. Their terminal support is too inconsistent for core behavior.
- Do not add page-per-wheel-notch ticket movement. That behavior was previously
  added and reverted; wheel movement should be fine-grained and predictable.

## Current-state findings

The codebase already has a useful mouse foundation:

- `src/main.rs` enables crossterm mouse capture and bracketed paste, routes
  `Event::Mouse` and `Event::Paste`, and restores terminal state on exit.
- `src/app.rs` handles left clicks and wheel events. It can select ticket rows,
  open underlined IDs and URLs, sort headers, operate filters, choose overlay
  rows, and scroll details and help.
- `src/ui.rs` records hit rectangles during rendering and displays scrollbars,
  underlined links, filter pills, removable chips, and selected rows.
- `src/export.rs` and the command palette already copy ticket IDs, URLs, titles,
  Markdown links, and summaries through platform clipboard commands.
- Existing `TestBackend` and application tests cover several click regions,
  scrolling bounds, paste sanitization, copying selected tickets, and responsive
  layouts. The current baseline is 95 passing tests across the library and
  binary targets.

The remaining friction comes from incomplete interaction semantics rather than
missing product capabilities:

1. Mouse capture prevents ordinary terminal drag selection, while the app has
   no internal text-selection model. Copy commands exist, but arbitrary visible
   text cannot be selected with the mouse.
2. Paste is accepted only by search and import prompts. The palette query and
   named-view editor are also text fields but ignore paste events.
3. Wheel input changes the selected ticket and keyboard focus instead of
   scrolling the hovered viewport independently, which differs from common GUI
   list behavior.
4. Overlay scrolling is inconsistent. Filter hit regions are based on unscrolled
   row positions, the facet menu can move its selection beyond the clipped
   viewport without scrolling the content, and several overlays share generic
   scroll state even when their rows are not rendered with that offset.
5. Scrollbars are visual indicators only. Their tracks and thumbs look
   interactive but cannot be clicked or dragged.
6. Several controls are available only by keyboard even though the underlying
   command already exists. Examples include opening the command palette,
   closing most overlays, changing column order or width, saving or deleting a
   named view, and switching the narrow Tickets/Details view.
7. A click on a table ID immediately opens the browser on mouse-down, and other
   row actions also fire before the app can know whether the user intended to
   drag. This conflicts with introducing text selection safely.
8. Click rectangles are stored by widget category rather than by action. This
   duplicates routing rules, makes overlay z-order implicit, and makes clipped
   or scrolled rows easy to map incorrectly.

## Target interaction contract

### Pointer routing

- The topmost visible surface owns the pointer. An open popup or menu receives
  clicks and wheel events before the obscured application beneath it.
- Clicking a control focuses or activates it. Scrolling a surface does not move
  keyboard focus merely because the pointer passed over that surface.
- A press begins a possible click or drag. A control activates on left-button
  release only when the pointer has not become a text-selection or scrollbar
  drag. Dragging across a link must never open it accidentally.
- Only visible, clipped content receives hit regions. A row scrolled out of a
  popup must not remain clickable at its old coordinate.
- Clickable text has a stable visual affordance: links stay underlined; buttons
  use a consistent bracketed treatment; checkboxes look like checkboxes; column
  headers retain sort arrows. Do not rely on color alone.
- Track the hovered target and apply a restrained hover style to rows, links,
  buttons, and scrollbar parts. Redraw only when the resolved hover target
  changes, not for every raw pointer-motion event.

### Scrolling

- The wheel scrolls the scrollable region under the pointer:
  - ticket body: three logical ticket rows per notch;
  - details body: three rendered lines per notch;
  - help and list overlays: three visible rows per notch.
- Wheel movement clamps at both ends and does nothing when the hovered surface
  has no overflow.
- Ticket scrolling changes the table viewport, not the selected ticket. The
  details pane therefore remains stable while browsing past it with the wheel.
- Keyboard selection continues to move the current ticket and automatically
  brings that ticket into view. A mouse click selects the clicked ticket and
  likewise ensures it is visible.
- Compact and comfortable table density use logical ticket rows, so one wheel
  notch moves the same number of tickets in either mode.
- Each scrollable widget owns its offset and maximum. Opening or changing an
  overlay initializes or clamps that overlay's offset instead of leaking a
  previous overlay's scroll position.
- Visible vertical scrollbars support:
  - wheel scrolling anywhere in their owning surface;
  - a track click above or below the thumb for one viewport-minus-one step;
  - left-button thumb dragging with proportional, clamped movement.
- Scrollbar interaction does not change the selected ticket or active overlay
  row unless that row must be brought into view by a subsequent keyboard action.

### Text selection, copy, and paste

- Left-button dragging over selectable rendered text creates an in-app
  selection while mouse capture remains enabled.
- Selection is limited to one visible text surface at a time: search, table,
  details, help, informational popup, or text-bearing list popup. Crossing into
  another surface clamps the selection to its origin surface.
- Render the selection with a theme-aware reverse or selection style. Preserve
  enough contrast in normal color and `NO_COLOR` modes.
- Releasing a non-empty drag copies the selected visible plain text through the
  existing `AppAction::Copy` clipboard path and shows the existing success or
  error notification.
- Copy the text the user can see, in reading order. Join selected screen rows
  with newlines, omit borders and scrollbar glyphs, trim only layout padding,
  and preserve meaningful internal spaces and Unicode. A selection spanning
  wrapped details text should produce readable lines rather than ANSI styling
  or terminal-cell artifacts.
- A drag never changes table selection, toggles a filter, sorts a column, jumps
  a relationship, or opens a URL. A simple click retains those behaviors.
- Keep semantic copy commands for complete underlying values that may be
  truncated on screen. Add a single visible `Copy` button in the details title
  area that opens the command palette prefiltered to its existing copy actions.
  Do not create a separate context-menu system.
- Keep `y` as the quick copy-ID shortcut and retain all existing copy commands.
- Route bracketed paste to every active text editor:
  - search query;
  - command-palette query;
  - import path;
  - named-view name.
- Sanitize pasted input according to the destination. Search and palette input
  convert newlines and tabs to spaces; single-line names and paths discard
  control characters. Paste at the current cursor for editors that expose a
  cursor, not always at the end.
- Clicking inside search places the caret at the nearest character after
  accounting for borders and horizontal scroll. Clicking the palette, import,
  or view-name field focuses that editor and places the caret when the field has
  explicit cursor support.

### Click parity for existing capabilities

- Search:
  - clicking the field enters search mode and places the caret;
  - show a clickable clear button only when a query is present;
  - retain `/`, editing keys, `Enter`, and `Esc` unchanged.
- Application actions:
  - add compact `Actions` and `?` buttons in unused search-block title space;
  - `Actions` opens the existing command palette and `?` opens help;
  - hide or abbreviate these buttons when width is insufficient rather than
    shrinking the search input below a usable size.
- Ticket table:
  - a plain row click selects the row;
  - the underlined numeric ID remains the explicit open-in-browser target;
  - render the bookmark marker separately from the ID underline and make the
    marker toggle the bookmark rather than open the browser;
  - provide a small selection checkbox target per row so existing multi-select
    is usable without the keyboard; keep `Space` unchanged;
  - retain clickable sort headers and make their full visible cells active.
- Details:
  - URL and family-ticket links remain clickable;
  - the details title-area `Copy` button exposes existing copy formats;
  - clicking non-control details content focuses the details pane without
    scrolling or activating a link.
- Responsive tabs:
  - replace instructional-looking narrow titles with clear `[Tickets]` and
    `[Details]` tab targets;
  - clicking either tab changes the visible narrow pane;
  - `Tab` and `d` continue to work exactly as they do now.
- Filters:
  - pills open their value menus, checkboxes toggle values, and `x` chips remove
    only the represented token;
  - a click outside the small facet menu closes it without changing the query;
  - the full filter overlay gets a visible close button and correct scrolled-row
    mapping.
- Sort popup:
  - clicking a field applies the same default/toggle direction rules already
    used by clickable table headers;
  - provide explicit ascending and descending targets for the active row;
  - add a close button and preserve all arrow/Enter behavior.
- Column popup:
  - keep row checkboxes clickable for visibility;
  - add compact up, down, narrower, and wider targets on each applicable row,
    invoking the existing `move_column` and `resize` operations;
  - title remains fixed-width and non-resizable, matching the existing keyboard
    rules;
  - add a close button.
- Command palette:
  - clicking a command runs it, as today;
  - make the query portion a recognizable text field with click and paste;
  - scroll long result lists and keep clicked rows aligned with their visible
    indices;
  - add a close button.
- Named views:
  - clicking a view loads it, as today;
  - expose `Save current` and `Delete selected` buttons for the existing actions;
  - make the name editor clickable and paste-aware;
  - add close and cancel buttons without removing `V`, `n`, `d`, `Enter`, or
    `Esc`.
- Help, database info, and import prompt:
  - add a visible close button to each;
  - make help's scrollbar interactive;
  - add `Import` and `Cancel` buttons to the existing import prompt;
  - do not close a modal merely because the user clicked selectable text inside
    it.

## Implementation plan

### Phase 1: replace ad hoc hit regions with typed targets

1. In `src/app.rs`, replace category-specific vectors in `HitRegions` with a
   rendered list of `PointerRegion { rect, target, layer, selectable_surface }`
   or an equivalent typed structure. `PointerTarget` should encode the command
   to perform rather than require another chain of widget-specific coordinate
   checks.
2. Include targets for controls, table rows, links, modal backgrounds,
   scrollable surfaces, scrollbar tracks/thumbs, text fields, and selectable
   text surfaces. Keep data payloads such as row index, `TicketKey`,
   `FilterToken`, `SortField`, or overlay index in the target.
3. Register regions in `src/ui.rs` only after layout and clipping are known.
   Resolve overlaps by explicit layer and reverse paint order so popups cannot
   click through to the table.
4. Introduce pointer state in `App` for pressed target, hover target, drag kind,
   and drag origin. Move click activation from `Down(Left)` to `Up(Left)` after
   confirming no drag occurred.
5. Change mouse handling to report whether the visible state changed. Update the
   event loop in `src/main.rs` so repeated pointer movement over the same target
   does not force needless redraws.
6. Preserve the current keyboard dispatch untouched except where both inputs
   are redirected into a shared command method.

Verification for this phase:

- Unit-test topmost-region resolution, border coordinates, clipped rows, and
  press/drag/release cancellation.
- Assert that dragging from a link does not return `OpenUrl` and that a simple
  click still does.
- Render at widths 36, 69, 70, 109, and 110 to catch breakpoint hit-region
  errors.

### Phase 2: make scrolling viewport-based and consistent

1. Give the ticket list an explicit viewport offset separate from its selected
   row. If ratatui's `TableState` insists on scrolling the selection back into
   view, render a viewport slice with a render-local state rather than coupling
   user selection to widget offset.
2. Add shared clamped-scroll helpers for line, logical-row, page, and
   proportional scrollbar movement. Use the same helpers from wheel, keyboard,
   track-click, and drag paths where their semantics match.
3. Route wheel events to the scrollable surface under the pointer without
   changing `Focus`. Preserve current keyboard rules that bring the selected or
   focused row into view.
4. Give facet, filter, column, palette, views, help, and details surfaces
   explicit offsets and viewport sizes. Keep a selected overlay row visible
   during keyboard movement, but let pure wheel scrolling leave selection
   unchanged.
5. Build row hit targets from `logical_index = offset + visible_index`. Do not
   register rows outside the clipped inner rectangle.
6. Register scrollbar track and thumb geometry during render, then implement
   page clicks and proportional thumb dragging. Recalculate drag position after
   terminal resize.

Verification for this phase:

- Wheel-scroll each surface at its start, middle, and end and assert bounds.
- Assert table wheel movement does not change selected ticket, details content,
  `Focus`, or narrow-pane choice.
- Test both row densities and tables shorter and longer than one viewport.
- Scroll every overlay, click its first and last visible rows, and assert the
  logical item—not its former screen position—receives the action.
- Test scrollbar track clicks, thumb drags, zero-overflow widgets, and resize
  during/after a drag.

### Phase 3: add in-app selection and complete clipboard input

1. During `src/ui.rs` rendering, record a plain-text snapshot for each
   selectable surface alongside its screen rectangle. Derive it from the final
   rendered cells or a parallel text layout, never from ANSI output. Mark
   borders, padding, and scrollbar cells as non-copyable.
2. Add selection anchor and extent state. On left drag inside one surface,
   convert terminal cell coordinates into ordered text positions, including
   wrapped lines and wide Unicode cells.
3. Render the selection after the base widget but before the terminal frame is
   presented. Clear it on a new click, surface replacement, resize, or modal
   transition.
4. On release, normalize the selected visible text and return
   `AppAction::Copy`. Reuse `copy_to_clipboard` and its current notification and
   error handling.
5. Add an `open_copy_actions` helper that opens the existing palette with
   `"copy"` as its query. Connect it to a visible details `Copy` target; do not
   duplicate clipboard format logic.
6. Generalize `handle_paste` by active text editor. Add cursor-aware insertion
   to palette, import, and view-name state where needed, while leaving their
   current keyboard shortcuts intact.
7. Add click-to-caret mapping for all visible text inputs. Clamp clicks in
   padding or past the rendered value to the nearest valid character boundary.

Verification for this phase:

- Drag-select and copy one cell, a partial line, multiple rows, wrapped details,
  highlighted search text, and Unicode containing wide characters.
- Assert copied text contains no ANSI codes, borders, scrollbar characters, or
  layout-only trailing padding.
- Assert a click still activates a control, a drag never does, and a zero-width
  drag produces no clipboard action.
- Paste multiline and Unicode content into every editor and verify sanitization,
  insertion point, cursor position, and cancel/submit behavior.
- Exercise clipboard success, missing-command failure, and nonzero clipboard
  command exit without logging copied contents.

### Phase 4: expose existing actions through restrained controls

1. Add shared render helpers for buttons, close targets, checkboxes, and tabs so
   visual and hit rectangles cannot drift apart.
2. Add `Actions`, help, query-clear, details-copy, narrow tabs, row bookmark and
   multi-select targets, and modal close buttons.
3. Add only the overlay-specific controls required for mouse parity: sort
   direction, column move/resize, view save/delete, and import/cancel.
4. Route each control into the same method or `CommandId` used by its keyboard
   equivalent. Do not create parallel mouse-only business logic.
5. Apply hover styles and disabled styles. Disabled controls must remain visible
   when their placement is stable but must not register active hit targets.
6. Update contextual footer/help text and `README.md` to describe mouse
   scrolling, drag-to-copy, clickable actions, and paste behavior without
   replacing the keyboard control table.

Verification for this phase:

- For every visible button, test its action and its disabled state.
- Compare keyboard and mouse paths for sort, filters, columns, views, copy,
  import, help, and responsive tab switching.
- Confirm click targets do not overlap at minimum width and abbreviate or hide
  in the documented priority order.
- Confirm all controls remain legible and selected/hovered/disabled states are
  distinguishable with `NO_COLOR`.

### Phase 5: end-to-end regression pass

1. Add a small table-driven mouse workflow suite using crossterm events and the
   ratatui `TestBackend`. Cover wide, stacked, narrow-table, and narrow-details
   layouts.
2. Retain all existing keyboard tests and add explicit assertions for the
   shortcut paths touched by shared command refactors.
3. Run the full repository gate:

   ```console
   cargo fmt --all -- --check
   cargo clippy --all-targets --all-features -- -D warnings
   cargo test --all-targets
   cargo build --release
   ```

4. Perform manual terminal checks on macOS and Linux. At minimum, use one
   mainstream terminal on each platform and include Ghostty/Herdr on macOS
   because the repository history identifies copy-on-select there as a real
   workflow. Verify actual clipboard contents rather than relying only on an
   `AppAction::Copy` assertion.
5. Manually test slow drags, fast drags, wheel bursts, track clicks, terminal
   resize, modal overlap, and quitting after a drag to confirm terminal mouse
   and paste modes are always restored.

## Suggested delivery checkpoints

Implement and verify these as independent, reviewable checkpoints:

1. Typed pointer targets and release-based click dispatch.
2. Correct hovered-surface wheel scrolling and overlay row mapping.
3. Clickable and draggable scrollbars.
4. In-app visible-text selection and clipboard copy.
5. Paste and click-to-caret parity for all text editors.
6. Existing-command buttons and overlay mouse parity.
7. Documentation and end-to-end regression coverage.

Each checkpoint must leave existing keyboard behavior green. If a shared input
refactor cannot preserve a shortcut, stop and fix that regression before adding
the next mouse affordance.

## Completion criteria

The optimization is complete when all of the following are true:

- Every existing keyboard binding still performs its documented action.
- A mouse-only user can search, paste, select tickets, multi-select, bookmark,
  filter, sort, configure columns, use named views, copy ticket information,
  open links, inspect help/info, import a file, switch responsive panes, and
  close every popup.
- Dragging visible text copies it without disabling mouse capture and without
  accidentally activating the text underneath.
- The wheel scrolls the hovered table, details pane, help, or overlay smoothly,
  with correct bounds and no unintended focus or ticket-selection changes.
- Every rendered scrollbar works as an indicator, track, and draggable thumb.
- Every element styled as a link, button, checkbox, tab, chip, or sortable
  header is clickable over exactly its visible bounds.
- Scrolled and clipped overlay rows activate the correct logical item.
- Copy and paste handle Unicode and never expose clipboard contents in logs or
  error messages.
- Wide, stacked, narrow, short, and `NO_COLOR` layouts remain usable.
- Formatting, Clippy, all tests, and the release build pass on macOS and Linux.

## Explicitly deferred

- Right-click context menus and terminal-specific popup menus.
- Double-click-to-open behavior.
- Selecting text that is offscreen or across multiple panes in one drag.
- Copying rich formatting or ANSI styles.
- Horizontal table scrolling or trackpad gesture recognition.
- Drag-and-drop column reordering; the small up/down buttons provide mouse
  parity with substantially less interaction complexity.
- Dragging files into the import prompt; ordinary path paste remains supported.
- Any ticket mutation or new Azure DevOps capability.
