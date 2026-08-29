use crate::app::App;
use crate::model::{Identity, PrBuild, PrReviewer, PrStatus, PullRequest, Repo};
use crate::timestamp::ts;

pub(crate) fn reviewer(name: &str, vote: i8, required: bool) -> PrReviewer {
    PrReviewer {
        id: format!("reviewer-{name}"),
        display_name: name.to_owned(),
        unique_name: None,
        vote,
        is_required: required,
    }
}

pub(crate) fn pull_request(id: i64, title: &str, author: &str, status: PrStatus) -> PullRequest {
    PullRequest {
        repo_id: "aaa-111".into(),
        id,
        title: title.to_owned(),
        description: "<p>What it does and why.</p>".into(),
        status,
        is_draft: false,
        created_by: Identity::new(author.to_owned(), None),
        // One an hour apart, so the newest-change ordering means something.
        created_at: Some(ts(match id {
            10 => "2026-08-29T06:00:00Z",
            11 => "2026-08-29T07:00:00Z",
            12 => "2026-08-29T08:00:00Z",
            _ => "2026-08-29T09:00:00Z",
        })),
        closed_at: matches!(status, PrStatus::Completed | PrStatus::Abandoned)
            .then(|| ts("2026-08-29T11:00:00Z")),
        source_ref: "refs/heads/feature/tabs".into(),
        target_ref: "refs/heads/main".into(),
        merge_status: "succeeded".into(),
        last_merge_source_commit: "abc1234".into(),
        auto_complete_set_by: None,
        url: format!("https://dev.azure.com/demo/atlas/_git/ticket-tui/pullrequest/{id}"),
        reviewers: Vec::new(),
        work_items: Vec::new(),
        build: None,
    }
}

/// An app whose Pull requests tab holds four: one waiting on me, one of mine,
/// one somebody else's, and one that closed.
pub(crate) fn pull_requests_app() -> App {
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
    app.shell.set_me(Some("Jacob Ragsdale".to_owned()));

    let mut waiting = pull_request(11, "Split the files", "Avery", PrStatus::Active);
    waiting.reviewers = vec![
        reviewer("Jacob Ragsdale", 0, true),
        reviewer("Sam", 10, false),
    ];
    waiting.work_items = vec![10_001];
    waiting.build = Some(PrBuild {
        status: "approved".into(),
        run_id: Some(14),
    });

    let mut mine = pull_request(12, "Tab bar", "Jacob Ragsdale", PrStatus::Active);
    mine.reviewers = vec![reviewer("Avery", 10, true)];
    mine.is_draft = true;

    let mut conflicted = pull_request(13, "Rename the menu", "Sam", PrStatus::Active);
    conflicted.merge_status = "conflicts".into();
    conflicted.reviewers = vec![reviewer("Jacob Ragsdale", -10, true)];

    let closed = pull_request(10, "Earlier work", "Avery", PrStatus::Completed);

    let requests = vec![waiting, mine, conflicted, closed];
    let shell = &app.shell;
    app.pull_requests.set_pull_requests(requests, shell);
    app
}

#[test]
fn the_table_hides_closed_pull_requests_until_the_chip_puts_them_back() {
    let mut app = pull_requests_app();

    assert_eq!(
        app.pull_requests
            .visible(&app.shell)
            .iter()
            .map(|row| row.request.id)
            .collect::<Vec<_>>(),
        [13, 12, 11],
        "the three active ones, newest change first"
    );
    assert_eq!(app.pull_requests.hidden_closed(&app.shell), 1);

    app.pull_requests.show_closed(true);
    assert_eq!(app.pull_requests.visible(&app.shell).len(), 4);
}

#[test]
fn the_built_in_views_ask_the_questions_the_epic_names() {
    let mut app = pull_requests_app();

    app.pull_requests.apply_view("To review");
    assert_eq!(
        app.pull_requests
            .visible(&app.shell)
            .iter()
            .map(|row| row.request.id)
            .collect::<Vec<_>>(),
        [11],
        "the one I am a reviewer on and have not voted"
    );

    app.pull_requests.apply_view("Mine");
    assert_eq!(
        app.pull_requests
            .visible(&app.shell)
            .iter()
            .map(|row| row.request.id)
            .collect::<Vec<_>>(),
        [12]
    );

    app.pull_requests.apply_view("Active");
    assert_eq!(app.pull_requests.visible(&app.shell).len(), 3);

    app.pull_requests.apply_view("Recently closed");
    assert_eq!(
        app.pull_requests
            .visible(&app.shell)
            .iter()
            .map(|row| row.request.id)
            .collect::<Vec<_>>(),
        [10],
        "a query that names a status shows those, hidden or not"
    );
}

#[test]
fn the_grammar_filters_on_this_screens_own_fields() {
    let mut app = pull_requests_app();

    app.pull_requests.set_query("author:@me".to_owned());
    assert_eq!(app.pull_requests.visible(&app.shell).len(), 1);

    app.pull_requests.set_query("draft:yes".to_owned());
    assert_eq!(app.pull_requests.visible(&app.shell).len(), 1);

    app.pull_requests.set_query("build:approved".to_owned());
    assert_eq!(app.pull_requests.visible(&app.shell).len(), 1);

    app.pull_requests
        .set_query("source:feature/tabs".to_owned());
    assert_eq!(app.pull_requests.visible(&app.shell).len(), 3);

    app.pull_requests.set_query("repo:ticket-tui".to_owned());
    assert_eq!(app.pull_requests.visible(&app.shell).len(), 3);
}

#[test]
fn the_badge_counts_what_is_waiting_on_my_vote() {
    let app = pull_requests_app();
    assert_eq!(app.pull_requests.to_review(&app.shell), 1);

    let mut nobody = App::new(Vec::new());
    let shell = &nobody.shell;
    nobody.pull_requests.set_pull_requests(
        vec![pull_request(11, "Anything", "Avery", PrStatus::Active)],
        shell,
    );
    assert_eq!(
        nobody.pull_requests.to_review(&nobody.shell),
        0,
        "nobody signed in is nobody's review queue"
    );
}
