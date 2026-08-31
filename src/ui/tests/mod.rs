//! Tests for the work items screen renderer, split the way the module is.

mod details;
mod overlays;
mod panes;
mod pickers;
mod pipelines;
mod pull_requests;
mod repos;
mod table;
mod tabs;
mod widgets;

use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use ratatui::Terminal;

use ratatui::backend::TestBackend;

use super::*;
use crate::columns::ColumnId;

use crate::app::FormFieldId;

use crate::model::{
    CommentRecord, HistoryRecord, RelationKind, RelationRecord, StateCatalog, StateOption,
    TicketGraph, TicketKey,
};

use crate::command::CommandId;

use crate::pointer::PointerTarget;

fn ticket() -> Ticket {
    Ticket {
        revision: 3,
        work_item_type: "Bug".into(),
        reason: Some("Implementation started".into()),
        assigned_to: Some("Avery Chen".into()),
        priority: Some(1),
        area_path: "Atlas\\Platform".into(),
        tags: vec!["rust".into(), "search".into()],
        description: "A ticket description".into(),
        changed_at: crate::timestamp::ts("2026-01-02T00:00:00Z"),
        ..Ticket::fixture(10_001, "Fix ticket search")
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
    app.shell
        .hit_regions
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
        |target| matches!(target, PointerTarget::SortHeader(painted) if *painted == field.key()),
    )
}

fn detail_url(app: &App) -> Option<Rect> {
    app.shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::OpenSelectedUrl))
        .map(|region| region.rect)
}

fn family_row(app: &App, id: i64) -> Option<Rect> {
    app.shell
        .hit_regions
        .find_target(
            |target| matches!(target, PointerTarget::Follow(Jump::WorkItem(key)) if key.id == id),
        )
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

/// The rows inside the modal frame on screen, located from the close button
/// every frame paints on its top-right corner. The last interior column is dropped
/// with them: that one is the overlay's own scrollbar track, which draws the
/// same `\u{2502}` the panes behind it do.
fn modal_interior(text: &str) -> Option<Vec<String>> {
    // Whichever corners the theme draws with.
    let corners = ratatui::widgets::BorderType::border_symbols(theme().border_type);
    let corner = |glyph: &str| glyph.chars().next().unwrap_or(' ');
    let (top_left, top_right, bottom_left) = (
        corner(corners.top_left),
        corner(corners.top_right),
        corner(corners.bottom_left),
    );
    let rows: Vec<Vec<char>> = text.lines().map(|line| line.chars().collect()).collect();
    // The row with a corner on it and the close button after it: the chip bar
    // has an `×` of its own, and it is not a modal.
    let (top, close) = rows.iter().enumerate().find_map(|(y, row)| {
        let line: String = row.iter().collect();
        let corner = line.find(top_left)?;
        let byte = corner + line[corner..].find(crate::ui::widgets::CLOSE_LABEL)?;
        Some((y, line[..byte].chars().count()))
    })?;
    let left = rows[top][..close]
        .iter()
        .rposition(|glyph| *glyph == top_left)?;
    let right = close
        + rows[top][close..]
            .iter()
            .position(|glyph| *glyph == top_right)?;
    let bottom = (top + 1..rows.len()).find(|y| rows[*y].get(left) == Some(&bottom_left))?;
    let (from, to) = (left + 1, right.saturating_sub(1));
    (from < to).then(|| {
        rows[top + 1..bottom]
            .iter()
            .map(|row| row[from..to.min(row.len())].iter().collect())
            .collect()
    })
}

/// Whether a pane's frame says both what it is and what it holds: the name is
/// on the top border and the count on the bottom, unless the pane is stacked
/// over another, where the two share the top border row.
fn pane_reads(text: &str, name: &str, status: &str) -> bool {
    text.contains(name)
        && text
            .lines()
            .any(|line| line.contains(status) && line.contains('─'))
}

#[test]
fn layouts_render_both_panes_and_expose_hit_regions_at_every_breakpoint() {
    let mut app = App::new(vec![ticket()]);
    let wide = render_text(130, 30, &mut app);
    assert!(pane_reads(&wide, "Tickets", "1/1"), "{wide}");
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

    // Whichever pane is showing wears the switcher, so the way back to the
    // other one is on screen in both directions.
    app.shell.narrow_details = true;
    let details = render_text(60, 20, &mut app);
    assert!(details.contains("[Tickets]"));
    assert!(details.contains("[Details]"));
    assert!(details.contains("Fix ticket search"));
    app.shell.narrow_details = false;

    for width in [36, 69, 70, 109, 110] {
        render_text(width, 16, &mut app);
        assert!(
            app.shell
                .hit_regions
                .find_target(|target| matches!(target, PointerTarget::SearchField))
                .is_some(),
            "search field missing at width {width}"
        );
        assert!(
            app.shell
                .hit_regions
                .find_target(|target| matches!(target, PointerTarget::FocusTickets))
                .is_some(),
            "table body missing at width {width}"
        );
        if width >= 70 {
            assert!(
                app.shell
                    .hit_regions
                    .find_target(|target| matches!(target, PointerTarget::FocusDetails))
                    .is_some(),
                "details pane missing at width {width}"
            );
        }
    }
}

#[test]
fn the_status_bar_reports_the_sync_state_at_every_width() {
    let mut app = App::new(vec![ticket()]);
    assert!(
        !render_text(130, 12, &mut app).contains("Sync"),
        "an offline run says nothing about a sync it cannot run"
    );

    app.shell.enable_sync();
    app.shell.begin_sync();
    for width in [60, 130] {
        assert!(
            render_text(width, 12, &mut app).contains("Syncing…"),
            "the narrow bar keeps step at width {width}"
        );
    }

    app.shell.finish_sync();
    assert!(render_text(130, 12, &mut app).contains("Synced just now"));

    app.shell.mark_stale();
    assert!(
        render_text(130, 12, &mut app).contains("Stale"),
        "a database change outranks the last sync time"
    );

    app.shell.fail_sync("network unreachable", true);
    assert!(
        render_text(130, 12, &mut app).contains("Sync failed"),
        "a failing sync outranks a stale database"
    );

    app.shell.reload_pending = true;
    assert!(render_text(130, 12, &mut app).contains("Reloading…"));
    app.shell.begin_sync();
    assert!(
        render_text(130, 12, &mut app).contains("Syncing…"),
        "a pull in flight is the most urgent thing the bar can say"
    );
}

#[test]
fn the_database_overlay_reports_the_last_sync() {
    let mut app = App::new(vec![ticket()]);
    app.work_items.mode = WorkItemMode::Info;
    assert!(render_text(90, 24, &mut app).contains("offline"));

    app.shell.enable_sync();
    app.shell.finish_sync();
    let synced = render_text(90, 24, &mut app);
    assert!(synced.contains("Sync       just now"), "{synced}");

    app.shell.fail_sync("network unreachable", true);
    assert!(render_text(90, 24, &mut app).contains("failed"));
}

#[test]
fn the_database_overlay_counts_the_finished_rows_the_table_is_leaving_out() {
    let mut app = App::new(vec![
        ticket_at(10_001, "Alpha", "Issue", "To Do", "2026-03-03T00:00:00Z"),
        ticket_at(10_002, "Beta", "Issue", "Done", "2026-03-02T00:00:00Z"),
        ticket_at(10_003, "Gamma", "Issue", "Removed", "2026-03-01T00:00:00Z"),
    ]);
    app.work_items.mode = WorkItemMode::Info;

    let hiding = render_text(90, 24, &mut app);
    assert!(hiding.contains("Finished"), "{hiding}");
    assert!(hiding.contains("2 hidden"), "{hiding}");

    app.work_items.set_show_finished(&mut app.shell, true);
    let showing = render_text(90, 24, &mut app);
    assert!(showing.contains("Finished"), "{showing}");
    assert!(showing.contains("shown"), "{showing}");
}

#[test]
fn empty_reloading_and_no_result_states_render_with_a_usable_search_field() {
    let mut app = App::new(Vec::new());
    let empty = render_text(90, 24, &mut app);
    assert!(empty.contains("No tickets in this database"));

    app.shell.reload_pending = true;
    let loading = render_text(90, 24, &mut app);
    assert!(loading.contains("Reloading tickets"));
    app.shell.reload_pending = false;

    app.work_items.mode = WorkItemMode::Search;
    app.work_items.set_query(
        &mut app.shell,
        "a very long query whose visible tail is unique".into(),
    );
    let long_search = render_text(40, 12, &mut app);
    assert!(
        long_search.contains("visible tail is unique"),
        "a long query scrolls to keep the cursor end visible"
    );

    let mut searched = App::new(vec![ticket()]);
    searched
        .work_items
        .set_query(&mut searched.shell, "qqqqqqqqqq".into());
    await_search(&mut searched);
    let no_results = render_text(90, 24, &mut searched);
    assert!(no_results.contains("No tickets match this search"));

    searched.work_items.mode = WorkItemMode::Sort;
    let sort = render_text(90, 24, &mut searched);
    assert!(sort.contains("Sort tickets"));
    assert!(sort.contains("Priority"));
}

#[test]
fn help_documents_every_bound_command() {
    let mut app = App::new(Vec::new());
    app.work_items.mode = WorkItemMode::Help;
    let mut help = String::new();
    for _ in 0..40 {
        help.push_str(&render_text(90, 24, &mut app));
        if app.work_items.help.offset >= app.work_items.help.max_offset() {
            break;
        }
        app.work_items.help.scroll_by(4);
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
    while app.work_items.search_pending {
        app.work_items.poll_search(&mut app.shell);
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
    app.shell
        .hit_regions
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
    app
}

#[test]
fn a_notification_takes_the_hints_over_and_leaves_the_sync_where_it_is() {
    let mut app = App::new(vec![ticket()]);
    app.shell.enable_sync();
    app.shell.finish_sync();
    let quiet = render_text(130, 12, &mut app);
    let bar = quiet.lines().last().expect("a status bar").to_owned();
    assert!(bar.contains("move"), "the hints read on the left: {bar}");
    assert!(bar.contains("\u{25cf} Synced just now"), "{bar}");

    app.shell.set_status("Saved view 'Mine'");
    let text = render_text(130, 12, &mut app);
    let bar = text.lines().last().expect("a status bar").to_owned();
    assert!(bar.contains("\u{2713} Saved view 'Mine'"), "{bar}");
    assert!(
        bar.contains("\u{25cf} Synced just now"),
        "a notification never hides the sync segment: {bar}"
    );
    assert!(!bar.contains("move"), "it does take the hints over: {bar}");
}

#[test]
fn the_hints_are_cut_where_one_ends_rather_than_mid_key() {
    let mut app = App::new(vec![ticket()]);
    let narrow = render_text(60, 12, &mut app);
    let bar = narrow.lines().last().expect("a status bar").to_owned();
    assert!(
        bar.contains("\u{2191}\u{2193}/jk move"),
        "the first hint always fits: {bar}"
    );
    assert!(
        !bar.contains("wheel scr\u{200b}"),
        "and no hint is cut in half: {bar}"
    );
    let hint = crate::app::Screen::footer_hint(&app.work_items, &app.shell);
    for word in hint.split("  ") {
        let word = word.trim();
        if bar.contains(word) {
            continue;
        }
        assert!(
            !word.split_whitespace().any(|part| bar.contains(part)),
            "a hint that did not fit left nothing behind: {word:?} in {bar}"
        );
    }
}
