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
fn a_workspace_that_moved_is_read_whatever_tab_is_showing_and_the_answer_narrows_the_tabs() {
    use ticket_tui::model::{LocalRepo, Repo};

    let directory = tempdir().unwrap();
    let (mut app, mut runtime, requests, events) = local_runtime(directory.path());
    let repo = |id: &str, name: &str| Repo {
        id: id.to_owned(),
        name: name.to_owned(),
        project: "atlas".into(),
        default_branch: None,
        remote_url: format!("https://dev.azure.com/demo/atlas/_git/{name}"),
        ssh_url: String::new(),
        web_url: String::new(),
        is_disabled: false,
        size: None,
    };
    app.shell.set_repos(vec![
        repo("aaa-111", "ticket-tui"),
        repo("bbb-222", "skillbook"),
    ]);
    let mut here = super::notify::authored_by_me_with(0);
    here.repo_id = "aaa-111".to_owned();
    let mut elsewhere = super::notify::authored_by_me_with(0);
    elsewhere.id = 813;
    elsewhere.repo_id = "bbb-222".to_owned();
    let shell = &app.shell;
    app.pull_requests
        .set_pull_requests(vec![here, elsewhere], shell);
    assert!(
        app.pull_requests.visible(&app.shell).is_empty(),
        "nothing is on the tab until the workspace has been read"
    );

    // Start-up already read it, so nothing is asked for on the first turn.
    runtime.local.scanned = Some(Instant::now());
    app.tab = TabId::PullRequests;
    poll_local(&mut app, &mut runtime);
    assert_eq!(scans_asked(&requests), 0);

    // `r`, on any tab and even offline, reads it again.
    handle_action(AppAction::Sync, &mut app, &mut runtime, &failing_opener);
    poll_local(&mut app, &mut runtime);
    assert_eq!(scans_asked(&requests), 1, "r re-reads the workspace");

    // The answer: one clone whose origin is the repository, one that only
    // has its name.
    let clone = |verified: bool| LocalRepo {
        branch: "main".to_owned(),
        verified,
        ..LocalRepo::default()
    };
    events
        .send(LocalEvent::Scanned(vec![
            ("aaa-111".to_owned(), clone(true)),
            ("bbb-222".to_owned(), clone(false)),
        ]))
        .unwrap();
    poll_local(&mut app, &mut runtime);
    assert_eq!(
        app.pull_requests
            .visible(&app.shell)
            .iter()
            .map(|row| row.request.id)
            .collect::<Vec<_>>(),
        [812],
        "the verified clone's pull request is on the tab; the name-matched one's is not"
    );
    assert!(app.shell.has_clone("aaa-111") && !app.shell.has_clone("bbb-222"));
    assert!(
        app.repos.local_for("bbb-222").is_some(),
        "the Repos tab still lists the clone that only has the name"
    );

    // A git command finishing moves the workspace: it is read again even
    // though the Repos tab is not showing.
    events
        .send(LocalEvent::Finished {
            repo_id: "bbb-222".into(),
            job: GitJob::Cloning,
            message: "Cloned skillbook".into(),
            error: false,
        })
        .unwrap();
    poll_local(&mut app, &mut runtime);
    poll_local(&mut app, &mut runtime);
    assert_eq!(
        scans_asked(&requests),
        1,
        "a finished clone is read for, on the Pull requests tab"
    );
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
