use super::*;

#[test]
fn search_order_switches_between_relevance_and_field_sorting_and_keeps_the_selection() {
    let mut app = App::new(vec![
        ticket(1, "Search alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Search beta", "2026-02-01T00:00:00Z"),
    ]);
    app.work_items.select_row(&mut app.shell, 1);
    let selected = app.work_items.selected_ticket().unwrap().key.clone();
    assert_eq!(selected.id, 1, "the newest ticket leads by default");

    app.work_items.set_query(&mut app.shell, "search".into());
    await_search(&mut app);
    app.work_items
        .set_sort(&mut app.shell, SortField::Title, SortDirection::Ascending);
    assert_eq!(app.work_items.selected_ticket().unwrap().key, selected);

    app.work_items.visible = vec![
        SearchMatch {
            ticket_index: 1,
            score: 100,
        },
        SearchMatch {
            ticket_index: 0,
            score: 1,
        },
    ];
    app.work_items.sort_visible();
    assert_eq!(app.work_items.search_order, SearchOrder::Relevance);
    assert_eq!(
        app.work_items.visible_tickets().next().unwrap().key.id,
        2,
        "relevance leads with the best scoring match"
    );

    app.work_items.toggle_search_order(&mut app.shell);

    assert_eq!(app.work_items.search_order, SearchOrder::Field);
    assert_eq!(
        app.work_items.visible_tickets().next().unwrap().key.id,
        1,
        "field order falls back to the sort column"
    );

    let mut without_fuzzy = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
    let order = without_fuzzy.work_items.search_order;
    without_fuzzy.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    assert_eq!(
        without_fuzzy.work_items.search_order, order,
        "there is nothing to re-rank without a fuzzy query"
    );
}

#[test]
fn pasting_fills_the_search_editor_and_escape_clears_the_query() {
    let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
    app.work_items.mode = WorkItemMode::Search;
    app.handle_paste("search\n");
    assert_eq!(app.work_items.query(), "search ");
    assert_eq!(app.work_items.query_cursor(), 7);
    app.work_items.mode = WorkItemMode::Browse;

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(app.work_items.query().is_empty());
    assert_eq!(app.work_items.visible_count(), 1);
}

#[test]
fn reload_during_search_does_not_keep_stale_indices() {
    let mut app = App::new(vec![
        ticket(1, "Search alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Search beta", "2026-02-01T00:00:00Z"),
    ]);
    app.work_items.set_query(&mut app.shell, "search".into());
    await_search(&mut app);

    app.work_items.replace_tickets(
        &mut app.shell,
        vec![ticket(1, "Search alpha", "2026-01-01T00:00:00Z")],
    );

    assert_eq!(app.work_items.visible_count(), 0);
    await_search(&mut app);
    assert_eq!(app.work_items.visible_count(), 1);
    assert_eq!(app.work_items.selected_ticket().unwrap().key.id, 1);
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
        app.work_items.visible_tickets().next().unwrap().key.id,
        3,
        "tickets start sorted by most recently changed"
    );
    app.work_items.select_row(&mut app.shell, 1);
    let selected = app.work_items.selected_ticket().unwrap().key.clone();
    app.work_items.details.set_viewport(0, 5);
    app.work_items.details.scroll_to(3);
    app.work_items.table.offset = 1;
    app.work_items.table.viewport = 2;

    app.work_items
        .set_sort(&mut app.shell, SortField::Title, SortDirection::Descending);
    assert_eq!(app.work_items.selected_ticket().unwrap().key, selected);
    assert_eq!(app.work_items.details.offset, 3);
    assert_eq!(app.work_items.table.offset, 1);

    app.work_items.replace_tickets(&mut app.shell, original);
    assert_eq!(app.work_items.selected_ticket().unwrap().key, selected);
    assert_eq!(app.work_items.details.offset, 3);
    assert_eq!(app.work_items.table.offset, 1);

    app.work_items.replace_tickets(
        &mut app.shell,
        vec![ticket(9, "Delta", "2026-03-01T00:00:00Z")],
    );
    assert_eq!(app.work_items.selected_ticket().unwrap().key.id, 9);
    assert_eq!(
        app.work_items.details.offset, 0,
        "a lost selection resets the details"
    );
    assert_eq!(
        app.work_items.table.offset, 0,
        "a lost selection resets the table"
    );
}

#[test]
fn structured_query_filters_tickets_and_keeps_fuzzy_search() {
    let mut app = App::new(vec![
        ticket(1, "Search alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Other beta", "2026-02-01T00:00:00Z"),
    ]);
    app.work_items
        .set_query(&mut app.shell, "state:active search".into());
    await_search(&mut app);

    assert_eq!(app.work_items.visible_count(), 1);
    assert_eq!(app.work_items.visible_tickets().next().unwrap().key.id, 1);
    assert_eq!(app.work_items.fuzzy_query(), "search");
    assert_eq!(app.work_items.filter_tokens().len(), 1);
}

#[test]
fn a_facet_toggle_rewrites_the_query_and_removing_the_chip_clears_it() {
    let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
    app.work_items.open_filters();
    app.work_items.filter_overlay.showing_values = true;
    app.work_items.filter_overlay.field_index = 0;
    app.work_items.toggle_current_facet(&mut app.shell);

    assert!(app.work_items.query().contains("state:"));
    let last = app.work_items.filter_tokens().len() - 1;
    app.work_items.remove_filter_token(&mut app.shell, last);
    assert!(app.work_items.query().is_empty());
}

#[test]
fn named_views_restore_query_and_sort() {
    let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
    app.work_items
        .set_query(&mut app.shell, "state:active".into());
    app.work_items
        .set_sort(&mut app.shell, SortField::Title, SortDirection::Ascending);
    app.work_items.save_view(&mut app.shell, "Active".into());
    app.work_items.set_query(&mut app.shell, String::new());
    app.work_items.set_sort(
        &mut app.shell,
        SortField::Changed,
        SortDirection::Descending,
    );

    let row = view_row(&app, "Active");

    app.work_items.apply_view_at(&mut app.shell, row);

    assert_eq!(app.work_items.query(), "state:active");
    assert_eq!(app.work_items.sort_field, SortField::Title);
    assert_eq!(app.work_items.active_view.as_deref(), Some("Active"));
}

#[test]
fn command_palette_runs_density_toggle() {
    let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
    app.work_items.open_palette();
    app.work_items.palette.query = TextInput::new("density");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.work_items.row_density, RowDensity::Comfortable);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
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
                CommandId::Sort => Some(WorkItemMode::Sort),
                CommandId::Help => Some(WorkItemMode::Help),
                CommandId::Views => Some(WorkItemMode::Views),
                CommandId::Columns => Some(WorkItemMode::Columns),
                CommandId::Palette => Some(WorkItemMode::Palette),
                CommandId::DatabaseInfo => Some(WorkItemMode::Info),
                CommandId::Search => Some(WorkItemMode::Search),
                CommandId::Filters => Some(WorkItemMode::Facets),
                CommandId::MoreFilters => Some(WorkItemMode::Filter),
                CommandId::EditMenu => Some(WorkItemMode::Edit),
                CommandId::ChangeState => Some(WorkItemMode::StatePicker),
                _ => None,
            };
            if let Some(mode) = expected {
                assert_eq!(
                    app.work_items.mode,
                    mode,
                    "{:?} via {}",
                    command.id,
                    key.label()
                );
            }
        }
    }
}
