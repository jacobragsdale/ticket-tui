//! The Key Vault tab: the vault table, what one vault holds, and the one line
//! that ever shows a value.

use super::*;
use crate::app::key_vault::tests::{key_vault_app, secret};
use crate::app::key_vault::{Level, REVEAL_FOR};
use crate::app::{Focus, TabId};

/// The tab, drawn, with the Key Vault tab showing.
fn key_vault_text(width: u16, height: u16, app: &mut App) -> String {
    app.select_tab(TabId::KeyVault);
    render_text(width, height, app)
}

fn open_items(app: &mut App) {
    app.select_tab(TabId::KeyVault);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
}

/// The style one cell is painted in: the row is found by a word only it holds,
/// and the cell by where its own text starts along that row.
fn cell_style(app: &mut App, row: &str, cell: &str) -> Style {
    let mut terminal = Terminal::new(TestBackend::new(150, 26)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    for y in 0..26 {
        let line: String = (0..150).map(|x| buffer[(x, y)].symbol()).collect();
        if !line.contains(row) {
            continue;
        }
        let Some(column) = line.find(cell) else {
            panic!("the row holding {row} does not read {cell}: {line}");
        };
        return buffer[(u16::try_from(column).unwrap_or(u16::MAX), y)].style();
    }
    panic!("no row holds {row}");
}

/// The day one item's expiry falls on, which is where its cell starts. The
/// stamp itself is cut off by the column's width, and the day is enough to
/// find it by.
fn expiry_text(app: &App, name: &str) -> String {
    app.key_vault
        .visible_items()
        .into_iter()
        .find(|row| row.item.name == name)
        .and_then(|row| row.item.expires)
        .expect("an expiry")
        .calendar_date()
}

#[test]
fn the_table_lists_every_vault_with_where_it_lives() {
    let mut app = key_vault_app();
    let text = key_vault_text(140, 24, &mut app);

    assert!(
        text.contains("Resource group"),
        "the header names it: {text}"
    );
    assert!(text.contains("SKU"), "{text}");
    assert!(text.contains("Location"), "{text}");
    assert!(text.contains("atlas-kv"), "{text}");
    assert!(text.contains("labs-kv"), "{text}");
    assert!(text.contains("westeurope"), "{text}");
    assert!(pane_reads(&text, "Vaults", "2 vaults"), "{text}");

    // The details pane describes the one under the cursor, and the portal is
    // one line of it.
    assert!(text.contains("https://atlas-kv.vault.azure.net/"), "{text}");
    assert!(text.contains("Portal:"), "{text}");
    assert!(text.contains("portal.azure.com"), "{text}");
}

#[test]
fn one_vaults_contents_are_drawn_as_one_table_over_the_three_kinds() {
    let mut app = key_vault_app();
    open_items(&mut app);
    let text = render_text(140, 24, &mut app);

    assert_eq!(*app.key_vault.level(), Level::Items("atlas-kv".to_owned()));
    assert!(text.contains("Kind"), "{text}");
    assert!(text.contains("Enabled"), "{text}");
    assert!(text.contains("Expires"), "{text}");
    assert!(text.contains("db-password"), "{text}");
    assert!(text.contains("signing-key"), "{text}");
    assert!(text.contains("wildcard"), "{text}");
    assert!(text.contains("secret"), "{text}");
    assert!(text.contains("cert"), "{text}");
    assert!(
        pane_reads(&text, "atlas-kv", "6 items \u{00b7} 2 expiring"),
        "the border counts what is running out: {text}"
    );
    // The pane opens on what lapses first, and says which side of now it is.
    assert!(text.contains("3 days ago"), "{text}");

    app.key_vault.cursor_mut().focus(1);
    let ahead = render_text(140, 24, &mut app);
    assert!(ahead.contains("in 10 days"), "{ahead}");
    assert!(ahead.contains("Recoverable+Purgeable"), "{ahead}");
}

#[test]
fn a_disabled_row_fades_and_an_expiry_is_coloured_by_how_near_it_is() {
    let mut app = key_vault_app();
    open_items(&mut app);
    // The cursor is painted over whichever row it is on, so it is parked out
    // of the way of the three rows the colours are read off.
    app.key_vault.cursor_mut().focus(2);

    if theme().muted != Color::Reset {
        assert_eq!(
            cell_style(&mut app, "legacy-token", "legacy-token").fg,
            Some(theme().muted),
            "a disabled item is not in use, so its whole row is faded"
        );
        assert_ne!(
            cell_style(&mut app, "signing-key", "signing-key").fg,
            Some(theme().muted)
        );
    }
    if theme().warning != Color::Reset {
        let expires = expiry_text(&app, "wildcard");
        assert_eq!(
            cell_style(&mut app, "wildcard", &expires).fg,
            Some(theme().warning),
            "a month or less to go is worth a colour"
        );
    }
    if theme().error != Color::Reset {
        let expires = expiry_text(&app, "expired-api");
        assert_eq!(
            cell_style(&mut app, "expired-api", &expires).fg,
            Some(theme().error),
            "and one the clock has caught up with is worth a louder one"
        );
    }
}

#[test]
fn the_pane_shows_one_value_at_a_time_and_says_how_long_it_has_left() {
    let mut app = key_vault_app();
    open_items(&mut app);
    app.key_vault.set_query("name:db-password".to_owned());
    let idle = render_text(140, 26, &mut app);
    assert!(
        idle.contains("Reveal"),
        "a secret is offered the chip: {idle}"
    );
    assert!(idle.contains("Copy name"), "{idle}");
    assert!(!idle.contains("\u{2022}\u{2022}\u{2022}"), "{idle}");

    app.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE));
    let pending = render_text(140, 26, &mut app);
    assert!(
        pending.contains("\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}"),
        "the pane stands the value in until it lands: {pending}"
    );

    app.key_vault
        .set_revealed("atlas-kv", "db-password", Ok(secret("hunter2")));
    let shown = render_text(140, 26, &mut app);
    assert!(shown.contains("hunter2"), "{shown}");
    assert!(
        shown.contains(&format!("clears in {}s", REVEAL_FOR.as_secs())),
        "{shown}"
    );

    // And a minute later it is gone, without anybody pressing anything.
    app.key_vault.age_reveal(REVEAL_FOR);
    assert!(app.key_vault.tick());
    let cleared = render_text(140, 26, &mut app);
    assert!(!cleared.contains("hunter2"), "{cleared}");
}

#[test]
fn a_key_is_not_offered_the_chip_that_shows_a_value() {
    let mut app = key_vault_app();
    open_items(&mut app);
    app.key_vault.set_query("name:signing-key".to_owned());
    let text = render_text(140, 26, &mut app);

    assert!(text.contains("signing-key"), "{text}");
    assert!(text.contains("Copy name"), "{text}");
    assert!(
        !text.contains("Reveal"),
        "there is nothing to reveal, so there is no chip: {text}"
    );
}

#[test]
fn the_chips_run_what_the_keys_do() {
    let mut app = key_vault_app();
    open_items(&mut app);
    app.key_vault.set_query("name:db-password".to_owned());
    render_text(140, 26, &mut app);

    let chip = |app: &App, id: CommandId| {
        app.shell
            .hit_regions
            .find_target(
                |target| matches!(target, PointerTarget::RunCommand(found) if *found == id),
            )
            .unwrap_or_else(|| panic!("{id:?} has a chip"))
            .rect
    };
    let reveal = chip(&app, CommandId::RevealSecret);
    assert!(matches!(
        click(&mut app, reveal.x + 1, reveal.y),
        crate::app::AppAction::Arm(_)
    ));
    render_text(140, 26, &mut app);
    let copy = chip(&app, CommandId::CopyId);
    match click(&mut app, copy.x + 1, copy.y) {
        crate::app::AppAction::Copy { text, .. } => assert_eq!(text, "db-password"),
        other => panic!("the copy chip gave {other:?}"),
    }

    // A click on a row moves the cursor onto it.
    app.key_vault.set_query(String::new());
    render_text(140, 26, &mut app);
    let row = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::TableRow { index: 1 }))
        .expect("the second row is clickable")
        .rect;
    click(&mut app, row.x + 2, row.y);
    assert_eq!(app.shell.focus, Focus::Tickets);
    assert_eq!(
        app.key_vault.selected_item().map(|row| row.item.name),
        Some("wildcard".to_owned())
    );
}

#[test]
fn the_pane_says_why_when_there_is_nothing_to_read() {
    let mut app = App::new(Vec::new());
    app.shell.set_arm_state(Some(
        "no Azure subscription: pass --subscription".to_owned(),
    ));
    let text = key_vault_text(120, 20, &mut app);
    assert!(text.contains("no Azure subscription"), "{text}");
    assert!(pane_reads(&text, "Vaults", "0 vaults"), "{text}");
}
