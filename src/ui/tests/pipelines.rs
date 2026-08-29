use super::*;
use crate::app::TabId;
use crate::app::pipelines::tests::{pipeline, pipelines_app, run};
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
        text.contains("ticket-tui CI \u{00b7} 3 runs"),
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
    let text = pipelines_text(140, 30, &mut app);

    assert!(text.contains("20260829.14"), "{text}");
    assert!(text.contains("Branch"), "{text}");
    assert!(text.contains("main"), "{text}");
    assert!(text.contains("abc1234d"), "the commit is shortened: {text}");
    assert!(text.contains("Jacob Ragsdale"), "{text}");
    assert!(text.contains("Queued"), "{text}");
    assert!(
        text.contains("[Cancel]") && text.contains("[Retry]"),
        "the controls are drawn, muted until #687: {text}"
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
    render_text(140, 34, &mut app);
    let run = app
        .pipelines
        .focused_run()
        .expect("the details pane settles on a run");
    app.pipelines.set_timeline(run, timeline_fixture());

    let text = render_text(140, 34, &mut app);
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
    render_text(140, 34, &mut app);
    let run = app.pipelines.focused_run().expect("a run");
    app.pipelines.set_timeline(run, timeline_fixture());
    render_text(140, 34, &mut app);

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
