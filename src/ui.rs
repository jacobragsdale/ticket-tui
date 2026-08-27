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
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

use crate::app::{App, AppMode, Focus, HitRegions, NotificationLevel, RowDensity, SearchOrder};
use crate::filter::FilterField;
use crate::model::{SortField, Ticket};
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
    info: Color,
    error: Color,
    scrollbar: Color,
    search_match: Color,
    state_new: Color,
    state_active: Color,
    state_resolved: Color,
    state_closed: Color,
    priority_critical: Color,
    priority_high: Color,
    priority_normal: Color,
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
                info: Color::Reset,
                error: Color::Reset,
                scrollbar: Color::Reset,
                search_match: Color::Reset,
                state_new: Color::Reset,
                state_active: Color::Reset,
                state_resolved: Color::Reset,
                state_closed: Color::Reset,
                priority_critical: Color::Reset,
                priority_high: Color::Reset,
                priority_normal: Color::Reset,
            }
        } else {
            Self {
                accent: Color::Cyan,
                muted: Color::DarkGray,
                text: Color::White,
                body: Color::Gray,
                link: Color::Blue,
                selected_background: Color::DarkGray,
                info: Color::Yellow,
                error: Color::Red,
                scrollbar: Color::DarkGray,
                search_match: Color::Yellow,
                state_new: Color::Blue,
                state_active: Color::Yellow,
                state_resolved: Color::Magenta,
                state_closed: Color::Green,
                priority_critical: Color::Red,
                priority_high: Color::Yellow,
                priority_normal: Color::Blue,
            }
        }
    }
}

fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| Theme::new(std::env::var_os("NO_COLOR").is_some()))
}

pub fn render(frame: &mut Frame<'_>, app: &mut App) {
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

    let chip_height = u16::from(!app.filter_tokens().is_empty());
    let sections = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(chip_height),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, app, sections[0]);
    if chip_height > 0 {
        render_chips(frame, app, sections[1]);
    }
    render_content(frame, app, sections[2]);
    render_footer(frame, app, sections[3]);

    match app.mode {
        AppMode::Sort => render_sort_popup(frame, app),
        AppMode::Help => render_help_popup(frame, app),
        AppMode::Filter => render_filter_overlay(frame, app),
        AppMode::Columns => render_column_overlay(frame, app),
        AppMode::Palette => render_palette(frame, app),
        AppMode::Views => render_views_overlay(frame, app),
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
    let block = focused_block(title, active);
    let inner = block.inner(area);
    let text = if app.query.is_empty() && !active {
        Line::styled(
            "Type / to search, or filters like state:active priority:1 tag:rust",
            Style::default().fg(theme().muted),
        )
    } else {
        Line::from(app.query.as_str())
    };
    let cursor_offset = u16::try_from(app.query_cursor).unwrap_or(u16::MAX);
    let horizontal_scroll = cursor_offset.saturating_sub(inner.width.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(text)
            .block(block)
            .scroll((0, horizontal_scroll)),
        area,
    );
    app.hit_regions.search = Some(area);

    if active {
        let cursor_x = inner
            .x
            .saturating_add(cursor_offset.saturating_sub(horizontal_scroll));
        frame.set_cursor_position((cursor_x, inner.y));
    }
}

fn render_content(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    if area.width >= WIDE_BREAKPOINT {
        let panes = Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)])
            .spacing(1)
            .split(area);
        render_table(frame, app, panes[0]);
        render_details(frame, app, panes[1]);
    } else if area.width >= NARROW_BREAKPOINT {
        let panes = Layout::vertical([Constraint::Percentage(56), Constraint::Percentage(44)])
            .spacing(1)
            .split(area);
        render_table(frame, app, panes[0]);
        render_details(frame, app, panes[1]);
    } else if app.narrow_details {
        render_details(frame, app, area);
    } else {
        render_table(frame, app, area);
    }
}

fn render_table(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let count = app.visible_count();
    let total = app.tickets().len();
    let ordering = if app.query.is_empty() || app.search_order == SearchOrder::Field {
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
    } else {
        ""
    };
    let title = if area.width < NARROW_BREAKPOINT {
        let short_order = if app.query.is_empty() {
            app.sort_direction.symbol()
        } else {
            match app.search_order {
                SearchOrder::Relevance => "Rel",
                SearchOrder::Field => "Field",
            }
        };
        format!(" Tickets {count}/{total} · {short_order}{activity} · Tab: Details → ")
    } else {
        format!(" Tickets {count}/{total} · {ordering}{activity} ")
    };
    let block = focused_block(title, app.focus == Focus::Tickets);
    let inner = block.inner(area);
    let columns = app.layout.visible_columns(inner.width);
    let constraints: Vec<_> = columns
        .iter()
        .copied()
        .map(crate::columns::TableLayout::constraint)
        .collect();

    let header = Row::new(columns.iter().map(|column| {
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
    }))
    .style(
        Style::default()
            .fg(theme().accent)
            .add_modifier(Modifier::BOLD),
    )
    .height(1)
    .bottom_margin(1);

    let now = OffsetDateTime::now_utc();
    let density = app.row_density;
    let fuzzy = app.fuzzy_query();
    let mut highlighter = QueryHighlighter::new(&fuzzy);
    let rows = app.visible_tickets().map(|ticket| {
        let bookmarked = app.is_bookmarked(&ticket.key);
        let checked = app.is_row_selected(&ticket.key);
        Row::new(columns.iter().map(|column| {
            table_cell(
                ticket,
                column.id,
                now,
                density,
                &mut highlighter,
                bookmarked,
                checked,
            )
        }))
        .height(density.row_height())
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
    frame.render_stateful_widget(table, area, &mut app.table_state);

    app.hit_regions.table = Some(area);
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
        app.hit_regions.headers = header_columns
            .iter()
            .zip(columns.iter())
            .map(|(area, column)| (*area, column.id))
            .collect();
        let body = Rect::new(
            inner.x,
            inner.y.saturating_add(2),
            inner.width,
            inner.height.saturating_sub(2),
        );
        app.hit_regions.table_body = Some(body);
        if let Some(id_area) = header_columns.first() {
            app.hit_regions.id_column =
                Some(Rect::new(id_area.x, body.y, id_area.width, body.height));
        }
        let visible_rows = usize::from(body.height / density.row_height()).max(1);
        if count > visible_rows {
            let mut scrollbar_state = ScrollbarState::new(count)
                .position(app.table_state.offset())
                .viewport_content_length(visible_rows);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .track_symbol(Some("│"))
                    .thumb_symbol("┃")
                    .style(Style::default().fg(theme().scrollbar)),
                body,
                &mut scrollbar_state,
            );
        }
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
    let title = if area.width < NARROW_BREAKPOINT {
        " ← Tab: Tickets · Details "
    } else {
        " Details "
    };
    let block = focused_block(title, app.focus == Focus::Details);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.hit_regions.details = Some(area);

    let Some(ticket) = app.selected_ticket().cloned() else {
        app.set_details_max_scroll(0);
        frame.render_widget(
            Paragraph::new("Select a ticket to view details")
                .style(Style::default().fg(theme().muted)),
            inner,
        );
        return;
    };

    let metadata_height = inner.height.saturating_sub(2).min(4);
    let chunks = Layout::vertical([
        Constraint::Length(metadata_height),
        Constraint::Length((inner.height > metadata_height).into()),
        Constraint::Fill(1),
    ])
    .split(inner);
    let mut highlighter = QueryHighlighter::new(&app.query);
    let title_style = Style::default()
        .fg(theme().text)
        .add_modifier(Modifier::BOLD);
    let metadata = Text::from(vec![
        highlight_line(
            ticket.title.clone(),
            &highlighter.indices(&ticket.title),
            title_style,
            search_match_style(title_style),
        ),
        ticket_identity_line(&ticket, &mut highlighter),
        ticket_assignment_line(&ticket, &mut highlighter),
        field_line(
            "Project / Revision",
            format!(
                "{} / {} · r{}",
                ticket.key.organization, ticket.project, ticket.revision
            ),
        ),
    ]);
    frame.render_widget(Paragraph::new(metadata), chunks[0]);

    if chunks[1].height > 0 {
        frame.render_widget(
            Paragraph::new(Line::styled(
                ticket.web_url.as_str(),
                Style::default()
                    .fg(theme().link)
                    .add_modifier(Modifier::UNDERLINED),
            )),
            chunks[1],
        );
        app.hit_regions.detail_url = Some(chunks[1]);
    }
    if chunks[2].height > 0 {
        let mut detail_lines = vec![
            section_line("Planning"),
            highlighted_field_line("Area", &ticket.area_path, &mut highlighter),
            highlighted_field_line("Iteration", &ticket.iteration_path, &mut highlighter),
            tags_field_line(&ticket.tags, &mut highlighter),
            field_line("Created", exact_timestamp(&ticket.created_at)),
            field_line("Changed", exact_timestamp(&ticket.changed_at)),
            Line::default(),
            section_line("Description"),
        ];
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
            detail_lines.extend(ticket.description.lines().map(Line::from));
        }
        let paragraph = Paragraph::new(Text::from(detail_lines))
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(theme().body));
        let line_count = paragraph.line_count(chunks[2].width);
        let maximum = line_count.saturating_sub(usize::from(chunks[2].height));
        app.set_details_max_scroll(u16::try_from(maximum).unwrap_or(u16::MAX));
        frame.render_widget(paragraph.scroll((app.details_scroll, 0)), chunks[2]);
        if maximum > 0 {
            let mut scrollbar_state = ScrollbarState::new(line_count)
                .position(usize::from(app.details_scroll))
                .viewport_content_length(usize::from(chunks[2].height));
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .track_symbol(Some("│"))
                    .thumb_symbol("┃")
                    .style(Style::default().fg(theme().scrollbar)),
                chunks[2],
                &mut scrollbar_state,
            );
        }
    } else {
        app.set_details_max_scroll(0);
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
            AppMode::Browse if app.focus == Focus::Details => {
                "↑↓/jk scroll details  Tab tickets  Enter/o open  / search  ? help  q quit"
            }
            AppMode::Browse if !app.query.is_empty() => {
                "↑↓/jk move  / edit  f filters  p commands  Enter/o open  Esc clear  ? help  q quit"
            }
            AppMode::Browse => {
                "↑↓/jk move  / search  f filters  p commands  s sort  Enter/o open  ? help  q quit"
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
    let area = centered_rect(frame.area(), 42, 16);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Sort tickets ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme().accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    app.hit_regions.sort_rows.clear();
    let lines = SortField::ALL.iter().enumerate().map(|(index, field)| {
        let selected = index == app.sort_draft.field_index;
        let marker = if selected { "›" } else { " " };
        let direction = if selected {
            app.sort_draft.direction.symbol()
        } else if *field == app.sort_field {
            app.sort_direction.symbol()
        } else {
            " "
        };
        if let Ok(y) = u16::try_from(index) {
            app.hit_regions
                .sort_rows
                .push((Rect::new(inner.x, inner.y + y, inner.width, 1), *field));
        }
        Line::styled(
            format!("{marker} {:<14} {direction}", field.label()),
            if selected {
                Style::default()
                    .bg(theme().selected_background)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        )
    });
    frame.render_widget(Paragraph::new(Text::from_iter(lines)), inner);
}

fn render_help_popup(frame: &mut Frame<'_>, app: &mut App) {
    let height = frame.area().height.saturating_sub(2).min(18);
    let area = centered_rect(frame.area(), 62, height);
    frame.render_widget(Clear, area);
    let help = Text::from(vec![
        Line::styled("Navigation", Style::default().add_modifier(Modifier::BOLD)),
        Line::from("  ↑/↓, j/k       Move ticket or scroll focused pane"),
        Line::from("  PgUp/PgDn       Move ten rows"),
        Line::from("  Home/End        First/last ticket or detail line"),
        Line::from("  Tab             Focus tickets/details"),
        Line::from("  d               Toggle details below 70 columns"),
        Line::from("  c               Toggle compact / comfortable rows"),
        Line::from("  [ / ]           Recently viewed back / forward"),
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
        Line::from("  f               Filter overlay with value counts"),
        Line::from("  Paste           Insert sanitized text"),
        Line::from(""),
        Line::styled("Actions", Style::default().add_modifier(Modifier::BOLD)),
        Line::from("  /               Search core ticket fields"),
        Line::from("  p / :           Command palette"),
        Line::from("  s               Choose field and direction"),
        Line::from("  w               Show, hide, reorder, resize columns"),
        Line::from("  V               Save and restore named views"),
        Line::from("  v               Toggle relevance / field order"),
        Line::from("  m               Bookmark the selected ticket"),
        Line::from("  Space           Toggle multi-select"),
        Line::from("  y               Copy selected IDs"),
        Line::from("  Enter/o         Open selected ticket in browser"),
        Line::from("  r               Reload tickets from SQLite"),
        Line::from("  Esc             Clear active search or selection"),
        Line::from("  q / Ctrl-C      Quit"),
        Line::from(""),
        Line::styled(
            "Press ? or Esc to close",
            Style::default().fg(theme().muted),
        ),
    ]);
    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme().accent));
    let inner = block.inner(area);
    let paragraph = Paragraph::new(help).block(block).wrap(Wrap { trim: false });
    let line_count = paragraph.line_count(area.width);
    let maximum = line_count.saturating_sub(usize::from(inner.height));
    app.set_help_max_scroll(u16::try_from(maximum).unwrap_or(u16::MAX));
    frame.render_widget(paragraph.scroll((app.help_scroll, 0)), area);
    if maximum > 0 {
        let mut scrollbar_state = ScrollbarState::new(line_count)
            .position(usize::from(app.help_scroll))
            .viewport_content_length(usize::from(inner.height));
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .style(Style::default().fg(theme().scrollbar)),
            area,
            &mut scrollbar_state,
        );
    }
}

fn render_chips(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let mut spans = Vec::new();
    let mut x = area.x;
    for token in app.filter_tokens() {
        let label = format!(" {} × ", token.chip_label());
        let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
        if x.saturating_add(width) > area.x.saturating_add(area.width) {
            break;
        }
        app.hit_regions
            .chips
            .push((Rect::new(x, area.y, width, 1), token.clone()));
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

fn render_filter_overlay(frame: &mut Frame<'_>, app: &mut App) {
    let area = centered_rect(frame.area(), 52, 16);
    frame.render_widget(Clear, area);
    let title = if app.filter_overlay.showing_values {
        format!(" {} ", app.facet_field().label())
    } else {
        " Filters ".into()
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme().accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.hit_regions.overlay_rows.clear();

    let lines: Vec<Line> = if app.filter_overlay.showing_values {
        app.current_facets()
            .into_iter()
            .enumerate()
            .map(|(index, facet)| {
                let selected = index == app.filter_overlay.value_index;
                if let Ok(y) = u16::try_from(index) {
                    app.hit_regions.overlay_rows.push((
                        Rect::new(inner.x, inner.y.saturating_add(y), inner.width, 1),
                        index,
                    ));
                }
                let marker = if selected { "›" } else { " " };
                let check = if facet.selected { "[x]" } else { "[ ]" };
                overlay_line(
                    format!("{marker} {check} {:<18} {:>4}", facet.value, facet.count),
                    selected,
                )
            })
            .collect()
    } else {
        FilterField::ALL
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let selected = index == app.filter_overlay.field_index;
                if let Ok(y) = u16::try_from(index) {
                    app.hit_regions.overlay_rows.push((
                        Rect::new(inner.x, inner.y.saturating_add(y), inner.width, 1),
                        index,
                    ));
                }
                let marker = if selected { "›" } else { " " };
                let count = app.parsed_query().filters.selected_count(*field);
                let suffix = if count == 0 {
                    String::new()
                } else {
                    format!("{count} selected")
                };
                overlay_line(format!("{marker} {:<12} {suffix}", field.label()), selected)
            })
            .collect()
    };
    let paragraph = Paragraph::new(Text::from(lines)).scroll((app.overlay_scroll, 0));
    app.set_overlay_max_scroll(
        u16::try_from(
            paragraph
                .line_count(inner.width)
                .saturating_sub(usize::from(inner.height)),
        )
        .unwrap_or(u16::MAX),
    );
    frame.render_widget(paragraph, inner);
}

fn render_column_overlay(frame: &mut Frame<'_>, app: &mut App) {
    let area = centered_rect(frame.area(), 48, 18);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Columns ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme().accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.hit_regions.overlay_rows.clear();
    let lines = app
        .layout
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let selected = index == app.column_overlay.index;
            if let Ok(y) = u16::try_from(index) {
                app.hit_regions.overlay_rows.push((
                    Rect::new(inner.x, inner.y.saturating_add(y), inner.width, 1),
                    index,
                ));
            }
            let marker = if selected { "›" } else { " " };
            let check = if column.visible { "[x]" } else { "[ ]" };
            let width = if column.id == SortField::Title {
                "fill".into()
            } else {
                column.width.to_string()
            };
            overlay_line(
                format!("{marker} {check} {:<12} {width}", column.id.label()),
                selected,
            )
        });
    frame.render_widget(Paragraph::new(Text::from_iter(lines)), inner);
}

fn render_palette(frame: &mut Frame<'_>, app: &mut App) {
    let commands = app.palette_commands();
    let height = u16::try_from(commands.len().saturating_add(3))
        .unwrap_or(u16::MAX)
        .min(16);
    let area = centered_rect(frame.area(), 56, height.max(6));
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(" Commands / {} ", app.palette.query))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme().accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.hit_regions.overlay_rows.clear();
    let lines = commands.iter().enumerate().map(|(index, command)| {
        let selected = index == app.palette.selected;
        if let Ok(y) = u16::try_from(index) {
            app.hit_regions.overlay_rows.push((
                Rect::new(inner.x, inner.y.saturating_add(y), inner.width, 1),
                index,
            ));
        }
        let marker = if selected { "›" } else { " " };
        overlay_line(
            format!("{marker} {:<28} {}", command.title, command.hint),
            selected,
        )
    });
    frame.render_widget(Paragraph::new(Text::from_iter(lines)), inner);
}

fn render_views_overlay(frame: &mut Frame<'_>, app: &mut App) {
    let area = centered_rect(frame.area(), 48, 14);
    frame.render_widget(Clear, area);
    let title = if let Some(name) = &app.views_overlay.naming {
        format!(" Save view: {name} ")
    } else {
        " Views ".into()
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme().accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.hit_regions.overlay_rows.clear();
    if app.views().is_empty() && app.views_overlay.naming.is_none() {
        frame.render_widget(
            Paragraph::new("No saved views. Press n to save the current view.")
                .style(Style::default().fg(theme().muted)),
            inner,
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
    let lines = views
        .into_iter()
        .enumerate()
        .map(|(index, (name, query, current))| {
            let selected = index == app.views_overlay.index;
            if let Ok(y) = u16::try_from(index) {
                app.hit_regions.overlay_rows.push((
                    Rect::new(inner.x, inner.y.saturating_add(y), inner.width, 1),
                    index,
                ));
            }
            let marker = if selected { "›" } else { " " };
            let current = if current { "*" } else { " " };
            overlay_line(format!("{marker}{current} {name:<18} {query}"), selected)
        });
    frame.render_widget(Paragraph::new(Text::from_iter(lines)), inner);
}

fn overlay_line(text: String, selected: bool) -> Line<'static> {
    Line::styled(
        text,
        if selected {
            Style::default()
                .bg(theme().selected_background)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        },
    )
}

fn table_cell(
    ticket: &Ticket,
    field: SortField,
    now: OffsetDateTime,
    density: RowDensity,
    highlighter: &mut QueryHighlighter,
    bookmarked: bool,
    checked: bool,
) -> Cell<'static> {
    let line = match field {
        SortField::Type => Line::from(type_badge_spans(&ticket.work_item_type, highlighter)),
        SortField::Title => {
            let mut line = highlight_searchable(&ticket.title, Style::default(), highlighter);
            if checked {
                let mut spans = vec![Span::styled(
                    "* ",
                    Style::default()
                        .fg(theme().accent)
                        .add_modifier(Modifier::BOLD),
                )];
                spans.extend(line.spans);
                line = Line::from(spans);
            }
            line
        }
        SortField::Id => {
            let style = Style::default()
                .fg(theme().link)
                .add_modifier(Modifier::UNDERLINED);
            let mut text = ticket.key.id.to_string();
            if bookmarked {
                text.push('*');
            }
            highlight_searchable(&text, style, highlighter)
        }
        SortField::State => {
            highlight_searchable(&ticket.state, state_style(&ticket.state), highlighter)
        }
        SortField::Assignee => {
            let (text, searchable) = ticket.assigned_to.as_deref().map_or_else(
                || ("Unassigned".to_owned(), false),
                |name| (name.to_owned(), true),
            );
            if searchable {
                highlight_searchable(&text, Style::default(), highlighter)
            } else {
                Line::styled(text, Style::default().fg(theme().muted))
            }
        }
        SortField::Priority => Line::from(
            ticket
                .priority
                .map_or_else(|| "—".into(), |priority| priority.to_string()),
        )
        .right_aligned()
        .style(priority_style(ticket.priority)),
        SortField::Changed => {
            Line::from(relative_changed_at(&ticket.changed_at, now)).right_aligned()
        }
        SortField::Created => {
            Line::from(relative_changed_at(&ticket.created_at, now)).right_aligned()
        }
        SortField::Organization => {
            highlight_searchable(&ticket.key.organization, Style::default(), highlighter)
        }
        SortField::Project => highlight_searchable(&ticket.project, Style::default(), highlighter),
        SortField::Area => highlight_searchable(&ticket.area_path, Style::default(), highlighter),
        SortField::Iteration => {
            highlight_searchable(&ticket.iteration_path, Style::default(), highlighter)
        }
        SortField::Tags => Line::from(tag_badge_spans(&ticket.tags, highlighter)),
    };
    let line = if field == SortField::Id {
        line.right_aligned()
    } else {
        line
    };

    if density == RowDensity::Comfortable && field == SortField::Title {
        Cell::from(Text::from(vec![
            line,
            Line::from(tag_badge_spans(&ticket.tags, highlighter)),
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
        return Line::styled(text, base);
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

fn type_style(work_item_type: &str) -> Style {
    let color = match work_item_type.to_ascii_lowercase().as_str() {
        "bug" => theme().priority_critical,
        "task" => theme().accent,
        "user story" | "story" => theme().state_new,
        "feature" => theme().state_resolved,
        "epic" => theme().state_active,
        _ => theme().muted,
    };
    Style::default().fg(color)
}

fn type_badge_spans(
    work_item_type: &str,
    highlighter: &mut QueryHighlighter,
) -> Vec<Span<'static>> {
    badge_spans(work_item_type, type_style(work_item_type), highlighter)
}

fn tag_badge_spans(tags: &[String], highlighter: &mut QueryHighlighter) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, tag) in tags.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        spans.extend(badge_spans(
            tag,
            Style::default().fg(theme().muted),
            highlighter,
        ));
    }
    spans
}

fn badge_spans(
    label: &str,
    style: Style,
    highlighter: &mut QueryHighlighter,
) -> Vec<Span<'static>> {
    let inner = highlight_searchable(label, style.add_modifier(Modifier::BOLD), highlighter);
    let mut spans = Vec::with_capacity(inner.spans.len() + 2);
    spans.push(Span::styled("[", style));
    spans.extend(inner.spans);
    spans.push(Span::styled("]", style));
    spans
}

fn state_style(state: &str) -> Style {
    let color = match state.to_ascii_lowercase().as_str() {
        "new" => theme().state_new,
        "active" => theme().state_active,
        "resolved" => theme().state_resolved,
        "closed" => theme().state_closed,
        _ => theme().muted,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
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

fn relative_changed_at(timestamp: &str, now: OffsetDateTime) -> String {
    let Ok(changed) = OffsetDateTime::parse(timestamp, &Rfc3339) else {
        return short_date(timestamp).to_owned();
    };
    let age = now - changed;
    if age.is_negative() {
        return short_date(timestamp).to_owned();
    }
    if age.whole_minutes() < 1 {
        return "now".into();
    }
    if age.whole_hours() < 1 {
        return format!("{}m", age.whole_minutes());
    }
    if age.whole_days() < 1 {
        return format!("{}h", age.whole_hours());
    }
    if age.whole_days() < 7 {
        return format!("{}d", age.whole_days());
    }
    if changed.year() == now.year() {
        return changed
            .format(format_description!("[month repr:short] [day padding:none]"))
            .unwrap_or_else(|_| short_date(timestamp).to_owned());
    }
    short_date(timestamp).to_owned()
}

fn exact_timestamp(timestamp: &str) -> String {
    OffsetDateTime::parse(timestamp, &Rfc3339)
        .ok()
        .and_then(|value| {
            value
                .to_offset(UtcOffset::UTC)
                .format(format_description!(
                    "[year]-[month]-[day] [hour]:[minute]:[second] UTC"
                ))
                .ok()
        })
        .unwrap_or_else(|| timestamp.to_owned())
}

fn short_date(timestamp: &str) -> &str {
    timestamp.get(..10).unwrap_or(timestamp)
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
    spans.extend(type_badge_spans(&ticket.work_item_type, highlighter));
    spans.push(Span::raw(" · "));
    spans.extend(highlight_searchable(&ticket.state, state, highlighter).spans);
    Line::from(spans)
}

fn ticket_assignment_line(ticket: &Ticket, highlighter: &mut QueryHighlighter) -> Line<'static> {
    let priority = ticket
        .priority
        .map_or_else(|| "—".into(), |priority| priority.to_string());
    let assignee = ticket
        .assigned_to
        .clone()
        .unwrap_or_else(|| "Unassigned".into());
    let assignee_line = if ticket.assigned_to.is_some() {
        highlight_searchable(&assignee, Style::default(), highlighter)
    } else {
        Line::styled(assignee, Style::default().fg(theme().muted))
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
        spans.extend(tag_badge_spans(tags, highlighter));
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
    use time::macros::datetime;

    use super::*;
    use crate::model::TicketKey;

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
            created_at: "2026-01-01T00:00:00Z".into(),
            changed_at: "2026-01-02T00:00:00Z".into(),
            web_url: "https://dev.azure.com/demo/atlas/_workitems/edit/10001".into(),
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
    fn wide_layout_renders_table_and_details() {
        let mut app = App::new(vec![ticket()]);
        let text = render_text(130, 30, &mut app);

        assert!(text.contains("Tickets 1/1"));
        assert!(text.contains("Details"));
        assert!(text.contains("Fix ticket search"));
        assert!(text.contains("Pri"));
        assert!(text.contains("2026-01-01 00:00:00 UTC"));
        assert!(app.hit_regions.detail_url.is_some());
    }

    #[test]
    fn narrow_layout_can_toggle_details() {
        let mut app = App::new(vec![ticket()]);
        let table = render_text(60, 20, &mut app);
        assert!(table.contains("Tickets 1/1"));
        assert!(table.contains("Tab: Details"));
        assert!(!table.contains("ID / Type / State"));

        app.narrow_details = true;
        let details = render_text(60, 20, &mut app);
        assert!(details.contains("Details"));
        assert!(details.contains("Tab: Tickets"));
        assert!(details.contains("Fix ticket search"));
    }

    #[test]
    fn empty_state_and_help_render_without_a_selection() {
        let mut app = App::new(Vec::new());
        let empty = render_text(90, 24, &mut app);
        assert!(empty.contains("No tickets in this database"));

        app.reload_pending = true;
        let loading = render_text(90, 24, &mut app);
        assert!(loading.contains("Reloading tickets"));
        app.reload_pending = false;

        app.mode = AppMode::Help;
        let help = render_text(90, 24, &mut app);
        assert!(help.contains("Navigation"));
        assert!(app.help_max_scroll > 0);
        app.help_scroll = app.help_max_scroll;
        let scrolled_help = render_text(90, 24, &mut app);
        assert!(scrolled_help.contains("Open selected ticket"));
    }

    #[test]
    fn stacked_layout_exposes_working_mouse_regions() {
        let mut second = ticket();
        second.key.id = 10_002;
        second.title = "Second ticket".into();
        second.web_url = "https://dev.azure.com/demo/atlas/_workitems/edit/10002".into();
        let mut app = App::new(vec![ticket(), second]);
        render_text(90, 24, &mut app);

        let id = app.hit_regions.id_column.unwrap();
        let body = app.hit_regions.table_body.unwrap();
        let action = app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: id.x,
            row: body.y + 1,
            modifiers: KeyModifiers::NONE,
        });

        assert!(matches!(action, crate::app::AppAction::OpenUrl(_)));
        assert_eq!(app.selected_row(), Some(1));

        let id_header = app
            .hit_regions
            .headers
            .iter()
            .find(|(_, field)| *field == SortField::Id)
            .unwrap()
            .0;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: id_header.x,
            row: id_header.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.sort_field, SortField::Id);
    }

    #[test]
    fn no_results_and_sort_menu_are_rendered() {
        let mut app = App::new(vec![ticket()]);
        app.set_query("qqqqqqqqqq".into());
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.search_pending {
            app.poll_search();
            assert!(Instant::now() < deadline, "search worker timed out");
            thread::yield_now();
        }
        let no_results = render_text(90, 24, &mut app);
        assert!(no_results.contains("No tickets match this search"));

        app.mode = AppMode::Sort;
        let sort = render_text(90, 24, &mut app);
        assert!(sort.contains("Sort tickets"));
        assert!(sort.contains("Priority"));
    }

    #[test]
    fn long_details_are_bounded_and_render_a_scrollbar() {
        let mut long_ticket = ticket();
        long_ticket.description = "A long wrapped detail line. ".repeat(30);
        let mut app = App::new(vec![long_ticket]);
        app.narrow_details = true;
        app.focus = Focus::Details;

        let text = render_text(60, 20, &mut app);

        assert!(app.details_max_scroll > 0);
        assert!(text.contains('┃'));
        app.details_scroll = u16::MAX;
        render_text(60, 20, &mut app);
        assert_eq!(app.details_scroll, app.details_max_scroll);
    }

    #[test]
    fn help_can_scroll_in_a_short_terminal() {
        let mut app = App::new(Vec::new());
        app.mode = AppMode::Help;
        let initial = render_text(50, 10, &mut app);
        assert!(initial.contains("Navigation"));
        assert!(app.help_max_scroll > 0);

        app.help_scroll = app.help_max_scroll;
        let scrolled = render_text(50, 10, &mut app);
        assert_ne!(initial, scrolled);
        assert!(scrolled.contains("Press ? or Esc to close"));
    }

    #[test]
    fn long_search_keeps_the_cursor_end_visible() {
        let mut app = App::new(Vec::new());
        app.mode = AppMode::Search;
        app.set_query("a very long query whose visible tail is unique".into());

        let text = render_text(40, 12, &mut app);

        assert!(text.contains("visible tail is unique"));
    }

    #[test]
    fn long_ticket_table_renders_a_position_scrollbar() {
        let tickets = (0..30)
            .map(|index| {
                let mut item = ticket();
                item.key.id += index;
                item.title = format!("Ticket {index}");
                item
            })
            .collect();
        let mut app = App::new(tickets);

        let text = render_text(60, 15, &mut app);

        assert!(text.contains('┃'));
    }

    #[test]
    fn changed_dates_use_compact_relative_labels() {
        let now = datetime!(2026-08-26 18:00 UTC);

        assert_eq!(relative_changed_at("2026-08-26T17:30:00Z", now), "30m");
        assert_eq!(relative_changed_at("2026-08-26T12:00:00Z", now), "6h");
        assert_eq!(relative_changed_at("2026-08-23T18:00:00Z", now), "3d");
        assert_eq!(relative_changed_at("2026-07-01T00:00:00Z", now), "Jul 1");
        assert_eq!(
            relative_changed_at("2025-07-01T00:00:00Z", now),
            "2025-07-01"
        );
    }

    #[test]
    fn details_normalize_exact_timestamps_to_utc() {
        assert_eq!(
            exact_timestamp("2026-08-26T13:00:00-05:00"),
            "2026-08-26 18:00:00 UTC"
        );
    }

    #[test]
    fn monochrome_theme_resets_every_semantic_color() {
        let monochrome = Theme::new(true);

        assert_eq!(monochrome.accent, Color::Reset);
        assert_eq!(monochrome.state_active, Color::Reset);
        assert_eq!(monochrome.priority_critical, Color::Reset);
        assert_eq!(monochrome.search_match, Color::Reset);
        assert_eq!(monochrome.error, Color::Reset);
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
    fn search_results_underline_matched_title_characters() {
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
    }

    #[test]
    fn types_and_tags_render_as_compact_badges() {
        let mut app = App::new(vec![ticket()]);
        let text = render_text(110, 24, &mut app);

        assert!(text.contains("[Bug]"));
        assert!(text.contains("[rust]"));
        assert!(text.contains("[search]"));
        assert!(!text.contains("Tags: rust, search"));
    }

    #[test]
    fn filter_chips_and_overlay_render_active_filters() {
        let mut app = App::new(vec![ticket()]);
        app.set_query("state:active type:bug".into());
        await_search(&mut app);

        let text = render_text(110, 24, &mut app);
        assert!(text.contains("state:active"));
        assert!(text.contains("type:bug"));

        app.mode = AppMode::Filter;
        let overlay = render_text(110, 24, &mut app);
        assert!(overlay.contains("Filters"));
        assert!(overlay.contains("State"));
    }

    #[test]
    fn command_palette_lists_matching_actions() {
        let mut app = App::new(vec![ticket()]);
        app.mode = AppMode::Palette;
        app.palette.query = "copy".into();
        let text = render_text(110, 24, &mut app);
        assert!(text.contains("Copy ID"));
        assert!(text.contains("Commands"));
    }

    #[test]
    fn comfortable_rows_show_tags_in_the_table_and_change_click_mapping() {
        let mut second = ticket();
        second.key.id = 10_002;
        second.title = "Second ticket".into();
        second.tags = vec!["backend".into()];
        let mut app = App::new(vec![ticket(), second]);
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            KeyModifiers::NONE,
        ));
        assert_eq!(app.row_density, RowDensity::Comfortable);

        let text = render_text(110, 24, &mut app);
        assert!(text.contains("[backend]"));
        assert!(text.contains("[rust]"));

        let body = app.hit_regions.table_body.unwrap();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: body.x + 4,
            row: body.y + 2,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.selected_row(), Some(1));
    }
}
