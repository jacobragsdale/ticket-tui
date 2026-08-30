use super::*;
use crate::app::pull_requests::tests::pull_requests_app;
use crate::app::{Jump, TabId};

fn tab_text(width: u16, height: u16, app: &mut App) -> String {
    app.select_tab(TabId::PullRequests);
    render_text(width, height, app)
}

#[test]
fn the_table_draws_a_row_per_pull_request_with_its_votes_and_build() {
    let mut app = pull_requests_app();
    let text = tab_text(200, 24, &mut app);

    assert!(pane_reads(&text, "Pull requests", "3"), "{text}");
    assert!(
        text.contains("!11"),
        "the id reads as a pull request: {text}"
    );
    assert!(text.contains("Split the files"), "{text}");
    assert!(text.contains("feature/tabs \u{2192} main"), "{text}");
    assert!(
        text.contains("[draft]"),
        "a draft says so after its title: {text}"
    );
    assert!(
        text.contains("\u{26a0} conflicts"),
        "a merge that cannot happen is what the Build column says: {text}"
    );
    assert!(
        text.contains("Closed hidden (1)"),
        "the chip says what is being left out: {text}"
    );
}

#[test]
fn the_details_pane_carries_every_section_the_epic_asks_for() {
    let mut app = pull_requests_app();
    tab_text(160, 50, &mut app);
    // The one with reviewers, a work item and a build on it.
    app.pull_requests.cursor.focus(2);
    let text = render_text(160, 50, &mut app);

    assert!(text.contains("Active"), "{text}");
    assert!(
        text.contains("What it does and why"),
        "the description is rendered as text: {text}"
    );
    assert!(text.contains("Reviewers"), "{text}");
    assert!(text.contains("required"), "{text}");
    assert!(
        text.contains("no vote") || text.contains("approved"),
        "{text}"
    );
    assert!(text.contains("Related"), "{text}");
    assert!(text.contains("Completion"), "{text}");
    assert!(text.contains("Auto-complete: off"), "{text}");
    assert!(
        text.contains(" Approve ") && text.contains(" Abandon "),
        "every button is drawn as a chip: {text}"
    );
}

#[test]
fn clicking_a_row_moves_the_cursor_and_the_chip_puts_the_closed_ones_back() {
    let mut app = pull_requests_app();
    tab_text(150, 24, &mut app);

    let row = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::TableRow { index: 1 }))
        .expect("every row is clickable")
        .rect;
    click(&mut app, row.x + 20, row.y);
    assert_eq!(app.pull_requests.cursor.index, 1);

    let chip = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::ShowFinished))
        .expect("the chip is clickable")
        .rect;
    click(&mut app, chip.x + 2, chip.y);
    assert_eq!(app.pull_requests.visible(&app.shell).len(), 4);
}

#[test]
fn the_tab_badge_counts_what_is_waiting_on_my_review() {
    let mut app = pull_requests_app();
    let text = tab_text(150, 24, &mut app);
    let bar = text.lines().next().expect("the tab bar");
    assert!(
        bar.contains("3 Pull requests 1"),
        "the badge is the review queue: {bar}"
    );
}

#[test]
fn the_details_pane_follows_a_pull_request_to_its_work_items() {
    let mut app = pull_requests_app();
    tab_text(160, 50, &mut app);
    app.pull_requests.cursor.focus(2);
    render_text(160, 50, &mut app);
    let work_items = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::Follow(Jump::WorkItems(_))))
        .expect("the work items line is a jump")
        .rect;

    click(&mut app, work_items.x + 16, work_items.y);
    assert_eq!(app.tab, TabId::WorkItems);
    assert_eq!(app.work_items.query(), "id:10001");
}

#[test]
fn a_pull_request_follows_its_work_items_by_name_and_its_build_run() {
    let mut app = pull_requests_app();
    app.shell
        .set_work_item_titles(vec![(10_001, "Split the files".to_owned())]);
    tab_text(160, 50, &mut app);
    app.pull_requests.cursor.focus(2);
    render_text(160, 50, &mut app);

    let text = render_text(160, 50, &mut app);
    assert!(
        text.contains("#10001  Split the files"),
        "a linked work item reads as the work items tab has it: {text}"
    );

    let work_item = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::Follow(Jump::WorkItems(_))))
        .expect("the work item line is a jump")
        .rect;
    click(&mut app, work_item.x + 16, work_item.y);
    assert_eq!(app.tab, TabId::WorkItems);
    assert_eq!(app.work_items.query(), "id:10001");

    // Back, then follow the build to the run that gates it.
    app.handle_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE));
    render_text(160, 50, &mut app);
    let build = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::Follow(Jump::Run(_))));
    assert!(build.is_some(), "the build line is a jump too");
}

#[test]
fn the_buttons_under_the_details_pane_are_the_keys_they_name() {
    let mut app = pull_requests_app();
    tab_text(160, 50, &mut app);
    let selected = app
        .pull_requests
        .selected(&app.shell)
        .expect("a pull request under the cursor")
        .request
        .id;

    let approve = target_rect(&app, |target| {
        matches!(
            target,
            PointerTarget::RunCommand(crate::command::CommandId::ApprovePr)
        )
    });
    let action = click(&mut app, approve.x + 2, approve.y);
    assert!(
        matches!(action, crate::app::AppAction::VotePullRequest { id, vote: 10, .. } if id == selected),
        "the button votes on the pull request under the cursor, got {action:?}"
    );

    let abandon = target_rect(&app, |target| {
        matches!(
            target,
            PointerTarget::RunCommand(crate::command::CommandId::AbandonPr)
        )
    });
    click(&mut app, abandon.x + 2, abandon.y);
    assert_eq!(
        app.pull_requests.mode,
        crate::app::pull_requests::PrMode::ConfirmAbandon,
        "and asks once more, the way the key does"
    );
}
