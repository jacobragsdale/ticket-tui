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
            &action,
            AppAction::PullRequestAction {
                id: 11,
                action: crate::sync::PrAction::Complete(options),
                ..
            } if options.strategy == crate::model::MergeStrategy::Merge
                && !options.delete_source
                && options.transition_work_items
                // The head the row was read at travels with the completion.
                && options.last_merge_source_commit == "abc1234"
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

#[test]
fn a_pull_keeps_the_cursor_on_the_same_pull_request_and_a_jump_beats_the_query() {
    let mut app = pull_requests_app();
    app.select_tab(TabId::PullRequests);
    let index = app
        .pull_requests
        .visible(&app.shell)
        .iter()
        .position(|row| row.request.id == 12)
        .expect("!12 is on the table");
    app.pull_requests.cursor.focus(index);

    // A newer pull request arrives at the top of the queue.
    let mut requests: Vec<PullRequest> = app
        .pull_requests
        .visible(&app.shell)
        .into_iter()
        .map(|row| row.request)
        .collect();
    requests.push(pull_request(14, "Newest of all", "Sam", PrStatus::Active));
    let shell = &app.shell;
    app.pull_requests.set_pull_requests(requests, shell);
    assert_eq!(
        app.pull_requests
            .selected(&app.shell)
            .map(|row| row.request.id),
        Some(12),
        "the hand stays on the pull request it was on, one row down"
    );

    // A view that leaves !11 off the table does not stop a reference to it.
    app.pull_requests.apply_view("Mine");
    assert!(
        app.pull_requests
            .visible(&app.shell)
            .iter()
            .all(|row| row.request.id != 11),
        "the view hides it"
    );
    assert!(app.follow(&crate::app::Jump::PullRequest {
        repo: "ticket-tui".into(),
        id: 11,
    }));
    assert_eq!(
        app.pull_requests
            .selected(&app.shell)
            .map(|row| row.request.id),
        Some(11)
    );
    assert!(
        app.pull_requests.query().is_empty() && app.pull_requests.active_view.is_none(),
        "the query is cleared rather than the reference refused"
    );
}

#[test]
fn g_goes_to_the_work_items_the_request_carries_and_says_when_it_carries_none() {
    use crate::app::{Jump, Screen};

    let mut app = pull_requests_app();
    // The work item is on file, which is what makes it somewhere to go.
    app.shell
        .set_work_item_titles(vec![(10_001, "Split the files".to_owned())]);
    app.select_tab(TabId::PullRequests);
    // !11 closes one work item; the rows are newest first, so it is last.
    app.pull_requests.cursor.focus(2);

    assert_eq!(
        Screen::follow_target(&app.pull_requests, &app.shell),
        Ok((Jump::WorkItems(vec![10_001]), "work items"))
    );
    assert_eq!(press(&mut app, KeyCode::Char('g')), AppAction::None);
    assert_eq!(app.tab, TabId::WorkItems, "and the key lands there");
    assert_eq!(app.work_items.query(), "id:10001");

    // !12 carries nothing and has no build behind it either.
    app.select_tab(TabId::PullRequests);
    app.pull_requests.cursor.focus(1);
    assert_eq!(press(&mut app, KeyCode::Char('g')), AppAction::None);
    assert_eq!(app.tab, TabId::PullRequests, "nowhere to go");
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("!12 carries no work items")
    );
}

/// The deployment repository as the tests have it: the one the fixture's pull
/// requests are against, with nothing to render — `preflight_due` builds the
/// request without flying it.
pub(crate) fn deployment(repo: &str) -> crate::preflight::Deployment {
    crate::preflight::Deployment {
        repo: repo.to_owned(),
        clone: std::path::PathBuf::from("/nowhere"),
        render: "true".to_owned(),
        environments: vec![crate::config::Environment {
            name: "qa".to_owned(),
            overlays: vec!["overlays/qa".to_owned()],
            vault: Some("kv-qa".to_owned()),
            ..crate::config::Environment::default()
        }],
    }
}

#[test]
fn a_pull_request_against_the_deployment_repository_is_flown_once_per_head() {
    let mut app = pull_requests_app();
    app.pull_requests
        .set_deployment(Some(deployment("ticket-tui")));

    let request = app
        .pull_requests
        .preflight_due(&app.shell, Vec::new())
        .expect("the selected pull request is flown");
    let crate::local::LocalRequest::Preflight {
        id,
        commit,
        source,
        target,
        ..
    } = request
    else {
        panic!("a pre-flight is what the local thread is asked for");
    };
    assert_eq!(
        (commit.as_str(), source.as_str(), target.as_str()),
        ("abc1234", "feature/tabs", "main")
    );
    assert!(
        app.pull_requests
            .preflight_due(&app.shell, Vec::new())
            .is_none(),
        "one is in the air, so holding j down the table queues no more"
    );

    app.pull_requests
        .set_preflight(id, commit, Ok(crate::preflight::Report::default()));
    assert!(
        app.pull_requests
            .preflight_due(&app.shell, Vec::new())
            .is_none(),
        "a re-selection costs nothing until the branch moves"
    );

    // The branch moves: what was flown at the old head answers nothing.
    let mut moved = app
        .pull_requests
        .selected(&app.shell)
        .expect("a row")
        .request;
    moved.last_merge_source_commit = "def5678".to_owned();
    app.pull_requests.apply_pull_request(&mut app.shell, moved);
    assert!(
        app.pull_requests
            .preflight_due(&app.shell, Vec::new())
            .is_some(),
        "a head nothing was flown at is flown"
    );
}

#[test]
fn r_flies_the_selected_pull_request_again() {
    let mut app = pull_requests_app();
    app.pull_requests
        .set_deployment(Some(deployment("ticket-tui")));
    let request = app
        .pull_requests
        .preflight_due(&app.shell, Vec::new())
        .expect("flown");
    let crate::local::LocalRequest::Preflight { id, commit, .. } = request else {
        panic!("a pre-flight");
    };
    app.pull_requests
        .set_preflight(id, commit, Ok(crate::preflight::Report::default()));
    assert!(
        app.pull_requests
            .preflight_due(&app.shell, Vec::new())
            .is_none()
    );

    let action = app
        .pull_requests
        .run_command(&mut app.shell, crate::command::CommandId::Sync);
    assert!(matches!(action, AppAction::Sync), "r still syncs");
    assert!(
        app.pull_requests
            .preflight_due(&app.shell, Vec::new())
            .is_some(),
        "and flies the selected pull request again"
    );
}

#[test]
fn a_pull_request_on_any_other_repository_carries_no_pre_flight_at_all() {
    let mut app = pull_requests_app();
    app.pull_requests
        .set_deployment(Some(deployment("somewhere-else")));

    assert!(
        app.pull_requests
            .preflight_due(&app.shell, Vec::new())
            .is_none()
    );
    let row = app.pull_requests.selected(&app.shell).expect("a row");
    assert!(app.pull_requests.preflight(&row).is_none());
    assert!(
        app.pull_requests.preflight_notes(&row).is_none(),
        "no section is drawn for it"
    );
    assert_eq!(
        app.pull_requests
            .agent_context(&app.shell)
            .selected
            .expect("a selected pull request")
            .preflight,
        crate::agent_context::PreflightContext::NotApplicable
    );
}

#[test]
fn a_follow_that_finds_nothing_leaves_the_history_and_the_forward_list_as_they_were() {
    use crate::app::Jump;

    let mut app = pull_requests_app();
    app.select_tab(TabId::PullRequests);
    app.shell.record_jump(Jump::Repo("somewhere".to_owned()));
    app.shell.future.push(Jump::Repo("ahead".to_owned()));
    let history = app.shell.history().to_vec();

    assert!(!app.follow(&Jump::Run(999_999)), "nothing on file");
    assert_eq!(app.shell.history(), history.as_slice());
    assert_eq!(app.shell.future.len(), 1, "`]` still has somewhere to go");
}

#[test]
fn arriving_from_a_tab_with_no_place_still_records_the_arrival() {
    use crate::app::Jump;

    let mut app = pull_requests_app();
    app.select_tab(TabId::WorkItems);
    app.shell.history.clear();
    press(&mut app, KeyCode::Char('3'));
    assert!(
        matches!(app.shell.history().last(), Some(Jump::PullRequest { .. })),
        "{:?}",
        app.shell.history()
    );
}

#[test]
fn g_offers_only_the_work_items_and_the_run_the_database_holds() {
    use crate::app::{Jump, Screen};

    let mut app = pull_requests_app();
    app.select_tab(TabId::PullRequests);
    app.pull_requests.cursor.focus(2);
    app.shell.set_work_item_titles(Vec::new());
    let target = Screen::follow_target(&app.pull_requests, &app.shell);
    assert!(
        !matches!(target, Ok((Jump::WorkItems(_), _))),
        "a work item the query leaves out is not somewhere to go: {target:?}"
    );
}
