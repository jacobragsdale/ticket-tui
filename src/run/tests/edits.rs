//! Writing back: creating, commenting, editing one work item and many.

use super::*;

#[test]
fn a_filed_work_item_reaches_the_table_and_a_refused_one_reopens_the_form() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let mut filed = ticket(4);
    filed.work_item_type = "Issue".into();
    filed.title = "Honour Retry-After".into();
    let parent = TicketKey {
        organization: "example-org".into(),
        id: 3,
    };
    let (mut app, mut repository, mut runtime) = synced_app(
        &path,
        FakeAzure::creating(
            filed.clone(),
            vec![RelationRecord {
                from: filed.key.clone(),
                to: parent.clone(),
                kind: RelationKind::Parent,
            }],
        ),
    );

    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    assert_eq!(app.work_items.mode, WorkItemMode::Form);
    app.work_items
        .form
        .as_mut()
        .expect("the form is open")
        .set_value(FormFieldId::Title, "Honour Retry-After");
    let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(matches!(action, AppAction::Create { .. }));

    handle_action(action, &mut app, &mut runtime, &failing_opener);
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Creating Issue\u{2026}")
    );
    assert_eq!(
        app.work_items.tickets().len(),
        3,
        "nothing shows until it is stored"
    );
    await_create(&mut app, &mut repository, &mut runtime);

    assert_eq!(app.work_items.tickets().len(), 4);
    assert_eq!(
        app.work_items.selected_ticket().map(|ticket| ticket.key.id),
        Some(4),
        "the table selects the work item that was just filed"
    );
    assert_eq!(
        app.work_items.family_of(&filed.key).ancestors,
        vec![parent.clone()]
    );
    assert_eq!(
        app.work_items.family_of(&parent).children,
        vec![filed.key.clone()]
    );
    assert!(
        repository
            .load_all()
            .unwrap()
            .iter()
            .any(|ticket| ticket.key.id == 4),
        "the worker wrote it to SQLite on the way through"
    );
    assert!(
        !poll_watch(&mut app, &repository, &mut ReloadEngine::default()),
        "our own write is not another writer to reload behind"
    );

    let (mut app, mut repository, mut runtime) = synced_app(
        &path,
        FakeAzure::refusing(400, "TF401320: rule error", Vec::new()),
    );
    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    app.work_items
        .form
        .as_mut()
        .expect("the form is open")
        .set_value(FormFieldId::Title, "Honour Retry-After");
    let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    handle_action(action, &mut app, &mut runtime, &failing_opener);
    await_create(&mut app, &mut repository, &mut runtime);

    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Form,
        "the form comes back to be answered"
    );
    assert_eq!(
        app.work_items
            .form
            .as_ref()
            .unwrap()
            .value(FormFieldId::Title),
        "Honour Retry-After",
        "with everything still in it"
    );
    let (message, level) = app.shell.notification().expect("the refusal is reported");
    assert!(message.contains("TF401320: rule error"), "{message}");
    assert_eq!(level, NotificationLevel::Error);
}

#[test]
fn a_posted_comment_reaches_the_discussion_and_a_refused_one_only_the_toast() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let stored = CommentRecord {
        ticket: TicketKey {
            organization: "example-org".into(),
            id: 3,
        },
        comment_id: 11,
        created_at: Timestamp::parse("2026-03-04T09:15:00Z").unwrap(),
        author: Some("Jacob Ragsdale".into()),
        text: "Merged into main".into(),
    };
    let (mut app, mut repository, mut runtime) =
        synced_app(&path, FakeAzure::commenting(stored.clone()));
    let selected = app.work_items.selected_ticket().unwrap().key.clone();
    assert_eq!(selected.id, 3, "the newest work item starts selected");

    let action = app
        .work_items
        .comment_selected(&mut app.shell, "Merged into main".into());
    assert!(matches!(action, AppAction::Comment { .. }));
    handle_action(action, &mut app, &mut runtime, &failing_opener);
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Posting comment on #3\u{2026}")
    );
    assert!(
        app.work_items.comments_for(&selected).is_empty(),
        "nothing shows until Azure DevOps has stored it"
    );
    await_comment(&mut app, &mut repository, &mut runtime);

    assert_eq!(app.work_items.comments_for(&selected), vec![&stored]);
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Commented on #3")
    );
    assert_eq!(
        app.shell.data_signature,
        db::data_signature(&path),
        "the worker wrote that row, so the watcher leaves the file alone"
    );

    let (mut app, mut repository, mut runtime) =
        synced_app(&path, FakeAzure::returning(Vec::new()));
    let action = app
        .work_items
        .comment_selected(&mut app.shell, "Merged into main".into());
    handle_action(action, &mut app, &mut runtime, &failing_opener);
    await_comment(&mut app, &mut repository, &mut runtime);

    let (message, level) = app.shell.notification().expect("a refusal is reported");
    assert!(message.contains("comment not posted"), "{message}");
    assert!(message.contains("read only"), "{message}");
    assert_eq!(level, NotificationLevel::Error);
    assert!(
        app.work_items.comments_for(&selected).is_empty(),
        "a refused comment files nothing"
    );
}

#[test]
fn an_accepted_edit_updates_the_row_and_the_database_without_a_reload() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let mut stored = ticket(3);
    stored.state = "Done".into();
    stored.revision = 9;
    let (mut app, mut repository, mut runtime) =
        synced_app(&path, FakeAzure::storing(stored.clone()));
    // The edit itself is the subject, so the row it finishes stays on the
    // table for the assertions below to find it on.
    app.work_items.set_show_finished(&mut app.shell, true);
    let selected = app.work_items.selected_ticket().unwrap().key.clone();
    assert_eq!(selected.id, 3, "the newest work item starts selected");

    let action = app
        .work_items
        .edit_selected(&mut app.shell, FieldEdit::state("Done"));
    assert!(matches!(action, AppAction::Edit(_)));
    assert_eq!(
        app.work_items.selected_ticket().unwrap().state,
        "Done",
        "the row changes before the worker is even asked"
    );
    handle_action(action, &mut app, &mut runtime, &failing_opener);
    await_edit(&mut app, &mut repository, &mut runtime);

    assert_eq!(app.work_items.ticket_by_key(&selected), Some(&stored));
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Updated #3 · State → Done")
    );
    assert_eq!(
        app.work_items.selected_ticket().map(|ticket| ticket.key.id),
        Some(3),
        "an edit landing leaves the selection where it was"
    );
    assert_eq!(
        SqliteTicketRepository::open_existing(&path)
            .unwrap()
            .load_all()
            .unwrap()
            .iter()
            .find(|ticket| ticket.key.id == 3)
            .map(|ticket| ticket.state.clone()),
        Some("Done".to_owned()),
        "the worker wrote the row it was told to write"
    );
    assert!(
        !poll_watch(&mut app, &repository, &mut ReloadEngine::default()),
        "the watcher does not chase the row our own worker just wrote"
    );
    assert!(!runtime.scheduler.in_flight(), "nothing else was asked for");
}

#[test]
fn finishing_the_selected_work_item_takes_it_off_the_table_once_the_write_lands() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let stored = Ticket {
        state: "Done".into(),
        revision: 9,
        ..ticket(3)
    };
    let (mut app, mut repository, mut runtime) = synced_app(&path, FakeAzure::storing(stored));
    assert_eq!(visible_ids(&app), vec![3, 2, 1]);

    let action = app
        .work_items
        .edit_selected(&mut app.shell, FieldEdit::state("Done"));
    handle_action(action, &mut app, &mut runtime, &failing_opener);
    assert_eq!(
        visible_ids(&app),
        vec![3, 2, 1],
        "the optimistic copy stays on the table, so a refusal has a row to revert to"
    );

    await_edit(&mut app, &mut repository, &mut runtime);

    assert_eq!(
        visible_ids(&app),
        vec![2, 1],
        "the copy Azure DevOps stored is finished, so the row leaves"
    );
    assert_eq!(
        app.work_items.selected_ticket().map(|ticket| ticket.key.id),
        Some(2),
        "the cursor lands on the next piece of work rather than on nothing"
    );
    assert_eq!(
        app.work_items.hidden_finished(&app.shell),
        1,
        "and the row it took off the table is counted for the `i` overlay"
    );
}

#[test]
fn a_bulk_change_writes_every_checked_work_item_and_reports_itself_once() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let stored: Vec<Ticket> = [2, 3]
        .into_iter()
        .map(|id| Ticket {
            state: "Done".into(),
            revision: 9,
            ..ticket(id)
        })
        .collect();
    let (mut app, mut repository, mut runtime) =
        synced_app(&path, FakeAzure::storing_each(stored.clone()));

    // Space checks the row under the cursor: #3, then #2 below it.
    for _ in 0..2 {
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    let action = app
        .work_items
        .edit_checked(&mut app.shell, FieldEdit::state("Done"));
    assert!(
        matches!(&action, AppAction::Edit(requests) if requests.len() == 2),
        "one request a checked row, got {action:?}"
    );
    handle_action(action, &mut app, &mut runtime, &failing_opener);
    await_edit(&mut app, &mut repository, &mut runtime);

    for copy in &stored {
        assert_eq!(
            app.work_items.ticket_by_key(&copy.key),
            Some(copy),
            "every checked work item carries the copy Azure DevOps stored"
        );
    }
    assert_eq!(
        app.work_items
            .ticket_by_key(&ticket(1).key)
            .map(|ticket| ticket.state.clone()),
        Some("Active".to_owned()),
        "the row that was never checked is untouched"
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Updated 2 tickets · State → Done"),
        "one summary, not one toast a work item"
    );
    assert_eq!(
        SqliteTicketRepository::open_existing(&path)
            .unwrap()
            .load_all()
            .unwrap()
            .iter()
            .filter(|ticket| ticket.state == "Done")
            .count(),
        2,
        "the worker wrote both rows it was told to write"
    );
}

#[test]
fn a_conflicting_edit_puts_the_row_back_and_pulls_the_latest_copy() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let (mut app, mut repository, mut runtime) = synced_app(
        &path,
        FakeAzure::refusing(409, "the work item has been changed", vec![ticket(3)]),
    );
    let selected = app.work_items.selected_ticket().unwrap().key.clone();

    let action = app
        .work_items
        .edit_selected(&mut app.shell, FieldEdit::state("Done"));
    handle_action(action, &mut app, &mut runtime, &failing_opener);
    await_edit(&mut app, &mut repository, &mut runtime);

    assert_eq!(
        app.work_items
            .ticket_by_key(&selected)
            .map(|ticket| ticket.state.clone()),
        Some("Active".to_owned()),
        "the row goes back to what Azure DevOps still holds"
    );
    let (message, level) = app
        .shell
        .notification()
        .expect("a conflict is always reported");
    assert!(message.contains("#3 changed in Azure DevOps"), "{message}");
    assert!(message.contains("State not saved"), "{message}");
    assert_eq!(level, NotificationLevel::Error);
    assert!(
        app.shell.sync_pending && runtime.scheduler.in_flight(),
        "a conflict asks for the latest copy straight away"
    );

    await_sync(&mut app, &mut repository, &mut runtime);
    assert_eq!(
        app.work_items.tickets().len(),
        1,
        "the pull the conflict asked for ran"
    );
}

#[test]
fn an_edit_with_no_worker_left_reverts_the_row_and_says_why() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let (mut app, _repository, mut runtime) =
        synced_app(&path, FakeAzure::returning(vec![ticket(3)]));
    let action = app
        .work_items
        .edit_selected(&mut app.shell, FieldEdit::state("Done"));
    runtime.worker = None;
    runtime.offline_reason = Some("no Azure DevOps organization; pass --org".into());

    handle_action(action, &mut app, &mut runtime, &failing_opener);

    assert_eq!(
        app.work_items
            .selected_ticket()
            .map(|ticket| ticket.state.clone()),
        Some("Active".to_owned()),
        "an edit that never left is not left showing"
    );
    assert!(!app.work_items.edits_pending());
    let (message, level) = app.shell.notification().unwrap();
    assert!(message.contains("--org"), "{message}");
    assert_eq!(level, NotificationLevel::Error);
}
