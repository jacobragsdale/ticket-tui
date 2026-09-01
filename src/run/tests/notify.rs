//! Desktop notifications as the run fires them: the same words in the footer
//! and on the desktop, and nothing at all without a `[notify]` table.

use super::*;
use ticket_tui::model::{Pipeline, Run, RunStatus};
use ticket_tui::notify::Notifier;

fn nightly() -> Pipeline {
    Pipeline {
        id: 4,
        name: "Nightly".to_owned(),
        folder: "\\".to_owned(),
        repo_id: None,
        default_branch: Some("refs/heads/main".to_owned()),
        url: String::new(),
        queue_status: "enabled".to_owned(),
    }
}

/// The run the watcher hands back when the one being followed has stopped.
fn finished(result: RunResult) -> Run {
    Run {
        id: 91,
        pipeline_id: 4,
        build_number: "20260829.14".to_owned(),
        status: RunStatus::Completed,
        result: Some(result),
        source_branch: "refs/heads/main".to_owned(),
        source_version: String::new(),
        requested_for: None,
        reason: "manual".to_owned(),
        pr_id: None,
        queue_time: None,
        start_time: Timestamp::parse("2026-08-29T10:00:00Z").ok(),
        finish_time: Timestamp::parse("2026-08-29T10:04:12Z").ok(),
        url: String::new(),
    }
}

/// A run being watched, with its pipeline on file so the notification can name
/// it.
fn watching() -> App {
    let mut app = App::new(Vec::new());
    let mut live = finished(RunResult::Succeeded);
    live.status = RunStatus::InProgress;
    live.result = None;
    live.finish_time = None;
    let shell = &app.shell;
    app.pipelines
        .set_pipelines(vec![nightly()], vec![live], shell);
    app.pipelines.watch_run(91);
    app
}

#[test]
fn a_watched_run_finishing_says_the_same_thing_in_the_footer_and_on_the_desktop() {
    let mut app = watching();
    let (notifier, said) = Notifier::recording();
    app.shell.set_notifier(notifier);

    run_finished(&mut app, finished(RunResult::Succeeded));

    assert_eq!(
        said.lock().unwrap().clone(),
        [(
            "\u{2713} Build 20260829.14 succeeded \u{00b7} 4m 12s".to_owned(),
            "Nightly \u{00b7} main".to_owned()
        )],
        "one notification, the pipeline and branch under it"
    );
    assert_eq!(
        app.shell.notification(),
        Some((
            "\u{2713} Build 20260829.14 succeeded \u{00b7} 4m 12s \u{2014} Nightly \u{00b7} main",
            NotificationLevel::Info
        )),
        "and the footer says the same words"
    );
    assert!(
        !app.pipelines.is_watched(91),
        "a run that has stopped is not watched any more, so it is announced once"
    );
}

#[test]
fn a_failed_run_is_red_in_the_footer_and_still_reaches_the_desktop() {
    let mut app = watching();
    let (notifier, said) = Notifier::recording();
    app.shell.set_notifier(notifier);

    run_finished(&mut app, finished(RunResult::Failed));

    assert_eq!(said.lock().unwrap().len(), 1);
    let (message, level) = app.shell.notification().unwrap();
    assert!(
        message.starts_with("\u{2717} Build 20260829.14 failed"),
        "{message}"
    );
    assert_eq!(level, NotificationLevel::Error);
}

#[test]
fn without_a_notify_table_the_footer_is_all_there_is() {
    // The shell a run starts with: `[notify]` unwritten, nothing to run.
    let mut app = watching();
    run_finished(&mut app, finished(RunResult::Succeeded));
    assert!(
        app.shell.notification().is_some(),
        "the footer still says it"
    );
}

fn authored_by_me_with(vote: i8) -> ticket_tui::model::PullRequest {
    ticket_tui::model::PullRequest {
        repo_id: "repo".to_owned(),
        id: 812,
        title: "Tidy the watcher".to_owned(),
        description: String::new(),
        status: ticket_tui::model::PrStatus::Active,
        is_draft: false,
        created_by: ticket_tui::model::Identity {
            display_name: "Jacob".to_owned(),
            unique_name: None,
        },
        created_at: None,
        closed_at: None,
        source_ref: "refs/heads/feature".to_owned(),
        target_ref: "refs/heads/main".to_owned(),
        merge_status: "succeeded".to_owned(),
        last_merge_source_commit: String::new(),
        auto_complete_set_by: None,
        url: String::new(),
        reviewers: vec![ticket_tui::model::PrReviewer {
            id: "d".to_owned(),
            display_name: "Dana Ali".to_owned(),
            unique_name: None,
            vote,
            is_required: false,
        }],
        work_items: Vec::new(),
        build: None,
        threads: Vec::new(),
    }
}

/// The rows loaded at startup never went through `apply_snapshot`, so they
/// have to be seeded as the baseline by hand — otherwise the first pull of the
/// run is taken as the baseline and the vote it carries is never said.
#[test]
fn the_rows_loaded_at_startup_are_the_baseline_so_the_first_pull_can_be_news() {
    let mut app = App::new(Vec::new());
    let (notifier, log) = ticket_tui::notify::Notifier::recording();
    app.shell.set_notifier(notifier);
    app.shell.set_me(Some("Jacob".to_owned()));
    app.seed_pull_request_marks(&[authored_by_me_with(0)]);

    app.announce_pull_requests(&[authored_by_me_with(10)]);
    let fired = log.lock().unwrap();
    assert_eq!(fired.len(), 1, "{fired:?}");
    assert_eq!(fired[0].0, "!812 approved by Dana Ali");
}
