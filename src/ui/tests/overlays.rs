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
        .facet_pill(FacetTarget::Field(FilterField::Type))
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
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::FilterRow { index: 2 }))
        .map(|region| (region.rect.x, region.rect.y))
        .expect("scrolled row 2 should be the first visible hit");
    click(&mut app, x, y);
    assert!(app.filter_overlay.showing_values);
    assert_eq!(app.filter_overlay.field_index, 2);
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
        text.contains("Tickets 1/2"),
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
    assert!(text.contains("Tickets 2/2"), "{text}");
}

#[test]
fn the_views_overlay_paints_the_built_ins_under_their_heading_above_the_saved_ones() {
    let mut app = App::new(vec![ticket()]);
    app.set_query("tag:rust".into());
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
    assert_eq!(app.mode, AppMode::Sprint, "the palette has no key for it");

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

    assert_eq!(app.mode, AppMode::Browse);
    assert_eq!(
        app.query(),
        "assignee:\"Avery Chen\" iteration:\"Atlas\\\\Sprint 1\""
    );

    let mut empty = App::new(vec![]);
    empty.mode = AppMode::Sprint;
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
