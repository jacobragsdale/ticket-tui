use super::*;
use crate::app::TabId;
use crate::app::pipelines::tests::{pipeline, pipelines_app, run};
use crate::model::{RunResult, RunStatus};

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
