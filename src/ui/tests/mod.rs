//! Tests for the work items screen renderer, split the way the module is.

mod details;
mod overlays;
mod pickers;
mod table;
mod widgets;

use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use ratatui::Terminal;

use ratatui::backend::TestBackend;

use super::*;

use crate::app::FormFieldId;

use crate::model::{
    CommentRecord, HistoryRecord, RelationKind, RelationRecord, StateCatalog, StateOption,
    TicketGraph, TicketKey,
};

use crate::pointer::PointerTarget;

fn ticket() -> Ticket {
    Ticket {
        key: TicketKey {
            organization: "demo".into(),
            id: 10_001,
        },
        project: "atlas".into(),
        revision: 3,
        work_item_type: "Bug".into(),
        title: "Fix ticket search".into(),
        state: "Active".into(),
        reason: Some("Implementation started".into()),
        assigned_to: Some("Avery Chen".into()),
        priority: Some(1),
        area_path: "Atlas\\Platform".into(),
        iteration_path: "Atlas\\Sprint 1".into(),
        tags: vec!["rust".into(), "search".into()],
        description: "A ticket description".into(),
        description_html: String::new(),
        created_at: crate::timestamp::ts("2026-01-01T00:00:00Z"),
        changed_at: crate::timestamp::ts("2026-01-02T00:00:00Z"),
        web_url: "https://dev.azure.com/demo/atlas/_workitems/edit/10001".into(),
        details_rev: 0,
    }
}

fn ticket_at(id: i64, title: &str, work_item_type: &str, state: &str, changed_at: &str) -> Ticket {
    let mut item = ticket();
    item.key.id = id;
    item.title = title.into();
    item.work_item_type = work_item_type.into();
    item.state = state.into();
    item.changed_at = crate::timestamp::ts(changed_at);
    item.web_url = format!("https://dev.azure.com/demo/atlas/_workitems/edit/{id}");
    item
}

fn parent_child_graph() -> TicketGraph {
    let org = |id| TicketKey {
        organization: "demo".into(),
        id,
    };
    TicketGraph {
        relations: vec![
            RelationRecord {
                from: org(10_002),
                to: org(10_001),
                kind: RelationKind::Parent,
            },
            RelationRecord {
                from: org(10_003),
                to: org(10_001),
                kind: RelationKind::Parent,
            },
            RelationRecord {
                from: org(10_002),
                to: org(10_004),
                kind: RelationKind::Child,
            },
            RelationRecord {
                from: org(10_002),
                to: org(10_005),
                kind: RelationKind::Related,
            },
        ],
        ..TicketGraph::default()
    }
}

/// Where the last frame painted a target the predicate names.
fn target_rect(app: &App, predicate: impl Fn(&PointerTarget) -> bool) -> Rect {
    app.hit_regions
        .find_target(predicate)
        .expect("the frame painted that target")
        .rect
}

fn table_body(app: &App) -> Rect {
    target_rect(app, |target| matches!(target, PointerTarget::FocusTickets))
}

fn details_pane(app: &App) -> Rect {
    target_rect(app, |target| matches!(target, PointerTarget::FocusDetails))
}

fn header_rect(app: &App, field: SortField) -> Rect {
    target_rect(
        app,
        |target| matches!(target, PointerTarget::SortHeader(painted) if *painted == field),
    )
}

fn detail_url(app: &App) -> Option<Rect> {
    app.hit_regions
        .find_target(|target| matches!(target, PointerTarget::OpenSelectedUrl))
        .map(|region| region.rect)
}

fn family_row(app: &App, id: i64) -> Option<Rect> {
    app.hit_regions
        .find_target(|target| matches!(target, PointerTarget::JumpToTicket(key) if key.id == id))
        .map(|region| region.rect)
}

fn render_text(width: u16, height: u16, app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..height {
        for x in 0..width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

#[test]
fn layouts_render_both_panes_and_expose_hit_regions_at_every_breakpoint() {
    let mut app = App::new(vec![ticket()]);
    let wide = render_text(130, 30, &mut app);
    assert!(wide.contains("Tickets 1/1"));
    assert!(wide.contains("Details"));
    assert!(wide.contains("Fix ticket search"));
    assert!(wide.contains("Pri"));
    assert!(wide.contains("2026-01-01 00:00:00 UTC"));
    assert!(detail_url(&app).is_some());

    let table = render_text(60, 20, &mut app);
    assert!(table.contains("[Tickets]"));
    assert!(table.contains("1/1"));
    assert!(table.contains("[Details]"));
    assert!(!table.contains("ID / Type / State"));

    app.narrow_details = true;
    let details = render_text(60, 20, &mut app);
    assert!(details.contains("Details"));
    assert!(!details.contains("[Tickets]"));
    assert!(!details.contains("[Details]"));
    assert!(details.contains("Fix ticket search"));
    app.narrow_details = false;

    for width in [36, 69, 70, 109, 110] {
        render_text(width, 16, &mut app);
        assert!(
            app.hit_regions
                .find_target(|target| matches!(target, PointerTarget::SearchField))
                .is_some(),
            "search field missing at width {width}"
        );
        assert!(
            app.hit_regions
                .find_target(|target| matches!(target, PointerTarget::FocusTickets))
                .is_some(),
            "table body missing at width {width}"
        );
        if width >= 70 {
            assert!(
                app.hit_regions
                    .find_target(|target| matches!(target, PointerTarget::FocusDetails))
                    .is_some(),
                "details pane missing at width {width}"
            );
        }
    }
}

#[test]
fn the_table_title_reports_the_sync_state_in_both_layouts() {
    let mut app = App::new(vec![ticket()]);
    assert!(
        !render_text(130, 12, &mut app).contains("Sync"),
        "an offline run says nothing about a sync it cannot run"
    );

    app.enable_sync();
    app.begin_sync();
    for width in [60, 130] {
        assert!(
            render_text(width, 12, &mut app).contains("Syncing…"),
            "the narrow title keeps step at width {width}"
        );
    }

    app.finish_sync();
    assert!(render_text(130, 12, &mut app).contains("Synced just now"));

    app.mark_stale();
    assert!(
        render_text(130, 12, &mut app).contains("Stale"),
        "a database change outranks the last sync time"
    );

    app.fail_sync("network unreachable", true);
    assert!(
        render_text(130, 12, &mut app).contains("Sync failed"),
        "a failing sync outranks a stale database"
    );

    app.reload_pending = true;
    assert!(render_text(130, 12, &mut app).contains("Reloading…"));
    app.begin_sync();
    assert!(
        render_text(130, 12, &mut app).contains("Syncing…"),
        "a pull in flight is the most urgent thing the title can say"
    );
}

#[test]
fn the_database_overlay_reports_the_last_sync() {
    let mut app = App::new(vec![ticket()]);
    app.mode = AppMode::Info;
    assert!(render_text(90, 24, &mut app).contains("offline"));

    app.enable_sync();
    app.finish_sync();
    let synced = render_text(90, 24, &mut app);
    assert!(synced.contains("Sync: just now"), "{synced}");

    app.fail_sync("network unreachable", true);
    assert!(render_text(90, 24, &mut app).contains("failed"));
}

#[test]
fn the_database_overlay_counts_the_finished_rows_the_table_is_leaving_out() {
    let mut app = App::new(vec![
        ticket_at(10_001, "Alpha", "Issue", "To Do", "2026-03-03T00:00:00Z"),
        ticket_at(10_002, "Beta", "Issue", "Done", "2026-03-02T00:00:00Z"),
        ticket_at(10_003, "Gamma", "Issue", "Removed", "2026-03-01T00:00:00Z"),
    ]);
    app.mode = AppMode::Info;

    let hiding = render_text(90, 24, &mut app);
    assert!(hiding.contains("Finished"), "{hiding}");
    assert!(hiding.contains("2 hidden"), "{hiding}");

    app.set_show_finished(true);
    let showing = render_text(90, 24, &mut app);
    assert!(showing.contains("Finished"), "{showing}");
    assert!(showing.contains("shown"), "{showing}");
}

#[test]
fn empty_reloading_and_no_result_states_render_with_a_usable_search_field() {
    let mut app = App::new(Vec::new());
    let empty = render_text(90, 24, &mut app);
    assert!(empty.contains("No tickets in this database"));

    app.reload_pending = true;
    let loading = render_text(90, 24, &mut app);
    assert!(loading.contains("Reloading tickets"));
    app.reload_pending = false;

    app.mode = AppMode::Search;
    app.set_query("a very long query whose visible tail is unique".into());
    let long_search = render_text(40, 12, &mut app);
    assert!(
        long_search.contains("visible tail is unique"),
        "a long query scrolls to keep the cursor end visible"
    );

    let mut searched = App::new(vec![ticket()]);
    searched.set_query("qqqqqqqqqq".into());
    await_search(&mut searched);
    let no_results = render_text(90, 24, &mut searched);
    assert!(no_results.contains("No tickets match this search"));

    searched.mode = AppMode::Sort;
    let sort = render_text(90, 24, &mut searched);
    assert!(sort.contains("Sort tickets"));
    assert!(sort.contains("Priority"));
}

#[test]
fn help_documents_every_bound_command() {
    let mut app = App::new(Vec::new());
    app.mode = AppMode::Help;
    let mut help = String::new();
    for _ in 0..40 {
        help.push_str(&render_text(90, 24, &mut app));
        if app.help.offset >= app.help.max_offset() {
            break;
        }
        app.help.scroll_by(4);
    }
    for command in COMMANDS.iter().filter(|command| !command.keys.is_empty()) {
        assert!(
            help.contains(command.title),
            "help is missing {}",
            command.title
        );
        assert!(
            help.contains(&command.key_label()),
            "help is missing the {} binding",
            command.title
        );
    }
}

fn assert_distinct_and_legible(colors: &[Color]) {
    for (index, color) in colors.iter().enumerate() {
        assert_ne!(*color, theme().muted, "column {index} rendered as muted");
        for other in &colors[index + 1..] {
            assert_ne!(
                color, other,
                "column colours should be distinct: {colors:?}"
            );
        }
    }
}

/// Foreground, background, and modifiers of one rendered buffer cell.
fn painted_cell(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> (Color, Color, Modifier) {
    let cell = &terminal.backend().buffer()[(x, y)];
    (cell.fg, cell.bg, cell.modifier)
}

/// A hovered row tints its background, or reverses where there is no palette.
fn assert_row_hovered(terminal: &Terminal<TestBackend>, x: u16, y: u16, context: &str) {
    let (_, bg, modifier) = painted_cell(terminal, x, y);
    if theme().hover_background == Color::Reset {
        assert!(modifier.contains(Modifier::REVERSED), "{context}");
    } else {
        assert_eq!(bg, theme().hover_background, "{context}");
    }
}

/// Left edge of one table column, shared by the header and the body rows.
fn column_x(app: &App, field: SortField) -> u16 {
    header_rect(app, field).x
}

fn await_search(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.search_pending {
        app.poll_search();
        assert!(Instant::now() < deadline, "search worker timed out");
        thread::yield_now();
    }
}

fn find_buffer_text_in(
    buffer: &ratatui::buffer::Buffer,
    area: Rect,
    needle: &str,
) -> Option<(u16, u16)> {
    let chars: Vec<char> = needle.chars().collect();
    for y in area.y..area.y.saturating_add(area.height) {
        let width = area.width;
        let row: Vec<char> = (0..width)
            .map(|dx| {
                buffer[(area.x + dx, y)]
                    .symbol()
                    .chars()
                    .next()
                    .unwrap_or(' ')
            })
            .collect();
        if let Some(start) = row.windows(chars.len()).position(|window| window == chars) {
            return Some((area.x + u16::try_from(start).unwrap(), y));
        }
    }
    None
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn click(app: &mut App, column: u16, row: u16) -> crate::app::AppAction {
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), column, row));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), column, row))
        .action
}

fn drag(app: &mut App, from: (u16, u16), to: (u16, u16)) -> crate::app::AppAction {
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        from.0,
        from.1,
    ));
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), to.0, to.1));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), to.0, to.1))
        .action
}

fn edit_field_rect(app: &App, field: EditableField) -> Rect {
    app.hit_regions
        .edit_field(field)
        .unwrap_or_else(|| panic!("{field:?} should be clickable"))
}

fn issue_app() -> App {
    let mut app = App::new(vec![ticket_at(
        10_001,
        "Fix ticket search",
        "Issue",
        "To Do",
        "2026-03-03T00:00:00Z",
    )]);
    app.enable_sync();
    let mut catalog = StateCatalog::default();
    catalog.insert(
        "Issue",
        vec![
            StateOption::new("To Do", StateCategory::Proposed),
            StateOption::new("Doing", StateCategory::InProgress),
            StateOption::new("Done", StateCategory::Completed),
        ],
    );
    app.set_state_catalog(catalog);
    app
}
