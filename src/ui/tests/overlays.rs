use super::*;

#[test]
fn facet_pills_open_their_menu_and_the_filter_overlay_maps_scrolled_clicks() {
    let mut app = App::new(vec![ticket()]);
    let text = render_text(110, 24, &mut app);
    assert!(text.contains("State"));
    assert!(text.contains("Type"));
    assert!(text.contains("▾"));

    let pill = app
        .shell
        .hit_regions
        .facet_pill(FacetTarget::Field(FilterField::Type.key()))
        .expect("type pill should be clickable");
    click(&mut app, pill.x, pill.y);
    assert_eq!(app.work_items.mode, WorkItemMode::Facets);
    assert_eq!(
        FilterField::BAR
            .get(app.work_items.facet_bar.field_index)
            .copied(),
        Some(FilterField::Type)
    );

    app.work_items.mode = WorkItemMode::Filter;
    app.work_items.filter_overlay.scroll.offset = 2;
    let overlay = render_text(110, 24, &mut app);
    assert!(overlay.contains("Filters"));
    let (x, y) = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::FilterRow { index: 2 }))
        .map(|region| (region.rect.x, region.rect.y))
        .expect("scrolled row 2 should be the first visible hit");
    click(&mut app, x, y);
    assert!(app.work_items.filter_overlay.showing_values);
    assert_eq!(app.work_items.filter_overlay.field_index, 2);
}

#[test]
fn the_chip_bar_says_finished_work_is_hidden_and_its_cross_puts_it_back() {
    let mut app = App::new(vec![
        ticket_at(10_001, "Alpha", "Issue", "To Do", "2026-03-03T00:00:00Z"),
        ticket_at(10_002, "Gamma", "Issue", "Done", "2026-03-01T00:00:00Z"),
    ]);

    let text = render_text(130, 24, &mut app);
    assert!(text.contains("Finished hidden ×"), "{text}");
    assert!(
        pane_reads(&text, "Tickets", "1/2"),
        "the total stays the database's, so the count hidden is the difference: {text}"
    );

    let chip = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::ShowFinished))
        .map(|region| region.rect)
        .expect("the chip is clickable");
    click(&mut app, chip.x, chip.y);

    let text = render_text(130, 24, &mut app);
    assert!(!text.contains("Finished hidden"), "{text}");
    assert!(pane_reads(&text, "Tickets", "2/2"), "{text}");
}

#[test]
fn the_views_overlay_paints_the_built_ins_under_their_heading_above_the_saved_ones() {
    let mut app = App::new(vec![ticket()]);
    app.work_items.set_query(&mut app.shell, "tag:rust".into());
    app.handle_key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT));
    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    for character in "Rust work".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let screen = render_text(80, 26, &mut app);
    let at = |needle: &str| {
        screen
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} should be on screen:\n{screen}"))
    };
    let listed = [
        "Built-in",
        "assignee:@me",
        "assignee:@none",
        "state:doing",
        "changed:>14d state:@open",
        "iteration:@current",
        "Saved",
        "Rust work",
    ];

    for pair in listed.windows(2) {
        assert!(
            at(pair[0]) < at(pair[1]),
            "{} should be listed above {}:\n{screen}",
            pair[0],
            pair[1]
        );
    }
    assert!(
        screen.contains("\u{203a}  Mine"),
        "the cursor opens on the first built-in rather than on its heading:\n{screen}"
    );
}

#[test]
fn the_sprint_summary_draws_its_grid_and_a_clicked_row_filters_the_table() {
    let mut app = App::new(vec![
        ticket_at(1, "Search", "Bug", "To Do", "2026-08-28T00:00:00Z"),
        ticket_at(2, "Cache", "Task", "Done", "2026-08-27T00:00:00Z"),
    ]);
    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
    for character in "sprint summary".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Sprint,
        "the palette has no key for it"
    );

    let screen = render_text(100, 30, &mut app);
    for needle in [
        "Sprint summary \u{b7} Sprint 1",
        "Assignee",
        "To Do",
        "Doing",
        "Done",
        "Total",
        "Avery Chen",
        "By type",
        "2 items \u{b7} 1 done (50%)",
    ] {
        assert!(
            screen.contains(needle),
            "{needle} should be on screen:\n{screen}"
        );
    }
    assert!(
        screen.contains("\u{203a} Avery Chen"),
        "the cursor opens on the first person rather than on the headings:\n{screen}"
    );

    let (x, y) = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::SummaryRow { index: 1 }))
        .map(|region| (region.rect.x, region.rect.y))
        .expect("each grid row is clickable");
    click(&mut app, x, y);

    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert_eq!(
        app.work_items.query(),
        "assignee:\"Avery Chen\" iteration:\"Atlas\\\\Sprint 1\""
    );

    let mut empty = App::new(vec![]);
    empty.work_items.mode = WorkItemMode::Sprint;
    let screen = render_text(100, 30, &mut empty);
    assert!(
        screen.contains("No sprint to summarise."),
        "an overlay with no sprint to count explains itself:\n{screen}"
    );
    assert!(
        !screen.contains("Assignee      To Do"),
        "and paints no grid at all:\n{screen}"
    );
}

/// Every overlay is painted into the frame that already holds the tickets and
/// details panes, so one that does not clear its own area first leaves them
/// showing through it. The pane borders are the tell: they run down fixed
/// columns of every content row, and land inside a modal that forgot.
#[test]
fn every_overlay_paints_over_the_panes_behind_it() {
    let mut app = App::new(vec![
        ticket_at(10_001, "Alpha", "Issue", "Active", "2026-03-03T00:00:00Z"),
        ticket_at(10_002, "Beta", "Bug", "Active", "2026-03-02T00:00:00Z"),
        ticket_at(10_003, "Gamma", "Task", "Active", "2026-03-01T00:00:00Z"),
    ]);
    assert!(
        render_text(130, 30, &mut app).contains('│'),
        "the panes behind the overlays have to draw borders for one to bleed through"
    );

    // Three overlays paint nothing until they have something to work on, so
    // they are opened the way a user opens them and left standing: setting the
    // mode below is then enough to bring each one back up.
    app.shell.enable_sync();
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.work_items.mode, WorkItemMode::Prompt);
    app.work_items
        .run_command(&mut app.shell, CommandId::NewWorkItem);
    assert_eq!(app.work_items.mode, WorkItemMode::Form);
    app.work_items
        .run_command(&mut app.shell, CommandId::DeleteWorkItem);
    assert_eq!(app.work_items.mode, WorkItemMode::ConfirmDelete);

    for mode in [
        WorkItemMode::Sort,
        WorkItemMode::Help,
        WorkItemMode::Filter,
        WorkItemMode::Columns,
        WorkItemMode::Palette,
        WorkItemMode::Views,
        WorkItemMode::Info,
        WorkItemMode::Sprint,
        WorkItemMode::Facets,
        WorkItemMode::Edit,
        WorkItemMode::StatePicker,
        WorkItemMode::PriorityPicker,
        WorkItemMode::Prompt,
        WorkItemMode::AssigneePicker,
        WorkItemMode::ParentPicker,
        WorkItemMode::NodePicker,
        WorkItemMode::Form,
        WorkItemMode::TypePicker,
        WorkItemMode::ConfirmDelete,
    ] {
        app.work_items.mode = mode;
        let text = render_text(130, 30, &mut app);
        let interior = modal_interior(&text)
            .unwrap_or_else(|| panic!("{mode:?} drew no modal frame:\n{text}"));
        assert!(
            !interior.iter().any(|row| row.contains('│')),
            "{mode:?} let the panes behind it show through:\n{text}"
        );
    }
}

#[test]
fn a_modal_washes_out_what_is_behind_it_and_leaves_nothing_behind_when_it_closes() {
    if !theme().dim_behind_modals {
        return;
    }
    let mut app = App::new(vec![ticket()]);
    let sample = |app: &mut App| {
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        let body = table_body(app);
        let cell = &terminal.backend().buffer()[(body.x + 7, body.y)];
        (cell.fg, cell.modifier)
    };
    let (before, _) = sample(&mut app);
    assert_eq!(before, theme().link, "the id cell reads as a link");

    app.work_items.mode = WorkItemMode::Palette;
    let (behind, modifier) = sample(&mut app);
    assert_eq!(
        behind,
        theme().muted,
        "the row behind the modal is washed out"
    );
    assert!(
        !modifier.contains(Modifier::BOLD),
        "and gives up its weight"
    );

    app.work_items.mode = WorkItemMode::Browse;
    assert_eq!(
        sample(&mut app).0,
        before,
        "closing it leaves no paint behind"
    );
}

#[test]
fn the_help_takes_a_share_of_a_big_screen_and_still_fits_a_small_one() {
    let mut app = App::new(vec![ticket()]);
    app.work_items.mode = WorkItemMode::Help;

    render_text(140, 42, &mut app);
    let big = target_rect(&app, |target| matches!(target, PointerTarget::OverlayBody));
    assert!(
        (26..=32).contains(&big.height),
        "about 70% of a 140x42 terminal's height: {big:?}"
    );
    assert!(
        (74..=98).contains(&big.width),
        "and as wide as the help itself, never past 70%: {big:?}"
    );

    render_text(60, 24, &mut app);
    let small = target_rect(&app, |target| matches!(target, PointerTarget::OverlayBody));
    assert!(
        small.width <= 58 && small.height <= 22,
        "and it still fits a small one: {small:?}"
    );
}
