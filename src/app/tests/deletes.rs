use super::*;

/// An Epic over two issues, one of which has a task of its own. Everything
/// is open, so every row is on the table and the child counts read the
/// family rather than what the finished filter left of it.
fn deletable_tickets() -> Vec<Ticket> {
    let mut epic = ticket(1, "Auth rewrite", "2026-01-04T00:00:00Z");
    epic.work_item_type = "Epic".into();
    let mut login = ticket(2, "Login form", "2026-01-03T00:00:00Z");
    login.work_item_type = "Issue".into();
    let mut session = ticket(3, "Session notes", "2026-01-02T00:00:00Z");
    session.work_item_type = "Issue".into();
    vec![
        epic,
        login,
        session,
        ticket(4, "Validate email", "2026-01-01T00:00:00Z"),
    ]
}

/// An editable app over that family, so a delete has children to leave
/// behind and a ratio to move.
fn deleting_app() -> App {
    let mut app = App::new(deletable_tickets());
    app.set_workspace_graph(TicketGraph {
        relations: vec![child_of(2, 1), child_of(3, 1), child_of(4, 3)],
        ..TicketGraph::default()
    });
    app.enable_sync();
    app.set_table_viewport(4);
    app
}

/// The ids on the table, in the order it holds them.
fn rows_of(app: &App) -> Vec<i64> {
    app.visible_tickets().map(|ticket| ticket.key.id).collect()
}

/// Opens the confirmation the way somebody would, through the Edit menu.
fn open_delete_menu(app: &mut App) {
    press(app, KeyCode::Char('e'));
    for _ in 0..menu_row(app, CommandId::DeleteWorkItem) {
        press(app, KeyCode::Down);
    }
    press(app, KeyCode::Enter);
}

#[test]
fn the_delete_confirmation_names_the_work_item_and_the_children_it_leaves_behind() {
    let mut app = deleting_app();

    open_delete_menu(&mut app);

    assert_eq!(app.mode, AppMode::ConfirmDelete);
    let confirm = app
        .delete_confirm
        .clone()
        .expect("the Edit menu row opens the confirmation");
    assert_eq!(confirm.keys, vec![family_key(1)]);
    assert_eq!(confirm.question(), "Delete #1 Auth rewrite?");
    assert_eq!(
        confirm.children, 2,
        "the epic's two issues are counted, and the task under one of them is not"
    );
    assert_eq!(
        confirm.orphans().as_deref(),
        Some("Its 2 children are not deleted \u{2014} left with no parent."),
        "what is at stake is the work under the row, so the overlay says so"
    );
    assert_eq!(rows_of(&app), [1, 2, 3, 4], "and nothing has gone yet");
    assert!(!app.deletes_pending());
}

#[test]
fn a_work_item_with_nothing_under_it_is_confirmed_without_an_orphan_warning() {
    let mut app = deleting_app();
    app.select_row(1);

    open_delete_menu(&mut app);

    let confirm = app
        .delete_confirm
        .clone()
        .expect("the confirmation is open");
    assert_eq!(confirm.question(), "Delete #2 Login form?");
    assert_eq!(confirm.children, 0);
    assert_eq!(
        confirm.orphans(),
        None,
        "an issue nobody broke down leaves nothing behind to warn about"
    );
}

#[test]
fn escaping_the_delete_confirmation_changes_nothing_at_all() {
    let mut app = deleting_app();
    open_delete_menu(&mut app);

    assert_eq!(press(&mut app, KeyCode::Esc), AppAction::None);

    assert_eq!(app.mode, AppMode::Browse);
    assert!(app.delete_confirm.is_none());
    assert!(!app.deletes_pending());
    assert_eq!(app.notification(), None, "cancelling closes silently");
    assert_eq!(rows_of(&app), [1, 2, 3, 4]);
    assert_eq!(
        app.family_of(&family_key(1)).children,
        vec![family_key(2), family_key(3)],
        "and the family is exactly as it was"
    );
}

#[test]
fn a_delete_that_lands_takes_the_row_and_its_links_and_moves_the_cursor_on() {
    let mut app = deleting_app();
    app.select_row(2);
    open_delete_menu(&mut app);

    let action = press(&mut app, KeyCode::Char('d'));

    assert_eq!(action, AppAction::Delete(vec![family_key(3)]));
    assert_eq!(app.mode, AppMode::Browse);
    assert!(app.deletes_pending());
    assert_eq!(
        rows_of(&app),
        [1, 2, 3, 4],
        "nothing leaves the table until Azure DevOps has taken the delete"
    );

    app.apply_deleted(&family_key(3));

    assert_eq!(rows_of(&app), [1, 2, 4]);
    assert!(!app.deletes_pending());
    assert_eq!(
        app.selected_ticket().map(|ticket| ticket.key.id),
        Some(4),
        "the cursor takes the row that moved up into its place"
    );
    assert!(
        app.relations_from(&family_key(4)).is_empty(),
        "the task it was over stops claiming a parent that is gone"
    );
    assert_eq!(
        app.family_of(&family_key(1)).children,
        vec![family_key(2)],
        "and the epic is left with the one issue it still has"
    );
    assert_eq!(
        app.notification().map(|(message, _)| message),
        Some("Deleted #3 \u{b7} restore it from the Azure DevOps recycle bin"),
        "a soft delete is recoverable, and the line reporting it says so"
    );
}

#[test]
fn deleting_the_last_row_leaves_the_cursor_on_the_one_above_it() {
    let mut app = deleting_app();
    app.select_row(3);
    open_delete_menu(&mut app);
    press(&mut app, KeyCode::Char('d'));

    app.apply_deleted(&family_key(4));

    assert_eq!(rows_of(&app), [1, 2, 3]);
    assert_eq!(
        app.selected_ticket().map(|ticket| ticket.key.id),
        Some(3),
        "with nothing below it, the cursor takes the row above"
    );
}

#[test]
fn deleting_a_child_leaves_its_parent_counting_the_children_it_still_has() {
    let mut app = deleting_app();
    assert_eq!(progress_of(&app, 1), Some((0, 2)));
    assert_eq!(progress_of(&app, 3), Some((0, 1)));

    app.select_row(2);
    open_delete_menu(&mut app);
    press(&mut app, KeyCode::Char('d'));
    app.apply_deleted(&family_key(3));

    assert_eq!(
        progress_of(&app, 1),
        Some((0, 1)),
        "the epic's ratio counts the issue it has left"
    );
    assert_eq!(
        progress_of(&app, 3),
        None,
        "and the work item that went has no ratio at all"
    );
}

#[test]
fn a_refused_delete_says_so_and_leaves_the_row_on_the_table() {
    let mut app = deleting_app();
    open_delete_menu(&mut app);
    press(&mut app, KeyCode::Char('d'));

    app.reject_delete(&family_key(1), "TF401232: the work item does not exist");

    assert!(!app.deletes_pending());
    assert_eq!(
        rows_of(&app),
        [1, 2, 3, 4],
        "the row is exactly where it was"
    );
    assert_eq!(
        app.family_of(&family_key(1)).children,
        vec![family_key(2), family_key(3)],
        "and so are the links under it"
    );
    assert_eq!(
        app.notification(),
        Some((
            "#1 not deleted: TF401232: the work item does not exist",
            NotificationLevel::Error
        ))
    );
}

#[test]
fn a_checked_set_deletes_one_at_a_time_and_speaks_once_at_the_end() {
    let mut app = deleting_app();
    app.select_row(1);
    press(&mut app, KeyCode::Char(' '));
    app.select_row(2);
    press(&mut app, KeyCode::Char(' '));

    open_delete_menu(&mut app);
    let confirm = app
        .delete_confirm
        .clone()
        .expect("the confirmation is open");
    assert_eq!(confirm.question(), "Delete 2 tickets?");
    assert_eq!(
        confirm.orphans().as_deref(),
        Some("Their 1 child is not deleted \u{2014} left with no parent."),
        "the checked rows are counted together, and so is the work under them"
    );

    let action = press(&mut app, KeyCode::Char('d'));
    assert_eq!(
        action,
        AppAction::Delete(vec![family_key(2), family_key(3)]),
        "one request each, in the order the table holds them"
    );

    app.apply_deleted(&family_key(2));
    assert_eq!(
        app.notification().map(|(message, _)| message),
        Some("Deleting 2 tickets\u{2026}"),
        "the first answer says nothing of its own"
    );

    app.apply_deleted(&family_key(3));

    assert_eq!(rows_of(&app), [1, 4]);
    assert_eq!(
        app.notification(),
        Some(("Deleted 2 tickets", NotificationLevel::Info)),
        "the whole change speaks once, when the last answer is in"
    );
}

#[test]
fn a_checked_set_that_only_partly_lands_counts_what_went_and_names_what_stayed() {
    let mut app = deleting_app();
    app.select_row(1);
    press(&mut app, KeyCode::Char(' '));
    app.select_row(2);
    press(&mut app, KeyCode::Char(' '));
    open_delete_menu(&mut app);
    press(&mut app, KeyCode::Char('d'));

    app.apply_deleted(&family_key(2));
    app.reject_delete(&family_key(3), "it is locked");

    assert_eq!(
        app.notification(),
        Some((
            "Deleted 1 of 2 \u{b7} #3 failed: it is locked",
            NotificationLevel::Error
        ))
    );
    assert_eq!(rows_of(&app), [1, 3, 4], "the one that was refused stays");
}

#[test]
fn a_delete_never_reaches_the_undo_stack() {
    let mut app = deleting_app();
    app.select_row(2);
    let AppAction::Edit(requests) = app.edit_selected(FieldEdit::state("Doing")) else {
        panic!("an ordinary edit should dispatch a request");
    };
    accept(&mut app, &only(requests));
    assert_eq!(app.undo_stack.len(), 1, "an edit is undoable");

    open_delete_menu(&mut app);
    press(&mut app, KeyCode::Char('d'));
    app.apply_deleted(&family_key(3));

    assert!(
        app.undo_stack.is_empty(),
        "the delete files nothing, and the edit under it has no row left to go back to"
    );
    assert_eq!(press(&mut app, KeyCode::Char('u')), AppAction::None);
    assert_eq!(
        app.notification().map(|(message, _)| message),
        Some("Nothing to undo")
    );
    assert_eq!(rows_of(&app), [1, 2, 4], "and the row stays gone");
}

#[test]
fn a_delete_is_refused_before_the_confirmation_when_there_is_nothing_to_write_to() {
    let mut app = App::new(deletable_tickets());
    app.set_offline_reason(Some("no Azure DevOps organization is configured".into()));

    app.run_command(CommandId::DeleteWorkItem);

    assert_eq!(app.mode, AppMode::Browse, "the confirmation never opens");
    assert!(app.delete_confirm.is_none());
    assert_eq!(
        app.notification(),
        Some((
            "no Azure DevOps organization is configured",
            NotificationLevel::Error
        ))
    );
}

#[test]
fn deleting_the_work_item_the_details_pane_is_showing_leaves_it_over_the_next_one() {
    let mut app = deleting_app();
    app.select_row(2);
    app.details.set_viewport(4, 40);
    app.details.scroll_to(12);
    open_delete_menu(&mut app);
    press(&mut app, KeyCode::Char('d'));

    app.apply_deleted(&family_key(3));

    assert_eq!(
        app.selected_ticket().map(|ticket| ticket.key.id),
        Some(4),
        "the pane is over a work item that is still there"
    );
    assert_eq!(
        app.details.offset, 0,
        "and reads from the top of it rather than from where the last one was"
    );
    assert_eq!(
        app.family_cursor,
        Some(family_key(4)),
        "the family cursor follows the selection rather than the work item that went"
    );
}
