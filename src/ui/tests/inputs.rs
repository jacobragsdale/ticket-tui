//! The one-row text fields: a caret that stays on its row however long the
//! text, measured in columns rather than characters; lists that say when they
//! have nothing; the form's wording and guidance; and a footer that keeps the
//! way out at any width.

use ratatui::layout::Position;

use super::*;
use crate::app::TabId;
use crate::text_input::display_width;

/// The screen as text, and where the caret was left.
fn render_with_caret(width: u16, height: u16, app: &mut App) -> (String, Position) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    let caret = terminal.get_cursor_position().unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..height {
        for x in 0..width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    (text, caret)
}

fn key(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn type_text(app: &mut App, text: &str) {
    for character in text.chars() {
        key(app, KeyCode::Char(character));
    }
}

/// The cells of one screen row between two columns, as text.
fn row_text(text: &str, y: u16, rect: Rect) -> String {
    text.lines()
        .nth(usize::from(y))
        .unwrap_or_default()
        .chars()
        .skip(usize::from(rect.x))
        .take(usize::from(rect.width))
        .collect()
}

/// `text` as the buffer prints it: a wide character is followed by the blank
/// cell it covers.
fn cells(text: &str) -> String {
    text.chars()
        .flat_map(|glyph| {
            let wide = display_width(glyph.encode_utf8(&mut [0; 4])) == 2;
            std::iter::once(glyph).chain(wide.then_some(' '))
        })
        .collect()
}

fn inside(caret: Position, rect: Rect) -> bool {
    rect.x <= caret.x && caret.x < rect.right() && rect.y <= caret.y && caret.y < rect.bottom()
}

fn search_field(app: &App) -> Rect {
    target_rect(app, |target| matches!(target, PointerTarget::SearchField))
}

#[test]
fn a_long_query_scrolls_by_whole_characters_so_the_caret_and_its_neighbours_stay_on_the_row() {
    let mut app = App::new(vec![ticket()]);
    app.work_items
        .run_command(&mut app.shell, CommandId::Search);
    let long: String = (0..120)
        .map(|n| char::from(b'a' + (n % 26) as u8))
        .collect();
    type_text(&mut app, &long);

    // The caret at the end: on the row, with the tail of the query before it.
    let (text, caret) = render_with_caret(60, 16, &mut app);
    let field = search_field(&app);
    assert!(inside(caret, field), "{caret:?} outside {field:?}");
    let shown = row_text(&text, field.y, field);
    let tail: String = long
        .chars()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    assert!(
        shown.trim_end().ends_with(&tail),
        "the end of the query is what the row shows: {shown}"
    );
    assert_eq!(
        caret.x,
        field.x + u16::try_from(shown.trim_end().chars().count()).unwrap(),
        "the caret sits after the last character shown"
    );

    // Home shows the start again and the caret goes with it.
    key(&mut app, KeyCode::Home);
    let (text, caret) = render_with_caret(60, 16, &mut app);
    assert_eq!(caret, Position::new(field.x, field.y));
    assert!(
        row_text(&text, field.y, field).starts_with("abcdef"),
        "{text}"
    );

    // Moving right past the edge scrolls one character at a time, and the
    // character under the caret is always on screen.
    for _ in 0..usize::from(field.width) + 3 {
        key(&mut app, KeyCode::Right);
    }
    let (text, caret) = render_with_caret(60, 16, &mut app);
    let index = app.work_items.query_cursor();
    assert!(inside(caret, field), "{caret:?} outside {field:?}");
    let under = long.chars().nth(index).unwrap();
    let shown = row_text(&text, field.y, field);
    assert_eq!(
        shown.chars().nth(usize::from(caret.x - field.x)),
        Some(under),
        "the caret is on the character it belongs to: {shown}"
    );

    // A click on the scrolled row lands on the character it is over, not on
    // that column of the text.
    let under_click = shown.chars().nth(4).unwrap();
    click(&mut app, field.x + 4, field.y);
    assert_eq!(app.work_items.mode, WorkItemMode::Search);
    assert_eq!(
        app.work_items.query_cursor(),
        index - usize::from(field.width) + 1 + 4,
        "that character in the text, not that column of the row"
    );
    assert_eq!(
        long.chars().nth(app.work_items.query_cursor()),
        Some(under_click)
    );
    let (text, after) = render_with_caret(60, 16, &mut app);
    assert!(inside(after, field), "{after:?} outside {field:?}");
    assert_eq!(
        row_text(&text, field.y, field)
            .chars()
            .nth(usize::from(after.x - field.x)),
        Some(under_click),
        "and the caret is still on it once the row settles: {text}"
    );

    // Narrower and wider, the caret is still on the row it belongs to.
    key(&mut app, KeyCode::End);
    for (width, height) in [(120, 35), (80, 24), (50, 16), (20, 8), (8, 3)] {
        let (_, caret) = render_with_caret(width, height, &mut app);
        assert!(
            caret.x < width && caret.y < height,
            "{width}x{height} left the caret at {caret:?}"
        );
    }
}

#[test]
fn wide_characters_and_combining_marks_put_the_caret_on_their_own_column() {
    let mut app = App::new(vec![ticket()]);
    app.work_items
        .run_command(&mut app.shell, CommandId::Search);
    type_text(&mut app, "日本語 tea");
    let (text, caret) = render_with_caret(80, 16, &mut app);
    let field = search_field(&app);
    assert_eq!(display_width("日本語 tea"), 10);
    assert_eq!(
        caret.x,
        field.x + 10,
        "three wide characters are six columns"
    );
    assert!(
        row_text(&text, field.y, field).starts_with(&cells("日本語 tea")),
        "{text}"
    );
    for _ in 0..4 {
        key(&mut app, KeyCode::Left);
    }
    let (_, caret) = render_with_caret(80, 16, &mut app);
    assert_eq!(caret.x, field.x + 6, "after 日本語, before the space");

    // A combining mark shares its base's column, so the caret after it is
    // one column on, not two.
    app.work_items.set_query(&mut app.shell, String::new());
    type_text(&mut app, "e\u{301}x");
    let (_, caret) = render_with_caret(80, 16, &mut app);
    assert_eq!(caret.x, field.x + 2);
    key(&mut app, KeyCode::Left);
    let (_, caret) = render_with_caret(80, 16, &mut app);
    assert_eq!(caret.x, field.x + 1);

    // A row too narrow for the whole of a CJK title shows whole characters
    // only, none cut in half, and the caret stays on it.
    app.work_items.set_query(&mut app.shell, String::new());
    type_text(&mut app, &"日本語".repeat(20));
    let (text, caret) = render_with_caret(40, 12, &mut app);
    let field = search_field(&app);
    assert!(inside(caret, field), "{caret:?} outside {field:?}");
    let shown = row_text(&text, field.y, field);
    assert!(
        shown.chars().all(|glyph| "日本語 ".contains(glyph)),
        "nothing but whole characters on the row: {shown:?}"
    );

    // The capture row measures the same way.
    key(&mut app, KeyCode::Esc);
    app.work_items.set_query(&mut app.shell, String::new());
    key(&mut app, KeyCode::Char('+'));
    assert_eq!(app.work_items.mode, WorkItemMode::Capture);
    type_text(&mut app, "日本");
    let (text, caret) = render_with_caret(80, 16, &mut app);
    assert!(text.contains(&format!("+ {}", cells("日本"))), "{text}");
    assert_eq!(
        caret.x,
        2 + 4,
        "two wide characters after the glyph and its gap"
    );
}

#[test]
fn modal_inputs_scroll_their_values_and_keep_their_labels_still() {
    let mut app = App::new(vec![ticket()]);
    app.shell.enable_sync();
    let long: String = (0..90).map(|n| char::from(b'a' + (n % 26) as u8)).collect();
    let tail: String = long.chars().skip(86).collect();

    // The palette's filter field.
    app.work_items
        .run_command(&mut app.shell, CommandId::Palette);
    assert_eq!(app.work_items.mode, WorkItemMode::Palette);
    type_text(&mut app, &long);
    let (text, caret) = render_with_caret(80, 24, &mut app);
    let field = target_rect(&app, |target| matches!(target, PointerTarget::PaletteQuery));
    assert!(inside(caret, field), "{caret:?} outside {field:?}");
    assert!(
        row_text(&text, field.y, field).trim_end().ends_with(&tail),
        "the tail of the filter is on the row: {text}"
    );
    key(&mut app, KeyCode::Home);
    let (text, caret) = render_with_caret(80, 24, &mut app);
    assert_eq!(caret.x, field.x);
    assert!(
        row_text(&text, field.y, field).starts_with("abcd"),
        "{text}"
    );
    key(&mut app, KeyCode::Esc);

    // The title prompt: `Title:` stays put while the value scrolls.
    key(&mut app, KeyCode::Char('e'));
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.work_items.mode, WorkItemMode::Prompt);
    let (before, _) = render_with_caret(80, 24, &mut app);
    let label_column = before
        .lines()
        .find_map(|line| line.find("Title: "))
        .expect("the prompt names its field");
    key(&mut app, KeyCode::End);
    type_text(&mut app, &long);
    let (text, caret) = render_with_caret(80, 24, &mut app);
    let editable = target_rect(&app, |target| matches!(target, PointerTarget::PromptInput));
    assert_eq!(
        text.lines().find_map(|line| line.find("Title: ")),
        Some(label_column),
        "the label did not move: {text}"
    );
    assert!(inside(caret, editable), "{caret:?} outside {editable:?}");
    assert!(
        row_text(&text, editable.y, editable)
            .trim_end()
            .ends_with(&tail),
        "{text}"
    );
    key(&mut app, KeyCode::Esc);

    // The form's Title row: the label column never moves either.
    key(&mut app, KeyCode::Char('n'));
    assert_eq!(app.work_items.mode, WorkItemMode::Form);
    key(&mut app, KeyCode::Down);
    let (before, _) = render_with_caret(80, 24, &mut app);
    let title_column = before
        .lines()
        .find_map(|line| line.find("Title *"))
        .expect("the form has a Title row");
    type_text(&mut app, &long);
    let (text, caret) = render_with_caret(80, 24, &mut app);
    assert_eq!(
        text.lines().find_map(|line| line.find("Title *")),
        Some(title_column),
        "{text}"
    );
    let title = app
        .work_items
        .form
        .as_ref()
        .and_then(|form| form.index_of(FormFieldId::Title))
        .unwrap();
    // The row's value rect is the later of its two regions.
    let value = target_rect(
        &app,
        |target| matches!(target, PointerTarget::FormField { index } if *index == title),
    );
    assert_eq!(caret.y, value.y, "the caret is on the Title row");
    assert!(
        inside(caret, value) && usize::from(caret.x) > title_column + "Title *".len(),
        "and past the label, on the value: {caret:?} in {value:?}"
    );
    let row = text.lines().nth(usize::from(value.y)).unwrap();
    assert!(
        row.contains(&tail),
        "the tail of the title is on its row: {row}"
    );

    // The view-name field, the same way.
    key(&mut app, KeyCode::Esc);
    app.work_items.run_command(&mut app.shell, CommandId::Views);
    key(&mut app, KeyCode::Char('n'));
    assert!(app.work_items.views_overlay.naming.is_some());
    type_text(&mut app, &long);
    let (text, caret) = render_with_caret(80, 24, &mut app);
    let name = target_rect(&app, |target| matches!(target, PointerTarget::ViewName));
    assert!(inside(caret, name), "{caret:?} outside {name:?}");
    assert!(text.contains("Name: "), "{text}");
    assert!(
        row_text(&text, name.y, name).trim_end().ends_with(&tail),
        "{text}"
    );
}

#[test]
fn an_empty_list_says_so_without_a_row_to_pick() {
    let mut app = App::new(vec![ticket()]);
    app.shell.enable_sync();
    app.shell.set_me(Some("Jacob Ragsdale".into()));
    let muted = theme().muted;

    // The palette.
    app.work_items
        .run_command(&mut app.shell, CommandId::Palette);
    type_text(&mut app, "zzzzzz");
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let text = render_text(90, 24, &mut app);
    assert!(
        text.contains("No matching commands \u{2014} Ctrl-U clears the filter"),
        "{text}"
    );
    assert!(
        app.shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::PaletteCommand { .. }))
            .is_none(),
        "a note is not a command to click"
    );
    let (y, x) = text
        .lines()
        .enumerate()
        .find_map(|(y, line)| line.find("No matching").map(|x| (y, x)))
        .unwrap();
    let cell = &terminal.backend().buffer()[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())];
    assert_eq!(cell.fg, muted, "the note is muted");
    assert!(
        !cell.modifier.contains(Modifier::BOLD),
        "and does not read as the row under the cursor"
    );
    if theme().surface != Color::Reset {
        assert_ne!(cell.bg, theme().surface);
    }
    let narrow = render_text(40, 16, &mut app);
    assert!(
        narrow.contains("No matching commands") && !narrow.contains("Ctrl-U"),
        "a narrow palette keeps the message and drops the hint: {narrow}"
    );
    key(&mut app, KeyCode::Esc);

    // The assignee picker.
    key(&mut app, KeyCode::Char('a'));
    assert_eq!(app.work_items.mode, WorkItemMode::AssigneePicker);
    type_text(&mut app, "zzz");
    let text = render_text(90, 24, &mut app);
    assert!(text.contains("No matching people"), "{text}");
    assert!(
        app.shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::AssigneeOption { .. }))
            .is_none()
    );
    // The first Esc clears the filter; the next closes the picker.
    while app.work_items.mode != WorkItemMode::Browse {
        key(&mut app, KeyCode::Esc);
    }

    // The iteration picker: a project no tree has been read for and no work
    // item names a sprint in says so, and a filter that matches nothing says
    // that instead.
    let mut unplanned = ticket();
    unplanned.iteration_path = String::new();
    let mut app = App::new(vec![unplanned]);
    app.shell.enable_sync();
    app.work_items
        .run_command(&mut app.shell, CommandId::EditIteration);
    assert_eq!(app.work_items.mode, WorkItemMode::NodePicker);
    let text = render_text(90, 24, &mut app);
    assert!(text.contains("No iterations known yet"), "{text}");
    assert!(
        app.shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::NodeOption { .. }))
            .is_none()
    );
    key(&mut app, KeyCode::Esc);
    use crate::classification::{ClassificationNode, NodeKind};
    app.work_items.set_classification_nodes(
        vec![ClassificationNode::new(
            NodeKind::Iteration,
            "development\\Sprint 1",
            1,
        )],
        None,
    );
    app.work_items
        .run_command(&mut app.shell, CommandId::EditIteration);
    assert_eq!(app.work_items.mode, WorkItemMode::NodePicker);
    type_text(&mut app, "zzz");
    let text = render_text(90, 24, &mut app);
    assert!(text.contains("No matching iterations"), "{text}");
}

#[test]
fn the_form_closes_keeping_its_draft_and_says_why_create_is_off() {
    let mut app = App::new(vec![ticket()]);
    app.shell.enable_sync();
    key(&mut app, KeyCode::Char('n'));
    assert_eq!(app.work_items.mode, WorkItemMode::Form);

    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let text = render_text(90, 24, &mut app);
    assert!(text.contains("Title is required"), "{text}");
    assert!(text.contains(" Close "), "{text}");
    assert!(text.contains("Esc keep draft"), "{text}");
    assert!(!text.contains("Esc cancel"), "{text}");
    let (y, x) = text
        .lines()
        .enumerate()
        .find_map(|(y, line)| line.find("Title is required").map(|x| (y, x)))
        .unwrap();
    let cell = &terminal.backend().buffer()[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())];
    assert_eq!(cell.fg, theme().muted, "guidance, not an error");
    assert!(
        app.shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::SubmitForm))
            .is_none(),
        "Create is off while the guidance says why"
    );
    assert!(
        app.shell.notification().is_none(),
        "nothing has been refused yet"
    );

    // Filling the field takes the guidance away and lights the button.
    key(&mut app, KeyCode::Down);
    type_text(&mut app, "Hello");
    let text = render_text(90, 24, &mut app);
    assert!(!text.contains("is required"), "{text}");
    assert!(
        app.shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::SubmitForm))
            .is_some()
    );
    // Emptying it again brings the guidance back.
    for _ in 0..5 {
        key(&mut app, KeyCode::Backspace);
    }
    assert!(render_text(90, 24, &mut app).contains("Title is required"));
    type_text(&mut app, "Hello");

    // Esc keeps the draft for `n` to bring back; so does the button.
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    key(&mut app, KeyCode::Char('n'));
    assert_eq!(
        app.work_items
            .form
            .as_ref()
            .map(|form| form.value(FormFieldId::Title)),
        Some("Hello")
    );
    render_text(90, 24, &mut app);
    let close = target_rect(&app, |target| matches!(target, PointerTarget::CancelForm));
    click(&mut app, close.x, close.y);
    assert_eq!(app.work_items.mode, WorkItemMode::Browse);
    key(&mut app, KeyCode::Char('n'));
    assert_eq!(
        app.work_items
            .form
            .as_ref()
            .map(|form| form.value(FormFieldId::Title)),
        Some("Hello")
    );

    // A short terminal still shows the field under the caret and the buttons.
    let (text, caret) = render_with_caret(60, 11, &mut app);
    assert!(
        text.contains(" Create ") && text.contains(" Close "),
        "{text}"
    );
    assert!(caret.x < 60 && caret.y < 11, "{caret:?}");
}

/// The footer's left segment, split into the hints it shows; the sync
/// segment on the right is cut off first.
fn footer_hints(app: &App, text: &str) -> Vec<String> {
    let label = app.shell.sync_status().label();
    let last = text.lines().last().unwrap_or_default();
    let left = last.rfind(&label).map_or(last, |at| &last[..at]);
    let left = left.trim_end_matches(|glyph: char| !glyph.is_alphanumeric());
    left.split("  ")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_narrow_footer_keeps_the_way_out_and_drops_the_rest_whole() {
    let mut app = App::new(vec![ticket()]);
    app.shell.enable_sync();
    app.work_items
        .run_command(&mut app.shell, CommandId::Palette);
    let full = app.work_items.footer_hint(&app.shell).to_owned();
    let units: Vec<&str> = full.split("  ").collect();

    let text = render_text(50, 16, &mut app);
    let hints = footer_hints(&app, &text);
    assert!(
        hints.iter().any(|hint| hint == "Enter run"),
        "the submit hint survives: {hints:?}"
    );
    assert!(
        hints.iter().any(|hint| hint == "Esc close"),
        "and so does the way out: {hints:?}"
    );
    assert!(
        hints.len() < units.len(),
        "something was dropped to fit: {hints:?}"
    );
    for hint in &hints {
        assert!(
            units.contains(&hint.as_str()),
            "nothing is cut in the middle of a hint: {hint:?} from {full:?}"
        );
    }
    key(&mut app, KeyCode::Esc);

    // The form, whose way out is the draft.
    key(&mut app, KeyCode::Char('n'));
    let text = render_text(44, 16, &mut app);
    let hints = footer_hints(&app, &text);
    assert!(
        hints.iter().any(|hint| hint == "Ctrl-S create"),
        "{hints:?}"
    );
    assert!(
        hints.iter().any(|hint| hint == "Esc keep draft"),
        "{hints:?}"
    );
    key(&mut app, KeyCode::Esc);

    // Browsing keeps the help.
    let text = render_text(50, 16, &mut app);
    let hints = footer_hints(&app, &text);
    assert!(hints.iter().any(|hint| hint == "? help"), "{hints:?}");
    let last = text.lines().last().unwrap();
    assert!(
        last.contains(&app.shell.sync_status().label()),
        "the sync segment is still on the right: {last}"
    );

    // Too narrow for anything is nothing at all, never a fragment.
    let text = render_text(14, 6, &mut app);
    let last = text.lines().last().unwrap();
    assert!(!last.contains("↑↓/j"), "{last}");
}

#[test]
fn every_size_paints_an_open_input_inside_the_frame() {
    let long: String = (0..100)
        .map(|n| char::from(b'a' + (n % 26) as u8))
        .collect();
    type Opener = fn(&mut App, &str);
    let open: Vec<(&str, Opener)> = vec![
        ("search", |app, long| {
            app.work_items
                .run_command(&mut app.shell, CommandId::Search);
            type_text(app, long);
        }),
        ("palette", |app, long| {
            app.work_items
                .run_command(&mut app.shell, CommandId::Palette);
            type_text(app, long);
        }),
        ("prompt", |app, long| {
            key(app, KeyCode::Char('e'));
            key(app, KeyCode::Down);
            key(app, KeyCode::Enter);
            type_text(app, long);
        }),
        ("form", |app, long| {
            key(app, KeyCode::Char('n'));
            key(app, KeyCode::Down);
            type_text(app, long);
        }),
        ("assignee", |app, long| {
            key(app, KeyCode::Char('a'));
            type_text(app, long);
        }),
        ("capture", |app, long| {
            key(app, KeyCode::Char('+'));
            type_text(app, long);
        }),
    ];
    for (name, open) in open {
        for tab in [TabId::WorkItems, TabId::Repos] {
            let mut app = App::new(vec![ticket()]);
            app.shell.enable_sync();
            app.tab = tab;
            if tab != TabId::WorkItems && name != "capture" {
                continue;
            }
            open(&mut app, &long);
            for (width, height) in [
                (120, 35),
                (80, 24),
                (50, 16),
                (20, 8),
                (8, 3),
                (2, 2),
                (1, 1),
            ] {
                let (_, caret) = render_with_caret(width, height, &mut app);
                assert!(
                    caret.x < width && caret.y < height,
                    "{name} at {width}x{height} left the caret at {caret:?}"
                );
            }
        }
    }
}
