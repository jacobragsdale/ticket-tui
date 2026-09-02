use crate::app::{App, TabId};
use crate::local::LocalRequest;
use crate::model::{GitJob, LocalRepo, Repo};

pub(crate) fn repo(id: &str, name: &str, disabled: bool) -> Repo {
    Repo {
        id: id.to_owned(),
        name: name.to_owned(),
        project: "atlas".into(),
        default_branch: Some("refs/heads/main".into()),
        remote_url: format!("https://dev.azure.com/demo/atlas/_git/{name}"),
        ssh_url: format!("git@ssh.dev.azure.com:v3/demo/atlas/{name}"),
        web_url: format!("https://dev.azure.com/demo/atlas/_git/{name}"),
        is_disabled: disabled,
        size: Some(2_097_152),
    }
}

pub(crate) fn local(branch: &str, dirty: bool, ahead: u32, behind: u32) -> LocalRepo {
    LocalRepo {
        path: std::path::PathBuf::from("/Users/jacob/Development/ticket-tui"),
        origin: "https://dev.azure.com/demo/atlas/_git/ticket-tui".to_owned(),
        branch: branch.to_owned(),
        dirty,
        ahead,
        behind,
        busy: None,
    }
}

/// An app whose Repos tab holds three: one cloned and clean, one cloned and
/// dirty, and one nobody has here — plus a disabled one.
pub(crate) fn repos_app() -> App {
    let mut app = App::new(Vec::new());
    app.shell.set_repos(vec![
        repo("aaa-111", "ticket-tui", false),
        repo("bbb-222", "skillbook", false),
        repo("ccc-333", "home-server", false),
        repo("ddd-444", "archived", true),
    ]);
    app.repos.set_repos(&app.shell);
    app.shell
        .set_workspace(Some("/Users/jacob/Development".into()));
    app.repos.set_local(vec![
        ("aaa-111".to_owned(), local("main", false, 0, 0)),
        ("bbb-222".to_owned(), local("feature/x", true, 1, 2)),
    ]);
    app.repos.set_related(
        vec![
            ("aaa-111".to_owned(), 11, "Split the files".to_owned()),
            ("aaa-111".to_owned(), 12, "Tab bar".to_owned()),
        ],
        vec![("aaa-111".to_owned(), 1, "ticket-tui CI".to_owned(), None)],
    );
    app.select_tab(TabId::Repos);
    app
}

#[test]
fn the_table_lists_every_repository_with_what_is_open_against_it() {
    let app = repos_app();
    let rows = app.repos.visible(&app.shell);

    assert_eq!(rows.len(), 4, "a disabled repository stays on the table");
    let ticket_tui = rows
        .iter()
        .find(|row| row.repo.name == "ticket-tui")
        .expect("the repository");
    assert_eq!(ticket_tui.pull_requests, 2);
    assert_eq!(ticket_tui.pipelines, 1);
    assert_eq!(
        ticket_tui.local.as_ref().map(|local| local.branch.clone()),
        Some("main".to_owned())
    );
    assert_eq!(
        rows.iter()
            .find(|row| row.repo.name == "home-server")
            .and_then(|row| row.local.clone()),
        None,
        "one nobody has here says nothing about a local copy"
    );
}

#[test]
fn the_local_filter_asks_what_state_the_clone_is_in() {
    let mut app = repos_app();

    app.repos.set_query("local:cloned".to_owned());
    assert_eq!(app.repos.visible(&app.shell).len(), 2);

    app.repos.set_query("local:dirty".to_owned());
    assert_eq!(
        app.repos
            .visible(&app.shell)
            .iter()
            .map(|row| row.repo.name.clone())
            .collect::<Vec<_>>(),
        ["skillbook".to_owned()]
    );

    app.repos.set_query("local:missing".to_owned());
    assert_eq!(app.repos.visible(&app.shell).len(), 2);

    app.repos.set_query("local:behind".to_owned());
    assert_eq!(app.repos.visible(&app.shell).len(), 1);

    app.repos.set_query("disabled:yes".to_owned());
    assert_eq!(app.repos.visible(&app.shell).len(), 1);
}

#[test]
fn a_repository_being_cloned_says_so_where_its_status_goes() {
    let mut app = repos_app();
    // A clone has nothing on this machine yet, so the job is all the row has.
    app.repos.set_job("ccc-333", Some(GitJob::Cloning));

    let row = app
        .repos
        .visible(&app.shell)
        .into_iter()
        .find(|row| row.repo.name == "home-server")
        .expect("the repository");
    assert_eq!(
        row.local.and_then(|local| local.busy),
        Some(GitJob::Cloning)
    );
}

fn press(app: &mut App, code: crossterm::event::KeyCode) -> crate::app::AppAction {
    app.handle_key(crossterm::event::KeyEvent::new(
        code,
        crossterm::event::KeyModifiers::SHIFT,
    ))
}

/// `g`, which unlike this tab's own keys carries no modifier.
fn go(app: &mut App) -> crate::app::AppAction {
    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('g'),
        crossterm::event::KeyModifiers::NONE,
    ))
}

/// The row order is by name: archived, home-server, skillbook, ticket-tui.
fn focus(app: &mut App, name: &str) {
    let index = app
        .repos
        .visible(&app.shell)
        .iter()
        .position(|row| row.repo.name == name)
        .expect("the repository");
    app.repos.cursor.focus(index);
}

#[test]
fn c_clones_what_is_not_here_and_refuses_what_is() {
    let mut app = repos_app();
    focus(&mut app, "home-server");

    let action = press(&mut app, crossterm::event::KeyCode::Char('C'));
    assert_eq!(
        action,
        crate::app::AppAction::LocalGit(LocalRequest::Clone {
            repo_id: "ccc-333".to_owned(),
            // https by default: the local thread signs it with the sync's
            // login, so it works before any SSH key is set up.
            url: "https://dev.azure.com/demo/atlas/_git/home-server".to_owned(),
            into: "/Users/jacob/Development/home-server".into(),
        })
    );

    focus(&mut app, "ticket-tui");
    assert_eq!(
        press(&mut app, crossterm::event::KeyCode::Char('C')),
        crate::app::AppAction::None
    );
    assert!(
        app.shell
            .notification()
            .is_some_and(|(text, _)| text.contains("already at")),
        "a repository that is here says where it is"
    );
}

#[test]
fn a_clone_with_nowhere_to_go_says_so_rather_than_guessing() {
    let mut app = repos_app();
    app.shell.set_workspace(None);
    focus(&mut app, "home-server");

    assert_eq!(
        press(&mut app, crossterm::event::KeyCode::Char('C')),
        crate::app::AppAction::None
    );
    assert!(
        app.shell
            .notification()
            .is_some_and(|(text, _)| text.contains("TICKET_TUI_WORKSPACE")),
        "and says how to give it one"
    );
}

#[test]
fn g_fetches_and_p_pulls_only_what_is_here_and_clean() {
    let mut app = repos_app();
    focus(&mut app, "ticket-tui");

    assert_eq!(
        press(&mut app, crossterm::event::KeyCode::Char('G')),
        crate::app::AppAction::LocalGit(LocalRequest::Fetch {
            repo_id: "aaa-111".to_owned(),
            path: "/Users/jacob/Development/ticket-tui".into(),
        })
    );
    assert_eq!(
        press(&mut app, crossterm::event::KeyCode::Char('P')),
        crate::app::AppAction::LocalGit(LocalRequest::Pull {
            repo_id: "aaa-111".to_owned(),
            path: "/Users/jacob/Development/ticket-tui".into(),
        })
    );

    // The dirty one may be fetched — that changes nothing in the tree — but
    // not pulled.
    focus(&mut app, "skillbook");
    assert!(matches!(
        press(&mut app, crossterm::event::KeyCode::Char('G')),
        crate::app::AppAction::LocalGit(LocalRequest::Fetch { .. })
    ));
    assert_eq!(
        press(&mut app, crossterm::event::KeyCode::Char('P')),
        crate::app::AppAction::None
    );
    assert!(
        app.shell
            .notification()
            .is_some_and(|(text, _)| text.contains("uncommitted")),
        "and says why"
    );

    // Neither is offered for a repository that is not on the machine.
    focus(&mut app, "home-server");
    assert_eq!(
        press(&mut app, crossterm::event::KeyCode::Char('G')),
        crate::app::AppAction::None
    );
    assert!(
        app.shell
            .notification()
            .is_some_and(|(text, _)| text.contains("C clones it")),
        "it points at the key that would"
    );
}

#[test]
fn a_repository_git_is_busy_with_is_left_alone() {
    let mut app = repos_app();
    focus(&mut app, "ticket-tui");
    app.repos.set_job("aaa-111", Some(GitJob::Pulling));

    assert_eq!(
        press(&mut app, crossterm::event::KeyCode::Char('G')),
        crate::app::AppAction::None
    );
    assert!(
        app.shell
            .notification()
            .is_some_and(|(text, _)| text.contains("Already pulling")),
        "one git command at a time on one clone"
    );

    // Including a second clone of a repository already being cloned.
    focus(&mut app, "home-server");
    app.repos.set_job("ccc-333", Some(GitJob::Cloning));
    assert_eq!(
        press(&mut app, crossterm::event::KeyCode::Char('C')),
        crate::app::AppAction::None
    );
    assert!(
        app.shell
            .notification()
            .is_some_and(|(text, _)| text.contains("Already cloning")),
        "and the second C says so rather than starting another"
    );
    assert!(app.repos.busy(), "which is what makes the glyph turn");
}

/// The same app with the other two tabs filled in, so a jump has somewhere to
/// land: two pull requests and one pipeline, all against ticket-tui. The tabs
/// are crossed the way the app crosses them, through `relate_repos`, so the
/// rows carry the run their pipeline last had.
pub(crate) fn crossed_app() -> App {
    use crate::app::pipelines::tests::{pipeline, run};
    use crate::app::pull_requests::tests::pull_request;
    use crate::model::{PrStatus, RunResult, RunStatus};

    let mut app = repos_app();
    let requests = vec![
        pull_request(11, "Split the files", "Avery", PrStatus::Active),
        pull_request(12, "Tab bar", "Jacob Ragsdale", PrStatus::Active),
    ];
    let shell = &app.shell;
    app.pull_requests.set_pull_requests(requests.clone(), shell);
    let pipelines = vec![pipeline(1, "ticket-tui CI", "\\")];
    let runs = vec![run(14, 1, RunStatus::Completed, Some(RunResult::Succeeded))];
    let shell = &app.shell;
    app.pipelines.set_pipelines(pipelines, runs, shell);
    app.relate_repos(&requests);
    app.select_tab(TabId::Repos);
    app
}

#[test]
fn the_details_pane_walks_to_a_pull_request_and_a_pipeline_and_back() {
    use crate::model::Jump;

    let mut app = crossed_app();
    focus(&mut app, "ticket-tui");
    // Tab moves the focus to the pane, where j/k walk the references.
    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(app.shell.focus, crate::app::Focus::Details);

    let action = app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(action, crate::app::AppAction::None, "the shell follows it");
    assert_eq!(app.tab, TabId::PullRequests, "the first reference is a PR");
    assert_eq!(
        app.pull_requests
            .selected(&app.shell)
            .map(|row| row.request.id),
        Some(11)
    );
    assert_eq!(
        app.shell.history(),
        [
            Jump::Repo("ticket-tui".to_owned()),
            Jump::PullRequest {
                repo: "ticket-tui".to_owned(),
                id: 11
            }
        ],
        "both ends of the walk are on the history"
    );

    app.history_back();
    assert_eq!(app.tab, TabId::Repos, "and [ comes back");
    assert_eq!(
        app.repos.selected(&app.shell).map(|row| row.repo.name),
        Some("ticket-tui".to_owned())
    );

    // The last reference is the pipeline that builds it.
    app.shell.focus = crate::app::Focus::Details;
    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::End,
        crossterm::event::KeyModifiers::NONE,
    ));
    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(app.tab, TabId::Pipelines);
    assert_eq!(
        app.pipelines
            .visible_pipelines(&app.shell)
            .get(app.pipelines.pipeline_cursor.index)
            .map(|row| row.pipeline.name.clone()),
        Some("ticket-tui CI".to_owned())
    );

    app.history_back();
    assert_eq!(app.tab, TabId::Repos, "and back again");
}

#[test]
fn a_repository_name_on_another_tab_comes_back_here() {
    use crate::model::Jump;

    let mut app = crossed_app();
    app.select_tab(TabId::PullRequests);
    app.pull_requests.cursor.focus(0);
    let jumps = app.pull_requests.jumps(&app.shell, &|_| None);
    let (_, back) = jumps.first().expect("the repository line");
    assert_eq!(back, &Jump::Repo("ticket-tui".to_owned()));

    assert!(app.follow(back));
    assert_eq!(app.tab, TabId::Repos);
    assert_eq!(
        app.repos.selected(&app.shell).map(|row| row.repo.name),
        Some("ticket-tui".to_owned())
    );
    app.history_back();
    assert_eq!(
        app.tab,
        TabId::PullRequests,
        "the pull request it came from is where [ goes"
    );
}

#[test]
fn the_count_columns_add_up_what_the_other_tabs_hold() {
    let app = crossed_app();
    let rows = app.repos.visible(&app.shell);
    let ticket_tui = rows
        .iter()
        .find(|row| row.repo.name == "ticket-tui")
        .expect("the repository");

    assert_eq!(ticket_tui.pull_requests, 2);
    assert_eq!(ticket_tui.pipelines, 1);
    assert!(
        rows.iter()
            .filter(|row| row.repo.name != "ticket-tui")
            .all(|row| row.pull_requests == 0 && row.pipelines == 0),
        "and nothing is credited to the others"
    );
}

#[test]
fn g_goes_to_the_newest_pull_request_open_on_the_repository() {
    use crate::app::pull_requests::tests::pull_request;
    use crate::app::{Jump, Screen};
    use crate::model::PrStatus;

    let mut app = repos_app();
    focus(&mut app, "ticket-tui");
    assert_eq!(
        Screen::follow_target(&app.repos, &app.shell),
        Ok((
            Jump::PullRequest {
                repo: "ticket-tui".to_owned(),
                id: 12,
            },
            "pull request"
        )),
        "two are open on it; the newer one is where g goes"
    );

    let requests = vec![pull_request(12, "Tab bar", "Avery", PrStatus::Active)];
    let shell = &app.shell;
    app.pull_requests.set_pull_requests(requests, shell);
    go(&mut app);
    assert_eq!(app.tab, TabId::PullRequests);
    assert_eq!(
        app.pull_requests
            .selected(&app.shell)
            .map(|row| row.request.id),
        Some(12)
    );

    // Nothing is open on this one and nothing builds it, so the key says so
    // and stays put.
    app.select_tab(TabId::Repos);
    focus(&mut app, "skillbook");
    assert_eq!(go(&mut app), crate::app::AppAction::None);
    assert_eq!(app.tab, TabId::Repos);
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("No open pull request or pipeline on skillbook")
    );
}

#[test]
fn a_repository_row_says_how_its_pipelines_last_went() {
    use crate::app::pipelines::tests::{pipeline, run};
    use crate::model::{RunResult, RunStatus};

    let build = |app: &App| {
        app.repos
            .visible(&app.shell)
            .into_iter()
            .find(|row| row.repo.name == "ticket-tui")
            .expect("the repository")
            .build
    };

    let mut app = crossed_app();
    assert_eq!(
        build(&app).map(|run| (run.id, run.result)),
        Some((14, Some(RunResult::Succeeded))),
        "the one pipeline that builds it went green"
    );

    // A second pipeline whose last run failed is what the row says: the worst
    // of them is the thing to know.
    let pipelines = vec![
        pipeline(1, "ticket-tui CI", "\\"),
        pipeline(2, "nightly", "\\"),
    ];
    let runs = vec![
        run(14, 1, RunStatus::Completed, Some(RunResult::Succeeded)),
        run(15, 2, RunStatus::Completed, Some(RunResult::Failed)),
    ];
    let shell = &app.shell;
    app.pipelines.set_pipelines(pipelines, runs, shell);
    app.relate_repos(&[]);
    assert_eq!(
        build(&app).map(|run| (run.id, run.result)),
        Some((15, Some(RunResult::Failed)))
    );
    assert_eq!(
        app.repos
            .visible(&app.shell)
            .into_iter()
            .find(|row| row.repo.name == "skillbook")
            .and_then(|row| row.build),
        None,
        "and a repository nothing builds says nothing"
    );
}

#[test]
fn the_details_pane_lines_carry_the_run_each_pipeline_last_had() {
    let mut app = crossed_app();
    focus(&mut app, "ticket-tui");
    let jumps = app.repos.jumps(&app.shell);
    assert!(
        jumps
            .iter()
            .any(|(label, _)| label == "ticket-tui CI  \u{2713} 20260829.14"),
        "the pipeline line says how it last went: {jumps:?}"
    );
}

#[test]
fn g_falls_through_to_the_pipeline_when_nothing_is_open() {
    use crate::app::pipelines::tests::{pipeline, run};
    use crate::app::{Jump, Screen};
    use crate::model::{RunResult, RunStatus};

    let mut app = crossed_app();
    // Two pipelines build it and nothing is open against it any more.
    let pipelines = vec![
        pipeline(1, "ticket-tui CI", "\\"),
        pipeline(2, "nightly", "\\"),
    ];
    let runs = vec![
        run(14, 1, RunStatus::Completed, Some(RunResult::Succeeded)),
        run(15, 2, RunStatus::Completed, Some(RunResult::Failed)),
    ];
    let shell = &app.shell;
    app.pipelines.set_pipelines(pipelines, runs, shell);
    app.relate_repos(&[]);
    focus(&mut app, "ticket-tui");

    assert_eq!(
        Screen::follow_target(&app.repos, &app.shell),
        Ok((Jump::Pipeline(2), "pipeline")),
        "the one that ran most recently is where g goes"
    );
    go(&mut app);
    assert_eq!(app.tab, TabId::Pipelines);
    assert_eq!(
        app.pipelines
            .visible_pipelines(&app.shell)
            .get(app.pipelines.pipeline_cursor.index)
            .map(|row| row.pipeline.name.clone()),
        Some("nightly".to_owned())
    );

    // One with neither still says so and stays put.
    app.history_back();
    focus(&mut app, "skillbook");
    assert_eq!(go(&mut app), crate::app::AppAction::None);
    assert_eq!(app.tab, TabId::Repos);
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("No open pull request or pipeline on skillbook")
    );
}
