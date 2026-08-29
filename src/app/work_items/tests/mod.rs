//! Tests for the work items screen, split the way the module is.

/// The states a Basic-process Task moves through, as a sync would have
/// cached them.
fn task_states() -> Vec<StateOption> {
    vec![
        StateOption::new("To Do", StateCategory::Proposed),
        StateOption::new("Doing", StateCategory::InProgress),
        StateOption::new("Done", StateCategory::Completed),
    ]
}

mod context;
mod deletes;
mod edits;
mod family;
mod forms;
mod pickers;
mod pointer;
mod query;
mod views;

use std::thread;

use std::time::{Duration, Instant};

use super::*;

use crate::model::StateCategory;

use crate::session;

fn ticket(id: i64, title: &str, changed_at: &str) -> Ticket {
    Ticket {
        key: TicketKey {
            organization: "demo".into(),
            id,
        },
        project: "atlas".into(),
        revision: 1,
        work_item_type: "Task".into(),
        title: title.into(),
        state: "Active".into(),
        reason: None,
        assigned_to: Some("Avery".into()),
        priority: Some(2),
        area_path: "Atlas".into(),
        iteration_path: "Atlas\\Sprint 1".into(),
        tags: vec![],
        description: String::new(),
        description_html: String::new(),
        created_at: crate::timestamp::ts("2026-01-01T00:00:00Z"),
        changed_at: crate::timestamp::ts(changed_at),
        web_url: format!("https://dev.azure.com/demo/atlas/_workitems/edit/{id}"),
        details_rev: 0,
    }
}

fn await_search(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.work_items.search_pending {
        app.work_items.poll_search(&mut app.shell);
        assert!(Instant::now() < deadline, "search worker timed out");
        thread::yield_now();
    }
}

/// Types one line into the focused form field.
fn type_text(app: &mut App, text: &str) {
    for character in text.chars() {
        press(app, KeyCode::Char(character));
    }
}

/// Moves the form cursor onto one field by name.
fn focus_field(app: &mut App, id: FormFieldId) {
    let index = app
        .work_items
        .form
        .as_ref()
        .and_then(|form| form.index_of(id))
        .expect("the form has that field");
    app.work_items.focus_form_field(index);
}

/// An app that can write, with one work item to hang new work under.
fn creating_app() -> App {
    let mut app = App::new(vec![ticket(10, "Sync timer", "2026-01-01T00:00:00Z")]);
    app.shell.enable_sync();
    app
}

/// A work item as Azure DevOps hands one back from a create: an id, a
/// revision, and a URL only the server could have given it.
fn created(id: i64, work_item_type: &str, title: &str) -> Ticket {
    Ticket {
        work_item_type: work_item_type.into(),
        title: title.into(),
        state: "To Do".into(),
        assigned_to: None,
        priority: None,
        ..ticket(id, title, "2026-08-29T12:00:00Z")
    }
}

/// Where a view sits in the overlay, which is not its position among the
/// user's own views: the built-ins and the headings are counted too.
fn view_row(app: &App, name: &str) -> usize {
    app.work_items
        .view_rows()
        .iter()
        .position(|row| !row.is_heading() && row.label == name)
        .unwrap_or_else(|| panic!("no view named {name}"))
}

#[test]
fn bookmarks_multi_select_and_copy_use_selected_tickets() {
    let mut app = App::new(vec![
        ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Beta", "2026-02-01T00:00:00Z"),
    ]);
    app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
    assert!(
        app.work_items
            .is_bookmarked(&app.work_items.selected_ticket().unwrap().key)
    );

    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    app.work_items.select_row(&mut app.shell, 1);
    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    let action = app
        .work_items
        .copy_with(CopiedContent::Id, export::copy_ids);
    assert_eq!(
        action,
        AppAction::Copy {
            text: "1\n2\n".into(),
            content: CopiedContent::Id,
        }
    );
}

fn family_key(id: i64) -> TicketKey {
    TicketKey {
        organization: "demo".into(),
        id,
    }
}

fn press(app: &mut App, code: KeyCode) -> AppAction {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn child_of(child: i64, parent: i64) -> RelationRecord {
    RelationRecord {
        from: family_key(child),
        to: family_key(parent),
        kind: RelationKind::Parent,
    }
}

/// An Epic over three issues — one closed, one removed, one still open —
/// with a task hanging off the open issue.
fn epic_tickets() -> Vec<Ticket> {
    let mut epic = ticket(1, "Auth rewrite", "2026-01-05T00:00:00Z");
    epic.work_item_type = "Epic".into();
    let mut closed = ticket(2, "Login form", "2026-01-04T00:00:00Z");
    closed.state = "Closed".into();
    let mut removed = ticket(3, "Logout", "2026-01-03T00:00:00Z");
    removed.state = "Removed".into();
    let open = ticket(4, "Session notes", "2026-01-02T00:00:00Z");
    let mut task = ticket(5, "Validate email", "2026-01-01T00:00:00Z");
    task.state = "New".into();
    vec![epic, closed, removed, open, task]
}

fn epic_graph() -> TicketGraph {
    TicketGraph {
        relations: vec![
            child_of(2, 1),
            child_of(3, 1),
            child_of(4, 1),
            child_of(5, 4),
        ],
        ..TicketGraph::default()
    }
}

/// Two epics, two issues under the first of them, and a task under one of
/// those issues: enough family to move a work item out of one epic and into
/// another, and enough depth to have a descendant the picker must hide.
fn reparent_app() -> App {
    let mut epic = ticket(1, "Auth rewrite", "2026-01-05T00:00:00Z");
    epic.work_item_type = "Epic".into();
    let mut other = ticket(2, "Payments", "2026-01-04T00:00:00Z");
    other.work_item_type = "Epic".into();
    let mut issue = ticket(3, "Login form", "2026-01-03T00:00:00Z");
    issue.work_item_type = "Issue".into();
    let mut closed = ticket(4, "Logout", "2026-01-02T00:00:00Z");
    closed.work_item_type = "Issue".into();
    closed.state = "Closed".into();
    let task = ticket(5, "Validate email", "2026-01-01T00:00:00Z");
    let mut app = App::new(vec![epic, other, issue, closed, task]);
    app.work_items.set_workspace_graph(
        &mut app.shell,
        TicketGraph {
            relations: vec![
                child_of(3, 1),
                child_of(4, 1),
                child_of(5, 3),
                RelationRecord {
                    from: family_key(1),
                    to: family_key(3),
                    kind: RelationKind::Child,
                },
            ],
            ..TicketGraph::default()
        },
    );
    app.shell.enable_sync();
    app.work_items.set_table_viewport(5);
    app.work_items
        .jump_to_ticket(&mut app.shell, &family_key(3));
    app
}

fn candidate_ids(candidates: &[ParentCandidate]) -> Vec<i64> {
    candidates
        .iter()
        .map(|candidate| candidate.key.id)
        .collect()
}

fn menu_labels(app: &App) -> Vec<&'static str> {
    app.work_items
        .edit_menu_entries()
        .into_iter()
        .map(|entry| entry.label)
        .collect()
}

fn progress_of(app: &App, id: i64) -> Option<(usize, usize)> {
    app.work_items
        .child_progress(&family_key(id))
        .map(|progress| (progress.done, progress.total))
}

/// Three work items over a configured Azure DevOps project, which is what
/// an edit needs to go anywhere.
fn editing_app() -> App {
    let mut app = App::new(vec![
        ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Beta", "2026-02-01T00:00:00Z"),
        ticket(3, "Gamma", "2026-03-01T00:00:00Z"),
    ]);
    app.shell.enable_sync();
    app.work_items.set_table_viewport(3);
    app
}

fn edit_request(app: &mut App, edit: FieldEdit) -> EditRequest {
    match app.work_items.edit_selected(&mut app.shell, edit) {
        AppAction::Edit(requests) => only(requests),
        other => panic!("expected an edit to be dispatched, got {other:?}"),
    }
}

/// The one request an edit of a single work item dispatches.
fn only(requests: Vec<EditRequest>) -> EditRequest {
    assert_eq!(requests.len(), 1, "one work item, one request");
    requests.into_iter().next().expect("the request is there")
}

/// Checks every row the app holds, which is what turns the pickers into
/// bulk changes.
fn check_all(app: &mut App) {
    for key in app
        .work_items
        .tickets()
        .iter()
        .map(|ticket| ticket.key.clone())
        .collect::<Vec<_>>()
    {
        app.work_items.selected_keys.insert(key);
    }
}

/// The work item as Azure DevOps hands it back: the field written, and the
/// revision and changed date it decided on.
fn stored_copy(app: &App, key: &TicketKey, state: &str) -> Ticket {
    let mut ticket = app
        .work_items
        .ticket_by_key(key)
        .expect("the row is loaded")
        .clone();
    ticket.state = state.to_owned();
    ticket.revision += 1;
    ticket.changed_at = crate::timestamp::ts("2026-04-01T00:00:00Z");
    ticket
}

/// One press of `u`, and the requests it dispatched.
fn undo(app: &mut App) -> Vec<EditRequest> {
    match press(app, KeyCode::Char('u')) {
        AppAction::Edit(requests) => requests,
        other => panic!("an undo should be dispatched like any other edit, got {other:?}"),
    }
}

/// An editable app whose rows are all in the first state, with the states
/// their type allows already cached.
fn picker_app() -> App {
    let mut tickets = vec![
        ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Beta", "2026-02-01T00:00:00Z"),
        ticket(3, "Gamma", "2026-03-01T00:00:00Z"),
    ];
    for ticket in &mut tickets {
        ticket.state = "To Do".into();
    }
    let mut app = App::new(tickets);
    app.shell.enable_sync();
    app.work_items.set_table_viewport(3);
    let mut catalog = StateCatalog::default();
    catalog.insert("Task", task_states());
    app.work_items.set_state_catalog(catalog);
    app
}

fn state_names(options: &[StateOption]) -> Vec<&str> {
    options.iter().map(|option| option.name.as_str()).collect()
}

fn shift(app: &mut App, ch: char) -> AppAction {
    app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::SHIFT))
}

/// The work item as Azure DevOps hands it back after a bulk change: the
/// state written, on the revision it settled on.
fn accept(app: &mut App, request: &EditRequest) {
    let ticket = stored_copy(app, &request.key, request.edit.value_text().as_str());
    app.work_items.apply_edit(
        &mut app.shell,
        EditApplied {
            ticket,
            relations: Vec::new(),
            edit: request.edit.clone(),
        },
    );
}

/// An editable app whose selected row — the most recently changed one — has
/// a priority and a tag to open the field editors on.
fn edit_app() -> App {
    let mut gamma = ticket(3, "Gamma", "2026-03-01T00:00:00Z");
    gamma.priority = Some(1);
    gamma.tags = vec!["rust".into()];
    let mut app = App::new(vec![
        ticket(1, "Alpha", "2026-01-01T00:00:00Z"),
        ticket(2, "Beta", "2026-02-01T00:00:00Z"),
        gamma,
    ]);
    app.shell.enable_sync();
    app.work_items.set_table_viewport(3);
    app
}

/// Opens the Edit menu and runs the row at `index`, the way a hand does.
fn open_editor(app: &mut App, index: usize) {
    press(app, KeyCode::Char('e'));
    for _ in 0..index {
        press(app, KeyCode::Down);
    }
    press(app, KeyCode::Enter);
}

/// The Edit menu row for one command, found by the command itself so a new
/// field editor above it moves nothing here.
/// Where a row sits in the Edit menu *as this app draws it*: the menu is
/// dynamic — `Remove parent` only appears when the selection has one — so a
/// position taken from the static table walks to the wrong row.
fn menu_row(app: &App, command: CommandId) -> usize {
    app.work_items
        .edit_menu_entries()
        .iter()
        .position(|entry| entry.command == command)
        .expect("the Edit menu offers the row")
}

fn type_query(app: &mut App, text: &str) {
    for character in text.chars() {
        press(app, KeyCode::Char(character));
    }
}

/// The two trees a project with a nested quarter has, as a fetch flattens
/// them. Sprint 1 is the one running today, whenever today is.
fn classification_trees() -> Vec<ClassificationNode> {
    let today = Timestamp::now().calendar_date();
    let day = || Timestamp::parse(&format!("{today}T00:00:00Z")).ok();
    vec![
        ClassificationNode::new(NodeKind::Area, "development", 0),
        ClassificationNode::new(NodeKind::Area, "development\\Platform", 1),
        ClassificationNode::new(NodeKind::Iteration, "development", 0),
        ClassificationNode {
            start_date: day(),
            finish_date: day(),
            ..ClassificationNode::new(NodeKind::Iteration, "development\\Sprint 1", 1)
        },
        ClassificationNode::new(NodeKind::Iteration, "development\\Q3", 1),
        ClassificationNode::new(NodeKind::Iteration, "development\\Q3\\Sprint 7", 2),
    ]
}
