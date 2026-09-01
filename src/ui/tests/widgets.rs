use super::*;

/// The one contiguous run of thumb glyphs down a scrollbar track, as
/// (first row, height). Every other row of the track has to be track, so a
/// gap, a stray glyph or a second run fails here rather than silently
/// matching.
fn painted_thumb(terminal: &Terminal<TestBackend>, track: Rect) -> Option<(u16, u16)> {
    let buffer = terminal.backend().buffer();
    let rows = track.y..track.y.saturating_add(track.height);
    let mut painted = Vec::new();
    for y in rows {
        match buffer[(track.x, y)].symbol() {
            "┃" => painted.push(y),
            "│" => {}
            other => panic!("the track column holds only scrollbar glyphs, found {other:?}"),
        }
    }
    let first = *painted.first()?;
    let last = *painted.last()?;
    let height = last - first + 1;
    assert_eq!(
        usize::from(height),
        painted.len(),
        "the thumb is one contiguous run"
    );
    Some((first, height))
}

#[test]
fn the_table_thumb_is_painted_where_it_can_be_grabbed_and_reaches_the_bottom() {
    let tickets = (0..100)
        .map(|index| {
            let mut item = ticket();
            item.key.id += index;
            item.title = format!("Ticket {index}");
            item
        })
        .collect();
    let mut app = App::new(tickets);
    // No chip bar over the table, so the arithmetic below is the table's.
    app.work_items.set_show_finished(&mut app.shell, true);
    for offset in [0, 45, 90] {
        app.work_items.table.offset = offset;
        // 30 rows of terminal, less the tab bar, the search row, the facet
        // row and the footer, leave the table body exactly 22 rows tall.
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let metrics = app
            .shell
            .hit_regions
            .scroll(ScrollSurface::Table)
            .expect("an overflowing table registers its scrollbar");
        assert_eq!((metrics.content, metrics.viewport), (100, 22));
        let track = metrics.track;
        assert_eq!(track.height, 22);
        let thumb = metrics.thumb().expect("100 rows overflow 22");
        assert_eq!(
            painted_thumb(&terminal, track),
            Some((track.y + thumb.y, thumb.height)),
            "the painted thumb is the draggable thumb at offset {}",
            metrics.offset
        );
        if metrics.offset == 0 {
            assert_eq!(
                track.y + thumb.y,
                track.y,
                "offset 0 starts the thumb flush"
            );
        }
        if metrics.offset == metrics.max_offset() {
            assert_eq!(
                track.y + thumb.y + thumb.height,
                track.y + track.height,
                "the last offset finishes the thumb on the last row of the track"
            );
        }
    }
    assert_eq!(
        app.work_items.table.offset,
        app.work_items.table.max_offset(),
        "90 clamps to the end"
    );
}

#[test]
fn the_details_thumb_finishes_on_the_last_row_of_its_track() {
    let mut long_ticket = ticket();
    long_ticket.description = "A long wrapped detail line. ".repeat(40);
    let mut app = App::new(vec![long_ticket]);
    app.shell.narrow_details = true;
    app.shell.focus = Focus::Details;
    app.work_items.details.offset = usize::MAX;
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let metrics = app
        .shell
        .hit_regions
        .scroll(ScrollSurface::Details)
        .expect("an overflowing details pane registers its scrollbar");
    assert_eq!(metrics.offset, metrics.max_offset(), "scrolled to the end");
    let track = metrics.track;
    let pane = details_pane(&app);
    assert_eq!(
        (track.y, track.height),
        (pane.y, pane.height),
        "the track spans everything inside the border, heading included"
    );
    let thumb = metrics.thumb().expect("the description overflows the pane");
    assert_eq!(
        painted_thumb(&terminal, track),
        Some((track.y + thumb.y, thumb.height)),
        "the painted thumb is the draggable thumb"
    );
    assert_eq!(
        track.y + thumb.y + thumb.height,
        track.y + track.height,
        "a tall viewport still lands the thumb on the last row"
    );
}

#[test]
fn long_content_is_bounded_and_the_wheel_scrolls_without_moving_the_selection() {
    let mut long_ticket = ticket();
    long_ticket.description = "A long wrapped detail line. ".repeat(30);
    let mut app = App::new(vec![long_ticket]);
    app.shell.narrow_details = true;
    app.shell.focus = Focus::Details;

    let text = render_text(60, 20, &mut app);

    assert!(app.work_items.details.max_offset() > 0);
    assert!(text.contains('┃'));
    app.work_items.details.offset = usize::MAX;
    render_text(60, 20, &mut app);
    assert_eq!(
        app.work_items.details.offset,
        app.work_items.details.max_offset()
    );

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
    let selected = app.work_items.selected_row();
    let body = table_body(&app);
    let column = body.x + body.width / 2;
    let row = body.y + 1;
    app.handle_mouse(mouse(MouseEventKind::Moved, column, row));
    assert_eq!(
        app.shell.hovered(),
        Some(&PointerTarget::TableRow { index: 1 })
    );

    app.handle_mouse(mouse(MouseEventKind::ScrollDown, column, row));
    assert_eq!(
        app.work_items.selected_row(),
        selected,
        "the wheel selects nothing"
    );
    assert_eq!(app.shell.focus, Focus::Tickets);
    assert!(app.work_items.table.offset > 0);
    let mut terminal = Terminal::new(TestBackend::new(60, 15)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    assert_eq!(
        app.shell.hovered(),
        Some(&PointerTarget::TableRow {
            index: app.work_items.table.offset + 1,
        })
    );
    assert_row_hovered(
        &terminal,
        column,
        row,
        "the ticket under the stationary pointer should remain highlighted",
    );
}

#[test]
fn dragging_a_link_copies_text_while_a_plain_click_opens_it() {
    let mut app = App::new(vec![ticket()]);
    render_text(130, 30, &mut app);
    let url = detail_url(&app).expect("detail url");
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

    let body = table_body(&app);
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
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::OpenPalette))
        .map(|region| (region.rect.x, region.rect.y))
        .expect("Actions button");
    click(&mut app, x, y);
    assert_eq!(app.work_items.mode, WorkItemMode::Palette);

    render_text(110, 24, &mut app);
    let (x, y) = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::CloseOverlay))
        .map(|region| (region.rect.x, region.rect.y))
        .expect("palette close");
    click(&mut app, x, y);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);

    render_text(110, 24, &mut app);
    let (x, y) = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::OpenHelp))
        .map(|region| (region.rect.x, region.rect.y))
        .expect("help button");
    click(&mut app, x, y);
    assert_eq!(app.work_items.mode, WorkItemMode::Help);

    app.work_items.mode = WorkItemMode::Browse;
    render_text(110, 24, &mut app);
    let (x, y) = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::ToggleRowSelect { index: 0 }))
        .map(|region| (region.rect.x, region.rect.y))
        .expect("row checkbox");
    click(&mut app, x, y);
    assert!(
        app.work_items
            .is_row_selected(&app.work_items.selected_ticket().unwrap().key)
    );
}

fn divider(app: &App) -> Rect {
    app.shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::PaneDivider { .. }))
        .expect("pane divider")
        .rect
}

#[test]
fn dragging_the_divider_resizes_both_layouts_and_keeps_both_panes_usable() {
    let mut app = App::new(vec![ticket()]);
    let screen = render_text(130, 30, &mut app);
    let before = divider(&app);
    assert_eq!(before.width, 1, "the panes share one border column");
    assert_eq!(app.shell.pane_split_wide, 62);
    assert_eq!(
        screen
            .lines()
            .nth(usize::from(before.y + 1))
            .and_then(|row| row.chars().nth(usize::from(before.x))),
        Some('│'),
        "the seam between the panes is the border they share"
    );
    assert!(
        app.shell
            .hit_regions
            .resolve_scroll(before.x, before.y + 1)
            .is_none(),
        "the wheel over the divider scrolls nothing"
    );

    app.handle_mouse(mouse(MouseEventKind::Moved, before.x, before.y + 1));
    assert_eq!(
        app.shell.hovered(),
        Some(&PointerTarget::PaneDivider {
            split: crate::pointer::PaneSplit::Workspace
        })
    );
    let mut terminal = Terminal::new(TestBackend::new(130, 30)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    assert!(
        terminal.backend().buffer()[(before.x, before.y + 1)]
            .modifier
            .contains(Modifier::REVERSED),
        "the hovered divider is painted reversed"
    );

    app.shell.session_dirty = false;
    let action = drag(
        &mut app,
        (before.x, before.y + 2),
        (before.x + 15, before.y + 2),
    );
    assert!(matches!(action, crate::app::AppAction::None));
    assert!(
        app.shell.selection().is_none(),
        "the divider selects no text"
    );
    assert!(app.shell.pane_split_wide > 62);
    assert!(
        app.shell.session_dirty,
        "a finished drag is worth persisting"
    );
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
    let content = app
        .shell
        .seam(crate::pointer::PaneSplit::Workspace)
        .expect("a seam knows the workspace it divides")
        .workspace;
    // The panes share the divider column, so the tickets pane runs from the
    // content's left edge to the far side of the seam.
    assert!(
        leftmost.right() - content.x >= 40,
        "tickets pane kept {} columns",
        leftmost.right() - content.x
    );
    drag(&mut app, (leftmost.x, leftmost.y), (129, leftmost.y));
    render_text(130, 30, &mut app);
    let rightmost = divider(&app);
    assert!(
        rightmost.x > leftmost.x,
        "dragging right still moves the divider"
    );
    assert!(
        content.right() - rightmost.x >= 30,
        "details pane kept {} columns",
        content.right() - rightmost.x
    );

    let mut stacked = App::new(vec![ticket()]);
    render_text(90, 30, &mut stacked);
    let before = divider(&stacked);
    assert_eq!(before.height, 1, "and one border row when they are stacked");
    assert_eq!(stacked.shell.pane_split_stacked, 56);
    let action = drag(
        &mut stacked,
        (before.x + 5, before.y),
        (before.x + 5, before.y + 3),
    );
    assert!(matches!(action, crate::app::AppAction::None));
    assert!(stacked.shell.pane_split_stacked > 56);
    render_text(90, 30, &mut stacked);
    assert!(
        divider(&stacked).y > before.y,
        "the stacked divider moved down"
    );
}

#[test]
fn the_search_is_one_row_with_a_prompt_that_says_whether_it_has_the_keyboard() {
    let mut app = App::new(vec![ticket()]);
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let idle = target_rect(&app, |target| matches!(target, PointerTarget::SearchField));
    assert_eq!(idle.height, 1, "one row, not a box three rows tall");
    assert_eq!(idle.y, 1, "directly under the tab bar");
    let buffer = terminal.backend().buffer();
    assert_eq!(
        buffer[(idle.x - 2, idle.y)].symbol(),
        "/",
        "the prompt glyph"
    );
    assert_eq!(
        buffer[(idle.x, idle.y)].fg,
        theme().muted,
        "the placeholder is muted"
    );

    app.work_items
        .run_command(&mut app.shell, crate::command::CommandId::Search);
    app.work_items
        .set_query(&mut app.shell, "state:doing".into());
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let active = target_rect(&app, |target| matches!(target, PointerTarget::SearchField));
    let buffer = terminal.backend().buffer();
    assert_eq!(
        buffer[(active.x - 2, active.y)].symbol(),
        "\u{203a}",
        "the glyph says the row has the keyboard"
    );
    if theme() == Theme::mono() {
        assert!(
            buffer[(active.x, active.y)]
                .modifier
                .contains(Modifier::REVERSED),
            "with no palette to tint the ground, the row reverses"
        );
    } else {
        assert_eq!(
            buffer[(active.x, active.y)].bg,
            theme().surface,
            "the row takes the surface ground while it is active"
        );
    }
    assert!(
        app.shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::ClearQuery))
            .is_some_and(|region| region.rect.y == active.y),
        "the clear button sits on the same row as the query"
    );
}

#[test]
fn the_tab_bar_carries_the_two_controls_that_open_over_every_tab() {
    let mut app = App::new(vec![ticket()]);
    render_text(120, 20, &mut app);
    let palette = target_rect(&app, |target| matches!(target, PointerTarget::OpenPalette));
    let help = target_rect(&app, |target| matches!(target, PointerTarget::OpenHelp));
    assert_eq!((palette.y, help.y), (0, 0), "both are on the tab bar");
    assert!(palette.right() <= help.x, "Actions then ?");

    app.select_tab(crate::app::TabId::Repos);
    render_text(120, 20, &mut app);
    let help = target_rect(&app, |target| matches!(target, PointerTarget::OpenHelp));
    let action = click(&mut app, help.x, help.y);
    assert_eq!(action, crate::app::AppAction::None);
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Help,
        "the help opens over another tab too"
    );
}

/// The capture row is one row where the tab already keeps its search row, on
/// whatever tab the thought arrived on.
#[test]
fn the_capture_row_takes_the_search_rows_place_on_every_tab() {
    let row = |text: &str| {
        text.lines()
            .nth(1)
            .expect("the row under the tab bar")
            .trim_end()
            .to_owned()
    };

    let mut app = App::new(vec![ticket()]);
    app.handle_key(KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE));
    let captured = render_text(120, 20, &mut app);
    assert!(
        row(&captured).starts_with("+ Title"),
        "the glyph and what the row wants:\n{captured}"
    );
    app.handle_paste("swallowed 412");
    let typed = render_text(120, 20, &mut app);
    assert!(
        row(&typed).starts_with("+ swallowed 412"),
        "and the title as it is typed:\n{typed}"
    );
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        row(&render_text(120, 20, &mut app)).starts_with('/'),
        "Esc gives the search row its place back"
    );

    let mut app = crate::app::pipelines::tests::pipelines_app();
    app.handle_key(KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE));
    let over_runs = render_text(120, 20, &mut app);
    assert!(
        row(&over_runs).starts_with("+ Title"),
        "and it draws over the runs the same way:\n{over_runs}"
    );
}
