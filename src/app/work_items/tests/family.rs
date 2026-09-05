use super::*;

fn family_app() -> App {
    let mut parent = ticket(1, "Parent", "2026-01-01T00:00:00Z");
    parent.work_item_type = "Feature".into();
    let mut child = ticket(2, "Child", "2026-02-01T00:00:00Z");
    child.work_item_type = "Task".into();
    let grandchild = ticket(3, "Grandchild", "2026-01-15T00:00:00Z");
    let mut app = App::new(vec![parent, child, grandchild]);
    app.work_items.set_workspace_graph(
        &mut app.shell,
        TicketGraph {
            relations: vec![
                RelationRecord {
                    from: family_key(2),
                    to: family_key(1),
                    kind: crate::model::RelationKind::Parent,
                },
                RelationRecord {
                    from: family_key(3),
                    to: family_key(2),
                    kind: crate::model::RelationKind::Parent,
                },
            ],
            ..TicketGraph::default()
        },
    );
    app
}

#[test]
fn child_progress_counts_direct_children_and_closes_on_completed_or_removed() {
    let mut app = App::new(epic_tickets());
    app.work_items
        .set_workspace_graph(&mut app.shell, epic_graph());

    let epic = app
        .work_items
        .child_progress(&family_key(1))
        .expect("an epic with children has a ratio");
    assert_eq!(
        (epic.done, epic.total),
        (2, 3),
        "Closed and Removed both count as done, and the grandchild is not the epic's own child"
    );
    assert!(!epic.is_complete());
    let issue = app
        .work_items
        .child_progress(&family_key(4))
        .expect("the issue has a task under it");
    assert_eq!((issue.done, issue.total), (0, 1));
    assert_eq!(
        app.work_items.child_progress(&family_key(5)),
        None,
        "a work item nobody broke down has no ratio at all, not 0/0"
    );
}

#[test]
fn an_epic_reads_as_complete_once_its_last_child_closes() {
    let mut app = App::new(epic_tickets());
    app.work_items
        .set_workspace_graph(&mut app.shell, epic_graph());
    assert!(
        !app.work_items
            .child_progress(&family_key(1))
            .unwrap()
            .is_complete()
    );

    let mut closing = epic_tickets();
    closing[3].state = "Closed".into();
    app.work_items
        .replace_prepared_tickets(&mut app.shell, Snapshot::with_graph(closing, epic_graph()));

    let epic = app.work_items.child_progress(&family_key(1)).unwrap();
    assert_eq!((epic.done, epic.total), (3, 3));
    assert!(
        epic.is_complete(),
        "the last issue closing finishes the epic without anything recounting it by hand"
    );
}

#[test]
fn sorting_by_progress_runs_least_finished_first_and_leaves_childless_rows_last() {
    let mut app = App::new(epic_tickets());
    app.work_items
        .set_workspace_graph(&mut app.shell, epic_graph());
    // The ordering is the subject, so every row stays on the table.
    app.work_items.set_show_finished(&mut app.shell, true);

    app.work_items.set_sort(
        &mut app.shell,
        SortField::Progress,
        SortDirection::Ascending,
    );

    let order: Vec<i64> = app
        .work_items
        .visible_tickets()
        .map(|ticket| ticket.key.id)
        .collect();
    assert_eq!(
        order,
        vec![4, 1, 2, 3, 5],
        "0/1 before 2/3, then the work items with no children in id order"
    );
}

#[test]
fn a_bar_fills_only_on_a_whole_ratio_and_never_reads_empty_once_work_has_landed() {
    let bar = |done, total| ChildProgress { done, total }.filled_cells(PROGRESS_BAR_CELLS);

    assert_eq!(bar(0, 7), 0);
    assert_eq!(bar(1, 40), 1, "a single closed child still shows one cell");
    assert_eq!(bar(3, 7), 2);
    assert_eq!(bar(39, 40), 5, "an unfinished parent never fills the bar");
    assert_eq!(bar(7, 7), PROGRESS_BAR_CELLS);
}

#[test]
fn pane_keys_move_focus_and_only_the_details_pane_opens_on_enter() {
    let mut app = family_app();
    assert_eq!(app.shell.focus, Focus::Tickets);

    press(&mut app, KeyCode::Tab);
    assert_eq!(app.shell.focus, Focus::Details);
    assert!(
        app.shell.narrow_details,
        "the narrow layout follows the focus"
    );

    press(&mut app, KeyCode::Char('d'));
    assert_eq!(app.shell.focus, Focus::Tickets);
    assert!(!app.shell.narrow_details);

    app.shell.focus = Focus::Family;
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.shell.focus, Focus::Details);

    app.shell.focus = Focus::Tickets;
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        AppAction::None,
        "Enter must not open a browser from the tickets pane"
    );
    assert!(matches!(
        press(&mut app, KeyCode::Char('o')),
        AppAction::OpenUrl(_)
    ));
    app.shell.focus = Focus::Details;
    assert!(matches!(
        press(&mut app, KeyCode::Enter),
        AppAction::OpenUrl(_)
    ));
}

#[test]
fn family_cursor_movement_clamps_and_scrolls_the_details_viewport() {
    let mut app = family_app();
    app.shell.focus = Focus::Family;
    app.work_items.details.set_viewport(2, 20);

    press(&mut app, KeyCode::Home);
    press(&mut app, KeyCode::Up);
    assert_eq!(
        app.work_items.family_cursor.as_ref().map(|key| key.id),
        Some(1)
    );
    assert_eq!(app.work_items.details.offset, 0);

    press(&mut app, KeyCode::End);
    press(&mut app, KeyCode::Down);
    assert_eq!(
        app.work_items.family_cursor.as_ref().map(|key| key.id),
        Some(3)
    );
    assert!(
        app.work_items.details.offset > 0,
        "the details pane scrolls to keep the cursor visible"
    );
}

#[test]
fn family_enter_selects_visible_tickets_records_history_once_and_explains_hidden_ones() {
    let mut app = family_app();
    assert_eq!(app.work_items.selected_ticket().unwrap().key.id, 2);
    app.shell.focus = Focus::Family;

    let opened = press(&mut app, KeyCode::Char('o'));
    assert!(matches!(opened, AppAction::OpenUrl(_)));
    assert_eq!(app.work_items.selected_ticket().unwrap().key.id, 2);

    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.work_items.selected_ticket().unwrap().key.id, 3);
    assert_eq!(app.shell.focus, Focus::Family);
    assert_eq!(
        app.shell
            .history()
            .iter()
            .map(|jump| match jump {
                Jump::WorkItem(key) => key.id,
                other => panic!("only work items so far: {other:?}"),
            })
            .collect::<Vec<_>>(),
        vec![2, 3]
    );

    press(&mut app, KeyCode::Char('['));
    assert_eq!(app.work_items.selected_ticket().unwrap().key.id, 2);

    app.work_items
        .visible
        .retain(|entry| app.work_items.tickets[entry.ticket_index].key.id != 3);
    app.work_items.family_cursor = Some(family_key(3));
    let query = app.work_items.query().to_owned();
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.work_items.selected_ticket().unwrap().key.id, 2);
    assert_eq!(
        app.work_items.query(),
        query,
        "a hidden target changes no search"
    );
    assert_eq!(
        app.shell.notification(),
        Some((
            "3 is hidden by the current search — Esc to go back",
            NotificationLevel::Info
        ))
    );
    assert!(
        app.work_items.peeking(),
        "the details pane follows it anyway"
    );
    assert_eq!(
        app.work_items.detail_ticket().map(|ticket| ticket.key.id),
        Some(3),
        "and shows the hidden relative rather than the selected row"
    );

    press(&mut app, KeyCode::Esc);
    assert!(!app.work_items.peeking(), "Esc puts the pane back");
    assert_eq!(
        app.work_items.detail_ticket().map(|ticket| ticket.key.id),
        Some(2)
    );
    assert_eq!(
        app.work_items.query(),
        query,
        "and leaves the search it never touched"
    );
}

#[test]
fn a_background_sync_leaves_the_search_box_and_the_selection_alone() {
    let mut app = App::new(vec![
        ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Beta", "2026-02-01T00:00:00Z"),
    ]);
    press(&mut app, KeyCode::Char('/'));
    for character in "alp".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    await_search(&mut app);
    let selected = app.work_items.selected_ticket().unwrap().key.clone();

    // The sync worker's rows land while the user is still typing.
    let mut refreshed = app.work_items.tickets().to_vec();
    refreshed.push(ticket(3, "Gamma", "2026-03-01T00:00:00Z"));
    app.work_items
        .replace_prepared_tickets(&mut app.shell, Snapshot::new(refreshed));
    await_search(&mut app);

    assert_eq!(app.work_items.mode, WorkItemMode::Search);
    assert_eq!(app.work_items.query(), "alp");
    assert_eq!(app.work_items.tickets().len(), 3);
    assert_eq!(app.work_items.selected_ticket().unwrap().key, selected);

    press(&mut app, KeyCode::Char('h'));
    assert_eq!(
        app.work_items.query(),
        "alph",
        "the caret stayed where it was"
    );
}

#[test]
fn family_selection_and_cursor_restore_after_reload() {
    let mut app = family_app();
    app.shell.focus = Focus::Family;
    press(&mut app, KeyCode::Down);
    assert_eq!(
        app.work_items.family_cursor.as_ref().map(|key| key.id),
        Some(3)
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.work_items.selected_ticket().unwrap().key.id, 3);

    let graph = app.work_items.graph.clone();
    let tickets = app.work_items.tickets().to_vec();
    app.work_items
        .replace_prepared_tickets(&mut app.shell, Snapshot::with_graph(tickets, graph));

    assert_eq!(app.work_items.selected_ticket().unwrap().key.id, 3);
    assert_eq!(
        app.work_items.family_cursor.as_ref().map(|key| key.id),
        Some(3)
    );
    assert_eq!(
        app.work_items
            .visible_family_tree()
            .iter()
            .map(|entry| entry.key.id)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

/// An Epic over fifteen issues, with twenty-five tasks under the one the table
/// starts on — enough of both to run past the sibling window and the child cap.
fn crowded_family_app() -> App {
    let mut tickets = vec![ticket(1, "Epic", "2026-01-01T00:00:00Z")];
    tickets.extend((10..25).map(|id| ticket(id, "Issue", "2026-01-02T00:00:00Z")));
    tickets.extend((100..125).map(|id| ticket(id, "Task", "2026-01-01T00:00:00Z")));
    // The most recently changed row is the one the table selects.
    tickets[8].changed_at = crate::timestamp::ts("2026-03-01T00:00:00Z");
    let mut app = App::new(tickets);
    app.work_items.set_workspace_graph(
        &mut app.shell,
        TicketGraph {
            relations: (10..25)
                .map(|id| child_of(id, 1))
                .chain((100..125).map(|id| child_of(id, 17)))
                .collect(),
            ..TicketGraph::default()
        },
    );
    app
}

fn cursor_id(app: &App) -> Option<i64> {
    app.work_items.family_cursor.as_ref().map(|key| key.id)
}

#[test]
fn the_family_cursor_runs_the_windowed_tree_and_stops_on_its_rows_only() {
    let mut app = crowded_family_app();
    assert_eq!(app.work_items.selected_ticket().unwrap().key.id, 17);
    app.shell.focus = Focus::Family;

    let tree = app.work_items.visible_family_tree();
    assert_eq!(
        tree.iter().map(|entry| entry.key.id).collect::<Vec<_>>(),
        [1, 14, 15, 16, 17]
            .into_iter()
            .chain(100..120)
            .chain([18, 19, 20])
            .collect::<Vec<_>>(),
        "the chain above, three siblings either side, twenty children under"
    );
    assert_eq!((tree.more_siblings, tree.more_children), (8, 5));

    press(&mut app, KeyCode::Home);
    assert_eq!(cursor_id(&app), Some(1), "the top of the chain");
    press(&mut app, KeyCode::End);
    assert_eq!(
        cursor_id(&app),
        Some(20),
        "the last sibling drawn, never one the window cut"
    );
    press(&mut app, KeyCode::Up);
    assert_eq!(cursor_id(&app), Some(19));

    app.work_items.details.set_viewport(6, 200);
    press(&mut app, KeyCode::Home);
    press(&mut app, KeyCode::PageDown);
    assert_eq!(
        cursor_id(&app),
        Some(101),
        "a page is six rows of the tree, children included"
    );
}

#[test]
fn the_family_cursor_scrolls_to_the_row_the_summary_line_pushed_down() {
    let mut app = crowded_family_app();
    app.shell.focus = Focus::Family;
    let last = app.work_items.visible_family_tree().len() - 1;
    app.work_items.details.set_viewport(5, 200);
    // What the renderer records after drawing a `… 5 more children` line
    // between the last child and the sibling under it.
    app.work_items.family_rows = (0..=last)
        .map(|index| if index > 24 { index + 1 } else { index })
        .collect();

    press(&mut app, KeyCode::End);
    assert_eq!(cursor_id(&app), Some(20));
    assert_eq!(
        app.work_items.details.offset,
        last + 1 + 1 - 5,
        "the cursor scrolls to the row it was drawn on, not to its place in \
         the tree"
    );
}
