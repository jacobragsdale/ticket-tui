use super::*;
use crate::app::App;
use crate::model::{Pipeline, Repo, Run, RunResult, RunStatus};
use crate::timestamp::ts;

pub(crate) fn pipeline(id: i64, name: &str, folder: &str) -> Pipeline {
    Pipeline {
        id,
        name: name.to_owned(),
        folder: folder.to_owned(),
        repo_id: Some("aaa-111".into()),
        default_branch: Some("refs/heads/main".into()),
        url: format!("https://dev.azure.com/demo/atlas/_build?definitionId={id}"),
        queue_status: "enabled".into(),
    }
}

pub(crate) fn run(id: i64, pipeline_id: i64, status: RunStatus, result: Option<RunResult>) -> Run {
    Run {
        id,
        pipeline_id,
        build_number: format!("20260829.{id}"),
        status,
        result,
        source_branch: "refs/heads/main".into(),
        source_version: "abc1234def5678".into(),
        requested_for: Some("Jacob Ragsdale".into()),
        reason: "individualCI".into(),
        pr_id: None,
        queue_time: Some(ts("2026-08-29T10:00:00Z")),
        start_time: Some(ts("2026-08-29T10:00:05Z")),
        finish_time: (!status.is_live()).then(|| ts("2026-08-29T10:04:17Z")),
        url: format!("https://dev.azure.com/demo/atlas/_build/results?buildId={id}"),
    }
}

/// An app whose Pipelines tab holds two pipelines and four runs between them.
pub(crate) fn pipelines_app() -> App {
    let mut app = App::new(Vec::new());
    app.shell.set_repos(vec![Repo {
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
    let pipelines = vec![
        pipeline(1, "ticket-tui CI", "\\"),
        pipeline(2, "nightly", "\\scheduled"),
    ];
    let runs = vec![
        run(14, 1, RunStatus::InProgress, None),
        run(13, 1, RunStatus::Completed, Some(RunResult::Failed)),
        run(12, 1, RunStatus::Completed, Some(RunResult::Succeeded)),
        run(11, 2, RunStatus::Completed, Some(RunResult::Canceled)),
    ];
    let shell = &app.shell;
    app.pipelines.set_pipelines(pipelines, runs, shell);
    app
}

#[test]
fn the_pipelines_level_lists_every_definition_with_the_run_it_last_had() {
    let app = pipelines_app();
    let rows = app.pipelines.visible_pipelines(&app.shell);

    assert_eq!(rows.len(), 2);
    let ci = rows
        .iter()
        .find(|row| row.pipeline.name == "ticket-tui CI")
        .expect("the CI pipeline");
    assert_eq!(
        ci.last_run.as_ref().map(|run| run.id),
        Some(14),
        "the newest run is the one the Last run cell shows"
    );
    assert_eq!(ci.repo, "ticket-tui", "the repository reads as its name");
    assert_eq!(ci.branch(), "main", "and the branch without its ref prefix");
}

#[test]
fn enter_opens_the_runs_of_the_pipeline_under_the_cursor_and_backspace_goes_back() {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);

    let index = app
        .pipelines
        .visible_pipelines(&app.shell)
        .iter()
        .position(|row| row.pipeline.name == "ticket-tui CI")
        .expect("the CI pipeline is listed");
    app.pipelines.pipeline_cursor.focus(index);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.pipelines.level(), Level::Runs(1));
    assert_eq!(
        app.pipelines
            .visible_runs(&app.shell)
            .iter()
            .map(|row| row.run.id)
            .collect::<Vec<_>>(),
        [14, 13, 12],
        "its runs, newest first, and nobody else's"
    );

    app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(app.pipelines.level(), Level::Pipelines);
}

#[test]
fn the_runs_level_filters_on_its_own_grammar() {
    let mut app = pipelines_app();
    app.pipelines.pipeline_cursor.focus(0);
    app.pipelines.open_runs(&app.shell);
    let shell = &app.shell;
    app.pipelines.set_query("result:failed".to_owned());

    assert_eq!(
        app.pipelines
            .visible_runs(shell)
            .iter()
            .map(|row| row.run.id)
            .collect::<Vec<_>>(),
        [13],
        "result: reads the run's own result"
    );

    app.pipelines.set_query("by:@me".to_owned());
    assert_eq!(
        app.pipelines.visible_runs(&app.shell).len(),
        0,
        "@me is nobody until a sync says who is signed in"
    );
    app.shell.set_me(Some("Jacob Ragsdale".to_owned()));
    assert_eq!(
        app.pipelines.visible_runs(&app.shell).len(),
        3,
        "and everybody's runs once it does"
    );
}

#[test]
fn a_running_row_reports_the_time_it_has_been_going_and_a_finished_one_its_duration() {
    let app = pipelines_app();
    let rows = app.pipelines.visible_pipelines(&app.shell);
    let ci = rows
        .iter()
        .find(|row| row.pipeline.name == "ticket-tui CI")
        .expect("the CI pipeline");
    let running = RunRow {
        run: ci.last_run.clone().expect("a run"),
        pipeline: ci.pipeline.name.clone(),
    };
    assert_eq!(
        running.duration_seconds(ts("2026-08-29T10:03:17Z")),
        Some(192),
        "a run still going is measured against now, which is what makes it tick"
    );
    assert_eq!(running.finished_duration(), None);

    let finished = RunRow {
        run: run(13, 1, RunStatus::Completed, Some(RunResult::Failed)),
        pipeline: "ticket-tui CI".to_owned(),
    };
    assert_eq!(finished.finished_duration(), Some(252));
    assert_eq!(rows::duration_label(252), "4m 12s");
    assert_eq!(rows::duration_label(45), "45s");
    assert_eq!(rows::duration_label(3_800), "1h 03m");
}

#[test]
fn the_tab_wears_a_badge_while_anything_is_running() {
    let app = pipelines_app();
    assert_eq!(Screen::badge(&app.pipelines), Some("\u{25d0}1".to_owned()));

    let mut quiet = App::new(Vec::new());
    let shell = &quiet.shell;
    quiet.pipelines.set_pipelines(
        vec![pipeline(1, "ticket-tui CI", "\\")],
        vec![run(12, 1, RunStatus::Completed, Some(RunResult::Succeeded))],
        shell,
    );
    assert_eq!(Screen::badge(&quiet.pipelines), None);
}

#[test]
fn a_jump_to_a_run_opens_its_pipelines_runs_and_settles_on_it() {
    let mut app = pipelines_app();

    assert!(app.follow(&Jump::Run(13)));
    assert_eq!(app.tab, TabId::Pipelines);
    assert_eq!(app.pipelines.level(), Level::Runs(1));
    assert_eq!(
        app.pipelines.selected_run(&app.shell).map(|row| row.run.id),
        Some(13)
    );

    assert!(
        !app.follow(&Jump::Run(4_242)),
        "a run nothing holds says so"
    );
}
