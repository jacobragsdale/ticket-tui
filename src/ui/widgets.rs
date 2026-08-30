//! The pieces every screen is built from: the modal frame, the query
//! field, controls, the scrollbar, and the hover and selection paint.

use super::*;

/// Clears `area`, draws the framed and titled box every overlay sits in, and
/// registers its close button. The inner area is what the overlay paints in.
///
/// The clear is what makes an overlay opaque: it is painted into the same
/// frame as the table underneath it, so without one the border lands over
/// rows that go on showing through. It belongs here rather than at each call
/// site, because every overlay wants it and forgetting it is invisible until
/// somebody opens that one pane.
/// What the close button on a modal frame reads, and how wide it is. The
/// hit region and the tests find the frame by it.
pub(super) const CLOSE_LABEL: &str = " × ";

/// Washes out everything outside `modal` so the overlay reads as the layer in
/// front of the screen rather than as more of it: every cell keeps its text
/// and its ground and gives up its colour and its weight.
///
/// Runs before the modal paints, so nothing it draws is touched, and the
/// hover and selection passes come later still.
pub(super) fn dim_behind(frame: &mut Frame<'_>, modal: Rect) {
    if !theme().dim_behind_modals {
        return;
    }
    let area = frame.area();
    let muted = theme().muted;
    let buffer = frame.buffer_mut();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if modal.contains(Position::new(x, y)) {
                continue;
            }
            let cell = &mut buffer[(x, y)];
            let style = cell
                .style()
                .fg(muted)
                .remove_modifier(Modifier::BOLD | Modifier::REVERSED);
            cell.set_style(style);
        }
    }
}

pub(super) fn render_modal_frame(
    frame: &mut Frame<'_>,
    layer: PointerLayer,
    shell: &mut Shell,
    area: Rect,
    title: &str,
) -> Rect {
    dim_behind(frame, area);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(Line::styled(
            title.to_owned(),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .title(Line::styled(CLOSE_LABEL, Style::default().fg(theme().muted)).right_aligned())
        .borders(Borders::ALL)
        .border_type(theme().border_type)
        // A gutter on the left; the column on the right is where a list's own
        // scrollbar goes.
        .padding(Padding::left(1))
        .border_style(Style::default().fg(theme().accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    register_close_button(shell, area, layer);
    inner
}

pub(super) fn register_close_button(shell: &mut Shell, area: Rect, layer: PointerLayer) {
    shell.hit_regions.push(region(
        Rect::new(
            area.x.saturating_add(area.width.saturating_sub(4)),
            area.y,
            3,
            1,
        ),
        PointerTarget::CloseOverlay,
        layer,
        None,
        None,
    ));
}

/// The footer: what the keys do on the left, how the sync is going on the
/// right. A notification takes the left segment over until it expires and
/// never covers the right one, so the sync state is on screen whatever else
/// is being said.
pub(super) fn render_status_bar(frame: &mut Frame<'_>, shell: &Shell, area: Rect, hint: &str) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let status = shell.sync_status();
    let (glyph, tone) = sync_glyph(&status);
    let mut right = vec![
        Span::styled(glyph, Style::default().fg(tone)),
        Span::raw(" "),
        Span::styled(status.label(), Style::default().fg(tone)),
        Span::raw(" "),
    ];
    let width = |spans: &[Span<'_>]| {
        u16::try_from(
            spans
                .iter()
                .map(|span| span.content.chars().count())
                .sum::<usize>(),
        )
        .unwrap_or(u16::MAX)
    };
    // The project only earns its place once the hints have room of their own.
    if let Some(project) = shell.project_label()
        && area.width
            > width(&right)
                .saturating_add(u16::try_from(project.chars().count()).unwrap_or(u16::MAX))
                .saturating_add(32)
    {
        right.insert(
            0,
            Span::styled(project.to_owned(), Style::default().fg(theme().muted)),
        );
        right.insert(1, Span::raw("  "));
    }
    let right_width = width(&right).min(area.width);
    frame.render_widget(
        Line::from(right).right_aligned(),
        Rect::new(
            area.right().saturating_sub(right_width),
            area.y,
            right_width,
            1,
        ),
    );

    let left = Rect::new(
        area.x.saturating_add(1),
        area.y,
        area.width
            .saturating_sub(1)
            .saturating_sub(right_width)
            .saturating_sub(1),
        1,
    );
    if left.width == 0 {
        return;
    }
    let (text, style) = match shell.notification() {
        Some((message, NotificationLevel::Info)) => {
            (format!("✓ {message}"), Style::default().fg(theme().info))
        }
        Some((message, NotificationLevel::Error)) => {
            (format!("✗ {message}"), Style::default().fg(theme().error))
        }
        None => (
            trim_hints(hint, left.width),
            Style::default().fg(theme().muted),
        ),
    };
    frame.render_widget(Line::styled(text, style), left);
}

/// The glyph and the colour one sync state reads in. `Syncing` spins on the
/// wake-ups the pull itself causes.
fn sync_glyph(status: &SyncStatus) -> (String, Color) {
    match status {
        SyncStatus::Syncing | SyncStatus::Reloading => {
            (spinner_frame().to_string(), theme().accent)
        }
        SyncStatus::Synced(_) => ("●".to_owned(), theme().success),
        SyncStatus::Failed => ("!".to_owned(), theme().error),
        SyncStatus::Paused(_) | SyncStatus::Stale => ("◌".to_owned(), theme().warning),
        SyncStatus::Offline => ("⊘".to_owned(), theme().muted),
        SyncStatus::Waiting => ("○".to_owned(), theme().muted),
    }
}

/// As many whole hints as `width` holds, cut where one ends rather than in
/// the middle of a key. The `?` overlay has the rest of them.
fn trim_hints(hint: &str, width: u16) -> String {
    let width = usize::from(width);
    if hint.chars().count() <= width {
        return hint.to_owned();
    }
    let mut kept = String::new();
    for part in hint.split("  ") {
        if part.is_empty() {
            continue;
        }
        let next = if kept.is_empty() {
            part.chars().count()
        } else {
            kept.chars().count() + 2 + part.chars().count()
        };
        if next > width {
            break;
        }
        if !kept.is_empty() {
            kept.push_str("  ");
        }
        kept.push_str(part);
    }
    kept
}

/// The one-row search every tab opens with: a prompt glyph, the query or the
/// placeholder, and a `[×]` at the right end of the tabs that offer one.
pub(super) struct SearchRow<'a> {
    pub area: Rect,
    pub text: &'a str,
    pub cursor: usize,
    pub placeholder: &'a str,
    /// Whether the row has the keyboard: the glyph goes `›` and the row takes the
    /// surface ground.
    pub active: bool,
    /// Whether a search is still running, which the prompt cell spins for.
    pub pending: bool,
    /// Whether the row offers a clear button once there is something to
    /// clear.
    pub clearable: bool,
    /// What the row is searching inside, when it is not the whole tab: the
    /// saved view a pull request list is filtered by, the pipeline whose runs
    /// these are. Muted, at the right end of the row.
    pub trailer: String,
    pub layer: PointerLayer,
    pub selectable: SelectableSurface,
}

/// The prompt cell, the field and the clear button, on one row. The field's
/// rect is the `SearchField` target and the selectable surface, so a click
/// places the caret and a drag across it selects, as they did when the row
/// was a box three rows tall.
pub(super) fn render_search_row(frame: &mut Frame<'_>, shell: &mut Shell, row: SearchRow<'_>) {
    let SearchRow {
        area,
        text,
        cursor,
        placeholder,
        active,
        pending,
        clearable,
        trailer,
        layer,
        selectable,
    } = row;
    if area.width < 4 || area.height == 0 {
        return;
    }
    let area = Rect::new(area.x, area.y, area.width, 1);
    // The ground says the row has the keyboard. A palette with no surface
    // colour — `mono` — reverses it instead, so the state still reads.
    if active {
        let style = if theme().surface == Color::Reset {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().bg(theme().surface)
        };
        frame.render_widget(Block::default().style(style), area);
    }
    let (glyph, glyph_style) = if pending {
        (
            spinner_frame().to_string(),
            Style::default().fg(theme().accent),
        )
    } else if active {
        (
            "›".to_owned(),
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("/".to_owned(), Style::default().fg(theme().muted))
    };
    frame.render_widget(
        Line::styled(glyph, glyph_style),
        Rect::new(area.x, area.y, 1, 1),
    );

    let clear = u16::from(clearable && !text.is_empty() && area.width > 8) * 3;
    // The breadcrumb only fits where it leaves the query most of the row.
    let trailer_width = u16::try_from(trailer.chars().count())
        .unwrap_or(u16::MAX)
        .saturating_add(1);
    let trailer_width = if trailer.is_empty() || area.width < trailer_width.saturating_add(32) {
        0
    } else {
        trailer_width
    };
    let field = Rect::new(
        area.x.saturating_add(2),
        area.y,
        area.width
            .saturating_sub(2)
            .saturating_sub(clear)
            .saturating_sub(trailer_width),
        1,
    );
    if trailer_width > 0 {
        frame.render_widget(
            Line::styled(trailer, Style::default().fg(theme().muted)).right_aligned(),
            Rect::new(field.right(), area.y, trailer_width, 1),
        );
    }
    let line = if text.is_empty() && !active {
        Line::styled(placeholder.to_owned(), Style::default().fg(theme().muted))
    } else {
        Line::styled(text.to_owned(), Style::default().fg(theme().text))
    };
    // The caret stays on screen in a query longer than the row.
    let cursor_offset = u16::try_from(cursor).unwrap_or(u16::MAX);
    let scroll = cursor_offset.saturating_sub(field.width.saturating_sub(1));
    frame.render_widget(Paragraph::new(line).scroll((0, scroll)), field);
    if clear > 0 {
        render_control(
            frame,
            shell,
            Control {
                area: Rect::new(field.right().saturating_add(trailer_width), area.y, 3, 1),
                label: CLOSE_LABEL,
                target: PointerTarget::ClearQuery,
                layer,
                kind: ControlKind::Glyph,
                enabled: true,
            },
        );
    }
    shell.hit_regions.push(region(
        field,
        PointerTarget::SearchField,
        layer,
        Some(selectable),
        None,
    ));
    capture_selectable(frame, shell, selectable, field, false);
    if active {
        frame.set_cursor_position((
            field
                .x
                .saturating_add(cursor_offset.saturating_sub(scroll))
                .min(field.right().saturating_sub(1)),
            field.y,
        ));
    }
}

/// The frame a braille spinner is on this instant. Nothing schedules a
/// repaint for it: it turns on the wake-ups the work itself causes, so an
/// idle screen still paints nothing.
pub(super) fn spinner_frame() -> char {
    const FRAMES: [char; 10] = [
        '\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}',
        '\u{2827}', '\u{2807}', '\u{280f}',
    ];
    let ticks = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() / 100);
    FRAMES[usize::try_from(ticks % 10).unwrap_or(0)]
}

/// The filter field at the top of a picker, and the caret in it: the text as
/// typed, or `placeholder` while nothing has been. Clicking it places the
/// caret, and dragging across it selects.
pub(super) fn render_query_field(
    frame: &mut Frame<'_>,
    shell: &mut Shell,
    area: Rect,
    text: &str,
    cursor: usize,
    placeholder: &str,
    target: PointerTarget,
) {
    let query = if text.is_empty() {
        Line::styled(placeholder.to_owned(), Style::default().fg(theme().muted))
    } else {
        Line::from(text.to_owned())
    };
    frame.render_widget(
        Paragraph::new(query).style(Style::default().fg(theme().text)),
        area,
    );
    shell.hit_regions.push(region(
        area,
        target,
        PointerLayer::Modal,
        Some(SelectableSurface::Overlay),
        None,
    ));
    capture_selectable(frame, shell, SelectableSurface::Overlay, area, false);
    let cursor_x = area.x.saturating_add(
        u16::try_from(cursor)
            .unwrap_or(u16::MAX)
            .min(area.width.saturating_sub(1)),
    );
    frame.set_cursor_position((cursor_x, area.y));
}

/// The row each line starts on once the paragraph has wrapped to `width`,
/// and how many rows the whole thing takes. A hit region has to be placed by
/// this rather than by the line's index: a long URL or title takes more than
/// one row and pushes everything under it down.
pub(super) fn wrapped_rows(lines: &[Line<'_>], width: u16) -> (Vec<usize>, usize) {
    let mut rows = Vec::with_capacity(lines.len());
    let mut row = 0;
    for line in lines {
        rows.push(row);
        row += Paragraph::new(line.clone())
            .wrap(Wrap { trim: false })
            .line_count(width)
            .max(1);
    }
    (rows, row)
}

/// The screen row one wrapped line lands on, once the pane has scrolled by
/// `offset`, or nothing when it is off the pane.
pub(super) fn row_on_screen(
    inner: Rect,
    rows: &[usize],
    index: usize,
    offset: usize,
) -> Option<u16> {
    let row = rows.get(index)?.checked_sub(offset)?;
    if row >= usize::from(inner.height) {
        return None;
    }
    Some(
        inner
            .y
            .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
    )
}

/// One click target per button on one row. Each is as wide as the pane
/// paints it: the label with a space either side.
/// A row of chips standing for the keys they name. Each label carries its own
/// padding \u{2014} ` Approve ` \u{2014} and the cell after it is the gap to the next, which
/// is the width [`register_buttons`] steps by.
pub(super) fn button_row(buttons: &[(&str, PointerTarget)]) -> Line<'static> {
    let chip = control_style(ControlKind::Chip, false, true);
    Line::from(
        buttons
            .iter()
            .flat_map(|(label, _)| [Span::styled((*label).to_owned(), chip), Span::raw(" ")])
            .collect::<Vec<_>>(),
    )
}

pub(super) fn register_buttons(
    shell: &mut Shell,
    inner: Rect,
    y: u16,
    layer: PointerLayer,
    buttons: &[(&str, PointerTarget)],
) {
    let mut x = inner.x;
    for (label, target) in buttons {
        let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
        if x.saturating_add(width) > inner.x.saturating_add(inner.width) {
            break;
        }
        shell.hit_regions.push(region(
            Rect::new(x, y, width, 1),
            target.clone(),
            layer,
            None,
            None,
        ));
        x = x.saturating_add(width).saturating_add(1);
    }
}

/// How a control is painted. Every kind keeps the width its label always had,
/// so the hit regions and the rows they sit on are unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ControlKind {
    /// A glyph on its own — a sort arrow, a column's `<`/`>`, the search
    /// row's clear.
    Glyph,
    /// An action with a word on it, on the surface ground.
    Chip,
    /// The action the overlay exists for, filled with the accent.
    Primary,
}

/// How one control reads: disabled, under the pointer, or waiting.
pub(super) fn control_style(kind: ControlKind, hovered: bool, enabled: bool) -> Style {
    if !enabled {
        return Style::default().fg(theme().muted);
    }
    if hovered {
        return Style::default()
            .fg(theme().text)
            .bg(theme().selected_background)
            .add_modifier(Modifier::REVERSED);
    }
    match kind {
        ControlKind::Glyph => Style::default().fg(theme().accent),
        ControlKind::Chip => Style::default().fg(theme().text).bg(theme().surface),
        // A palette with no colour to fill with reverses instead, so the
        // action an overlay is for still leads under NO_COLOR.
        ControlKind::Primary if theme().accent == Color::Reset => {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
        ControlKind::Primary => Style::default()
            .fg(theme().surface)
            .bg(theme().accent)
            .add_modifier(Modifier::BOLD),
    }
}

/// One control: where it goes, what it says, what it does, and how it reads.
pub(super) struct Control<'a> {
    pub area: Rect,
    pub label: &'a str,
    pub target: PointerTarget,
    pub layer: PointerLayer,
    pub kind: ControlKind,
    /// A control nobody can use yet is muted and registers no hit region.
    pub enabled: bool,
}

pub(super) fn render_control(frame: &mut Frame<'_>, shell: &mut Shell, control: Control<'_>) {
    let Control {
        area,
        label,
        target,
        layer,
        kind,
        enabled,
    } = control;
    let hovered = shell.hovered() == Some(&target);
    frame.render_widget(
        Paragraph::new(label).style(control_style(kind, hovered, enabled)),
        area,
    );
    if enabled {
        shell
            .hit_regions
            .push(region(area, target, layer, None, None));
    }
}

/// Paints the scrollbar down the last column of `area` and registers the click
/// and drag regions that go with it.
///
/// Painting reads the same [`ScrollMetrics::thumb`] geometry the hit regions
/// do, so the thumb on screen is exactly the thumb you can grab. Ratatui's own
/// `Scrollbar` widget reads its content length as a count of scroll positions
/// rather than of rows, which left the painted thumb short of the bottom of the
/// track at the maximum offset while the draggable one reached it.
pub(super) fn render_scrollbar(
    frame: &mut Frame<'_>,
    layer: PointerLayer,
    shell: &mut Shell,
    area: Rect,
    surface: ScrollSurface,
    scroll: ScrollState,
) {
    let ScrollState {
        offset,
        content,
        viewport,
    } = scroll;
    let track = Rect::new(
        area.x.saturating_add(area.width.saturating_sub(1)),
        area.y,
        1,
        area.height,
    );
    let metrics = ScrollMetrics {
        offset,
        content,
        viewport,
        track,
    };
    let geometry = metrics.thumb();
    paint_scrollbar(frame, track, geometry);
    shell.hit_regions.set_scroll(surface, metrics);
    if let Some(thumb) = geometry {
        let thumb_rect = Rect::new(track.x, track.y.saturating_add(thumb.y), 1, thumb.height);
        let above = Rect::new(track.x, track.y, 1, thumb.y);
        let below_y = track.y.saturating_add(thumb.y).saturating_add(thumb.height);
        let below_height = track.y.saturating_add(track.height).saturating_sub(below_y);
        if above.height > 0 {
            shell.hit_regions.push(region(
                above,
                PointerTarget::ScrollbarTrack {
                    surface,
                    page_down: false,
                },
                layer,
                None,
                Some(surface),
            ));
        }
        shell.hit_regions.push(region(
            thumb_rect,
            PointerTarget::ScrollbarThumb { surface },
            layer,
            None,
            Some(surface),
        ));
        if below_height > 0 {
            shell.hit_regions.push(region(
                Rect::new(track.x, below_y, 1, below_height),
                PointerTarget::ScrollbarTrack {
                    surface,
                    page_down: true,
                },
                layer,
                None,
                Some(surface),
            ));
        }
    }
}

/// Fills the track column, then overwrites the thumb's rows on top of it. The
/// thumb's weight carries the distinction under NO_COLOR, where the scrollbar
/// colour resets along with every other.
pub(super) fn paint_scrollbar(frame: &mut Frame<'_>, track: Rect, thumb: Option<ThumbGeometry>) {
    let track_style = Style::default().fg(theme().scrollbar);
    let thumb_style = track_style.add_modifier(Modifier::BOLD);
    let thumb_rows = thumb.map_or(0..0, |thumb| thumb.y..thumb.y.saturating_add(thumb.height));
    let buffer = frame.buffer_mut();
    for row in 0..track.height {
        let (symbol, style) = if thumb_rows.contains(&row) {
            ("┃", thumb_style)
        } else {
            ("│", track_style)
        };
        if let Some(cell) = buffer.cell_mut((track.x, track.y.saturating_add(row))) {
            cell.set_symbol(symbol).set_style(style);
        }
    }
}

pub(super) fn capture_selectable(
    frame: &mut Frame<'_>,
    shell: &mut Shell,
    surface: SelectableSurface,
    rect: Rect,
    skip_last_col: bool,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let buffer = frame.buffer_mut();
    let width = if skip_last_col {
        rect.width.saturating_sub(1)
    } else {
        rect.width
    };
    let mut cells = Vec::with_capacity(usize::from(rect.height));
    for dy in 0..rect.height {
        let mut row = Vec::with_capacity(usize::from(width));
        for dx in 0..width {
            row.push(
                buffer[(rect.x.saturating_add(dx), rect.y.saturating_add(dy))]
                    .symbol()
                    .to_string(),
            );
        }
        cells.push(row);
    }
    shell.hit_regions.add_selectable(SelectableSnapshot {
        surface,
        rect: Rect { width, ..rect },
        cells,
    });
}

/// Whether a hovered target is a row of content rather than a control.
///
/// Rows carry colour-coded cells (State, Pri, and the `[Type]`/`[tag]` badges)
/// that reversing would flatten into solid blocks, so a row is tinted instead.
/// Controls stay reversed, because there a block is the intended affordance.
pub(super) fn row_like(target: &PointerTarget) -> bool {
    matches!(
        target,
        PointerTarget::ApprovalRow { .. }
            | PointerTarget::TreeRow { .. }
            | PointerTarget::SelectTab { .. }
            | PointerTarget::TableRow { .. }
            | PointerTarget::OpenInBrowser { .. }
            | PointerTarget::ToggleBookmark { .. }
            | PointerTarget::ToggleRowSelect { .. }
            | PointerTarget::Follow(_)
            | PointerTarget::FacetValue { .. }
            | PointerTarget::FilterRow { .. }
            | PointerTarget::PaletteCommand { .. }
            | PointerTarget::ViewRow { .. }
            | PointerTarget::SummaryRow { .. }
            | PointerTarget::SortChoose(_)
            | PointerTarget::ColumnToggle { .. }
    )
}

pub(super) fn paint_hover(frame: &mut Frame<'_>, shell: &Shell) {
    let Some(region) = shell.hovered_region() else {
        return;
    };
    let target = &region.target;
    if matches!(
        target,
        PointerTarget::FocusTickets
            | PointerTarget::FocusDetails
            | PointerTarget::SearchField
            | PointerTarget::OverlayBody
            | PointerTarget::DismissFacet
            | PointerTarget::DismissOverlay
            | PointerTarget::PaletteQuery
            | PointerTarget::ViewName
            | PointerTarget::PromptInput
    ) {
        return;
    }
    // An editable value underlines rather than inverts, the way a link does,
    // and a modifier says so under NO_COLOR too.
    if matches!(target, PointerTarget::EditField { .. }) {
        let rect = region.rect;
        let buffer = frame.buffer_mut();
        for y in rect.y..rect.y.saturating_add(rect.height) {
            for x in rect.x..rect.x.saturating_add(rect.width) {
                let cell = &mut buffer[(x, y)];
                let style = cell.style().add_modifier(Modifier::UNDERLINED);
                cell.set_style(style);
            }
        }
        return;
    }
    // Painted last, so on a hovered *and* selected row the tint covers the
    // selection background. Without a palette there is no tint to see, so a
    // monochrome terminal falls back to reversing the row.
    let tint = (row_like(target) && theme().hover_background != Color::Reset)
        .then(|| theme().hover_background);
    let rect = region.rect;
    let buffer = frame.buffer_mut();
    for y in rect.y..rect.y.saturating_add(rect.height) {
        for x in rect.x..rect.x.saturating_add(rect.width) {
            let cell = &mut buffer[(x, y)];
            let style = match tint {
                Some(background) => cell.style().bg(background),
                None => cell.style().add_modifier(Modifier::REVERSED),
            };
            cell.set_style(style);
        }
    }
}

pub(super) fn paint_selection(frame: &mut Frame<'_>, shell: &Shell) {
    let Some(selection) = shell.selection().filter(|selection| !selection.is_empty()) else {
        return;
    };
    let Some(snapshot) = shell.hit_regions.selectable(selection.surface) else {
        return;
    };
    let (start, end) = selection.ordered();
    let buffer = frame.buffer_mut();
    let last_line = end.line.min(snapshot.cells.len().saturating_sub(1));
    for (row, cells) in snapshot.cells.iter().enumerate() {
        if row < start.line || row > last_line {
            continue;
        }
        let from = if row == start.line { start.col } else { 0 };
        let to = if row == end.line {
            end.col.max(from)
        } else {
            cells.len()
        };
        let y = snapshot
            .rect
            .y
            .saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
        for col in from..to.min(cells.len()) {
            let x = snapshot
                .rect
                .x
                .saturating_add(u16::try_from(col).unwrap_or(u16::MAX));
            let cell = &mut buffer[(x, y)];
            cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
        }
    }
}
