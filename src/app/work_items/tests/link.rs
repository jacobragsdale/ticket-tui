use super::*;

fn link_app() -> App {
    let mut app = App::new(vec![ticket(715, "Fix the thing!", "2026-01-05T00:00:00Z")]);
    app.shell.set_repos(vec![
        crate::app::repos::tests::repo("aaa-111", "ticket-tui", false),
        crate::app::repos::tests::repo("bbb-222", "ado-helper", false),
        crate::app::repos::tests::repo("ccc-333", "retired", true),
    ]);
    app
}

#[test]
fn l_picks_a_repository_then_a_branch_and_links_the_work_item() {
    let mut app = link_app();

    assert_eq!(press(&mut app, KeyCode::Char('L')), AppAction::None);
    assert_eq!(app.work_items.mode, WorkItemMode::LinkPicker);
    assert_eq!(
        app.work_items.link_matches(),
        ["ticket-tui", "ado-helper"],
        "the disabled repository is not offered"
    );
    for ch in "ado".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(app.work_items.link_matches(), ["ado-helper"]);

    assert_eq!(
        press(&mut app, KeyCode::Enter),
        AppAction::FetchBranches("bbb-222".into())
    );
    assert_eq!(
        app.work_items.link_picker.query.text(),
        "715-fix-the-thing",
        "the branch name is offered from the title"
    );
    // Enter before the branches are read sends nothing.
    assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
    assert_eq!(app.work_items.mode, WorkItemMode::LinkPicker);

    app.work_items
        .set_branches("bbb-222", &["main".into(), "maintenance".into()]);
    assert!(
        app.work_items.link_matches().is_empty(),
        "no branch of that name yet"
    );
    // A name no branch has is made at the head of the default branch.
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        AppAction::LinkBranch {
            repo_id: "bbb-222".into(),
            branch: "715-fix-the-thing".into(),
            work_item: 715,
            create_from: Some("refs/heads/main".into()),
        }
    );
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("Linking #715 to ado-helper/715-fix-the-thing\u{2026}")
    );
    app.work_items
        .apply_branch_link(&mut app.shell, 715, "bbb-222", "715-fix-the-thing");
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("#715 linked to ado-helper/715-fix-the-thing")
    );
}

#[test]
fn an_existing_branch_is_linked_as_it_is_and_the_exact_name_comes_first() {
    let mut app = link_app();
    press(&mut app, KeyCode::Char('L'));
    press(&mut app, KeyCode::Enter);
    app.work_items
        .set_branches("aaa-111", &["maintenance".into(), "main".into()]);
    app.work_items.link_picker.query = TextInput::default();
    for ch in "main".chars() {
        press(&mut app, KeyCode::Char(ch));
    }

    assert_eq!(app.work_items.link_matches(), ["main", "maintenance"]);
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        AppAction::LinkBranch {
            repo_id: "aaa-111".into(),
            branch: "main".into(),
            work_item: 715,
            create_from: None,
        }
    );
}

#[test]
fn esc_backs_out_of_the_branches_to_the_repositories_and_then_out() {
    let mut app = link_app();
    press(&mut app, KeyCode::Char('L'));
    press(&mut app, KeyCode::Enter);
    assert!(app.work_items.link_picker.repo.is_some());

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.work_items.mode, WorkItemMode::LinkPicker);
    assert!(app.work_items.link_picker.repo.is_none());
    assert_eq!(app.work_items.link_picker.query.text(), "");

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
}
