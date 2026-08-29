use super::*;

/// A workspace holding one work item for each daily question: mine and
/// moving, nobody's, two that have gone quiet, one finished long ago, and
/// one more planned into the sprint running today.
fn views_app() -> App {
    let today = Timestamp::now().calendar_date();
    let row = |id: i64,
               title: &str,
               state: &str,
               assignee: Option<&str>,
               iteration: &str,
               changed: &str| Ticket {
        state: state.into(),
        assigned_to: assignee.map(str::to_owned),
        iteration_path: iteration.into(),
        ..ticket(id, title, changed)
    };
    let sprint = "development\\Sprint 1";
    let quarter = "development\\Q3";
    let mut app = App::new(vec![
        row(
            1,
            "Mine and moving",
            "Doing",
            Some("Avery Chen"),
            sprint,
            &format!("{today}T09:00:00Z"),
        ),
        row(
            2,
            "Nobody has this",
            "To Do",
            None,
            quarter,
            &format!("{today}T08:00:00Z"),
        ),
        row(
            3,
            "Gone quiet",
            "To Do",
            Some("Jordan Patel"),
            quarter,
            "2020-01-01T00:00:00Z",
        ),
        row(
            4,
            "Quieter still",
            "To Do",
            Some("Jordan Patel"),
            quarter,
            "2019-01-01T00:00:00Z",
        ),
        row(
            5,
            "Finished long ago",
            "Done",
            Some("Avery Chen"),
            quarter,
            "2018-01-01T00:00:00Z",
        ),
        row(
            6,
            "Also this sprint",
            "To Do",
            Some("Jordan Patel"),
            sprint,
            &format!("{today}T07:00:00Z"),
        ),
    ]);
    app.shell.set_me(Some("Avery Chen".into()));
    app.work_items
        .set_classification_nodes(classification_trees(), None);
    app
}

fn visible_ids(app: &App) -> Vec<i64> {
    app.work_items
        .visible_tickets()
        .map(|ticket| ticket.key.id)
        .collect()
}

#[test]
fn the_views_overlay_lists_the_built_ins_above_whatever_the_user_saved() {
    let mut app = views_app();

    let rows = app.work_items.view_rows();
    let listed: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| (row.label.as_str(), row.query.as_str()))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("Built-in", ""),
            ("Mine", "assignee:@me"),
            ("Unassigned", "assignee:@none"),
            ("Doing", "state:doing"),
            ("Stale", "changed:>14d state:@open"),
            ("Current sprint", "iteration:@current"),
        ]
    );
    assert!(rows[0].is_heading());
    assert!(
        rows[1..].iter().all(|row| !row.is_heading()),
        "with nothing saved there is no second heading to show"
    );

    app.work_items.set_query(&mut app.shell, "tag:rust".into());
    app.work_items.save_view(&mut app.shell, "Rust work".into());

    let rows = app.work_items.view_rows();
    assert_eq!(rows.len(), 8);
    assert!(rows[6].is_heading());
    assert_eq!(rows[6].label, "Saved");
    assert_eq!(rows[7].label, "Rust work");
    assert!(rows[7].active, "the view just saved is the one on screen");
}

#[test]
fn each_built_in_view_yields_the_rows_its_question_asks_for() {
    let mut app = views_app();
    // What each view asks for is the subject, so the finished row answers
    // its question rather than being taken off the table before it is put.
    app.work_items.set_show_finished(&mut app.shell, true);
    let load = |app: &mut App, name: &str| {
        let row = view_row(app, name);
        app.work_items.apply_view_at(&mut app.shell, row);
        visible_ids(app)
    };

    assert_eq!(load(&mut app, "Mine"), vec![1, 5]);
    assert_eq!(load(&mut app, "Unassigned"), vec![2]);
    assert_eq!(load(&mut app, "Doing"), vec![1]);
    assert_eq!(load(&mut app, "Current sprint"), vec![1, 6]);

    assert_eq!(
        app.work_items.active_view.as_deref(),
        Some("Current sprint")
    );
    assert_eq!(app.work_items.query(), "iteration:@current");
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Browse,
        "loading a view closes the overlay"
    );
}

#[test]
fn the_stale_view_leaves_out_finished_work_and_puts_the_quietest_row_first() {
    let mut app = views_app();

    let row = view_row(&app, "Stale");

    app.work_items.apply_view_at(&mut app.shell, row);

    assert_eq!(app.work_items.query(), "changed:>14d state:@open");
    assert_eq!(
        (app.work_items.sort_field, app.work_items.sort_direction),
        (SortField::Changed, SortDirection::Ascending),
        "the one built-in that turns the default order around"
    );
    assert_eq!(
        visible_ids(&app),
        vec![4, 3],
        "the longest untouched row leads, and the Done row nobody has \
             touched since 2018 is not waiting on anybody"
    );
}

#[test]
fn a_built_in_view_cannot_be_saved_over_or_deleted() {
    let mut app = views_app();
    app.work_items.set_query(&mut app.shell, "tag:rust".into());

    app.work_items.save_view(&mut app.shell, "mine".into());

    assert!(
        app.work_items.views().is_empty(),
        "a built-in owns its name"
    );
    assert_eq!(
        app.work_items.view_rows().len(),
        6,
        "and no second Mine is listed"
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("'Mine' is a built-in view; choose another name")
    );

    let row = view_row(&app, "Mine");

    app.work_items.delete_view_at(&mut app.shell, row);

    assert_eq!(app.work_items.view_rows().len(), 6);
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("'Mine' is a built-in view and cannot be deleted")
    );
}

#[test]
fn the_views_cursor_opens_on_the_first_built_in_and_steps_over_the_headings() {
    let mut app = views_app();
    app.work_items.set_query(&mut app.shell, "tag:rust".into());
    app.work_items.save_view(&mut app.shell, "Rust work".into());

    app.work_items.open_views();
    assert_eq!(
        app.work_items.views_overlay.index, 1,
        "row zero is the Built-in heading"
    );

    for _ in 0..4 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(app.work_items.views_overlay.index, 5, "the last built-in");
    press(&mut app, KeyCode::Down);
    assert_eq!(
        app.work_items.views_overlay.index, 7,
        "the Saved heading is stepped over"
    );
    press(&mut app, KeyCode::Down);
    assert_eq!(
        app.work_items.views_overlay.index, 7,
        "and the list stops at its end"
    );
    assert!(
        app.work_items.can_delete_focused_view(),
        "a saved view can be deleted"
    );

    press(&mut app, KeyCode::Up);
    assert_eq!(app.work_items.views_overlay.index, 5);
    assert!(
        !app.work_items.can_delete_focused_view(),
        "a built-in cannot"
    );
}

/// `TICKET_TUI_ME` is resolved against the last sync's display name by
/// `resolve_me` before the app is told who it is, so a different name here
/// is exactly what the override produces.
#[test]
fn the_mine_view_follows_the_name_the_session_is_signed_in_under() {
    let mut app = views_app();
    app.work_items.set_show_finished(&mut app.shell, true);
    let row = view_row(&app, "Mine");
    app.work_items.apply_view_at(&mut app.shell, row);
    assert_eq!(visible_ids(&app), vec![1, 5]);

    app.shell.set_me(Some("Jordan Patel".into()));
    app.work_items.show_all(&mut app.shell, None);
    assert_eq!(
        visible_ids(&app),
        vec![6, 3, 4],
        "the saved query is unchanged; the name under it is not"
    );

    app.shell.set_me(None);
    app.work_items.show_all(&mut app.shell, None);
    assert!(
        visible_ids(&app).is_empty(),
        "with nobody signed in @me is nobody rather than everybody"
    );
}

#[test]
fn the_current_sprint_view_follows_the_iteration_dates_rather_than_a_written_path() {
    let mut app = views_app();
    app.work_items.set_show_finished(&mut app.shell, true);
    let row = view_row(&app, "Current sprint");
    app.work_items.apply_view_at(&mut app.shell, row);

    assert_eq!(
        app.work_items.current_iteration(),
        Some("development\\Sprint 1".to_owned())
    );
    assert_eq!(visible_ids(&app), vec![1, 6]);

    let today =
        Timestamp::parse(&format!("{}T00:00:00Z", Timestamp::now().calendar_date())).unwrap();
    let rolled_over: Vec<ClassificationNode> = classification_trees()
        .into_iter()
        .map(|node| {
            let current = node.path == "development\\Q3";
            ClassificationNode {
                start_date: current.then_some(today),
                finish_date: current.then_some(today),
                ..node
            }
        })
        .collect();
    app.work_items.set_classification_nodes(rolled_over, None);
    app.work_items.show_all(&mut app.shell, None);
    assert_eq!(
        visible_ids(&app),
        vec![2, 3, 4, 5],
        "the same saved query follows the sprint over its rollover"
    );

    app.work_items.set_classification_nodes(Vec::new(), None);
    app.work_items.show_all(&mut app.shell, None);
    assert!(
        visible_ids(&app).is_empty(),
        "with no sprint scheduled @current is no sprint at all"
    );
}

/// A backlog with one work item in every category the table can hold, so
/// what the finished rule takes and what it leaves are both several rows.
fn backlog_app() -> App {
    let row = |id: i64, title: &str, state: &str, changed: &str| Ticket {
        state: state.into(),
        ..ticket(id, title, changed)
    };
    App::new(vec![
        row(1, "Still to start", "To Do", "2026-03-05T00:00:00Z"),
        row(2, "Under way", "Doing", "2026-03-04T00:00:00Z"),
        row(3, "Waiting on test", "Resolved", "2026-03-03T00:00:00Z"),
        row(4, "Finished", "Done", "2026-03-02T00:00:00Z"),
        row(5, "Cut", "Removed", "2026-03-01T00:00:00Z"),
    ])
}

#[test]
fn a_fresh_session_opens_on_the_open_backlog_with_the_finished_rows_left_out() {
    let app = backlog_app();

    assert_eq!(
        visible_ids(&app),
        vec![1, 2, 3],
        "Completed and Removed go; Resolved is still somebody's problem"
    );
    assert!(app.work_items.finished_hidden());
    assert_eq!(app.work_items.hidden_finished(&app.shell), 2);

    let context = app.work_items.agent_context(&app.shell).tickets;
    assert!(
        context.finished_hidden,
        "an agent is told the rows it can see are a subset"
    );
    assert_eq!(
        (context.matching_count, context.total_count),
        (3, 5),
        "the total stays the whole database, so the difference is the count hidden"
    );
}

#[test]
fn showing_the_finished_tickets_puts_them_back_and_the_choice_outlives_the_run() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tickets.session.json");
    let mut app = backlog_app();

    app.work_items.set_show_finished(&mut app.shell, true);

    assert_eq!(visible_ids(&app), vec![1, 2, 3, 4, 5]);
    assert!(!app.work_items.finished_hidden());
    assert_eq!(app.work_items.hidden_finished(&app.shell), 0);
    session::save(&path, &app.work_items.snapshot_session(&app.shell)).unwrap();

    let mut restored = backlog_app();
    restored
        .work_items
        .restore_session(&mut restored.shell, session::load(&path).unwrap());
    assert!(
        restored.work_items.show_finished(),
        "the choice comes back off the session file"
    );
    assert_eq!(visible_ids(&restored), vec![1, 2, 3, 4, 5]);

    // The palette command and the chip's `×` are the two ways to it, and
    // they turn the same setting over.
    restored
        .work_items
        .run_command(&mut restored.shell, CommandId::ToggleFinished);
    assert_eq!(visible_ids(&restored), vec![1, 2, 3]);
    restored
        .work_items
        .activate_target(&mut restored.shell, PointerTarget::ShowFinished, 0, 0);
    assert_eq!(visible_ids(&restored), vec![1, 2, 3, 4, 5]);
}

#[test]
fn a_state_the_query_names_lists_finished_work_while_the_toggle_stays_on() {
    let mut app = backlog_app();

    app.work_items
        .set_query(&mut app.shell, "state:done".into());

    assert_eq!(visible_ids(&app), vec![4], "state:done just works");
    assert!(
        !app.work_items.finished_hidden(),
        "the query names a state, so nothing is being left out behind it"
    );
    assert!(
        !app.work_items.show_finished(),
        "and the setting itself is untouched, so clearing the query hides them again"
    );

    app.work_items.set_query(&mut app.shell, String::new());
    assert_eq!(visible_ids(&app), vec![1, 2, 3]);
}

#[test]
fn the_open_sentinel_and_the_toggle_ask_for_the_same_rows_rather_than_fighting() {
    let mut app = backlog_app();
    let hidden_by_the_toggle = visible_ids(&app);

    app.work_items
        .set_query(&mut app.shell, "state:@open".into());

    assert_eq!(
        visible_ids(&app),
        hidden_by_the_toggle,
        "the toggle is that sentinel, so writing it out changes nothing"
    );
    assert!(!app.work_items.finished_hidden());

    app.work_items.set_show_finished(&mut app.shell, true);
    assert_eq!(
        visible_ids(&app),
        hidden_by_the_toggle,
        "and a query that asks for open work still means it once they are shown"
    );
}

#[test]
fn a_built_in_view_that_names_a_state_takes_the_toggle_out_of_the_way() {
    let mut app = views_app();
    assert!(
        app.work_items.finished_hidden(),
        "nothing named a state yet"
    );

    let row = view_row(&app, "Mine");

    app.work_items.apply_view_at(&mut app.shell, row);
    assert_eq!(
        visible_ids(&app),
        vec![1],
        "Mine names no state, so the finished row it matches stays off the table"
    );
    assert!(app.work_items.finished_hidden());

    let row = view_row(&app, "Doing");

    app.work_items.apply_view_at(&mut app.shell, row);
    assert_eq!(visible_ids(&app), vec![1]);
    assert!(
        !app.work_items.finished_hidden(),
        "state:doing is a state the query named, whatever it happens to match"
    );

    let row = view_row(&app, "Stale");

    app.work_items.apply_view_at(&mut app.shell, row);
    assert_eq!(
        visible_ids(&app),
        vec![4, 3],
        "Stale asks for open work itself; the finished row is out either way"
    );
    assert!(!app.work_items.finished_hidden());
}

#[test]
fn a_finished_relative_is_still_in_the_family_tree_of_the_row_that_holds_it() {
    let mut app = App::new(epic_tickets());
    app.work_items
        .set_workspace_graph(&mut app.shell, epic_graph());

    assert_eq!(
        visible_ids(&app),
        vec![1, 4, 5],
        "the closed and the removed child are off the table"
    );

    let family: Vec<i64> = app
        .work_items
        .visible_family_tree()
        .into_iter()
        .map(|entry| entry.key.id)
        .collect();
    assert!(
        family.contains(&2) && family.contains(&3),
        "the epic's own children are its family however the table is filtered: {family:?}"
    );
    assert_eq!(
        app.work_items
            .child_progress(&family_key(1))
            .map(|progress| progress.done),
        Some(2),
        "and they still count towards how far it has got"
    );

    app.work_items
        .jump_to_ticket(&mut app.shell, &family_key(2));
    assert_eq!(
        app.shell.notification(),
        Some((
            "2 is finished, and finished tickets are hidden",
            NotificationLevel::Info
        )),
        "following one back to the table says which rule is in the way"
    );
}

#[test]
fn the_facets_count_what_the_table_shows_but_still_offer_the_finished_states() {
    let app = backlog_app();

    let states: Vec<(String, usize)> = app
        .work_items
        .facets_for(&app.shell, FilterField::State)
        .into_iter()
        .map(|facet| (facet.value, facet.count))
        .collect();
    assert!(
        states.contains(&("Done".to_owned(), 1)),
        "a state has to be listed to be checked off: {states:?}"
    );

    let types: Vec<usize> = app
        .work_items
        .facets_for(&app.shell, FilterField::Type)
        .into_iter()
        .map(|facet| facet.count)
        .collect();
    assert_eq!(
        types,
        vec![3],
        "every other field counts the rows the table is showing"
    );
}

#[test]
fn sentinels_come_back_from_the_session_file_as_the_chips_they_were_typed_as() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tickets.session.json");
    let mut app = views_app();
    app.work_items
        .set_query(&mut app.shell, "assignee:@me iteration:@current".into());
    app.work_items.save_view(&mut app.shell, "My sprint".into());
    session::save(&path, &app.work_items.snapshot_session(&app.shell)).unwrap();

    let mut restored = views_app();
    restored
        .work_items
        .restore_session(&mut restored.shell, session::load(&path).unwrap());

    assert_eq!(
        restored.work_items.query(),
        "assignee:@me iteration:@current"
    );
    assert_eq!(restored.work_items.views()[0].name, "My sprint");
    assert_eq!(
        restored.work_items.views()[0].query,
        "assignee:@me iteration:@current"
    );
    let labels: Vec<String> = restored
        .work_items
        .filter_tokens()
        .iter()
        .map(FilterToken::chip_label)
        .collect();
    assert_eq!(labels, vec!["assignee:@me", "iteration:@current"]);

    let context = restored.work_items.agent_context(&restored.shell);
    assert_eq!(context.search.query, "assignee:@me iteration:@current");
    assert_eq!(
        context.search.filters,
        vec!["assignee:@me", "iteration:@current"],
        "an agent reads the sentinels as typed and the me field beside them"
    );
    assert_eq!(context.me.as_deref(), Some("Avery Chen"));
    assert_eq!(
        visible_ids(&restored),
        vec![1],
        "and the query still means me, in this sprint"
    );
}

#[test]
fn a_stored_view_never_takes_a_name_a_built_in_owns() {
    let mut app = views_app();

    app.work_items.restore_session(
        &mut app.shell,
        Session {
            views: vec![NamedView {
                name: "Mine".into(),
                query: "tag:rust".into(),
                sort_field: SortField::Changed,
                sort_direction: SortDirection::Descending,
                search_order: SearchOrder::Relevance,
                row_density: RowDensity::Compact,
                columns: Vec::new(),
                auto_hide: true,
            }],
            ..Session::default()
        },
    );

    assert!(app.work_items.views().is_empty());
    assert_eq!(
        app.work_items
            .view_rows()
            .iter()
            .filter(|row| row.label == "Mine")
            .count(),
        1,
        "a session written before the built-ins existed lists Mine once"
    );
    assert_eq!(app.work_items.view_rows()[1].query, "assignee:@me");
}

#[test]
fn date_filters_come_back_from_the_session_file_as_the_chips_they_were_typed_as() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tickets.session.json");
    let mut app = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
    app.work_items.set_query(
        &mut app.shell,
        "changed:<7d created:>2026-08-01 rust".into(),
    );
    session::save(&path, &app.work_items.snapshot_session(&app.shell)).unwrap();

    let mut restored = App::new(vec![ticket(1, "Search", "2026-01-01T00:00:00Z")]);
    restored
        .work_items
        .restore_session(&mut restored.shell, session::load(&path).unwrap());

    assert_eq!(
        restored.work_items.query(),
        "changed:<7d created:>2026-08-01 rust"
    );
    let labels: Vec<String> = restored
        .work_items
        .filter_tokens()
        .iter()
        .map(FilterToken::chip_label)
        .collect();
    assert_eq!(labels, vec!["changed:<7d", "created:>2026-08-01"]);
}

/// A work item in `state`, last touched `changed_at`.
fn aged(id: i64, state: &str, changed_at: &str) -> Ticket {
    Ticket {
        state: state.into(),
        ..ticket(id, "Neglected", changed_at)
    }
}

#[test]
fn the_stale_threshold_flags_open_work_past_it_and_never_finished_work() {
    let now = crate::timestamp::ts("2026-08-29T12:00:00Z");
    let app = App::new(vec![]);

    assert_eq!(app.work_items.stale_days(), DEFAULT_STALE_DAYS);
    assert_eq!(
        BUILTIN_VIEWS
            .iter()
            .find(|view| view.name == "Stale")
            .map(|view| view.query),
        Some(stale_query(DEFAULT_STALE_DAYS).as_str()),
        "the built-in view asks the question the highlight answers"
    );
    assert_eq!(
        app.work_items
            .stale_age_days_at(&aged(1, "To Do", "2026-08-08T12:00:00Z"), now),
        Some(21),
        "three weeks untouched is flagged, and the pane says how long"
    );
    assert_eq!(
        app.work_items
            .stale_age_days_at(&aged(2, "To Do", "2026-08-15T12:00:00Z"), now),
        None,
        "exactly fourteen days has not crossed the threshold yet"
    );
    assert_eq!(
        app.work_items
            .stale_age_days_at(&aged(3, "To Do", "2026-08-15T11:59:59Z"), now),
        Some(14),
        "a second past it has"
    );
    for finished in ["Done", "Closed", "Removed"] {
        assert_eq!(
            app.work_items
                .stale_age_days_at(&aged(4, finished, "2025-01-01T00:00:00Z"), now),
            None,
            "{finished} work is never stale, whatever its age"
        );
    }
}

#[test]
fn the_stale_threshold_takes_the_flag_over_the_session_and_the_palette_over_both() {
    let now = crate::timestamp::ts("2026-08-29T12:00:00Z");
    let three_weeks_old = aged(1, "To Do", "2026-08-08T12:00:00Z");
    let mut app = App::new(vec![]);

    app.work_items.set_stale_days(&mut app.shell, 30);
    let session = app.work_items.snapshot_session(&app.shell);
    let mut restored = App::new(vec![]);
    restored
        .work_items
        .restore_session(&mut restored.shell, session.clone());
    assert_eq!(
        restored.work_items.stale_days(),
        30,
        "the session remembers it"
    );
    assert_eq!(
        restored.work_items.stale_age_days_at(&three_weeks_old, now),
        None
    );

    // `--stale-days`, or TICKET_TUI_STALE_DAYS, is applied after the
    // session has been restored, and beats what it carried.
    restored.work_items.override_stale_days(7);
    assert_eq!(restored.work_items.stale_days(), 7);
    assert_eq!(
        restored.work_items.stale_age_days_at(&three_weeks_old, now),
        Some(21)
    );
    assert_eq!(
        restored
            .work_items
            .snapshot_session(&restored.shell)
            .stale_days,
        30,
        "a flag passed once does not quietly become the setting"
    );

    restored
        .work_items
        .run_command(&mut restored.shell, CommandId::SetStaleThreshold);
    assert_eq!(
        restored.work_items.stale_days(),
        14,
        "the palette steps up from the seven days in force"
    );
    assert_eq!(
        restored
            .work_items
            .snapshot_session(&restored.shell)
            .stale_days,
        14,
        "and the palette is what gets remembered"
    );
}

#[test]
fn setting_the_stale_threshold_steps_through_the_choices_and_names_the_query() {
    let mut app = App::new(vec![]);

    let steps: Vec<u16> = (0..5)
        .map(|_| {
            app.work_items
                .run_command(&mut app.shell, CommandId::SetStaleThreshold);
            app.work_items.stale_days()
        })
        .collect();
    assert_eq!(
        steps,
        vec![21, 30, 7, 14, 21],
        "the choices step upward and wrap round"
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Stale after 21 days · changed:>21d state:@open"),
        "the status names the query the highlight stands for"
    );
    assert!(
        app.shell.session_dirty,
        "moving the setting is worth saving"
    );
}

#[test]
fn a_threshold_of_no_days_at_all_is_held_at_the_one_day_floor() {
    let mut app = App::new(vec![]);

    app.work_items.override_stale_days(0);
    assert_eq!(app.work_items.stale_days(), 1);

    app.work_items.set_stale_days(&mut app.shell, 0);
    assert_eq!(app.work_items.stale_days(), 1);

    let mut restored = App::new(vec![]);
    restored.work_items.restore_session(
        &mut restored.shell,
        Session {
            stale_days: 0,
            ..Session::default()
        },
    );
    assert_eq!(
        restored.work_items.stale_days(),
        1,
        "including one edited by hand"
    );
}

/// A sprint of seven work items — two people and one pile nobody owns,
/// spread across the three board columns — with an eighth parked in the
/// quarter beside it. One open item and one finished item have sat
/// untouched since January, so the stale rule has something to bite on and
/// something to leave alone.
fn sprint_tickets() -> Vec<Ticket> {
    let planned =
        |id: i64, state: &str, assignee: Option<&str>, node: &str, changed: &str| Ticket {
            state: state.into(),
            assigned_to: assignee.map(Into::into),
            iteration_path: node.into(),
            ..ticket(id, "Sprint work", changed)
        };
    let sprint = "development\\Sprint 1";
    vec![
        planned(1, "To Do", Some("Avery"), sprint, "2026-08-28T00:00:00Z"),
        planned(2, "Doing", Some("Avery"), sprint, "2026-08-27T00:00:00Z"),
        planned(3, "Done", Some("Avery"), sprint, "2026-08-26T00:00:00Z"),
        planned(4, "Done", Some("Avery"), sprint, "2026-08-25T00:00:00Z"),
        planned(5, "To Do", Some("Blake"), sprint, "2026-08-24T00:00:00Z"),
        planned(6, "Done", Some("Blake"), sprint, "2026-01-06T00:00:00Z"),
        planned(7, "To Do", None, sprint, "2026-01-05T00:00:00Z"),
        planned(
            8,
            "Doing",
            Some("Avery"),
            "development\\Q3",
            "2026-08-22T00:00:00Z",
        ),
    ]
}

/// The sprint above with no iteration tree cached, which is the state a
/// project whose sprints carry no dates is always in.
fn sprint_app() -> App {
    App::new(sprint_tickets())
}

/// What the cursor is sitting on in the open sprint summary.
fn summary_cursor(app: &App) -> SummaryRowKind {
    app.work_items.summary_rows()[app.work_items.sprint_overlay.index].kind
}

#[test]
fn the_sprint_summary_counts_every_row_including_the_finished_ones_the_table_hides() {
    let mut app = sprint_app();
    assert!(
        app.work_items.finished_hidden(),
        "the table leaves finished work out"
    );
    assert_eq!(
        visible_ids(&app),
        vec![1, 2, 5, 8, 7],
        "the three Done rows are off the table"
    );

    app.work_items
        .run_command(&mut app.shell, CommandId::SprintSummary);

    assert_eq!(app.work_items.mode, WorkItemMode::Sprint);
    let summary = app
        .work_items
        .sprint_summary()
        .expect("the selected row names a sprint");
    assert_eq!(summary.iteration, "development\\Sprint 1");
    let grid: Vec<(&str, [usize; 3], usize)> = summary
        .assignees
        .iter()
        .map(|row| (row.name.as_str(), row.counts, row.total()))
        .collect();
    assert_eq!(
        grid,
        vec![
            ("Avery", [1, 1, 2], 4),
            ("Blake", [1, 0, 1], 2),
            ("Unassigned", [1, 0, 0], 1),
        ],
        "the Done column is filled from every work item on file, not from the five \
             rows the table is showing"
    );
    assert_eq!(summary.total.counts, [3, 1, 3], "and so is the Total row");
    assert_eq!(
        summary.items(),
        7,
        "the work item in Q3 is another sprint's"
    );
    assert_eq!(summary.types, vec![("Task".to_owned(), 7)]);
    assert_eq!(summary.done_percent(), 43);
}

#[test]
fn the_summary_stale_figure_is_the_one_the_changed_column_paints() {
    let mut app = sprint_app();
    app.work_items
        .run_command(&mut app.shell, CommandId::SprintSummary);
    let now = Timestamp::now();

    let summary = app.work_items.sprint_summary().expect("a sprint to count");

    assert_eq!(
        summary.stale,
        app.work_items
            .tickets()
            .iter()
            .filter(|ticket| ticket.iteration_path == summary.iteration)
            .filter(|ticket| is_stale(ticket, app.work_items.stale_days(), now))
            .count(),
        "the summary and the highlight ask the same question of the same rows"
    );
    assert_eq!(
        summary.stale, 1,
        "the open work item nobody has touched since January, and never the \
             finished one beside it"
    );
}

#[test]
fn the_summary_falls_back_to_the_sprint_the_selected_row_is_planned_into() {
    let mut app = sprint_app();
    assert_eq!(
        app.work_items.current_iteration(),
        None,
        "no iteration is scheduled, which is every project whose sprints carry no dates"
    );

    app.work_items.select_row(&mut app.shell, 3);
    assert_eq!(
        app.work_items.selected_ticket().map(|ticket| ticket.key.id),
        Some(8)
    );
    app.work_items
        .run_command(&mut app.shell, CommandId::SprintSummary);

    assert_eq!(
        app.work_items.sprint_overlay.iteration.as_deref(),
        Some("development\\Q3")
    );
    assert_eq!(
        app.work_items.sprint_summary().expect("a sprint").items(),
        1
    );
    assert_eq!(
        app.work_items.summary_title(),
        " Sprint summary \u{b7} Q3 ",
        "the title names the sprint, not the path it hangs off"
    );
}

#[test]
fn the_summary_says_so_when_there_is_no_sprint_and_no_row_to_borrow_one_from() {
    let mut app = App::new(vec![]);

    app.work_items
        .run_command(&mut app.shell, CommandId::SprintSummary);

    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Sprint,
        "the overlay opens either way"
    );
    assert_eq!(app.work_items.sprint_overlay.iteration, None);
    assert!(app.work_items.sprint_summary().is_none());
    assert_eq!(
        app.work_items
            .summary_rows()
            .iter()
            .map(|row| row.text.clone())
            .collect::<Vec<_>>(),
        NO_SPRINT_NOTICE.map(str::to_owned).to_vec(),
        "it explains itself rather than painting an empty grid"
    );
    assert_eq!(app.work_items.summary_title(), " Sprint summary ");

    press(&mut app, KeyCode::Right);
    assert_eq!(
        app.work_items.sprint_overlay.iteration, None,
        "with no tree cached there is nowhere to step to"
    );
    assert_eq!(press(&mut app, KeyCode::Enter), AppAction::None);
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Sprint,
        "and nothing to filter to"
    );
    assert!(app.work_items.query().is_empty());
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
}

#[test]
fn the_summary_cursor_opens_on_the_first_grid_row_and_steps_over_the_rest() {
    let mut app = sprint_app();
    app.work_items
        .run_command(&mut app.shell, CommandId::SprintSummary);

    assert_eq!(
        summary_cursor(&app),
        SummaryRowKind::Assignee(0),
        "the column headings are read, not landed on"
    );
    press(&mut app, KeyCode::Up);
    assert_eq!(
        summary_cursor(&app),
        SummaryRowKind::Assignee(0),
        "and there is nowhere above them to go"
    );

    for _ in 0..3 {
        press(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(summary_cursor(&app), SummaryRowKind::Total);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(
        summary_cursor(&app),
        SummaryRowKind::Total,
        "the by-type tally and the headline are read too"
    );
}

#[test]
fn enter_on_a_grid_row_filters_the_table_to_that_persons_sprint_work() {
    let mut app = sprint_app();
    app.work_items
        .run_command(&mut app.shell, CommandId::SprintSummary);
    assert_eq!(summary_cursor(&app), SummaryRowKind::Assignee(0));

    press(&mut app, KeyCode::Enter);

    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Browse,
        "the overlay closes so the rows it counted are there to look at"
    );
    assert_eq!(
        app.work_items.query(),
        "assignee:Avery iteration:\"development\\\\Sprint 1\"",
        "the full path the grid counted over, so the table shows exactly those rows"
    );
    assert_eq!(visible_ids(&app), vec![1, 2]);
    assert_eq!(
        app.work_items.hidden_finished(&app.shell),
        2,
        "the summary counted Avery's two finished items; the table's own rule \
             holds them back, and the chip over the table says how many"
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Avery in Sprint 1")
    );
}

#[test]
fn enter_on_the_unassigned_row_asks_for_the_work_nobody_owns() {
    let mut app = sprint_app();
    app.work_items
        .run_command(&mut app.shell, CommandId::SprintSummary);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(summary_cursor(&app), SummaryRowKind::Assignee(2));

    press(&mut app, KeyCode::Enter);

    assert_eq!(
        app.work_items.query(),
        "assignee:Unassigned iteration:\"development\\\\Sprint 1\""
    );
    assert_eq!(visible_ids(&app), vec![7]);
}

#[test]
fn enter_on_the_total_row_asks_for_the_whole_sprint() {
    let mut app = sprint_app();
    app.work_items
        .run_command(&mut app.shell, CommandId::SprintSummary);
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(summary_cursor(&app), SummaryRowKind::Total);

    press(&mut app, KeyCode::Enter);

    assert_eq!(
        app.work_items.query(),
        "iteration:\"development\\\\Sprint 1\"",
        "nobody's name on it"
    );
    assert_eq!(visible_ids(&app), vec![1, 2, 5, 7]);
}

#[test]
fn two_sprints_sharing_a_leaf_name_stay_apart_when_the_summary_filters() {
    // The grid counts a work item whose whole iteration path matches, so
    // the query it hands the table has to be just as exact: a bare
    // `Sprint 1` also matches `development\Release 2\Sprint 1`, and the
    // table would then show rows the grid never counted.
    let mut tickets = sprint_tickets();
    tickets.push(Ticket {
        state: "To Do".into(),
        assigned_to: Some("Avery".into()),
        iteration_path: "development\\Release 2\\Sprint 1".into(),
        ..ticket(9, "Another sprint of the same name", "2026-08-21T00:00:00Z")
    });
    let mut app = App::new(tickets);
    app.work_items.select_row(&mut app.shell, 0);

    app.work_items
        .run_command(&mut app.shell, CommandId::SprintSummary);
    let summary = app
        .work_items
        .sprint_summary()
        .expect("the selected row names a sprint");
    assert_eq!(summary.iteration, "development\\Sprint 1");
    assert_eq!(
        summary.total.total(),
        7,
        "the namesake sprint is not counted"
    );

    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(summary_cursor(&app), SummaryRowKind::Total);
    press(&mut app, KeyCode::Enter);

    assert_eq!(
        app.work_items.query(),
        "iteration:\"development\\\\Sprint 1\""
    );
    assert_eq!(
        visible_ids(&app),
        vec![1, 2, 5, 7],
        "#9 sits in a different node that happens to end in the same name"
    );
}

#[test]
fn left_and_right_walk_the_cached_iterations_and_stop_at_either_end() {
    let mut app = sprint_app();
    app.work_items
        .set_classification_nodes(classification_trees(), None);
    app.work_items.select_row(&mut app.shell, 3);
    assert_eq!(
        app.work_items.selected_ticket().map(|ticket| ticket.key.id),
        Some(8)
    );

    app.work_items
        .run_command(&mut app.shell, CommandId::SprintSummary);
    assert_eq!(
        app.work_items.sprint_overlay.iteration.as_deref(),
        Some("development\\Sprint 1"),
        "a scheduled sprint wins over the one the selected row sits in"
    );

    press(&mut app, KeyCode::Right);
    assert_eq!(
        app.work_items.sprint_overlay.iteration.as_deref(),
        Some("development\\Q3")
    );
    assert_eq!(
        app.work_items.sprint_summary().expect("a sprint").items(),
        1,
        "and the grid follows the step"
    );
    press(&mut app, KeyCode::Char('l'));
    assert_eq!(
        app.work_items.sprint_overlay.iteration.as_deref(),
        Some("development\\Q3\\Sprint 7")
    );
    press(&mut app, KeyCode::Right);
    assert_eq!(
        app.work_items.sprint_overlay.iteration.as_deref(),
        Some("development\\Q3\\Sprint 7"),
        "the last one is the last one, rather than wrapping round"
    );

    for _ in 0..3 {
        press(&mut app, KeyCode::Char('h'));
    }
    assert_eq!(
        app.work_items.sprint_overlay.iteration.as_deref(),
        Some("development\\Sprint 1"),
        "and the project root is somewhere to file work, not a sprint to stop on"
    );
}
