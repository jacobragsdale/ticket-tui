use crate::app::pull_requests::PrMode;
use crate::app::{App, AppAction, TabId};
use crate::model::{Identity, PrBuild, PrReviewer, PrStatus, PullRequest, Repo};
use crate::timestamp::ts;
use crossterm::event::KeyCode;

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
        threads: Vec::new(),
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

#[test]
fn every_vote_key_writes_its_value_and_changes_the_glyph_at_once() {
    let mut app = pull_requests_app();
    app.select_tab(TabId::PullRequests);
    // The one waiting on me.
    app.pull_requests.cursor.focus(2);

    for (key, vote) in [
        (KeyCode::Char('a'), 10),
        (KeyCode::Char('A'), 5),
        (KeyCode::Char('w'), -5),
        (KeyCode::Char('x'), -10),
    ] {
        let action = app.handle_key(crossterm::event::KeyEvent::new(
            key,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(
            matches!(
                action,
                AppAction::VotePullRequest { id: 11, vote: sent, .. } if sent == vote
            ),
            "{key:?} writes {vote}, got {action:?}"
        );
        assert_eq!(
            app.pull_requests.my_vote(11, "Jacob Ragsdale"),
            vote,
            "and the glyph changes at once"
        );
        app.pull_requests.vote_accepted(11);
    }
}

#[test]
fn a_refused_vote_puts_the_glyph_back_and_says_why() {
    let mut app = pull_requests_app();
    app.select_tab(TabId::PullRequests);
    app.pull_requests.cursor.focus(2);

    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Char('a'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(app.pull_requests.my_vote(11, "Jacob Ragsdale"), 10);

    app.pull_requests
        .vote_rejected(&mut app.shell, 11, "you are not a reviewer");
    assert_eq!(
        app.pull_requests.my_vote(11, "Jacob Ragsdale"),
        0,
        "the vote that was there comes back"
    );
    assert!(
        app.shell
            .notification()
            .is_some_and(|(message, _)| message.contains("not a reviewer")),
    );
}

#[test]
fn u_puts_the_last_vote_back() {
    let mut app = pull_requests_app();
    app.select_tab(TabId::PullRequests);
    app.pull_requests.cursor.focus(2);

    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Char('a'),
        crossterm::event::KeyModifiers::NONE,
    ));
    app.pull_requests.vote_accepted(11);
    assert_eq!(app.pull_requests.my_vote(11, "Jacob Ragsdale"), 10);

    let action = app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Char('u'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(
        matches!(
            action,
            AppAction::VotePullRequest {
                id: 11,
                vote: 0,
                ..
            }
        ),
        "undo writes the vote that was there before, got {action:?}"
    );
    assert_eq!(app.pull_requests.my_vote(11, "Jacob Ragsdale"), 0);
}

#[test]
fn voting_on_a_pull_request_i_am_not_a_reviewer_of_adds_me() {
    let mut app = pull_requests_app();
    app.select_tab(TabId::PullRequests);
    // The draft, which nobody has asked me to review.
    app.pull_requests.cursor.focus(1);

    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Char('a'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(
        app.pull_requests.my_vote(12, "Jacob Ragsdale"),
        10,
        "which is what the endpoint does"
    );
}

fn press(app: &mut App, code: KeyCode) -> AppAction {
    app.handle_key(crossterm::event::KeyEvent::new(
        code,
        crossterm::event::KeyModifiers::NONE,
    ))
}

#[test]
fn the_completion_form_sends_the_options_it_was_left_on() {
    let mut app = pull_requests_app();
    app.select_tab(TabId::PullRequests);
    app.pull_requests.cursor.focus(2);

    press(&mut app, KeyCode::Char('C'));
    assert_eq!(app.pull_requests.mode, PrMode::Complete);
    assert_eq!(
        app.pull_requests.completion().strategy,
        crate::model::MergeStrategy::Squash,
        "squash is the default, as it is in the web UI"
    );

    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char(' '));
    let action = press(&mut app, KeyCode::Enter);

    assert!(
        matches!(
            action,
            AppAction::PullRequestAction {
                id: 11,
                action: crate::sync::PrAction::Complete(options),
                ..
            } if options.strategy == crate::model::MergeStrategy::Merge
                && !options.delete_source
                && options.transition_work_items
        ),
        "got {action:?}"
    );
}

#[test]
fn completing_is_refused_here_when_the_merge_cannot_happen() {
    let mut app = pull_requests_app();
    app.select_tab(TabId::PullRequests);
    // The one with conflicts.
    app.pull_requests.cursor.focus(0);

    press(&mut app, KeyCode::Char('C'));
    assert_eq!(app.pull_requests.mode, PrMode::Browse, "no form opens");
    assert!(
        app.shell
            .notification()
            .is_some_and(|(message, _)| message.contains("conflicts") && message.contains("o")),
        "it says what is wrong and what to press"
    );
}

#[test]
fn x_asks_before_abandoning_and_t_toggles_auto_complete() {
    let mut app = pull_requests_app();
    app.select_tab(TabId::PullRequests);
    app.pull_requests.cursor.focus(2);

    press(&mut app, KeyCode::Char('X'));
    assert_eq!(app.pull_requests.mode, PrMode::ConfirmAbandon);
    let action = press(&mut app, KeyCode::Char('X'));
    assert!(
        matches!(
            action,
            AppAction::PullRequestAction {
                id: 11,
                action: crate::sync::PrAction::Abandon,
                ..
            }
        ),
        "got {action:?}"
    );

    // Turning auto-complete on asks how it should land first.
    let action = press(&mut app, KeyCode::Char('t'));
    assert!(matches!(action, AppAction::None));
    assert_eq!(app.pull_requests.mode, PrMode::Complete);
    let action = press(&mut app, KeyCode::Enter);
    assert!(
        matches!(
            action,
            AppAction::PullRequestAction {
                action: crate::sync::PrAction::AutoComplete(true),
                ..
            }
        ),
        "got {action:?}"
    );
}

#[test]
fn n_posts_one_comment_and_it_joins_the_discussion() {
    let mut app = pull_requests_app();
    app.select_tab(TabId::PullRequests);
    app.pull_requests.cursor.focus(2);

    press(&mut app, KeyCode::Char('n'));
    let action = press(&mut app, KeyCode::Enter);
    assert!(
        matches!(action, AppAction::None),
        "an empty comment is refused here rather than posted"
    );
    for character in "LGTM once CI is green".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    let action = press(&mut app, KeyCode::Enter);
    assert!(
        matches!(
            action,
            AppAction::CommentOnPullRequest { id: 11, ref text, .. }
                if text == "LGTM once CI is green"
        ),
        "got {action:?}"
    );

    app.pull_requests.apply_comment(
        &mut app.shell,
        11,
        crate::model::PrThread {
            id: 3,
            author: "Jacob Ragsdale".into(),
            text: "LGTM once CI is green".into(),
            published_at: Some(ts("2026-08-29T12:00:00Z")),
            status: "active".into(),
        },
    );
    let row = app.pull_requests.selected(&app.shell).expect("a row");
    assert_eq!(row.request.threads.len(), 1);
}

#[test]
fn a_completed_pull_request_leaves_the_active_view() {
    let mut app = pull_requests_app();
    app.select_tab(TabId::PullRequests);
    app.pull_requests.apply_view("Active");
    assert_eq!(app.pull_requests.visible(&app.shell).len(), 3);

    let mut landed = pull_request(11, "Split the files", "Avery", PrStatus::Completed);
    landed.closed_at = Some(ts("2026-08-29T12:00:00Z"));
    app.pull_requests.apply_pull_request(&mut app.shell, landed);

    assert_eq!(
        app.pull_requests.visible(&app.shell).len(),
        2,
        "it is not active any more"
    );
    assert!(
        app.shell
            .notification()
            .is_some_and(|(message, _)| message.contains("completed")),
    );
}
