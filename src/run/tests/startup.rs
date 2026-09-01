//! Opening the database: what the flags and the environment settle, and
//! what a database another project filled does to the run.

use super::*;

#[test]
fn the_stale_threshold_comes_from_the_flag_before_the_environment() {
    assert_eq!(
        resolve_stale_days(None, None).unwrap(),
        None,
        "with neither given, whatever the session remembers stands"
    );
    assert_eq!(resolve_stale_days(Some(7), None).unwrap(), Some(7));
    assert_eq!(
        resolve_stale_days(Some(7), Some("30".into())).unwrap(),
        Some(7),
        "the flag wins over TICKET_TUI_STALE_DAYS"
    );
    assert_eq!(
        resolve_stale_days(None, Some(" 30 ".into())).unwrap(),
        Some(30)
    );
    assert_eq!(
        resolve_stale_days(None, Some("   ".into())).unwrap(),
        None,
        "an empty variable is not an answer"
    );

    let error = resolve_stale_days(None, Some("a fortnight".into())).unwrap_err();
    assert!(
        format!("{error:#}").contains("TICKET_TUI_STALE_DAYS is not a number of days"),
        "{error:#}"
    );
}

#[test]
fn the_refresh_interval_comes_from_the_flag_before_the_environment() {
    assert_eq!(resolve_refresh(None, None).unwrap(), 60);
    assert_eq!(resolve_refresh(Some(5), None).unwrap(), 5);
    assert_eq!(
        resolve_refresh(Some(5), Some("300".into())).unwrap(),
        5,
        "the flag wins over TICKET_TUI_REFRESH"
    );
    assert_eq!(resolve_refresh(None, Some(" 300 ".into())).unwrap(), 300);
    assert_eq!(
        resolve_refresh(None, Some("   ".into())).unwrap(),
        60,
        "a blank variable is not a setting"
    );
    assert_eq!(
        resolve_refresh(Some(0), None).unwrap(),
        0,
        "the timer can still be turned off"
    );

    let error = resolve_refresh(None, Some("hourly".into())).unwrap_err();
    assert!(
        format!("{error:#}").contains("TICKET_TUI_REFRESH is not a number of seconds"),
        "{error:#}"
    );
}

#[test]
fn a_database_another_project_filled_is_browsed_rather_than_replaced() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let mut repository = seeded_repository(&path);
    repository
        .set_meta(db::ORGANIZATION_KEY, "other-org")
        .unwrap();
    repository.set_meta(db::PROJECT_KEY, "borealis").unwrap();
    let stored = repository
        .meta(db::ORGANIZATION_KEY)
        .unwrap()
        .zip(repository.meta(db::PROJECT_KEY).unwrap())
        .expect("a pull records the project it ran under");
    let stored = (stored.0.as_str(), stored.1.as_str());
    let config = AzureConfig {
        organization: "example-org".into(),
        project: "atlas".into(),
        code_project: "atlas".into(),
        scope: None,
    };

    let message = project_mismatch(Some(stored), &config, false)
        .expect("another project's rows are not replaced by accident");
    assert_eq!(
        message,
        "Database holds other-org/borealis; pass --database for another project or run `ticket-tui sync --full` to replace it"
    );
    assert_eq!(
        project_mismatch(Some(stored), &config, true),
        None,
        "a database with nothing in it belongs to nobody"
    );
    assert_eq!(
        project_mismatch(Some(("example-org", "atlas")), &config, false),
        None
    );
    assert_eq!(
        project_mismatch(None, &config, false),
        None,
        "a database from a build that recorded nothing adopts the project that pulls it"
    );

    // The run that finds one opens offline: no worker, and the reason both
    // in the overlay and under the sync key.
    let mut app = App::new(repository.load_all().unwrap());
    app.shell.set_offline_reason(Some(message.clone()));
    app.shell.set_sync_source(Some(sync_source(&config, 60)));
    let mut runtime = SyncRuntime {
        worker: None,
        scheduler: SyncScheduler::new(None),
        config: Some(config),
        offline_reason: Some(message.clone()),
        details: DetailsEngine::default(),
        pipelines: None,
        watching_tab: false,
        watching_run: (None, None),
        watched_runs: Vec::new(),
        approvals_seen: None,
        local: LocalRuntime::default(),
        aks: AksRuntime::default(),
        arm: ArmRuntime::default(),
        arm_config: ArmConfig::default(),
    };

    handle_action(AppAction::Sync, &mut app, &mut runtime, &failing_opener);

    assert_eq!(
        app.shell.notification(),
        Some((message.as_str(), NotificationLevel::Error))
    );
    assert!(!app.shell.sync_pending, "there is no worker to pull with");
    assert_eq!(
        app.shell.sync_summary(),
        format!("example-org/atlas every 60s · offline; {message}"),
        "the database overlay says where the rows would come from and why they do not"
    );
}

#[test]
fn the_database_overlay_names_the_project_the_timer_and_the_scope() {
    let mut config = AzureConfig {
        organization: "example-org".into(),
        project: "atlas".into(),
        code_project: "atlas".into(),
        scope: None,
    };
    assert_eq!(sync_source(&config, 60), "example-org/atlas every 60s");
    assert_eq!(
        sync_source(&config, 0),
        "example-org/atlas on request",
        "--refresh 0 leaves r as the only way to pull"
    );

    config.code_project = "fiquants".into();
    assert_eq!(
        sync_source(&config, 60),
        "example-org/atlas every 60s · code fiquants",
        "the code project is named only when it is somewhere else"
    );
    config.code_project.clone_from(&config.project);

    config.scope = Some("[System.ChangedDate] > @today-180".into());
    assert_eq!(
        sync_source(&config, 300),
        "example-org/atlas every 300s · scope ([System.ChangedDate] > @today-180)"
    );

    let mut app = App::new(vec![ticket(1)]);
    app.shell.enable_sync();
    app.shell.set_sync_source(Some(sync_source(&config, 300)));
    assert_eq!(
        app.shell.sync_summary(),
        "example-org/atlas every 300s · scope ([System.ChangedDate] > @today-180) · not yet"
    );
}

#[test]
fn an_offline_run_explains_why_it_cannot_sync_and_says_nothing_in_the_title() {
    let mut app = App::new(Vec::new());
    let mut runtime = SyncRuntime {
        worker: None,
        scheduler: SyncScheduler::new(None),
        config: None,
        offline_reason: Some("no Azure DevOps organization; pass --org".into()),
        details: DetailsEngine::default(),
        pipelines: None,
        watching_tab: false,
        watching_run: (None, None),
        watched_runs: Vec::new(),
        approvals_seen: None,
        local: LocalRuntime::default(),
        aks: AksRuntime::default(),
        arm: ArmRuntime::default(),
        arm_config: ArmConfig::default(),
    };

    handle_action(AppAction::Sync, &mut app, &mut runtime, &failing_opener);

    let (message, level) = app
        .shell
        .notification()
        .expect("the sync key answers offline");
    assert!(message.contains("--org"), "{message}");
    assert_eq!(level, NotificationLevel::Error);
    assert!(!app.shell.sync_pending);
    assert_eq!(app.shell.sync_status(), SyncStatus::Offline);
    assert!(offline_status(true).contains("ticket-tui sync"));
    assert!(offline_status(false).contains("offline"));
}
