//! The Repos tab's scans: one out at a time, one follow-up at most, and a
//! thread that stops taking its glyphs and its promises with it.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use super::*;
use ticket_tui::app::TabId;
use ticket_tui::model::GitJob;

/// A runtime whose local thread is the test: it reads the requests and
/// answers with whatever events it likes, whenever it likes.
fn local_runtime(
    workspace: &Path,
) -> (App, SyncRuntime, Receiver<LocalRequest>, Sender<LocalEvent>) {
    let (request_sender, request_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let mut app = App::new(Vec::new());
    app.shell.set_workspace(Some(workspace.to_path_buf()));
    let runtime = SyncRuntime {
        worker: None,
        scheduler: SyncScheduler::new(None),
        config: None,
        offline_reason: None,
        details: DetailsEngine::default(),
        pipelines: None,
        watching_tab: false,
        watching_run: (None, None),
        watched_runs: Vec::new(),
        approvals_seen: None,
        local: LocalRuntime {
            worker: Some(LocalHandle::from_channels(request_sender, event_receiver)),
            ..LocalRuntime::default()
        },
        agents: AgentRuntime::default(),
    };
    (app, runtime, request_receiver, event_sender)
}

/// How many scans the thread has been asked for since the last look.
fn scans_asked(requests: &Receiver<LocalRequest>) -> usize {
    let mut scans = 0;
    loop {
        match requests.try_recv() {
            Ok(LocalRequest::Scan { .. }) => scans += 1,
            Ok(other) => panic!("only scans were expected, got {other:?}"),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return scans,
        }
    }
}

#[test]
fn a_slow_scan_is_not_joined_by_a_second_however_often_the_tab_is_opened() {
    let directory = tempdir().unwrap();
    let (mut app, mut runtime, requests, events) = local_runtime(directory.path());

    app.tab = TabId::Repos;
    poll_local(&mut app, &mut runtime);
    assert_eq!(
        scans_asked(&requests),
        1,
        "opening the tab reads the workspace"
    );
    assert!(runtime.local.scanning);

    // The thread is slow. The tab is left and opened again, three times over,
    // and the cadence comes round on top of that.
    for _ in 0..3 {
        app.tab = TabId::WorkItems;
        poll_local(&mut app, &mut runtime);
        app.tab = TabId::Repos;
        poll_local(&mut app, &mut runtime);
    }
    runtime.local.scanned = Some(Instant::now() - LOCAL_SCAN_CADENCE);
    poll_local(&mut app, &mut runtime);
    assert_eq!(
        scans_asked(&requests),
        0,
        "nothing queues behind the scan that is out"
    );
    assert!(runtime.local.rescan, "but one follow-up is booked");

    // The scan answers: the follow-up goes, and only one.
    events.send(LocalEvent::Scanned(Vec::new())).unwrap();
    poll_local(&mut app, &mut runtime);
    assert_eq!(scans_asked(&requests), 1);
    assert!(runtime.local.scanning);
    assert!(!runtime.local.rescan);

    // That one answers too, with nothing new to look for.
    events.send(LocalEvent::Scanned(Vec::new())).unwrap();
    poll_local(&mut app, &mut runtime);
    assert_eq!(scans_asked(&requests), 0);
    assert!(!runtime.local.scanning);
    assert!(app.repos.scanned_at().is_some(), "the tab has its answer");
}

#[test]
fn a_job_that_finishes_while_a_scan_is_out_gets_one_scan_after_it() {
    let directory = tempdir().unwrap();
    let (mut app, mut runtime, requests, events) = local_runtime(directory.path());
    app.tab = TabId::Repos;
    poll_local(&mut app, &mut runtime);
    assert_eq!(scans_asked(&requests), 1);

    events
        .send(LocalEvent::Started {
            repo_id: "aaa-111".into(),
            job: GitJob::Fetching,
        })
        .unwrap();
    poll_local(&mut app, &mut runtime);
    assert!(app.repos.busy(), "the glyph turns while git runs");
    events
        .send(LocalEvent::Finished {
            repo_id: "aaa-111".into(),
            job: GitJob::Fetching,
            message: "Fetched".into(),
            error: false,
        })
        .unwrap();
    poll_local(&mut app, &mut runtime);
    assert!(!app.repos.busy());
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Fetched")
    );
    // The workspace moved, but the scan that is out will not see it.
    poll_local(&mut app, &mut runtime);
    assert_eq!(scans_asked(&requests), 0, "still one scan at a time");
    assert!(runtime.local.rescan);

    events.send(LocalEvent::Scanned(Vec::new())).unwrap();
    poll_local(&mut app, &mut runtime);
    assert_eq!(
        scans_asked(&requests),
        1,
        "and one follow-up reads what git did"
    );
    events.send(LocalEvent::Scanned(Vec::new())).unwrap();
    poll_local(&mut app, &mut runtime);
    assert_eq!(scans_asked(&requests), 0);
}

#[test]
fn a_thread_that_stops_takes_its_busy_glyphs_and_its_pending_scan_with_it() {
    // The thread's events end: whatever it had started is not finishing.
    let directory = tempdir().unwrap();
    let (mut app, mut runtime, requests, events) = local_runtime(directory.path());
    app.tab = TabId::Repos;
    poll_local(&mut app, &mut runtime);
    assert_eq!(scans_asked(&requests), 1);
    events
        .send(LocalEvent::Started {
            repo_id: "aaa-111".into(),
            job: GitJob::Cloning,
        })
        .unwrap();
    poll_local(&mut app, &mut runtime);
    runtime.local.rescan = true;
    drop(events);

    poll_local(&mut app, &mut runtime);

    assert!(runtime.local.worker.is_none());
    assert!(!runtime.local.scanning && !runtime.local.rescan);
    assert!(!app.repos.busy(), "the clone will never report back");
    let (message, level) = app.shell.notification().expect("the tab says why");
    assert!(
        message.starts_with("Local repositories stopped"),
        "{message}"
    );
    assert_eq!(level, NotificationLevel::Error);
    assert!(
        !poll_local(&mut app, &mut runtime),
        "with no thread there is nothing to poll"
    );

    // The thread is gone before it is asked: the send itself fails, whether
    // for a scan or for git.
    let (mut app, mut runtime, requests, _events) = local_runtime(directory.path());
    app.repos.set_job("aaa-111", Some(GitJob::Fetching));
    drop(requests);
    app.tab = TabId::Repos;
    poll_local(&mut app, &mut runtime);
    assert!(runtime.local.worker.is_none());
    assert!(!runtime.local.scanning);
    assert!(!app.repos.busy());
    let (message, level) = app.shell.notification().expect("the tab says why");
    assert!(message.contains("thread stopped"), "{message}");
    assert_eq!(level, NotificationLevel::Error);

    let (mut app, mut runtime, requests, _events) = local_runtime(directory.path());
    app.repos.set_job("aaa-111", Some(GitJob::Fetching));
    drop(requests);
    handle_action(
        AppAction::LocalGit(LocalRequest::Fetch {
            repo_id: "aaa-111".into(),
            path: directory.path().join("ticket-tui"),
        }),
        &mut app,
        &mut runtime,
        &failing_opener,
    );
    assert!(runtime.local.worker.is_none());
    assert!(!app.repos.busy());
    let (message, level) = app.shell.notification().expect("the tab says why");
    assert!(message.contains("thread stopped"), "{message}");
    assert_eq!(level, NotificationLevel::Error);
}
