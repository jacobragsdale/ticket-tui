use super::*;

#[test]
fn walking_a_form_moves_a_field_at_a_time_and_wraps_at_both_ends() {
    let mut app = creating_app();
    press(&mut app, KeyCode::Char('n'));

    assert_eq!(app.work_items.mode, WorkItemMode::Form);
    let fields = app
        .work_items
        .form
        .as_ref()
        .expect("the form is open")
        .fields
        .len();
    assert_eq!(
        fields, 8,
        "type, title, parent, iteration, area, assignee, priority, tags"
    );
    assert_eq!(app.work_items.form.as_ref().unwrap().index, 0);

    press(&mut app, KeyCode::Down);
    assert_eq!(
        app.work_items.form.as_ref().unwrap().index,
        1,
        "down moves on"
    );
    press(&mut app, KeyCode::Tab);
    assert_eq!(
        app.work_items.form.as_ref().unwrap().index,
        2,
        "tab moves on too"
    );
    press(&mut app, KeyCode::BackTab);
    assert_eq!(
        app.work_items.form.as_ref().unwrap().index,
        1,
        "shift-tab moves back"
    );
    press(&mut app, KeyCode::Up);
    assert_eq!(app.work_items.form.as_ref().unwrap().index, 0);

    press(&mut app, KeyCode::Up);
    assert_eq!(
        app.work_items.form.as_ref().unwrap().index,
        fields - 1,
        "up from the first field wraps to the last"
    );
    press(&mut app, KeyCode::Down);
    assert_eq!(
        app.work_items.form.as_ref().unwrap().index,
        0,
        "and down from the last comes back to the first"
    );
}

#[test]
fn enter_on_a_picker_field_opens_that_picker_and_the_choice_lands_in_the_form() {
    let mut app = creating_app();
    app.work_items
        .set_work_item_types(vec!["Epic".into(), "Issue".into(), "Task".into()]);
    app.work_items.set_identities(vec![Identity::new(
        "Avery Chen",
        Some("avery@example.com".into()),
    )]);
    app.work_items.set_classification_nodes(
        vec![ClassificationNode {
            kind: NodeKind::Iteration,
            path: "Atlas\\Sprint 2".into(),
            depth: 1,
            start_date: None,
            finish_date: None,
        }],
        Some(Timestamp::now()),
    );
    press(&mut app, KeyCode::Char('n'));

    focus_field(&mut app, FormFieldId::Type);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.work_items.mode, WorkItemMode::TypePicker);
    assert_eq!(
        app.work_items.type_picker.options,
        ["Epic", "Issue", "Task"]
    );
    press(&mut app, KeyCode::Home);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Form,
        "the picker hands back to the form"
    );
    assert_eq!(
        app.work_items
            .form
            .as_ref()
            .unwrap()
            .value(FormFieldId::Type),
        "Epic"
    );

    focus_field(&mut app, FormFieldId::Iteration);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.work_items.mode, WorkItemMode::NodePicker);
    assert_eq!(
        app.work_items.node_picker.scope,
        EditScope::Form(FormFieldId::Iteration),
        "the picker knows which field it is filling in"
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.work_items.mode, WorkItemMode::Form);
    assert_eq!(
        app.work_items
            .form
            .as_ref()
            .unwrap()
            .value(FormFieldId::Iteration),
        "Atlas\\Sprint 2"
    );

    focus_field(&mut app, FormFieldId::Assignee);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.work_items.mode, WorkItemMode::AssigneePicker);
    let row = app
        .work_items
        .assignee_matches()
        .iter()
        .position(|candidate| candidate.display == "Avery Chen")
        .expect("the picker offers the person the project knows");
    app.work_items.choose_assignee(&mut app.shell, row);
    assert_eq!(app.work_items.mode, WorkItemMode::Form);
    assert_eq!(
        app.work_items
            .form
            .as_ref()
            .unwrap()
            .value(FormFieldId::Assignee),
        "Avery Chen"
    );

    focus_field(&mut app, FormFieldId::Title);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Form,
        "enter on a typed field moves on rather than filing the form"
    );
}

#[test]
fn escaping_a_picker_a_form_opened_goes_back_to_the_form_rather_than_the_table() {
    let mut app = creating_app();
    press(&mut app, KeyCode::Char('n'));
    focus_field(&mut app, FormFieldId::Iteration);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.work_items.mode, WorkItemMode::NodePicker);

    press(&mut app, KeyCode::Esc);

    assert_eq!(app.work_items.mode, WorkItemMode::Form);
    assert!(
        app.work_items.form.is_some(),
        "the form is still open behind it"
    );
}

#[test]
fn submitting_a_form_sends_the_fields_it_holds_and_the_parent_as_a_link() {
    let mut app = creating_app();
    app.work_items.set_identities(vec![Identity::new(
        "Avery Chen",
        Some("avery@example.com".into()),
    )]);
    press(&mut app, KeyCode::Char('n'));

    focus_field(&mut app, FormFieldId::Title);
    type_text(&mut app, "Back off on throttling");
    focus_field(&mut app, FormFieldId::Parent);
    type_text(&mut app, "10");
    focus_field(&mut app, FormFieldId::Assignee);
    app.work_items
        .form
        .as_mut()
        .unwrap()
        .set_value(FormFieldId::Assignee, "Avery Chen");
    focus_field(&mut app, FormFieldId::Priority);
    type_text(&mut app, "2");
    focus_field(&mut app, FormFieldId::Tags);
    type_text(&mut app, "sync;  infra");

    let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

    let AppAction::Create {
        work_item_type,
        patch,
        parent,
    } = action
    else {
        panic!("Ctrl-S files the form");
    };
    assert_eq!(work_item_type, "Issue", "the default type is filed as is");
    assert_eq!(parent, Some(10));
    assert_eq!(
        patch,
        vec![
            crate::edit::set_field(crate::edit::TITLE_FIELD, "Back off on throttling"),
            crate::edit::set_field(crate::edit::ASSIGNED_TO_FIELD, "avery@example.com"),
            crate::edit::set_field(crate::edit::PRIORITY_FIELD, 2),
            crate::edit::set_field(crate::edit::ITERATION_PATH_FIELD, "Atlas\\Sprint 1"),
            crate::edit::set_field(crate::edit::AREA_PATH_FIELD, "Atlas"),
            crate::edit::set_field(crate::edit::TAGS_FIELD, "sync; infra"),
        ],
        "the fields travel in the order the form holds them, and only the ones filled in"
    );

    let config = crate::azure::AzureConfig {
        organization: "demo".into(),
        project: "atlas".into(),
        code_project: "atlas".into(),
        scope: None,
    };
    let document = crate::azure::create_document(&patch, parent, &config);
    assert_eq!(&document[..patch.len()], &patch[..], "the fields lead");
    assert_eq!(
        document[patch.len()],
        serde_json::json!({
            "op": "add",
            "path": "/relations/-",
            "value": {
                "rel": "System.LinkTypes.Hierarchy-Reverse",
                "url": "https://dev.azure.com/demo/_apis/wit/workItems/10",
            },
        }),
        "the parent travels as a link rather than as a field"
    );
    assert!(
        app.work_items.creates_pending(),
        "the form is held until it answers"
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Creating Issue\u{2026}")
    );
}

#[test]
fn a_form_missing_a_required_field_or_holding_nonsense_refuses_to_be_sent() {
    let mut app = creating_app();
    press(&mut app, KeyCode::Char('n'));

    let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert_eq!(action, AppAction::None, "nothing goes out without a title");
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Form,
        "the form stays open on it"
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Title is required")
    );
    assert_eq!(
        app.work_items.form.as_ref().unwrap().focused().unwrap().id,
        FormFieldId::Title,
        "the cursor lands on the field the refusal names"
    );

    focus_field(&mut app, FormFieldId::Title);
    type_text(&mut app, "Something to do");
    app.work_items
        .form
        .as_mut()
        .unwrap()
        .set_value(FormFieldId::Type, "   ");
    let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert_eq!(action, AppAction::None);
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Type is required")
    );

    app.work_items
        .form
        .as_mut()
        .unwrap()
        .set_value(FormFieldId::Type, "Issue");
    focus_field(&mut app, FormFieldId::Priority);
    type_text(&mut app, "high");
    let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert_eq!(action, AppAction::None, "garbage is refused, not sent");
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Priority must be a whole number, not \"high\"")
    );
    assert!(!app.work_items.creates_pending());
}

#[test]
fn escape_keeps_the_draft_and_opening_the_form_again_brings_it_back() {
    let mut app = creating_app();
    press(&mut app, KeyCode::Char('n'));
    focus_field(&mut app, FormFieldId::Title);
    type_text(&mut app, "Half a thought");

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert!(app.work_items.form.is_none(), "the form is closed");

    press(&mut app, KeyCode::Char('n'));

    assert_eq!(app.work_items.mode, WorkItemMode::Form);
    let form = app.work_items.form.as_ref().expect("the draft came back");
    assert_eq!(form.value(FormFieldId::Title), "Half a thought");
    assert_eq!(
        form.focused().unwrap().id,
        FormFieldId::Title,
        "and the cursor came back with it"
    );
}

/// The type the child form fills in over a parent of this type, with the
/// project's own types read as they came back from Azure DevOps.
fn child_type_under(parent_type: &str, types: &[&str]) -> String {
    let mut app = parent_app(parent_type);
    app.work_items
        .set_work_item_types(types.iter().map(|name| (*name).to_owned()).collect());
    press(&mut app, KeyCode::Char('N'));
    app.work_items
        .form
        .as_ref()
        .expect("the child form is open")
        .value(FormFieldId::Type)
        .to_owned()
}

#[test]
fn the_child_form_files_the_type_the_parents_own_type_breaks_down_into() {
    // The order `GET /_apis/wit/workitemtypes` answers in is the process's own
    // and is no hierarchy: this one lists the Issue before the Epic it hangs
    // under. Reading the next name out of it filed an Epic under an Issue.
    let basic = ["Issue", "Epic", "Task"];
    for (parent_type, child_type) in [("Epic", "Issue"), ("Issue", "Task"), ("Task", "Task")] {
        assert_eq!(
            child_type_under(parent_type, &basic),
            child_type,
            "a Basic project breaks a {parent_type} down into a {child_type} \
             whatever order its types came back in"
        );
    }
}

#[test]
fn a_project_on_another_process_breaks_its_work_down_that_processs_way() {
    let agile = [
        "Bug",
        "Epic",
        "Feature",
        "Issue",
        "Task",
        "Test Case",
        "User Story",
    ];
    for (parent_type, child_type) in [
        ("Epic", "Feature"),
        ("Feature", "User Story"),
        ("User Story", "Task"),
    ] {
        assert_eq!(
            child_type_under(parent_type, &agile),
            child_type,
            "Agile breaks a {parent_type} down into a {child_type}, though its \
             types name an Issue as Basic's do"
        );
    }

    let scrum = [
        "Bug",
        "Epic",
        "Feature",
        "Impediment",
        "Product Backlog Item",
        "Task",
    ];
    for (parent_type, child_type) in [
        ("Epic", "Feature"),
        ("Feature", "Product Backlog Item"),
        ("Product Backlog Item", "Task"),
    ] {
        assert_eq!(
            child_type_under(parent_type, &scrum),
            child_type,
            "Scrum breaks a {parent_type} down into a {child_type}"
        );
    }
}

#[test]
fn a_child_type_the_project_does_not_offer_leaves_the_parents_own_type() {
    assert_eq!(
        child_type_under("Epic", &["Epic", "Task"]),
        "Epic",
        "a project with no Issue in it is filed no Issue"
    );
}

#[test]
fn a_child_of_a_type_the_process_says_nothing_about_falls_back_to_the_basic_rule() {
    for (parent_type, child_type) in [
        ("Epic", "Issue"),
        ("Issue", "Task"),
        ("Task", "Task"),
        ("Bug", "Bug"),
    ] {
        let mut app = parent_app(parent_type);
        press(&mut app, KeyCode::Char('N'));

        assert_eq!(
            app.work_items
                .form
                .as_ref()
                .expect("the child form is open")
                .value(FormFieldId::Type),
            child_type,
            "with no cached types {parent_type} still breaks down into {child_type}"
        );
    }
}

#[test]
fn the_child_form_inherits_the_area_and_the_iteration_the_parent_sits_in() {
    let mut app = parent_app("Epic");
    press(&mut app, KeyCode::Char('N'));

    let form = app
        .work_items
        .form
        .as_ref()
        .expect("the child form is open");
    assert_eq!(form.value(FormFieldId::Area), "Atlas\\Platform");
    assert_eq!(form.value(FormFieldId::Iteration), "Atlas\\Sprint 3");

    focus_field(&mut app, FormFieldId::Title);
    type_text(&mut app, "Break the epic up");
    let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

    let AppAction::Create { patch, .. } = action else {
        panic!("Ctrl-S files the child");
    };
    assert_eq!(
        patch,
        vec![
            crate::edit::set_field(crate::edit::TITLE_FIELD, "Break the epic up"),
            crate::edit::set_field(crate::edit::ITERATION_PATH_FIELD, "Atlas\\Sprint 3"),
            crate::edit::set_field(crate::edit::AREA_PATH_FIELD, "Atlas\\Platform"),
        ],
        "the two the child inherited travel with it"
    );
}

#[test]
fn the_child_forms_parent_row_names_the_work_item_and_takes_nothing_typed_at_it() {
    let mut app = parent_app("Epic");
    press(&mut app, KeyCode::Char('N'));
    focus_field(&mut app, FormFieldId::Parent);
    type_text(&mut app, "999");
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        AppAction::None,
        "and it opens no picker"
    );
    assert_eq!(app.work_items.mode, WorkItemMode::Form);

    let form = app
        .work_items
        .form
        .as_ref()
        .expect("the child form is open");
    let parent = form
        .field(FormFieldId::Parent)
        .expect("the form has a parent row");
    assert!(parent.read_only, "whoever opened the form filled it in");
    assert!(!parent.is_typed());
    assert!(parent.picker_kind().is_none());
    assert_eq!(parent.value(), "10", "typing left the id where it was");
    assert_eq!(
        parent.shown(),
        "#10 Tech debt and architecture foundation",
        "the row reads as the work item rather than as a number"
    );
    assert_eq!(form.title, "New child of #10");
}

#[test]
fn a_draft_of_the_new_work_item_form_never_opens_in_the_child_form_or_the_other_way() {
    let mut app = parent_app("Epic");
    press(&mut app, KeyCode::Char('n'));
    focus_field(&mut app, FormFieldId::Title);
    type_text(&mut app, "Something loose");
    press(&mut app, KeyCode::Esc);

    press(&mut app, KeyCode::Char('N'));
    let form = app
        .work_items
        .form
        .as_ref()
        .expect("the child form is open");
    assert_eq!(form.kind, FormKind::NewChild(10));
    assert_eq!(
        form.value(FormFieldId::Title),
        "",
        "N opens its own form rather than what n was left holding"
    );

    focus_field(&mut app, FormFieldId::Title);
    type_text(&mut app, "Break the epic up");
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('N'));
    assert_eq!(
        app.work_items
            .form
            .as_ref()
            .unwrap()
            .value(FormFieldId::Title),
        "Break the epic up",
        "the child's own draft does come back"
    );

    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('n'));
    let form = app
        .work_items
        .form
        .as_ref()
        .expect("the new work item form is open");
    assert_eq!(form.kind, FormKind::NewWorkItem);
    assert_eq!(
        form.value(FormFieldId::Title),
        "",
        "and n takes nothing back from the child form"
    );
}

#[test]
fn a_child_filed_from_the_form_hangs_under_its_parent_in_the_family_tree() {
    let mut app = parent_app("Epic");
    let parent = app.work_items.tickets()[0].key.clone();
    press(&mut app, KeyCode::Char('N'));
    focus_field(&mut app, FormFieldId::Title);
    type_text(&mut app, "Break the epic up");
    let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

    let AppAction::Create {
        work_item_type,
        parent: under,
        ..
    } = action
    else {
        panic!("Ctrl-S files the child");
    };
    assert_eq!(work_item_type, "Issue");
    assert_eq!(under, Some(10), "the parent travels as the link it is");

    let child = created(42, "Issue", "Break the epic up");
    let key = child.key.clone();
    app.work_items.apply_created(
        &mut app.shell,
        child,
        vec![RelationRecord {
            from: key.clone(),
            to: parent.clone(),
            kind: RelationKind::Parent,
        }],
    );

    assert_eq!(
        app.work_items.family_of(&parent).children,
        vec![key.clone()],
        "the parent knows its new child at once"
    );
    assert_eq!(
        app.work_items.family_of(&key).ancestors,
        vec![parent.clone()],
        "and the child knows its parent"
    );
    assert_eq!(
        family_ids(&app, &parent),
        [10, 42],
        "the parent's tree shows the child under it"
    );
    assert_eq!(family_ids(&app, &key), [10, 42], "and so does the child's");
}

#[test]
fn the_edit_menus_new_child_row_opens_the_same_form_the_key_does() {
    let mut app = parent_app("Epic");
    press(&mut app, KeyCode::Char('e'));
    for _ in 0..menu_row(&app, CommandId::NewChild) {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);

    assert_eq!(app.work_items.mode, WorkItemMode::Form);
    let form = app
        .work_items
        .form
        .as_ref()
        .expect("the child form is open");
    assert_eq!(form.kind, FormKind::NewChild(10));
    assert_eq!(form.value(FormFieldId::Type), "Issue");
}

/// The work items one family tree draws, in the order it draws them.
fn family_ids(app: &App, key: &TicketKey) -> Vec<i64> {
    app.work_items
        .graph
        .visible_family_tree(key)
        .iter()
        .map(|entry| entry.key.id)
        .collect()
}

/// An app whose one work item is of the given type, sitting somewhere other
/// than the project root, to open the child form over.
fn parent_app(work_item_type: &str) -> App {
    let mut parent = ticket(
        10,
        "Tech debt and architecture foundation",
        "2026-01-01T00:00:00Z",
    );
    parent.work_item_type = work_item_type.to_owned();
    parent.area_path = "Atlas\\Platform".into();
    parent.iteration_path = "Atlas\\Sprint 3".into();
    let mut app = App::new(vec![parent]);
    app.shell.enable_sync();
    app
}

#[test]
fn a_created_work_item_joins_the_rows_with_its_family_and_the_selection_follows_it() {
    let mut app = creating_app();
    let parent = app.work_items.tickets()[0].key.clone();
    press(&mut app, KeyCode::Char('n'));
    app.work_items
        .form
        .as_mut()
        .unwrap()
        .set_value(FormFieldId::Title, "Honour Retry-After");
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

    let child = created(42, "Issue", "Honour Retry-After");
    let key = child.key.clone();
    app.work_items.apply_created(
        &mut app.shell,
        child,
        vec![RelationRecord {
            from: key.clone(),
            to: parent.clone(),
            kind: RelationKind::Parent,
        }],
    );

    assert!(!app.work_items.creates_pending(), "the create has answered");
    assert_eq!(
        app.work_items.tickets().len(),
        2,
        "the new row joined the table"
    );
    assert_eq!(
        app.work_items.selected_ticket().map(|ticket| ticket.key.id),
        Some(42),
        "and the selection moved onto it"
    );
    assert_eq!(
        app.work_items.family_of(&key).ancestors,
        vec![parent.clone()],
        "the child knows its parent"
    );
    assert_eq!(
        app.work_items.family_of(&parent).children,
        vec![key],
        "and the parent knows its child"
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Created Issue #42")
    );
}

#[test]
fn a_created_work_item_the_query_would_hide_clears_it_and_says_so() {
    let mut app = creating_app();
    app.work_items.set_query(&mut app.shell, "type:Task".into());
    assert_eq!(app.work_items.visible_count(), 1);

    app.work_items.apply_created(
        &mut app.shell,
        created(42, "Issue", "Honour Retry-After"),
        Vec::new(),
    );

    assert_eq!(
        app.work_items.query(),
        "",
        "a row nobody could see is worth no filter"
    );
    assert_eq!(app.work_items.visible_count(), 2);
    assert_eq!(
        app.work_items.selected_ticket().map(|ticket| ticket.key.id),
        Some(42)
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Created Issue #42 \u{b7} search cleared so it is visible")
    );
}

#[test]
fn a_created_work_item_the_query_already_admits_leaves_it_alone() {
    let mut app = creating_app();
    app.work_items
        .set_query(&mut app.shell, "type:Issue".into());

    app.work_items.apply_created(
        &mut app.shell,
        created(42, "Issue", "Honour Retry-After"),
        Vec::new(),
    );

    assert_eq!(
        app.work_items.query(),
        "type:Issue",
        "the filter still holds"
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Created Issue #42")
    );
}

#[test]
fn a_refused_create_reopens_the_form_with_everything_still_in_it() {
    let mut app = creating_app();
    press(&mut app, KeyCode::Char('n'));
    focus_field(&mut app, FormFieldId::Title);
    type_text(&mut app, "Honour Retry-After");
    focus_field(&mut app, FormFieldId::Tags);
    type_text(&mut app, "sync");
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(
        app.work_items.form.is_none(),
        "the form is out with the request"
    );

    app.work_items.reject_create(
        &mut app.shell,
        "the work item type Issue is not in this project",
    );

    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Form,
        "the form comes straight back"
    );
    let form = app.work_items.form.as_ref().expect("with the draft in it");
    assert_eq!(form.value(FormFieldId::Title), "Honour Retry-After");
    assert_eq!(form.value(FormFieldId::Tags), "sync");
    assert!(
        !app.work_items.creates_pending(),
        "nothing is in flight any more"
    );
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Work item not created: the work item type Issue is not in this project")
    );
    assert_eq!(
        app.work_items.tickets().len(),
        1,
        "and no row was ever shown for it"
    );
}
