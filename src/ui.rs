use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Table, Wrap,
};

use crate::app::{App, AppMode, Focus, HitRegions, NotificationLevel, SearchOrder};
use crate::model::{SortField, Ticket};

const WIDE_BREAKPOINT: u16 = 110;
const NARROW_BREAKPOINT: u16 = 70;

#[derive(Clone, Copy)]
struct ColumnDef {
    field: SortField,
    constraint: Constraint,
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

    let sections = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, app, sections[0]);
    render_content(frame, app, sections[1]);
    render_footer(frame, app, sections[2]);

    match app.mode {
        AppMode::Sort => render_sort_popup(frame, app),
        AppMode::Help => render_help_popup(frame, app),
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
            "Type / to search ID, title, assignee, state, type, area, iteration, or tags",
            Style::default().fg(Color::DarkGray),
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
    let columns = columns_for(inner.width);
    let constraints: Vec<_> = columns.iter().map(|column| column.constraint).collect();

    let header = Row::new(columns.iter().map(|column| {
        let direction = if column.field == app.sort_field {
            app.sort_direction.symbol()
        } else {
            ""
        };
        Cell::from(format!("{}{}", column.field.label(), direction))
    }))
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .height(1)
    .bottom_margin(1);

    let rows = app.visible_tickets().map(|ticket| {
        Row::new(columns.iter().map(|column| {
            let cell = Cell::from(cell_value(ticket, column.field));
            if column.field == SortField::Id {
                cell.style(
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::UNDERLINED),
                )
            } else {
                cell
            }
        }))
    });
    let table = Table::new(rows, constraints.clone())
        .header(header)
        .block(block)
        .column_spacing(1)
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
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
            .map(|(area, column)| (*area, column.field))
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
    }

    if count == 0 && inner.height > 2 {
        let message = if app.reload_pending {
            "Reloading tickets…"
        } else if app.query.is_empty() {
            "No tickets in this database"
        } else if app.search_pending {
            "Searching…"
        } else {
            "No tickets match this search"
        };
        frame.render_widget(
            Paragraph::new(message)
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray)),
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
                .style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    };

    let metadata_height = inner.height.saturating_sub(2).min(8);
    let chunks = Layout::vertical([
        Constraint::Length(metadata_height),
        Constraint::Length((inner.height > metadata_height).into()),
        Constraint::Fill(1),
    ])
    .split(inner);
    let metadata = Text::from(vec![
        Line::styled(
            ticket.title.as_str(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        field_line(
            "ID / Type / State",
            format!(
                "{} · {} · {}",
                ticket.key.id, ticket.work_item_type, ticket.state
            ),
        ),
        field_line(
            "Assignee / Priority",
            format!(
                "{} · {}",
                ticket.assigned_to.as_deref().unwrap_or("Unassigned"),
                ticket
                    .priority
                    .map_or_else(|| "—".into(), |priority| priority.to_string())
            ),
        ),
        field_line(
            "Project / Revision",
            format!(
                "{} / {} · r{}",
                ticket.key.organization, ticket.project, ticket.revision
            ),
        ),
        field_line("Area", ticket.area_path.as_str()),
        field_line("Iteration", ticket.iteration_path.as_str()),
        field_line("Tags", ticket.tags.join(", ")),
        field_line(
            "Created / Changed",
            format!(
                "{} · {}",
                short_date(&ticket.created_at),
                short_date(&ticket.changed_at)
            ),
        ),
    ]);
    frame.render_widget(Paragraph::new(metadata), chunks[0]);

    if chunks[1].height > 0 {
        frame.render_widget(
            Paragraph::new(Line::styled(
                ticket.web_url.as_str(),
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::UNDERLINED),
            )),
            chunks[1],
        );
        app.hit_regions.detail_url = Some(chunks[1]);
    }
    if chunks[2].height > 0 {
        let reason = ticket
            .reason
            .as_deref()
            .map_or_else(String::new, |reason| format!("Reason: {reason}\n\n"));
        let paragraph = Paragraph::new(format!("{reason}{}", ticket.description))
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(Color::Gray));
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
                    .style(Style::default().fg(Color::DarkGray)),
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
            NotificationLevel::Info => Color::Yellow,
            NotificationLevel::Error => Color::Red,
        };
        (message, Style::default().fg(color))
    } else {
        let text = match app.mode {
            AppMode::Search => {
                "←→ cursor  Ctrl-P/N history  Ctrl-W delete word  Ctrl-U clear  Enter/Esc finish"
            }
            AppMode::Sort => "↑↓ choose field  ←→ direction  Enter apply  Esc cancel",
            AppMode::Help => "↑↓/jk scroll  PgUp/PgDn page  Home/End jump  ?/Esc close",
            AppMode::Browse if app.focus == Focus::Details => {
                "↑↓/jk scroll details  Tab tickets  Enter/o open  / search  ? help  q quit"
            }
            AppMode::Browse if !app.query.is_empty() => {
                "↑↓/jk move  / edit search  v order  s sort  Enter/o open  Esc clear  ? help  q quit"
            }
            AppMode::Browse => {
                "↑↓/jk move  / search  s sort  Enter/o open  r reload  Tab details  ? help  q quit"
            }
        };
        (text, Style::default().fg(Color::DarkGray))
    };
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(style),
        area,
    );
}

fn render_sort_popup(frame: &mut Frame<'_>, app: &mut App) {
    let area = centered_rect(frame.area(), 42, 11);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Sort tickets ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
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
                    .bg(Color::DarkGray)
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
        Line::from(""),
        Line::styled("Search", Style::default().add_modifier(Modifier::BOLD)),
        Line::from("  ←/→, Home/End   Move the query cursor"),
        Line::from("  Backspace/Del   Delete around the cursor"),
        Line::from("  Ctrl-W/Ctrl-U   Delete word / clear query"),
        Line::from("  Ctrl-P/Ctrl-N   Previous / next completed query"),
        Line::from("  Paste           Insert sanitized text"),
        Line::from(""),
        Line::styled("Actions", Style::default().add_modifier(Modifier::BOLD)),
        Line::from("  /               Search core ticket fields"),
        Line::from("  s               Choose field and direction"),
        Line::from("  v               Toggle relevance / field order"),
        Line::from("  Enter/o         Open selected ticket in browser"),
        Line::from("  r               Reload tickets from SQLite"),
        Line::from("  Esc             Clear active search"),
        Line::from("  q / Ctrl-C      Quit"),
        Line::from(""),
        Line::styled(
            "Press ? or Esc to close",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
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
                .style(Style::default().fg(Color::DarkGray)),
            area,
            &mut scrollbar_state,
        );
    }
}

fn columns_for(width: u16) -> Vec<ColumnDef> {
    let mut columns = vec![
        ColumnDef {
            field: SortField::Id,
            constraint: Constraint::Length(7),
        },
        ColumnDef {
            field: SortField::Title,
            constraint: Constraint::Fill(1),
        },
    ];
    if width >= 36 {
        columns.push(ColumnDef {
            field: SortField::State,
            constraint: Constraint::Length(10),
        });
    }
    if width >= 52 {
        columns.push(ColumnDef {
            field: SortField::Type,
            constraint: Constraint::Length(11),
        });
    }
    if width >= 65 {
        columns.push(ColumnDef {
            field: SortField::Priority,
            constraint: Constraint::Length(4),
        });
    }
    if width >= 78 {
        columns.push(ColumnDef {
            field: SortField::Changed,
            constraint: Constraint::Length(10),
        });
    }
    if width >= 98 {
        columns.push(ColumnDef {
            field: SortField::Assignee,
            constraint: Constraint::Length(16),
        });
    }
    columns
}

fn cell_value(ticket: &Ticket, field: SortField) -> String {
    match field {
        SortField::Changed => short_date(&ticket.changed_at).to_owned(),
        SortField::Priority => ticket
            .priority
            .map_or_else(|| "—".into(), |p| p.to_string()),
        SortField::Id => ticket.key.id.to_string(),
        SortField::Title => ticket.title.clone(),
        SortField::State => ticket.state.clone(),
        SortField::Type => ticket.work_item_type.clone(),
        SortField::Assignee => ticket
            .assigned_to
            .clone()
            .unwrap_or_else(|| "Unassigned".into()),
    }
}

fn short_date(timestamp: &str) -> &str {
    timestamp.get(..10).unwrap_or(timestamp)
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

fn focused_block<'a>(title: impl Into<Line<'a>>, focused: bool) -> Block<'a> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
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
}
