use super::*;

#[test]
fn agent_context_describes_the_live_ticket_workspace() {
    let mut app = App::new(vec![
        ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Beta", "2026-02-01T00:00:00Z"),
        ticket(3, "Gamma", "2026-03-01T00:00:00Z"),
    ]);
    app.shell
        .configure_database(PathBuf::from("/tmp/tickets.sqlite3"), 0);
    app.work_items.set_table_viewport(2);
    app.work_items
        .set_query(&mut app.shell, "state:Active".into());
    app.work_items.toggle_row_selection();
    app.shell.focus = Focus::Details;
    app.work_items.mode = WorkItemMode::Filter;
    app.work_items.active_view = Some("Active work".into());

    let whole = app.agent_context();
    let context = &whole.work_items;

    assert_eq!(whole.database_path, "/tmp/tickets.sqlite3");
    assert_eq!(whole.active_tab, "work_items");
    assert_eq!(context.mode, "filter");
    assert_eq!(context.focus, "details");
    assert_eq!(context.active_view.as_deref(), Some("Active work"));
    assert_eq!(context.search.filters, vec!["state:Active"]);
    assert_eq!(context.tickets.total_count, 3);
    assert_eq!(context.tickets.matching_count, 3);
    assert_eq!(context.tickets.visible_rows.len(), 2);
    assert_eq!(context.selected_ticket.as_ref().unwrap().id, 3);
    assert!(context.selected_ticket.as_ref().unwrap().checked);
    assert_eq!(context.checked_tickets.len(), 1);
    assert_eq!(context.checked_tickets[0].id, 3);

    let mut mine = app.work_items.tickets()[0].clone();
    mine.assigned_to = Some("  avery CHEN ".into());
    let mut theirs = app.work_items.tickets()[1].clone();
    theirs.assigned_to = Some("Jordan Patel".into());
    let mut unassigned = app.work_items.tickets()[1].clone();
    unassigned.assigned_to = None;
    assert!(
        !app.shell.is_mine(&mine),
        "nobody is \"me\" until a name is set"
    );

    app.shell.set_me(Some("Avery Chen".into()));

    assert_eq!(app.shell.me(), Some("Avery Chen"));
    assert!(app.shell.is_mine(&mine), "casing and padding do not matter");
    assert!(!app.shell.is_mine(&theirs));
    assert!(!app.shell.is_mine(&unassigned));
    assert_eq!(app.agent_context().me.as_deref(), Some("Avery Chen"));
}

#[test]
fn the_agent_context_says_where_the_rows_come_from_and_how_the_last_pull_went() {
    let mut app = App::new(vec![ticket(1, "Alpha", "2026-01-01T00:00:00Z")]);

    let offline = app.agent_context().sync;
    assert!(offline.offline, "a run with no organization cannot sync");
    assert_eq!(offline.organization, None);
    assert_eq!(offline.project, None);
    assert_eq!(offline.refresh_seconds, 0);
    assert_eq!(offline.last_success_at, None);
    assert_eq!(offline.last_error, None);

    app.shell.enable_sync();
    app.shell.set_sync_target(Some(SyncTarget {
        organization: "example-org".into(),
        project: "atlas".into(),
        refresh_seconds: 60,
    }));
    app.shell.begin_sync();

    let running = app.agent_context().sync;
    assert!(!running.offline);
    assert_eq!(running.organization.as_deref(), Some("example-org"));
    assert_eq!(running.project.as_deref(), Some("atlas"));
    assert_eq!(running.refresh_seconds, 60);
    assert!(running.in_progress, "a pull is out");

    app.shell.finish_sync();

    let succeeded = app.agent_context().sync;
    assert!(!succeeded.in_progress);
    assert_eq!(succeeded.last_error, None);
    let landed = succeeded.last_success_at.expect("a pull landed");
    assert!(
        Timestamp::parse(&landed).is_ok(),
        "the last sync is RFC 3339: {landed}"
    );

    app.shell.begin_sync();
    app.shell.fail_sync("network unreachable", true);

    let failed = app.agent_context().sync;
    assert!(!failed.in_progress);
    assert_eq!(failed.last_error.as_deref(), Some("network unreachable"));
    assert_eq!(
        failed.last_success_at.as_deref(),
        Some(landed.as_str()),
        "a failure does not erase when the rows last arrived"
    );

    app.shell.finish_sync();
    assert_eq!(
        app.agent_context().sync.last_error,
        None,
        "the next success clears the error"
    );
}

#[test]
fn pending_edits_are_published_while_in_flight_and_gone_once_answered() {
    let mut app = editing_app();
    let request = edit_request(&mut app, FieldEdit::state("Doing"));

    let pending = app.agent_context().pending_edits;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, request.key.id);
    assert_eq!(pending[0].field, "State");
    assert_eq!(pending[0].value, "Doing");
    assert!(
        Timestamp::parse(&pending[0].since).is_ok(),
        "the dispatch time is RFC 3339: {}",
        pending[0].since
    );

    let applied = EditApplied {
        ticket: stored_copy(&app, &request.key, "Doing"),
        relations: Vec::new(),
        edit: request.edit,
    };
    app.work_items.apply_edit(&mut app.shell, applied);
    assert!(
        app.agent_context().pending_edits.is_empty(),
        "an edit that landed is no longer in flight"
    );

    let refused = edit_request(&mut app, FieldEdit::priority(1));
    assert_eq!(app.agent_context().pending_edits.len(), 1);

    app.work_items.reject_edit(
        &mut app.shell,
        &EditRejection {
            key: refused.key,
            label: "Priority".into(),
            conflict: false,
            message: "field is read only".into(),
        },
    );
    assert!(
        app.agent_context().pending_edits.is_empty(),
        "a refused edit is no longer in flight either"
    );
}

#[test]
fn the_context_lists_what_the_selected_work_item_was_worked_on_with() {
    use crate::model::{ArtifactKind, ArtifactLink, PrStatus, TicketGraph};

    let mut app = App::new(vec![ticket(1, "Alpha", "2026-01-01T00:00:00Z")]);
    let key = app.work_items.tickets()[0].key.clone();
    app.shell.set_repos(vec![crate::app::repos::tests::repo(
        "aaa-111", "atlas", false,
    )]);
    app.shell.set_artifact_labels(
        vec![(42, "Split the files".to_owned(), PrStatus::Active)],
        Vec::new(),
    );
    app.work_items.set_workspace_graph(
        &mut app.shell,
        TicketGraph {
            artifacts: vec![
                ArtifactLink {
                    work_item: key.clone(),
                    kind: ArtifactKind::PullRequest {
                        repo_id: "aaa-111".into(),
                        id: 42,
                    },
                    name: "Pull Request".into(),
                },
                ArtifactLink {
                    work_item: key,
                    kind: ArtifactKind::Build(14),
                    name: "Integrated in build".into(),
                },
            ],
            ..TicketGraph::default()
        },
    );

    let context = app.agent_context().work_items;
    let related = &context.selected_ticket.as_ref().unwrap().related;

    assert_eq!(related.len(), 2);
    assert_eq!(related[0].kind, "pull_request");
    assert_eq!(related[0].target, "42");
    assert_eq!(related[0].repo.as_deref(), Some("atlas"));
    assert!(
        related[0].in_database,
        "an agent is told what it can ask this app for"
    );
    assert!(
        !related[1].in_database,
        "and what it cannot: no run 14 is on file"
    );
    assert!(
        context
            .checked_tickets
            .iter()
            .all(|ticket| ticket.related.is_empty()),
        "the list of checked work items stays a list"
    );
}

#[test]
fn the_context_describes_every_tab_and_says_which_one_is_showing() {
    use crate::app::pipelines::tests::pipelines_app;
    use crate::model::RunStatus;

    // The Pipelines fixture already holds two definitions and four runs; the
    // other tabs are filled from theirs.
    let mut app = pipelines_app();
    app.repos.set_repos(&app.shell);
    app.repos.set_local(vec![(
        "aaa-111".to_owned(),
        crate::app::repos::tests::local("main", true, 0, 2),
    )]);
    app.shell
        .set_workspace(Some("/Users/jacob/Development".into()));
    let requests = vec![crate::app::pull_requests::tests::pull_request(
        11,
        "Split the files",
        "Avery",
        crate::model::PrStatus::Active,
    )];
    let shell = &app.shell;
    app.pull_requests.set_pull_requests(requests, shell);

    let context = app.agent_context();

    assert_eq!(
        context.active_tab, "work_items",
        "a fresh app opens where it always did"
    );

    // Repos.
    assert_eq!(
        context.repos.workspace.as_deref(),
        Some("/Users/jacob/Development")
    );
    let repo = context.repos.selected.as_ref().expect("a repository");
    assert_eq!(repo.name, "ticket-tui");
    assert_eq!(repo.default_branch, "main");
    let local = repo.local.as_ref().expect("the clone on this machine");
    assert!(local.dirty);
    assert_eq!(local.behind, 2);
    assert!(local.busy.is_none());

    // Pull requests.
    let request = context
        .pull_requests
        .selected
        .as_ref()
        .expect("a pull request");
    assert_eq!(request.row.id, 11);
    assert_eq!(request.row.status, "active");
    assert_eq!(request.row.target_branch, "main");
    assert_eq!(context.pull_requests.visible_rows.len(), 1);
    assert!(!context.pull_requests.closed_shown);

    // Pipelines: the tab is not showing and is described anyway.
    assert_eq!(context.pipelines.level, "pipelines");
    assert_eq!(
        context
            .pipelines
            .selected_pipeline
            .as_ref()
            .map(|pipeline| pipeline.name.clone()),
        Some("ticket-tui CI".to_owned()),
        "the cursor opens on the first row"
    );
    assert_eq!(
        context.pipelines.running, 1,
        "one of the four runs is still going"
    );
    assert!(context.pipelines.watched.is_empty());

    // And moving to a tab is the one thing active_tab says.
    app.select_tab(TabId::Pipelines);
    app.pipelines.open_runs(&app.shell);
    let context = app.agent_context();
    assert_eq!(context.active_tab, "pipelines");
    assert_eq!(context.pipelines.level, "runs");
    let run = context.pipelines.selected_run.as_ref().expect("a run");
    assert_eq!(run.status, RunStatus::InProgress.as_str());
    assert_eq!(run.branch, "main");
}
