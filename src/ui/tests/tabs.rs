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
fn the_tab_bar_names_four_tabs_and_marks_the_one_showing_at_every_breakpoint() {
    let mut app = App::new(vec![ticket()]);
    for width in [120, 90, 60] {
        let text = render_text(width, 30, &mut app);
        let bar = text.lines().next().expect("a tab bar row").to_owned();
        assert!(bar.contains("1 Work items"), "{width} columns: {bar}");
        assert!(bar.contains("4 Pipelines"), "{width} columns: {bar}");
    }

    // Narrower than the names, every tab is still there and still clickable.
    let narrow = render_text(40, 30, &mut app);
    let bar = narrow.lines().next().expect("a tab bar row");
    assert!(bar.contains("1 Items"), "{bar}");
    assert!(bar.contains("4 Runs"), "{bar}");
    assert!(
        app.shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::SelectTab { index: 3 }))
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
    assert!(
        active.add_modifier.contains(Modifier::REVERSED),
        "and stands out without colour"
    );
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
    assert!(text.contains("Repos 0"), "{text}");
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
        render_text(110, 30, &mut app).contains("Pipelines 0"),
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
    ];
    let mut terminal = Terminal::new(TestBackend::new(80, 3)).unwrap();
    terminal
        .draw(|frame| {
            let area = Rect::new(0, 0, 80, 1);
            render_tab_bar(frame, &mut shell, &tabs, area);
        })
        .unwrap();

    let bar = row_text(&terminal, 0, 80);
    assert!(bar.contains("3 Pull requests 3"), "{bar}");
    assert!(bar.contains("4 Pipelines ◐2"), "{bar}");
    assert!(
        shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::SelectTab { index: 2 }))
            .is_some(),
        "a badged tab is still clickable"
    );
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
