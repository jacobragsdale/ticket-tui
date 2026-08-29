use super::*;
use crate::app::TabId;
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
    assert!(text.contains("Repos — coming in #669"), "{text}");
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
        render_text(110, 30, &mut app).contains("Pipelines — coming in #680"),
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
