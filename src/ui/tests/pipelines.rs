use super::*;
use crate::app::pipelines::Level;
use crate::app::pipelines::tests::{pipeline, pipelines_app, run};
use crate::app::{Jump, Screen, TabId};
use crate::model::{RunResult, RunStatus, TimelineKind, TimelineRecord};

/// The tab, drawn, with the Pipelines tab showing.
fn pipelines_text(width: u16, height: u16, app: &mut App) -> String {
    app.select_tab(TabId::Pipelines);
    render_text(width, height, app)
}

#[test]
fn the_pipelines_level_draws_a_row_per_definition_with_its_last_run() {
    let mut app = pipelines_app();
    let text = pipelines_text(120, 24, &mut app);

    assert!(
        text.contains("Pipeline"),
        "the header names the columns: {text}"
    );
    assert!(text.contains("Last run"), "{text}");
    assert!(text.contains("ticket-tui CI"), "{text}");
    assert!(text.contains("nightly"), "{text}");
    assert!(
        text.contains("\u{25d0} 20260829.14"),
        "a running last run is a half-circle and its build number: {text}"
    );
    assert!(
        text.contains("\u{2298} 20260829.11"),
        "and a canceled one carries the canceled glyph: {text}"
    );
    assert!(
        text.contains("scheduled"),
        "the folder is its own column: {text}"
    );
}

#[test]
fn the_runs_level_draws_the_runs_of_the_pipeline_that_was_opened() {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);
    app.pipelines.pipeline_cursor.focus(
        app.pipelines
            .visible_pipelines(&app.shell)
            .iter()
            .position(|row| row.pipeline.name == "ticket-tui CI")
            .expect("the CI pipeline"),
    );
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let text = render_text(170, 24, &mut app);

    assert!(
        pane_reads(&text, "ticket-tui CI", "3 runs"),
        "the title says whose runs these are: {text}"
    );
    assert!(text.contains("Result"), "{text}");
    assert!(text.contains("Running"), "{text}");
    assert!(text.contains("Failed"), "{text}");
    assert!(
        text.contains("4m 12s"),
        "a finished run reports how long it took: {text}"
    );
    assert!(
        !text.contains("20260829.11"),
        "and the other pipeline's runs are not on it: {text}"
    );
}

#[test]
fn the_details_pane_heads_a_run_with_what_it_was_and_where_it_came_from() {
    let mut app = pipelines_app();
    let text = pipelines_text(140, 44, &mut app);

    assert!(text.contains("20260829.14"), "{text}");
    assert!(text.contains("Branch"), "{text}");
    assert!(text.contains("main"), "{text}");
    assert!(text.contains("abc1234d"), "the commit is shortened: {text}");
    assert!(text.contains("Jacob Ragsdale"), "{text}");
    assert!(text.contains("Queued"), "{text}");
    assert!(
        text.contains(" Cancel ") && text.contains(" Retry "),
        "the controls are drawn as chips: {text}"
    );
}

#[test]
fn a_click_moves_the_cursor_and_a_header_click_turns_the_sort_around() {
    let mut app = pipelines_app();
    pipelines_text(120, 24, &mut app);

    let row = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::TableRow { index: 1 }))
        .expect("every row on screen is clickable")
        .rect;
    click(&mut app, row.x + 4, row.y);
    assert_eq!(app.pipelines.pipeline_cursor.index, 1);

    let header = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::SortHeader("name")))
        .expect("the header sorts")
        .rect;
    click(&mut app, header.x, header.y);
    assert_eq!(
        app.pipelines
            .visible_pipelines(&app.shell)
            .first()
            .map(|row| row.pipeline.name.clone()),
        Some("ticket-tui CI".to_owned()),
        "descending by name puts the CI pipeline first"
    );
    click(&mut app, header.x, header.y);
    assert_eq!(
        app.pipelines
            .visible_pipelines(&app.shell)
            .first()
            .map(|row| row.pipeline.name.clone()),
        Some("nightly".to_owned()),
        "and clicking it again turns it around"
    );
}

#[test]
fn a_running_row_reports_its_elapsed_time_and_a_search_narrows_the_list() {
    let mut app = App::new(Vec::new());
    let shell = &app.shell;
    app.pipelines.set_pipelines(
        vec![
            pipeline(1, "ticket-tui CI", "\\"),
            pipeline(2, "nightly", "\\"),
        ],
        vec![run(14, 1, RunStatus::InProgress, None)],
        shell,
    );
    let text = pipelines_text(120, 24, &mut app);
    assert!(
        text.contains("\u{25d0} 20260829.14 \u{00b7} "),
        "a running row carries the time it has been going: {text}"
    );

    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    for character in "nightly".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    let text = render_text(120, 24, &mut app);
    assert!(text.contains("nightly"), "{text}");
    assert!(
        !text.contains("ticket-tui CI"),
        "the search box filters this list the way it filters work items: {text}"
    );
}

#[test]
fn a_canceled_run_fades_and_a_failed_one_is_painted_red() {
    let mut app = App::new(Vec::new());
    let shell = &app.shell;
    app.pipelines.set_pipelines(
        vec![pipeline(1, "ticket-tui CI", "\\")],
        vec![
            run(13, 1, RunStatus::Completed, Some(RunResult::Failed)),
            run(11, 1, RunStatus::Completed, Some(RunResult::Canceled)),
        ],
        shell,
    );
    app.select_tab(TabId::Pipelines);
    app.pipelines.pipeline_cursor.focus(0);
    app.pipelines.open_runs(&app.shell);

    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut failed = None;
    let mut canceled = None;
    for y in 0..20 {
        for x in 0..120 {
            match buffer[(x, y)].symbol() {
                "\u{2717}" => failed = Some(buffer[(x, y)].style()),
                "\u{2298}" => canceled = Some(buffer[(x, y)].style()),
                _ => {}
            }
        }
    }
    let failed = failed.expect("the failed run carries its glyph");
    let canceled = canceled.expect("and so does the canceled one");
    // The glyphs are what say which is which; the colours only add to it, and
    // under NO_COLOR there are none to add.
    if failed.fg != Some(Color::Reset) {
        assert_ne!(
            failed.fg, canceled.fg,
            "a failed run and a canceled one do not read the same"
        );
    }
}

fn timeline_fixture() -> Vec<TimelineRecord> {
    let at = |raw: &str| Some(crate::timestamp::ts(raw));
    vec![
        TimelineRecord {
            id: "stage-1".into(),
            parent_id: None,
            kind: TimelineKind::Stage,
            name: "Build".into(),
            state: RunStatus::Completed,
            result: Some(RunResult::Succeeded),
            start: at("2026-08-29T10:00:05Z"),
            finish: at("2026-08-29T10:02:05Z"),
            percent_complete: None,
            log_id: None,
            order: 1,
            issues: Vec::new(),
        },
        TimelineRecord {
            id: "job-1".into(),
            parent_id: Some("stage-1".into()),
            kind: TimelineKind::Job,
            name: "cargo test".into(),
            state: RunStatus::Completed,
            result: Some(RunResult::Succeeded),
            start: at("2026-08-29T10:00:05Z"),
            finish: at("2026-08-29T10:02:05Z"),
            percent_complete: None,
            log_id: Some(7),
            order: 2,
            issues: Vec::new(),
        },
        TimelineRecord {
            id: "task-1".into(),
            parent_id: Some("job-1".into()),
            kind: TimelineKind::Task,
            name: "Checkout".into(),
            state: RunStatus::Completed,
            result: Some(RunResult::Failed),
            start: at("2026-08-29T10:00:05Z"),
            finish: at("2026-08-29T10:00:20Z"),
            percent_complete: None,
            log_id: Some(8),
            order: 3,
            issues: vec![
                crate::model::Issue {
                    kind: "error".into(),
                    message: "fatal: could not read from remote".into(),
                },
                crate::model::Issue {
                    kind: "warning".into(),
                    message: "shallow clone".into(),
                },
            ],
        },
        TimelineRecord {
            id: "stage-2".into(),
            parent_id: None,
            kind: TimelineKind::Stage,
            name: "Publish".into(),
            state: RunStatus::InProgress,
            result: None,
            start: at("2026-08-29T10:02:05Z"),
            finish: None,
            percent_complete: Some(42),
            log_id: None,
            order: 4,
            issues: Vec::new(),
        },
        TimelineRecord {
            id: "stage-3".into(),
            parent_id: None,
            kind: TimelineKind::Stage,
            name: "Deploy".into(),
            state: RunStatus::NotStarted,
            result: None,
            start: None,
            finish: None,
            percent_complete: None,
            log_id: None,
            order: 5,
            issues: Vec::new(),
        },
    ]
}

#[test]
fn the_details_pane_draws_the_timeline_as_a_tree_with_a_glyph_and_a_duration_each() {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);
    render_text(140, 60, &mut app);
    let run = app
        .pipelines
        .focused_run()
        .expect("the details pane settles on a run");
    app.pipelines.set_timeline(run, timeline_fixture());

    let text = render_text(140, 60, &mut app);
    assert!(text.contains("Timeline"), "{text}");
    assert!(
        text.contains("\u{2713} Build"),
        "a finished stage is a tick: {text}"
    );
    assert!(
        text.contains("  \u{2713} cargo test"),
        "its job is indented under it: {text}"
    );
    assert!(
        text.contains("    \u{2717} Checkout"),
        "and the task under that, failed: {text}"
    );
    assert!(
        text.contains("\u{2717} 1"),
        "the failing task says how many errors it reported: {text}"
    );
    assert!(
        text.contains("\u{25d0} Publish") && text.contains("42%"),
        "a running stage carries its glyph and how far it says it has got: {text}"
    );
    assert!(
        text.contains("\u{25cb} Deploy") && text.contains("\u{2014}"),
        "and one that has not started has no duration to report: {text}"
    );
    assert!(
        text.contains("2m 00s"),
        "a finished node reports its own: {text}"
    );
}

#[test]
fn the_timeline_cursor_moves_with_the_keyboard_and_a_click() {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);
    render_text(140, 60, &mut app);
    let run = app.pipelines.focused_run().expect("a run");
    app.pipelines.set_timeline(run, timeline_fixture());
    render_text(140, 60, &mut app);

    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.shell.focus, Focus::Details);
    app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    assert_eq!(app.pipelines.timeline_cursor(), 2);

    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
    assert_eq!(app.pipelines.timeline_cursor(), 1);

    let node = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::TreeRow { index: 3 }))
        .expect("every node on screen is clickable")
        .rect;
    click(&mut app, node.x + 2, node.y);
    assert_eq!(app.pipelines.timeline_cursor(), 3);
}

/// A run on screen with a timeline whose second node has a log.
fn logging_app() -> App {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);
    render_text(140, 60, &mut app);
    let run = app.pipelines.focused_run().expect("a run");
    app.pipelines.set_timeline(run, timeline_fixture());
    app
}

#[test]
fn the_log_pane_paints_every_marker_and_says_what_it_is_following() {
    let mut app = logging_app();
    let run = app.pipelines.focused_run().expect("a run");
    app.pipelines.append_log(
        run,
        7,
        0,
        vec![
            "2026-08-29T10:00:06.1234567Z ##[section]Starting: cargo test".to_owned(),
            "2026-08-29T10:00:07.1234567Z ##[group]Environment".to_owned(),
            "2026-08-29T10:00:08.1234567Z ##[command]cargo test --all-targets".to_owned(),
            "2026-08-29T10:00:09.1234567Z ##[warning]unused variable".to_owned(),
            "2026-08-29T10:00:10.1234567Z ##[error]test failed".to_owned(),
            "2026-08-29T10:00:11.1234567Z ##[debug]inner detail".to_owned(),
            "2026-08-29T10:00:12.1234567Z plain output".to_owned(),
        ],
        false,
    );
    // The tree cursor is on the job, whose log this is.
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));

    let text = render_text(140, 60, &mut app);
    assert!(
        text.contains("Log \u{00b7} cargo test \u{00b7} 7 lines \u{00b7} ")
            && text.contains(" following "),
        "the title names the node, the size and the state: {text}"
    );
    assert!(text.contains("Starting: cargo test"), "{text}");
    assert!(
        text.contains("\u{25b8} Environment"),
        "a group carries its marker: {text}"
    );
    assert!(text.contains("test failed"), "{text}");
    assert!(text.contains("plain output"), "{text}");
    assert!(
        text.contains("10:00:06"),
        "the timestamp is kept, dimmed: {text}"
    );
    assert!(
        !text.contains("##["),
        "and the markers themselves are painted, not printed: {text}"
    );
}

#[test]
fn scrolling_the_log_leaves_follow_mode_and_end_goes_back_to_it() {
    let mut app = logging_app();
    let run = app.pipelines.focused_run().expect("a run");
    let lines: Vec<String> = (1..=200).map(|line| format!("line {line}")).collect();
    app.pipelines.append_log(run, 7, 0, lines, false);
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));

    let text = render_text(140, 30, &mut app);
    assert!(
        text.contains("line 200"),
        "following shows the tail: {text}"
    );
    assert!(text.contains("following"), "{text}");

    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    let text = render_text(140, 30, &mut app);
    assert!(
        text.contains("scrolled"),
        "scrolling up by hand leaves follow mode: {text}"
    );
    assert!(!text.contains("line 200"), "{text}");

    app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    let text = render_text(140, 30, &mut app);
    assert!(
        text.contains("following") && text.contains("line 200"),
        "{text}"
    );
}

#[test]
fn a_log_past_the_cap_keeps_the_tail_and_says_how_much_it_dropped() {
    let mut app = logging_app();
    let run = app.pipelines.focused_run().expect("a run");
    let lines: Vec<String> = (1..=20_010).map(|line| format!("line {line}")).collect();
    app.pipelines.append_log(run, 7, 0, lines, true);

    let held = app.pipelines.log(run, 7);
    assert_eq!(held.len(), 20_000, "the cap holds");
    assert!(
        held[0].contains("earlier lines skipped"),
        "and says what went: {}",
        held[0]
    );
    assert_eq!(
        held.last().unwrap(),
        "line 20010",
        "the tail is what is kept"
    );
}

#[test]
fn w_watches_the_run_under_the_cursor_and_the_marker_says_so() {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);
    render_text(140, 30, &mut app);

    app.handle_key(KeyEvent::new(KeyCode::Char('W'), KeyModifiers::SHIFT));
    let run = app
        .pipelines
        .visible_pipelines(&app.shell)
        .first()
        .and_then(|row| row.last_run.as_ref())
        .map(|run| run.id)
        .expect("the row under the cursor has a run");
    assert!(app.pipelines.is_watched(run));
    assert_eq!(app.pipelines.watched_runs(), vec![run]);

    let text = render_text(140, 30, &mut app);
    assert!(
        text.contains("\u{25c9}"),
        "a watched row wears its marker: {text}"
    );

    app.handle_key(KeyEvent::new(KeyCode::Char('W'), KeyModifiers::SHIFT));
    assert!(!app.pipelines.is_watched(run), "and W again lets it go");
}

#[test]
fn a_watched_run_finishing_is_reported_while_another_tab_is_showing() {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);
    render_text(140, 30, &mut app);
    app.handle_key(KeyEvent::new(KeyCode::Char('W'), KeyModifiers::SHIFT));
    app.select_tab(TabId::WorkItems);

    // What main.rs does with a RunFinished event, which is what the watcher
    // sends once a watched run leaves the live list.
    let mut finished = run(14, 1, RunStatus::Completed, Some(RunResult::Succeeded));
    finished.finish_time = Some(crate::timestamp::ts("2026-08-29T10:04:17Z"));
    app.shell
        .set_status("\u{2713} Build 20260829.14 succeeded \u{00b7} 4m 12s".to_owned());
    app.pipelines.unwatch_run(finished.id);
    let shell = &app.shell;
    app.pipelines.merge_live_runs(vec![finished], shell);

    let (message, level) = app.shell.notification().expect("the toast is up");
    assert_eq!(
        message,
        "\u{2713} Build 20260829.14 succeeded \u{00b7} 4m 12s"
    );
    assert_eq!(level, crate::app::NotificationLevel::Info);
    assert_eq!(app.tab, TabId::WorkItems, "wherever the user is");
    assert!(!app.pipelines.is_watched(14), "and the watch is spent");
    assert_eq!(
        Screen::badge(&app.pipelines),
        None,
        "the badge goes with the last running run"
    );
}

#[test]
fn t_opens_a_branch_picker_and_enter_starts_the_pipeline_on_what_it_names() {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);
    render_text(140, 30, &mut app);

    let action = app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
    assert!(
        matches!(action, crate::app::AppAction::FetchBranches(repo) if repo == "aaa-111"),
        "the picker asks for the repository's branches"
    );
    let text = render_text(140, 30, &mut app);
    assert!(text.contains("Run on branch"), "{text}");
    assert!(
        text.contains("main"),
        "it opens on the default branch at once: {text}"
    );

    app.pipelines.set_branches(
        "aaa-111",
        vec![
            "develop".to_owned(),
            "main".to_owned(),
            "release".to_owned(),
        ],
    );
    let text = render_text(140, 30, &mut app);
    assert!(
        text.contains("develop") && text.contains("release"),
        "{text}"
    );

    for character in "rel".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    assert_eq!(app.pipelines.branch_matches(), vec!["release".to_owned()]);

    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(
            action,
            crate::app::AppAction::TriggerRun { pipeline_id, ref branch }
                if pipeline_id == 1 && branch == "release"
        ),
        "and Enter starts that pipeline on that branch, got {action:?}"
    );
}

#[test]
fn the_run_azure_devops_starts_is_selected_focused_and_watched() {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);
    let started = run(20, 1, RunStatus::InProgress, None);

    app.pipelines.accept_run(&mut app.shell, started);

    assert_eq!(app.pipelines.level(), Level::Runs(1), "its runs come up");
    assert_eq!(
        app.pipelines.selected_run(&app.shell).map(|row| row.run.id),
        Some(20),
        "with the new run under the cursor"
    );
    assert_eq!(app.pipelines.focused_run(), Some(20), "and focused");
    assert!(app.pipelines.is_watched(20), "and watched");
}

#[test]
fn x_asks_before_cancelling_and_r_retries_a_run_that_has_stopped() {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);
    app.pipelines.pipeline_cursor.focus(0);
    app.pipelines.open_runs(&app.shell);
    render_text(140, 30, &mut app);

    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    let text = render_text(140, 30, &mut app);
    assert!(text.contains("Cancel 20260829.14?"), "{text}");
    assert!(text.contains("x again"), "{text}");

    let action = app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert!(
        matches!(
            action,
            crate::app::AppAction::RunAction {
                run_id: 14,
                retry: false
            }
        ),
        "x again cancels it, got {action:?}"
    );

    // The second run in the list has failed, so it is the one Retry is for.
    app.pipelines.run_cursor.focus(1);
    let action = app.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
    assert!(
        matches!(
            action,
            crate::app::AppAction::RunAction {
                run_id: 13,
                retry: true
            }
        ),
        "R retries the failed run, got {action:?}"
    );

    // And neither is offered where it makes no sense.
    app.pipelines.run_cursor.focus(0);
    let action = app.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
    assert!(matches!(action, crate::app::AppAction::None));
    assert!(
        app.shell
            .notification()
            .is_some_and(|(message, _)| message.contains("still going")),
        "a run still going cannot be retried"
    );
}

fn approval(id: &str, stage: &str) -> crate::model::Approval {
    crate::model::Approval {
        id: id.to_owned(),
        pipeline: "ticket-tui CI".to_owned(),
        run_id: Some(14),
        build_number: "20260829.14".to_owned(),
        stage: stage.to_owned(),
        instructions: "Check the release notes".to_owned(),
        requested_at: Some(crate::timestamp::ts("2026-08-29T10:02:00Z")),
    }
}

#[test]
fn a_opens_the_approvals_waiting_and_answers_one_with_a_word() {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);
    app.pipelines
        .set_approvals(vec![approval("approval-1", "Deploy")]);

    let action = app.handle_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
    assert!(
        matches!(action, crate::app::AppAction::RefreshApprovals),
        "opening asks for a fresh read rather than waiting out the minute"
    );
    let text = render_text(140, 30, &mut app);
    assert!(text.contains("Approvals"), "{text}");
    assert!(text.contains("20260829.14"), "{text}");
    assert!(text.contains("Deploy"), "{text}");
    assert!(
        text.contains("Check the release notes"),
        "the instructions are shown for the one under the cursor: {text}"
    );

    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    let text = render_text(140, 30, &mut app);
    assert!(text.contains("Approve"), "the comment prompt opens: {text}");
    for character in "looks fine".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(
            action,
            crate::app::AppAction::AnswerApproval { ref id, approve: true, ref comment }
                if id == "approval-1" && comment == "looks fine"
        ),
        "and Enter sends the answer, got {action:?}"
    );

    app.pipelines.approval_answered("approval-1");
    assert!(
        app.pipelines.approvals().is_empty(),
        "the answered one goes"
    );
}

#[test]
fn the_tab_badge_counts_the_runs_going_and_the_approvals_waiting() {
    let mut app = pipelines_app();
    assert_eq!(Screen::badge(&app.pipelines), Some("\u{25d0}1".to_owned()));

    app.pipelines
        .set_approvals(vec![approval("approval-1", "Deploy")]);
    assert_eq!(
        Screen::badge(&app.pipelines),
        Some("\u{25d0}1 \u{25c7}1".to_owned()),
        "both, when both"
    );

    let mut quiet = App::new(Vec::new());
    quiet
        .pipelines
        .set_approvals(vec![approval("approval-2", "Deploy")]);
    assert_eq!(
        Screen::badge(&quiet.pipelines),
        Some("\u{25c7}1".to_owned())
    );
}

#[test]
fn x_in_the_approvals_overlay_rejects_rather_than_cancelling_a_run() {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);
    app.pipelines
        .set_approvals(vec![approval("approval-1", "Deploy")]);
    app.handle_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));

    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(
            action,
            crate::app::AppAction::AnswerApproval { approve: false, .. }
        ),
        "got {action:?}"
    );
}

#[test]
fn a_runs_details_follow_its_repository_its_pull_request_and_the_work_items_it_built() {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);
    // A run raised for a pull request, carrying two work items.
    let mut from_pr = run(15, 1, RunStatus::Completed, Some(RunResult::Succeeded));
    from_pr.pr_id = Some(42);
    from_pr.reason = "pullRequest".into();
    // The pull request the run was raised for has to be on file for the jump
    // to land on it, the way a work item does.
    let mut raised = crate::app::pull_requests::tests::pull_request(
        42,
        "The one this run is for",
        "Avery",
        crate::model::PrStatus::Active,
    );
    raised.repo_id = "aaa-111".into();
    let shell = &app.shell;
    app.pull_requests.set_pull_requests(vec![raised], shell);
    let shell = &app.shell;
    app.pipelines.merge_live_runs(vec![from_pr], shell);
    app.pipelines.pipeline_cursor.focus(0);
    app.pipelines.open_runs(&app.shell);
    app.pipelines.run_cursor.focus(0);
    render_text(140, 60, &mut app);
    app.pipelines.set_run_work_items(15, vec![10_001, 10_002]);
    let text = render_text(140, 60, &mut app);

    assert!(text.contains("Related"), "{text}");
    assert!(text.contains("Repository: ticket-tui"), "{text}");
    assert!(text.contains("Pull request: !42"), "{text}");
    assert!(text.contains("#10001 #10002"), "{text}");

    // The repository has to be on file for the jump to land on it.
    app.shell.set_repos(vec![crate::model::Repo {
        id: "aaa-111".into(),
        name: "ticket-tui".into(),
        project: "atlas".into(),
        default_branch: Some("refs/heads/main".into()),
        remote_url: String::new(),
        ssh_url: String::new(),
        web_url: String::new(),
        is_disabled: false,
        size: None,
    }]);
    app.repos.set_repos(&app.shell);
    // Following the repository takes the Repos tab, and `[` comes back.
    let repo = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::Follow(Jump::Repo(_))))
        .expect("the repository line is a jump")
        .rect;
    click(&mut app, repo.x + 12, repo.y);
    assert_eq!(app.tab, TabId::Repos);

    app.handle_key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE));
    let pull_request = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::Follow(Jump::PullRequest { .. })))
        .expect("the pull request line is a jump")
        .rect;
    click(&mut app, pull_request.x + 14, pull_request.y);
    assert_eq!(app.tab, TabId::PullRequests);

    app.handle_key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE));
    let work_items = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::Follow(Jump::WorkItems(_))))
        .expect("the work items line is a jump")
        .rect;
    click(&mut app, work_items.x + 14, work_items.y);
    assert_eq!(app.tab, TabId::WorkItems);
    assert_eq!(
        app.work_items.query(),
        "id:10001 id:10002",
        "the work items the run built are what the table is filtered to"
    );
}
