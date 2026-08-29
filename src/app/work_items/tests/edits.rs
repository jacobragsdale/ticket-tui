use super::*;

/// Azure DevOps accepting whatever a request asked for. The optimistic row
/// already carries the field it wrote, so the stored copy is that row on
/// the next revision.
fn accept_edit(app: &mut App, request: &EditRequest) {
    let mut ticket = app
        .work_items
        .ticket_by_key(&request.key)
        .expect("the row is loaded")
        .clone();
    ticket.revision += 1;
    app.work_items.apply_edit(
        &mut app.shell,
        EditApplied {
            ticket,
            relations: Vec::new(),
            edit: request.edit.clone(),
        },
    );
}

#[test]
fn an_edit_shows_at_once_and_the_stored_copy_replaces_it() {
    let mut app = editing_app();
    let request = edit_request(&mut app, FieldEdit::state("Doing"));
    let key = request.key.clone();

    assert_eq!(request.expected_revision, 1, "the row's revision is tested");
    assert_eq!(request.edit.summary(), "State → Doing");
    assert!(app.work_items.edits_pending());
    assert_eq!(
        app.work_items.ticket_by_key(&key).unwrap().state,
        "Doing",
        "the row does not wait for the network"
    );

    app.work_items.set_query(&mut app.shell, "Doing".into());
    await_search(&mut app);
    assert_eq!(
        app.work_items.visible_count(),
        1,
        "the search index follows the optimistic value"
    );
    app.work_items.set_query(&mut app.shell, String::new());
    await_search(&mut app);

    let stored = stored_copy(&app, &key, "Doing");
    app.work_items.apply_edit(
        &mut app.shell,
        EditApplied {
            ticket: stored.clone(),
            relations: Vec::new(),
            edit: FieldEdit::state("Doing"),
        },
    );

    assert!(!app.work_items.edits_pending());
    assert_eq!(
        app.work_items.ticket_by_key(&key),
        Some(&stored),
        "the server wins"
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Updated #3 · State → Doing")
    );
    assert_eq!(
        app.work_items.selected_ticket().map(|ticket| ticket.key.id),
        Some(key.id),
        "the selection stays on the work item it was on"
    );
}

#[test]
fn a_refused_edit_puts_the_row_back_and_names_the_field() {
    let mut app = editing_app();
    let request = edit_request(&mut app, FieldEdit::state("Doing"));
    let before = app.work_items.tickets().to_vec();

    app.work_items.reject_edit(
        &mut app.shell,
        &EditRejection {
            key: request.key.clone(),
            label: "State".into(),
            conflict: true,
            message: "the test operation on /rev failed".into(),
        },
    );

    assert!(!app.work_items.edits_pending());
    assert_eq!(
        app.work_items.ticket_by_key(&request.key).unwrap().state,
        "Active",
        "a refused write leaves nothing of itself behind"
    );
    assert_ne!(before, app.work_items.tickets());
    let (message, level) = app
        .shell
        .notification()
        .expect("a refusal is always reported");
    assert!(message.contains("#3 changed in Azure DevOps"), "{message}");
    assert!(message.contains("State not saved"), "{message}");
    assert_eq!(level, NotificationLevel::Error);
}

#[test]
fn a_pull_that_lands_during_an_edit_keeps_the_optimistic_value() {
    let mut app = editing_app();
    let request = edit_request(&mut app, FieldEdit::state("Doing"));
    let key = request.key.clone();

    // A pull that was already in flight when the edit went out: it cannot
    // know about the edit, but it must not undo it on screen either.
    let mut pulled = ticket(3, "Gamma renamed", "2026-03-02T00:00:00Z");
    pulled.revision = 4;
    app.work_items.replace_prepared_tickets(
        &mut app.shell,
        PreparedTickets::new(vec![
            ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
            ticket(2, "Beta", "2026-02-01T00:00:00Z"),
            pulled.clone(),
        ]),
    );

    let row = app
        .work_items
        .ticket_by_key(&key)
        .expect("the row survived the pull");
    assert_eq!(row.state, "Doing", "the edit is still showing");
    assert_eq!(row.title, "Gamma renamed", "everything else is the pull's");
    assert!(app.work_items.edits_pending());

    app.work_items.reject_edit(
        &mut app.shell,
        &EditRejection {
            key: key.clone(),
            label: "State".into(),
            conflict: false,
            message: "field is read only".into(),
        },
    );
    assert_eq!(
        app.work_items.ticket_by_key(&key),
        Some(&pulled),
        "a refusal restores the freshest copy the edit did not make"
    );
}

#[test]
fn an_edit_leaves_the_filtered_view_only_once_it_lands() {
    let mut app = editing_app();
    app.work_items
        .set_query(&mut app.shell, "state:Active".into());
    assert_eq!(app.work_items.visible_count(), 3);

    let request = edit_request(&mut app, FieldEdit::state("Done"));
    assert_eq!(
        app.work_items.visible_count(),
        3,
        "the row stays where it is while the write is in flight"
    );

    let stored = stored_copy(&app, &request.key, "Done");
    app.work_items.apply_edit(
        &mut app.shell,
        EditApplied {
            ticket: stored,
            relations: Vec::new(),
            edit: request.edit.clone(),
        },
    );

    assert_eq!(
        app.work_items.visible_count(),
        2,
        "the filter drops the row when the change lands"
    );
    assert_eq!(
        app.work_items.query(),
        "state:Active",
        "the query is left alone"
    );
}

#[test]
fn an_offline_app_refuses_an_edit_and_changes_nothing() {
    let mut app = App::new(vec![ticket(1, "Alpha", "2026-01-01T00:00:00Z")]);
    app.shell
        .set_offline_reason(Some("no Azure DevOps organization; pass --org".into()));

    assert_eq!(
        app.work_items
            .edit_selected(&mut app.shell, FieldEdit::state("Doing")),
        AppAction::None
    );

    assert_eq!(app.work_items.tickets()[0].state, "Active");
    assert!(!app.work_items.edits_pending());
    let (message, level) = app.shell.notification().expect("the refusal is reported");
    assert!(message.contains("State not saved"), "{message}");
    assert!(message.contains("--org"), "{message}");
    assert_eq!(level, NotificationLevel::Error);
}

#[test]
fn a_second_edit_of_the_same_row_waits_for_the_first_to_answer() {
    let mut app = editing_app();
    let request = edit_request(&mut app, FieldEdit::state("Doing"));

    assert_eq!(
        app.work_items
            .edit_selected(&mut app.shell, FieldEdit::state("Done")),
        AppAction::None
    );
    assert_eq!(
        app.work_items.ticket_by_key(&request.key).unwrap().state,
        "Doing"
    );
    let (message, _) = app.shell.notification().unwrap();
    assert!(
        message.contains("an earlier edit is still in flight"),
        "{message}"
    );

    let applied = EditApplied {
        ticket: stored_copy(&app, &request.key, "Doing"),
        relations: Vec::new(),
        edit: request.edit,
    };
    app.work_items.apply_edit(&mut app.shell, applied);
    assert!(
        matches!(
            app.work_items
                .edit_selected(&mut app.shell, FieldEdit::state("Done")),
            AppAction::Edit(_)
        ),
        "the next edit goes out once the first has answered"
    );
}

/// The states every row is showing, in the order the table holds them.
fn states_of(app: &App) -> Vec<&str> {
    app.work_items
        .tickets()
        .iter()
        .map(|ticket| ticket.state.as_str())
        .collect()
}

/// Checks all three rows and moves them to `Doing`, which is the bulk
/// change the other tests here take apart.
fn bulk_state_change(app: &mut App) -> Vec<EditRequest> {
    check_all(app);
    shift(app, 'S');
    press(app, KeyCode::Down);
    match press(app, KeyCode::Enter) {
        AppAction::Edit(requests) => requests,
        other => panic!("a checked picker should dispatch a bulk edit, got {other:?}"),
    }
}

#[test]
fn a_picker_over_checked_rows_dispatches_one_request_for_each_of_them() {
    let mut app = picker_app();
    check_all(&mut app);

    shift(&mut app, 'S');
    assert_eq!(
        app.work_items.state_picker.scope,
        EditScope::Checked(3),
        "the picker says how many work items it is about to move"
    );
    assert_eq!(
        app.work_items.state_picker.scope.label(),
        "3 tickets",
        "which is what its title reads"
    );

    press(&mut app, KeyCode::Down);
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("choosing a state should dispatch an edit for every checked row");
    };

    assert_eq!(
        requests
            .iter()
            .map(|request| request.key.id)
            .collect::<Vec<_>>(),
        [1, 2, 3],
        "one request a work item, in the order the table holds them"
    );
    for request in &requests {
        assert_eq!(
            request.document(),
            vec![
                serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
                serde_json::json!({
                    "op": "add",
                    "path": "/fields/System.State",
                    "value": "Doing",
                }),
            ],
            "each carries its own revision test",
        );
    }
    assert_eq!(
        states_of(&app),
        ["Doing", "Doing", "Doing"],
        "every row shows the new state without waiting for Azure DevOps"
    );
    assert!(app.work_items.edits_pending());
    assert_eq!(
        app.shell.notification(),
        None,
        "nothing is said until they answer"
    );
}

#[test]
fn a_bulk_change_reports_itself_once_the_last_work_item_has_answered() {
    let mut app = picker_app();
    let requests = bulk_state_change(&mut app);

    accept(&mut app, &requests[0]);
    assert_eq!(
        app.shell.notification(),
        None,
        "a bulk change speaks once, not once a row"
    );
    accept(&mut app, &requests[1]);
    assert_eq!(app.shell.notification(), None);

    accept(&mut app, &requests[2]);
    let (message, level) = app
        .shell
        .notification()
        .expect("the tally goes up at the end");
    assert_eq!(message, "Updated 3 tickets \u{b7} State \u{2192} Doing");
    assert_eq!(level, NotificationLevel::Info);
    assert!(!app.work_items.edits_pending());
    assert_eq!(
        states_of(&app),
        ["Doing", "Doing", "Doing"],
        "the copies Azure DevOps stored replace the optimistic rows"
    );
    assert_eq!(
        app.work_items
            .tickets()
            .iter()
            .filter(|ticket| app.work_items.is_row_selected(&ticket.key))
            .count(),
        3,
        "the checked set survives the change, ready for the next one"
    );
}

#[test]
fn one_refusal_in_a_bulk_change_reverts_only_its_own_row_and_is_named() {
    let mut app = picker_app();
    let requests = bulk_state_change(&mut app);

    accept(&mut app, &requests[0]);
    accept(&mut app, &requests[1]);
    app.work_items.reject_edit(
        &mut app.shell,
        &EditRejection {
            key: requests[2].key.clone(),
            label: "State".into(),
            conflict: false,
            message: "the transition is not allowed".into(),
        },
    );

    let (message, level) = app
        .shell
        .notification()
        .expect("a refusal is never dropped");
    assert_eq!(
        message,
        "Updated 2 of 3 \u{b7} #3 failed: the transition is not allowed"
    );
    assert_eq!(level, NotificationLevel::Error);
    assert_eq!(
        states_of(&app),
        ["Doing", "Doing", "To Do"],
        "only the work item that was refused goes back"
    );
    assert!(!app.work_items.edits_pending());
    assert!(
        app.work_items.is_row_selected(&requests[2].key),
        "a refused row stays checked, so it can be tried again"
    );
}

#[test]
fn a_bulk_change_passes_over_the_work_items_already_carrying_the_value() {
    let mut app = picker_app();
    let key = app.work_items.tickets()[1].key.clone();
    let AppAction::Edit(first) =
        app.work_items
            .edit_ticket(&mut app.shell, &key, FieldEdit::state("Doing"))
    else {
        panic!("one work item moves on its own");
    };
    let first = only(first);
    accept(&mut app, &first);

    check_all(&mut app);
    shift(&mut app, 'S');
    press(&mut app, KeyCode::Down);
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("the rows that are not there yet should still be moved");
    };
    assert_eq!(
        requests
            .iter()
            .map(|request| request.key.id)
            .collect::<Vec<_>>(),
        [1, 3],
        "the work item already in the state is left alone rather than rewritten"
    );

    accept(&mut app, &requests[0]);
    accept(&mut app, &requests[1]);
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Updated 2 tickets \u{b7} State \u{2192} Doing")
    );
}

#[test]
fn a_bulk_change_with_nothing_left_to_do_says_so_and_writes_nothing() {
    let mut app = picker_app();
    check_all(&mut app);

    shift(&mut app, 'S');
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        AppAction::None,
        "the state they are all already in is a no-op"
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Nothing to change \u{b7} State \u{2192} To Do")
    );
    assert!(!app.work_items.edits_pending());
    assert_eq!(states_of(&app), ["To Do", "To Do", "To Do"]);
}

#[test]
fn the_editors_that_are_not_worth_making_in_bulk_stay_on_the_row_under_the_cursor() {
    let mut app = picker_app();
    check_all(&mut app);

    app.work_items
        .run_command(&mut app.shell, CommandId::EditTitle);
    type_query(&mut app, "!");
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("the title prompt should still write one work item");
    };
    assert_eq!(
        only(requests).key.id,
        3,
        "the same title on three work items is never what was meant"
    );
    assert_eq!(
        app.work_items
            .tickets()
            .iter()
            .filter(|ticket| ticket.title.ends_with('!'))
            .count(),
        1,
        "and only that row is renamed"
    );
}

#[test]
fn an_undo_puts_the_value_back_and_writes_it_to_azure_devops_to_do_it() {
    let mut app = editing_app();
    let request = edit_request(&mut app, FieldEdit::state("Doing"));
    let key = request.key.clone();
    accept_edit(&mut app, &request);

    let undone = only(undo(&mut app));
    assert_eq!(undone.key, key, "the work item the edit was made on");
    assert_eq!(
        undone.document(),
        vec![
            serde_json::json!({"op": "test", "path": "/rev", "value": 2}),
            serde_json::json!({
                "op": "add",
                "path": "/fields/System.State",
                "value": "Active",
            }),
        ],
        "an undo is an ordinary edit, guarded by the revision the write settled on"
    );
    assert_eq!(
        app.work_items.ticket_by_key(&key).unwrap().state,
        "Active",
        "the row goes back without waiting for the network"
    );

    accept_edit(&mut app, &undone);
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Undid State on #3 (Doing \u{2192} Active)")
    );
    assert_eq!(
        press(&mut app, KeyCode::Char('u')),
        AppAction::None,
        "an undo is not itself undoable, or u would only ever toggle"
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Nothing to undo")
    );
}

#[test]
fn undoing_an_edit_of_a_field_that_was_empty_clears_it_rather_than_emptying_it() {
    let mut unset = ticket(1, "Alpha", "2026-01-01T00:00:00Z");
    unset.priority = None;
    let mut app = App::new(vec![unset]);
    app.shell.enable_sync();
    app.work_items.set_table_viewport(1);

    let request = edit_request(&mut app, FieldEdit::priority(1));
    accept_edit(&mut app, &request);
    assert_eq!(app.work_items.tickets()[0].priority, Some(1));

    let undone = only(undo(&mut app));
    assert_eq!(
        undone.document(),
        vec![
            serde_json::json!({"op": "test", "path": "/rev", "value": 2}),
            serde_json::json!({
                "op": "remove",
                "path": "/fields/Microsoft.VSTS.Common.Priority",
            }),
        ],
        "a field that was unset goes back to unset, not to an empty value"
    );

    accept_edit(&mut app, &undone);
    assert_eq!(app.work_items.tickets()[0].priority, None);
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Undid Priority on #1 (1 \u{2192} (none))")
    );
}

#[test]
fn pressing_undo_with_nothing_to_take_back_says_so() {
    let mut app = editing_app();

    assert_eq!(press(&mut app, KeyCode::Char('u')), AppAction::None);
    let (message, level) = app
        .shell
        .notification()
        .expect("a key that did nothing says why");
    assert_eq!(message, "Nothing to undo");
    assert_eq!(level, NotificationLevel::Info);
    assert!(!app.work_items.edits_pending(), "and nothing went out");
}

#[test]
fn a_refused_edit_never_reaches_the_undo_stack() {
    let mut app = editing_app();
    let request = edit_request(&mut app, FieldEdit::state("Doing"));

    app.work_items.reject_edit(
        &mut app.shell,
        &EditRejection {
            key: request.key.clone(),
            label: "State".into(),
            conflict: true,
            message: "the test operation on /rev failed".into(),
        },
    );

    assert_eq!(
        press(&mut app, KeyCode::Char('u')),
        AppAction::None,
        "an edit that left nothing behind has nothing to take back"
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Nothing to undo")
    );
}

#[test]
fn a_refused_undo_is_reported_like_any_other_conflict() {
    let mut app = editing_app();
    let request = edit_request(&mut app, FieldEdit::state("Doing"));
    let key = request.key.clone();
    accept_edit(&mut app, &request);

    let undone = only(undo(&mut app));
    app.work_items.reject_edit(
        &mut app.shell,
        &EditRejection {
            key: undone.key.clone(),
            label: "State".into(),
            conflict: true,
            message: "the test operation on /rev failed".into(),
        },
    );

    let (message, level) = app
        .shell
        .notification()
        .expect("a refused undo is never dropped");
    assert!(message.contains("#3 changed in Azure DevOps"), "{message}");
    assert!(message.contains("State not saved"), "{message}");
    assert_eq!(level, NotificationLevel::Error);
    assert_eq!(
        app.work_items.ticket_by_key(&key).unwrap().state,
        "Doing",
        "the row stays where the edit left it"
    );
    assert_eq!(
        press(&mut app, KeyCode::Char('u')),
        AppAction::None,
        "and the value is not offered again on a copy that has moved on"
    );
}

#[test]
fn the_undo_stack_remembers_twenty_edits_and_forgets_the_ones_before_them() {
    let mut app = editing_app();
    let key = app
        .work_items
        .selected_ticket()
        .expect("a row is selected")
        .key
        .clone();

    for round in 1..=UNDO_DEPTH + 1 {
        let title = FieldEdit::title(&format!("Alpha {round}"));
        let AppAction::Edit(requests) = app.work_items.edit_ticket(&mut app.shell, &key, title)
        else {
            panic!("a rename should be dispatched");
        };
        let request = only(requests);
        accept_edit(&mut app, &request);
    }

    for _ in 0..UNDO_DEPTH {
        let request = only(undo(&mut app));
        accept_edit(&mut app, &request);
    }

    assert_eq!(
        app.work_items.ticket_by_key(&key).unwrap().title,
        "Alpha 1",
        "twenty edits back is as far as it goes; the title before them is forgotten"
    );
    assert_eq!(press(&mut app, KeyCode::Char('u')), AppAction::None);
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Nothing to undo")
    );
}

#[test]
fn one_press_takes_a_whole_bulk_change_back() {
    let mut app = picker_app();
    let requests = bulk_state_change(&mut app);
    for request in &requests {
        accept(&mut app, request);
    }
    assert_eq!(states_of(&app), ["Doing", "Doing", "Doing"]);

    let undone = undo(&mut app);
    assert_eq!(
        undone
            .iter()
            .map(|request| request.key.id)
            .collect::<Vec<_>>(),
        [1, 2, 3],
        "every work item the change touched, under one press"
    );
    assert_eq!(
        states_of(&app),
        ["To Do", "To Do", "To Do"],
        "and every row goes back at once"
    );

    for request in &undone {
        assert_eq!(
            request.expected_revision, 2,
            "each carries the revision its own write settled on"
        );
        accept(&mut app, request);
    }
    let (message, level) = app
        .shell
        .notification()
        .expect("the tally goes up at the end");
    assert_eq!(message, "Undid State on 3 tickets");
    assert_eq!(level, NotificationLevel::Info);
    assert_eq!(
        press(&mut app, KeyCode::Char('u')),
        AppAction::None,
        "the whole change went back as one, so there is nothing left of it"
    );
}

#[test]
fn a_bulk_undo_that_only_partly_lands_names_the_rows_left_where_they_were() {
    let mut app = picker_app();
    let requests = bulk_state_change(&mut app);
    for request in &requests {
        accept(&mut app, request);
    }

    let undone = undo(&mut app);
    accept(&mut app, &undone[0]);
    accept(&mut app, &undone[1]);
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Updated 3 tickets \u{b7} State \u{2192} Doing"),
        "the change's own summary still stands: an undo speaks once, not once a row"
    );

    app.work_items.reject_edit(
        &mut app.shell,
        &EditRejection {
            key: undone[2].key.clone(),
            label: "State".into(),
            conflict: true,
            message: "the test operation on /rev failed".into(),
        },
    );

    let (message, level) = app
        .shell
        .notification()
        .expect("a half-done undo is never silent");
    assert_eq!(
        message,
        "Undid 2 of 3 \u{b7} #3 failed: it changed in Azure DevOps"
    );
    assert_eq!(level, NotificationLevel::Error);
    assert_eq!(
        states_of(&app),
        ["To Do", "To Do", "Doing"],
        "only the work item that was refused is left where the change put it"
    );
}

#[test]
fn the_edit_menu_lists_the_field_editors_and_opens_the_one_chosen() {
    let mut app = picker_app();

    assert_eq!(press(&mut app, KeyCode::Char('e')), AppAction::None);
    assert_eq!(app.work_items.mode, WorkItemMode::Edit);
    assert_eq!(
        EDIT_MENU
            .iter()
            .map(|entry| entry.label)
            .collect::<Vec<_>>(),
        [
            "State",
            "Title",
            "Priority",
            "Tags",
            "Assignee",
            "Iteration",
            "Area",
            "Set parent\u{2026}",
            "Description",
            "Add comment",
            "New child",
            "Delete work item\u{2026}"
        ],
        "later field editors append their own row above the two that act on
             the work item as a whole"
    );
    assert_eq!(app.work_items.edit_menu.index, 0);

    assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
    assert_eq!(app.work_items.mode, WorkItemMode::StatePicker);
    assert_eq!(
        state_names(&app.work_items.state_picker.options),
        ["To Do", "Doing", "Done"]
    );

    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(app.work_items.mode, WorkItemMode::Edit);
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Browse,
        "e closes the menu it opened"
    );
}

fn prompt_text(app: &App) -> String {
    app.work_items
        .prompt
        .as_ref()
        .expect("a prompt should be open")
        .input
        .text()
        .to_owned()
}

/// Clears the prompt and types `text` into it, one key at a time.
fn type_over(app: &mut App, text: &str) {
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    for character in text.chars() {
        press(app, KeyCode::Char(character));
    }
}

#[test]
fn the_title_prompt_opens_on_the_current_title_and_saves_a_trimmed_one() {
    let mut app = edit_app();

    open_editor(&mut app, 1);
    assert_eq!(app.work_items.mode, WorkItemMode::Prompt);
    assert_eq!(prompt_text(&app), "Gamma", "the prompt opens prefilled");

    type_over(&mut app, "  Renamed gamma  ");
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("a new title should dispatch an edit");
    };
    let request = only(requests);

    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert!(app.work_items.prompt.is_none());
    assert_eq!(request.key.id, 3);
    assert_eq!(
        request.document(),
        vec![
            serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
            serde_json::json!({
                "op": "add",
                "path": "/fields/System.Title",
                "value": "Renamed gamma",
            }),
        ],
        "the title is trimmed before it is sent"
    );
    assert_eq!(
        app.work_items
            .selected_ticket()
            .map(|ticket| ticket.title.as_str()),
        Some("Renamed gamma"),
        "the row shows the new title without waiting for Azure DevOps"
    );
}

#[test]
fn an_empty_title_is_refused_locally_and_an_unchanged_one_writes_nothing() {
    let mut app = edit_app();

    open_editor(&mut app, 1);
    type_over(&mut app, "   ");
    assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Prompt,
        "a blank title leaves the prompt open to fix"
    );
    assert!(!app.work_items.edits_pending(), "nothing was sent");
    let (message, level) = app.shell.notification().expect("a refusal is reported");
    assert!(message.contains("title cannot be empty"), "{message}");
    assert_eq!(level, NotificationLevel::Error);

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert_eq!(
        app.work_items
            .selected_ticket()
            .map(|ticket| ticket.title.as_str()),
        Some("Gamma"),
        "cancelling leaves the row exactly as it was"
    );

    let mut app = edit_app();
    open_editor(&mut app, 1);
    assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert!(!app.work_items.edits_pending());
    assert_eq!(
        app.shell.notification(),
        None,
        "an unchanged title closes silently"
    );
}

#[test]
fn the_tags_prompt_trims_deduplicates_and_rejoins_what_it_saves() {
    let mut app = edit_app();

    open_editor(&mut app, 3);
    assert_eq!(app.work_items.mode, WorkItemMode::Prompt);
    assert_eq!(
        prompt_text(&app),
        "rust",
        "the prompt opens on the tags held"
    );

    type_over(&mut app, "rust; Rust ;; tui");
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("a new tag list should dispatch an edit");
    };
    let request = only(requests);
    assert_eq!(
        request.document(),
        vec![
            serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
            serde_json::json!({
                "op": "add",
                "path": "/fields/System.Tags",
                "value": "rust; tui",
            }),
        ]
    );
    assert_eq!(
        app.work_items
            .selected_ticket()
            .map(|ticket| ticket.tags.clone()),
        Some(vec!["rust".to_owned(), "tui".to_owned()]),
        "the Tags cell shows the normalised list at once"
    );
}

#[test]
fn a_tag_list_that_normalises_to_what_is_there_writes_nothing() {
    let mut app = edit_app();

    open_editor(&mut app, 3);
    type_over(&mut app, "  rust ;; RUST ");
    assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert!(!app.work_items.edits_pending());
    assert_eq!(app.shell.notification(), None);
}

#[test]
fn the_description_row_hands_the_raw_html_to_the_editor() {
    let mut gamma = ticket(3, "Gamma", "2026-03-01T00:00:00Z");
    gamma.description_html = "<p>Hand it to <code>$EDITOR</code>.</p>".into();
    gamma.description = "Hand it to `$EDITOR`.".into();
    let mut app = App::new(vec![
        ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Beta", "2026-02-01T00:00:00Z"),
        gamma,
    ]);
    app.shell.enable_sync();
    app.work_items.set_table_viewport(3);
    let key = app.work_items.selected_ticket().unwrap().key.clone();

    press(&mut app, KeyCode::Char('e'));
    for _ in 0..menu_row(&app, CommandId::EditDescription) {
        press(&mut app, KeyCode::Down);
    }
    let action = press(&mut app, KeyCode::Enter);

    assert_eq!(
        action,
        AppAction::EditDescription {
            key,
            html: "<p>Hand it to <code>$EDITOR</code>.</p>".into(),
        },
        "the editor is opened on the markup Azure DevOps stores, not the reading of it"
    );
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Browse,
        "the TUI is on its way out"
    );
    assert!(
        !app.work_items.edits_pending(),
        "nothing is written until the editor is"
    );
    assert_eq!(app.shell.notification(), None);
}

#[test]
fn an_offline_run_refuses_the_description_before_the_editor_opens() {
    let mut app = App::new(vec![ticket(3, "Gamma", "2026-03-01T00:00:00Z")]);
    app.work_items.set_table_viewport(3);

    let row = menu_row(&app, CommandId::EditDescription);
    open_editor(&mut app, row);

    let (message, level) = app.shell.notification().expect("an offline run says so");
    assert!(message.contains("#3 description not saved"), "{message}");
    assert_eq!(level, NotificationLevel::Error);
    assert!(!app.work_items.edits_pending());
}

/// The Edit menu row that opens the comment box, found by the command it
/// runs so adding a field editor above it moves nothing here.
fn comment_row() -> usize {
    EDIT_MENU
        .iter()
        .position(|entry| entry.command == CommandId::AddComment)
        .expect("the Edit menu offers a comment row")
}

/// One comment as Azure DevOps hands it back, already carrying the id,
/// date, and author only the server can give it.
fn comment(id: i64, at: &str, text: &str) -> CommentRecord {
    CommentRecord {
        ticket: TicketKey {
            organization: "demo".into(),
            id: 3,
        },
        comment_id: id,
        created_at: crate::timestamp::ts(at),
        author: Some("Jacob Ragsdale".into()),
        text: text.into(),
    }
}

#[test]
fn the_comment_prompt_opens_empty_and_posts_what_was_typed() {
    let mut app = edit_app();

    open_editor(&mut app, comment_row());
    assert_eq!(app.work_items.mode, WorkItemMode::Prompt);
    assert_eq!(
        prompt_text(&app),
        "",
        "there is nothing to edit, only to say"
    );
    let prompt = app
        .work_items
        .prompt
        .as_ref()
        .expect("a prompt should be open");
    assert_eq!(prompt.field, PromptField::Comment);
    assert_eq!(
        prompt.field.title(prompt.id),
        "Comment on #3",
        "the prompt names the work item it is about"
    );

    type_over(&mut app, "  Merged into main  ");
    let action = press(&mut app, KeyCode::Enter);
    assert_eq!(
        action,
        AppAction::Comment {
            key: app.work_items.selected_ticket().unwrap().key.clone(),
            text: "Merged into main".into(),
        },
        "the comment is trimmed before it is sent"
    );
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert!(app.work_items.prompt.is_none());
    assert!(
        app.work_items.comments_pending(),
        "the post is waiting on Azure DevOps"
    );
    assert!(
        app.work_items
            .comments_for(&app.work_items.selected_ticket().unwrap().key)
            .is_empty(),
        "nothing is shown until the server has stored it"
    );

    assert_eq!(
        app.work_items
            .comment_selected(&mut app.shell, "And again".into()),
        AppAction::None,
        "one comment at a time"
    );
    let (message, level) = app
        .shell
        .notification()
        .expect("the second attempt says so");
    assert!(message.contains("still in flight"), "{message}");
    assert_eq!(level, NotificationLevel::Error);
}

#[test]
fn a_blank_comment_is_refused_locally_and_leaves_the_prompt_open() {
    let mut app = edit_app();

    open_editor(&mut app, comment_row());
    type_over(&mut app, "   ");
    assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Prompt,
        "a blank comment leaves the prompt open to fix"
    );
    assert!(!app.work_items.comments_pending(), "nothing was sent");
    let (message, level) = app.shell.notification().expect("a refusal is reported");
    assert!(message.contains("comment cannot be empty"), "{message}");
    assert_eq!(level, NotificationLevel::Error);

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert!(app.work_items.prompt.is_none());
}

#[test]
fn a_stored_comment_joins_the_discussion_newest_first() {
    let mut app = edit_app();
    let key = app.work_items.selected_ticket().unwrap().key.clone();

    app.work_items
        .comment_selected(&mut app.shell, "Merged into main".into());
    app.work_items.apply_comment(
        &mut app.shell,
        comment(9, "2026-03-04T00:00:00Z", "Merged into main"),
    );

    assert!(!app.work_items.comments_pending(), "the post was answered");
    assert_eq!(
        app.work_items
            .comments_for(&key)
            .iter()
            .map(|held| held.text.as_str())
            .collect::<Vec<_>>(),
        ["Merged into main"]
    );
    let (message, level) = app.shell.notification().expect("the post reports itself");
    assert_eq!(message, "Commented on #3");
    assert_eq!(level, NotificationLevel::Info);

    // A details fetch that lands afterwards brings the same comment back;
    // it replaces the one already held rather than doubling it, and an
    // older comment files under it.
    app.work_items.apply_comment(
        &mut app.shell,
        comment(5, "2026-03-01T00:00:00Z", "Blocked on the API"),
    );
    app.work_items.apply_comment(
        &mut app.shell,
        comment(9, "2026-03-04T00:00:00Z", "Merged into main"),
    );
    assert_eq!(
        app.work_items
            .comments_for(&key)
            .iter()
            .map(|held| held.text.as_str())
            .collect::<Vec<_>>(),
        ["Merged into main", "Blocked on the API"],
        "the newest comment reads first"
    );
}

#[test]
fn a_refused_comment_changes_nothing_and_says_why() {
    let mut app = edit_app();
    let key = app.work_items.selected_ticket().unwrap().key.clone();

    app.work_items
        .comment_selected(&mut app.shell, "Merged into main".into());
    app.work_items
        .reject_comment(&mut app.shell, &key, "HTTP 403: the work item is read only");

    assert!(
        app.work_items.comments_for(&key).is_empty(),
        "nothing was filed"
    );
    assert!(
        !app.work_items.comments_pending(),
        "the row is free to try again"
    );
    let (message, level) = app.shell.notification().expect("a refusal is reported");
    assert_eq!(
        message,
        "#3 comment not posted: HTTP 403: the work item is read only"
    );
    assert_eq!(level, NotificationLevel::Error);

    assert!(
        matches!(
            app.work_items
                .comment_selected(&mut app.shell, "Merged into main".into()),
            AppAction::Comment { .. }
        ),
        "a refusal does not block the next attempt"
    );
}

#[test]
fn a_prompt_takes_a_paste_at_its_caret() {
    let mut app = edit_app();

    open_editor(&mut app, 1);
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    app.handle_paste("Pasted\ttitle");
    assert_eq!(prompt_text(&app), "Pastedtitle");
}
