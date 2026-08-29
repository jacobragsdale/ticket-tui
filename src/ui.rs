use std::cmp::Ordering;
use std::sync::OnceLock;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Table, Wrap,
};
use time::OffsetDateTime;

use crate::app::{
    App, AppMode, DividerOrientation, Focus, HitRegions, NotificationLevel, RowDensity, SearchOrder,
};
use crate::command::COMMANDS;
use crate::filter::{FacetTarget, FilterField};
use crate::model::{
    FamilySnapshot, FamilyTreeEntry, SortDirection, SortField, StateCategory, Ticket, TicketKey,
    path_leaf,
};
use crate::pointer::{
    PointerLayer, PointerTarget, ScrollMetrics, ScrollSurface, SelectableSnapshot,
    SelectableSurface, region,
};
use crate::search::QueryHighlighter;

const WIDE_BREAKPOINT: u16 = 110;
const NARROW_BREAKPOINT: u16 = 70;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Theme {
    accent: Color,
    muted: Color,
    text: Color,
    body: Color,
    link: Color,
    selected_background: Color,
    /// A dimmer wash than `selected_background`, laid under a hovered row so
    /// its colour-coded cells keep their own foregrounds.
    hover_background: Color,
    info: Color,
    error: Color,
    scrollbar: Color,
    search_match: Color,
    state_proposed: Color,
    state_in_progress: Color,
    state_resolved: Color,
    state_completed: Color,
    state_removed: Color,
    type_epic: Color,
    type_feature: Color,
    type_story: Color,
    type_task: Color,
    type_bug: Color,
    type_test: Color,
    priority_critical: Color,
    priority_high: Color,
    priority_normal: Color,
    /// Restrained badge colours a tag is hashed into, so one tag always reads
    /// the same wherever it appears.
    tag_palette: [Color; 6],
}

impl Theme {
    const fn new(monochrome: bool) -> Self {
        if monochrome {
            Self {
                accent: Color::Reset,
                muted: Color::Reset,
                text: Color::Reset,
                body: Color::Reset,
                link: Color::Reset,
                selected_background: Color::Reset,
                hover_background: Color::Reset,
                info: Color::Reset,
                error: Color::Reset,
                scrollbar: Color::Reset,
                search_match: Color::Reset,
                state_proposed: Color::Reset,
                state_in_progress: Color::Reset,
                state_resolved: Color::Reset,
                state_completed: Color::Reset,
                state_removed: Color::Reset,
                type_epic: Color::Reset,
                type_feature: Color::Reset,
                type_story: Color::Reset,
                type_task: Color::Reset,
                type_bug: Color::Reset,
                type_test: Color::Reset,
                priority_critical: Color::Reset,
                priority_high: Color::Reset,
                priority_normal: Color::Reset,
                tag_palette: [Color::Reset; 6],
            }
        } else {
            Self {
                accent: Color::Cyan,
                muted: Color::DarkGray,
                text: Color::White,
                body: Color::Gray,
                link: Color::Blue,
                selected_background: Color::DarkGray,
                hover_background: Color::Indexed(237),
                info: Color::Yellow,
                error: Color::Red,
                scrollbar: Color::DarkGray,
                search_match: Color::Yellow,
                state_proposed: Color::Blue,
                state_in_progress: Color::Yellow,
                state_resolved: Color::Magenta,
                state_completed: Color::Green,
                state_removed: Color::DarkGray,
                type_epic: Color::Yellow,
                type_feature: Color::Magenta,
                type_story: Color::Blue,
                type_task: Color::Cyan,
                type_bug: Color::Red,
                type_test: Color::Green,
                priority_critical: Color::Red,
                priority_high: Color::Yellow,
                priority_normal: Color::Blue,
                tag_palette: [
                    Color::Cyan,
                    Color::Blue,
                    Color::Magenta,
                    Color::Green,
                    Color::Yellow,
                    Color::White,
                ],
            }
        }
    }
}

fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| Theme::new(std::env::var_os("NO_COLOR").is_some()))
}

pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    render_pass(frame, app);
    if app.refresh_hover() {
        render_pass(frame, app);
    }
    paint_hover(frame, app);
    paint_selection(frame, app);
}

fn render_pass(frame: &mut Frame<'_>, app: &mut App) {
    app.hit_regions = HitRegions::default();
    let area = frame.area();
    if area.width < 36 || area.height < 10 {
        frame.render_widget(
            Paragraph::new("Terminal too small\nResize to at least 36 × 10")
                .alignment(Alignment::Center)
                .block(Block::bordered().title("ticket-tui")),
            area,
        );
        return;
    }

    let chip_height = u16::from(!app.overflow_filter_tokens().is_empty());
    let sections = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(chip_height),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, app, sections[0]);
    render_facet_bar(frame, app, sections[1]);
    if chip_height > 0 {
        render_chips(frame, app, sections[2]);
    }
    render_content(frame, app, sections[3]);
    render_footer(frame, app, sections[4]);

    match app.mode {
        AppMode::Sort => render_sort_popup(frame, app),
        AppMode::Help => render_help_popup(frame, app),
        AppMode::Filter => render_filter_overlay(frame, app),
        AppMode::Columns => render_column_overlay(frame, app),
        AppMode::Palette => render_palette(frame, app),
        AppMode::Views => render_views_overlay(frame, app),
        AppMode::Info => render_info_overlay(frame, app),
        AppMode::Facets => render_facet_menu(frame, app),
        AppMode::Browse | AppMode::Search => {}
    }
}

fn render_search(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let active = app.mode == AppMode::Search;
    let title = if app.search_pending {
        " Search (matching…) "
    } else {
        " Search / "
    };
    let mut block = focused_block(title, active);
    let actions_width = 11;
    let help_width = 4;
    let mut right_title = String::new();
    if area.width >= 48 {
        right_title.push_str("[Actions] ");
    }
    if area.width >= 36 {
        right_title.push_str("[?]");
    }
    if !right_title.is_empty() {
        block = block.title(Line::from(right_title.clone()).right_aligned());
    }
    let inner = block.inner(area);
    let clear = if !app.query().is_empty() && inner.width > 4 {
        3
    } else {
        0
    };
    let field = Rect::new(
        inner.x,
        inner.y,
        inner.width.saturating_sub(clear),
        inner.height.max(1),
    );
    let text = if app.query().is_empty() && !active {
        Line::styled(
            "Type / to search, or pick State, Type, Tags, or Assignee below",
            Style::default().fg(theme().muted),
        )
    } else {
        Line::from(app.query())
    };
    let cursor_offset = u16::try_from(app.query_cursor()).unwrap_or(u16::MAX);
    let horizontal_scroll = cursor_offset.saturating_sub(field.width.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(text)
            .block(block)
            .scroll((0, horizontal_scroll)),
        area,
    );
    if clear > 0 {
        let clear_area = Rect::new(
            inner.x.saturating_add(inner.width.saturating_sub(3)),
            inner.y,
            3,
            1,
        );
        render_control(
            frame,
            app,
            clear_area,
            "[×]",
            PointerTarget::ClearQuery,
            PointerLayer::Base,
            true,
        );
    }
    app.hit_regions.push(region(
        field,
        PointerTarget::SearchField,
        PointerLayer::Base,
        Some(SelectableSurface::Search),
        None,
    ));
    if area.width >= 48 {
        let actions = Rect::new(
            area.x
                .saturating_add(area.width.saturating_sub(actions_width + help_width)),
            area.y,
            actions_width.saturating_sub(1),
            1,
        );
        app.hit_regions.push(region(
            actions,
            PointerTarget::OpenPalette,
            PointerLayer::Base,
            None,
            None,
        ));
    }
    if area.width >= 36 {
        let help = Rect::new(
            area.x.saturating_add(area.width.saturating_sub(5)),
            area.y,
            3,
            1,
        );
        app.hit_regions.push(region(
            help,
            PointerTarget::OpenHelp,
            PointerLayer::Base,
            None,
            None,
        ));
    }
    capture_selectable(frame, app, SelectableSurface::Search, field, false);

    if active {
        let cursor_x = field
            .x
            .saturating_add(cursor_offset.saturating_sub(horizontal_scroll));
        frame.set_cursor_position((
            cursor_x.min(field.x.saturating_add(field.width.saturating_sub(1))),
            field.y,
        ));
    }
}

fn render_content(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    if area.width >= WIDE_BREAKPOINT {
        app.set_content_layout(area, Some(DividerOrientation::Vertical));
        let panes = Layout::horizontal([
            Constraint::Percentage(app.pane_split_wide),
            Constraint::Fill(1),
        ])
        .spacing(1)
        .split(area);
        render_table(frame, app, panes[0]);
        render_details(frame, app, panes[1]);
        render_divider(frame, app, panes[0], panes[1], DividerOrientation::Vertical);
    } else if area.width >= NARROW_BREAKPOINT {
        app.set_content_layout(area, Some(DividerOrientation::Horizontal));
        let panes = Layout::vertical([
            Constraint::Percentage(app.pane_split_stacked),
            Constraint::Fill(1),
        ])
        .spacing(1)
        .split(area);
        render_table(frame, app, panes[0]);
        render_details(frame, app, panes[1]);
        render_divider(
            frame,
            app,
            panes[0],
            panes[1],
            DividerOrientation::Horizontal,
        );
    } else {
        app.set_content_layout(area, None);
        if app.narrow_details {
            render_details(frame, app, area);
        } else {
            render_table(frame, app, area);
        }
    }
}

/// Paints the gap the layout leaves between the panes and registers it as the
/// draggable divider. Hovering reverses it through the usual hover pass.
fn render_divider(
    frame: &mut Frame<'_>,
    app: &mut App,
    first: Rect,
    second: Rect,
    orientation: DividerOrientation,
) {
    let Some(rect) = divider_rect(first, second, orientation) else {
        return;
    };
    let glyph = match orientation {
        DividerOrientation::Vertical => "\u{2502}",
        DividerOrientation::Horizontal => "\u{2500}",
    };
    let style = Style::default().fg(theme().muted);
    let row = glyph.repeat(usize::from(rect.width));
    let lines: Vec<Line<'_>> = (0..rect.height)
        .map(|_| Line::styled(row.clone(), style))
        .collect();
    frame.render_widget(Paragraph::new(Text::from(lines)), rect);
    app.hit_regions.push(region(
        rect,
        PointerTarget::PaneDivider,
        PointerLayer::Base,
        None,
        None,
    ));
}

/// The cells the layout left between two panes, if any.
fn divider_rect(first: Rect, second: Rect, orientation: DividerOrientation) -> Option<Rect> {
    let rect = match orientation {
        DividerOrientation::Vertical => Rect {
            x: first.right(),
            y: first.y,
            width: second.x.checked_sub(first.right())?,
            height: first.height,
        },
        DividerOrientation::Horizontal => Rect {
            x: first.x,
            y: first.bottom(),
            width: first.width,
            height: second.y.checked_sub(first.bottom())?,
        },
    };
    (rect.width > 0 && rect.height > 0).then_some(rect)
}

fn render_table(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let count = app.visible_count();
    let total = app.tickets().len();
    let ordering = if app.query().is_empty() || app.search_order == SearchOrder::Field {
        format!("{} {}", app.sort_field, app.sort_direction.symbol())
    } else {
        format!(
            "Relevance → {} {}",
            app.sort_field,
            app.sort_direction.symbol()
        )
    };
    let activity = if app.reload_pending {
        " · Reloading…"
    } else if app.stale {
        " · Stale"
    } else {
        ""
    };
    let title = if area.width < NARROW_BREAKPOINT {
        let short_order = if app.query().is_empty() {
            app.sort_direction.symbol()
        } else {
            match app.search_order {
                SearchOrder::Relevance => "Rel",
                SearchOrder::Field => "Field",
            }
        };
        format!(" [Tickets] [Details] {count}/{total} · {short_order}{activity} ")
    } else {
        format!(" Tickets {count}/{total} · {ordering}{activity} ")
    };
    let block = focused_block(title, app.focus == Focus::Tickets);
    let inner = block.inner(area);
    let columns = app.layout.visible_columns(inner.width.saturating_sub(5));
    let mut constraints = vec![Constraint::Length(4)];
    constraints.extend(
        columns
            .iter()
            .copied()
            .map(crate::columns::TableLayout::constraint),
    );

    let mut header_cells = vec![Cell::from("")];
    header_cells.extend(columns.iter().map(|column| {
        let direction = if column.id == app.sort_field {
            app.sort_direction.symbol()
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
    let density = app.row_density;
    let row_height = density.row_height();
    let body_height = inner.height.saturating_sub(2);
    let visible_rows = usize::from(body_height / row_height).max(1);
    app.set_table_viewport(visible_rows);
    let offset = app.table.offset;
    let selected = app.selected_row();
    let fuzzy = app.fuzzy_query();
    let mut highlighter = QueryHighlighter::new(&fuzzy);
    let tickets: Vec<&Ticket> = app.visible_tickets().collect();
    let slice = tickets
        .get(offset..)
        .unwrap_or(&[])
        .iter()
        .copied()
        .take(visible_rows);
    let rows = slice.map(|ticket| {
        let bookmarked = app.is_bookmarked(&ticket.key);
        let checked = app.is_row_selected(&ticket.key);
        let mut cells = vec![Cell::from(row_marker_line(checked, bookmarked))];
        let tone = RowTone::of(&ticket.state);
        let mine = app.is_mine(ticket);
        cells.extend(columns.iter().map(|column| {
            table_cell(
                ticket,
                column.id,
                now,
                density,
                tone,
                mine,
                &mut highlighter,
            )
        }));
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
        register_narrow_tabs(app, area);
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
            app.hit_regions.push(region(
                *header_rect,
                PointerTarget::SortHeader(column.id),
                PointerLayer::Base,
                None,
                None,
            ));
        }
        let body = Rect::new(inner.x, inner.y.saturating_add(2), inner.width, body_height);
        app.hit_regions.set_table_body(body);
        app.hit_regions.push(region(
            body,
            PointerTarget::FocusTickets,
            PointerLayer::Base,
            Some(SelectableSurface::Table),
            Some(ScrollSurface::Table),
        ));
        if let Some(id_area) = header_columns.get(1) {
            let id_column = Rect::new(id_area.x, body.y, id_area.width, body.height);
            app.hit_regions.set_id_column(id_column);
        }
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
            app.hit_regions.push(region(
                row_rect,
                PointerTarget::TableRow { index: logical },
                PointerLayer::Base,
                Some(SelectableSurface::Table),
                Some(ScrollSurface::Table),
            ));
            if let Some(marker) = header_columns.first() {
                app.hit_regions.push(region(
                    Rect::new(marker.x, y, 3, 1),
                    PointerTarget::ToggleRowSelect { index: logical },
                    PointerLayer::Base,
                    None,
                    None,
                ));
                app.hit_regions.push(region(
                    Rect::new(marker.x.saturating_add(3), y, 1, 1),
                    PointerTarget::ToggleBookmark { index: logical },
                    PointerLayer::Base,
                    None,
                    None,
                ));
            }
            if let Some(id_area) = header_columns.get(1) {
                app.hit_regions.push(region(
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
                app,
                body,
                ScrollSurface::Table,
                count,
                offset,
                visible_rows,
            );
        }
        capture_selectable(frame, app, SelectableSurface::Table, body, overflow);
    }

    if count == 0 && inner.height > 2 {
        let message = if app.reload_pending {
            "Reloading tickets…"
        } else if !app.parsed_query().is_active() {
            "No tickets in this database"
        } else if app.search_pending {
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

fn render_details(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let mut block = focused_block(" Details ", app.focus.is_details_pane());
    if area.width >= 24 {
        block = block.title(Line::from("[Copy]").right_aligned());
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.hit_regions.set_details(area);
    app.hit_regions.push(region(
        area,
        PointerTarget::FocusDetails,
        PointerLayer::Base,
        Some(SelectableSurface::Details),
        Some(ScrollSurface::Details),
    ));
    if area.width >= 24 {
        app.hit_regions.push(region(
            Rect::new(
                area.x.saturating_add(area.width.saturating_sub(8)),
                area.y,
                6,
                1,
            ),
            PointerTarget::CopyActions,
            PointerLayer::Base,
            None,
            None,
        ));
    }

    let Some(ticket) = app.selected_ticket().cloned() else {
        // Nothing scrollable this frame; keep the measured height.
        let viewport = app.details.viewport;
        app.details.set_viewport(viewport, 0);
        frame.render_widget(
            Paragraph::new("Select a ticket to view details")
                .style(Style::default().fg(theme().muted)),
            inner,
        );
        return;
    };

    let family = app.family_of(&ticket.key);
    let mut highlighter = QueryHighlighter::new(app.query());
    let title_style = Style::default()
        .fg(theme().text)
        .add_modifier(Modifier::BOLD);
    let mut metadata_lines = vec![
        highlight_line(
            ticket.title.clone(),
            &highlighter.indices(&ticket.title),
            title_style,
            search_match_style(title_style),
        ),
        ticket_identity_line(&ticket, &mut highlighter),
    ];
    if family.has_family() {
        metadata_lines.push(family_breadcrumb_line(app, &family));
    }
    metadata_lines.push(ticket_assignment_line(
        &ticket,
        app.is_mine(&ticket),
        &mut highlighter,
    ));
    metadata_lines.push(tags_field_line(&ticket.tags, &mut highlighter));
    metadata_lines.push(field_line(
        "Project / Revision",
        format!(
            "{} / {} · r{}",
            ticket.key.organization, ticket.project, ticket.revision
        ),
    ));
    let metadata_height = inner
        .height
        .saturating_sub(2)
        .min(u16::try_from(metadata_lines.len()).unwrap_or(6));
    let chunks = Layout::vertical([
        Constraint::Length(metadata_height),
        Constraint::Length((inner.height > metadata_height).into()),
        Constraint::Fill(1),
    ])
    .split(inner);
    frame.render_widget(Paragraph::new(Text::from(metadata_lines)), chunks[0]);

    if chunks[1].height > 0 {
        frame.render_widget(Paragraph::new(link_line(ticket.web_url.clone())), chunks[1]);
        app.hit_regions.push(region(
            chunks[1],
            PointerTarget::OpenSelectedUrl,
            PointerLayer::Base,
            Some(SelectableSurface::Details),
            Some(ScrollSurface::Details),
        ));
    }
    if chunks[2].height > 0 {
        let width = chunks[2].width;
        let cursor = app.family_cursor.clone();
        let family_focused = app.focus == Focus::Family;
        let mut detail_lines = Vec::new();
        let mut family_hits: Vec<FamilyHit> = Vec::new();
        let mut line_links: Vec<(u16, TicketKey)> = Vec::new();

        if family.has_family() {
            detail_lines.push(family_section_line(
                family_closed_summary(app, &family),
                family_focused,
            ));
            for entry in app.visible_family_tree() {
                let related = app.ticket_by_key(&entry.key);
                let is_cursor = family_focused && cursor.as_ref() == Some(&entry.key);
                let line = family_tree_line(&entry, related, is_cursor, width);
                if let Ok(index) = u16::try_from(detail_lines.len()) {
                    family_hits.push(FamilyHit {
                        line: index,
                        key: entry.key.clone(),
                        jumpable: related.is_some(),
                    });
                }
                detail_lines.push(line);
            }
            for parent in &family.extra_parents {
                let related = app.ticket_by_key(parent);
                let is_cursor = family_focused && cursor.as_ref() == Some(parent);
                if let Ok(index) = u16::try_from(detail_lines.len())
                    && related.is_some()
                {
                    line_links.push((index, parent.clone()));
                }
                detail_lines.push(family_member_line(
                    "  also ", parent, related, false, is_cursor, width,
                ));
            }
            detail_lines.push(Line::default());
        }
        detail_lines.push(section_line("Planning"));
        detail_lines.push(highlighted_field_line(
            "Area",
            &ticket.area_path,
            &mut highlighter,
        ));
        detail_lines.push(highlighted_field_line(
            "Iteration",
            &ticket.iteration_path,
            &mut highlighter,
        ));
        detail_lines.push(field_line("Created", ticket.created_at.exact_utc()));
        detail_lines.push(field_line("Changed", ticket.changed_at.exact_utc()));
        let history = app.history_for(&ticket.key);
        if !history.is_empty() {
            detail_lines.push(Line::default());
            detail_lines.push(section_line("History"));
            for entry in history {
                let who = entry.changed_by.as_deref().unwrap_or("unknown");
                let old = entry.old_value.as_deref().unwrap_or("—");
                let new = entry.new_value.as_deref().unwrap_or("—");
                detail_lines.push(Line::from(format!(
                    "  r{} · {} · {who}",
                    entry.revision,
                    entry.changed_at.exact_utc()
                )));
                detail_lines.push(Line::styled(
                    format!("    {}: {old} → {new}", entry.field_name),
                    Style::default().fg(theme().body),
                ));
            }
        }
        let comments = app.comments_for(&ticket.key);
        if !comments.is_empty() {
            detail_lines.push(Line::default());
            detail_lines.push(section_line("Comments"));
            for comment in comments {
                let who = comment.author.as_deref().unwrap_or("unknown");
                detail_lines.push(Line::from(format!(
                    "  {who} · {}",
                    comment.created_at.exact_utc()
                )));
                detail_lines.extend(comment.text.lines().map(|line| {
                    Line::styled(format!("    {line}"), Style::default().fg(theme().body))
                }));
            }
        }
        detail_lines.push(Line::default());
        detail_lines.push(section_line("Description"));
        if let Some(reason) = ticket.reason.as_deref() {
            detail_lines.push(field_line("Reason", reason));
            detail_lines.push(Line::default());
        }
        if ticket.description.is_empty() {
            detail_lines.push(Line::styled(
                "No description",
                Style::default().fg(theme().muted),
            ));
        } else {
            detail_lines.extend(
                ticket
                    .description
                    .lines()
                    .map(|line| Line::from(line.to_owned())),
            );
        }
        let paragraph = Paragraph::new(Text::from(detail_lines))
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(theme().body));
        let line_count = paragraph.line_count(chunks[2].width);
        let viewport = usize::from(chunks[2].height);
        app.details.set_viewport(viewport, line_count);
        let scroll = app.details.offset;
        let scroll_rows = u16::try_from(scroll).unwrap_or(u16::MAX);
        frame.render_widget(paragraph.scroll((scroll_rows, 0)), chunks[2]);
        for hit in family_hits {
            let Some(y) = visible_row_y(chunks[2], hit.line, scroll_rows) else {
                continue;
            };
            if hit.jumpable {
                app.hit_regions.push(region(
                    Rect::new(chunks[2].x, y, chunks[2].width.saturating_sub(1), 1),
                    PointerTarget::JumpToTicket(hit.key.clone()),
                    PointerLayer::Base,
                    Some(SelectableSurface::Details),
                    Some(ScrollSurface::Details),
                ));
            }
        }
        for (logical, key) in line_links {
            if let Some(y) = visible_row_y(chunks[2], logical, scroll_rows) {
                app.hit_regions.push(region(
                    Rect::new(chunks[2].x, y, chunks[2].width.saturating_sub(1), 1),
                    PointerTarget::JumpToTicket(key),
                    PointerLayer::Base,
                    Some(SelectableSurface::Details),
                    Some(ScrollSurface::Details),
                ));
            }
        }
        let overflow = line_count > viewport;
        if overflow {
            render_scrollbar(
                frame,
                app,
                chunks[2],
                ScrollSurface::Details,
                line_count,
                scroll,
                viewport,
            );
        }
        capture_selectable(frame, app, SelectableSurface::Details, inner, overflow);
    } else {
        // Nothing scrollable this frame; keep the measured height.
        let viewport = app.details.viewport;
        app.details.set_viewport(viewport, 0);
        capture_selectable(frame, app, SelectableSurface::Details, inner, false);
    }
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let (text, style) = if let Some((message, level)) = app.notification() {
        let color = match level {
            NotificationLevel::Info => theme().info,
            NotificationLevel::Error => theme().error,
        };
        (message, Style::default().fg(color))
    } else {
        let text = match app.mode {
            AppMode::Search => {
                "←→ cursor  Ctrl-P/N history  Ctrl-W delete word  Ctrl-U clear  Enter/Esc finish"
            }
            AppMode::Sort => "↑↓ choose field  ←→ direction  Enter apply  Esc cancel",
            AppMode::Help => "↑↓/jk scroll  PgUp/PgDn page  Home/End jump  ?/Esc close",
            AppMode::Facets if app.facet_bar.field_index >= FilterField::BAR.len() => {
                "←→ field  Enter more filters  Esc back"
            }
            AppMode::Facets => "←→/hl field  ↑↓/jk value  Space toggle  + more  Esc back",
            AppMode::Filter if app.filter_overlay.showing_values => {
                "↑↓ values  Space toggle  ← fields  Esc close"
            }
            AppMode::Filter => "↑↓ field  Enter values  Esc close",
            AppMode::Columns => "↑↓ choose  Space show/hide  JK reorder  <> width  Esc close",
            AppMode::Palette => "Type to filter  ↑↓ select  Enter run  Esc close",
            AppMode::Views if app.views_overlay.naming.is_some() => {
                "Type a view name  Enter save  Esc cancel"
            }
            AppMode::Views => "↑↓ choose  Enter load  n save  d delete  Esc close",
            AppMode::Info => "Esc/i close",
            AppMode::Browse if app.focus == Focus::Family => "↑↓ move  Enter select  Tab details",
            AppMode::Browse if app.focus == Focus::Details => {
                "↑↓/jk scroll details  Tab tickets  Enter/o open  / search  ? help  q quit"
            }
            AppMode::Browse if !app.query().is_empty() => {
                "↑↓/jk move  f filters  Esc clear  ? help  q quit"
            }
            AppMode::Browse => {
                "↑↓/jk move  / search  click/drag copy  wheel scroll  ? help  q quit"
            }
        };
        (text, Style::default().fg(theme().muted))
    };
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(style),
        area,
    );
}

fn render_sort_popup(frame: &mut Frame<'_>, app: &mut App) {
    let area = centered_rect(frame.area(), 48, 16);
    frame.render_widget(Clear, area);
    let inner = render_modal_frame(frame, app, area, " Sort tickets ");
    let selected = app.sort_draft.field_index;
    let rows: Vec<Line> = SortField::ALL
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let marker = if index == selected { "›" } else { " " };
            let direction = if index == selected {
                app.sort_draft.direction.symbol()
            } else if *field == app.sort_field {
                app.sort_direction.symbol()
            } else {
                " "
            };
            Line::from(format!("{marker} {:<14} {direction}", field.label()))
        })
        .collect();
    render_list_overlay(
        frame,
        app,
        ListOverlay {
            area: inner,
            surface: ScrollSurface::Sort,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: false,
            selected,
            rows,
            row_hit_width: Some(inner.width.saturating_sub(8)),
            target: &|index| PointerTarget::SortChoose(SortField::ALL[index]),
            decorate: Some(&|frame: &mut Frame<'_>, app: &mut App, logical, y| {
                if logical == selected {
                    render_sort_controls(frame, app, inner, y);
                }
            }),
        },
    );
    capture_selectable(frame, app, SelectableSurface::Overlay, inner, false);
}

fn render_sort_controls(frame: &mut Frame<'_>, app: &mut App, inner: Rect, y: u16) {
    for (offset, label, direction) in [
        (7, "[↑]", SortDirection::Ascending),
        (3, "[↓]", SortDirection::Descending),
    ] {
        render_control(
            frame,
            app,
            Rect::new(
                inner.x.saturating_add(inner.width.saturating_sub(offset)),
                y,
                3,
                1,
            ),
            label,
            PointerTarget::SortSetDirection(direction),
            PointerLayer::Modal,
            true,
        );
    }
}

fn render_help_popup(frame: &mut Frame<'_>, app: &mut App) {
    let height = frame.area().height.saturating_sub(2).min(18);
    let area = centered_rect(frame.area(), 62, height);
    frame.render_widget(Clear, area);
    let mut lines = vec![
        Line::styled("Navigation", Style::default().add_modifier(Modifier::BOLD)),
        Line::from("  ↑/↓, j/k        Move ticket, family row, or details"),
        Line::from("  PgUp/PgDn       Move ten rows or a family page"),
        Line::from("  Home/End        First/last ticket, family row, or line"),
        Line::from("  Tab             Toggle tickets / details focus"),
        Line::from("  Enter           Select family cursor, or open from details"),
        Line::from("  Space           Toggle ticket multi-select"),
        Line::from("  Esc             Clear active search or selection"),
        Line::from(""),
        Line::styled(
            "Search and filters",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::from("  ←/→, Home/End   Move the query cursor"),
        Line::from("  Backspace/Del   Delete around the cursor"),
        Line::from("  Ctrl-W/Ctrl-U   Delete word / clear query"),
        Line::from("  Ctrl-P/Ctrl-N   Previous / next completed query"),
        Line::from("  state:active    Structured filters in the query"),
        Line::from("  Paste           Insert sanitized text"),
        Line::from(""),
        Line::styled("Actions", Style::default().add_modifier(Modifier::BOLD)),
    ];
    // The bound commands describe themselves; the palette lists the rest.
    lines.extend(
        COMMANDS
            .iter()
            .filter(|command| !command.keys.is_empty())
            .map(|command| {
                let detail = if command.help.is_empty() {
                    command.title.to_owned()
                } else {
                    format!("{} — {}", command.title, command.help)
                };
                Line::from(format!("  {:<15} {detail}", command.key_label()))
            }),
    );
    lines.extend([
        Line::from(""),
        Line::styled("Mouse", Style::default().add_modifier(Modifier::BOLD)),
        Line::from("  Wheel           Scroll the hovered table, details, help, or overlay"),
        Line::from("  Click           Activate buttons, rows, links, headers, and checkboxes"),
        Line::from("  Drag            Select visible text and copy it on release"),
        Line::from("  Scrollbar       Click the track or drag the thumb"),
        Line::from("  Divider         Drag between panes to resize"),
        Line::from("  Paste           Insert into search, palette, and view-name fields"),
        Line::from(""),
        Line::styled(
            "Press ? or Esc to close",
            Style::default().fg(theme().muted),
        ),
    ]);
    let help = Text::from(lines);
    let block = Block::default()
        .title(" Help ")
        .title(Line::from("[×]").right_aligned())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme().accent));
    let inner = block.inner(area);
    let paragraph = Paragraph::new(help).block(block).wrap(Wrap { trim: false });
    let line_count = paragraph.line_count(area.width);
    app.help.set_viewport(usize::from(inner.height), line_count);
    let scroll = app.help.offset;
    frame.render_widget(
        paragraph.scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        area,
    );
    app.hit_regions.push(region(
        inner,
        PointerTarget::OverlayBody,
        PointerLayer::Modal,
        Some(SelectableSurface::Help),
        Some(ScrollSurface::Help),
    ));
    register_close_button(app, area, PointerLayer::Modal);
    let overflow = line_count > usize::from(inner.height);
    if overflow {
        render_scrollbar(
            frame,
            app,
            inner,
            ScrollSurface::Help,
            line_count,
            scroll,
            usize::from(inner.height),
        );
    }
    capture_selectable(frame, app, SelectableSurface::Help, inner, overflow);
}

fn render_chips(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let mut spans = Vec::new();
    let mut x = area.x;
    for token in app.overflow_filter_tokens() {
        let label = format!(" {} × ", token.chip_label());
        let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
        if x.saturating_add(width) > area.x.saturating_add(area.width) {
            break;
        }
        app.hit_regions.push(region(
            Rect::new(x, area.y, width, 1),
            PointerTarget::RemoveChip(token.clone()),
            PointerLayer::Base,
            None,
            None,
        ));
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(theme().text)
                .bg(theme().selected_background),
        ));
        spans.push(Span::raw(" "));
        x = x.saturating_add(width.saturating_add(1));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_facet_bar(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let filters = app.parsed_query().filters;
    let focused = app.mode == AppMode::Facets;
    let mut spans = Vec::new();
    let mut x = area.x;
    let mut remaining = area.width;
    for (index, field) in FilterField::BAR.iter().enumerate() {
        let label = facet_pill_label(*field, &filters);
        let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
        if remaining < width.saturating_add(1) {
            break;
        }
        let rect = Rect::new(x, area.y, width, 1);
        app.hit_regions.push(region(
            rect,
            PointerTarget::FacetPill(FacetTarget::Field(*field)),
            PointerLayer::Base,
            None,
            None,
        ));
        let selected = focused && app.facet_bar.field_index == index;
        let active = filters.selected_count(*field) > 0;
        spans.push(Span::styled(label, pill_style(selected, active)));
        spans.push(Span::raw(" "));
        x = x.saturating_add(width.saturating_add(1));
        remaining = remaining.saturating_sub(width.saturating_add(1));
    }
    if remaining >= 5 {
        let more_count = app.overflow_filter_tokens().len();
        let more = if more_count == 0 {
            " + ".to_owned()
        } else {
            format!(" +{more_count} ")
        };
        let width = u16::try_from(more.chars().count()).unwrap_or(u16::MAX);
        app.hit_regions.push(region(
            Rect::new(x, area.y, width.min(remaining), 1),
            PointerTarget::FacetPill(FacetTarget::More),
            PointerLayer::Base,
            None,
            None,
        ));
        let selected = focused && app.facet_bar.field_index >= FilterField::BAR.len();
        spans.push(Span::styled(more, pill_style(selected, more_count > 0)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn facet_pill_label(field: FilterField, filters: &crate::filter::FilterSet) -> String {
    let selected = filters.selected_values(field);
    match selected.as_slice() {
        [] => format!(" {} ▾ ", field.label()),
        [value] => format!(" {}:{} ", field.label(), truncate_pill(value, 12)),
        [value, rest @ ..] => {
            format!(
                " {}:{} +{} ",
                field.label(),
                truncate_pill(value, 8),
                rest.len()
            )
        }
    }
}

fn truncate_pill(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_owned()
    } else {
        format!(
            "{}…",
            value
                .chars()
                .take(max.saturating_sub(1))
                .collect::<String>()
        )
    }
}

fn pill_style(selected: bool, active: bool) -> Style {
    if selected {
        Style::default()
            .fg(theme().text)
            .bg(theme().selected_background)
            .add_modifier(Modifier::BOLD)
    } else if active {
        Style::default()
            .fg(theme().text)
            .bg(theme().selected_background)
    } else {
        Style::default().fg(theme().muted)
    }
}

/// Paints extras for one visible overlay row, given its logical index and `y`.
type RowDecorator<'a> = &'a dyn Fn(&mut Frame<'_>, &mut App, usize, u16);

struct ListOverlay<'a> {
    area: Rect,
    surface: ScrollSurface,
    layer: PointerLayer,
    /// Selectable surface recorded on each row hit region.
    selectable: Option<SelectableSurface>,
    /// Snapshot `area` for text selection once the rows are painted.
    capture: bool,
    selected: usize,
    /// One unstyled line per logical row; the selected row is styled here.
    rows: Vec<Line<'a>>,
    /// Hit region width, defaulting to the area minus its scrollbar column.
    row_hit_width: Option<u16>,
    target: &'a dyn Fn(usize) -> PointerTarget,
    /// Extra painting for each visible row. It runs before the scrollbar, so row
    /// controls reaching into the last column stay underneath the scrollbar.
    decorate: Option<RowDecorator<'a>>,
}

/// Renders one scrollable list inside an overlay: viewport bookkeeping, the visible
/// window, selection styling, per-row hit regions, the scrollbar on overflow, and the
/// text selection snapshot.
fn render_list_overlay(frame: &mut Frame<'_>, app: &mut App, overlay: ListOverlay<'_>) {
    let ListOverlay {
        area,
        surface,
        layer,
        selectable,
        capture,
        selected,
        rows,
        row_hit_width,
        target,
        decorate,
    } = overlay;
    let content = rows.len();
    let viewport = usize::from(area.height);
    app.scroll_state_mut(surface)
        .set_viewport(viewport, content);
    let scroll = app.scroll_state(surface).offset;
    let lines: Vec<Line<'_>> = rows
        .into_iter()
        .enumerate()
        .skip(scroll)
        .take(viewport)
        .map(|(index, line)| overlay_line(line, index == selected))
        .collect();
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
    let hit_width = row_hit_width.unwrap_or_else(|| area.width.saturating_sub(1));
    let visible = scroll..content.min(scroll.saturating_add(viewport));
    for (logical, y) in visible.clone().zip(area.y..) {
        app.hit_regions.push(region(
            Rect::new(area.x, y, hit_width, 1),
            target(logical),
            layer,
            selectable,
            Some(surface),
        ));
    }
    if let Some(decorate) = decorate {
        for (logical, y) in visible.zip(area.y..) {
            decorate(frame, app, logical, y);
        }
    }
    let overflow = content > viewport;
    if overflow {
        render_scrollbar(frame, app, area, surface, content, scroll, viewport);
    }
    if capture && let Some(surface) = selectable {
        capture_selectable(frame, app, surface, area, overflow);
    }
}

fn render_facet_menu(frame: &mut Frame<'_>, app: &mut App) {
    let Some(field) = FilterField::BAR.get(app.facet_bar.field_index).copied() else {
        return;
    };
    let facets = app.facets_for(field);
    let pill = app
        .hit_regions
        .facet_pills
        .iter()
        .find(|(_, target)| *target == FacetTarget::Field(field))
        .map(|(area, _)| *area);
    let width = 36.min(frame.area().width.saturating_sub(2)).max(20);
    let height = u16::try_from(facets.len().saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(14)
        .min(frame.area().height.saturating_sub(2));
    let mut area = Rect {
        x: pill.map_or(frame.area().x + 1, |pill| pill.x),
        y: pill.map_or(4, |pill| pill.y.saturating_add(1)),
        width,
        height,
    };
    if area.x.saturating_add(area.width) > frame.area().width {
        area.x = frame.area().width.saturating_sub(area.width);
    }
    if area.y.saturating_add(area.height) > frame.area().height {
        area.y = area.y.saturating_sub(area.height.saturating_add(1));
    }
    app.hit_regions.push(region(
        frame.area(),
        PointerTarget::DismissFacet,
        PointerLayer::Popup,
        None,
        None,
    ));
    frame.render_widget(Clear, area);
    let inner = render_modal_frame(frame, app, area, &format!(" {} ", field.label()));
    let selected = app.facet_bar.value_index;
    let rows: Vec<Line> = facets
        .iter()
        .enumerate()
        .map(|(index, facet)| {
            let marker = if index == selected { "›" } else { " " };
            let check = if facet.selected { "[x]" } else { "[ ]" };
            Line::from(format!(
                "{marker} {check} {:<18} {:>4}",
                facet.value, facet.count
            ))
        })
        .collect();
    render_list_overlay(
        frame,
        app,
        ListOverlay {
            area: inner,
            surface: ScrollSurface::FacetMenu,
            layer: PointerLayer::Popup,
            selectable: None,
            capture: false,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::FacetValue { index },
            decorate: None,
        },
    );
}

fn render_filter_overlay(frame: &mut Frame<'_>, app: &mut App) {
    let area = centered_rect(frame.area(), 52, 16);
    frame.render_widget(Clear, area);
    let title = if app.filter_overlay.showing_values {
        format!(" {} ", app.facet_field().label())
    } else {
        " Filters ".into()
    };
    let inner = render_modal_frame(frame, app, area, &title);
    let showing_values = app.filter_overlay.showing_values;
    let selected = if showing_values {
        app.filter_overlay.value_index
    } else {
        app.filter_overlay.field_index
    };
    let rows: Vec<Line> = if showing_values {
        app.current_facets()
            .into_iter()
            .enumerate()
            .map(|(index, facet)| {
                let marker = if index == selected { "›" } else { " " };
                let check = if facet.selected { "[x]" } else { "[ ]" };
                Line::from(format!(
                    "{marker} {check} {:<18} {:>4}",
                    facet.value, facet.count
                ))
            })
            .collect()
    } else {
        FilterField::ALL
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let marker = if index == selected { "›" } else { " " };
                let count = app.parsed_query().filters.selected_count(*field);
                let suffix = if count == 0 {
                    String::new()
                } else {
                    format!("{count} selected")
                };
                Line::from(format!("{marker} {:<12} {suffix}", field.label()))
            })
            .collect()
    };
    render_list_overlay(
        frame,
        app,
        ListOverlay {
            area: inner,
            surface: ScrollSurface::Filter,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: true,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::FilterRow { index },
            decorate: None,
        },
    );
}

fn render_column_overlay(frame: &mut Frame<'_>, app: &mut App) {
    let area = centered_rect(frame.area(), 56, 18);
    frame.render_widget(Clear, area);
    let inner = render_modal_frame(frame, app, area, " Columns ");
    let content = app.layout.columns.len();
    let selected = app.column_overlay.index;
    let rows: Vec<Line> = app
        .layout
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let marker = if index == selected { "›" } else { " " };
            let check = if column.visible { "[x]" } else { "[ ]" };
            let width = if column.id == SortField::Title {
                "fill".into()
            } else {
                column.width.to_string()
            };
            Line::from(format!(
                "{marker} {check} {:<12} {width}",
                column.id.label()
            ))
        })
        .collect();
    render_list_overlay(
        frame,
        app,
        ListOverlay {
            area: inner,
            surface: ScrollSurface::Columns,
            layer: PointerLayer::Modal,
            selectable: None,
            capture: false,
            selected,
            rows,
            row_hit_width: Some(5),
            target: &|index| PointerTarget::ColumnToggle { index },
            decorate: Some(&|frame: &mut Frame<'_>, app: &mut App, logical, y| {
                render_column_controls(frame, app, inner, content, logical, y);
            }),
        },
    );
}

fn render_column_controls(
    frame: &mut Frame<'_>,
    app: &mut App,
    inner: Rect,
    content: usize,
    logical: usize,
    y: u16,
) {
    let resizable = app
        .layout
        .columns
        .get(logical)
        .is_some_and(|column| column.id != SortField::Title);
    let controls = [
        (
            15,
            "[↑]",
            PointerTarget::ColumnMove {
                index: logical,
                delta: -1,
            },
            logical > 0,
        ),
        (
            11,
            "[↓]",
            PointerTarget::ColumnMove {
                index: logical,
                delta: 1,
            },
            logical + 1 < content,
        ),
        (
            7,
            "[−]",
            PointerTarget::ColumnResize {
                index: logical,
                delta: -1,
            },
            resizable,
        ),
        (
            3,
            "[+]",
            PointerTarget::ColumnResize {
                index: logical,
                delta: 1,
            },
            resizable,
        ),
    ];
    for (offset, label, target, enabled) in controls {
        render_control(
            frame,
            app,
            Rect::new(
                inner.x.saturating_add(inner.width.saturating_sub(offset)),
                y,
                3,
                1,
            ),
            label,
            target,
            PointerLayer::Modal,
            enabled,
        );
    }
}

fn render_palette(frame: &mut Frame<'_>, app: &mut App) {
    let commands = app.palette_commands();
    let height = u16::try_from(commands.len().saturating_add(4))
        .unwrap_or(u16::MAX)
        .min(16);
    let area = centered_rect(frame.area(), 56, height.max(6));
    frame.render_widget(Clear, area);
    let inner = render_modal_frame(frame, app, area, " Commands ");
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).split(inner);
    let query_area = chunks[0];
    let list_area = chunks[1];
    let query = if app.palette.query.is_empty() {
        Line::styled("Filter commands…", Style::default().fg(theme().muted))
    } else {
        Line::from(app.palette.query.text().to_owned())
    };
    frame.render_widget(
        Paragraph::new(query).style(Style::default().fg(theme().text)),
        query_area,
    );
    app.hit_regions.push(region(
        query_area,
        PointerTarget::PaletteQuery,
        PointerLayer::Modal,
        Some(SelectableSurface::Overlay),
        None,
    ));
    capture_selectable(frame, app, SelectableSurface::Overlay, query_area, false);
    let cursor_x = query_area.x.saturating_add(
        u16::try_from(app.palette.query.cursor())
            .unwrap_or(u16::MAX)
            .min(query_area.width.saturating_sub(1)),
    );
    frame.set_cursor_position((cursor_x, query_area.y));
    let selected = app.palette.selected;
    let rows: Vec<Line> = commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let marker = if index == selected { "›" } else { " " };
            Line::from(format!(
                "{marker} {:<28} {}",
                command.title,
                command.key_label()
            ))
        })
        .collect();
    render_list_overlay(
        frame,
        app,
        ListOverlay {
            area: list_area,
            surface: ScrollSurface::Palette,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: false,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::PaletteCommand { index },
            decorate: None,
        },
    );
}

fn render_views_overlay(frame: &mut Frame<'_>, app: &mut App) {
    let area = centered_rect(frame.area(), 56, 14);
    frame.render_widget(Clear, area);
    let title = if app.views_overlay.naming.is_some() {
        " Save view "
    } else {
        " Views "
    };
    let inner = render_modal_frame(frame, app, area, title);
    if let Some((name, name_cursor)) = app
        .views_overlay
        .naming
        .as_ref()
        .map(|input| (input.text().to_owned(), input.cursor()))
    {
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .split(inner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Name: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(name.clone()),
            ])),
            chunks[0],
        );
        let field = Rect::new(
            chunks[0].x.saturating_add(6),
            chunks[0].y,
            chunks[0].width.saturating_sub(6),
            1,
        );
        app.hit_regions.push(region(
            field,
            PointerTarget::ViewName,
            PointerLayer::Modal,
            Some(SelectableSurface::Overlay),
            None,
        ));
        capture_selectable(frame, app, SelectableSurface::Overlay, field, false);
        let cursor_x = field
            .x
            .saturating_add(u16::try_from(name_cursor).unwrap_or(u16::MAX))
            .min(field.x.saturating_add(field.width.saturating_sub(1)));
        frame.set_cursor_position((cursor_x, field.y));
        render_control(
            frame,
            app,
            Rect::new(chunks[1].x, chunks[1].y, 6, 1),
            "[Save]",
            PointerTarget::SaveView,
            PointerLayer::Modal,
            !name.trim().is_empty(),
        );
        render_control(
            frame,
            app,
            Rect::new(chunks[1].x.saturating_add(7), chunks[1].y, 8, 1),
            "[Cancel]",
            PointerTarget::CancelNaming,
            PointerLayer::Modal,
            true,
        );
        return;
    }
    if inner.width >= 28 {
        render_control(
            frame,
            app,
            Rect::new(inner.x, inner.y, 14, 1),
            "[Save current]",
            PointerTarget::SaveView,
            PointerLayer::Modal,
            true,
        );
        render_control(
            frame,
            app,
            Rect::new(inner.x.saturating_add(15), inner.y, 8, 1),
            "[Delete]",
            PointerTarget::DeleteView,
            PointerLayer::Modal,
            !app.views().is_empty(),
        );
    }
    let list = Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        inner.height.saturating_sub(1),
    );
    if app.views().is_empty() {
        frame.render_widget(
            Paragraph::new("No saved views. Press n to save the current view.")
                .style(Style::default().fg(theme().muted)),
            list,
        );
        return;
    }
    let views: Vec<(String, String, bool)> = app
        .views()
        .iter()
        .map(|view| {
            (
                view.name.clone(),
                view.query.clone(),
                app.active_view.as_deref() == Some(view.name.as_str()),
            )
        })
        .collect();
    let selected = app.views_overlay.index;
    let rows: Vec<Line> = views
        .iter()
        .enumerate()
        .map(|(index, (name, query, current))| {
            let marker = if index == selected { "›" } else { " " };
            let current = if *current { "*" } else { " " };
            Line::from(format!("{marker}{current} {name:<18} {query}"))
        })
        .collect();
    render_list_overlay(
        frame,
        app,
        ListOverlay {
            area: list,
            surface: ScrollSurface::Views,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: false,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::ViewRow { index },
            decorate: None,
        },
    );
}

fn link_line(text: String) -> Line<'static> {
    terminate_underline(Line::from(Span::styled(
        text,
        Style::default()
            .fg(theme().link)
            .add_modifier(Modifier::UNDERLINED),
    )))
}

fn terminate_underline(mut line: Line<'static>) -> Line<'static> {
    line.spans.push(Span::styled(
        " ",
        Style::default().remove_modifier(Modifier::UNDERLINED),
    ));
    line
}

struct FamilyHit {
    line: u16,
    key: TicketKey,
    jumpable: bool,
}

fn family_closed_summary(app: &App, family: &FamilySnapshot) -> Option<(usize, usize)> {
    if family.children.is_empty() {
        return None;
    }
    let done = family
        .children
        .iter()
        .filter(|key| {
            app.ticket_by_key(key)
                .is_some_and(|ticket| state_is_done(&ticket.state))
        })
        .count();
    Some((done, family.children.len()))
}

fn family_section_line(summary: Option<(usize, usize)>, focused: bool) -> Line<'static> {
    let heading = family_heading_style(focused);
    let Some((done, total)) = summary else {
        return Line::styled("Family", heading);
    };
    let mut count = Style::default().fg(state_color(StateCategory::Completed));
    if focused {
        count = with_cursor_style(count);
    }
    Line::from(vec![
        Span::styled("Family · ", heading),
        Span::styled(format!("{done}/{total} closed"), count),
    ])
}

fn family_heading_style(focused: bool) -> Style {
    let mut style = Style::default()
        .fg(theme().accent)
        .add_modifier(Modifier::BOLD);
    if focused {
        style = with_cursor_style(style);
    }
    style
}

fn family_row_style(is_current: bool, is_cursor: bool) -> Style {
    let mut style = Style::default();
    if is_current {
        style = style.fg(theme().text).add_modifier(Modifier::BOLD);
    }
    if is_cursor {
        style = with_cursor_style(style);
    }
    style
}

pub(crate) fn with_cursor_style(style: Style) -> Style {
    let style = style.bg(theme().selected_background);
    if theme().selected_background == Color::Reset {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style
    }
}

fn family_connector(prefix: &str) -> String {
    if prefix.chars().all(char::is_whitespace) {
        prefix.to_owned()
    } else {
        format!("{prefix} ")
    }
}

/// A one-character state marker for the family tree, where there is no room to
/// spell the state out.
const fn state_glyph(category: StateCategory) -> &'static str {
    match category {
        StateCategory::Proposed => "\u{25cb}",
        StateCategory::InProgress => "\u{25d0}",
        StateCategory::Resolved => "\u{25cf}",
        StateCategory::Completed => "\u{2713}",
        StateCategory::Removed => "\u{2717}",
        StateCategory::Unknown => "",
    }
}

/// Colour the glyph like the table's State cell; monochrome leans on weight.
fn state_glyph_style(base: Style, category: StateCategory) -> Style {
    let color = state_color(category);
    let style = base.fg(color).remove_modifier(Modifier::UNDERLINED);
    if color == Color::Reset {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn family_tree_line(
    entry: &FamilyTreeEntry,
    ticket: Option<&Ticket>,
    is_cursor: bool,
    width: u16,
) -> Line<'static> {
    let connector = family_connector(&entry.prefix);
    let id = entry.key.id.to_string();
    let type_label = ticket.map_or("?", |ticket| ticket.work_item_type.as_str());
    let title = ticket.map_or("missing ticket", |ticket| ticket.title.as_str());
    let category = ticket.map(|ticket| StateCategory::of(&ticket.state));
    let current_label = if entry.is_current { " current" } else { "" };
    let packed = pack_family_row(
        usize::from(width),
        &format!("{connector}{id}"),
        type_label,
        category.map_or("", state_glyph),
        title,
        current_label,
    );
    let base = family_row_style(entry.is_current, is_cursor);
    let id_style = if entry.is_current || ticket.is_none() {
        base.fg(theme().muted)
    } else {
        base.fg(theme().link).add_modifier(Modifier::UNDERLINED)
    };
    // The row you are reading keeps its weight; finished relatives fade.
    let tone = if entry.is_current {
        RowTone::Normal
    } else {
        ticket.map_or(RowTone::Normal, |ticket| RowTone::of(&ticket.state))
    };
    let rest_style = tone
        .apply(if entry.is_current {
            base
        } else {
            base.fg(theme().body)
        })
        .remove_modifier(Modifier::UNDERLINED);
    let head_len = connector.chars().count() + id.chars().count();
    let mut spans = vec![Span::styled(connector, base), Span::styled(id, id_style)];
    if let Some(at) = packed.glyph_at {
        let lead: String = packed
            .text
            .chars()
            .skip(head_len)
            .take(at.saturating_sub(head_len))
            .collect();
        let glyph: String = packed.text.chars().skip(at).take(1).collect();
        let tail: String = packed.text.chars().skip(at + 1).collect();
        spans.push(Span::styled(lead, rest_style));
        spans.push(Span::styled(
            glyph,
            state_glyph_style(base, category.unwrap_or(StateCategory::Unknown)),
        ));
        spans.push(Span::styled(tail, rest_style));
    } else {
        let rest: String = packed.text.chars().skip(head_len).collect();
        spans.push(Span::styled(rest, rest_style));
    }
    Line::from(spans)
}

/// A family row packed to its width, with the char offset of the state glyph
/// when one survived the fit.
struct PackedFamilyRow {
    text: String,
    glyph_at: Option<usize>,
}

impl PackedFamilyRow {
    fn plain(text: String) -> Self {
        Self {
            text,
            glyph_at: None,
        }
    }
}

fn pack_family_row(
    width: usize,
    head: &str,
    type_label: &str,
    glyph: &str,
    title: &str,
    current_label: &str,
) -> PackedFamilyRow {
    let assemble = |include_type: bool, include_glyph: bool, include_current: bool, title: &str| {
        let mut text = head.to_owned();
        if include_type {
            text.push_str("  ");
            text.push_str(type_label);
        }
        let mut glyph_at = None;
        if include_glyph && !glyph.is_empty() {
            text.push_str("  ");
            glyph_at = Some(text.chars().count());
            text.push_str(glyph);
        }
        if !title.is_empty() {
            text.push_str(if glyph_at.is_some() { " " } else { "  " });
            text.push_str(title);
        }
        if include_current {
            text.push_str(current_label);
        }
        PackedFamilyRow { text, glyph_at }
    };
    let include_current = !current_label.is_empty();
    let fit = |row: PackedFamilyRow| (row.text.chars().count() <= width).then_some(row);
    let budget = |include_type: bool, include_current: bool| {
        width.saturating_sub(
            assemble(include_type, false, include_current, "")
                .text
                .chars()
                .count()
                .saturating_add(2),
        )
    };
    if let Some(row) = fit(assemble(true, true, include_current, title)) {
        return row;
    }
    // Shed one thing at a time so the connector and id always survive: the
    // glyph goes before the title is truncated, then the type, then the
    // current marker.
    if let Some(row) = fit(assemble(true, false, include_current, title)) {
        return row;
    }
    for (with_type, with_current) in [
        (true, include_current),
        (false, include_current),
        (false, false),
    ] {
        let truncated = take_chars(title, budget(with_type, with_current));
        if let Some(row) = fit(assemble(with_type, false, with_current, &truncated)) {
            return row;
        }
    }
    let without_title = assemble(false, false, false, "");
    if without_title.text.chars().count() <= width {
        return without_title;
    }
    if head.chars().count() <= width {
        return PackedFamilyRow::plain(head.to_owned());
    }
    PackedFamilyRow::plain(take_chars(head, width))
}

fn family_breadcrumb_line(app: &App, family: &FamilySnapshot) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "Family: ",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if let Some(parent) = family.parent() {
        let ticket = app.ticket_by_key(parent);
        let type_label = ticket.map_or("?", |ticket| ticket.work_item_type.as_str());
        let title = ticket.map_or("missing ticket", |ticket| ticket.title.as_str());
        spans.push(Span::raw(format!("{type_label} ")));
        spans.push(Span::raw(parent.id.to_string()));
        spans.push(Span::raw(format!("  {title} › this")));
    } else {
        spans.push(Span::raw("this"));
    }
    if !family.children.is_empty() {
        let done = family
            .children
            .iter()
            .filter(|key| {
                app.ticket_by_key(key)
                    .is_some_and(|ticket| state_is_done(&ticket.state))
            })
            .count();
        spans.push(Span::styled(
            format!(" · {done}/{} closed", family.children.len()),
            Style::default().fg(state_color(StateCategory::Completed)),
        ));
    }
    Line::from(spans)
}

fn family_member_line(
    prefix: &str,
    key: &TicketKey,
    ticket: Option<&Ticket>,
    is_current: bool,
    is_focused: bool,
    width: u16,
) -> Line<'static> {
    let id = key.id.to_string();
    let work_item_type = ticket.map_or("?", |ticket| ticket.work_item_type.as_str());
    let title = ticket.map_or("missing ticket", |ticket| ticket.title.as_str());
    let marker = if is_current { " ←" } else { "" };
    let used = prefix.chars().count()
        + id.chars().count()
        + 1
        + work_item_type.chars().count()
        + 2
        + marker.chars().count();
    let title = take_chars(title, usize::from(width).saturating_sub(used));
    let base = family_row_style(is_current, is_focused);
    let id_style = if is_current || ticket.is_none() {
        base.fg(theme().muted)
    } else {
        base.fg(theme().link).add_modifier(Modifier::UNDERLINED)
    };
    Line::from(vec![
        Span::styled(prefix.to_owned(), base),
        Span::styled(id, id_style),
        Span::styled(" ", base.remove_modifier(Modifier::UNDERLINED)),
        Span::styled(
            format!("{work_item_type}  {title}{marker}"),
            if is_current {
                base
            } else {
                base.fg(theme().body)
            },
        ),
    ])
}

fn take_chars(text: &str, max: usize) -> String {
    let total = text.chars().count();
    if total <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut truncated: String = text.chars().take(max.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

fn visible_row_y(area: Rect, logical: u16, scroll: u16) -> Option<u16> {
    if logical < scroll {
        return None;
    }
    let offset = logical - scroll;
    if offset >= area.height {
        return None;
    }
    Some(area.y.saturating_add(offset))
}

fn state_is_done(state: &str) -> bool {
    matches!(
        StateCategory::of(state),
        StateCategory::Completed | StateCategory::Removed
    )
}

/// How strongly a row is painted: finished work fades so open work stands out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowTone {
    Normal,
    Muted,
}

impl RowTone {
    fn of(state: &str) -> Self {
        if state_is_done(state) {
            Self::Muted
        } else {
            Self::Normal
        }
    }

    /// Fade a style. Colour themes drop to the muted foreground; the monochrome
    /// theme has no muted colour, so it dims instead. Bold goes either way, so
    /// weight alone still separates open work from finished work.
    fn apply(self, style: Style) -> Style {
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

fn render_info_overlay(frame: &mut Frame<'_>, app: &mut App) {
    let area = centered_rect(frame.area(), 62, 10);
    frame.render_widget(Clear, area);
    let stale = if app.stale { "stale" } else { "current" };
    let path = if app.database_path.as_os_str().is_empty() {
        "(not set)".into()
    } else {
        app.database_path.display().to_string()
    };
    let text = Text::from(vec![
        field_line("Path", path),
        field_line("Tickets", app.tickets().len().to_string()),
        field_line("Visible", app.visible_count().to_string()),
        field_line("Loaded", app.freshness_label()),
        field_line("Freshness", stale),
        Line::default(),
        Line::styled(
            "Press Esc or i to close",
            Style::default().fg(theme().muted),
        ),
    ]);
    let inner = render_modal_frame(frame, app, area, " Database ");
    frame.render_widget(Paragraph::new(text), inner);
    app.hit_regions.push(region(
        inner,
        PointerTarget::OverlayBody,
        PointerLayer::Modal,
        Some(SelectableSurface::Overlay),
        None,
    ));
    capture_selectable(frame, app, SelectableSurface::Overlay, inner, false);
}

fn overlay_line(line: Line<'_>, selected: bool) -> Line<'_> {
    if selected {
        line.style(
            Style::default()
                .bg(theme().selected_background)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        line
    }
}

fn table_cell(
    ticket: &Ticket,
    field: SortField,
    now: OffsetDateTime,
    density: RowDensity,
    tone: RowTone,
    mine: bool,
    highlighter: &mut QueryHighlighter,
) -> Cell<'static> {
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
            .style(plain),
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

fn highlight_searchable(
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

fn search_match_style(base: Style) -> Style {
    let style = base.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    if base.fg == Some(Color::Reset) || base.fg.is_none() {
        style.fg(theme().search_match)
    } else {
        style
    }
}

fn highlight_line(text: String, indices: &[u32], base: Style, matched: Style) -> Line<'static> {
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
fn type_style(work_item_type: &str) -> Style {
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

fn type_badge_spans(
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

fn tag_badge_spans(
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
fn tag_color(tag: &str) -> Color {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in tag.bytes() {
        hash ^= u32::from(byte.to_ascii_lowercase());
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let palette = theme().tag_palette;
    palette[usize::try_from(hash).unwrap_or_default() % palette.len()]
}

fn badge_spans(
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

fn state_color(category: StateCategory) -> Color {
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

fn state_style(state: &str) -> Style {
    Style::default()
        .fg(state_color(StateCategory::of(state)))
        .add_modifier(Modifier::BOLD)
}

fn priority_style(priority: Option<i64>) -> Style {
    let color = match priority {
        Some(1) => theme().priority_critical,
        Some(2) => theme().priority_high,
        Some(3 | 4) => theme().priority_normal,
        _ => theme().muted,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn ticket_identity_line(ticket: &Ticket, highlighter: &mut QueryHighlighter) -> Line<'static> {
    let id = ticket.key.id.to_string();
    let state = state_style(&ticket.state);
    let mut spans = vec![Span::styled(
        "ID / Type / State: ",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    spans.extend(highlight_searchable(&id, Style::default(), highlighter).spans);
    spans.push(Span::raw(" · "));
    spans.extend(type_badge_spans(
        &ticket.work_item_type,
        RowTone::Normal,
        highlighter,
    ));
    spans.push(Span::raw(" · "));
    spans.extend(highlight_searchable(&ticket.state, state, highlighter).spans);
    Line::from(spans)
}

/// The signed-in user's own work. Bold carries the emphasis where NO_COLOR
/// resets the accent, so "mine" reads either way.
fn assigned_to_me_style() -> Style {
    Style::default()
        .fg(theme().accent)
        .add_modifier(Modifier::BOLD)
}

fn ticket_assignment_line(
    ticket: &Ticket,
    mine: bool,
    highlighter: &mut QueryHighlighter,
) -> Line<'static> {
    let priority = ticket
        .priority
        .map_or_else(|| "—".into(), |priority| priority.to_string());
    let assignee_line = match ticket.assigned_to.as_deref() {
        Some(name) if mine => highlight_searchable(name, assigned_to_me_style(), highlighter),
        Some(name) => highlight_searchable(name, Style::default(), highlighter),
        None => Line::styled("Unassigned", Style::default().fg(theme().muted)),
    };
    let mut spans = vec![Span::styled(
        "Assignee / Priority: ",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    spans.extend(assignee_line.spans);
    spans.push(Span::raw(" · "));
    spans.push(Span::styled(priority, priority_style(ticket.priority)));
    Line::from(spans)
}

fn highlighted_field_line(
    label: &'static str,
    value: &str,
    highlighter: &mut QueryHighlighter,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{label}: "),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    spans.extend(highlight_searchable(value, Style::default(), highlighter).spans);
    Line::from(spans)
}

fn tags_field_line(tags: &[String], highlighter: &mut QueryHighlighter) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "Tags: ",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if tags.is_empty() {
        spans.push(Span::styled("—", Style::default().fg(theme().muted)));
    } else {
        spans.extend(tag_badge_spans(tags, RowTone::Normal, highlighter));
    }
    Line::from(spans)
}

fn field_line<'a>(label: &'a str, value: impl Into<String>) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(value.into()),
    ])
}

fn section_line(title: &'static str) -> Line<'static> {
    Line::styled(
        title,
        Style::default()
            .fg(theme().accent)
            .add_modifier(Modifier::BOLD),
    )
}

fn row_marker_line(checked: bool, bookmarked: bool) -> Line<'static> {
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

fn register_narrow_tabs(app: &mut App, area: Rect) {
    let tickets = Rect::new(area.x.saturating_add(1), area.y, 9, 1);
    let details = Rect::new(area.x.saturating_add(11), area.y, 9, 1);
    app.hit_regions.push(region(
        tickets,
        PointerTarget::NarrowTickets,
        PointerLayer::Base,
        None,
        None,
    ));
    app.hit_regions.push(region(
        details,
        PointerTarget::NarrowDetails,
        PointerLayer::Base,
        None,
        None,
    ));
}

fn render_modal_frame(frame: &mut Frame<'_>, app: &mut App, area: Rect, title: &str) -> Rect {
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

fn register_close_button(app: &mut App, area: Rect, layer: PointerLayer) {
    app.hit_regions.push(region(
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

fn render_control(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    label: &str,
    target: PointerTarget,
    layer: PointerLayer,
    enabled: bool,
) {
    let hovered = app.hovered() == Some(&target);
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
        app.hit_regions
            .push(region(area, target, layer, None, None));
    }
}

fn render_scrollbar(
    frame: &mut Frame<'_>,
    app: &mut App,
    area: Rect,
    surface: ScrollSurface,
    content: usize,
    offset: usize,
    viewport: usize,
) {
    let mut scrollbar_state = ScrollbarState::new(content)
        .position(offset)
        .viewport_content_length(viewport);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("┃")
            .style(Style::default().fg(theme().scrollbar)),
        area,
        &mut scrollbar_state,
    );
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
    app.hit_regions.set_scroll(surface, metrics);
    if let Some(thumb) = metrics.thumb() {
        let thumb_rect = Rect::new(track.x, track.y.saturating_add(thumb.y), 1, thumb.height);
        let above = Rect::new(track.x, track.y, 1, thumb.y);
        let below_y = track.y.saturating_add(thumb.y).saturating_add(thumb.height);
        let below_height = track.y.saturating_add(track.height).saturating_sub(below_y);
        if above.height > 0 {
            app.hit_regions.push(region(
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
        app.hit_regions.push(region(
            thumb_rect,
            PointerTarget::ScrollbarThumb { surface },
            current_layer(app),
            None,
            Some(surface),
        ));
        if below_height > 0 {
            app.hit_regions.push(region(
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

fn current_layer(app: &App) -> PointerLayer {
    match app.mode {
        AppMode::Facets => PointerLayer::Popup,
        AppMode::Browse | AppMode::Search => PointerLayer::Base,
        _ => PointerLayer::Modal,
    }
}

fn capture_selectable(
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
    app.hit_regions.add_selectable(SelectableSnapshot {
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
fn row_like(target: &PointerTarget) -> bool {
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
            | PointerTarget::SortChoose(_)
            | PointerTarget::ColumnToggle { .. }
    )
}

fn paint_hover(frame: &mut Frame<'_>, app: &App) {
    let Some(region) = app.hovered_region() else {
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
            | PointerTarget::PaletteQuery
            | PointerTarget::ViewName
    ) {
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

fn paint_selection(frame: &mut Frame<'_>, app: &App) {
    let Some(selection) = app.selection().filter(|selection| !selection.is_empty()) else {
        return;
    };
    let Some(snapshot) = app.hit_regions.selectable(selection.surface) else {
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

fn focused_block<'a>(title: impl Into<Line<'a>>, focused: bool) -> Block<'a> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            theme().accent
        } else {
            theme().muted
        }))
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height - height) / 2),
            Constraint::Length(height),
            Constraint::Fill(1),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width - width) / 2),
            Constraint::Length(width),
            Constraint::Fill(1),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::model::{
        CommentRecord, HistoryRecord, RelationKind, RelationRecord, TicketGraph, TicketKey,
    };
    use crate::pointer::PointerTarget;

    fn ticket() -> Ticket {
        Ticket {
            key: TicketKey {
                organization: "demo".into(),
                id: 10_001,
            },
            project: "atlas".into(),
            revision: 3,
            work_item_type: "Bug".into(),
            title: "Fix ticket search".into(),
            state: "Active".into(),
            reason: Some("Implementation started".into()),
            assigned_to: Some("Avery Chen".into()),
            priority: Some(1),
            area_path: "Atlas\\Platform".into(),
            iteration_path: "Atlas\\Sprint 1".into(),
            tags: vec!["rust".into(), "search".into()],
            description: "A ticket description".into(),
            created_at: crate::timestamp::ts("2026-01-01T00:00:00Z"),
            changed_at: crate::timestamp::ts("2026-01-02T00:00:00Z"),
            web_url: "https://dev.azure.com/demo/atlas/_workitems/edit/10001".into(),
        }
    }

    fn ticket_at(
        id: i64,
        title: &str,
        work_item_type: &str,
        state: &str,
        changed_at: &str,
    ) -> Ticket {
        let mut item = ticket();
        item.key.id = id;
        item.title = title.into();
        item.work_item_type = work_item_type.into();
        item.state = state.into();
        item.changed_at = crate::timestamp::ts(changed_at);
        item.web_url = format!("https://dev.azure.com/demo/atlas/_workitems/edit/{id}");
        item
    }

    fn parent_child_graph() -> TicketGraph {
        let org = |id| TicketKey {
            organization: "demo".into(),
            id,
        };
        TicketGraph {
            relations: vec![
                RelationRecord {
                    from: org(10_002),
                    to: org(10_001),
                    kind: RelationKind::Parent,
                },
                RelationRecord {
                    from: org(10_003),
                    to: org(10_001),
                    kind: RelationKind::Parent,
                },
                RelationRecord {
                    from: org(10_002),
                    to: org(10_004),
                    kind: RelationKind::Child,
                },
                RelationRecord {
                    from: org(10_002),
                    to: org(10_005),
                    kind: RelationKind::Related,
                },
            ],
            ..TicketGraph::default()
        }
    }

    fn render_text(width: u16, height: u16, app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn layouts_render_both_panes_and_expose_hit_regions_at_every_breakpoint() {
        let mut app = App::new(vec![ticket()]);
        let wide = render_text(130, 30, &mut app);
        assert!(wide.contains("Tickets 1/1"));
        assert!(wide.contains("Details"));
        assert!(wide.contains("Fix ticket search"));
        assert!(wide.contains("Pri"));
        assert!(wide.contains("2026-01-01 00:00:00 UTC"));
        assert!(app.hit_regions.detail_url.is_some());

        let table = render_text(60, 20, &mut app);
        assert!(table.contains("[Tickets]"));
        assert!(table.contains("1/1"));
        assert!(table.contains("[Details]"));
        assert!(!table.contains("ID / Type / State"));

        app.narrow_details = true;
        let details = render_text(60, 20, &mut app);
        assert!(details.contains("Details"));
        assert!(!details.contains("[Tickets]"));
        assert!(!details.contains("[Details]"));
        assert!(details.contains("Fix ticket search"));
        app.narrow_details = false;

        for width in [36, 69, 70, 109, 110] {
            render_text(width, 16, &mut app);
            assert!(
                app.hit_regions.search.is_some(),
                "search field missing at width {width}"
            );
            assert!(
                app.hit_regions.table_body.is_some(),
                "table body missing at width {width}"
            );
            if width >= 70 {
                assert!(
                    app.hit_regions.details.is_some(),
                    "details pane missing at width {width}"
                );
            }
        }
    }

    #[test]
    fn empty_reloading_and_no_result_states_render_with_a_usable_search_field() {
        let mut app = App::new(Vec::new());
        let empty = render_text(90, 24, &mut app);
        assert!(empty.contains("No tickets in this database"));

        app.reload_pending = true;
        let loading = render_text(90, 24, &mut app);
        assert!(loading.contains("Reloading tickets"));
        app.reload_pending = false;

        app.mode = AppMode::Search;
        app.set_query("a very long query whose visible tail is unique".into());
        let long_search = render_text(40, 12, &mut app);
        assert!(
            long_search.contains("visible tail is unique"),
            "a long query scrolls to keep the cursor end visible"
        );

        let mut searched = App::new(vec![ticket()]);
        searched.set_query("qqqqqqqqqq".into());
        await_search(&mut searched);
        let no_results = render_text(90, 24, &mut searched);
        assert!(no_results.contains("No tickets match this search"));

        searched.mode = AppMode::Sort;
        let sort = render_text(90, 24, &mut searched);
        assert!(sort.contains("Sort tickets"));
        assert!(sort.contains("Priority"));
    }

    #[test]
    fn help_documents_every_bound_command() {
        let mut app = App::new(Vec::new());
        app.mode = AppMode::Help;
        let mut help = String::new();
        for _ in 0..40 {
            help.push_str(&render_text(90, 24, &mut app));
            if app.help.offset >= app.help.max_offset() {
                break;
            }
            app.help.scroll_by(4);
        }
        for command in COMMANDS.iter().filter(|command| !command.keys.is_empty()) {
            assert!(
                help.contains(command.title),
                "help is missing {}",
                command.title
            );
            assert!(
                help.contains(&command.key_label()),
                "help is missing the {} binding",
                command.title
            );
        }
    }

    #[test]
    fn table_clicks_open_tickets_sort_columns_and_follow_the_row_density() {
        let mut second = ticket();
        second.key.id = 10_002;
        second.title = "Second ticket".into();
        second.tags = vec!["backend".into()];
        second.web_url = "https://dev.azure.com/demo/atlas/_workitems/edit/10002".into();
        let mut app = App::new(vec![ticket(), second]);
        render_text(90, 24, &mut app);

        let id = app.hit_regions.id_column.unwrap();
        let body = app.hit_regions.table_body.unwrap();
        let action = click(&mut app, id.x, body.y + 1);

        assert!(matches!(action, crate::app::AppAction::OpenUrl(_)));
        assert_eq!(app.selected_row(), Some(1));

        let id_header = app
            .hit_regions
            .headers
            .iter()
            .find(|(_, field)| *field == SortField::Id)
            .unwrap()
            .0;
        click(&mut app, id_header.x, id_header.y);
        assert_eq!(app.sort_field, SortField::Id);

        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            KeyModifiers::NONE,
        ));
        assert_eq!(app.row_density, RowDensity::Comfortable);
        let text = render_text(110, 24, &mut app);
        assert!(text.contains("[backend]"), "comfortable rows show tags");
        assert!(text.contains("[rust]"));

        let body = app.hit_regions.table_body.unwrap();
        click(&mut app, body.x + 8, body.y + 2);
        assert_eq!(
            app.selected_row(),
            Some(1),
            "a comfortable row spans two lines"
        );
    }

    #[test]
    fn long_content_is_bounded_and_the_wheel_scrolls_without_moving_the_selection() {
        let mut long_ticket = ticket();
        long_ticket.description = "A long wrapped detail line. ".repeat(30);
        let mut app = App::new(vec![long_ticket]);
        app.narrow_details = true;
        app.focus = Focus::Details;

        let text = render_text(60, 20, &mut app);

        assert!(app.details.max_offset() > 0);
        assert!(text.contains('┃'));
        app.details.offset = usize::MAX;
        render_text(60, 20, &mut app);
        assert_eq!(app.details.offset, app.details.max_offset());

        let tickets = (0..20)
            .map(|index| {
                let mut item = ticket();
                item.key.id += index;
                item.title = format!("Ticket {index}");
                item
            })
            .collect();
        let mut app = App::new(tickets);
        let text = render_text(60, 15, &mut app);
        assert!(
            text.contains('┃'),
            "a long table renders a position scrollbar"
        );
        let selected = app.selected_row();
        let body = app.hit_regions.table_body.unwrap();
        let column = body.x + body.width / 2;
        let row = body.y + 1;
        app.handle_mouse(mouse(MouseEventKind::Moved, column, row));
        assert_eq!(app.hovered(), Some(&PointerTarget::TableRow { index: 1 }));

        app.handle_mouse(mouse(MouseEventKind::ScrollDown, column, row));
        assert_eq!(app.selected_row(), selected, "the wheel selects nothing");
        assert_eq!(app.focus, Focus::Tickets);
        assert!(app.table.offset > 0);
        let mut terminal = Terminal::new(TestBackend::new(60, 15)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(
            app.hovered(),
            Some(&PointerTarget::TableRow {
                index: app.table.offset + 1,
            })
        );
        assert_row_hovered(
            &terminal,
            column,
            row,
            "the ticket under the stationary pointer should remain highlighted",
        );
    }

    /// Foreground colours of one table column, top row first.
    fn column_cell_colors(app: &mut App, field: SortField, rows: usize) -> Vec<Color> {
        let mut terminal = Terminal::new(TestBackend::new(130, 20)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        let header = app
            .hit_regions
            .headers
            .iter()
            .find(|(_, id)| *id == field)
            .expect("column should be visible")
            .0;
        let body = app.hit_regions.table_body.expect("table body should exist");
        let buffer = terminal.backend().buffer();
        (0..rows)
            .map(|row| buffer[(header.x, body.y + u16::try_from(row).unwrap())].fg)
            .collect()
    }

    fn assert_distinct_and_legible(colors: &[Color]) {
        for (index, color) in colors.iter().enumerate() {
            assert_ne!(*color, theme().muted, "column {index} rendered as muted");
            for other in &colors[index + 1..] {
                assert_ne!(
                    color, other,
                    "column colours should be distinct: {colors:?}"
                );
            }
        }
    }

    /// Foreground, background, and modifiers of one rendered buffer cell.
    fn painted_cell(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> (Color, Color, Modifier) {
        let cell = &terminal.backend().buffer()[(x, y)];
        (cell.fg, cell.bg, cell.modifier)
    }

    /// A hovered row tints its background, or reverses where there is no palette.
    fn assert_row_hovered(terminal: &Terminal<TestBackend>, x: u16, y: u16, context: &str) {
        let (_, bg, modifier) = painted_cell(terminal, x, y);
        if theme().hover_background == Color::Reset {
            assert!(modifier.contains(Modifier::REVERSED), "{context}");
        } else {
            assert_eq!(bg, theme().hover_background, "{context}");
        }
    }

    /// Left edge of one table column, shared by the header and the body rows.
    fn column_x(app: &App, field: SortField) -> u16 {
        app.hit_regions
            .headers
            .iter()
            .find(|(_, id)| *id == field)
            .expect("column should be visible")
            .0
            .x
    }

    #[test]
    fn states_and_types_stay_distinct_while_completed_rows_fade() {
        let mut app = App::new(vec![
            ticket_at(10_001, "Alpha", "Issue", "To Do", "2026-03-03T00:00:00Z"),
            ticket_at(10_002, "Beta", "Issue", "Doing", "2026-03-02T00:00:00Z"),
            ticket_at(10_003, "Gamma", "Issue", "Done", "2026-03-01T00:00:00Z"),
        ]);
        if theme() != &Theme::new(true) {
            // NO_COLOR renders every colour as Reset, so only compare palettes.
            let states = column_cell_colors(&mut app, SortField::State, 3);
            assert_distinct_and_legible(&states[..2]);
            assert_eq!(
                states[2],
                theme().muted,
                "the done state should fade with its row"
            );
            let mut open = App::new(vec![
                ticket_at(10_001, "Alpha", "Epic", "To Do", "2026-03-03T00:00:00Z"),
                ticket_at(10_002, "Beta", "Issue", "To Do", "2026-03-02T00:00:00Z"),
                ticket_at(10_003, "Gamma", "Task", "To Do", "2026-03-01T00:00:00Z"),
            ]);
            assert_distinct_and_legible(&column_cell_colors(&mut open, SortField::Type, 3));
        }

        let mut terminal = Terminal::new(TestBackend::new(130, 20)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let title_x = column_x(&app, SortField::Title);
        let state_x = column_x(&app, SortField::State);
        let body = app.hit_regions.table_body.expect("table body");
        let (open_fg, _, open_modifier) = painted_cell(&terminal, title_x, body.y);
        let (done_fg, _, done_modifier) = painted_cell(&terminal, title_x, body.y + 2);
        let (state_fg, _, state_modifier) = painted_cell(&terminal, state_x, body.y + 2);

        if theme().muted == Color::Reset {
            assert!(
                done_modifier.contains(Modifier::DIM),
                "the done row should dim when there is no muted colour"
            );
            assert!(
                !open_modifier.contains(Modifier::DIM),
                "open rows must stay undimmed"
            );
            assert!(
                state_modifier.contains(Modifier::DIM),
                "the done state cell should dim with its row"
            );
        } else {
            assert_eq!(done_fg, theme().muted, "the done title should be muted");
            assert_ne!(open_fg, theme().muted, "the open title should stay bright");
            assert_eq!(
                state_fg,
                theme().muted,
                "the done state cell should fade with its row"
            );
        }
        assert!(
            !state_modifier.contains(Modifier::BOLD),
            "the faded state cell drops the weight open work keeps"
        );

        // The row highlight is painted over the faded cells, so a selected done
        // row stays readable.
        click(&mut app, title_x, body.y + 2);
        assert_eq!(app.selected_row(), Some(2));
        // Park the pointer on another row so the hover tint does not cover the
        // selection background this assertion is about.
        app.handle_mouse(mouse(MouseEventKind::Moved, title_x, body.y));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let (selected_fg, selected_bg, selected_modifier) =
            painted_cell(&terminal, title_x, body.y + 2);
        assert!(
            selected_modifier.contains(Modifier::BOLD),
            "the selected row highlight should still bolden the done row"
        );
        if theme().muted == Color::Reset {
            assert!(selected_modifier.contains(Modifier::DIM));
        } else {
            assert_eq!(selected_fg, theme().muted);
            assert_eq!(selected_bg, theme().selected_background);
        }
    }

    #[test]
    fn my_own_work_items_stand_out_in_the_table_and_the_details_pane() {
        let mut mine = ticket_at(10_002, "Mine", "Issue", "To Do", "2026-03-02T00:00:00Z");
        // Azure DevOps is inconsistent about casing; "mine" should survive it.
        mine.assigned_to = Some("avery chen".into());
        let mut theirs = ticket_at(10_003, "Theirs", "Issue", "To Do", "2026-03-01T00:00:00Z");
        theirs.assigned_to = Some("Jordan Patel".into());
        let mut app = App::new(vec![
            ticket_at(10_001, "Selected", "Issue", "To Do", "2026-03-03T00:00:00Z"),
            mine,
            theirs,
        ]);
        app.set_me(Some("Avery Chen".into()));

        let mut terminal = Terminal::new(TestBackend::new(200, 20)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let assignee_x = column_x(&app, SortField::Assignee);
        // Row 0 is selected, and the selection highlight bolds it either way.
        let body = app.hit_regions.table_body.expect("table body");
        let (mine_fg, _, mine_modifier) = painted_cell(&terminal, assignee_x, body.y + 1);
        let (their_fg, _, their_modifier) = painted_cell(&terminal, assignee_x, body.y + 2);

        assert!(
            mine_modifier.contains(Modifier::BOLD),
            "my own assignee cell should be bold"
        );
        assert!(
            !their_modifier.contains(Modifier::BOLD),
            "someone else's assignee cell should stay plain"
        );
        assert_eq!(mine_fg, theme().accent);
        if theme().accent != Color::Reset {
            assert_ne!(their_fg, theme().accent);
        }

        let ticket = ticket();
        let mut highlighter = QueryHighlighter::new("");
        let mine = ticket_assignment_line(&ticket, true, &mut highlighter);
        let theirs = ticket_assignment_line(&ticket, false, &mut highlighter);

        assert_eq!(mine.spans[1].content, "Avery Chen");
        assert_eq!(mine.spans[1].style.fg, Some(theme().accent));
        assert!(mine.spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(theirs.spans[1].content, "Avery Chen");
        assert!(!theirs.spans[1].style.add_modifier.contains(Modifier::BOLD));

        let mut highlighter = QueryHighlighter::new("chen");
        let matched = ticket_assignment_line(&ticket, true, &mut highlighter);
        let name: String = matched.spans[1..]
            .iter()
            .take_while(|span| span.content.as_ref() != " · ")
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(name, "Avery Chen");
        assert!(
            matched.spans[1..].iter().any(|span| {
                span.style.add_modifier.contains(Modifier::UNDERLINED)
                    && span.content.contains("Chen")
            }),
            "the search match must still show through the mine styling: {matched:?}"
        );
    }

    #[test]
    fn tag_colours_are_stable_and_shared_by_the_table_and_details() {
        assert_eq!(tag_color("tech-debt"), tag_color("TECH-DEBT"));
        assert_eq!(tag_color("Rust"), tag_color("rust"));
        if theme() != &Theme::new(true) {
            // NO_COLOR renders every colour as Reset, so only compare palettes.
            let colors: Vec<Color> = ["docs", "flaky", "perf", "rust"]
                .iter()
                .map(|tag| tag_color(tag))
                .collect();
            assert_distinct_and_legible(&colors);
        }

        let mut app = App::new(vec![ticket()]);
        let tags = app
            .layout
            .columns
            .iter()
            .position(|column| column.id == SortField::Tags)
            .expect("tags column");
        app.layout.toggle_visible(tags);

        let mut terminal = Terminal::new(TestBackend::new(150, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let body = app.hit_regions.table_body.expect("table body");
        let details = app.hit_regions.details.expect("details pane");
        let (table_x, table_y) = find_buffer_text_in(terminal.backend().buffer(), body, "[rust]")
            .expect("tag badge in the table");
        let (details_x, details_y) =
            find_buffer_text_in(terminal.backend().buffer(), details, "[rust]")
                .expect("tag badge in the details pane");

        let (table_fg, _, _) = painted_cell(&terminal, table_x + 1, table_y);
        let (details_fg, _, _) = painted_cell(&terminal, details_x + 1, details_y);
        assert_eq!(table_fg, tag_color("rust"));
        assert_eq!(table_fg, details_fg, "one tag, one colour");
    }

    fn await_search(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.search_pending {
            app.poll_search();
            assert!(Instant::now() < deadline, "search worker timed out");
            thread::yield_now();
        }
    }

    fn find_buffer_text(
        buffer: &ratatui::buffer::Buffer,
        width: u16,
        height: u16,
        needle: &str,
    ) -> Option<(u16, u16)> {
        let chars: Vec<char> = needle.chars().collect();
        for y in 0..height {
            let row: Vec<char> = (0..width)
                .map(|x| buffer[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
            if let Some(start) = row.windows(chars.len()).position(|window| window == chars) {
                return Some((u16::try_from(start).unwrap(), y));
            }
        }
        None
    }

    #[test]
    fn underlines_mark_search_matches_and_stop_after_the_id_digits() {
        let mut app = App::new(vec![ticket()]);
        app.set_query("search".into());
        await_search(&mut app);

        let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let (x, y) = find_buffer_text(buffer, 110, 24, "Fix ticket search")
            .expect("title should be visible");
        let unmatched = buffer[(x, y)].modifier;
        assert!(
            !unmatched.contains(Modifier::UNDERLINED),
            "unmatched prefix should not be underlined"
        );
        let match_start = x + u16::try_from("Fix ticket ".len()).unwrap();
        for offset in 0..u16::try_from("search".len()).unwrap() {
            let modifier = buffer[(match_start + offset, y)].modifier;
            assert!(
                modifier.contains(Modifier::UNDERLINED),
                "expected underline on matched title character {offset}"
            );
        }

        let area = app.hit_regions.id_column.expect("id column");
        let (x, y) = find_buffer_text_in(buffer, area, "10001").expect("id visible in table");
        for offset in 0..5 {
            assert!(
                buffer[(x + offset, y)]
                    .modifier
                    .contains(Modifier::UNDERLINED),
                "digit {offset} should be underlined"
            );
        }
        assert!(
            !buffer[(x + 5, y)].modifier.contains(Modifier::UNDERLINED),
            "padding after the id must not stay underlined"
        );
    }

    fn find_buffer_text_in(
        buffer: &ratatui::buffer::Buffer,
        area: Rect,
        needle: &str,
    ) -> Option<(u16, u16)> {
        let chars: Vec<char> = needle.chars().collect();
        for y in area.y..area.y.saturating_add(area.height) {
            let width = area.width;
            let row: Vec<char> = (0..width)
                .map(|dx| {
                    buffer[(area.x + dx, y)]
                        .symbol()
                        .chars()
                        .next()
                        .unwrap_or(' ')
                })
                .collect();
            if let Some(start) = row.windows(chars.len()).position(|window| window == chars) {
                return Some((area.x + u16::try_from(start).unwrap(), y));
            }
        }
        None
    }

    #[test]
    fn details_render_relationships_history_and_comments() {
        let item = ticket();
        let mut app = App::new(vec![item.clone()]);
        app.narrow_details = true;
        app.focus = Focus::Details;
        app.set_workspace_graph(TicketGraph {
            relations: vec![RelationRecord {
                from: item.key.clone(),
                to: TicketKey {
                    organization: "demo".into(),
                    id: 99,
                },
                kind: RelationKind::Parent,
            }],
            comments: vec![CommentRecord {
                ticket: item.key.clone(),
                comment_id: 1,
                created_at: crate::timestamp::ts("2026-01-03T00:00:00Z"),
                author: Some("Avery Chen".into()),
                text: "Looks good".into(),
            }],
            history: vec![HistoryRecord {
                ticket: item.key,
                revision: 2,
                changed_at: crate::timestamp::ts("2026-01-02T00:00:00Z"),
                changed_by: Some("Jordan Patel".into()),
                field_name: "State".into(),
                old_value: Some("New".into()),
                new_value: Some("Active".into()),
            }],
        });

        let text = render_text(60, 36, &mut app);
        assert!(text.contains("Family"));
        assert!(text.contains("99"));
        assert!(text.contains("missing ticket"));
        assert!(text.contains("History"));
        assert!(text.contains("Comments"));
        assert!(text.contains("Looks good"));
        assert!(!text.contains("Relationships"));
    }

    #[test]
    fn details_render_family_tree_without_other_links() {
        let mut app = App::new(vec![
            ticket_at(
                10_001,
                "Auth rewrite",
                "Feature",
                "Active",
                "2026-01-01T00:00:00Z",
            ),
            ticket_at(
                10_002,
                "Login form",
                "User Story",
                "Active",
                "2026-02-01T00:00:00Z",
            ),
            ticket_at(
                10_003,
                "Logout",
                "User Story",
                "Closed",
                "2026-01-15T00:00:00Z",
            ),
            ticket_at(
                10_004,
                "Validate email",
                "Task",
                "New",
                "2026-01-20T00:00:00Z",
            ),
            ticket_at(
                10_005,
                "Session notes",
                "Task",
                "Active",
                "2026-01-21T00:00:00Z",
            ),
        ]);
        app.set_workspace_graph(parent_child_graph());
        app.narrow_details = true;
        app.focus = Focus::Details;
        assert_eq!(app.selected_ticket().unwrap().key.id, 10_002);

        let text = render_text(60, 36, &mut app);
        assert!(text.contains("Family: Feature 10001  Auth rewrite › this"));
        assert!(text.contains("0/1 closed"));
        assert!(text.contains("10001"));
        assert!(text.contains("10002"));
        assert!(text.contains("10004"));
        assert!(text.contains("10003"));
        assert!(text.contains("current"));
        assert!(text.contains("├─"));
        assert!(text.contains("└─"));
        assert!(text.contains('✓'), "closed family rows carry a check");
        assert!(text.contains('○'), "open family rows carry a circle");
        assert!(!text.contains("Links"));
        assert!(!text.contains("Related"));
        assert!(!text.contains("10005"));
        assert!(!text.contains("Relationships"));
        assert!(!app.hit_regions.detail_links.is_empty());
    }

    fn auth_family_app() -> App {
        let mut app = App::new(vec![
            ticket_at(
                10_001,
                "Auth rewrite",
                "Feature",
                "Active",
                "2026-01-01T00:00:00Z",
            ),
            ticket_at(
                10_002,
                "Login form",
                "User Story",
                "Active",
                "2026-02-01T00:00:00Z",
            ),
            ticket_at(
                10_003,
                "Logout",
                "User Story",
                "Closed",
                "2026-01-15T00:00:00Z",
            ),
            ticket_at(
                10_004,
                "Validate email",
                "Task",
                "New",
                "2026-01-20T00:00:00Z",
            ),
            ticket_at(
                10_005,
                "Session notes",
                "Task",
                "Active",
                "2026-01-21T00:00:00Z",
            ),
        ]);
        app.set_workspace_graph(parent_child_graph());
        app.narrow_details = true;
        app.focus = Focus::Family;
        app
    }

    #[test]
    fn family_rows_show_the_current_and_cursor_styles_and_click_through_to_a_ticket() {
        let mut app = auth_family_app();
        app.family_cursor = Some(TicketKey {
            organization: "demo".into(),
            id: 10_001,
        });
        render_text(60, 24, &mut app);
        let current = app
            .hit_regions
            .find_target(
                |target| matches!(target, PointerTarget::JumpToTicket(key) if key.id == 10_002),
            )
            .map(|region| region.rect)
            .expect("current row");
        let cursor = app
            .hit_regions
            .find_target(
                |target| matches!(target, PointerTarget::JumpToTicket(key) if key.id == 10_001),
            )
            .map(|region| region.rect)
            .expect("cursor row");
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let current_bold = (current.x..current.x.saturating_add(current.width))
            .any(|x| buffer[(x, current.y)].modifier.contains(Modifier::BOLD));
        assert!(current_bold, "current family row should be bold");
        let cursor_style = with_cursor_style(Style::default());
        if cursor_style.add_modifier.contains(Modifier::REVERSED) {
            let reversed = (cursor.x.saturating_sub(2)..cursor.x.saturating_add(12))
                .any(|x| buffer[(x, cursor.y)].modifier.contains(Modifier::REVERSED));
            assert!(
                reversed,
                "family cursor should reverse under a reset background"
            );
        } else {
            let highlighted = (cursor.x.saturating_sub(2)..cursor.x.saturating_add(12))
                .any(|x| buffer[(x, cursor.y)].bg == cursor_style.bg.unwrap_or(Color::Reset));
            assert!(
                highlighted,
                "family cursor should use the selected background"
            );
        }

        app.focus = Focus::Details;
        render_text(72, 36, &mut app);
        let details = app.hit_regions.details.expect("details area");
        let summary_x = details.x.saturating_add(8);
        let summary_y = details.y.saturating_add(3);
        assert!(!matches!(
            app.hit_regions
                .resolve(summary_x, summary_y)
                .map(|region| &region.target),
            Some(PointerTarget::JumpToTicket(_))
        ));
        click(&mut app, summary_x, summary_y);
        assert_eq!(app.selected_ticket().unwrap().key.id, 10_002);
        assert_eq!(app.focus, Focus::Details);

        let row = app
            .hit_regions
            .detail_links
            .iter()
            .find(|(_, key)| key.id == 10_001)
            .map(|(area, _)| *area)
            .expect("parent row");
        click(&mut app, row.x + 8, row.y);
        assert_eq!(app.selected_ticket().unwrap().key.id, 10_001);
        assert_eq!(app.focus, Focus::Family);
    }

    #[test]
    fn hovering_tints_a_row_without_recolouring_it_and_still_reverses_controls() {
        let mut app = App::new(vec![
            ticket_at(10_001, "Alpha", "Issue", "To Do", "2026-03-03T00:00:00Z"),
            ticket_at(10_002, "Beta", "Issue", "Doing", "2026-03-02T00:00:00Z"),
        ]);
        let mut terminal = Terminal::new(TestBackend::new(130, 20)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let state_x = column_x(&app, SortField::State);
        let body = app.hit_regions.table_body.expect("table body");
        let row_y = body.y + 1;
        let (resting_fg, _, resting_modifier) = painted_cell(&terminal, state_x, row_y);

        app.handle_mouse(mouse(MouseEventKind::Moved, state_x, row_y));
        assert_eq!(app.hovered(), Some(&PointerTarget::TableRow { index: 1 }));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let (hovered_fg, hovered_bg, hovered_modifier) = painted_cell(&terminal, state_x, row_y);

        assert_eq!(
            hovered_fg, resting_fg,
            "hover must not repaint the state colour"
        );
        if theme().hover_background == Color::Reset {
            assert!(hovered_modifier.contains(Modifier::REVERSED));
        } else {
            assert_eq!(hovered_bg, theme().hover_background);
            assert!(
                !hovered_modifier.contains(Modifier::REVERSED),
                "a tinted row must not flip its coloured cells into blocks"
            );
            assert_eq!(
                hovered_modifier, resting_modifier,
                "hover must not touch a row's modifiers"
            );

            // The tint is painted after the selection highlight, so it wins.
            let title_x = column_x(&app, SortField::Title);
            assert_eq!(app.selected_row(), Some(0));
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            let (_, selected_bg, _) = painted_cell(&terminal, title_x, body.y);
            assert_eq!(selected_bg, theme().selected_background);
            app.handle_mouse(mouse(MouseEventKind::Moved, title_x, body.y));
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            let (_, hovered_bg, _) = painted_cell(&terminal, title_x, body.y);
            assert_eq!(hovered_bg, theme().hover_background);
            assert_ne!(
                hovered_bg, selected_bg,
                "a hovered selected row must still read differently from a selected one"
            );
        }

        let header = app
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::SortHeader(SortField::Title)))
            .map(|region| region.rect)
            .expect("title sort header");
        app.handle_mouse(mouse(MouseEventKind::Moved, header.x, header.y));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let (_, _, header_modifier) = painted_cell(&terminal, header.x, header.y);
        assert!(
            header_modifier.contains(Modifier::REVERSED),
            "a hovered sort header should stay a reversed block"
        );

        app.mode = AppMode::Help;
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let close = app
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::CloseOverlay))
            .map(|region| region.rect)
            .expect("overlay close button");
        app.handle_mouse(mouse(MouseEventKind::Moved, close.x, close.y));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let (_, _, close_modifier) = painted_cell(&terminal, close.x, close.y);
        assert!(
            close_modifier.contains(Modifier::REVERSED),
            "a hovered close button should stay a reversed block"
        );
    }

    fn auth_family_app_with_long_details() -> App {
        let mut app = auth_family_app();
        let mut tickets = app.tickets().to_vec();
        tickets
            .iter_mut()
            .find(|ticket| ticket.key.id == 10_002)
            .expect("current ticket")
            .description = "line\n".repeat(40);
        let graph = parent_child_graph();
        app.replace_prepared_tickets(crate::app::PreparedTickets::with_graph(tickets, graph));
        app.narrow_details = true;
        app.focus = Focus::Family;
        app
    }

    #[test]
    fn family_hit_targets_follow_the_details_scroll_and_the_wheel_only_scrolls() {
        let mut app = auth_family_app_with_long_details();
        render_text(60, 24, &mut app);
        assert!(app.details.max_offset() > 0);
        let before = app
            .hit_regions
            .find_target(
                |target| matches!(target, PointerTarget::JumpToTicket(key) if key.id == 10_001),
            )
            .map(|region| region.rect.y)
            .expect("parent row should be on screen");
        app.details.scroll_to(app.details.max_offset());
        render_text(60, 24, &mut app);
        let after = app.hit_regions.find_target(
            |target| matches!(target, PointerTarget::JumpToTicket(key) if key.id == 10_001),
        );
        assert!(after.is_none() || after.is_some_and(|region| region.rect.y != before));

        app.details.scroll_to(0);
        render_text(60, 24, &mut app);
        let row = app
            .hit_regions
            .detail_links
            .iter()
            .find(|(_, key)| key.id == 10_002)
            .map(|(area, _)| *area)
            .expect("current family row");
        let cursor = app.family_cursor.clone();
        let focus = app.focus;
        app.handle_mouse(mouse(MouseEventKind::ScrollDown, row.x + 8, row.y));
        assert_eq!(app.family_cursor, cursor, "the wheel moves no cursor");
        assert_eq!(app.focus, focus, "the wheel takes no focus");
        assert!(app.details.offset > 0);
    }

    #[test]
    fn facet_pills_open_their_menu_and_the_filter_overlay_maps_scrolled_clicks() {
        let mut app = App::new(vec![ticket()]);
        let text = render_text(110, 24, &mut app);
        assert!(text.contains("State"));
        assert!(text.contains("Type"));
        assert!(text.contains("▾"));

        let pill = app
            .hit_regions
            .facet_pills
            .iter()
            .find(|(_, target)| matches!(target, FacetTarget::Field(FilterField::Type)))
            .map(|(area, _)| *area)
            .expect("type pill should be clickable");
        click(&mut app, pill.x, pill.y);
        assert_eq!(app.mode, AppMode::Facets);
        assert_eq!(
            FilterField::BAR.get(app.facet_bar.field_index).copied(),
            Some(FilterField::Type)
        );

        app.mode = AppMode::Filter;
        app.filter_overlay.scroll.offset = 2;
        let overlay = render_text(110, 24, &mut app);
        assert!(overlay.contains("Filters"));
        let (x, y) = app
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::FilterRow { index: 2 }))
            .map(|region| (region.rect.x, region.rect.y))
            .expect("scrolled row 2 should be the first visible hit");
        click(&mut app, x, y);
        assert!(app.filter_overlay.showing_values);
        assert_eq!(app.filter_overlay.field_index, 2);
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(app: &mut App, column: u16, row: u16) -> crate::app::AppAction {
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), column, row));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), column, row))
            .action
    }

    #[test]
    fn dragging_a_link_copies_text_while_a_plain_click_opens_it() {
        let mut app = App::new(vec![ticket()]);
        render_text(130, 30, &mut app);
        let url = app.hit_regions.detail_url.expect("detail url");
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), url.x, url.y));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            url.x + 4,
            url.y,
        ));
        let action = app
            .handle_mouse(mouse(
                MouseEventKind::Up(MouseButton::Left),
                url.x + 4,
                url.y,
            ))
            .action;
        assert!(
            matches!(action, crate::app::AppAction::Copy { .. }),
            "drag should copy visible text, got {action:?}"
        );
        assert!(!matches!(action, crate::app::AppAction::OpenUrl(_)));

        let action = click(&mut app, url.x, url.y);
        assert!(
            matches!(action, crate::app::AppAction::OpenUrl(_)),
            "a click without movement still opens the link"
        );

        let body = app.hit_regions.table_body.unwrap();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x + 8,
            body.y,
        ));
        let action = app
            .handle_mouse(mouse(
                MouseEventKind::Up(MouseButton::Left),
                body.x + 8,
                body.y,
            ))
            .action;
        assert!(
            !matches!(action, crate::app::AppAction::Copy { .. }),
            "a zero-width drag copies nothing"
        );
    }

    #[test]
    fn overlay_buttons_and_row_controls_run_their_commands() {
        let mut app = App::new(vec![ticket()]);
        render_text(110, 24, &mut app);
        let (x, y) = app
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::OpenPalette))
            .map(|region| (region.rect.x, region.rect.y))
            .expect("Actions button");
        click(&mut app, x, y);
        assert_eq!(app.mode, AppMode::Palette);

        render_text(110, 24, &mut app);
        let (x, y) = app
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::CloseOverlay))
            .map(|region| (region.rect.x, region.rect.y))
            .expect("palette close");
        click(&mut app, x, y);
        assert_eq!(app.mode, AppMode::Browse);

        render_text(110, 24, &mut app);
        let (x, y) = app
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::OpenHelp))
            .map(|region| (region.rect.x, region.rect.y))
            .expect("help button");
        click(&mut app, x, y);
        assert_eq!(app.mode, AppMode::Help);

        app.mode = AppMode::Browse;
        render_text(110, 24, &mut app);
        let (x, y) = app
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::ToggleRowSelect { index: 0 }))
            .map(|region| (region.rect.x, region.rect.y))
            .expect("row checkbox");
        click(&mut app, x, y);
        assert!(app.is_row_selected(&app.selected_ticket().unwrap().key));

        let (x, y) = app
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::CopyActions))
            .map(|region| (region.rect.x, region.rect.y))
            .expect("details copy");
        click(&mut app, x, y);
        assert_eq!(app.mode, AppMode::Palette);
        assert_eq!(app.palette.query.text(), "copy");
    }

    fn divider(app: &App) -> Rect {
        app.hit_regions
            .find_target(|target| matches!(target, PointerTarget::PaneDivider))
            .expect("pane divider")
            .rect
    }

    fn drag(app: &mut App, from: (u16, u16), to: (u16, u16)) -> crate::app::AppAction {
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            from.0,
            from.1,
        ));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), to.0, to.1));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), to.0, to.1))
            .action
    }

    #[test]
    fn dragging_the_divider_resizes_both_layouts_and_keeps_both_panes_usable() {
        let mut app = App::new(vec![ticket()]);
        let screen = render_text(130, 30, &mut app);
        let before = divider(&app);
        assert_eq!(before.width, 1, "the wide layout leaves a one-cell gap");
        assert_eq!(app.pane_split_wide, 62);
        assert_eq!(
            screen
                .lines()
                .nth(usize::from(before.y + 1))
                .and_then(|row| row.chars().nth(usize::from(before.x))),
            Some('│'),
            "the gap between the panes is drawn as a divider"
        );
        assert!(
            app.hit_regions
                .resolve_scroll(before.x, before.y + 1)
                .is_none(),
            "the wheel over the divider scrolls nothing"
        );

        app.handle_mouse(mouse(MouseEventKind::Moved, before.x, before.y + 1));
        assert_eq!(app.hovered(), Some(&PointerTarget::PaneDivider));
        let mut terminal = Terminal::new(TestBackend::new(130, 30)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(
            terminal.backend().buffer()[(before.x, before.y + 1)]
                .modifier
                .contains(Modifier::REVERSED),
            "the hovered divider is painted reversed"
        );

        app.session_dirty = false;
        let action = drag(
            &mut app,
            (before.x, before.y + 2),
            (before.x + 15, before.y + 2),
        );
        assert!(matches!(action, crate::app::AppAction::None));
        assert!(app.selection().is_none(), "the divider selects no text");
        assert!(app.pane_split_wide > 62);
        assert!(app.session_dirty, "a finished drag is worth persisting");
        render_text(130, 30, &mut app);
        let after = divider(&app);
        assert!(
            after.x > before.x,
            "divider moved from {} to {}",
            before.x,
            after.x
        );

        // Dragging past either edge stops while both panes are still usable.
        drag(&mut app, (after.x, after.y), (0, after.y));
        let leftmost = {
            render_text(130, 30, &mut app);
            divider(&app)
        };
        let content = app.content_area();
        assert!(
            leftmost.x - content.x >= 40,
            "tickets pane kept {} columns",
            leftmost.x - content.x
        );
        drag(&mut app, (leftmost.x, leftmost.y), (129, leftmost.y));
        render_text(130, 30, &mut app);
        let rightmost = divider(&app);
        assert!(
            rightmost.x > leftmost.x,
            "dragging right still moves the divider"
        );
        assert!(
            content.right() - rightmost.right() >= 30,
            "details pane kept {} columns",
            content.right() - rightmost.right()
        );

        let mut stacked = App::new(vec![ticket()]);
        render_text(90, 30, &mut stacked);
        let before = divider(&stacked);
        assert_eq!(before.height, 1, "the stacked layout leaves a one-row gap");
        assert_eq!(stacked.pane_split_stacked, 56);
        let action = drag(
            &mut stacked,
            (before.x + 5, before.y),
            (before.x + 5, before.y + 3),
        );
        assert!(matches!(action, crate::app::AppAction::None));
        assert!(stacked.pane_split_stacked > 56);
        render_text(90, 30, &mut stacked);
        assert!(
            divider(&stacked).y > before.y,
            "the stacked divider moved down"
        );
    }
}
