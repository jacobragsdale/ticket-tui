# Ticket tree interface plan

## Outcome

Turn the existing `Family` section in ticket details into a clean, compact,
always-expanded tree for exploring parent and child work items. A user should
be able to understand where the selected ticket sits and move to another family
ticket without losing orientation.

This is a focused interface improvement, not a new product area. Keep the tree
inside the existing details pane and build on the relationship, scrolling,
focus, pointer, and navigation code already in the repository.

## Scope guardrails

- Keep the application read-only and offline.
- Reuse `TicketGraph`, `FamilySnapshot`, the details pane, `Focus`,
  `PointerTarget`, `ScrollSurface::Details`, and `jump_to_ticket`.
- Add no dependency, database migration, session-file field, separate tree
  screen, modal, horizontal scrolling, or generic widget framework.
- Keep non-hierarchical relations in the existing `Links` section. Do not try
  to turn related, predecessor, successor, or duplicate links into tree edges.
- Treat the hierarchy as a deterministic tree projection. Continue using the
  existing primary-parent rule for the main branch and show additional parents
  separately; do not build a general-purpose directed-graph viewer.
- Keep the tree always expanded; do not add fold state to the app or session.
- Preserve ticket filtering. A tree row may describe a loaded ticket hidden by
  the current query, but navigation must not silently clear the query.
- Preserve current responsive layouts and the `36 x 10` minimum terminal size.
- Prefer extending the existing model, app, UI, and pointer types over creating
  a new module.

## Current foundation

The repository already contains most of the required machinery:

- `src/model.rs` normalizes parent and child edges in either direction, chooses
  a stable ancestor chain, prevents ancestor cycles, and produces the current
  static `FamilyTreeEntry` rows.
- `src/app.rs` stores details focus and scroll state, highlights a family link,
  jumps to visible related tickets, records recent-ticket history, and explains
  when a related ticket is hidden by the active filter.
- `src/ui.rs` renders a `Family` section with tree connectors, selected and
  focused styling, clickable IDs, a breadcrumb summary, and the normal details
  scrollbar.
- `src/pointer.rs` already resolves layered typed hit regions and distinguishes
  a row action from scrolling or text selection.

The change should replace the static ancestor/sibling/immediate-child snapshot
with a flattened view of the complete projected tree. It should not replace
these supporting systems.

## Target experience

The tree remains the first section of the scrollable details body. At a useful
width it should read approximately like this:

```text
Family · 2/5 closed
  10001  Feature     Authentication rewrite
  ├─ 10002  User Story  Login form                 current
  │ └─ 10004  Task       Validate email
  └─ 10003  User Story  Logout
```

Use the existing connector glyphs where branches need `├─`, `└─`, and `│`.
Do not render disclosure or leaf markers because the tree has no folded state.

The globally selected ticket and the keyboard cursor are different concepts.
Label the selected row with `current` and render it in bold. Render the family
cursor with the existing selected-background treatment. Do not rely on color
alone to distinguish either state.

Show ID, work-item type, and title when they fit. Protect the connector and ID
first; truncate the title before removing structural information. At narrow
widths, omit type details before the title. Do not wrap a tree row because
wrapping makes branch connectors and mouse targets ambiguous.

The breadcrumb above the details body should stay compact: primary parent,
`this`, and direct-child completion. It remains a summary.

## Interaction contract

### Focus

Add `Focus::Family` as a small extension to the existing focus enum.

- `Tab` cycles `Tickets -> Family -> Details -> Tickets` when the selected
  ticket has a family tree.
- Skip `Family` when the selected ticket has no parent or child rows.
- In the under-70-column layout, both `Family` and `Details` show the details
  screen; returning to `Tickets` shows the ticket table again.
- Treat the details border as focused for either `Family` or `Details`, and add
  the stronger focus cue to the `Family` heading only while `Focus::Family` is
  active.
- Clicking a tree row gives focus to `Family`. Clicking ordinary details
  content gives focus to `Details` as it does now.

This keeps ordinary details scrolling unchanged while giving the tree standard
navigation keys. It avoids a new application mode or popup.

### Keyboard behavior

While `Focus::Family` is active:

| Input | Behavior |
|---|---|
| `Up` / `k` | Move the cursor to the previous visible tree row |
| `Down` / `j` | Move the cursor to the next visible tree row |
| `Enter` | Select the cursor ticket in the main table and keep family focus |
| `Home` / `End` | Move to the first or last visible tree row |
| `Page Up` / `Page Down` | Move by the number of visible family rows, clamped at either end |
| `Tab` | Continue to ordinary details focus |
| `o` | Keep the existing behavior of opening the globally selected ticket |

Additional rules:

- Moving the cursor never changes the globally selected ticket or recent-ticket
  history. Only `Enter` or a row click performs the existing ticket jump.
- Keep the cursor row visible by adjusting the existing `details_scroll`; do
  not introduce a second scrollbar for the tree.
- If the target ticket is hidden by search or filters, keep the current table
  selection and query unchanged and reuse the existing explanatory
  notification.
- `Focus::Details` retains the current arrow, `j`/`k`, page, `Home`, and `End`
  scrolling behavior. Existing ticket-table keys remain unchanged under
  `Focus::Tickets`.
- The existing recent-ticket `[` and `]` history remains independent of tree
  navigation.

### Mouse behavior

- Clicking the rest of a family row performs the same selection as keyboard
  `Enter`, records the normal recent-ticket history, and keeps family focus.
- Clicking the already selected row only moves the family cursor and focus; it
  does not add a duplicate history entry.
- Give the entire visible row a hit target so a truncated title remains easy to
  select.
- Wheel input over the tree continues to scroll `ScrollSurface::Details` by the
  existing amount without moving the family cursor or changing focus.
- Do not paint a family row as selected on pointer hover. A drag that begins on
  a tree row remains text selection rather than a click.
- Do not add double-click, right-click, hover tooltips, drag-to-reparent, or
  horizontal gestures.

## Technical design

### 1. Produce visible tree rows from the graph

Evolve `FamilyTreeEntry` in `src/model.rs` into the complete render contract for
one flattened row. It should carry only data that the app and UI need:

```rust
pub struct FamilyTreeEntry {
    pub key: TicketKey,
    pub prefix: String,
    pub is_current: bool,
}
```

Add a graph helper with the equivalent of:

```rust
TicketGraph::visible_family_tree(
    current: &TicketKey,
) -> Vec<FamilyTreeEntry>
```

Its behavior should be:

1. Use the existing ancestor walk to find the highest primary ancestor for the
   selected ticket. When there is no parent, the current ticket is the root.
2. Emit the root and recursively emit all sorted children.
3. Reuse the current deterministic key ordering and connector-prefix builder so
   renders do not jump between frames or reloads.
4. Mark `is_current` by key comparison; do not bake cursor styling into the
   model.
5. Keep the existing maximum depth of 16 and a path-local visited set. If an
   imported hierarchy cycles, omit the repeating edge and stop that branch
   rather than recursing or hanging.
6. Keep the main tree single-parent. The existing primary parent remains in the
   tree; `extra_parents` remain short rows after the tree and continue to be
   jumpable.
7. Keep missing tickets visible as `missing ticket` and disable navigation to
   them. Continue through their known children so stored structure is visible.

Do not add a persistent adjacency cache in this pass. The current in-memory
dataset and details-only rendering make the existing graph scans acceptable;
the implementation can remain a small pure projection that is easy to test.

### 2. Store minimal interaction state in `App`

In `src/app.rs`, replace the index-based `link_cursor` with
`family_cursor: Option<TicketKey>` so the cursor remains attached to a ticket
across reloads.

Add small helpers rather than distributing index arithmetic through key
handling and rendering:

- `visible_family_tree()` delegates to the model projection for the selected
  ticket.
- `reset_family_cursor()` places the cursor on the selected ticket.
- `move_family_cursor(delta)` moves within the flattened visible rows and
  clamps rather than wrapping.
- `ensure_family_cursor_visible()` maps the flattened row index to the existing
  details line offset and adjusts `details_scroll` with the current viewport
  bounds.

Call the reset helper after table selection, recent-history navigation, reload
selection restoration, and successful relationship jumps. Do not mark the
session dirty for cursor changes.

The `Family` section is the first section of the scrollable details body, so
its line position can be calculated as the section heading plus the flattened
row index. Keep that simple ordering instead of adding render-coordinate state
to `App`.

### 3. Route keys through the new family focus

Update the focused movement code in `src/app.rs` instead of adding another
application mode:

- extend `move_focused`, `Home`, `End`, `Page Up`, `Page Down`, and `Enter` for
  `Focus::Family`;
- update `toggle_focus` to skip `Family` when there is no hierarchy;
- consider `Family` part of the details screen in narrow-layout focus helpers;
- keep `jump_to_ticket` as the only path that changes the table selection from
  a relationship row.

Remove the old flat `family_jump_targets`, `cycle_family_link`, and
`jump_focused_family_link` methods once their callers use the visible tree and
key-based cursor. Keep other relation rows clickable; this plan does not need a
second keyboard cursor for the non-hierarchical `Links` section.

### 4. Render tree and focus states

In `src/ui.rs`:

1. Render `visible_family_tree()` rather than `FamilySnapshot::tree_entries()`.
2. Build each row from connector prefix, underlined ID, optional type, truncated
   title, and selected label.
3. Apply current-ticket bold styling first, then family-cursor background so a
   row can visibly be both selected and focused. Add `REVERSED` to the cursor
   row under `NO_COLOR`, where a reset background alone is not distinguishable.
4. Give the `Family` section heading an accent/focus marker only when
   `Focus::Family` is active.
5. Preserve the existing `Links`, planning, history, comments, and description
   sections after the variable-height tree.
6. Continue deriving detail link coordinates after applying
   `details_scroll`, so hidden or clipped rows never receive mouse targets.
7. Update the footer and help overlay with short, context-sensitive hints. For
   example: `↑↓ move  Enter select  Tab details`.

Continue using `PointerTarget::JumpToTicket` for each tree row and
`ScrollSurface::Details` for its scroll ownership. Exclude tree rows from
generic hover painting while preserving press, release, drag cancellation, and
layered-target behavior.

### 5. Keep responsive behavior deliberate

- Wide and medium layouts keep the tree inside the current details pane.
- Narrow details mode gives structural glyphs and IDs priority over metadata.
- Tree content may extend below the viewport and uses the existing details
  scrollbar.
- A terminal resize rerenders and reclamps details scroll without changing the
  key-based cursor.
- Very short panes may show only part of the tree; keyboard cursor movement must
  scroll it into view.

## Verification plan

### Model tests

Add focused tests in `src/model.rs` for:

- fully expanded descendants with stable connectors and key order;
- primary versus additional parents;
- missing tickets;
- cyclic relations and the depth limit;
- parent-only, child-only, and mirrored relation records continuing to
  normalize to the same hierarchy.

### Application tests

Add tests in `src/app.rs` for:

- the selected tree always including every descendant;
- conditional `Tab` focus order with and without a family;
- cursor movement clamping at the first and last visible row;
- `Enter` selecting a visible ticket and recording history once;
- a filtered-out target preserving the query and current selection while
  showing the existing notification;
- selection and cursor restoration after reload;
- family cursor movement bringing the row into the details viewport;
- family navigation not setting `session_dirty`.

### Render and pointer tests

Extend `src/ui.rs` tests to cover:

- connector, current, and cursor rendering without fold markers;
- all descendants appearing in the buffer and hit regions;
- narrow truncation preserving connectors and IDs;
- a row click selecting its ticket;
- pointer hover not highlighting family rows;
- correct hit targets after details scrolling;
- wheel scrolling leaving cursor and focus unchanged;
- selected and focused styles remaining distinguishable under `NO_COLOR`.

Render representative cases at widths `36`, `60`, `72`, `110`, and `130`, and
include a short-height case that forces details scrolling.

### Repository checks

After implementation, update the control table and database relationship text
in `README.md`, then run:

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```

## Completion criteria

The work is complete when:

- parent and child structure is legible without opening another screen;
- the full projected tree remains visible without fold controls;
- the cursor, current ticket, and details scroll remain stable and visually
  distinct;
- navigating a tree row uses the existing table selection, filter protection,
  and recent-history behavior;
- non-hierarchical links and ordinary detail scrolling still work;
- responsive and monochrome renders remain usable;
- the implementation adds no dependency, persistent state, database change, or
  generalized tree framework; and
- formatting, linting, tests, and release build all pass.
