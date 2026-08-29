//! The pieces every screen is built from: the modal frame, the query
//! field, controls, the scrollbar, and the hover and selection paint.

use super::*;

/// Clears `area`, draws the framed and titled box every overlay sits in, and
/// registers its close button. The inner area is what the overlay paints in.
pub(super) fn render_modal_frame(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    title: &str,
) -> Rect {
    let layer = match app.mode {
        AppMode::Facets => PointerLayer::Popup,
        _ => PointerLayer::Modal,
    };
    let block = Block::default()
        .title(title.to_owned())
        .title(Line::from("[×]").right_aligned())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme().accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    register_close_button(app, area, layer);
    inner
}

pub(super) fn register_close_button(app: &mut App, area: Rect, layer: PointerLayer) {
    app.shell.hit_regions.push(region(
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

/// The filter field at the top of a picker, and the caret in it: the text as
/// typed, or `placeholder` while nothing has been. Clicking it places the
/// caret, and dragging across it selects.
pub(super) fn render_query_field(
    frame: &mut Frame<'_>,
    app: &mut App,
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
    app.shell.hit_regions.push(region(
        area,
        target,
        PointerLayer::Modal,
        Some(SelectableSurface::Overlay),
        None,
    ));
    capture_selectable(frame, app, SelectableSurface::Overlay, area, false);
    let cursor_x = area.x.saturating_add(
        u16::try_from(cursor)
            .unwrap_or(u16::MAX)
            .min(area.width.saturating_sub(1)),
    );
    frame.set_cursor_position((cursor_x, area.y));
}

pub(super) fn render_control(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    label: &str,
    target: PointerTarget,
    layer: PointerLayer,
    enabled: bool,
) {
    let hovered = app.shell.hovered() == Some(&target);
    let style = if !enabled {
        Style::default().fg(theme().muted)
    } else if hovered {
        Style::default()
            .fg(theme().text)
            .bg(theme().selected_background)
            .add_modifier(Modifier::REVERSED)
    } else {
        Style::default().fg(theme().accent)
    };
    frame.render_widget(Paragraph::new(label).style(style), area);
    if enabled {
        app.shell
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
    app: &mut App,
    area: Rect,
    surface: ScrollSurface,
    content: usize,
    offset: usize,
    viewport: usize,
) {
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
    app.shell.hit_regions.set_scroll(surface, metrics);
    if let Some(thumb) = geometry {
        let thumb_rect = Rect::new(track.x, track.y.saturating_add(thumb.y), 1, thumb.height);
        let above = Rect::new(track.x, track.y, 1, thumb.y);
        let below_y = track.y.saturating_add(thumb.y).saturating_add(thumb.height);
        let below_height = track.y.saturating_add(track.height).saturating_sub(below_y);
        if above.height > 0 {
            app.shell.hit_regions.push(region(
                above,
                PointerTarget::ScrollbarTrack {
                    surface,
                    page_down: false,
                },
                current_layer(app),
                None,
                Some(surface),
            ));
        }
        app.shell.hit_regions.push(region(
            thumb_rect,
            PointerTarget::ScrollbarThumb { surface },
            current_layer(app),
            None,
            Some(surface),
        ));
        if below_height > 0 {
            app.shell.hit_regions.push(region(
                Rect::new(track.x, below_y, 1, below_height),
                PointerTarget::ScrollbarTrack {
                    surface,
                    page_down: true,
                },
                current_layer(app),
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
    app: &mut App,
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
    app.shell.hit_regions.add_selectable(SelectableSnapshot {
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
        PointerTarget::TableRow { .. }
            | PointerTarget::OpenTicket { .. }
            | PointerTarget::ToggleBookmark { .. }
            | PointerTarget::ToggleRowSelect { .. }
            | PointerTarget::JumpToTicket(_)
            | PointerTarget::FacetValue { .. }
            | PointerTarget::FilterRow { .. }
            | PointerTarget::PaletteCommand { .. }
            | PointerTarget::ViewRow { .. }
            | PointerTarget::SummaryRow { .. }
            | PointerTarget::SortChoose(_)
            | PointerTarget::ColumnToggle { .. }
    )
}

pub(super) fn paint_hover(frame: &mut Frame<'_>, app: &App) {
    let Some(region) = app.shell.hovered_region() else {
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

pub(super) fn paint_selection(frame: &mut Frame<'_>, app: &App) {
    let Some(selection) = app
        .shell
        .selection()
        .filter(|selection| !selection.is_empty())
    else {
        return;
    };
    let Some(snapshot) = app.shell.hit_regions.selectable(selection.surface) else {
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
