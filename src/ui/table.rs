//! The work item table: its rows, its cells and the colours they carry.

use super::*;

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
    let activity = shell
        .activity_label()
        .map_or_else(String::new, |label| format!(" · {label}"));
    let title = if area.width < NARROW_BREAKPOINT {
        let short_order = if screen.query().is_empty() {
            screen.sort_direction.symbol()
        } else {
            match screen.search_order {
                SearchOrder::Relevance => "Rel",
                SearchOrder::Field => "Field",
            }
        };
        format!(" [Tickets] [Details] {count}/{total} · {short_order}{activity} ")
    } else {
        format!(" Tickets {count}/{total} · {ordering}{activity} ")
    };
    let block = focused_block(title, shell.focus == Focus::Tickets);
    let inner = block.inner(area);
    let columns = screen.layout.visible_columns(inner.width.saturating_sub(5));
    let mut constraints = vec![Constraint::Length(4)];
    constraints.extend(
        columns
            .iter()
            .copied()
            .map(crate::columns::TableLayout::constraint),
    );

    let mut header_cells = vec![Cell::from("")];
    header_cells.extend(columns.iter().map(|column| {
        let direction = if column.id == screen.sort_field {
            screen.sort_direction.symbol()
        } else {
            ""
        };
        let label = match column.id {
            SortField::Priority => "Pri",
            SortField::Organization => "Org",
            _ => column.id.label(),
        };
        let line = Line::from(format!("{label}{direction}"));
        Cell::from(if column.id.is_numeric() {
            line.right_aligned()
        } else {
            line
        })
    }));
    let header = Row::new(header_cells)
        .style(
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        )
        .height(1)
        .bottom_margin(1);

    let now = OffsetDateTime::now_utc();
    // The same instant the relative labels read against, so a row's age and
    // whether it is flagged for that age are decided by one clock.
    let table_now = Timestamp::from_offset_date_time(now);
    let density = screen.row_density;
    let row_height = density.row_height();
    let body_height = inner.height.saturating_sub(2);
    let visible_rows = usize::from(body_height / row_height).max(1);
    screen.set_table_viewport(visible_rows);
    let offset = screen.table.offset;
    let selected = screen.selected_row();
    let fuzzy = screen.fuzzy_query();
    let mut highlighter = QueryHighlighter::new(&fuzzy);
    let tickets: Vec<&Ticket> = screen.visible_tickets().collect();
    let slice = tickets
        .get(offset..)
        .unwrap_or(&[])
        .iter()
        .copied()
        .take(visible_rows);
    let rows = slice.map(|ticket| {
        let bookmarked = screen.is_bookmarked(&ticket.key);
        let checked = screen.is_row_selected(&ticket.key);
        let mut cells = vec![Cell::from(row_marker_line(checked, bookmarked))];
        let row = RowContext {
            tone: RowTone::of(&ticket.state),
            mine: shell.is_mine(ticket),
            progress: screen.child_progress(&ticket.key),
            stale: screen.stale_age_days_at(ticket, table_now).is_some(),
        };
        cells.extend(
            columns
                .iter()
                .map(|column| table_cell(ticket, column.id, now, density, row, &mut highlighter)),
        );
        Row::new(cells).height(row_height)
    });
    let table = Table::new(rows, constraints.clone())
        .header(header)
        .block(block)
        .column_spacing(1)
        .row_highlight_style(
            Style::default()
                .bg(theme().selected_background)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ")
        .highlight_spacing(HighlightSpacing::Always);
    let mut local_state = ratatui::widgets::TableState::default();
    if let Some(selected) = selected.and_then(|row| row.checked_sub(offset))
        && selected < visible_rows
    {
        local_state.select(Some(selected));
    }
    frame.render_stateful_widget(table, area, &mut local_state);

    if area.width < NARROW_BREAKPOINT {
        register_narrow_tabs(shell, area);
    }
    if inner.height >= 2 {
        let header_area = Rect::new(
            inner.x.saturating_add(2),
            inner.y,
            inner.width.saturating_sub(2),
            1,
        );
        let header_columns = Layout::horizontal(constraints)
            .spacing(1)
            .split(header_area);
        for (header_rect, column) in header_columns.iter().skip(1).zip(columns.iter()) {
            shell.hit_regions.push(region(
                *header_rect,
                PointerTarget::SortHeader(column.id),
                PointerLayer::Base,
                None,
                None,
            ));
        }
        let body = Rect::new(inner.x, inner.y.saturating_add(2), inner.width, body_height);
        shell.hit_regions.push(region(
            body,
            PointerTarget::FocusTickets,
            PointerLayer::Base,
            Some(SelectableSurface::Table),
            Some(ScrollSurface::Table),
        ));
        let rendered = count.saturating_sub(offset).min(visible_rows);
        for visible_index in 0..rendered {
            let logical = offset + visible_index;
            let y = body
                .y
                .saturating_add(u16::try_from(visible_index).unwrap_or(u16::MAX) * row_height);
            if y >= body.y.saturating_add(body.height) {
                break;
            }
            let row_rect = Rect::new(
                body.x,
                y,
                body.width.saturating_sub(1),
                row_height.min(body.y.saturating_add(body.height).saturating_sub(y)),
            );
            shell.hit_regions.push(region(
                row_rect,
                PointerTarget::TableRow { index: logical },
                PointerLayer::Base,
                Some(SelectableSurface::Table),
                Some(ScrollSurface::Table),
            ));
            if let Some(marker) = header_columns.first() {
                shell.hit_regions.push(region(
                    Rect::new(marker.x, y, 3, 1),
                    PointerTarget::ToggleRowSelect { index: logical },
                    PointerLayer::Base,
                    None,
                    None,
                ));
                shell.hit_regions.push(region(
                    Rect::new(marker.x.saturating_add(3), y, 1, 1),
                    PointerTarget::ToggleBookmark { index: logical },
                    PointerLayer::Base,
                    None,
                    None,
                ));
            }
            if let Some(id_area) = header_columns.get(1) {
                shell.hit_regions.push(region(
                    Rect::new(id_area.x, y, id_area.width, 1),
                    PointerTarget::OpenTicket { index: logical },
                    PointerLayer::Base,
                    None,
                    None,
                ));
            }
        }
        let overflow = count > visible_rows;
        if overflow {
            render_scrollbar(
                frame,
                screen,
                shell,
                body,
                ScrollSurface::Table,
                ScrollState {
                    offset,
                    content: count,
                    viewport: visible_rows,
                },
            );
        }
        capture_selectable(frame, shell, SelectableSurface::Table, body, overflow);
    }

    if count == 0 && inner.height > 2 {
        let message = if shell.sync_pending {
            "Syncing with Azure DevOps…"
        } else if shell.reload_pending {
            "Reloading tickets…"
        } else if !screen.parsed_query().is_active() {
            "No tickets in this database"
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
        // the row rather than staying bright against muted neighbours.
        SortField::State => highlight_searchable(
            &ticket.state,
            tone.apply(state_style(&ticket.state)),
            highlighter,
        ),
        SortField::Assignee => match ticket.assigned_to.as_deref() {
            Some(name) if mine => {
                highlight_searchable(name, tone.apply(assigned_to_me_style()), highlighter)
            }
            Some(name) => highlight_searchable(name, plain, highlighter),
            None => Line::styled("Unassigned", tone.apply(Style::default().fg(theme().muted))),
        },
        SortField::Priority => Line::from(
            ticket
                .priority
                .map_or_else(|| "—".into(), |priority| priority.to_string()),
        )
        .right_aligned()
        .style(tone.apply(priority_style(ticket.priority))),
        SortField::Changed => Line::from(ticket.changed_at.relative_to(now))
            .right_aligned()
            .style(changed_style(plain, stale)),
        SortField::Created => Line::from(ticket.created_at.relative_to(now))
            .right_aligned()
            .style(plain),
        SortField::Organization => {
            highlight_searchable(&ticket.key.organization, plain, highlighter)
        }
        SortField::Project => highlight_searchable(&ticket.project, plain, highlighter),
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
        Span::styled("Children: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("{} done", progress.ratio()), value),
        Span::raw("  "),
        Span::styled(progress_bar(progress, PROGRESS_BAR_CELLS), value),
    ])
}

/// The bar itself: filled cells for the share that is finished, hollow ones
/// for the rest.
pub(super) fn progress_bar(progress: ChildProgress, width: usize) -> String {
    let filled = progress.filled_cells(width);
    let mut bar = "\u{2586}".repeat(filled);
    bar.push_str(&"\u{2591}".repeat(width.saturating_sub(filled)));
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

pub(super) fn row_marker_line(checked: bool, bookmarked: bool) -> Line<'static> {
    let check = if checked { "[x]" } else { "[ ]" };
    let star = if bookmarked { "*" } else { " " };
    Line::from(vec![
        Span::raw(check),
        Span::styled(
            star,
            if bookmarked {
                Style::default()
                    .fg(theme().accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme().muted)
            },
        ),
    ])
}
