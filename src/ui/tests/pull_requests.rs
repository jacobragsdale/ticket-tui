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
    app.shell
        .set_work_item_titles(vec![(10_001, "Split the files".to_owned())]);
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

#[test]
fn the_details_header_carries_the_chip_g_would_follow_and_the_footer_names_it() {
    let mut app = pull_requests_app();
    app.shell
        .set_work_item_titles(vec![(10_001, "Split the files".to_owned())]);
    tab_text(160, 50, &mut app);
    app.pull_requests.cursor.focus(2);
    let text = render_text(160, 50, &mut app);

    assert!(text.contains("[Go to work items]"), "{text}");
    assert!(
        text.contains("g work items"),
        "and the footer names the key: {text}"
    );

    let (y, x) = text
        .lines()
        .enumerate()
        .find_map(|(y, line)| {
            line.find("[Go to work items]")
                .map(|x| (u16::try_from(y).unwrap(), u16::try_from(x).unwrap()))
        })
        .expect("the chip is on screen");
    click(&mut app, x + 2, y);
    assert_eq!(app.tab, TabId::WorkItems, "the chip follows on a click");
    assert_eq!(app.work_items.query(), "id:10001");

    // A request that carries nothing has no chip, and the footer does not
    // offer the key either.
    app.select_tab(TabId::PullRequests);
    app.pull_requests.cursor.focus(1);
    let text = render_text(160, 50, &mut app);
    assert!(!text.contains("[Go to"), "{text}");
    assert!(!text.contains("  g "), "{text}");
}

/// One finding, as a pre-flight of the deployment repository would report it.
fn missing(object: crate::kustomize::ObjectKind, key: &str) -> crate::kustomize::Finding {
    crate::kustomize::Finding {
        environment: "qa".into(),
        namespace: "shop-qa".into(),
        workload: "billing-api".into(),
        kind: "Deployment".into(),
        container: Some("api".into()),
        reference: crate::kustomize::Reference {
            source: crate::kustomize::Source::Env { var: key.into() },
            object,
            name: "billing-kv".into(),
            key: Some(key.into()),
            optional: false,
        },
        missing: crate::kustomize::Missing::Key,
        vault: None,
    }
}

#[test]
fn the_pre_flight_section_says_what_would_be_missing_and_its_lines_are_a_click_away() {
    let mut app = pull_requests_app();
    app.pull_requests
        .set_deployment(Some(crate::app::pull_requests::tests::deployment(
            "ticket-tui",
        )));
    // Wide enough that every column is on the table, the Pre-flight one last.
    tab_text(220, 50, &mut app);
    let row = app.pull_requests.selected(&app.shell).expect("a row");
    app.pull_requests.set_preflight(
        row.request.id,
        row.request.last_merge_source_commit.clone(),
        Ok(crate::preflight::Report {
            rendered: vec![("qa".into(), "overlays/qa".into())],
            findings: vec![missing(crate::kustomize::ObjectKind::Secret, "SIGNING_KEY")],
            ..crate::preflight::Report::default()
        }),
    );
    let text = render_text(220, 50, &mut app);

    assert!(text.contains("Pre-flight"), "the section is drawn: {text}");
    assert!(
        text.contains("SIGNING_KEY"),
        "it says what would be missing: {text}"
    );
    assert!(
        text.contains("\u{2717}1"),
        "and the column counts it: {text}"
    );

    // The same line the eye follows is the line the mouse follows.
    let line = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::Follow(Jump::Vault(_))))
        .expect("a missing secret key points at the vault")
        .rect;
    click(&mut app, line.x + 6, line.y);
    let (message, _) = app.shell.notification().expect("the jump is answered");
    assert!(message.contains("kv-qa"), "{message}");
}

#[test]
fn an_overlay_that_renders_clean_says_so_and_points_nowhere() {
    let mut app = pull_requests_app();
    app.pull_requests
        .set_deployment(Some(crate::app::pull_requests::tests::deployment(
            "ticket-tui",
        )));
    tab_text(160, 50, &mut app);
    let row = app.pull_requests.selected(&app.shell).expect("a row");
    app.pull_requests.set_preflight(
        row.request.id,
        row.request.last_merge_source_commit.clone(),
        Ok(crate::preflight::Report {
            rendered: vec![("qa".into(), "overlays/qa".into())],
            findings: Vec::new(),
            ..crate::preflight::Report::default()
        }),
    );
    let text = render_text(160, 50, &mut app);

    assert!(
        text.contains("qa overlays/qa renders clean"),
        "the clean line names the overlay it rendered: {text}"
    );
    assert!(
        app.shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::Follow(Jump::Vault(_))))
            .is_none(),
        "a clean line points nowhere"
    );
}
