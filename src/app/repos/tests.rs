use crate::app::{App, TabId};
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
    app.repos
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
        vec![("aaa-111".to_owned(), 1, "ticket-tui CI".to_owned())],
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
    let mut cloning = local("main", false, 0, 0);
    cloning.busy = Some(GitJob::Cloning);
    app.repos.set_local(vec![("ccc-333".to_owned(), cloning)]);

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
