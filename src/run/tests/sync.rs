//! Pulling from Azure DevOps: the scheduled pull, the reload it triggers,
//! and every way one can fail.

use super::*;

#[test]
fn reload_engine_loads_and_prepares_tickets_in_the_background() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    drop(seeded_repository(&path));
    let mut reloader = ReloadEngine::default();

    assert!(reloader.start(&path).unwrap());
    assert!(!reloader.start(&path).unwrap());

    let deadline = Instant::now() + Duration::from_secs(2);
    let snapshot = loop {
        if let Some(result) = reloader.try_result() {
            break result.unwrap();
        }
        assert!(Instant::now() < deadline, "reload worker timed out");
        thread::yield_now();
    };
    assert_eq!(snapshot.ticket_count(), 3);
}

#[test]
fn a_scheduled_pull_replaces_the_tickets_and_the_table_title_follows_it() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let (mut app, mut repository, mut runtime) =
        synced_app(&path, FakeAzure::returning(vec![ticket(9)]));
    runtime.scheduler.schedule_now(Instant::now());

    assert!(dispatch_due_pull(&mut app, &mut runtime));
    assert_eq!(app.shell.sync_status(), SyncStatus::Syncing);
    assert!(
        !dispatch_due_pull(&mut app, &mut runtime),
        "the timer never queues a second pull behind one in flight"
    );

    await_sync(&mut app, &mut repository, &mut runtime);
    assert_eq!(app.work_items.tickets().len(), 1);
    assert_eq!(app.work_items.tickets()[0].key.id, 9);
    assert_eq!(app.shell.sync_status().label(), "Synced just now");
    assert!(
        app.shell.notification().is_none(),
        "a timer pull says so in the title, not in a toast"
    );
    assert!(
        !poll_watch(&mut app, &repository, &mut ReloadEngine::default()),
        "the watcher does not chase the database our own worker just wrote"
    );
}

#[test]
fn a_pull_that_finds_nothing_says_so_and_reloads_nothing() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let (mut app, mut repository, mut runtime) =
        synced_app(&path, FakeAzure::quiet((1..=3).map(ticket).collect()));
    // The watermark an earlier pull left is what makes this one incremental.
    repository
        .set_meta(db::WATERMARK_KEY, "2026-01-01T00:00:00Z")
        .unwrap();
    app.shell
        .configure_database(path.clone(), db::data_signature(&path));

    start_sync(&mut app, &mut runtime);
    await_sync(&mut app, &mut repository, &mut runtime);

    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Nothing changed")
    );
    assert_eq!(
        app.shell.sync_status().label(),
        "Synced just now",
        "the pull still happened, so the status bar moves"
    );
    assert_eq!(
        app.work_items.tickets().len(),
        3,
        "the rows were never replaced"
    );
    assert!(
        !poll_watch(&mut app, &repository, &mut ReloadEngine::default()),
        "an unchanged pull writes nothing, so there is nothing to reload"
    );

    assert_eq!(
        runtime.status_for(SyncMode::Incremental, 1, PulledExtras::default()),
        "Synced 1 change from example-org/atlas"
    );
    assert_eq!(
        runtime.status_for(SyncMode::Incremental, 3, PulledExtras::default()),
        "Synced 3 changes from example-org/atlas"
    );
    assert_eq!(
        runtime.status_for(
            SyncMode::Full,
            52,
            PulledExtras {
                repos: 4,
                pipelines: 1,
                runs: 137,
                pull_requests: 2,
            }
        ),
        "Synced 52 work items, 4 repos, 1 pipeline, 137 runs, 2 pull requests from example-org/atlas",
        "a full pull counts everything it brought down with the rows"
    );
}

#[test]
fn a_failed_pull_keeps_the_tickets_and_reports_the_same_error_once() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let (mut app, mut repository, mut runtime) =
        synced_app(&path, FakeAzure::failing("network unreachable"));
    runtime.scheduler.schedule_now(Instant::now());

    dispatch_due_pull(&mut app, &mut runtime);
    await_sync(&mut app, &mut repository, &mut runtime);

    assert_eq!(
        app.work_items.tickets().len(),
        3,
        "a failed pull changes nothing"
    );
    let (message, level) = app
        .shell
        .notification()
        .expect("the first failure is reported");
    assert!(message.contains("network unreachable"), "{message}");
    assert_eq!(level, NotificationLevel::Error);
    assert_eq!(app.shell.sync_status(), SyncStatus::Failed);

    app.shell.set_status("still browsing");
    runtime.scheduler.schedule_now(Instant::now());
    dispatch_due_pull(&mut app, &mut runtime);
    await_sync(&mut app, &mut repository, &mut runtime);
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("still browsing"),
        "the same timer failure is not raised again"
    );
    assert_eq!(app.shell.sync_status(), SyncStatus::Failed);
}

#[test]
fn a_throttled_pull_pauses_the_timer_and_says_so_instead_of_failing() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let (mut app, mut repository, mut runtime) =
        synced_app(&path, FakeAzure::throttling(Duration::from_secs(120)));
    app.shell.set_status("still browsing");
    let start = Instant::now();
    runtime.scheduler.schedule_now(start);

    dispatch_due_pull(&mut app, &mut runtime);
    await_sync(&mut app, &mut repository, &mut runtime);

    assert_eq!(
        app.work_items.tickets().len(),
        3,
        "a throttled pull changes nothing"
    );
    assert_eq!(app.shell.sync_status().label(), "Sync paused 2m");
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("still browsing"),
        "throttling is the service working, not an error to toast"
    );
    let summary = app.shell.sync_summary();
    assert!(summary.contains("paused for throttling"), "{summary}");
    assert!(summary.contains("next in 2m"), "{summary}");

    assert!(
        !runtime.scheduler.due(start + Duration::from_secs(119)),
        "the next pull waits out the header value"
    );
    assert!(runtime.scheduler.due(start + Duration::from_secs(121)));
}

#[test]
fn a_second_sync_keypress_is_reported_rather_than_queued() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let (mut app, _repository, mut runtime) =
        synced_app(&path, FakeAzure::returning(vec![ticket(9)]));

    start_sync(&mut app, &mut runtime);
    assert!(app.shell.sync_pending);
    assert!(runtime.scheduler.in_flight());

    start_sync(&mut app, &mut runtime);
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Sync already in progress")
    );
}

#[test]
fn the_watcher_reloads_another_writer_but_never_our_own_sync() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let repository = seeded_repository(&path);
    let mut app = App::new(repository.load_all().unwrap());
    app.shell
        .configure_database(path.clone(), db::data_signature(&path));
    let mut reloader = ReloadEngine::default();
    assert!(!poll_watch(&mut app, &repository, &mut reloader));

    let write = |tickets: &[Ticket]| {
        SqliteTicketRepository::open_existing(&path)
            .unwrap()
            .replace_all(tickets, &TicketGraph::default())
            .unwrap();
    };

    app.shell.sync_pending = true;
    write(&[ticket(4)]);
    assert!(
        !poll_watch(&mut app, &repository, &mut reloader),
        "a pull in flight is writing the database itself"
    );

    app.shell.sync_pending = false;
    app.shell
        .configure_database(path.clone(), db::data_signature(&path));
    assert!(
        !poll_watch(&mut app, &repository, &mut reloader),
        "applying the pull records the signature it wrote"
    );

    write(&[ticket(5), ticket(6)]);
    assert!(
        poll_watch(&mut app, &repository, &mut reloader),
        "another process writing the database still reloads"
    );
    assert!(app.shell.reload_pending);
}
