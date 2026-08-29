use super::*;
use crate::ui::details::{changed_field_line, with_cursor_style};

#[test]
fn the_details_changed_line_says_how_long_a_stale_work_item_has_sat() {
    let item = ticket();

    let quiet = changed_field_line(&item, None);
    assert_eq!(
        quiet.spans.len(),
        2,
        "an item nobody is waiting on gets no suffix"
    );

    let flagged = changed_field_line(&item, Some(21));
    let suffix = flagged.spans.last().expect("a suffix span");
    assert_eq!(suffix.content, " (stale 21d)");
    assert_eq!(suffix.style.fg, Some(theme().warning));
    assert!(
        suffix.style.add_modifier.contains(Modifier::BOLD),
        "the suffix reads under NO_COLOR too"
    );
    assert!(
        flagged.spans[1]
            .content
            .contains(&item.changed_at.exact_utc()),
        "the exact instant is still there to read: {flagged:?}"
    );
}

#[test]
fn the_details_pane_flags_a_neglected_work_item_beside_its_changed_instant() {
    let mut item = ticket();
    item.state = "To Do".into();
    item.changed_at = crate::timestamp::ts("2020-01-01T00:00:00Z");
    let mut app = App::new(vec![item.clone()]);
    app.narrow_details = true;
    app.focus = Focus::Details;

    assert!(
        render_text(60, 44, &mut app).contains("(stale "),
        "the details pane says the work item has been sitting"
    );

    let mut finished = App::new(vec![Ticket {
        state: "Done".into(),
        ..item
    }]);
    finished.narrow_details = true;
    finished.focus = Focus::Details;
    assert!(
        !render_text(60, 44, &mut finished).contains("(stale "),
        "and says nothing about work that is over"
    );
}

#[test]
fn details_render_relationships_history_and_comments() {
    let item = ticket();
    let mut app = App::new(vec![item.clone()]);
    app.narrow_details = true;
    app.focus = Focus::Details;
    app.set_workspace_graph(TicketGraph {
        relations: vec![RelationRecord {
            from: item.key.clone(),
            to: TicketKey {
                organization: "demo".into(),
                id: 99,
            },
            kind: RelationKind::Parent,
        }],
        comments: vec![CommentRecord {
            ticket: item.key.clone(),
            comment_id: 1,
            created_at: crate::timestamp::ts("2026-01-03T00:00:00Z"),
            author: Some("Avery Chen".into()),
            text: "Looks good".into(),
        }],
        history: vec![HistoryRecord {
            ticket: item.key,
            revision: 2,
            changed_at: crate::timestamp::ts("2026-01-02T00:00:00Z"),
            changed_by: Some("Jordan Patel".into()),
            field_name: "State".into(),
            old_value: Some("New".into()),
            new_value: Some("Active".into()),
        }],
    });

    let text = render_text(60, 44, &mut app);
    assert!(text.contains("Family"));
    assert!(text.contains("99"));
    assert!(text.contains("missing ticket"));
    assert!(text.contains("History"));
    assert!(text.contains("Comments"));
    assert!(text.contains("Looks good"));
    assert!(!text.contains("Relationships"));

    let section = |title: &str| text.find(title).unwrap_or_else(|| panic!("{title}"));
    assert!(
        section("Family") < section("Planning"),
        "the family tree opens the sections"
    );
    assert!(
        section("Planning") < section("Description"),
        "Planning comes before Description"
    );
    assert!(
        section("Description") < section("History"),
        "Description comes before History"
    );
    assert!(
        section("History") < section("Comments"),
        "History comes before Comments"
    );
}

#[test]
fn a_comment_just_posted_shows_at_the_head_of_the_discussion() {
    let item = ticket();
    let mut app = App::new(vec![item.clone()]);
    app.narrow_details = true;
    app.focus = Focus::Details;
    app.set_workspace_graph(TicketGraph {
        comments: vec![CommentRecord {
            ticket: item.key.clone(),
            comment_id: 1,
            created_at: crate::timestamp::ts("2026-01-03T00:00:00Z"),
            author: Some("Avery Chen".into()),
            text: "Looks good".into(),
        }],
        ..TicketGraph::default()
    });

    app.apply_comment(CommentRecord {
        ticket: item.key,
        comment_id: 2,
        created_at: crate::timestamp::ts("2026-01-04T00:00:00Z"),
        author: Some("Jacob Ragsdale".into()),
        text: "Merged into main".into(),
    });

    let text = render_text(60, 36, &mut app);
    let earlier = text.find("Looks good").expect("the comment already held");
    let posted = text
        .find("Merged into main")
        .expect("the comment just posted");
    assert!(
        posted < earlier,
        "the new comment reads first, under Comments: {text}"
    );
    assert!(text.contains("Jacob Ragsdale"), "{text}");
}

#[test]
fn the_comment_prompt_opens_empty_and_names_the_work_item() {
    let mut app = App::new(vec![ticket()]);
    app.enable_sync();
    app.set_table_viewport(1);
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    let row = crate::command::EDIT_MENU
        .iter()
        .position(|entry| entry.command == crate::command::CommandId::AddComment)
        .expect("the Edit menu offers a comment row");
    for _ in 0..row {
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.mode, AppMode::Prompt);

    let prompt = render_text(80, 20, &mut app);
    assert!(prompt.contains("Comment on #10001"), "{prompt}");
    assert!(prompt.contains("Comment:"), "{prompt}");
    assert!(prompt.contains("[Save]"), "{prompt}");
    assert!(prompt.contains("[Cancel]"), "{prompt}");
    assert!(
        prompt.contains("Enter post"),
        "the footer explains the prompt: {prompt}"
    );
}

#[test]
fn the_delete_confirmation_names_the_work_item_the_children_and_the_recycle_bin() {
    let mut app = App::new(progress_tickets());
    app.set_workspace_graph(progress_graph());
    app.enable_sync();
    app.set_table_viewport(5);
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    let row = crate::command::EDIT_MENU
        .iter()
        .position(|entry| entry.command == crate::command::CommandId::DeleteWorkItem)
        .expect("the Edit menu offers a delete row");
    for _ in 0..row {
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.mode, AppMode::ConfirmDelete);

    let confirm = render_text(90, 24, &mut app);

    assert!(
        confirm.contains("Delete #10001 Auth rewrite?"),
        "the confirmation is about a work item, not about a row: {confirm}"
    );
    assert!(
        confirm.contains("3 children are not deleted"),
        "an epic over three issues says what happens to them: {confirm}"
    );
    assert!(
        confirm.contains("left with no parent"),
        "and says what that leaves them as: {confirm}"
    );
    assert!(
        confirm.contains("recycle bin"),
        "a soft delete is recoverable, and the overlay says so: {confirm}"
    );
    assert!(confirm.contains("[Delete]"), "{confirm}");
    assert!(confirm.contains("[Cancel]"), "{confirm}");
    assert!(
        confirm.contains("d delete  Esc cancel"),
        "the footer says what confirms it: {confirm}"
    );
    assert!(
        app.hit_regions
            .find_target(|target| matches!(target, PointerTarget::CancelDelete))
            .is_some(),
        "and both buttons are clickable"
    );
}

fn child_of(child: i64, parent: i64) -> RelationRecord {
    let key = |id| TicketKey {
        organization: "demo".into(),
        id,
    };
    RelationRecord {
        from: key(child),
        to: key(parent),
        kind: RelationKind::Parent,
    }
}

/// An Epic over three issues — one closed, one removed, one still open —
/// with a task hanging off the open issue, so the tree carries a parent
/// worth a ratio and a leaf worth none.
fn progress_tickets() -> Vec<Ticket> {
    vec![
        ticket_at(
            10_001,
            "Auth rewrite",
            "Epic",
            "Active",
            "2026-01-05T00:00:00Z",
        ),
        ticket_at(
            10_002,
            "Login form",
            "Issue",
            "Closed",
            "2026-01-04T00:00:00Z",
        ),
        ticket_at(10_003, "Logout", "Issue", "Removed", "2026-01-03T00:00:00Z"),
        ticket_at(
            10_004,
            "Session notes",
            "Issue",
            "Active",
            "2026-01-02T00:00:00Z",
        ),
        ticket_at(
            10_005,
            "Validate email",
            "Task",
            "New",
            "2026-01-01T00:00:00Z",
        ),
    ]
}

fn progress_graph() -> TicketGraph {
    TicketGraph {
        relations: vec![
            child_of(10_002, 10_001),
            child_of(10_003, 10_001),
            child_of(10_004, 10_001),
            child_of(10_005, 10_004),
        ],
        ..TicketGraph::default()
    }
}

fn progress_app() -> App {
    let mut app = App::new(progress_tickets());
    app.set_workspace_graph(progress_graph());
    app
}

fn column_index(app: &App, field: SortField) -> usize {
    app.layout
        .columns
        .iter()
        .position(|column| column.id == field)
        .expect("the layout holds every column")
}

#[test]
fn the_details_header_counts_the_children_and_a_childless_one_says_nothing() {
    let mut app = progress_app();
    assert_eq!(app.selected_ticket().unwrap().key.id, 10_001);

    let epic = render_text(130, 30, &mut app);
    assert!(epic.contains("Children: 2/3 done"), "{epic}");
    assert!(
        epic.contains("▆▆▆▆░░"),
        "the bar is two different glyphs, so it reads under NO_COLOR too: {epic}"
    );

    for _ in 0..4 {
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(app.selected_ticket().unwrap().key.id, 10_005);

    let leaf = render_text(130, 30, &mut app);
    assert!(
        !leaf.contains("Children"),
        "a work item nobody broke down shows no ratio and no bar: {leaf}"
    );
}

#[test]
fn an_epic_whose_children_have_all_finished_fills_its_bar() {
    let mut tickets = progress_tickets();
    tickets[3].state = "Closed".into();
    let mut app = App::new(tickets);
    app.set_workspace_graph(progress_graph());

    let text = render_text(130, 30, &mut app);
    assert!(text.contains("Children: 3/3 done"), "{text}");
    assert!(
        text.contains("▆▆▆▆▆▆"),
        "every child off the board fills the bar: {text}"
    );
}

#[test]
fn the_family_tree_writes_a_parents_ratio_after_its_title_and_leaves_the_rest_bare() {
    let mut app = progress_app();
    app.narrow_details = true;
    app.focus = Focus::Details;

    let text = render_text(60, 30, &mut app);
    assert!(text.contains("Auth rewrite 2/3"), "{text}");
    assert!(text.contains("Session notes 0/1"), "{text}");
    assert!(text.contains("Validate email"), "{text}");
    assert!(
        !text.contains("Validate email 0"),
        "a leaf of the tree trails nothing at all: {text}"
    );
    assert!(
        !text.contains("Login form 0"),
        "a closed issue with no children of its own trails nothing either: {text}"
    );
}

#[test]
fn the_progress_column_is_hidden_until_the_column_overlay_shows_it() {
    let mut app = progress_app();
    // The table on its own, narrowed to the columns under test, so nothing
    // the details pane says can be mistaken for the column's own output.
    for field in [
        SortField::State,
        SortField::Type,
        SortField::Priority,
        SortField::Changed,
        SortField::Assignee,
    ] {
        let index = column_index(&app, field);
        app.layout.toggle_visible(index);
    }
    let progress = column_index(&app, SortField::Progress);
    assert!(
        !app.layout.columns[progress].visible,
        "the column is off until somebody asks for it"
    );

    let hidden = render_text(60, 20, &mut app);
    assert!(!hidden.contains("Progress"), "{hidden}");
    assert!(!hidden.contains("2/3"), "{hidden}");

    app.layout.toggle_visible(progress);
    let shown = render_text(60, 20, &mut app);
    assert!(shown.contains("Progress"), "{shown}");
    assert!(shown.contains("2/3"), "{shown}");
    assert!(shown.contains("0/1"), "{shown}");
    assert!(
        !shown.contains("0/0"),
        "a work item with no children leaves the cell empty: {shown}"
    );

    app.layout.toggle_visible(progress);
    let hidden_again = render_text(60, 20, &mut app);
    assert!(!hidden_again.contains("Progress"), "{hidden_again}");
    assert!(!hidden_again.contains("2/3"), "{hidden_again}");
}

#[test]
fn details_render_family_tree_without_other_links() {
    let mut app = App::new(vec![
        ticket_at(
            10_001,
            "Auth rewrite",
            "Feature",
            "Active",
            "2026-01-01T00:00:00Z",
        ),
        ticket_at(
            10_002,
            "Login form",
            "User Story",
            "Active",
            "2026-02-01T00:00:00Z",
        ),
        ticket_at(
            10_003,
            "Logout",
            "User Story",
            "Closed",
            "2026-01-15T00:00:00Z",
        ),
        ticket_at(
            10_004,
            "Validate email",
            "Task",
            "New",
            "2026-01-20T00:00:00Z",
        ),
        ticket_at(
            10_005,
            "Session notes",
            "Task",
            "Active",
            "2026-01-21T00:00:00Z",
        ),
    ]);
    app.set_workspace_graph(parent_child_graph());
    app.narrow_details = true;
    app.focus = Focus::Details;
    assert_eq!(app.selected_ticket().unwrap().key.id, 10_002);

    let text = render_text(60, 36, &mut app);
    assert!(text.contains("Family: Feature 10001  Auth rewrite › this"));
    assert!(text.contains("0/1 closed"));
    assert!(text.contains("10001"));
    assert!(text.contains("10002"));
    assert!(text.contains("10004"));
    assert!(text.contains("10003"));
    assert!(text.contains("current"));
    assert!(text.contains("├─"));
    assert!(text.contains("└─"));
    assert!(text.contains('✓'), "closed family rows carry a check");
    assert!(text.contains('○'), "open family rows carry a circle");
    assert!(!text.contains("Links"));
    assert!(!text.contains("Related"));
    assert!(!text.contains("10005"));
    assert!(!text.contains("Relationships"));
    assert!(family_row(&app, 10_001).is_some());
}

fn auth_family_app() -> App {
    let mut app = App::new(vec![
        ticket_at(
            10_001,
            "Auth rewrite",
            "Feature",
            "Active",
            "2026-01-01T00:00:00Z",
        ),
        ticket_at(
            10_002,
            "Login form",
            "User Story",
            "Active",
            "2026-02-01T00:00:00Z",
        ),
        ticket_at(
            10_003,
            "Logout",
            "User Story",
            "Closed",
            "2026-01-15T00:00:00Z",
        ),
        ticket_at(
            10_004,
            "Validate email",
            "Task",
            "New",
            "2026-01-20T00:00:00Z",
        ),
        ticket_at(
            10_005,
            "Session notes",
            "Task",
            "Active",
            "2026-01-21T00:00:00Z",
        ),
    ]);
    app.set_workspace_graph(parent_child_graph());
    // These read the details pane row by row, so the chip bar saying
    // finished work is hidden must not sit between it and the top.
    app.set_show_finished(true);
    app.narrow_details = true;
    app.focus = Focus::Family;
    app
}

#[test]
fn family_rows_show_the_current_and_cursor_styles_and_click_through_to_a_ticket() {
    let mut app = auth_family_app();
    app.family_cursor = Some(TicketKey {
        organization: "demo".into(),
        id: 10_001,
    });
    render_text(60, 24, &mut app);
    let current = app
        .hit_regions
        .find_target(
            |target| matches!(target, PointerTarget::JumpToTicket(key) if key.id == 10_002),
        )
        .map(|region| region.rect)
        .expect("current row");
    let cursor = app
        .hit_regions
        .find_target(
            |target| matches!(target, PointerTarget::JumpToTicket(key) if key.id == 10_001),
        )
        .map(|region| region.rect)
        .expect("cursor row");
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let current_bold = (current.x..current.x.saturating_add(current.width))
        .any(|x| buffer[(x, current.y)].modifier.contains(Modifier::BOLD));
    assert!(current_bold, "current family row should be bold");
    let cursor_style = with_cursor_style(Style::default());
    if cursor_style.add_modifier.contains(Modifier::REVERSED) {
        let reversed = (cursor.x.saturating_sub(2)..cursor.x.saturating_add(12))
            .any(|x| buffer[(x, cursor.y)].modifier.contains(Modifier::REVERSED));
        assert!(
            reversed,
            "family cursor should reverse under a reset background"
        );
    } else {
        let highlighted = (cursor.x.saturating_sub(2)..cursor.x.saturating_add(12))
            .any(|x| buffer[(x, cursor.y)].bg == cursor_style.bg.unwrap_or(Color::Reset));
        assert!(
            highlighted,
            "family cursor should use the selected background"
        );
    }

    app.focus = Focus::Details;
    render_text(72, 36, &mut app);
    let details = details_pane(&app);
    let summary_x = details.x.saturating_add(8);
    let summary_y = details.y.saturating_add(3);
    assert!(!matches!(
        app.hit_regions
            .resolve(summary_x, summary_y)
            .map(|region| &region.target),
        Some(PointerTarget::JumpToTicket(_))
    ));
    click(&mut app, summary_x, summary_y);
    assert_eq!(app.selected_ticket().unwrap().key.id, 10_002);
    assert_eq!(app.focus, Focus::Details);

    let row = family_row(&app, 10_001).expect("parent row");
    click(&mut app, row.x + 8, row.y);
    assert_eq!(app.selected_ticket().unwrap().key.id, 10_001);
    assert_eq!(app.focus, Focus::Family);
}

#[test]
fn hovering_tints_a_row_without_recolouring_it_and_still_reverses_controls() {
    let mut app = App::new(vec![
        ticket_at(10_001, "Alpha", "Issue", "To Do", "2026-03-03T00:00:00Z"),
        ticket_at(10_002, "Beta", "Issue", "Doing", "2026-03-02T00:00:00Z"),
    ]);
    let mut terminal = Terminal::new(TestBackend::new(130, 20)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let state_x = column_x(&app, SortField::State);
    let body = table_body(&app);
    let row_y = body.y + 1;
    let (resting_fg, _, resting_modifier) = painted_cell(&terminal, state_x, row_y);

    app.handle_mouse(mouse(MouseEventKind::Moved, state_x, row_y));
    assert_eq!(app.hovered(), Some(&PointerTarget::TableRow { index: 1 }));
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let (hovered_fg, hovered_bg, hovered_modifier) = painted_cell(&terminal, state_x, row_y);

    assert_eq!(
        hovered_fg, resting_fg,
        "hover must not repaint the state colour"
    );
    if theme().hover_background == Color::Reset {
        assert!(hovered_modifier.contains(Modifier::REVERSED));
    } else {
        assert_eq!(hovered_bg, theme().hover_background);
        assert!(
            !hovered_modifier.contains(Modifier::REVERSED),
            "a tinted row must not flip its coloured cells into blocks"
        );
        assert_eq!(
            hovered_modifier, resting_modifier,
            "hover must not touch a row's modifiers"
        );

        // The tint is painted after the selection highlight, so it wins.
        let title_x = column_x(&app, SortField::Title);
        assert_eq!(app.selected_row(), Some(0));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let (_, selected_bg, _) = painted_cell(&terminal, title_x, body.y);
        assert_eq!(selected_bg, theme().selected_background);
        app.handle_mouse(mouse(MouseEventKind::Moved, title_x, body.y));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let (_, hovered_bg, _) = painted_cell(&terminal, title_x, body.y);
        assert_eq!(hovered_bg, theme().hover_background);
        assert_ne!(
            hovered_bg, selected_bg,
            "a hovered selected row must still read differently from a selected one"
        );
    }

    let header = app
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::SortHeader(SortField::Title)))
        .map(|region| region.rect)
        .expect("title sort header");
    app.handle_mouse(mouse(MouseEventKind::Moved, header.x, header.y));
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let (_, _, header_modifier) = painted_cell(&terminal, header.x, header.y);
    assert!(
        header_modifier.contains(Modifier::REVERSED),
        "a hovered sort header should stay a reversed block"
    );

    app.mode = AppMode::Help;
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let close = app
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::CloseOverlay))
        .map(|region| region.rect)
        .expect("overlay close button");
    app.handle_mouse(mouse(MouseEventKind::Moved, close.x, close.y));
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let (_, _, close_modifier) = painted_cell(&terminal, close.x, close.y);
    assert!(
        close_modifier.contains(Modifier::REVERSED),
        "a hovered close button should stay a reversed block"
    );
}

fn auth_family_app_with_long_details() -> App {
    let mut app = auth_family_app();
    let mut tickets = app.tickets().to_vec();
    tickets
        .iter_mut()
        .find(|ticket| ticket.key.id == 10_002)
        .expect("current ticket")
        .description = "line\n".repeat(40);
    let graph = parent_child_graph();
    app.replace_prepared_tickets(crate::app::PreparedTickets::with_graph(tickets, graph));
    app.narrow_details = true;
    app.focus = Focus::Family;
    app
}

#[test]
fn family_hit_targets_follow_the_details_scroll_and_the_wheel_only_scrolls() {
    let mut app = auth_family_app_with_long_details();
    render_text(60, 24, &mut app);
    assert!(app.details.max_offset() > 0);
    let before = app
        .hit_regions
        .find_target(
            |target| matches!(target, PointerTarget::JumpToTicket(key) if key.id == 10_001),
        )
        .map(|region| region.rect.y)
        .expect("parent row should be on screen");
    app.details.scroll_to(app.details.max_offset());
    render_text(60, 24, &mut app);
    let after = app.hit_regions.find_target(
        |target| matches!(target, PointerTarget::JumpToTicket(key) if key.id == 10_001),
    );
    assert!(after.is_none() || after.is_some_and(|region| region.rect.y != before));

    app.details.scroll_to(0);
    render_text(60, 24, &mut app);
    let row = family_row(&app, 10_002).expect("current family row");
    let cursor = app.family_cursor.clone();
    let focus = app.focus;
    app.handle_mouse(mouse(MouseEventKind::ScrollDown, row.x + 8, row.y));
    assert_eq!(app.family_cursor, cursor, "the wheel moves no cursor");
    assert_eq!(app.focus, focus, "the wheel takes no focus");
    assert!(app.details.offset > 0);
}

fn text_at(terminal: &Terminal<TestBackend>, rect: Rect) -> String {
    let buffer = terminal.backend().buffer();
    (rect.x..rect.x.saturating_add(rect.width))
        .map(|x| buffer[(x, rect.y)].symbol())
        .collect()
}

#[test]
fn every_details_field_is_clickable_on_its_own_value() {
    let mut app = App::new(vec![ticket()]);
    let mut terminal = Terminal::new(TestBackend::new(130, 40)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();

    for (field, value) in [
        (EditableField::Title, "Fix ticket search"),
        (EditableField::State, "Active"),
        (EditableField::Assignee, "Avery Chen"),
        (EditableField::Priority, "1"),
        (EditableField::Tags, "[rust] [search]"),
        (EditableField::Area, "Atlas\\Platform"),
        (EditableField::Iteration, "Atlas\\Sprint 1"),
    ] {
        let rect = edit_field_rect(&app, field);
        assert_eq!(
            text_at(&terminal, rect),
            value,
            "{field:?} should cover its own value"
        );
    }

    let assignee = edit_field_rect(&app, EditableField::Assignee);
    let priority = edit_field_rect(&app, EditableField::Priority);
    app.handle_mouse(mouse(MouseEventKind::Moved, assignee.x, assignee.y));
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let (_, _, modifier) = painted_cell(&terminal, assignee.x, assignee.y);
    assert!(
        modifier.contains(Modifier::UNDERLINED),
        "hovering a value underlines it, colours or not"
    );
    let (_, _, elsewhere) = painted_cell(&terminal, priority.x, priority.y);
    assert!(
        !elsewhere.contains(Modifier::UNDERLINED),
        "and only the value under the pointer"
    );
    assert_eq!(
        assignee.y, priority.y,
        "both sit on the Assignee / Priority line"
    );
    assert!(
        assignee.x + assignee.width < priority.x,
        "and each is its own target: {assignee:?} then {priority:?}"
    );

    let mut unassigned = App::new(vec![{
        let mut ticket = ticket();
        ticket.assigned_to = None;
        ticket.priority = None;
        ticket.tags.clear();
        ticket
    }]);
    let mut terminal = Terminal::new(TestBackend::new(130, 40)).unwrap();
    terminal
        .draw(|frame| render(frame, &mut unassigned))
        .unwrap();
    assert_eq!(
        text_at(
            &terminal,
            edit_field_rect(&unassigned, EditableField::Assignee)
        ),
        "Unassigned"
    );
    assert_eq!(
        text_at(
            &terminal,
            edit_field_rect(&unassigned, EditableField::Priority)
        ),
        "\u{2014}"
    );
    assert_eq!(
        text_at(&terminal, edit_field_rect(&unassigned, EditableField::Tags)),
        "\u{2014}"
    );
}

#[test]
fn planning_fields_follow_the_details_scroll_and_a_breadcrumb_shifts_the_rest() {
    let mut app = auth_family_app_with_long_details();
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    // A work item with a family carries a breadcrumb line, so the
    // assignment and tags lines sit one row lower than they otherwise do.
    assert_eq!(
        text_at(&terminal, edit_field_rect(&app, EditableField::Assignee)),
        "Avery Chen"
    );
    let before = edit_field_rect(&app, EditableField::Area);
    assert_eq!(text_at(&terminal, before), "Atlas\\Platform");

    app.details.scroll_to(2);
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let after = edit_field_rect(&app, EditableField::Area);
    assert_eq!(after.y + 2, before.y, "the value scrolled with the pane");
    assert_eq!(text_at(&terminal, after), "Atlas\\Platform");

    app.details.scroll_to(app.details.max_offset());
    render_text(60, 24, &mut app);
    assert!(
        app.hit_regions.edit_field(EditableField::Area).is_none(),
        "a value scrolled off the pane is not clickable"
    );
}

#[test]
fn the_heading_scrolls_away_and_its_fields_travel_with_it() {
    let mut app = auth_family_app_with_long_details();
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let before = edit_field_rect(&app, EditableField::Assignee);
    assert_eq!(text_at(&terminal, before), "Avery Chen");

    app.details.scroll_to(2);
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let after = edit_field_rect(&app, EditableField::Assignee);
    assert_eq!(
        after.y + 2,
        before.y,
        "the heading scrolls with everything under it"
    );
    assert_eq!(text_at(&terminal, after), "Avery Chen");
    click(&mut app, after.x, after.y);
    assert_eq!(
        app.mode,
        AppMode::AssigneePicker,
        "a scrolled value still opens its editor"
    );
    assert_eq!(app.overlay_anchor, OverlayAnchor::Below(after));

    let mut app = auth_family_app_with_long_details();
    render_text(60, 24, &mut app);
    app.details.scroll_to(app.details.max_offset());
    render_text(60, 24, &mut app);
    assert!(
        app.hit_regions.edit_field(EditableField::Title).is_none(),
        "a heading value scrolled off the pane is not clickable"
    );
    assert!(
        app.hit_regions
            .edit_field(EditableField::Assignee)
            .is_none(),
        "and neither is the assignee beside it"
    );
    assert!(
        detail_url(&app).is_none(),
        "the link line scrolls off with the rest of the heading"
    );
}

#[test]
fn the_family_cursor_scrolls_itself_back_into_view_below_the_heading() {
    let mut app = auth_family_app_with_long_details();
    app.focus = Focus::Family;
    render_text(60, 14, &mut app);
    let pane = details_pane(&app);
    let fold = usize::from(pane.height.saturating_sub(2));
    assert_eq!(app.details.offset, 0, "a fresh selection starts at the top");
    assert!(
        app.details_family_row >= fold,
        "the heading fills this pane, so the tree starts below the fold: \
             {} rows down, {fold} visible",
        app.details_family_row
    );

    app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    render_text(60, 14, &mut app);
    assert!(
        app.details.offset > 0,
        "the pane scrolled down to the family cursor"
    );
    assert_cursor_row_visible(&app);

    app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    render_text(60, 14, &mut app);
    assert_cursor_row_visible(&app);
}

fn assert_cursor_row_visible(app: &App) {
    let cursor = app.family_cursor.clone().expect("a family cursor");
    assert!(
        family_row(app, cursor.id).is_some(),
        "the cursor row should be on screen, offset {}",
        app.details.offset
    );
}

#[test]
fn end_scrolls_past_the_description_to_the_last_comment() {
    let item = ticket();
    let mut long = item.clone();
    long.description = "line\n".repeat(60);
    let mut app = App::new(vec![long]);
    app.set_workspace_graph(TicketGraph {
        relations: Vec::new(),
        comments: vec![CommentRecord {
            ticket: item.key,
            comment_id: 1,
            created_at: crate::timestamp::ts("2026-01-03T00:00:00Z"),
            author: Some("Avery Chen".into()),
            text: "The very last word".into(),
        }],
        history: Vec::new(),
    });
    app.narrow_details = true;
    app.focus = Focus::Details;

    let text = render_text(60, 20, &mut app);
    assert!(
        !text.contains("The very last word"),
        "the discussion starts below the fold"
    );
    app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    let text = render_text(60, 20, &mut app);
    assert!(
        text.contains("The very last word"),
        "End reaches the last comment"
    );
}
