//! The work item table: its rows, its cells and the colours they carry.

use super::*;
use crate::columns::{
    COLUMN_SPACING, ColumnId, MARKER_WIDTH, SCROLLBAR_WIDTH, SELECTION_WIDTH, TableLayout,
};
/// Where a list table's parts land inside its area. A screen works this out
/// before it draws, because how many rows fit is what its viewport is.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TableGeometry {
    /// Inside the border.
    pub inner: Rect,
    /// The rows, below the header and its blank line.
    pub body: Rect,
    pub visible_rows: usize,
}

#[must_use]
pub(crate) fn table_geometry(area: Rect, row_height: u16) -> TableGeometry {
    let inner = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    let body_height = inner.height.saturating_sub(2);
    let visible_rows = usize::from(body_height / row_height.max(1)).max(1);
    TableGeometry {
        inner,
        body: Rect::new(inner.x, inner.y.saturating_add(2), inner.width, body_height),
        visible_rows,
    }
}

/// One list, drawn as a table. Every tab's list goes through here: the header
/// comes from the screen's own columns, each cell from its `cell` closure, and
/// the hit regions are the same on all of them, so a row is selected, opened
/// and sorted the same way whatever it holds.
pub(crate) struct TableSpec<'a, C: ColumnId> {
    /// What the pane is, on the top border: `Tickets`, `Repos`, a pipeline's
    /// name. It stays the same while the list underneath changes.
    pub title: String,
    /// What the list is doing, on the bottom border: how many rows, how they
    /// are ordered. Empty leaves the bottom border bare.
    pub status: String,
    pub focused: bool,
    pub layout: &'a TableLayout<C>,
    /// The column the list is ordered by and the arrow that says which way, if
    /// the list is ordered by a column at all.
    pub sorted: Option<(C, &'static str)>,
    /// How many rows the list has, which is not how many are on screen.
    pub count: usize,
    pub offset: usize,
    pub selected: Option<usize>,
    pub row_height: u16,
    pub layer: PointerLayer,
    pub scroll: ScrollSurface,
    pub selectable: SelectableSurface,
    /// The gutter cell a row opens with — the check and bookmark markers on
    /// work items. A screen that passes none gets neither the column nor the
    /// two targets that go with it.
    pub marker: Option<&'a dyn Fn(usize) -> Line<'static>>,
    /// One cell, by row and column. Called only for the rows on screen.
    pub cell: &'a mut dyn FnMut(usize, C) -> Cell<'static>,
}

pub(crate) fn render_list_table<C: ColumnId>(
    frame: &mut Frame<'_>,
    shell: &mut Shell,
    area: Rect,
    spec: &mut TableSpec<'_, C>,
) {
    // A pane stacked above another shares its bottom border with it, and the
    // pane below paints that row last: there is nowhere down there to write,
    // so the status joins the name on the top border instead.
    let shares_its_bottom_border =
        shell.divider_orientation() == Some(DividerOrientation::Horizontal);
    let (title, status) = if spec.status.is_empty() {
        (spec.title.clone(), String::new())
    } else if shares_its_bottom_border {
        (format!("{}{} ", spec.title, spec.status), String::new())
    } else {
        (spec.title.clone(), format!(" {} ", spec.status))
    };
    // The scrollbar's column is padding, not a place a cell may be painted:
    // the table lays its columns out inside what is left, so the last one
    // keeps every character it was given whether or not the list overflows.
    let mut block = focused_block(title, spec.focused).padding(Padding::right(SCROLLBAR_WIDTH));
    if !status.is_empty() {
        block = block.title_bottom(Line::from(status));
    }
    let geometry = table_geometry(area, spec.row_height);
    let inner = geometry.inner;
    let columns = spec
        .layout
        .visible_columns(TableLayout::<C>::available_width(
            inner.width,
            spec.marker.is_some(),
        ));
    let mut constraints = Vec::new();
    if spec.marker.is_some() {
        constraints.push(Constraint::Length(MARKER_WIDTH));
    }
    constraints.extend(columns.iter().copied().map(TableLayout::constraint));

    let mut header_cells = Vec::new();
    if spec.marker.is_some() {
        header_cells.push(Cell::from(""));
    }
    header_cells.extend(columns.iter().map(|column| {
        let direction = spec
            .sorted
            .filter(|(sorted, _)| *sorted == column.id)
            .map_or("", |(_, symbol)| symbol);
        let line = Line::from(format!("{}{direction}", column.id.label()));
        Cell::from(if column.id.right_aligned() {
            line.right_aligned()
        } else {
            line
        })
    }));
    let header = Row::new(header_cells)
        .style(
            Style::default()
                .fg(theme().header)
                .add_modifier(Modifier::BOLD),
        )
        .height(1)
        .bottom_margin(1);

    let visible_rows = geometry.visible_rows;
    // A palette that names the colour a selected row reads in lends it to the
    // cells that have none of their own; the state, type and priority cells
    // keep theirs, so the row under the cursor is still colour-coded.
    let selection_fg = theme().selection_fg;
    let rows = (spec.offset..spec.count.min(spec.offset + visible_rows)).map(|index| {
        let mut cells = Vec::new();
        if let Some(marker) = spec.marker {
            cells.push(Cell::from(marker(index)));
        }
        cells.extend(columns.iter().map(|column| (spec.cell)(index, column.id)));
        let row = Row::new(cells).height(spec.row_height);
        if selection_fg == Color::Reset || spec.selected != Some(index) {
            row
        } else {
            row.style(Style::default().fg(selection_fg))
        }
    });
    let table = Table::new(rows, constraints.clone())
        .header(header)
        .block(block)
        .column_spacing(COLUMN_SPACING)
        .row_highlight_style(
            Style::default()
                .bg(theme().selected_background)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(Line::styled(
            "\u{203a} ",
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        ))
        .highlight_spacing(HighlightSpacing::Always);
    let mut local_state = ratatui::widgets::TableState::default();
    if let Some(selected) = spec
        .selected
        .and_then(|row| row.checked_sub(spec.offset))
        .filter(|selected| *selected < visible_rows)
    {
        local_state.select(Some(selected));
    }
    frame.render_stateful_widget(table, area, &mut local_state);

    if inner.height < 2 {
        return;
    }
    // The blank row the header's bottom margin leaves, drawn as a rule: the
    // column names read as a heading over the rows rather than as a first row
    // among them.
    frame.render_widget(
        Line::styled(
            BorderType::border_symbols(theme().border_type)
                .horizontal_top
                .repeat(usize::from(inner.width)),
            Style::default().fg(theme().border),
        ),
        Rect::new(inner.x, inner.y.saturating_add(1), inner.width, 1),
    );
    let header_area = Rect::new(
        inner.x.saturating_add(SELECTION_WIDTH),
        inner.y,
        inner
            .width
            .saturating_sub(SELECTION_WIDTH)
            .saturating_sub(SCROLLBAR_WIDTH),
        1,
    );
    let header_columns = Layout::horizontal(constraints)
        .spacing(COLUMN_SPACING)
        .split(header_area);
    let column_areas: Vec<Rect> = header_columns
        .iter()
        .copied()
        .skip(usize::from(spec.marker.is_some()))
        .collect();
    for (header_rect, column) in column_areas.iter().zip(columns.iter()) {
        shell.hit_regions.push(region(
            *header_rect,
            PointerTarget::SortHeader(column.id.key()),
            spec.layer,
            None,
            None,
        ));
    }
    let body = geometry.body;
    shell.hit_regions.push(region(
        body,
        PointerTarget::FocusTickets,
        spec.layer,
        Some(spec.selectable),
        Some(spec.scroll),
    ));
    let rendered = spec.count.saturating_sub(spec.offset).min(visible_rows);
    for visible_index in 0..rendered {
        let logical = spec.offset + visible_index;
        let y = body
            .y
            .saturating_add(u16::try_from(visible_index).unwrap_or(u16::MAX) * spec.row_height);
        if y >= body.y.saturating_add(body.height) {
            break;
        }
        let row_rect = Rect::new(
            body.x,
            y,
            body.width.saturating_sub(1),
            spec.row_height
                .min(body.y.saturating_add(body.height).saturating_sub(y)),
        );
        shell.hit_regions.push(region(
            row_rect,
            PointerTarget::TableRow { index: logical },
            spec.layer,
            Some(spec.selectable),
            Some(spec.scroll),
        ));
        if spec.marker.is_some()
            && let Some(gutter) = header_columns.first()
        {
            shell.hit_regions.push(region(
                Rect::new(gutter.x, y, 3, 1),
                PointerTarget::ToggleRowSelect { index: logical },
                spec.layer,
                None,
                None,
            ));
            shell.hit_regions.push(region(
                Rect::new(gutter.x.saturating_add(3), y, 1, 1),
                PointerTarget::ToggleBookmark { index: logical },
                spec.layer,
                None,
                None,
            ));
        }
        if let Some(first) = column_areas.first() {
            shell.hit_regions.push(region(
                Rect::new(first.x, y, first.width, 1),
                PointerTarget::OpenInBrowser { index: logical },
                spec.layer,
                None,
                None,
            ));
        }
    }
    let overflow = spec.count > visible_rows;
    if overflow {
        render_scrollbar(
            frame,
            spec.layer,
            shell,
            body,
            spec.scroll,
            ScrollState {
                offset: spec.offset,
                content: spec.count,
                viewport: visible_rows,
            },
        );
    }
    capture_selectable(frame, shell, spec.selectable, body, overflow);
}

pub(super) fn render_table(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let count = screen.visible_count();
    let total = screen.tickets().len();
    let ordering = if screen.query().is_empty() || screen.search_order == SearchOrder::Field {
        format!("{} {}", screen.sort_field, screen.sort_direction.symbol())
    } else {
        format!(
            "Relevance → {} {}",
            screen.sort_field,
            screen.sort_direction.symbol()
        )
    };
    // The narrowest layout wears the pane switcher on this border instead of
    // the name, so the title here is what the wider ones show; what the table
    // is doing is shortened to fit the bottom border beside it.
    let (title, status) = if area.width < NARROW_BREAKPOINT {
        let short_order = if screen.query().is_empty() {
            screen.sort_direction.symbol()
        } else {
            match screen.search_order {
                SearchOrder::Relevance => "Rel",
                SearchOrder::Field => "Field",
            }
        };
        (
            " Tickets ".to_owned(),
            format!("{count}/{total} · {short_order}"),
        )
    } else {
        (
            " Tickets ".to_owned(),
            format!("{count}/{total} · {ordering}"),
        )
    };

    let now = OffsetDateTime::now_utc();
    // The same instant the relative labels read against, so a row's age and
    // whether it is flagged for that age are decided by one clock.
    let table_now = Timestamp::from_offset_date_time(now);
    let density = screen.row_density;
    let row_height = density.row_height();
    let geometry = table_geometry(area, row_height);
    screen.set_table_viewport(geometry.visible_rows);
    let offset = screen.table.offset;
    let layer = current_layer(screen);
    let fuzzy = screen.fuzzy_query();

    // Everything the rows on screen need, read before the table takes the
    // shell: what a row says about itself is the screen's business, and the
    // list table only asks for cells.
    let rows: Vec<PaintedRow<'_>> = screen
        .visible_tickets()
        .skip(offset)
        .take(geometry.visible_rows)
        .map(|ticket| PaintedRow {
            ticket,
            checked: screen.is_row_selected(&ticket.key),
            bookmarked: screen.is_bookmarked(&ticket.key),
            flashing: shell.flashing_row(&ticket.key),
            context: RowContext {
                tone: RowTone::of(&ticket.state),
                mine: shell.is_mine(ticket),
                progress: screen.child_progress(&ticket.key),
                stale: screen.stale_age_days_at(ticket, table_now).is_some(),
            },
        })
        .collect();

    let mut highlighter = QueryHighlighter::new(&fuzzy);
    let marker = |index: usize| {
        rows.get(index.saturating_sub(offset))
            .map_or_else(Line::default, |row| {
                row_marker_line(row.checked, row.bookmarked, row.flashing)
            })
    };
    let mut cell = |index: usize, column: SortField| {
        rows.get(index.saturating_sub(offset)).map_or_else(
            || Cell::from(""),
            |row| {
                table_cell(
                    row.ticket,
                    column,
                    now,
                    density,
                    row.context,
                    &mut highlighter,
                )
            },
        )
    };
    let mut spec = TableSpec {
        title,
        status,
        focused: shell.focus == Focus::Tickets,
        layout: &screen.layout,
        sorted: Some((screen.sort_field, screen.sort_direction.symbol())),
        count,
        offset,
        selected: screen.selected_row(),
        row_height,
        layer,
        scroll: ScrollSurface::Table,
        selectable: SelectableSurface::Table,
        marker: Some(&marker),
        cell: &mut cell,
    };
    render_list_table(frame, shell, area, &mut spec);

    let inner = geometry.inner;
    if count == 0 && inner.height > 2 {
        // Counting the hidden rows is a pass over every ticket, so it happens
        // on the one branch that says the number and nowhere else.
        let hidden_finished_message;
        let message = if shell.sync_pending {
            "Syncing with Azure DevOps…"
        } else if shell.reload_pending {
            "Reloading tickets…"
        } else if !screen.parsed_query().is_active() {
            match screen.hidden_finished(shell) {
                0 => "No tickets in this database",
                // Everything on file is finished and hidden: say so, and how
                // to see it, rather than claiming the database is empty.
                hidden => {
                    hidden_finished_message = format!(
                        "All {hidden} tickets are finished and hidden \u{2014} click the chip's \u{00d7} or use the palette to show them"
                    );
                    &hidden_finished_message
                }
            }
        } else if screen.search_pending {
            "Searching…"
        } else {
            "No tickets match this search"
        };
        frame.render_widget(
            Paragraph::new(message)
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme().muted)),
            Rect::new(
                inner.x,
                inner.y.saturating_add(inner.height / 2),
                inner.width,
                1,
            ),
        );
    }
}

/// One row of the work item table, as the list table asks for it.
struct PaintedRow<'a> {
    ticket: &'a Ticket,
    checked: bool,
    bookmarked: bool,
    /// Whether an edit of this row has just landed or just been taken back.
    flashing: bool,
    context: RowContext,
}

/// How strongly a row is painted: finished work fades so open work stands out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RowTone {
    Normal,
    Muted,
}

impl RowTone {
    pub(super) fn of(state: &str) -> Self {
        if StateCategory::of(state).is_done() {
            Self::Muted
        } else {
            Self::Normal
        }
    }

    /// Fade a style. Colour themes drop to the muted foreground; the monochrome
    /// theme has no muted colour, so it dims instead. Bold goes either way, so
    /// weight alone still separates open work from finished work.
    pub(super) fn apply(self, style: Style) -> Style {
        if self == Self::Normal {
            return style;
        }
        let style = style.remove_modifier(Modifier::BOLD);
        if theme().muted == Color::Reset {
            style.add_modifier(Modifier::DIM)
        } else {
            style.fg(theme().muted)
        }
    }
}

/// What a row knows about itself beyond the work item: how strongly it is
/// painted, whether it is the signed-in user's, how far its children have got,
/// and whether it has sat untouched past the stale threshold.
#[derive(Clone, Copy)]
pub(super) struct RowContext {
    tone: RowTone,
    mine: bool,
    progress: Option<ChildProgress>,
    stale: bool,
}

pub(super) fn table_cell(
    ticket: &Ticket,
    field: SortField,
    now: OffsetDateTime,
    density: RowDensity,
    row: RowContext,
    highlighter: &mut QueryHighlighter,
) -> Cell<'static> {
    let RowContext {
        tone,
        mine,
        progress,
        stale,
    } = row;
    let plain = tone.apply(Style::default());
    let line = match field {
        SortField::Type => Line::from(type_badge_spans(&ticket.work_item_type, tone, highlighter)),
        SortField::Title => highlight_searchable(&ticket.title, plain, highlighter),
        SortField::Id => {
            let style = tone.apply(
                Style::default()
                    .fg(theme().link)
                    .add_modifier(Modifier::UNDERLINED),
            );
            terminate_underline(highlight_searchable(
                &ticket.key.id.to_string(),
                style,
                highlighter,
            ))
        }
        // Finished rows recede whole: the state cell fades with the rest of
        // the row rather than staying bright against muted neighbours. The
        // glyph in front of the word is the family tree's, so one state reads
        // the same in both, and it says which state this is under NO_COLOR,
        // where the colour cannot.
        SortField::State => {
            let style = tone.apply(state_style(&ticket.state));
            let mut line = highlight_searchable(&ticket.state, style, highlighter);
            let glyph = state_glyph(StateCategory::of(&ticket.state));
            if !glyph.is_empty() {
                line.spans
                    .insert(0, Span::styled(format!("{glyph} "), style));
            }
            line
        }
        SortField::Assignee => match ticket.assigned_to.as_deref() {
            Some(name) if mine => {
                highlight_searchable(name, tone.apply(assigned_to_me_style()), highlighter)
            }
            Some(name) => highlight_searchable(name, plain, highlighter),
            None => Line::styled("Unassigned", tone.apply(Style::default().fg(theme().muted))),
        },
        // `P1` rather than a bare `1`: a number alone in a narrow column reads
        // as a count, and the header has no room to say what it counts.
        SortField::Priority => Line::from(
            ticket
                .priority
                .map_or_else(|| "—".into(), |priority| format!("P{priority}")),
        )
        .right_aligned()
        .style(tone.apply(priority_style(ticket.priority))),
        SortField::Changed => Line::from(ticket.changed_at.relative_to(now))
            .right_aligned()
            .style(changed_style(plain, stale)),
        SortField::Created => Line::from(ticket.created_at.relative_to(now))
            .right_aligned()
            .style(plain),
        // Only the leaf fits a table column; the details pane keeps the full path.
        SortField::Area => highlight_searchable(path_leaf(&ticket.area_path), plain, highlighter),
        SortField::Iteration => {
            highlight_searchable(path_leaf(&ticket.iteration_path), plain, highlighter)
        }
        SortField::Tags => Line::from(tag_badge_spans(&ticket.tags, tone, highlighter)),
        // A work item with no children shows an empty cell rather than `0/0`:
        // there is no progress to report on work that was never broken down.
        SortField::Progress => Line::from(progress.map(ChildProgress::ratio).unwrap_or_default())
            .right_aligned()
            .style(progress_style(plain, progress)),
    };

    if density == RowDensity::Comfortable && field == SortField::Title {
        Cell::from(Text::from(vec![
            line,
            Line::from(tag_badge_spans(&ticket.tags, tone, highlighter)),
        ]))
    } else {
        Cell::from(line)
    }
}

pub(super) fn highlight_searchable(
    text: &str,
    style: Style,
    highlighter: &mut QueryHighlighter,
) -> Line<'static> {
    highlight_line(
        text.to_owned(),
        &highlighter.indices(text),
        style,
        search_match_style(style),
    )
}

pub(super) fn search_match_style(base: Style) -> Style {
    let style = base.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    if base.fg == Some(Color::Reset) || base.fg.is_none() {
        style.fg(theme().search_match)
    } else {
        style
    }
}

pub(super) fn highlight_line(
    text: String,
    indices: &[u32],
    base: Style,
    matched: Style,
) -> Line<'static> {
    if indices.is_empty() {
        // The style rides on the span so callers that harvest `spans` — the
        // badges, the details pane — keep the colour.
        return Line::from(Span::styled(text, base));
    }

    let mut spans = Vec::new();
    let mut current = String::new();
    let mut current_matched = false;
    let mut next_index = 0;
    for (index, character) in text.chars().enumerate() {
        let is_match = loop {
            if next_index >= indices.len() {
                break false;
            }
            match indices[next_index].cmp(&(index as u32)) {
                Ordering::Less => next_index += 1,
                Ordering::Equal => break true,
                Ordering::Greater => break false,
            }
        };
        if !current.is_empty() && is_match != current_matched {
            let style = if current_matched { matched } else { base };
            spans.push(Span::styled(std::mem::take(&mut current), style));
        }
        current.push(character);
        current_matched = is_match;
    }
    if !current.is_empty() {
        let style = if current_matched { matched } else { base };
        spans.push(Span::styled(current, style));
    }
    Line::from(spans)
}

/// Colour a work item type across the Agile, Basic, Scrum, and CMMI processes.
pub(super) fn type_style(work_item_type: &str) -> Style {
    let color = match work_item_type.trim().to_ascii_lowercase().as_str() {
        "epic" => theme().type_epic,
        "feature" => theme().type_feature,
        "issue" | "user story" | "story" | "product backlog item" => theme().type_story,
        "task" => theme().type_task,
        "bug" | "impediment" => theme().type_bug,
        "test case" => theme().type_test,
        // Custom types stay readable rather than fading into the background.
        _ => theme().text,
    };
    Style::default().fg(color)
}

pub(super) fn type_badge_spans(
    work_item_type: &str,
    tone: RowTone,
    highlighter: &mut QueryHighlighter,
) -> Vec<Span<'static>> {
    badge_spans(
        work_item_type,
        type_style(work_item_type),
        tone,
        highlighter,
    )
}

pub(super) fn tag_badge_spans(
    tags: &[String],
    tone: RowTone,
    highlighter: &mut QueryHighlighter,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, tag) in tags.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        spans.extend(badge_spans(
            tag,
            Style::default().fg(tag_color(tag)),
            tone,
            highlighter,
        ));
    }
    spans
}

/// Hash a tag onto a stable palette entry, ignoring case.
///
/// FNV-1a over the lowercased bytes keeps the mapping deterministic between
/// runs and between panes without reaching for a hasher dependency.
pub(super) fn tag_color(tag: &str) -> Color {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in tag.bytes() {
        hash ^= u32::from(byte.to_ascii_lowercase());
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let palette = theme().tag_palette;
    palette[usize::try_from(hash).unwrap_or_default() % palette.len()]
}

pub(super) fn badge_spans(
    label: &str,
    style: Style,
    tone: RowTone,
    highlighter: &mut QueryHighlighter,
) -> Vec<Span<'static>> {
    let bracket = tone.apply(style);
    let inner = highlight_searchable(
        label,
        tone.apply(style.add_modifier(Modifier::BOLD)),
        highlighter,
    );
    let mut spans = Vec::with_capacity(inner.spans.len() + 2);
    spans.push(Span::styled("[", bracket));
    spans.extend(inner.spans);
    spans.push(Span::styled("]", bracket));
    spans
}

pub(super) fn state_color(category: StateCategory) -> Color {
    match category {
        StateCategory::Proposed => theme().state_proposed,
        StateCategory::InProgress => theme().state_in_progress,
        StateCategory::Resolved => theme().state_resolved,
        StateCategory::Completed => theme().state_completed,
        StateCategory::Removed => theme().state_removed,
        // Custom states stay readable rather than fading into the background.
        StateCategory::Unknown => theme().text,
    }
}

pub(super) fn state_style(state: &str) -> Style {
    state_category_style(StateCategory::of(state))
}

/// The State column's styling for a category Azure DevOps named, rather than
/// one guessed from the state's own text. Under NO_COLOR every colour is
/// `Reset`, so the weight carries the distinction on its own.
pub(super) fn state_category_style(category: StateCategory) -> Style {
    Style::default()
        .fg(state_color(category))
        .add_modifier(Modifier::BOLD)
}

pub(super) fn priority_style(priority: Option<i64>) -> Style {
    let color = match priority {
        Some(1) => theme().priority_critical,
        Some(2) => theme().priority_high,
        Some(3 | 4) => theme().priority_normal,
        _ => theme().muted,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

/// `Children: 3/7 done  ▆▆▆░░░` — how far a work item's direct children have
/// got, with a bar a few cells wide beside the ratio.
///
/// Nothing here leans on colour: the filled and the empty cells are different
/// glyphs and a finished parent goes bold as well as green, so the bar reads
/// the same under NO_COLOR as it does in the colour theme.
pub(super) fn child_progress_line(progress: ChildProgress) -> Line<'static> {
    let mut value = Style::default();
    if progress.is_complete() {
        value = value
            .fg(state_color(StateCategory::Completed))
            .add_modifier(Modifier::BOLD);
    }
    Line::from(vec![
        field_label("Children"),
        Span::styled(format!("{} done", progress.ratio()), value),
        Span::raw("  "),
        Span::styled(progress_bar(progress, PROGRESS_BAR_CELLS), value),
    ])
}

/// The bar itself: whole blocks for the share that is finished, one partial
/// block for what is left over, and hollow cells for the rest. Eight steps to
/// a cell, so a bar six cells wide still tells `3/7` from `4/7`.
pub(super) fn progress_bar(progress: ChildProgress, width: usize) -> String {
    const PARTS: [&str; 8] = [
        "\u{258f}", "\u{258e}", "\u{258d}", "\u{258c}", "\u{258b}", "\u{258a}", "\u{2589}",
        "\u{2588}",
    ];
    let eighths = progress.filled_eighths(width);
    let whole = eighths / 8;
    let rest = eighths % 8;
    let mut bar = "\u{2588}".repeat(whole);
    if rest > 0 {
        bar.push_str(PARTS[rest - 1]);
    }
    bar.push_str(
        &"\u{2591}".repeat(
            width
                .saturating_sub(whole)
                .saturating_sub(usize::from(rest > 0)),
        ),
    );
    bar
}

/// A finished parent's ratio goes green and bold in the table too, so the
/// column reads at a glance without anybody comparing the two numbers.
pub(super) fn progress_style(plain: Style, progress: Option<ChildProgress>) -> Style {
    if progress.is_some_and(ChildProgress::is_complete) {
        return plain
            .fg(state_color(StateCategory::Completed))
            .add_modifier(Modifier::BOLD);
    }
    plain
}

/// The Changed column's styling. Work nobody has touched past the stale
/// threshold goes warning-coloured and bold; bold is what carries it under
/// NO_COLOR, where the palette is all `Reset`.
///
/// Nothing else paints this cell, so the row's own tone is the only styling to
/// rank against, and staleness wins it — but the two can never actually meet:
/// [`RowTone::Muted`] is the finished rows, and a finished work item is never
/// stale however long it has sat.
pub(super) fn changed_style(plain: Style, stale: bool) -> Style {
    if stale {
        return plain
            .fg(theme().warning)
            .add_modifier(Modifier::BOLD)
            .remove_modifier(Modifier::DIM);
    }
    plain
}

pub(super) fn row_marker_line(checked: bool, bookmarked: bool, flashing: bool) -> Line<'static> {
    let check = if checked { "[x]" } else { "[ ]" };
    let star = if bookmarked { "*" } else { " " };
    // An edit that has just landed, or just been taken back, leaves the row's
    // gutter in the accent for a couple of frames: a row several away from
    // the cursor still says that something happened to it.
    let gutter = if flashing {
        Style::default()
            .fg(theme().accent)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(check, gutter),
        Span::styled(
            star,
            if bookmarked {
                gutter.fg(theme().accent).add_modifier(Modifier::BOLD)
            } else {
                gutter.fg(theme().muted)
            },
        ),
    ])
}
