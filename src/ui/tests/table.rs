use super::*;
use crate::ui::details::ticket_assignment_line;
use crate::ui::table::tag_color;

#[test]
fn table_clicks_open_tickets_sort_columns_and_follow_the_row_density() {
    let mut second = ticket();
    second.key.id = 10_002;
    second.title = "Second ticket".into();
    second.tags = vec!["backend".into()];
    second.web_url = "https://dev.azure.com/demo/atlas/_workitems/edit/10002".into();
    let mut app = App::new(vec![ticket(), second]);
    render_text(90, 24, &mut app);

    let id = target_rect(&app, |target| {
        matches!(target, PointerTarget::OpenTicket { index: 1 })
    });
    let action = click(&mut app, id.x, id.y);

    assert!(matches!(action, crate::app::AppAction::OpenUrl(_)));
    assert_eq!(app.selected_row(), Some(1));

    let id_header = header_rect(&app, SortField::Id);
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

    let body = table_body(&app);
    click(&mut app, body.x + 8, body.y + 2);
    assert_eq!(
        app.selected_row(),
        Some(1),
        "a comfortable row spans two lines"
    );
}

/// Foreground colours of one table column, top row first.
fn column_cell_colors(app: &mut App, field: SortField, rows: usize) -> Vec<Color> {
    let mut terminal = Terminal::new(TestBackend::new(130, 20)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    let header = header_rect(app, field);
    let body = table_body(app);
    let buffer = terminal.backend().buffer();
    (0..rows)
        .map(|row| buffer[(header.x, body.y + u16::try_from(row).unwrap())].fg)
        .collect()
}

#[test]
fn states_and_types_stay_distinct_while_completed_rows_fade() {
    let mut app = App::new(vec![
        ticket_at(10_001, "Alpha", "Issue", "To Do", "2026-03-03T00:00:00Z"),
        ticket_at(10_002, "Beta", "Issue", "Doing", "2026-03-02T00:00:00Z"),
        ticket_at(10_003, "Gamma", "Issue", "Done", "2026-03-01T00:00:00Z"),
    ]);
    // How a finished row is painted, so it has to be on the table to look at.
    app.set_show_finished(true);
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
    let body = table_body(&app);
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
    app.shell.set_me(Some("Avery Chen".into()));

    let mut terminal = Terminal::new(TestBackend::new(200, 20)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let assignee_x = column_x(&app, SortField::Assignee);
    // Row 0 is selected, and the selection highlight bolds it either way.
    let body = table_body(&app);
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
            span.style.add_modifier.contains(Modifier::UNDERLINED) && span.content.contains("Chen")
        }),
        "the search match must still show through the mine styling: {matched:?}"
    );
}

/// Foreground and modifiers of the painted text in one body row of one
/// column, found by stepping past the padding a right-aligned cell leaves.
fn painted_column_cell(
    terminal: &Terminal<TestBackend>,
    column: Rect,
    y: u16,
) -> (Color, Modifier) {
    let buffer = terminal.backend().buffer();
    for x in column.x..column.x.saturating_add(column.width) {
        let cell = &buffer[(x, y)];
        if cell.symbol().trim() != "" {
            return (cell.fg, cell.modifier);
        }
    }
    panic!("column at {} row {y} painted nothing", column.x);
}

#[test]
fn the_changed_cell_flags_work_left_untouched_and_never_finished_work() {
    // Dated far enough back that the fortnight is crossed whenever this
    // runs, so the assertions do not depend on the wall clock.
    let now = OffsetDateTime::now_utc();
    let touched = |id, title, ago: Duration| Ticket {
        changed_at: Timestamp::from_offset_date_time(now - ago),
        ..ticket_at(id, title, "Issue", "To Do", "2026-01-01T00:00:00Z")
    };
    let mut app = App::new(vec![
        // The top row carries the selection, whose own bold would drown
        // out the flag, so nothing is asked of it.
        touched(10_001, "Selected", Duration::from_secs(60)),
        touched(10_002, "Fresh", Duration::from_secs(3600)),
        ticket_at(
            10_003,
            "Neglected",
            "Issue",
            "To Do",
            "2020-01-02T00:00:00Z",
        ),
        ticket_at(10_004, "Finished", "Issue", "Done", "2020-01-01T00:00:00Z"),
    ]);
    // The finished row is the point of the last two assertions, and the
    // table leaves finished work out until asked, so ask.
    app.set_show_finished(true);

    let mut terminal = Terminal::new(TestBackend::new(130, 20)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let column = header_rect(&app, SortField::Changed);
    let body = table_body(&app);
    let cell = |row: u16| painted_column_cell(&terminal, column, body.y + row);

    // Newest first, so the two recent rows lead and the old ones follow.
    let (fresh_fg, fresh_modifier) = cell(1);
    let (stale_fg, stale_modifier) = cell(2);
    let (done_fg, done_modifier) = cell(3);

    assert_eq!(
        stale_fg,
        theme().warning,
        "work nobody has touched in years should be flagged"
    );
    assert!(
        stale_modifier.contains(Modifier::BOLD),
        "bold carries the flag where NO_COLOR leaves no palette"
    );
    assert_ne!(
        (fresh_fg, fresh_modifier.contains(Modifier::BOLD)),
        (stale_fg, true),
        "a row touched today is not flagged"
    );
    assert_ne!(
        (done_fg, done_modifier.contains(Modifier::BOLD)),
        (stale_fg, true),
        "a finished row is never flagged, however long it has sat"
    );
    assert!(
        done_modifier.contains(Modifier::DIM) || done_fg == theme().muted,
        "and it still recedes with the rest of its row"
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
    let body = table_body(&app);
    let details = details_pane(&app);
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
    let (x, y) =
        find_buffer_text(buffer, 110, 24, "Fix ticket search").expect("title should be visible");
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

    let area = target_rect(&app, |target| {
        matches!(target, PointerTarget::OpenTicket { index: 0 })
    });
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
