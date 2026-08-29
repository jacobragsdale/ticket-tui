use super::*;

#[test]
fn search_order_switches_between_relevance_and_field_sorting_and_keeps_the_selection() {
    let mut app = App::new(vec![
        ticket(1, "Search alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Search beta", "2026-02-01T00:00:00Z"),
    ]);
    app.select_row(1);
    let selected = app.selected_ticket().unwrap().key.clone();
    assert_eq!(selected.id, 1, "the newest ticket leads by default");

    app.set_query("search".into());
    await_search(&mut app);
    app.set_sort(SortField::Title, SortDirection::Ascending);
    assert_eq!(app.selected_ticket().unwrap().key, selected);

    app.visible = vec![
        SearchMatch {
            ticket_index: 1,
            score: 100,
        },
        SearchMatch {
            ticket_index: 0,
            score: 1,
        },
    ];
    app.sort_visible();
    assert_eq!(app.search_order, SearchOrder::Relevance);
    assert_eq!(
        app.visible_tickets().next().unwrap().key.id,
        2,
        "relevance leads with the best scoring match"
    );

    app.toggle_search_order();

    assert_eq!(app.search_order, SearchOrder::Field);
    assert_eq!(
        app.visible_tickets().next().unwrap().key.id,
        1,
        "field order falls back to the sort column"
    );

    let mut without_fuzzy = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
    let order = without_fuzzy.search_order;
    without_fuzzy.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    assert_eq!(
        without_fuzzy.search_order, order,
        "there is nothing to re-rank without a fuzzy query"
    );
}

#[test]
fn pasting_fills_the_search_editor_and_escape_clears_the_query() {
    let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
    app.mode = AppMode::Search;
    app.handle_paste("search\n");
    assert_eq!(app.query(), "search ");
    assert_eq!(app.query_cursor(), 7);
    app.mode = AppMode::Browse;

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(app.query().is_empty());
    assert_eq!(app.visible_count(), 1);
}

#[test]
fn reload_during_search_does_not_keep_stale_indices() {
    let mut app = App::new(vec![
        ticket(1, "Search alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Search beta", "2026-02-01T00:00:00Z"),
    ]);
    app.set_query("search".into());
    await_search(&mut app);

    app.replace_tickets(vec![ticket(1, "Search alpha", "2026-01-01T00:00:00Z")]);

    assert_eq!(app.visible_count(), 0);
    await_search(&mut app);
    assert_eq!(app.visible_count(), 1);
    assert_eq!(app.selected_ticket().unwrap().key.id, 1);
}

#[test]
fn sorting_and_reload_keep_the_view_context_unless_the_selection_is_gone() {
    let original = vec![
        ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Beta", "2026-02-01T00:00:00Z"),
        ticket(3, "Gamma", "2026-03-01T00:00:00Z"),
    ];
    let mut app = App::new(original.clone());
    assert_eq!(
        app.visible_tickets().next().unwrap().key.id,
        3,
        "tickets start sorted by most recently changed"
    );
    app.select_row(1);
    let selected = app.selected_ticket().unwrap().key.clone();
    app.details.set_viewport(0, 5);
    app.details.scroll_to(3);
    app.table.offset = 1;
    app.table.viewport = 2;

    app.set_sort(SortField::Title, SortDirection::Descending);
    assert_eq!(app.selected_ticket().unwrap().key, selected);
    assert_eq!(app.details.offset, 3);
    assert_eq!(app.table.offset, 1);

    app.replace_tickets(original);
    assert_eq!(app.selected_ticket().unwrap().key, selected);
    assert_eq!(app.details.offset, 3);
    assert_eq!(app.table.offset, 1);

    app.replace_tickets(vec![ticket(9, "Delta", "2026-03-01T00:00:00Z")]);
    assert_eq!(app.selected_ticket().unwrap().key.id, 9);
    assert_eq!(app.details.offset, 0, "a lost selection resets the details");
    assert_eq!(app.table.offset, 0, "a lost selection resets the table");
}

#[test]
fn structured_query_filters_tickets_and_keeps_fuzzy_search() {
    let mut app = App::new(vec![
        ticket(1, "Search alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Other beta", "2026-02-01T00:00:00Z"),
    ]);
    app.set_query("state:active search".into());
    await_search(&mut app);

    assert_eq!(app.visible_count(), 1);
    assert_eq!(app.visible_tickets().next().unwrap().key.id, 1);
    assert_eq!(app.fuzzy_query(), "search");
    assert_eq!(app.filter_tokens().len(), 1);
}

#[test]
fn a_facet_toggle_rewrites_the_query_and_removing_the_chip_clears_it() {
    let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
    app.open_filters();
    app.filter_overlay.showing_values = true;
    app.filter_overlay.field_index = 0;
    app.toggle_current_facet();

    assert!(app.query().contains("state:"));
    let token = app.filter_tokens().pop().unwrap();
    app.remove_filter_token(token);
    assert!(app.query().is_empty());
}

#[test]
fn named_views_restore_query_and_sort() {
    let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
    app.set_query("state:active".into());
    app.set_sort(SortField::Title, SortDirection::Ascending);
    app.save_view("Active".into());
    app.set_query(String::new());
    app.set_sort(SortField::Changed, SortDirection::Descending);

    app.apply_view_at(view_row(&app, "Active"));

    assert_eq!(app.query(), "state:active");
    assert_eq!(app.sort_field, SortField::Title);
    assert_eq!(app.active_view.as_deref(), Some("Active"));
}

#[test]
fn command_palette_runs_density_toggle() {
    let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
    app.open_palette();
    app.palette.query = TextInput::new("density");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.row_density, RowDensity::Comfortable);
    assert_eq!(app.mode, AppMode::Browse);
}

#[test]
fn every_bound_key_runs_its_command_from_browse_mode() {
    for command in crate::command::COMMANDS
        .iter()
        .filter(|command| !command.keys.is_empty())
    {
        for key in command.keys {
            let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
            app.handle_key(KeyEvent::new(key.code, key.modifiers));
            let expected = match command.id {
                CommandId::Sort => Some(AppMode::Sort),
                CommandId::Help => Some(AppMode::Help),
                CommandId::Views => Some(AppMode::Views),
                CommandId::Columns => Some(AppMode::Columns),
                CommandId::Palette => Some(AppMode::Palette),
                CommandId::DatabaseInfo => Some(AppMode::Info),
                CommandId::Search => Some(AppMode::Search),
                CommandId::Filters => Some(AppMode::Facets),
                CommandId::MoreFilters => Some(AppMode::Filter),
                CommandId::EditMenu => Some(AppMode::Edit),
                CommandId::ChangeState => Some(AppMode::StatePicker),
                _ => None,
            };
            if let Some(mode) = expected {
                assert_eq!(app.mode, mode, "{:?} via {}", command.id, key.label());
            }
        }
    }
}
