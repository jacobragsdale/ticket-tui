use super::*;
use crate::app::{Jump, TabId};
use crate::model::TicketKey;
use crate::ui::render_tab_bar;

/// One key, the way the event loop sends it.
fn press(app: &mut App, code: KeyCode) -> crate::app::AppAction {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// Every cell of one row, with the style of the cell at `x`.
fn row_text(terminal: &Terminal<TestBackend>, y: u16, width: u16) -> String {
    let buffer = terminal.backend().buffer();
    (0..width).map(|x| buffer[(x, y)].symbol()).collect()
}

#[test]
fn the_tab_bar_names_seven_tabs_and_marks_the_one_showing_at_every_breakpoint() {
    let mut app = App::new(vec![ticket()]);
    for width in [120, 90] {
        let text = render_text(width, 30, &mut app);
        let bar = text.lines().next().expect("a tab bar row").to_owned();
        assert!(bar.contains("1 Work items"), "{width} columns: {bar}");
        assert!(bar.contains("4 Pipelines"), "{width} columns: {bar}");
        assert!(bar.contains("5 AKS"), "{width} columns: {bar}");
        assert!(bar.contains("6 ACR"), "{width} columns: {bar}");
        assert!(bar.contains("7 Key Vault"), "{width} columns: {bar}");
    }

    // Narrower than the seven names, they shorten but every tab stays.
    let short = render_text(60, 30, &mut app);
    let bar = short.lines().next().expect("a tab bar row");
    assert!(bar.contains("1 Items"), "{bar}");
    assert!(bar.contains("4 Runs"), "{bar}");
    assert!(bar.contains("5 AKS"), "{bar}");
    assert!(bar.contains("6 ACR"), "{bar}");
    assert!(bar.contains("7 Vault"), "{bar}");

    // Narrower than that, the numbers stand alone and stay clickable.
    let narrow = render_text(40, 30, &mut app);
    let bar = narrow.lines().next().expect("a tab bar row");
    assert!(bar.contains(" 1 ") && bar.contains(" 5 "), "{bar}");
    assert!(bar.contains(" 6 ") && bar.contains(" 7 "), "{bar}");
    assert!(
        app.shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::SelectTab { index: 6 }))
            .is_some(),
        "the last tab keeps its click target when its name is shortened"
    );

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let active = buffer[(2, 0)].style();
    assert!(
        active.add_modifier.contains(Modifier::BOLD),
        "the tab showing is bold"
    );
    if theme().surface == Color::Reset {
        assert!(
            active.add_modifier.contains(Modifier::REVERSED),
            "with no ground to sit on it reverses instead"
        );
    } else {
        assert_eq!(active.bg, Some(theme().surface), "and sits on the surface");
        assert_eq!(active.fg, Some(theme().accent));
    }
    let inactive_x = u16::try_from(row_text(&terminal, 0, 120).find("2 Repos").unwrap()).unwrap();
    let inactive = buffer[(inactive_x, 0)].style();
    assert!(
        !inactive.add_modifier.contains(Modifier::BOLD),
        "the others are quiet"
    );
}

#[test]
fn the_number_keys_switch_tabs_and_the_screens_keep_their_own_state() {
    let mut app = App::new(vec![ticket()]);
    app.work_items
        .set_query(&mut app.shell, "state:Active".into());
    press(&mut app, KeyCode::Char('2'));
    assert_eq!(app.tab, TabId::Repos);

    let text = render_text(110, 30, &mut app);
    assert!(pane_reads(&text, "Repos", "0"), "{text}");
    assert!(
        !text.contains("Fix ticket search"),
        "the work items screen is not painted while another tab is showing"
    );

    press(&mut app, KeyCode::Char('1'));
    assert_eq!(app.tab, TabId::WorkItems);
    assert_eq!(
        app.work_items.query(),
        "state:Active",
        "the query the screen was left with came back with it"
    );
}

#[test]
fn a_digit_typed_into_the_search_box_stays_in_the_search_box() {
    let mut app = App::new(vec![ticket()]);
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('2'));

    assert_eq!(app.tab, TabId::WorkItems, "the tab did not move");
    assert_eq!(app.work_items.query(), "2", "the digit was typed");
}

#[test]
fn switching_tabs_closes_whatever_the_screen_had_open() {
    let mut app = App::new(vec![ticket()]);
    press(&mut app, KeyCode::Char('?'));
    assert_eq!(app.work_items.mode, WorkItemMode::Help);

    press(&mut app, KeyCode::Char('3'));
    assert_eq!(app.tab, TabId::PullRequests);
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Browse,
        "the help came down on the way out"
    );
}

#[test]
fn clicking_a_tab_switches_to_it() {
    let mut app = App::new(vec![ticket()]);
    render_text(110, 30, &mut app);
    let pipelines = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::SelectTab { index: 3 }))
        .expect("every tab is clickable")
        .rect;

    click(&mut app, pipelines.x, pipelines.y);
    assert_eq!(app.tab, TabId::Pipelines);
    assert!(
        pane_reads(&render_text(110, 30, &mut app), "Pipelines", "0"),
        "the tab that was clicked is the one showing"
    );
}

#[test]
fn a_tab_with_something_waiting_wears_a_badge() {
    let mut shell = crate::app::Shell::default();
    let tabs = vec![
        (TabId::WorkItems, true, None),
        (TabId::Repos, false, None),
        (TabId::PullRequests, false, Some("3".to_owned())),
        (TabId::Pipelines, false, Some("◐2".to_owned())),
        (TabId::Aks, false, Some("✗1".to_owned())),
        (TabId::Acr, false, None),
        (TabId::KeyVault, false, None),
    ];
    let mut terminal = Terminal::new(TestBackend::new(100, 3)).unwrap();
    terminal
        .draw(|frame| {
            let area = Rect::new(0, 0, 100, 1);
            render_tab_bar(frame, &mut shell, &tabs, area);
        })
        .unwrap();

    let bar = row_text(&terminal, 0, 100);
    assert!(bar.contains("3 Pull requests 3"), "{bar}");
    assert!(bar.contains("4 Pipelines ◐2"), "{bar}");
    assert!(bar.contains("5 AKS ✗1"), "{bar}");
    assert!(
        shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::SelectTab { index: 2 }))
            .is_some(),
        "a badged tab is still clickable"
    );
}

#[test]
fn the_digits_reach_the_registry_and_vault_tabs_and_each_says_why_it_is_empty() {
    let mut app = App::new(vec![ticket()]);

    press(&mut app, KeyCode::Char('6'));
    assert_eq!(app.tab, TabId::Acr);
    let text = render_text(120, 30, &mut app);
    assert!(pane_reads(&text, "Registries", "0 registries"), "{text}");
    assert!(
        text.contains("Reading the subscription"),
        "the details pane says the read is on its way: {text}"
    );

    press(&mut app, KeyCode::Char('7'));
    assert_eq!(app.tab, TabId::KeyVault);
    let text = render_text(120, 30, &mut app);
    assert!(pane_reads(&text, "Vaults", "0 vaults"), "{text}");
    assert!(
        text.contains("Reading the subscription"),
        "the details pane says the read is on its way: {text}"
    );

    // With no subscription to read, both tabs say so instead: the reason is
    // what tells a missing subscription from an empty one.
    app.shell
        .set_arm_state(Some("Not signed in to Azure: run `az login`".to_owned()));
    let text = render_text(120, 30, &mut app);
    assert!(text.contains("Not signed in to Azure"), "{text}");
    press(&mut app, KeyCode::Char('6'));
    let text = render_text(120, 30, &mut app);
    assert!(text.contains("Not signed in to Azure"), "{text}");
}

#[test]
fn the_agent_context_says_which_arm_tab_is_showing_and_whether_it_can_read() {
    let mut app = App::new(vec![ticket()]);
    app.select_tab(TabId::Acr);
    app.shell.set_arm_state(Some(
        "no Azure subscription: pass --subscription, set TICKET_TUI_SUBSCRIPTION, or run `az account set`"
            .to_owned(),
    ));

    let context = app.agent_context();
    assert_eq!(context.active_tab, "acr");
    assert_eq!(context.acr.level, "registries");
    assert_eq!(context.key_vault.level, "vaults");
    assert!(context.arm.offline, "no subscription resolved");
    assert!(context.arm.subscription.is_none());
    assert!(
        context
            .arm
            .last_error
            .as_deref()
            .is_some_and(|reason| reason.contains("--subscription")),
        "the reason names what to do about it: {:?}",
        context.arm.last_error
    );

    // With one resolved, the tabs are online and the document names it.
    app.shell.set_arm_state(None);
    app.shell
        .set_arm_subscription(Some("00000000-0000-0000-0000-000000000000".to_owned()));
    app.select_tab(TabId::KeyVault);
    let context = app.agent_context();
    assert_eq!(context.active_tab, "key_vault");
    assert!(!context.arm.offline);
    assert_eq!(
        context.arm.subscription.as_deref(),
        Some("00000000-0000-0000-0000-000000000000")
    );
    assert!(context.arm.last_error.is_none());
}

#[test]
fn the_history_walks_back_and_forward_across_tabs() {
    let mut second = ticket();
    second.key.id = 10_002;
    second.title = "Second ticket".into();
    let mut app = App::new(vec![ticket(), second]);
    render_text(110, 30, &mut app);

    // Two work items, then somewhere on another tab. The repository has to be
    // on file for the jump back to it to land.
    app.shell.set_repos(vec![crate::model::Repo {
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
    app.repos.set_repos(&app.shell);
    app.work_items.select_row(&mut app.shell, 0);
    app.work_items.record_visit(&mut app.shell);
    app.work_items.select_row(&mut app.shell, 1);
    app.work_items.record_visit(&mut app.shell);
    app.select_tab(TabId::Repos);
    app.shell.record_jump(Jump::Repo("ticket-tui".to_owned()));

    press(&mut app, KeyCode::Char('['));
    assert_eq!(app.tab, TabId::WorkItems, "back crosses tabs");
    assert_eq!(app.work_items.selected_ticket().unwrap().key.id, 10_002);

    press(&mut app, KeyCode::Char(']'));
    assert_eq!(app.tab, TabId::Repos, "and forward crosses back");

    press(&mut app, KeyCode::Char('['));
    press(&mut app, KeyCode::Char('['));
    assert_eq!(app.tab, TabId::WorkItems);
    assert_eq!(
        app.work_items.selected_ticket().unwrap().key.id,
        10_001,
        "two steps back is the work item before the last one"
    );
}

#[test]
fn a_jump_to_something_this_database_does_not_hold_says_so_and_stays_put() {
    let mut app = App::new(vec![ticket()]);
    render_text(110, 30, &mut app);

    let followed = app.follow(&Jump::WorkItem(TicketKey {
        organization: "demo".into(),
        id: 4_242,
    }));

    assert!(!followed);
    assert_eq!(app.tab, TabId::WorkItems, "the tab did not move");
    let (message, level) = app.shell.notification().expect("a refusal is reported");
    assert_eq!(message, "Work item #4242 is not in this database");
    assert_eq!(level, crate::app::NotificationLevel::Error);
}

#[test]
fn following_several_work_items_at_once_filters_the_table_to_them() {
    let mut second = ticket();
    second.key.id = 10_002;
    second.title = "Second ticket".into();
    let mut third = ticket();
    third.key.id = 10_003;
    third.title = "Third ticket".into();
    let mut app = App::new(vec![ticket(), second, third]);

    assert!(app.follow(&Jump::WorkItems(vec![10_001, 10_003])));
    assert_eq!(app.work_items.query(), "id:10001 id:10003");
    assert_eq!(
        app.work_items
            .visible_tickets()
            .map(|ticket| ticket.key.id)
            .collect::<Vec<_>>(),
        vec![10_001, 10_003],
        "exactly the two the jump named"
    );
}

#[test]
fn a_click_on_the_tab_bar_releases_the_press_it_started_with() {
    let mut app = App::new(vec![ticket()]);
    let text = render_text(120, 30, &mut app);
    let bar = text.lines().next().expect("a tab bar row");
    let x = u16::try_from(bar.find("2 Repos").unwrap()).unwrap();
    click(&mut app, x, 0);
    assert_eq!(app.tab, TabId::Repos, "the click switched tabs");
    assert!(
        !app.shell.pointer.is_pressed(),
        "the release that switched tabs also ended the press: nothing is being dragged"
    );
    // Back on the work items, moving the mouse is a hover, not a drag that
    // paints a text selection behind the pointer.
    press(&mut app, KeyCode::Char('1'));
    render_text(120, 30, &mut app);
    app.handle_mouse(mouse(MouseEventKind::Moved, 30, 9));
    app.handle_mouse(mouse(MouseEventKind::Moved, 30, 12));
    assert!(
        app.shell.selection().is_none(),
        "no text selection follows a hover"
    );
    assert!(matches!(
        app.shell.pointer.drag(),
        crate::pointer::DragKind::None
    ));
}

#[test]
fn help_palette_columns_and_database_open_over_every_tab() {
    let mut app = crate::app::pull_requests::tests::pull_requests_app();
    app.select_tab(TabId::PullRequests);

    // `?` on the pull requests: the help, listing this tab's own keys.
    press(&mut app, KeyCode::Char('?'));
    let text = render_text(120, 40, &mut app);
    assert!(text.contains(" Help "), "{text}");
    // The popup is a screenful tall, so the tab's own section is paged to.
    let mut seen = text;
    for _ in 0..12 {
        if seen.contains("Approve with suggestions") {
            break;
        }
        press(&mut app, KeyCode::PageDown);
        seen = render_text(120, 40, &mut app);
    }
    assert!(
        seen.contains("Approve with suggestions"),
        "the help lists the tab's own verbs: {seen}"
    );
    press(&mut app, KeyCode::Esc);
    assert!(!render_text(120, 40, &mut app).contains(" Help "));
    assert_eq!(app.tab, TabId::PullRequests, "and the tab is still showing");

    // `p`: the palette lists the pull requests' commands and runs one there.
    press(&mut app, KeyCode::Char('p'));
    for character in "wait".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    let text = render_text(120, 40, &mut app);
    assert!(text.contains(" Commands "), "{text}");
    assert!(text.contains("Wait for author"), "{text}");
    let selected = app
        .pull_requests
        .selected(&app.shell)
        .expect("a pull request under the cursor")
        .request
        .id;
    let action = press(&mut app, KeyCode::Enter);
    assert!(
        matches!(action, crate::app::AppAction::VotePullRequest { id, vote: -5, .. } if id == selected),
        "the palette's choice runs on the tab it was opened for, got {action:?}"
    );
    assert!(!render_text(120, 40, &mut app).contains(" Commands "));

    // `c`: the columns editor edits this tab's columns, not the work items'.
    press(&mut app, KeyCode::Char('c'));
    let text = render_text(120, 40, &mut app);
    assert!(text.contains(" Columns "), "{text}");
    assert!(
        text.contains("Votes"),
        "the editor lists the pull request columns: {text}"
    );
    let visible = |layout: &dyn crate::columns::ColumnLayout| {
        (0..layout.count())
            .map(|index| layout.is_visible(index))
            .collect::<Vec<bool>>()
    };
    let before = visible(&app.pull_requests.layout);
    let work_items_before = visible(&app.work_items.layout);
    // Past the pinned id and title columns, which the editor refuses to hide.
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char(' '));
    assert_ne!(
        visible(&app.pull_requests.layout),
        before,
        "Space toggles a pull request column"
    );
    assert_eq!(
        visible(&app.work_items.layout),
        work_items_before,
        "and leaves the work items' columns alone"
    );
    press(&mut app, KeyCode::Esc);

    // `i`: the database overlay.
    press(&mut app, KeyCode::Char('i'));
    let text = render_text(120, 40, &mut app);
    assert!(text.contains(" Database "), "{text}");
    press(&mut app, KeyCode::Char('i'));
    assert!(!render_text(120, 40, &mut app).contains(" Database "));

    // A digit typed into the palette's filter is a character, not a tab.
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Char('1'));
    assert_eq!(app.tab, TabId::PullRequests);
    press(&mut app, KeyCode::Esc);
}

#[test]
fn g_goes_from_a_work_item_to_the_pull_request_that_carried_it() {
    use crate::app::pull_requests::tests::pull_request;
    use crate::model::{ArtifactKind, ArtifactLink, PrStatus, TicketGraph};

    let mut app = App::new(vec![ticket()]);
    let key = app.work_items.tickets()[0].key.clone();
    let link = |kind| ArtifactLink {
        work_item: key.clone(),
        kind,
        name: "Pull Request".to_owned(),
    };
    app.shell.set_repos(vec![crate::app::repos::tests::repo(
        "aaa-111",
        "ticket-tui",
        false,
    )]);
    app.repos.set_repos(&app.shell);
    app.shell.set_artifact_labels(
        vec![
            (41, "Earlier work".to_owned(), PrStatus::Completed),
            (42, "Split the files".to_owned(), PrStatus::Active),
        ],
        Vec::new(),
    );
    app.work_items.set_workspace_graph(
        &mut app.shell,
        TicketGraph {
            artifacts: vec![
                // The completed one is the newer number, and still loses to
                // the one that is open.
                link(ArtifactKind::PullRequest {
                    repo_id: "aaa-111".into(),
                    id: 43,
                }),
                link(ArtifactKind::PullRequest {
                    repo_id: "aaa-111".into(),
                    id: 42,
                }),
            ],
            ..TicketGraph::default()
        },
    );
    let requests = vec![
        pull_request(42, "Split the files", "Avery", PrStatus::Active),
        pull_request(43, "Earlier work", "Avery", PrStatus::Completed),
    ];
    let shell = &app.shell;
    app.pull_requests.set_pull_requests(requests, shell);
    app.shell.set_artifact_labels(
        vec![
            (43, "Earlier work".to_owned(), PrStatus::Completed),
            (42, "Split the files".to_owned(), PrStatus::Active),
        ],
        Vec::new(),
    );
    render_text(120, 40, &mut app);

    press(&mut app, KeyCode::Char('g'));
    assert_eq!(app.tab, TabId::PullRequests);
    assert_eq!(
        app.pull_requests
            .selected(&app.shell)
            .map(|row| row.request.id),
        Some(42),
        "the one still open, not the newer one that closed"
    );

    // `[` comes back to the work item, which is where the walk started.
    press(&mut app, KeyCode::Char('['));
    assert_eq!(app.tab, TabId::WorkItems);
    assert_eq!(app.work_items.selected_ticket().unwrap().key.id, 10_001);
}

#[test]
fn a_work_item_with_nothing_linked_says_so_rather_than_going_nowhere() {
    let mut app = App::new(vec![ticket()]);
    render_text(120, 40, &mut app);

    press(&mut app, KeyCode::Char('g'));
    assert_eq!(app.tab, TabId::WorkItems);
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("#10001 has no linked pull request or build")
    );
}

#[test]
fn the_history_records_every_tab_it_is_left_from_rather_than_the_cursor_moving() {
    let mut app = crate::app::pull_requests::tests::pull_requests_app();
    app.repos.set_repos(&app.shell);
    app.select_tab(TabId::PullRequests);
    render_text(160, 40, &mut app);

    for _ in 0..5 {
        press(&mut app, KeyCode::Char('j'));
    }
    assert!(
        app.shell.history().is_empty(),
        "walking the rows is not going anywhere"
    );
    let settled = app
        .pull_requests
        .selected(&app.shell)
        .expect("a row under the cursor")
        .request
        .id;

    press(&mut app, KeyCode::Char('2'));
    assert_eq!(app.tab, TabId::Repos);
    assert_eq!(
        app.shell.history().len(),
        2,
        "the row it stopped on, then the row it arrived at"
    );

    press(&mut app, KeyCode::Char('['));
    assert_eq!(app.tab, TabId::PullRequests);
    assert_eq!(
        app.pull_requests
            .selected(&app.shell)
            .map(|row| row.request.id),
        Some(settled),
        "back on the pull request the cursor was left on"
    );

    press(&mut app, KeyCode::Char(']'));
    assert_eq!(app.tab, TabId::Repos, "and forward again");
}

#[test]
fn a_reloaded_session_still_walks_back_through_a_pull_request_and_a_pod() {
    use crate::aks::tests::{cluster, pod};

    let build = || {
        let mut app = crate::app::pull_requests::tests::pull_requests_app();
        app.repos.set_repos(&app.shell);
        app.aks
            .set_clusters(vec![cluster("qa", &["orders"])], &mut app.shell);
        app.aks.set_pods(
            &mut app.shell,
            "qa",
            Some("orders"),
            Ok(vec![pod(
                "qa",
                "orders",
                "orders-api-7d9f5b-abc12",
                "Running",
            )]),
        );
        app
    };

    let mut app = build();
    app.select_tab(TabId::PullRequests);
    render_text(160, 40, &mut app);
    press(&mut app, KeyCode::Char('5'));
    assert_eq!(app.tab, TabId::Aks);
    let session = app.snapshot_session();

    let mut reopened = build();
    reopened.restore_session(session);
    assert_eq!(reopened.tab, TabId::Aks);

    press(&mut reopened, KeyCode::Char('['));
    assert_eq!(
        reopened.tab,
        TabId::PullRequests,
        "the walk is on file, so the next run can take it"
    );
    press(&mut reopened, KeyCode::Char(']'));
    assert_eq!(reopened.tab, TabId::Aks);
    assert_eq!(
        reopened
            .aks
            .selected_pod(&reopened.shell)
            .map(|row| row.pod.key.name),
        Some("orders-api-7d9f5b-abc12".to_owned())
    );
}
