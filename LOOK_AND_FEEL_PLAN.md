# Look-and-feel plan

Research (2026-08-29) into what polished Ratatui apps do, how ticket-tui looks
against them, and a sliced plan to close the gap without touching core
behaviour. Keys, hit regions, sync, editing and the database are out of scope;
everything below is paint, layout and theming.

## 1. What the ecosystem is doing

**Ratatui 0.30 itself** (we are on 0.30.2, so all of this is free):
`BorderType::Rounded` and the new dashed sets, `Block::padding`,
`Block::merge_borders(MergeStrategy::Exact)` for clean `┬`/`┼` joins where panes
meet, `Layout::spacing(Spacing::Overlap(1))` so two blocks share one border
column, `Rect::centered(...)` for overlays, `Style` constants
(`const S: Style = Style::new().cyan().on_black()`), `LineGauge` custom symbols.

**Theming, as the popular apps do it.** The pattern everywhere is *semantic
tokens, not colours in the code*, and *the default theme leaves the terminal's
own palette showing through*:

- **television** — one TOML per theme, ~18 tokens (`border_fg`, `text_fg`,
  `dimmed_text_fg`, `selection_bg/fg`, `match_fg`, `result_count_fg`, mode
  badges as fg/bg pairs). Background deliberately unset. Rounded/none borders,
  borderless input with a prompt glyph, status bar hidden by default.
- **atuin** — themes map "Meanings" (`Base`, `Title`, `Annotation`,
  `Important`, `AlertInfo`, `Guidance`…) to colours; `Base` is always unset so
  the terminal default shows. `style = auto` falls to a compact layout when the
  terminal is short.
- **yazi** — `theme.toml` per component (`[tabs]`, `[status]`, `[notify]`,
  `[confirm]`, `[which]`), powerline `{ open, close }` separators, and
  `[flavor] dark = "…" light = "…"` picking a flavour by terminal background.
- **gitui** — `theme.ron` of ~24 fields where you override only what you want;
  defaults are ANSI-16 names (`Blue`, `DarkGray`, `Reset`).
- **crates-tui** (the reference app the Ratatui team maintains) — Base16
  themes from TOML, async fetches never block the frame.
- Crates: `ratatui-themes` (15 palettes: Catppuccin, Tokyo Night, Gruvbox,
  Nord, Dracula…; `next()/prev()` cycling, serde, light/dark flag, targets
  0.30.2), `opaline` (39 themes, token → style → gradient pipeline, user theme
  discovery from `~/.config`, drop-in theme picker with live preview, 0.30),
  `terminal-colorsaurus` (OSC 10/11 "is this terminal dark or light?", with a
  timeout for SSH).
- The three-tier rule that keeps coming up: design in ANSI-16 first so SSH
  and the user's own palette work, then layer 256/truecolor themes on top;
  `NO_COLOR` must still read.

**Polish conventions in the well-regarded apps** (television, yazi, atuin,
gitui, bottom, rainfrog, serie, lazygit as the non-Rust reference):

- Rounded corners `╭─╮`, one shared border between neighbouring panes, padding
  inside panes, focused pane in the accent, everything else muted.
- A prompt glyph instead of a boxed search field; the input sits on one row.
- A status bar with segments: 3–5 context keys on the left, state on the
  right (`● Synced 2m`, spinner while busy). The `?` overlay carries the rest —
  "progressive disclosure".
- Modals dim what is behind them; buttons are filled pills, the primary one
  in the accent.
- Colour reinforces a hierarchy already carried by weight, position and
  glyphs (`● ○ ◐ ✓ ✗`, `▏▎▍▌▋▊▉█` for sub-cell bars); it never carries meaning
  alone.
- Spinners are braille (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`) at ~80–120 ms; nothing animates while
  idle. Effects libraries exist — `tachyonfx` (fade/coalesce/sweep composed
  through an `EffectManager` per frame), `throbber-widgets-tui`,
  `ratatui-braille-bar`, `ratatui-comfy-toaster` (positioned FIFO toasts) — but
  the big apps use them sparingly or not at all.

**X.com**: `@ratatui_rs` and `@orhundev` posts are mostly library launches —
braille bars, splash screens, a tree-sitter code editor, syntax highlighting,
PyRatatui — and app showcases (sdrrat's spectrum waterfall, ratisui). The
engagement goes to screenshots with rounded panes, a strong accent, and one
animated element. (X itself is not fetchable; this is from search excerpts.)

## 2. Where ticket-tui stands

Captured live at 140×42, 90×30 and 60×24 against the real database.

Already good: a semantic `Theme` struct with ~28 tokens; the `NO_COLOR`
monochrome theme with weight carrying every distinction; hover tint via a
256-colour background so coloured cells keep their foregrounds; the mouse
pointer shape follows what is under it; three layout breakpoints (110 / 70);
draggable divider; redraw-on-change event loop (no idle frames); Done rows
greyed as a whole; state/type/priority/tag colour code.

What holds the look back:

1. **The Title column starves.** At 140 columns with the default 50/50 split
   the tickets pane is 70 wide; ID 7 + State 10 + Type 13 + Pri 4 + Changed 10
   + marker 4 + spacing leaves Title 12 characters ("Serialize se"). At 90
   columns stacked it gets 15. The Changed cell also loses its unit under the
   scrollbar column ("1" instead of "1d"). Auto-hide is off once a layout is
   saved, so nothing gives way.
2. **Chrome is heavy.** Five rows sit above the table (3-row boxed search,
   facet row, chips row), plus a blank margin row under the table header.
   Where the two panes meet there are three columns of frame (`┐│┌`).
3. **Square single-line borders everywhere**, no padding, the same border for
   the search box, both panes, every overlay. Focus is cyan vs dark grey, which
   is right, but nothing else distinguishes a pane title from a section heading
   from a table header — all are bold accent.
4. **State lives in the wrong places.** Sync state (`Synced just now`,
   `Syncing…`, `Stale`) is inside the table title; the footer alternates
   between key hints and notifications, so a notification hides the hints.
5. **Overlays** are fixed-size (help is 78×18 on a 140×42 screen), the screen
   behind them is not dimmed, `[×]` / `[Approve]` / `[Delete]` are bracketed
   text, and the palette/edit menus are plain lists.
6. **One theme.** ANSI-16 plus `Indexed(237)` for hover. Nothing for a light
   terminal (DarkGray muted text is barely readable on white), no truecolor
   option, no runtime switch — `theme()` is a `OnceLock`.
7. **Only the repos glyph spins.** Search shows "(matching…)" in a title;
   sync, details fetch and the pipeline watcher show text or nothing.
8. **Details pane** is `Label: value` lines with section headings in the same
   bold accent as everything else; the identity line
   (`ID / Type / State: 600 · [Issue] · Done`) reads as prose rather than a
   badge row.

## 3. Principles for the changes

- The default theme stays ANSI-16 (`terminal`), so a user's palette shows
  through and SSH works. Named truecolor themes are opt-in.
- Every slice must still read under `NO_COLOR` — the test suite already runs
  under it.
- No key, hit region or behaviour changes. Paint and geometry only; where a
  rect moves (the search row), its hit region moves with it and the UI tests
  that address that rect are updated in the same slice.
- No idle animation. Spinners run only while something is in flight, on the
  wake-ups the loop already does (250 ms git, 33 ms search).
- One ADO issue per slice; each ships alone and is visible on screen.

## 4. The slices

Tracked in Azure DevOps as Epic #701: A #702 · B #703 · C #704 · D #705 ·
E #706 · F #707 · G #708 · H #709 · I #710.

### A. Theme engine v2 — foundation (do first)

Turn `Theme` into a switchable preset with a few more tokens, and make it
selectable.

- Add tokens: `border`, `border_focused`, `surface` (pill/button/selection
  background), `selection_fg`, `header`, `success`, `border_type`
  (`Rounded | Plain`), `dim_behind_modals: bool`.
- Presets: `terminal` (today's ANSI-16, default), `terminal-light` (ANSI-16
  with `muted = Gray`, `text = Black`, `surface = White`), `mono` (today's
  `NO_COLOR` theme), and four truecolor ones — Catppuccin Mocha, Tokyo Night,
  Gruvbox Dark, Solarized Light. Values as `Color::Rgb`.
- Selection: `--theme NAME`, `TICKET_TUI_THEME`, persisted in the session
  file; `NO_COLOR` still forces `mono`. `theme = auto` (default) picks
  `terminal` or `terminal-light` from the terminal background via
  `terminal-colorsaurus` (OSC 11, 100 ms timeout, queried before raw mode),
  falling back to `COLORFGBG`, then to dark.
- Runtime switch: `theme()` returns `Theme` by value from a `RwLock` instead
  of `&'static` from a `OnceLock` — `Theme` is `Copy`, so the ~300 call sites
  compile unchanged. A "Switch theme…" palette command opens a picker with
  live preview (the picker is the existing list overlay).
- Decision made here: write it in-house rather than depend on `opaline` or
  `ratatui-themes`. Our token vocabulary (state/type/priority/tag families) is
  the interesting part, and those crates only cover the generic palette. User
  TOML themes in the config directory can come later on the same struct.
- Tests: colour assertions in `src/ui/tests` go through tokens, never named
  colours (mostly true already); one test per preset that every token is
  legible against `surface`/`Reset`.

### B. Frame and chrome

- `BorderType::Rounded` when the theme says so (`terminal` and the truecolor
  presets: rounded; `mono`: plain).
- Panes share one border column: `Spacing::Overlap(1)` plus
  `merge_borders(MergeStrategy::Exact)` so the seam draws as `┬ … ┴`. The
  divider hit region becomes that column; it highlights on hover as now.
- Focused pane: `border_focused` + bold title. Unfocused: `border`, plain
  title. Table header gets `header` colour and a rule row (`─`) instead of
  the blank margin row.
- `Block::padding(Padding::horizontal(1))` on the details pane and every
  overlay.
- Bottom-border titles carry the counts and sort (`╰ 106/106 · Changed ↑ ─`),
  leaving the top border for the pane name and the sync state moves out (see
  D). This is a `title_position(TitlePosition::Bottom)` on the same block.

### C. Search row and filters

- The boxed search becomes one row: `/` prompt glyph in the accent, muted
  placeholder, the caret where it is now. Active: glyph goes `›` bold, row
  gets the `surface` background. `[×]` clear stays at the right edge. Saves
  two rows on every tab.
- Facet pills become filled chips (`surface` background; active filter in
  `accent` on `surface`; selected pill reversed as now).
- `[Actions] [?]` move to the right end of the tab bar, styled as pills.
- "(matching…)" becomes a braille spinner in the prompt cell on the 33 ms
  wake-up the search already uses.

### D. Status bar and tab bar

- The footer becomes a two-segment bar. Left: the context hints, trimmed to
  the 3–5 most useful for the mode (the `?` overlay has the rest). Right: sync
  segment — `⠹ Syncing`, `● Synced 2m`, `! Sync failed`, `◌ Stale`, `⊘
  Offline` — coloured by `success` / `error` / `warning` / `muted`, with the
  org/project name beside it when there is room.
- A notification no longer replaces the hints: it paints over the left
  segment in `info`/`error` with a leading `✓`/`✗` and expires as today.
- Tab bar: active tab in `accent` on `surface`, bold; inactive muted; badges
  (`⏵2`) in `warning`. Numbers stay so the `1–4` keys are discoverable.

### E. Table layout responsiveness

- Default wide split 60/40 instead of 50/50; the details pane wraps anyway.
- Auto-hide on by default and kept on after a column change; the rule becomes
  "drop the right-most unpinned column while Title would fall under 24
  characters", so Type and Pri go before Title is unreadable.
- Fix the last column losing cells under the scrollbar (reserve the column
  in the constraints rather than painting over it).
- State cell as glyph + word (`● Active`, `✓ Done`) using the existing
  `state_glyph`; priority as `P1`…`P4`; both keep their colours and their
  bold-under-`NO_COLOR` fallback.
- Selected row: `surface` background, accent `›` marker, `selection_fg` text
  in truecolor themes; unchanged in `terminal`.

### F. Overlays

- Size by ratio: help and palette `centered(Percentage(70), Percentage(70))`
  clamped to their content; dropdowns unchanged.
- Dim behind modals when the theme's `dim_behind_modals` is set: a pass that
  sets every cell outside the modal to `muted` foreground, no bold; `DIM`
  under `mono`. Runs before the modal paints, so hover/selection paint after
  it are untouched.
- Buttons as pills: ` Approve ` on `surface`, the primary one (`Complete`,
  `Delete`, `Save`) in `accent` fill, hovered reversed. Same character width
  as the bracketed labels, so `register_buttons` and every hit region stay as
  they are.
- `[×]` becomes a muted `×` at the same position; title on the modal in
  bold.
- List overlays (palette, actions, pickers): key label right-aligned in
  `muted`, the selected row on `surface`, a `›` marker in the accent.

### G. Details pane

- Heading block: title bold on the first line; a badge row under it —
  `#600 · [Issue] · ✓ Done · P1 · Jacob Ragsdale` — using the table's badge
  spans so colours match the row.
- Section headings as rules: `── Family ──────`, `── Planning ──` in
  `header`; the family tree, planning fields, description and history keep
  their content and their click targets.
- Fields as an aligned label column (`Assignee   Jacob Ragsdale`) with muted
  labels; editable values keep the hover underline.
- Child progress uses the fractional-block bar (`▏▎▍▌▋▊▉█`) over
  `PROGRESS_BAR_CELLS` and the `success` colour when complete.

### H. Motion, kept small

- One braille spinner helper (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`, 100 ms) used by search, sync,
  details fetch, git jobs and the pipeline log tail — replacing the four-frame
  circle. The loop already wakes for all of these; the spinner adds no
  wake-ups of its own.
- A two-frame accent flash on a row when an edit lands or is reverted, on the
  wake-up the sync reply already causes. Nothing more: no `tachyonfx`, no
  fades — they need a frame clock we deliberately do not run while idle.

### I. Docs and tests

- DESIGN.md's colour/look section (around line 1334) rewritten for tokens and
  presets; README screenshot replaced; `--theme` in the CLI table.
- UI tests keep asserting through `theme()` tokens; add a `TICKET_TUI_THEME`
  matrix run (`terminal`, `terminal-light`, one truecolor, `mono`) beside the
  existing `NO_COLOR=1` run.

## 5. Order and size

| Order | Slice | Why here | Size |
|---|---|---|---|
| 1 | A theme engine | Everything else paints through it | 1 day |
| 2 | E table layout | Highest visible payoff, fixes a real bug | ½ day |
| 3 | B frame | Rounded, shared borders, header rule | ½ day |
| 4 | C + D search row, status bar | Reclaims rows; state gets a home | 1 day |
| 5 | F overlays | Dim, pills, ratio sizing | ½ day |
| 6 | G details | Heading, rules, aligned labels | ½ day |
| 7 | H motion | Spinner helper, edit flash | ¼ day |
| 8 | I docs/tests | Lands with each slice, closed out here | ¼ day |

Not doing: Nerd Font icons (font dependency), images/sixel, `tachyonfx`,
a rewrite of the layout tree, any change to keys or the hit-region model.

## 6. Decisions (Jacob, 2026-08-29)

1. **Rounded corners by default**, in `terminal` too; `mono` stays plain.
2. **Borderless search row.**
3. **No built-in truecolor list.** Jacob's `theme` tool
   (`~/Development/theme`) already applies one palette to every program on
   the machine — Ghostty, herdr, gitui, yazi, lazygit and twenty others — from
   sixteen compiled-in themes plus user ones. ticket-tui becomes one of its
   targets: `theme` writes a `[theme.custom]` table into
   `~/.config/ticket-tui/config.toml` in its own vocabulary (`bg`, `bg_deep`,
   `surface`, `overlay`, `fg`, `subtle`, `muted`, `accent`, `red`, `green`,
   `yellow`, `blue`, `cyan`, `orange`, `teal`, `appearance`), exactly as it does
   for herdr, and ticket-tui maps that onto its own tokens. Without that
   table, `terminal` (ANSI-16) is the default. So slice A's presets are
   `terminal`, `terminal-light`, `mono` and `custom`; the Catppuccin/Tokyo
   Night/… list is dropped, and light/dark auto-detection is deferred —
   `appearance` in the palette says which it is.
4. **Dim behind modals, yes.**
5. **`config.toml`** in `$XDG_CONFIG_HOME/ticket-tui/` (`~/.config` by
   default, on macOS too, because that is where `theme` and every other
   terminal program on this machine keep theirs). A running ticket-tui reloads
   it when it changes, so `theme pick` repaints the app live the way it
   repaints Zed and Sublime.

### Slice A, revised

- `src/config.rs`: `Config { theme: ThemeChoice, custom: Option<Palette> }`
  read with `toml`; unknown keys ignored so `theme` can add to the table.
- `src/ui/theme.rs` (moved out of `ui/mod.rs`): the `Theme` struct with the
  new tokens; presets `terminal`, `terminal_light`, `mono`; `Theme::from_palette`
  mapping the `theme` vocabulary (accent→accent, fg→text, subtle→body,
  muted→muted, blue→link, overlay→selection, a bg/overlay mix→hover,
  overlay→border, accent→border_focused, green→success, yellow→warning,
  red→error, orange→epic and priority-high, teal/cyan in the tag palette);
  `theme()` returns `Theme` by value from a `RwLock`; `set_theme`.
- Precedence: `NO_COLOR` → `mono`; `--theme` / `TICKET_TUI_THEME`; `theme =`
  in config.toml; `[theme.custom]` present → `custom`; else `terminal`.
- The event loop polls config.toml's mtime once a second (it already wakes at
  least that often) and re-applies the theme when it changes.
- In the `theme` repo: a `ticket-tui` target next to `herdr`'s, twenty lines,
  plus its README row.

## Sources

- [awesome-ratatui](https://github.com/ratatui/awesome-ratatui/blob/main/README.md) ·
  [App showcase](https://ratatui.rs/showcase/apps/) ·
  [Third-party widgets](https://ratatui.rs/showcase/third-party-widgets/)
- [Ratatui 0.30 highlights](https://ratatui.rs/highlights/v030/) ·
  [Block docs](https://docs.rs/ratatui/latest/ratatui/widgets/struct.Block.html)
- [tachyonfx](https://github.com/ratatui/tachyonfx) ·
  [ratatui-themes](https://docs.rs/ratatui-themes/latest/ratatui_themes/) ·
  [opaline](https://lib.rs/crates/opaline) ·
  [ratatui-comfy-toaster](https://lib.rs/crates/ratatui-comfy-toaster) ·
  [tui-widgets](https://github.com/ratatui/tui-widgets) ·
  [terminal-colorsaurus](https://github.com/tautropfli/terminal-colorsaurus)
- [television config](https://github.com/alexpasmantier/television/blob/main/.config/config.toml) ·
  [television theme](https://raw.githubusercontent.com/alexpasmantier/television/main/themes/television.toml) ·
  [gitui THEMES.md](https://github.com/gitui-org/gitui/blob/master/THEMES.md) ·
  [gitui style.rs](https://raw.githubusercontent.com/gitui-org/gitui/master/src/ui/style.rs) ·
  [yazi theme.toml](https://yazi-rs.github.io/docs/configuration/theme/) ·
  [atuin theming](https://docs.atuin.sh/18.19/guide/theming/) ·
  [crates-tui](https://github.com/ratatui/crates-tui)
- [The Terminal Renaissance: Designing Beautiful TUIs](https://hyperbliss.tech/blog/2026.04.04_terminal-renaissance/) ·
  [The TUI Renaissance 2026](https://www.youngju.dev/blog/culture/2026-05-14-tui-development-ratatui-bubbletea-ink-textual-terminal-ui-renaissance-deep-dive-2026.en) ·
  [Terminal colour detection](https://terminfo.dev/fundamentals/color-detection)
- X: [@ratatui_rs](https://x.com/ratatui_rs) ·
  [ratatui-braille-bar](https://x.com/orhundev/status/2036450816488243313) ·
  [sdrrat](https://x.com/orhundev/status/2054987609345323421) ·
  [ratatui-splash-screen](https://x.com/orhundev/status/1768976745128935900) ·
  [ratatui-code-editor](https://x.com/orhundev/status/1977642147018105032)
