use super::*;

#[test]
fn the_new_work_item_form_draws_every_field_and_clicking_one_focuses_it() {
    let mut app = App::new(vec![ticket_at(
        10_001,
        "Fix ticket search",
        "Issue",
        "To Do",
        "2026-03-03T00:00:00Z",
    )]);
    app.shell.enable_sync();

    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    assert_eq!(app.work_items.mode, WorkItemMode::Form);

    let form = render_text(90, 24, &mut app);
    assert!(form.contains("New work item"), "{form}");
    for label in [
        "Type *",
        "Title *",
        "Parent",
        "Iteration",
        "Area",
        "Assignee",
        "Priority",
        "Tags",
    ] {
        assert!(form.contains(label), "{label} is missing: {form}");
    }
    assert!(form.contains("Issue"), "the type defaults to Issue: {form}");
    assert!(
        form.contains("what needs doing"),
        "an empty field says what it is for: {form}"
    );
    assert!(form.contains("[Create]"), "{form}");
    assert!(form.contains("[Cancel]"), "{form}");
    assert!(
        form.contains("Ctrl-S create"),
        "the footer explains the form: {form}"
    );
    // A form wider and taller than the terminal is clipped rather than a
    // panic, the way every other overlay is.
    render_text(34, 9, &mut app);
    render_text(90, 24, &mut app);

    let tags = app
        .work_items
        .form
        .as_ref()
        .and_then(|form| form.index_of(FormFieldId::Tags))
        .expect("the form has a Tags row");
    let (x, y) = app
        .shell
        .hit_regions
        .find_target(
            |target| matches!(target, PointerTarget::FormField { index } if *index == tags),
        )
        .map(|region| (region.rect.x, region.rect.y))
        .expect("every field is clickable");
    click(&mut app, x, y);
    assert_eq!(
        app.work_items.form.as_ref().unwrap().focused().unwrap().id,
        FormFieldId::Tags,
        "clicking a row focuses it"
    );

    let iteration = app
        .work_items
        .form
        .as_ref()
        .and_then(|form| form.index_of(FormFieldId::Iteration))
        .expect("the form has an Iteration row");
    let (x, y) = app
        .shell
        .hit_regions
        .find_target(
            |target| matches!(target, PointerTarget::FormField { index } if *index == iteration),
        )
        .map(|region| (region.rect.x, region.rect.y))
        .expect("the Iteration row is clickable too");
    click(&mut app, x, y);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let picker = render_text(90, 24, &mut app);
    assert!(
        picker.contains("Iteration \u{b7} New work item"),
        "a picker a form opened says which form it is filling in: {picker}"
    );
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Form,
        "escaping the picker comes back to the form, not the table"
    );
    render_text(90, 24, &mut app);

    let (x, y) = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::CancelForm))
        .map(|region| (region.rect.x, region.rect.y))
        .expect("the form offers a Cancel button");
    click(&mut app, x, y);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert!(app.work_items.form.is_none());
}

#[test]
fn the_child_form_names_the_parent_it_is_filing_under_rather_than_its_id() {
    let mut app = App::new(vec![ticket_at(
        595,
        "Tech debt and architecture foundation",
        "Epic",
        "To Do",
        "2026-03-03T00:00:00Z",
    )]);
    app.shell.enable_sync();

    app.handle_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));
    assert_eq!(app.work_items.mode, WorkItemMode::Form);

    let form = render_text(90, 24, &mut app);
    assert!(form.contains("New child of #595"), "{form}");
    assert!(
        form.contains("#595 Tech debt and architecture foundation"),
        "the parent row reads as the work item: {form}"
    );
    assert!(
        form.contains("Issue"),
        "an Epic breaks down into Issues: {form}"
    );
}

#[test]
fn the_title_prompt_renders_a_prefilled_field_with_save_and_cancel() {
    let mut app = App::new(vec![ticket_at(
        10_001,
        "Fix ticket search",
        "Issue",
        "To Do",
        "2026-03-03T00:00:00Z",
    )]);
    app.shell.enable_sync();

    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.work_items.mode, WorkItemMode::Prompt);

    let prompt = render_text(80, 20, &mut app);
    assert!(prompt.contains("Title \u{b7} #10001"), "{prompt}");
    assert!(prompt.contains("Title: Fix ticket search"), "{prompt}");
    assert!(prompt.contains("[Save]"), "{prompt}");
    assert!(prompt.contains("[Cancel]"), "{prompt}");
    assert!(
        prompt.contains("Enter save"),
        "the footer explains the prompt: {prompt}"
    );

    let (x, y) = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::CancelPrompt))
        .map(|region| (region.rect.x, region.rect.y))
        .expect("the prompt should offer a Cancel button");
    click(&mut app, x, y);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert!(app.work_items.prompt.is_none());

    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let priorities = render_text(80, 20, &mut app);
    assert!(
        priorities.contains("Priority \u{b7} #10001"),
        "{priorities}"
    );
    assert!(priorities.contains("Clear"), "{priorities}");
}

/// How many people the picker last painted, counted from the rows a click
/// can land on rather than from the text, which the table shares.
fn clickable_assignees(app: &App) -> usize {
    (0..)
        .take_while(|index| {
            app.shell.hit_regions
                .find_target(|target| {
                    matches!(target, PointerTarget::AssigneeOption { index: at } if at == index)
                })
                .is_some()
        })
        .count()
}

#[test]
fn the_assignee_picker_renders_a_filter_field_over_the_people_it_offers() {
    let mut first = ticket_at(
        10_001,
        "Fix ticket search",
        "Issue",
        "To Do",
        "2026-03-03T00:00:00Z",
    );
    first.assigned_to = Some("Avery Chen".into());
    let mut second = ticket_at(
        10_002,
        "Trim the toolbar",
        "Issue",
        "To Do",
        "2026-02-02T00:00:00Z",
    );
    second.assigned_to = Some("Priya Nair".into());
    let mut app = App::new(vec![first, second]);
    app.shell.enable_sync();
    app.shell.set_me(Some("Jacob Ragsdale".into()));

    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    assert_eq!(app.work_items.mode, WorkItemMode::AssigneePicker);

    let picker = render_text(90, 24, &mut app);
    assert!(picker.contains("Assignee \u{b7} #10001"), "{picker}");
    assert!(picker.contains("Filter people"), "{picker}");
    assert!(picker.contains("Unassigned"), "{picker}");
    assert!(
        picker.contains("Jacob Ragsdale (me)"),
        "the signed-in user is named as such: {picker}"
    );
    assert!(
        picker.contains("Enter assign"),
        "the footer explains the picker: {picker}"
    );
    assert_eq!(
        clickable_assignees(&app),
        4,
        "nobody, me, and the two people the rows name"
    );

    // Typing narrows the list, and the row left is still clickable.
    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
    let filtered = render_text(90, 24, &mut app);
    assert!(filtered.contains("Priya Nair"), "{filtered}");
    assert_eq!(clickable_assignees(&app), 1, "{filtered}");

    let (x, y) = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::AssigneeOption { index: 0 }))
        .map(|region| (region.rect.x, region.rect.y))
        .expect("the person left should be clickable");
    let crate::app::AppAction::Edit(requests) = click(&mut app, x, y) else {
        panic!("clicking somebody else should dispatch an edit");
    };
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert_eq!(requests[0].edit.value_text(), "Priya Nair");
}

#[test]
fn the_parent_picker_renders_the_work_items_that_could_hold_this_one() {
    let mut app = App::new(vec![
        ticket_at(
            10_001,
            "Auth rewrite",
            "Epic",
            "To Do",
            "2026-03-05T00:00:00Z",
        ),
        ticket_at(
            10_002,
            "Login form",
            "Issue",
            "To Do",
            "2026-03-04T00:00:00Z",
        ),
        ticket_at(10_003, "Payments", "Epic", "To Do", "2026-03-03T00:00:00Z"),
        ticket_at(
            10_004,
            "Validate email",
            "Task",
            "To Do",
            "2026-03-02T00:00:00Z",
        ),
    ]);
    app.work_items
        .set_workspace_graph(&mut app.shell, parent_child_graph());
    app.shell.enable_sync();
    app.work_items.set_table_viewport(4);
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(
        app.work_items.selected_ticket().map(|item| item.key.id),
        Some(10_002)
    );

    // The Actions menu's Set parent row, which is the eighth, with Remove
    // parent under it because this work item has a parent.
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    let menu = render_text(90, 24, &mut app);
    assert!(menu.contains("Set parent"), "{menu}");
    assert!(menu.contains("Remove parent"), "{menu}");
    for _ in 0..7 {
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.work_items.mode, WorkItemMode::ParentPicker);

    let picker = render_text(90, 24, &mut app);
    assert!(picker.contains("Parent of #10002"), "{picker}");
    assert!(picker.contains("Filter by id or title"), "{picker}");
    assert!(picker.contains("#10001 Epic Auth rewrite"), "{picker}");
    assert!(picker.contains("#10003 Epic Payments"), "{picker}");
    assert!(
        !picker.contains("#10004"),
        "the task hanging under this work item would make a cycle: {picker}"
    );
    assert!(
        picker.contains("Enter file under"),
        "the footer explains the picker: {picker}"
    );
}

#[test]
fn the_iteration_picker_renders_an_indented_tree_with_dates_and_the_current_sprint() {
    use crate::classification::{ClassificationNode, NodeKind};
    use crate::timestamp::Timestamp;

    let mut item = ticket_at(
        10_001,
        "Fix ticket search",
        "Issue",
        "To Do",
        "2026-03-03T00:00:00Z",
    );
    item.iteration_path = "development\\Q3".into();
    let mut app = App::new(vec![item]);
    app.shell.enable_sync();
    let today = Timestamp::now().calendar_date();
    let day = || Timestamp::parse(&format!("{today}T00:00:00Z")).ok();
    app.work_items.set_classification_nodes(
        vec![
            ClassificationNode::new(NodeKind::Iteration, "development", 0),
            ClassificationNode {
                start_date: day(),
                finish_date: day(),
                ..ClassificationNode::new(NodeKind::Iteration, "development\\Sprint 1", 1)
            },
            ClassificationNode::new(NodeKind::Iteration, "development\\Q3", 1),
            ClassificationNode::new(NodeKind::Iteration, "development\\Q3\\Sprint 7", 2),
        ],
        None,
    );

    // The Actions menu's Iteration row, which is the sixth.
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    for _ in 0..5 {
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.work_items.mode, WorkItemMode::NodePicker);

    let picker = render_text(90, 24, &mut app);
    assert!(picker.contains("Iteration \u{b7} #10001"), "{picker}");
    assert!(picker.contains("Filter iteration"), "{picker}");
    assert!(
        picker.contains("  Sprint 1"),
        "a child is indented under its root: {picker}"
    );
    assert!(
        picker.contains("    Sprint 7"),
        "and a grandchild twice over: {picker}"
    );
    assert!(
        picker.contains("current"),
        "the sprint containing today is marked: {picker}"
    );
    assert!(
        picker.contains(&Timestamp::now().calendar_day()),
        "a scheduled sprint shows the days it runs between: {picker}"
    );
    assert!(
        picker.contains("Enter move"),
        "the footer explains the picker: {picker}"
    );
    assert_eq!(clickable_nodes(&app), 4, "one row a node");

    // Typing narrows the tree, and the row left is still clickable.
    app.handle_key(KeyEvent::new(KeyCode::Char('7'), KeyModifiers::NONE));
    let filtered = render_text(90, 24, &mut app);
    assert!(filtered.contains("Sprint 7"), "{filtered}");
    assert_eq!(clickable_nodes(&app), 1, "{filtered}");

    let (x, y) = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::NodeOption { index: 0 }))
        .map(|region| (region.rect.x, region.rect.y))
        .expect("the node left should be clickable");
    let crate::app::AppAction::Edit(requests) = click(&mut app, x, y) else {
        panic!("clicking another node should dispatch an edit");
    };
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert_eq!(
        requests[0].edit.value_text(),
        "development\\Q3\\Sprint 7",
        "the write carries the full path even though the row showed the leaf"
    );
}

/// How many nodes the picker last painted, counted from the rows a click
/// can land on.
fn clickable_nodes(app: &App) -> usize {
    (0..)
        .take_while(|index| {
            app.shell.hit_regions
                .find_target(|target| {
                    matches!(target, PointerTarget::NodeOption { index: at } if at == index)
                })
                .is_some()
        })
        .count()
}

#[test]
fn the_edit_menu_and_the_state_picker_render_their_rows_and_state_colours() {
    let mut app = App::new(vec![ticket_at(
        10_001,
        "Fix ticket search",
        "Issue",
        "To Do",
        "2026-03-03T00:00:00Z",
    )]);
    app.shell.enable_sync();
    let mut catalog = StateCatalog::default();
    catalog.insert(
        "Issue",
        vec![
            StateOption::new("To Do", StateCategory::Proposed),
            StateOption::new("Doing", StateCategory::InProgress),
            StateOption::new("Done", StateCategory::Completed),
        ],
    );
    app.work_items.set_state_catalog(catalog);

    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    let menu = render_text(80, 20, &mut app);
    assert!(menu.contains("Actions"), "{menu}");
    assert!(menu.contains("State"), "{menu}");
    assert!(menu.contains('S'), "the menu names the key that skips it");

    let (x, y) = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::EditMenuRow { index: 0 }))
        .map(|region| (region.rect.x, region.rect.y))
        .expect("the State row should be clickable");
    assert_eq!(click(&mut app, x, y), crate::app::AppAction::None);
    assert_eq!(app.work_items.mode, WorkItemMode::StatePicker);

    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let rows: Vec<(u16, u16)> = (0..3)
        .map(|index| {
            app.shell.hit_regions
                .find_target(|target| {
                    matches!(target, PointerTarget::StateOption { index: at } if *at == index)
                })
                .map(|region| (region.rect.x, region.rect.y))
                .expect("every state should be clickable")
        })
        .collect();
    // The name starts after the cursor marker and the current-state dot.
    let colours: Vec<(Color, Modifier)> = rows
        .iter()
        .map(|(x, y)| {
            let (fg, _, modifier) = painted_cell(&terminal, x + 3, *y);
            (fg, modifier)
        })
        .collect();
    for (index, (fg, modifier)) in colours.iter().enumerate() {
        assert_eq!(
            *fg,
            state_color(
                [
                    StateCategory::Proposed,
                    StateCategory::InProgress,
                    StateCategory::Completed,
                ][index]
            ),
            "state {index} should carry its category colour"
        );
        assert!(
            modifier.contains(Modifier::BOLD),
            "bold carries the distinction where NO_COLOR leaves no palette"
        );
    }
    if theme() != Theme::mono() {
        assert_distinct_and_legible(&colours.iter().map(|(fg, _)| *fg).collect::<Vec<_>>());
    }

    let picker = render_text(80, 20, &mut app);
    assert!(picker.contains("State \u{b7} #10001"), "{picker}");
    assert!(picker.contains("Doing"), "{picker}");

    // Clicking another state writes it, the same as Enter would.
    let (x, y) = rows[1];
    let action = click(&mut app, x, y);
    let crate::app::AppAction::Edit(requests) = action else {
        panic!("clicking a state should dispatch an edit, got {action:?}");
    };
    assert_eq!(requests[0].edit.summary(), "State \u{2192} Doing");
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
}

#[test]
fn a_picker_over_checked_rows_counts_them_in_its_title() {
    let mut app = App::new(vec![
        ticket_at(
            10_001,
            "Fix ticket search",
            "Issue",
            "To Do",
            "2026-03-03T00:00:00Z",
        ),
        ticket_at(
            10_002,
            "Tidy the sprint",
            "Issue",
            "To Do",
            "2026-03-02T00:00:00Z",
        ),
    ]);
    app.shell.enable_sync();
    let mut catalog = StateCatalog::default();
    catalog.insert(
        "Issue",
        vec![
            StateOption::new("To Do", StateCategory::Proposed),
            StateOption::new("Doing", StateCategory::InProgress),
        ],
    );
    app.work_items.set_state_catalog(catalog);

    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
    let single = render_text(80, 20, &mut app);
    assert!(
        single.contains("State \u{b7} #10001"),
        "one row is named by its id: {single}"
    );
    app.handle_key(key(KeyCode::Esc));

    app.handle_key(key(KeyCode::Char(' ')));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Char(' ')));
    app.handle_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
    let bulk = render_text(80, 20, &mut app);
    assert!(
        bulk.contains("State \u{b7} 2 tickets"),
        "the scope of a bulk change is unmistakable: {bulk}"
    );
}

#[test]
fn clicking_a_field_opens_its_editor_anchored_under_the_value() {
    for (field, mode) in [
        (EditableField::Title, WorkItemMode::Prompt),
        (EditableField::State, WorkItemMode::StatePicker),
        (EditableField::Assignee, WorkItemMode::AssigneePicker),
        (EditableField::Priority, WorkItemMode::PriorityPicker),
        (EditableField::Tags, WorkItemMode::Prompt),
        (EditableField::Area, WorkItemMode::NodePicker),
        (EditableField::Iteration, WorkItemMode::NodePicker),
    ] {
        let mut app = issue_app();
        render_text(130, 40, &mut app);
        let rect = edit_field_rect(&app, field);
        click(&mut app, rect.x, rect.y);
        assert_eq!(app.work_items.mode, mode, "clicking {field:?}");
        assert_eq!(
            app.shell.overlay_anchor,
            OverlayAnchor::Below(rect),
            "{field:?} anchors its editor to its own value"
        );
    }
}

#[test]
fn an_anchored_dropdown_is_drawn_under_the_field_and_dismissed_by_a_click_away() {
    let mut app = issue_app();
    render_text(130, 40, &mut app);
    let field = edit_field_rect(&app, EditableField::Assignee);
    assert!(matches!(
        click(&mut app, field.x, field.y),
        crate::app::AppAction::FetchIdentities | crate::app::AppAction::None
    ));
    assert_eq!(app.work_items.mode, WorkItemMode::AssigneePicker);

    let mut terminal = Terminal::new(TestBackend::new(130, 40)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(
        buffer[(field.x, field.y + 1)].symbol(),
        "\u{250c}",
        "the dropdown's corner sits under the value"
    );
    let first = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::AssigneeOption { index: 0 }))
        .expect("the first candidate should be clickable")
        .rect;
    assert!(
        first.y > field.y && first.x >= field.x,
        "the candidates hang below the field: {first:?} under {field:?}"
    );

    // A field near the right edge keeps its dropdown on screen.
    app.work_items.mode = WorkItemMode::Browse;
    render_text(130, 40, &mut app);
    let state = edit_field_rect(&app, EditableField::State);
    click(&mut app, state.x, state.y);
    let mut terminal = Terminal::new(TestBackend::new(130, 40)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let corner = find_buffer_text_in(
        terminal.backend().buffer(),
        Rect::new(0, state.y + 1, 130, 1),
        "\u{250c}",
    )
    .expect("the state dropdown is drawn on the row under the value");
    assert!(
        corner.0 < state.x,
        "and pulled left to stay on screen: {corner:?} for {state:?}"
    );

    // Everything outside the dropdown closes it and reaches nothing else.
    let action = click(&mut app, 2, 1);
    assert_eq!(action, crate::app::AppAction::None);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    assert!(
        app.work_items.query().is_empty(),
        "the click never reached the search"
    );
    assert!(
        app.work_items.tickets()[0].state == "To Do",
        "and wrote nothing"
    );
}

#[test]
fn a_drag_across_a_field_copies_its_text_and_opens_no_editor() {
    let mut app = App::new(vec![ticket()]);
    render_text(130, 40, &mut app);
    let field = edit_field_rect(&app, EditableField::Assignee);
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        field.x,
        field.y,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        field.x + 4,
        field.y,
    ));
    let action = app
        .handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            field.x + 4,
            field.y,
        ))
        .action;
    assert!(
        matches!(action, crate::app::AppAction::Copy { .. }),
        "a drag still selects text, got {action:?}"
    );
    assert_eq!(
        app.work_items.mode,
        WorkItemMode::Browse,
        "and opens nothing"
    );
}

#[test]
fn enter_opens_the_field_under_the_pointer_and_still_opens_the_link() {
    let mut app = App::new(vec![ticket()]);
    render_text(130, 40, &mut app);
    let field = edit_field_rect(&app, EditableField::Priority);
    app.shell.focus = Focus::Details;
    app.handle_mouse(mouse(MouseEventKind::Moved, field.x, field.y));
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(action, crate::app::AppAction::None);
    assert_eq!(app.work_items.mode, WorkItemMode::PriorityPicker);
    assert_eq!(app.shell.overlay_anchor, OverlayAnchor::Below(field));

    let mut app = App::new(vec![ticket()]);
    render_text(130, 40, &mut app);
    let url = detail_url(&app).expect("detail url");
    app.shell.focus = Focus::Details;
    app.handle_mouse(mouse(MouseEventKind::Moved, url.x, url.y));
    assert!(matches!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        crate::app::AppAction::OpenUrl(_)
    ));
}

#[test]
fn an_anchored_picker_writes_the_same_edit_as_the_keyboard_one() {
    let mut keyboard = issue_app();
    keyboard.handle_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::NONE));
    assert_eq!(keyboard.shell.overlay_anchor, OverlayAnchor::Centered);
    keyboard.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let expected = keyboard.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(expected, crate::app::AppAction::Edit(_)),
        "the keyboard path writes an edit, got {expected:?}"
    );

    let mut clicked = issue_app();
    render_text(130, 40, &mut clicked);
    let field = edit_field_rect(&clicked, EditableField::State);
    click(&mut clicked, field.x, field.y);
    render_text(130, 40, &mut clicked);
    clicked.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let action = clicked.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(action, expected, "both paths produce the same edit");
    assert_eq!(clicked.work_items.mode, WorkItemMode::Browse);
}

#[test]
fn a_dropdown_opens_below_a_field_above_a_low_one_and_centred_when_neither_fits() {
    let screen = Rect::new(0, 0, 80, 24);
    let field = Rect::new(10, 4, 12, 1);
    assert_eq!(
        overlay_area(screen, OverlayAnchor::Below(field), 30, 8),
        Rect::new(10, 5, 30, 8),
        "a tall pane drops the list under the field"
    );

    let low = Rect::new(10, 22, 12, 1);
    assert_eq!(
        overlay_area(screen, OverlayAnchor::Below(low), 30, 8),
        Rect::new(10, 14, 30, 8),
        "a field near the bottom opens above itself"
    );

    let short = Rect::new(0, 0, 40, 5);
    let middle = Rect::new(4, 2, 8, 1);
    assert_eq!(
        overlay_area(short, OverlayAnchor::Below(middle), 30, 8),
        centered_rect(short, 30, 8),
        "with room neither way the picker goes back to the middle"
    );

    let right = Rect::new(70, 4, 8, 1);
    assert_eq!(
        overlay_area(screen, OverlayAnchor::Below(right), 30, 8).x,
        50,
        "a dropdown is pulled back inside the screen"
    );
    assert_eq!(
        overlay_area(screen, OverlayAnchor::Above(field), 30, 8),
        Rect::new(10, 0, 30, 4),
        "an upward anchor takes the rows it has"
    );
    assert_eq!(
        overlay_area(screen, OverlayAnchor::Centered, 30, 8),
        centered_rect(screen, 30, 8),
        "a keyboard-opened picker stays centred"
    );

    let rows = [
        Line::from("a short row"),
        Line::from("the longest row here"),
    ];
    assert_eq!(
        overlay_width(OverlayAnchor::Centered, &rows, 52, screen),
        52
    );
    assert_eq!(
        overlay_width(OverlayAnchor::Below(field), &rows, 52, screen),
        24,
        "a narrow list still opens at the minimum width"
    );
    let wide = [Line::from("x".repeat(40))];
    assert_eq!(
        overlay_width(OverlayAnchor::Below(field), &wide, 52, screen),
        44
    );
}
