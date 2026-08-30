//! Reading a work item's details once the selection settles, and the agent
//! context file the view publishes to.

use super::*;

/// Pumps the event loop's sync polling until the details fetch answers.
fn await_details(
    app: &mut App,
    repository: &mut SqliteTicketRepository,
    runtime: &mut SyncRuntime,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.work_items.details_pending.is_some() {
        poll_sync(app, repository, runtime);
        assert!(Instant::now() < deadline, "the sync worker timed out");
        thread::yield_now();
    }
}

/// A work item whose details nobody has read, so the pane wants them.
fn unread(id: i64) -> Ticket {
    let mut ticket = ticket(id);
    ticket.revision = 4;
    ticket
}

fn comment(id: i64, text: &str) -> CommentRecord {
    CommentRecord {
        ticket: ticket(id).key,
        comment_id: id * 10,
        created_at: Timestamp::parse("2026-03-04T00:00:00Z").unwrap(),
        author: Some("Avery Chen".into()),
        text: text.into(),
    }
}

fn transition(id: i64, to: &str) -> HistoryRecord {
    HistoryRecord {
        ticket: ticket(id).key,
        revision: 4,
        changed_at: Timestamp::parse("2026-03-05T10:00:00Z").unwrap(),
        changed_by: Some("Jacob Ragsdale".into()),
        field_name: "State".into(),
        old_value: Some("To Do".into()),
        new_value: Some(to.to_owned()),
    }
}

#[test]
fn details_are_read_once_the_selection_settles_and_never_while_it_is_moving() {
    let start = Instant::now();
    let after = |millis: u64| start + Duration::from_millis(millis);
    let mut engine = DetailsEngine::default();
    let (first, second) = (unread(1), unread(2));

    // Scrolling: a different work item every hundred milliseconds, so the
    // rest period never runs out and nothing is asked for.
    assert_eq!(engine.due(Some(&first), start), None);
    assert_eq!(engine.due(Some(&second), after(100)), None);
    assert_eq!(engine.due(Some(&first), after(200)), None);
    assert_eq!(engine.due(Some(&first), after(400)), None);
    assert_eq!(
        engine.time_until_due(after(400)),
        Some(Duration::from_millis(100)),
        "the event loop wakes for the rest of the rest period"
    );

    assert_eq!(
        engine.due(Some(&first), after(520)),
        Some(first.key.clone()),
        "settled for longer than the rest period, so it is worth reading"
    );
    assert_eq!(
        engine.due(Some(&first), after(900)),
        None,
        "one request at a time, however long the selection sits there"
    );
    assert_eq!(engine.time_until_due(after(900)), None);

    engine.finish();
    let mut read = first.clone();
    read.details_rev = read.revision;
    assert_eq!(
        engine.due(Some(&read), after(2000)),
        None,
        "a work item whose details are already current asks for nothing"
    );
    assert_eq!(engine.due(None, after(2100)), None);
    assert_eq!(engine.time_until_due(after(2100)), None);

    // A work item that cannot be read is reported once and never asked
    // about again, however often the selection returns to it.
    assert_eq!(engine.due(Some(&second), after(3000)), None);
    assert_eq!(
        engine.due(Some(&second), after(3400)),
        Some(second.key.clone())
    );
    assert!(engine.fail(second.key.clone()));
    assert_eq!(engine.due(Some(&second), after(4000)), None);
    assert_eq!(engine.due(Some(&second), after(5000)), None);

    let third = unread(3);
    assert_eq!(engine.due(Some(&third), after(6000)), None);
    assert_eq!(
        engine.due(Some(&third), after(6400)),
        Some(third.key.clone())
    );
    assert!(
        !engine.fail(third.key.clone()),
        "one notification about a pane nobody asked to fill is enough"
    );
}

#[test]
fn settling_on_a_work_item_reads_its_details_and_patches_only_its_own_rows() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let details = WorkItemDetails {
        comments: vec![comment(3, "Looks good")],
        history: vec![transition(3, "Doing")],
    };
    let (mut app, mut repository, mut runtime) =
        synced_app(&path, FakeAzure::detailing(3, details.clone()));
    let selected = app.work_items.selected_ticket().unwrap().key.clone();
    assert_eq!(selected.id, 3, "the newest work item starts selected");
    // Another work item's discussion, which this fetch must not disturb.
    let elsewhere = ticket(1).key;
    app.work_items.set_workspace_graph(
        &mut app.shell,
        TicketGraph {
            comments: vec![comment(1, "Someone else's thread")],
            history: vec![transition(1, "Done")],
            ..TicketGraph::default()
        },
    );

    assert!(
        !dispatch_due_details(&mut app, &mut runtime),
        "the selection has only just landed"
    );
    // Stand where the selection would be after the rest period, rather
    // than waiting out three hundred milliseconds of real time.
    runtime.details.resting = Some((selected.clone(), Instant::now() - DETAILS_REST));

    assert!(dispatch_due_details(&mut app, &mut runtime));
    assert_eq!(
        app.work_items.details_pending.as_ref(),
        Some(&selected),
        "the pane says it is reading while the request is out"
    );
    assert!(
        !dispatch_due_details(&mut app, &mut runtime),
        "nothing is queued behind the request in flight"
    );

    await_details(&mut app, &mut repository, &mut runtime);

    assert_eq!(
        app.work_items.comments_for(&selected),
        vec![&details.comments[0]]
    );
    assert_eq!(
        app.work_items.history_for(&selected),
        vec![&details.history[0]]
    );
    assert_eq!(
        app.work_items.comments_for(&elsewhere),
        vec![&comment(1, "Someone else's thread")],
        "another work item's discussion is left exactly as it was"
    );
    assert_eq!(
        app.work_items.history_for(&elsewhere),
        vec![&transition(1, "Done")]
    );
    assert_eq!(
        app.work_items
            .ticket_by_key(&selected)
            .map(|ticket| ticket.details_rev),
        Some(1),
        "the row records the revision its details came from"
    );
    assert!(
        app.shell.notification().is_none(),
        "a fetch nobody asked for is silent"
    );
    assert!(
        !poll_watch(&mut app, &repository, &mut ReloadEngine::default()),
        "the watcher does not chase the rows our own worker just wrote"
    );
    assert!(
        !dispatch_due_details(&mut app, &mut runtime),
        "the details are current, so nothing is asked for again"
    );
}

#[test]
fn view_changes_are_published_to_the_agent_context_file() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let repository = seeded_repository(&path);
    let mut app = App::new(repository.load_all().unwrap());
    app.shell
        .configure_database(path.clone(), db::data_signature(&path));
    app.work_items.set_table_viewport(3);
    let mut publisher = AgentContextPublisher::new(&path);
    publisher.publish(&app).unwrap();

    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Down,
        crossterm::event::KeyModifiers::NONE,
    ));
    let expected = app.work_items.selected_ticket().unwrap().key.clone();
    publisher.publish(&app).unwrap();

    let context_path = agent_context::path_for(&path);
    let observed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&context_path).unwrap()).unwrap();
    assert_eq!(
        observed["work_items"]["selected_ticket"]["organization"],
        expected.organization
    );
    assert_eq!(observed["work_items"]["selected_ticket"]["id"], expected.id);
    assert_eq!(
        observed["work_items"]["tickets"]["visible_rows"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(observed["schema_version"], agent_context::SCHEMA_VERSION);
    assert!(
        observed["sync"]["offline"].as_bool().unwrap(),
        "a run with no worker says so"
    );
    assert!(observed["pending_edits"].as_array().unwrap().is_empty());

    // A view that has not moved is not republished, which is what keeps the
    // file quiet enough for a watcher to trust every write it sees.
    fs::remove_file(&context_path).unwrap();
    publisher.publish(&app).unwrap();
    assert!(
        !context_path.exists(),
        "nothing changed, so nothing was written"
    );
}
