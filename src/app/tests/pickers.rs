use super::*;

#[test]
fn the_parent_picker_leaves_out_the_work_item_itself_and_everything_below_it() {
    let mut app = reparent_app();

    app.run_command(CommandId::SetParent);

    assert_eq!(app.mode, AppMode::ParentPicker);
    assert_eq!(
        candidate_ids(&app.parent_picker.candidates),
        [1, 2, 4],
        "#3 cannot be its own parent and #5 is already under it, so neither is offered"
    );
    assert_eq!(
        app.parent_picker.current,
        Some(family_key(1)),
        "the epic it hangs under now opens under the cursor"
    );
    assert_eq!(app.parent_picker.index, 0);
}

#[test]
fn the_parent_picker_filters_on_the_id_as_well_as_the_title() {
    let mut app = reparent_app();
    app.run_command(CommandId::SetParent);

    for ch in "pay".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(
        candidate_ids(&app.parent_matches()),
        [2],
        "the title matches"
    );

    for _ in 0..3 {
        press(&mut app, KeyCode::Backspace);
    }
    press(&mut app, KeyCode::Char('4'));
    assert_eq!(
        candidate_ids(&app.parent_matches()),
        [4],
        "and so does the id"
    );
}

#[test]
fn remove_parent_is_offered_only_when_the_work_item_has_one_to_remove() {
    let mut app = reparent_app();

    assert!(
        menu_labels(&app).contains(&"Remove parent"),
        "#3 hangs under an epic, so it can be detached: {:?}",
        menu_labels(&app)
    );
    assert_eq!(
        app.edit_menu_entries()[7].command,
        CommandId::SetParent,
        "the removal follows the row that sets a parent"
    );
    assert_eq!(app.edit_menu_entries()[8].command, CommandId::RemoveParent);

    app.jump_to_ticket(&family_key(2));
    assert!(
        !menu_labels(&app).contains(&"Remove parent"),
        "#2 hangs under nothing, so there is nothing to take off: {:?}",
        menu_labels(&app)
    );
    assert_eq!(
        app.run_command(CommandId::RemoveParent),
        AppAction::None,
        "and asking for it anyway writes nothing"
    );
}

#[test]
fn choosing_a_new_parent_moves_the_work_item_in_the_graph_and_in_both_ratios() {
    let mut app = reparent_app();
    assert_eq!(progress_of(&app, 1), Some((1, 2)));
    assert_eq!(progress_of(&app, 2), None);
    app.run_command(CommandId::SetParent);

    let index = app
        .parent_matches()
        .iter()
        .position(|candidate| candidate.key == family_key(2))
        .expect("the other epic is on offer");
    let action = app.choose_parent(index);

    assert_eq!(
        action,
        AppAction::Reparent {
            key: family_key(3),
            new_parent: Some(2),
        }
    );
    assert_eq!(app.mode, AppMode::Browse);
    assert_eq!(
        app.parent_of(&family_key(3)),
        Some(family_key(2)),
        "the work item names its new epic at once"
    );
    assert_eq!(
        app.family_of(&family_key(1)).children,
        vec![family_key(4)],
        "and the epic it left no longer names it, which is the other half of the link"
    );
    assert_eq!(app.family_of(&family_key(2)).children, vec![family_key(3)]);
    assert_eq!(
        progress_of(&app, 1),
        Some((1, 1)),
        "the epic it left has one issue fewer to finish"
    );
    assert_eq!(
        progress_of(&app, 2),
        Some((0, 1)),
        "and the epic it joined has one more"
    );
    assert_eq!(
        app.visible_family_tree().first().map(|entry| entry.key.id),
        Some(2),
        "the family tree redraws from the graph, so the new epic is the root"
    );
}

#[test]
fn removing_a_parent_detaches_the_work_item_in_both_directions() {
    let mut app = reparent_app();

    let action = app.run_command(CommandId::RemoveParent);

    assert_eq!(
        action,
        AppAction::Reparent {
            key: family_key(3),
            new_parent: None,
        }
    );
    assert_eq!(app.parent_of(&family_key(3)), None);
    assert_eq!(
        app.family_of(&family_key(1)).children,
        vec![family_key(4)],
        "the epic keeps the issue it still has and loses the one that left"
    );
    assert_eq!(progress_of(&app, 1), Some((1, 1)));
    assert_eq!(
        app.family_of(&family_key(3)).children,
        vec![family_key(5)],
        "what hangs under the detached work item comes with it"
    );
}

#[test]
fn a_refused_move_puts_both_halves_of_the_link_and_both_ratios_back() {
    let mut app = reparent_app();
    app.run_command(CommandId::SetParent);
    let index = app
        .parent_matches()
        .iter()
        .position(|candidate| candidate.key == family_key(2))
        .expect("the other epic is on offer");
    app.choose_parent(index);
    assert_eq!(app.parent_of(&family_key(3)), Some(family_key(2)));

    app.reject_reparent(&ReparentRejection {
        key: family_key(3),
        conflict: true,
        message: "it changed in Azure DevOps".into(),
    });

    assert_eq!(
        app.parent_of(&family_key(3)),
        Some(family_key(1)),
        "the work item is back under the epic it was under"
    );
    assert_eq!(
        app.family_of(&family_key(1)).children,
        vec![family_key(3), family_key(4)],
        "and that epic names it again"
    );
    assert_eq!(
        app.family_of(&family_key(2)).children,
        Vec::new(),
        "the epic it never joined is empty again"
    );
    assert_eq!(progress_of(&app, 1), Some((1, 2)));
    assert_eq!(progress_of(&app, 2), None);
    let (message, level) = app
        .shell
        .notification()
        .expect("a refused move is never silent");
    assert_eq!(level, NotificationLevel::Error);
    assert!(
        message.contains("#3 not moved") && message.contains("syncing"),
        "{message}"
    );
}

#[test]
fn a_cycle_the_stale_graph_could_not_see_is_refused_and_put_back_in_its_own_words() {
    // The picker cannot offer a descendant, so a cycle only ever comes from
    // a graph the project has already moved on from: #2 became a child of
    // #3 in Azure DevOps, and nothing here has read that yet.
    let mut app = reparent_app();
    app.run_command(CommandId::SetParent);
    let index = app
        .parent_matches()
        .iter()
        .position(|candidate| candidate.key == family_key(2))
        .expect("the other epic still looks like a candidate");
    app.choose_parent(index);

    app.reject_reparent(&ReparentRejection {
        key: family_key(3),
        conflict: false,
        message: "TF201036: adding this link would create a circular relationship".into(),
    });

    assert_eq!(
        app.parent_of(&family_key(3)),
        Some(family_key(1)),
        "the move is undone whole, not left half applied"
    );
    assert_eq!(
        app.family_of(&family_key(1)).children,
        vec![family_key(3), family_key(4)]
    );
    assert_eq!(app.family_of(&family_key(2)).children, Vec::new());
    assert_eq!(progress_of(&app, 1), Some((1, 2)));
    assert!(!app.reparents_pending());
    let (message, _) = app
        .shell
        .notification()
        .expect("a refused move is never silent");
    assert!(
        message.contains("circular relationship") && !message.contains("syncing"),
        "Azure DevOps's own reason is reported, and a rule refusal is not a conflict: {message}"
    );
}

#[test]
fn an_accepted_move_settles_on_the_links_azure_devops_sent_back() {
    let mut app = reparent_app();
    app.run_command(CommandId::RemoveParent);
    let mut stored = app.ticket_by_key(&family_key(3)).unwrap().clone();
    stored.revision += 1;

    // The server filed it under the other epic after all, which is what the
    // graph has to settle on rather than the detachment that was asked for.
    app.apply_reparent(ReparentApplied {
        ticket: stored,
        relations: vec![child_of(3, 2)],
        parent: Some(family_key(2)),
    });

    assert!(!app.reparents_pending());
    assert_eq!(app.parent_of(&family_key(3)), Some(family_key(2)));
    assert_eq!(app.family_of(&family_key(2)).children, vec![family_key(3)]);
    assert_eq!(app.family_of(&family_key(1)).children, vec![family_key(4)]);
    assert_eq!(progress_of(&app, 2), Some((0, 1)));
    assert_eq!(
        app.ticket_by_key(&family_key(3)).unwrap().revision,
        2,
        "the row takes the revision the server settled on"
    );
}

#[test]
fn a_second_move_is_refused_while_the_first_is_still_in_flight() {
    let mut app = reparent_app();
    assert!(matches!(
        app.run_command(CommandId::RemoveParent),
        AppAction::Reparent { .. }
    ));
    assert!(app.reparents_pending());

    app.run_command(CommandId::SetParent);
    let index = app
        .parent_matches()
        .iter()
        .position(|candidate| candidate.key == family_key(2))
        .expect("the other epic is on offer");

    assert_eq!(
        app.choose_parent(index),
        AppAction::None,
        "the second move would be tested against a revision that is already stale"
    );
    assert_eq!(
        app.parent_of(&family_key(3)),
        None,
        "and the graph still shows only the move that is in flight"
    );
}

#[test]
fn the_state_picker_opens_on_the_current_state_and_enter_writes_the_one_chosen() {
    let mut app = picker_app();

    assert_eq!(shift(&mut app, 'S'), AppAction::None);
    assert_eq!(app.mode, AppMode::StatePicker);
    assert_eq!(
        state_names(&app.state_picker.options),
        ["To Do", "Doing", "Done"]
    );
    assert_eq!(app.state_picker.current, "To Do");
    assert_eq!(
        app.state_picker.index, 0,
        "the state the work item is in starts under the cursor"
    );
    assert_eq!(
        app.state_picker.scope,
        EditScope::Ticket(3),
        "the picker names the selected row"
    );

    press(&mut app, KeyCode::Down);
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("choosing another state should dispatch an edit");
    };
    let request = only(requests);

    assert_eq!(app.mode, AppMode::Browse);
    assert_eq!(request.key.id, 3);
    assert_eq!(
        request.document(),
        vec![
            serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
            serde_json::json!({"op": "add", "path": "/fields/System.State", "value": "Doing"}),
        ]
    );
    assert_eq!(
        app.selected_ticket().map(|ticket| ticket.state.as_str()),
        Some("Doing"),
        "the row shows the new state without waiting for Azure DevOps"
    );
    assert!(app.edits_pending());
}

#[test]
fn choosing_the_current_state_or_pressing_escape_writes_nothing() {
    let mut app = picker_app();

    shift(&mut app, 'S');
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        AppAction::None,
        "the state it is already in is a no-op"
    );
    assert_eq!(app.mode, AppMode::Browse);
    assert!(!app.edits_pending());
    assert_eq!(app.shell.notification(), None, "a no-op closes silently");

    shift(&mut app, 'S');
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(press(&mut app, KeyCode::Esc), AppAction::None);
    assert_eq!(app.mode, AppMode::Browse);
    assert!(!app.edits_pending());
    assert_eq!(app.shell.notification(), None);
    assert_eq!(
        app.selected_ticket().map(|ticket| ticket.state.as_str()),
        Some("To Do"),
        "cancelling leaves the row exactly as it was"
    );
}

#[test]
fn the_priority_picker_opens_on_the_current_value_and_writes_the_one_chosen() {
    let mut app = edit_app();

    open_editor(&mut app, 2);
    assert_eq!(app.mode, AppMode::PriorityPicker);
    assert_eq!(app.priority_picker.current, Some(1));
    assert_eq!(
        app.priority_picker.index, 0,
        "the priority the work item has starts under the cursor"
    );
    assert_eq!(app.priority_picker.id, 3);

    assert_eq!(
        press(&mut app, KeyCode::Enter),
        AppAction::None,
        "the priority it already has is a no-op"
    );
    assert!(!app.edits_pending());
    assert_eq!(app.shell.notification(), None);

    open_editor(&mut app, 2);
    press(&mut app, KeyCode::Down);
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("another priority should dispatch an edit");
    };
    let request = only(requests);
    assert_eq!(
        request.document(),
        vec![
            serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
            serde_json::json!({
                "op": "add",
                "path": "/fields/Microsoft.VSTS.Common.Priority",
                "value": 2,
            }),
        ]
    );
    assert_eq!(
        app.selected_ticket().and_then(|ticket| ticket.priority),
        Some(2),
        "the Pri cell shows the new priority at once"
    );
}

#[test]
fn clearing_the_priority_removes_the_field_and_empties_the_cell() {
    let mut app = edit_app();

    open_editor(&mut app, 2);
    press(&mut app, KeyCode::End);
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("Clear should dispatch an edit");
    };
    let request = only(requests);
    assert_eq!(
        request.document(),
        vec![
            serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
            serde_json::json!({
                "op": "remove",
                "path": "/fields/Microsoft.VSTS.Common.Priority",
            }),
        ],
        "a priority goes back to unset by being removed"
    );
    assert_eq!(
        app.selected_ticket().and_then(|ticket| ticket.priority),
        None
    );
}

#[test]
fn the_picker_lists_cached_states_and_otherwise_the_ones_already_in_the_database() {
    let typed = |id: i64, work_item_type: &str, state: &str| {
        let mut ticket = ticket(id, "Row", "2026-01-01T00:00:00Z");
        ticket.work_item_type = work_item_type.to_owned();
        ticket.state = state.to_owned();
        ticket
    };
    let mut app = App::new(vec![
        typed(1, "Bug", "Done"),
        typed(2, "Bug", "New"),
        typed(3, "Bug", "Active"),
        typed(4, "Bug", "New"),
        typed(5, "Bug", "Approved"),
        typed(6, "Task", "Doing"),
    ]);

    assert_eq!(
        state_names(&app.states_for("Bug")),
        ["Approved", "New", "Active", "Done"],
        "the fallback runs Proposed, InProgress, Resolved, Completed, Removed, then name"
    );
    assert_eq!(state_names(&app.states_for("Task")), ["Doing"]);
    assert!(
        app.states_for("Epic").is_empty(),
        "a type with no rows and nothing cached has no states"
    );

    let mut catalog = StateCatalog::default();
    catalog.insert(
        "Bug",
        vec![
            StateOption::new("New", StateCategory::Proposed),
            StateOption::new("Active", StateCategory::InProgress),
            StateOption::new("Resolved", StateCategory::Resolved),
            StateOption::new("Closed", StateCategory::Completed),
        ],
    );
    app.set_state_catalog(catalog);

    assert_eq!(
        state_names(&app.states_for("Bug")),
        ["New", "Active", "Resolved", "Closed"],
        "cached states win, in the order the process template runs them"
    );
    assert_eq!(
        state_names(&app.states_for("Task")),
        ["Doing"],
        "a type without cached states still falls back"
    );
}

/// An editable app whose rows name three different people, with the
/// signed-in user holding none of them.
fn assignee_app() -> App {
    let mut alpha = ticket(1, "Alpha", "2026-01-01T00:00:00Z");
    alpha.assigned_to = Some("Priya Nair".into());
    let mut beta = ticket(2, "Beta", "2026-02-01T00:00:00Z");
    beta.assigned_to = None;
    let mut gamma = ticket(3, "Gamma", "2026-03-01T00:00:00Z");
    gamma.assigned_to = Some("Avery Chen".into());
    let mut app = App::new(vec![alpha, beta, gamma]);
    app.shell.enable_sync();
    app.set_table_viewport(3);
    app.shell.set_me(Some("Jacob Ragsdale".into()));
    app
}

fn candidate_names(app: &App) -> Vec<String> {
    app.assignee_matches()
        .into_iter()
        .map(|candidate| candidate.display)
        .collect()
}

#[test]
fn the_assignee_picker_lists_nobody_then_me_then_the_database_and_starts_on_the_current_one() {
    let mut app = assignee_app();

    assert_eq!(
        press(&mut app, KeyCode::Char('a')),
        AppAction::FetchIdentities,
        "the first open asks for the project's teams"
    );
    assert_eq!(app.mode, AppMode::AssigneePicker);
    assert_eq!(
        candidate_names(&app),
        ["Unassigned", "Jacob Ragsdale", "Avery Chen", "Priya Nair"],
        "nobody, then me, then everybody the rows name, sorted"
    );
    assert!(
        app.assignee_matches()[1].me,
        "the signed-in user is marked as such"
    );
    assert_eq!(
        app.assignee_picker.index, 2,
        "the picker opens on whoever holds the work item"
    );
    assert_eq!(
        app.assignee_picker.scope,
        EditScope::Ticket(3),
        "it names the selected row"
    );

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode, AppMode::Browse);
    assert_eq!(
        press(&mut app, KeyCode::Char('a')),
        AppAction::None,
        "the teams are asked for once a session"
    );
}

#[test]
fn checking_several_rows_hands_all_of_them_to_whoever_the_picker_names() {
    let mut app = assignee_app();
    check_all(&mut app);

    press(&mut app, KeyCode::Char('a'));
    assert_eq!(
        app.assignee_picker.scope,
        EditScope::Checked(3),
        "reassigning a departing engineer's work is one change, not three"
    );
    press(&mut app, KeyCode::Up);
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("choosing somebody should reassign every checked row");
    };

    assert_eq!(
        requests
            .iter()
            .map(|request| request.key.id)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    for request in &requests {
        assert_eq!(request.edit.summary(), "Assignee \u{2192} Jacob Ragsdale");
    }
    assert!(
        app.tickets()
            .iter()
            .all(|ticket| ticket.assigned_to.as_deref() == Some("Jacob Ragsdale")),
        "every row shows its new owner at once"
    );

    // Whoever holds the row under the cursor is a change worth making to
    // the others, so it is no longer the no-op it is for a single row.
    let mut app = assignee_app();
    check_all(&mut app);
    press(&mut app, KeyCode::Char('a'));
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("the other checked rows are held by somebody else");
    };
    assert_eq!(
        requests
            .iter()
            .map(|request| request.key.id)
            .collect::<Vec<_>>(),
        [1, 2],
        "#3 already holds it, so it is passed over rather than rewritten"
    );
}

#[test]
fn typing_filters_the_assignee_picker_and_enter_assigns_who_is_left() {
    let mut app = assignee_app();
    app.set_identities(vec![Identity::new(
        "Jacob Ragsdale",
        Some("jacob@example.com".into()),
    )]);

    press(&mut app, KeyCode::Char('a'));
    type_query(&mut app, "jr");
    assert_eq!(
        candidate_names(&app),
        ["Jacob Ragsdale"],
        "the filter matches characters in order, not only whole words"
    );
    assert_eq!(
        app.assignee_picker.index, 0,
        "typing moves to the first hit"
    );

    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("choosing somebody else should dispatch an edit");
    };
    let request = only(requests);
    assert_eq!(app.mode, AppMode::Browse);
    assert_eq!(request.key.id, 3);
    assert_eq!(
        request.document(),
        vec![
            serde_json::json!({"op": "test", "path": "/rev", "value": 1}),
            serde_json::json!({
                "op": "add",
                "path": "/fields/System.AssignedTo",
                "value": "jacob@example.com",
            }),
        ],
        "the write carries the address when the picker knows one"
    );
    assert_eq!(
        app.selected_ticket()
            .and_then(|ticket| ticket.assigned_to.clone()),
        Some("Jacob Ragsdale".to_owned()),
        "the cell reads as the display name, not the address"
    );
    assert!(app.shell.is_mine(app.selected_ticket().unwrap()));
}

#[test]
fn a_person_with_no_address_is_written_by_name_and_unassigned_removes_the_field() {
    let mut app = assignee_app();

    press(&mut app, KeyCode::Char('a'));
    type_query(&mut app, "priya");
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("choosing somebody else should dispatch an edit");
    };
    let request = only(requests);
    assert_eq!(
        request.edit.patch(),
        vec![serde_json::json!({
            "op": "add",
            "path": "/fields/System.AssignedTo",
            "value": "Priya Nair",
        })],
        "a name the database only ever saw is sent as itself"
    );

    let mut app = assignee_app();
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up);
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("Unassigned should dispatch an edit");
    };
    let request = only(requests);
    assert_eq!(
        request.edit.patch(),
        vec![serde_json::json!({"op": "remove", "path": "/fields/System.AssignedTo"})],
        "nobody is written by taking the field off the work item"
    );
    assert_eq!(
        app.selected_ticket()
            .and_then(|ticket| ticket.assigned_to.clone()),
        None,
        "the Assignee cell empties at once"
    );
}

#[test]
fn choosing_the_current_assignee_or_pressing_escape_writes_nothing() {
    let mut app = assignee_app();

    press(&mut app, KeyCode::Char('a'));
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        AppAction::None,
        "whoever holds it already is a no-op"
    );
    assert_eq!(app.mode, AppMode::Browse);
    assert!(!app.edits_pending());
    assert_eq!(app.shell.notification(), None, "a no-op closes silently");

    // The same again for a work item nobody holds, where Unassigned is the
    // row the picker opens on.
    app.select_row(1);
    assert_eq!(
        app.selected_ticket()
            .and_then(|ticket| ticket.assigned_to.clone()),
        None
    );
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.assignee_picker.index, 0);
    assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
    assert!(!app.edits_pending());
    assert_eq!(app.shell.notification(), None);

    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Down);
    assert_eq!(press(&mut app, KeyCode::Esc), AppAction::None);
    assert_eq!(app.mode, AppMode::Browse);
    assert!(!app.edits_pending());
}

#[test]
fn team_members_land_in_an_open_picker_without_moving_the_cursor() {
    let mut app = assignee_app();

    press(&mut app, KeyCode::Char('a'));
    let focused = app.assignee_matches()[app.assignee_picker.index]
        .display
        .clone();
    assert_eq!(focused, "Avery Chen");

    app.merge_identities(vec![
        Identity::new("Avery Chen", Some("avery@example.com".into())),
        Identity::new("Dana Okafor", Some("dana@example.com".into())),
    ]);

    assert_eq!(
        candidate_names(&app),
        [
            "Unassigned",
            "Jacob Ragsdale",
            "Avery Chen",
            "Priya Nair",
            "Dana Okafor"
        ],
        "a team member nobody holds work for is appended after the database's"
    );
    assert_eq!(
        app.assignee_matches()[app.assignee_picker.index].display,
        focused,
        "the cursor stays on the person it was on"
    );
    assert_eq!(
        app.assignee_matches()[2].unique.as_deref(),
        Some("avery@example.com"),
        "somebody already listed gains the address the teams knew"
    );

    type_query(&mut app, "dana");
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("a merged-in team member should be choosable");
    };
    let request = only(requests);
    assert_eq!(
        request.edit.patch(),
        vec![serde_json::json!({
            "op": "add",
            "path": "/fields/System.AssignedTo",
            "value": "dana@example.com",
        })]
    );
}

/// An editable app whose selected row is planned into `development\Q3` and
/// `development\Platform`, both nodes of the trees above.
fn planned_app() -> App {
    let mut app = edit_app();
    let planned: Vec<Ticket> = app
        .tickets()
        .iter()
        .map(|ticket| Ticket {
            iteration_path: "development\\Q3".into(),
            area_path: "development\\Platform".into(),
            ..ticket.clone()
        })
        .collect();
    app.replace_prepared_tickets(PreparedTickets::new(planned));
    app.set_table_viewport(3);
    app
}

/// The same app with the project's trees already cached.
fn node_app() -> App {
    let mut app = planned_app();
    app.set_classification_nodes(classification_trees(), None);
    app
}

/// The rows the open picker is showing, as they are drawn: the indent, the
/// leaf, and whether the row is marked as running today.
fn node_rows(app: &App) -> Vec<String> {
    app.node_matches()
        .into_iter()
        .map(|row| {
            let current = if row.current_period { " current" } else { "" };
            format!("{}{}{current}", row.indent(), row.leaf())
        })
        .collect()
}

/// Runs the Edit menu's Iteration or Area row.
fn open_nodes(app: &mut App, kind: NodeKind) -> AppAction {
    app.run_command(match kind {
        NodeKind::Iteration => CommandId::EditIteration,
        NodeKind::Area => CommandId::EditArea,
    })
}

#[test]
fn the_iteration_picker_draws_the_tree_indented_and_opens_on_the_current_node() {
    let mut app = node_app();

    assert_eq!(
        open_nodes(&mut app, NodeKind::Iteration),
        AppAction::FetchClassificationNodes,
        "the first open asks for the project's trees"
    );
    assert_eq!(app.mode, AppMode::NodePicker);
    assert_eq!(
        node_rows(&app),
        ["development", "  Sprint 1 current", "  Q3", "    Sprint 7"],
        "two spaces a level, the leaf named, and the sprint running today marked"
    );
    assert!(
        app.node_matches()[1].dates.is_some(),
        "a scheduled iteration carries its date range"
    );
    assert_eq!(
        app.node_picker.index, 2,
        "the picker opens on the node the work item sits in"
    );
    assert_eq!(app.node_picker.current, "development\\Q3");
    assert_eq!(
        app.node_picker.scope,
        EditScope::Ticket(3),
        "it names the selected row"
    );

    press(&mut app, KeyCode::Esc);
    assert_eq!(
        open_nodes(&mut app, NodeKind::Iteration),
        AppAction::None,
        "the trees are asked for once a session, so the second open is instant"
    );
    press(&mut app, KeyCode::Esc);
    assert_eq!(
        open_nodes(&mut app, NodeKind::Area),
        AppAction::None,
        "and the other picker shares that one fetch"
    );
    assert_eq!(
        node_rows(&app),
        ["development", "  Platform"],
        "the area picker draws the other tree, with no dates on it"
    );
    assert_eq!(app.node_picker.index, 1);
}

#[test]
fn enter_on_another_node_writes_the_full_path_and_the_row_shows_its_leaf() {
    let mut app = node_app();

    open_nodes(&mut app, NodeKind::Iteration);
    type_query(&mut app, "sprint 1");
    assert_eq!(
        node_rows(&app),
        ["  Sprint 1 current"],
        "the filter matches characters in order over the whole path"
    );
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("choosing another node should dispatch an edit");
    };
    let request = only(requests);

    assert_eq!(app.mode, AppMode::Browse);
    assert_eq!(request.key.id, 3);
    assert_eq!(
        request.edit.patch(),
        vec![serde_json::json!({
            "op": "add",
            "path": "/fields/System.IterationPath",
            "value": "development\\Sprint 1",
        })],
        "the write carries the full backslash path, not the leaf"
    );
    let moved = app.selected_ticket().expect("a selected row");
    assert_eq!(moved.iteration_path, "development\\Sprint 1");
    assert_eq!(
        path_leaf(&moved.iteration_path),
        "Sprint 1",
        "the Iteration column goes on showing the leaf"
    );

    let mut app = node_app();
    open_nodes(&mut app, NodeKind::Area);
    press(&mut app, KeyCode::Up);
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("choosing another area should dispatch an edit");
    };
    let request = only(requests);
    assert_eq!(
        request.edit.patch(),
        vec![serde_json::json!({
            "op": "add",
            "path": "/fields/System.AreaPath",
            "value": "development",
        })]
    );
    assert_eq!(
        app.selected_ticket().map(|ticket| ticket.area_path.clone()),
        Some("development".to_owned())
    );
}

#[test]
fn choosing_the_node_the_work_item_is_already_in_writes_nothing() {
    let mut app = node_app();

    open_nodes(&mut app, NodeKind::Iteration);
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        AppAction::None,
        "the node it sits in already is a no-op"
    );
    assert_eq!(app.mode, AppMode::Browse);
    assert!(!app.edits_pending());
    assert_eq!(app.shell.notification(), None, "a no-op closes silently");

    open_nodes(&mut app, NodeKind::Iteration);
    press(&mut app, KeyCode::Up);
    assert_eq!(press(&mut app, KeyCode::Esc), AppAction::None);
    assert_eq!(app.mode, AppMode::Browse);
    assert!(!app.edits_pending());
}

#[test]
fn checking_several_rows_moves_them_all_to_the_sprint_chosen_but_not_to_an_area() {
    let mut app = node_app();
    check_all(&mut app);

    open_nodes(&mut app, NodeKind::Iteration);
    assert_eq!(
        app.node_picker.scope,
        EditScope::Checked(3),
        "a sprint's leftovers move on together"
    );
    press(&mut app, KeyCode::Up);
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("choosing a sprint should move every checked row");
    };
    assert_eq!(
        requests
            .iter()
            .map(|request| request.key.id)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(
        app.tickets()
            .iter()
            .all(|ticket| ticket.iteration_path == "development\\Sprint 1"),
        "every row carries the full path at once"
    );

    let mut app = node_app();
    check_all(&mut app);
    open_nodes(&mut app, NodeKind::Area);
    assert_eq!(
        app.node_picker.scope,
        EditScope::Ticket(3),
        "the area tree stays on the row under the cursor"
    );
    press(&mut app, KeyCode::Up);
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("choosing another area should dispatch an edit");
    };
    assert_eq!(only(requests).key.id, 3);
}

#[test]
fn a_picker_with_nothing_cached_lists_the_paths_the_database_holds() {
    let mut app = planned_app();

    open_nodes(&mut app, NodeKind::Iteration);
    assert_eq!(
        node_rows(&app),
        ["  Q3"],
        "every work item is in development\\Q3, indented by its own depth"
    );
    assert_eq!(app.node_picker.index, 0, "which is where the cursor starts");

    press(&mut app, KeyCode::Esc);
    open_nodes(&mut app, NodeKind::Area);
    assert_eq!(node_rows(&app), ["  Platform"]);
}

#[test]
fn fetched_trees_land_in_an_open_picker_without_moving_the_cursor() {
    let mut app = planned_app();

    assert_eq!(
        open_nodes(&mut app, NodeKind::Iteration),
        AppAction::FetchClassificationNodes
    );
    assert_eq!(node_rows(&app), ["  Q3"]);
    let focused = app.node_matches()[app.node_picker.index].path.clone();

    app.merge_classification_nodes(classification_trees());
    assert_eq!(
        node_rows(&app),
        ["development", "  Sprint 1 current", "  Q3", "    Sprint 7"],
        "the fetched tree replaces the fallback in the open picker"
    );
    assert_eq!(
        app.node_matches()[app.node_picker.index].path,
        focused,
        "the cursor stays on the node it was on"
    );

    type_query(&mut app, "q3s7");
    let AppAction::Edit(requests) = press(&mut app, KeyCode::Enter) else {
        panic!("a merged-in node should be choosable");
    };
    let request = only(requests);
    assert_eq!(
        request.edit.patch(),
        vec![serde_json::json!({
            "op": "add",
            "path": "/fields/System.IterationPath",
            "value": "development\\Q3\\Sprint 7",
        })]
    );
}

#[test]
fn the_current_iteration_is_the_scheduled_one_containing_today() {
    let mut app = planned_app();
    assert_eq!(
        app.current_iteration(),
        None,
        "a project whose trees were never fetched has no current sprint"
    );

    app.set_classification_nodes(classification_trees(), None);
    assert_eq!(
        app.current_iteration(),
        Some("development\\Sprint 1".to_owned())
    );

    let undated: Vec<ClassificationNode> = classification_trees()
        .into_iter()
        .map(|node| ClassificationNode::new(node.kind, node.path, node.depth))
        .collect();
    app.set_classification_nodes(undated, None);
    assert_eq!(
        app.current_iteration(),
        None,
        "an iteration nobody scheduled is never the current one"
    );
}

#[test]
fn a_pull_without_cached_states_keeps_the_ones_an_earlier_pull_brought() {
    let mut app = picker_app();
    let tickets = app.tickets().to_vec();

    app.replace_prepared_tickets(PreparedTickets::new(tickets.clone()));
    assert_eq!(
        state_names(&app.states_for("Task")),
        ["To Do", "Doing", "Done"],
        "a pull that has not read the states endpoint must not empty the picker"
    );

    let mut catalog = StateCatalog::default();
    catalog.insert(
        "Task",
        vec![StateOption::new("Cut", StateCategory::Removed)],
    );
    app.replace_prepared_tickets(PreparedTickets::new(tickets).with_states(catalog));
    assert_eq!(state_names(&app.states_for("Task")), ["Cut"]);
}
