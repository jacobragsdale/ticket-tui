use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::app::App;
use crate::arm::tests::FakeArm;
use crate::arm::{ArmSource, Registry};
use crate::timestamp::ts;

/// A value as a vault would hand it back, since a [`Secret`] is only ever made
/// by reading one.
pub(crate) fn secret(value: &str) -> Secret {
    let source = FakeArm::default();
    *source.secret.lock().unwrap() = value.to_owned();
    source
        .secret_value(&vault("atlas-kv", "platform", "westeurope"), "db-password")
        .expect("the fake vault answers")
}

/// An instant `days` from now, which is what an expiry has to be measured
/// against: the colours and the badge read the clock, not a fixed date. The
/// extra hour is slack, so that the whole days a pane counts do not tip over
/// between the fixture being built and the frame being drawn.
pub(crate) fn in_days(days: i64) -> Timestamp {
    Timestamp::now().plus_seconds(days * 24 * 60 * 60 + days.signum() * 60 * 60)
}

pub(crate) fn vault(name: &str, group: &str, location: &str) -> Vault {
    Vault {
        id: format!(
            "/subscriptions/sub-1/resourceGroups/{group}/providers/Microsoft.KeyVault/vaults/{name}"
        ),
        name: name.to_owned(),
        resource_group: group.to_owned(),
        location: location.to_owned(),
        sku: "standard".to_owned(),
        uri: format!("https://{name}.vault.azure.net/"),
    }
}

fn item(kind: ItemKind, name: &str, enabled: bool, expires: Option<Timestamp>) -> VaultItem {
    VaultItem {
        kind,
        name: name.to_owned(),
        enabled,
        created: Some(ts("2026-08-01T09:00:00Z")),
        updated: Some(ts("2026-08-20T09:00:00Z")),
        expires,
        content_type: (kind == ItemKind::Secret).then(|| "text/plain".to_owned()),
        recovery_level: Some("Recoverable+Purgeable".to_owned()),
    }
}

fn inventory() -> Inventory {
    Inventory {
        registries: vec![Registry {
            id: "/subscriptions/sub-1/resourceGroups/platform/providers/Microsoft.ContainerRegistry/registries/atlas".to_owned(),
            name: "atlas".to_owned(),
            resource_group: "platform".to_owned(),
            location: "westeurope".to_owned(),
            sku: "Premium".to_owned(),
            login_server: "atlas.azurecr.io".to_owned(),
        }],
        vaults: vec![
            vault("atlas-kv", "platform", "westeurope"),
            vault("labs-kv", "labs", "northeurope"),
        ],
    }
}

/// Everything one vault holds: two secrets, a key, and three certificates —
/// one lapsed, one lapsing this month, one nowhere near.
pub(crate) fn items() -> Vec<VaultItem> {
    vec![
        item(ItemKind::Secret, "db-password", true, None),
        item(ItemKind::Secret, "legacy-token", false, None),
        item(ItemKind::Key, "signing-key", true, None),
        item(
            ItemKind::Certificate,
            "expired-api",
            true,
            Some(in_days(-3)),
        ),
        item(ItemKind::Certificate, "wildcard", true, Some(in_days(10))),
        item(ItemKind::Certificate, "far-off", true, Some(in_days(200))),
    ]
}

/// An app whose Key Vault tab holds two vaults, one of them with its contents
/// read.
pub(crate) fn key_vault_app() -> App {
    let mut app = App::new(Vec::new());
    app.key_vault.set_inventory(Ok(inventory()));
    app.key_vault.set_items("atlas-kv", Ok(items()));
    app.select_tab(TabId::KeyVault);
    app
}

fn press(app: &mut App, code: KeyCode) -> AppAction {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn vault_names(app: &App) -> Vec<String> {
    app.key_vault
        .visible_vaults()
        .into_iter()
        .map(|row| row.vault.name)
        .collect()
}

fn item_names(app: &App) -> Vec<String> {
    app.key_vault
        .visible_items()
        .into_iter()
        .map(|row| row.item.name)
        .collect()
}

/// The tab open on one vault's contents, with the cursor on the one secret
/// named.
fn opened_on(name: &str) -> App {
    let mut app = key_vault_app();
    press(&mut app, KeyCode::Enter);
    app.key_vault.set_query(format!("name:{name}"));
    app
}

fn notification(app: &App) -> String {
    app.shell
        .notification()
        .map(|(message, _)| message.to_owned())
        .unwrap_or_default()
}

#[test]
fn the_table_lists_every_vault_the_subscription_holds_and_the_query_narrows_it() {
    let mut app = key_vault_app();
    assert_eq!(vault_names(&app), vec!["atlas-kv", "labs-kv"]);

    app.key_vault.set_query("rg:labs".to_owned());
    assert_eq!(vault_names(&app), vec!["labs-kv"]);
    app.key_vault.set_query("location:westeurope".to_owned());
    assert_eq!(vault_names(&app), vec!["atlas-kv"]);
    app.key_vault.set_query("atlas".to_owned());
    assert_eq!(vault_names(&app), vec!["atlas-kv"]);
    app.key_vault.set_query(String::new());

    // A vault nobody has opened has no count to show yet.
    let counts: Vec<Option<usize>> = app
        .key_vault
        .visible_vaults()
        .into_iter()
        .map(|row| row.items)
        .collect();
    assert_eq!(counts, vec![Some(6), None]);
}

#[test]
fn enter_opens_a_vault_and_lists_all_three_kinds_soonest_to_lapse_first() {
    let mut app = key_vault_app();
    press(&mut app, KeyCode::Enter);

    assert_eq!(*app.key_vault.level(), Level::Items("atlas-kv".to_owned()));
    assert_eq!(
        item_names(&app),
        vec![
            "expired-api",
            "wildcard",
            "far-off",
            "db-password",
            "legacy-token",
            "signing-key",
        ],
        "what lapses first is at the top, and what never lapses is at the bottom"
    );

    // The one query the search box spells out.
    app.key_vault
        .set_query("kind:cert expires:<+30d".to_owned());
    assert_eq!(item_names(&app), vec!["expired-api", "wildcard"]);
    app.key_vault.set_query("kind:cert".to_owned());
    assert_eq!(item_names(&app), vec!["expired-api", "wildcard", "far-off"]);
    app.key_vault.set_query("enabled:no".to_owned());
    assert_eq!(item_names(&app), vec!["legacy-token"]);
    app.key_vault.set_query("enabled:true".to_owned());
    assert_eq!(item_names(&app).len(), 5, "both spellings are accepted");

    press(&mut app, KeyCode::Char('h'));
    assert_eq!(*app.key_vault.level(), Level::Vaults);
    assert_eq!(
        app.key_vault.query(),
        "",
        "each level keeps the query it was left with"
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.key_vault.query(), "enabled:true");
}

#[test]
fn r_reads_one_secrets_value_and_y_copies_it_while_it_is_showing() {
    let mut app = opened_on("db-password");
    assert_eq!(
        press(&mut app, KeyCode::Char('R')),
        AppAction::Arm(ArmRequest::Reveal {
            vault: "atlas-kv".to_owned(),
            name: "db-password".to_owned(),
        })
    );
    assert!(app.key_vault.reveal_pending(), "the pane draws dots for it");
    assert!(app.key_vault.busy(), "and the spinner turns until it lands");
    assert_eq!(
        press(&mut app, KeyCode::Char('Y')),
        AppAction::None,
        "there is nothing on screen to copy yet"
    );

    app.key_vault
        .set_revealed("atlas-kv", "db-password", Ok(secret("hunter2")));
    assert!(!app.key_vault.reveal_pending());
    assert!(!app.key_vault.busy());
    let revealed = app.key_vault.revealed().expect("the value is on screen");
    assert_eq!(revealed.value.expose(), "hunter2");
    assert!(revealed.clears_in <= REVEAL_FOR);

    match press(&mut app, KeyCode::Char('Y')) {
        AppAction::CopySecret(secret) => assert_eq!(secret.expose(), "hunter2"),
        other => panic!("Y gave {other:?}"),
    }
}

#[test]
fn only_a_secret_has_a_value_to_show() {
    let mut app = opened_on("signing-key");
    assert_eq!(press(&mut app, KeyCode::Char('R')), AppAction::None);
    assert_eq!(notification(&app), "Only a secret has a value to show");
    assert!(!app.key_vault.reveal_pending());

    // Up top there is no item at all.
    press(&mut app, KeyCode::Char('h'));
    assert_eq!(press(&mut app, KeyCode::Char('R')), AppAction::None);
    assert_eq!(notification(&app), "No secret here to show");
}

#[test]
fn a_value_that_landed_after_the_cursor_moved_on_is_dropped_rather_than_shown() {
    let mut app = opened_on("db-password");
    press(&mut app, KeyCode::Char('R'));
    app.key_vault.set_query("kind:key".to_owned());

    app.key_vault
        .set_revealed("atlas-kv", "db-password", Ok(secret("hunter2")));
    assert!(
        app.key_vault.revealed().is_none(),
        "the pane is on another item, and a value belongs to the one asked for"
    );

    // A refusal is said once and leaves the pane saying why.
    let mut app = opened_on("db-password");
    assert_eq!(
        app.key_vault.set_revealed(
            "atlas-kv",
            "db-password",
            Err("the vault refused the read".to_owned())
        ),
        Some("the vault refused the read".to_owned())
    );
    assert_eq!(
        app.key_vault.arm_error(),
        Some("the vault refused the read")
    );
    assert!(app.key_vault.revealed().is_none());
    assert!(!app.key_vault.busy());
}

#[test]
fn everything_that_means_somebody_looked_away_takes_the_value_off_the_screen() {
    let shown = |app: &App| app.key_vault.revealed().is_some();
    let reveal = |app: &mut App| {
        app.key_vault
            .set_revealed("atlas-kv", "db-password", Ok(secret("hunter2")));
    };

    // Esc, with nothing else to clear.
    let mut app = opened_on("db-password");
    reveal(&mut app);
    assert!(shown(&app));
    press(&mut app, KeyCode::Esc);
    assert!(!shown(&app));
    assert_eq!(press(&mut app, KeyCode::Char('Y')), AppAction::None);
    assert_eq!(notification(&app), "Nothing is revealed to copy");

    // The cursor moving off the item it belongs to.
    let mut app = opened_on("db-password");
    reveal(&mut app);
    press(&mut app, KeyCode::Char('j'));
    assert!(!shown(&app));

    // Leaving the tab, which the shell closes the screen on the way out of.
    let mut app = opened_on("db-password");
    reveal(&mut app);
    app.select_tab(TabId::WorkItems);
    assert!(!shown(&app));

    // Going back up a level.
    let mut app = opened_on("db-password");
    reveal(&mut app);
    press(&mut app, KeyCode::Char('h'));
    assert!(!shown(&app));

    // A refresh.
    let mut app = opened_on("db-password");
    reveal(&mut app);
    assert_eq!(
        press(&mut app, KeyCode::Char('r')),
        AppAction::Arm(ArmRequest::Refresh)
    );
    assert!(!shown(&app));

    // And the minute running out, which the loop wakes up for.
    let mut app = opened_on("db-password");
    reveal(&mut app);
    assert!(!app.key_vault.tick(), "not yet");
    assert!(
        app.key_vault
            .next_wakeup()
            .is_some_and(|left| left <= REVEAL_FOR)
    );
    app.key_vault.age_reveal(REVEAL_FOR);
    assert!(
        app.key_vault.tick(),
        "and the screen is repainted without it"
    );
    assert!(!shown(&app));
    assert_eq!(app.key_vault.next_wakeup(), None);
    assert_eq!(press(&mut app, KeyCode::Char('Y')), AppAction::None);
    assert_eq!(notification(&app), "Nothing is revealed to copy");
}

#[test]
fn the_badge_counts_the_certificates_running_out_across_every_vault_read() {
    let mut app = key_vault_app();
    assert_eq!(
        Screen::badge(&app.key_vault),
        Some("\u{25c7}2".to_owned()),
        "one lapsed and one lapsing this month; the one 200 days out is nobody's problem"
    );
    assert_eq!(app.key_vault.expiring_certificates(), 2);

    app.key_vault.set_items(
        "atlas-kv",
        Ok(vec![item(
            ItemKind::Certificate,
            "far-off",
            true,
            Some(in_days(200)),
        )]),
    );
    assert_eq!(Screen::badge(&app.key_vault), None);
}

#[test]
fn y_copies_the_name_and_o_opens_the_vault_in_the_portal() {
    let mut app = key_vault_app();
    match press(&mut app, KeyCode::Char('y')) {
        AppAction::Copy { text, content } => {
            assert_eq!(text, "atlas-kv");
            assert_eq!(content, CopiedContent::Id);
        }
        other => panic!("y up top gave {other:?}"),
    }
    assert!(matches!(
        press(&mut app, KeyCode::Char('o')),
        AppAction::OpenUrl(url) if url.ends_with("/vaults/atlas-kv")
    ));

    let mut app = opened_on("db-password");
    match press(&mut app, KeyCode::Char('y')) {
        AppAction::Copy { text, .. } => assert_eq!(text, "db-password"),
        other => panic!("y on an item gave {other:?}"),
    }
    // Down a level, `o` still opens the vault the items belong to.
    assert!(matches!(
        press(&mut app, KeyCode::Char('o')),
        AppAction::OpenUrl(url) if url.ends_with("/vaults/atlas-kv")
    ));
}

#[test]
fn the_focus_asks_for_one_vaults_contents_and_then_rests() {
    let mut app = App::new(Vec::new());
    app.key_vault.set_inventory(Ok(inventory()));
    app.select_tab(TabId::KeyVault);
    assert_eq!(
        app.key_vault.focus(),
        None,
        "a vault under the cursor up top is nothing to read"
    );

    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.key_vault.focus(),
        Some(ArmFocus::Vault("atlas-kv".to_owned()))
    );
    assert!(app.key_vault.busy());

    app.key_vault.set_items("atlas-kv", Ok(items()));
    assert_eq!(
        app.key_vault.focus(),
        None,
        "answered once, and a poll never asks again"
    );
    assert!(!app.key_vault.busy());

    // A refusal is an answer too: the pane says why rather than asking for
    // ever.
    app.key_vault
        .set_items("labs-kv", Err("no list access".to_owned()));
    assert_eq!(app.key_vault.arm_error(), Some("no list access"));
    assert!(!app.key_vault.busy());
}

#[test]
fn a_vault_and_an_item_are_both_places_to_jump_to_and_come_back_from() {
    let mut app = key_vault_app();
    press(&mut app, KeyCode::Enter);
    app.key_vault.set_query("name:wildcard".to_owned());
    let here = app
        .key_vault
        .here(&app.shell)
        .expect("a place to come back to");
    assert_eq!(
        here,
        Jump::VaultItem {
            vault: "atlas-kv".to_owned(),
            kind: "cert".to_owned(),
            name: "wildcard".to_owned(),
        }
    );
    // The session writes these out and reads them back.
    let written = serde_json::to_string(&here).unwrap();
    assert_eq!(
        serde_json::from_str::<Jump>(&written).unwrap(),
        here,
        "{written}"
    );
    assert!(here.describe().contains("wildcard"));
    let vault = Jump::Vault("atlas-kv".to_owned());
    let written = serde_json::to_string(&vault).unwrap();
    assert_eq!(
        serde_json::from_str::<Jump>(&written).unwrap(),
        vault,
        "{written}"
    );
    assert!(vault.describe().contains("atlas-kv"));

    app.select_tab(TabId::WorkItems);
    assert!(app.follow(&Jump::Vault("labs-kv".to_owned())));
    assert_eq!(app.tab, TabId::KeyVault);
    assert_eq!(*app.key_vault.level(), Level::Vaults);
    assert_eq!(
        app.key_vault.selected_vault().map(|row| row.vault.name),
        Some("labs-kv".to_owned())
    );

    // A jump beats a filter: a query hiding the target is cleared rather than
    // reported as a missing row.
    app.key_vault.set_query("name:labs-kv".to_owned());
    assert!(app.follow(&here));
    assert_eq!(*app.key_vault.level(), Level::Items("atlas-kv".to_owned()));
    assert_eq!(
        app.key_vault.selected_item().map(|row| row.item.name),
        Some("wildcard".to_owned())
    );

    app.select_tab(TabId::WorkItems);
    assert!(
        !app.follow(&Jump::Vault("nowhere".to_owned())),
        "a vault the subscription does not hold is not switched to"
    );
    assert_eq!(app.tab, TabId::WorkItems);
}

#[test]
fn the_agent_context_says_a_value_is_showing_and_never_what_it_is() {
    let app = key_vault_app();
    let top = app.key_vault.agent_context();
    assert_eq!(top.level, "vaults");
    assert_eq!(top.visible_rows, 2);
    assert_eq!(top.expiring_certificates, 2);
    let selected = top.selected_vault.expect("a vault under the cursor");
    assert_eq!(selected.name, "atlas-kv");
    assert_eq!(selected.resource_group, "platform");
    assert_eq!(selected.location, "westeurope");
    assert_eq!(selected.sku, "standard");
    assert_eq!(selected.uri, "https://atlas-kv.vault.azure.net/");
    assert!(selected.portal_url.contains("portal.azure.com"));
    assert!(top.selected_item.is_none());

    let mut app = opened_on("db-password");
    app.key_vault
        .set_revealed("atlas-kv", "db-password", Ok(secret("hunter2")));
    let context = app.key_vault.agent_context();
    assert_eq!(context.level, "items");
    let item = context.selected_item.expect("an item under the cursor");
    assert_eq!(item.kind, "secret");
    assert_eq!(item.name, "db-password");
    assert!(item.enabled);
    assert!(item.updated.is_some());
    assert_eq!(item.expires, None);
    assert!(item.revealed, "the pane is showing it this minute");

    // Nowhere a value could reach: not the context, printed or written, not
    // the session file, and not the action that copies it.
    let printed = format!("{:?}", app.agent_context());
    assert!(!printed.contains("hunter2"), "{printed}");
    let written = serde_json::to_string(&app.agent_context()).unwrap();
    assert!(!written.contains("hunter2"), "{written}");
    let session = serde_json::to_string(&app.snapshot_session()).unwrap();
    assert!(!session.contains("hunter2"), "{session}");
    let action = format!("{:?}", AppAction::CopySecret(secret("hunter2")));
    assert!(!action.contains("hunter2"), "{action}");
    assert!(action.contains("[redacted]"), "{action}");
}

#[test]
fn a_refusal_is_shown_once_and_leaves_what_was_read_standing() {
    let mut app = key_vault_app();
    assert_eq!(
        app.key_vault
            .set_inventory(Err("run `az login`".to_owned())),
        Some("run `az login`".to_owned())
    );
    assert_eq!(
        app.key_vault
            .set_inventory(Err("run `az login`".to_owned())),
        None,
        "the same refusal twice running is not worth saying twice"
    );
    assert_eq!(
        vault_names(&app),
        vec!["atlas-kv", "labs-kv"],
        "a failed read leaves the last good one on screen"
    );
    assert_eq!(app.key_vault.arm_error(), Some("run `az login`"));

    app.key_vault.set_inventory(Ok(inventory()));
    assert_eq!(app.key_vault.arm_error(), None);
}

#[test]
fn both_cursors_stay_where_they_were_when_a_read_replaces_the_lists() {
    let mut app = key_vault_app();
    app.key_vault.cursor_mut().focus(1);
    assert_eq!(
        app.key_vault.selected_vault().map(|row| row.vault.name),
        Some("labs-kv".to_owned())
    );

    let mut read = inventory();
    read.vaults
        .insert(0, vault("aardvark-kv", "platform", "westeurope"));
    app.key_vault.set_inventory(Ok(read));
    assert_eq!(
        app.key_vault.selected_vault().map(|row| row.vault.name),
        Some("labs-kv".to_owned()),
        "the hand does not move when a vault sorts in front of it"
    );

    let mut app = key_vault_app();
    press(&mut app, KeyCode::Enter);
    app.key_vault.cursor_mut().focus(1);
    assert_eq!(
        app.key_vault.selected_item().map(|row| row.item.name),
        Some("wildcard".to_owned())
    );

    // A read that puts a certificate lapsing sooner in front of it.
    let mut read = items();
    read.push(item(
        ItemKind::Certificate,
        "brand-new",
        true,
        Some(in_days(1)),
    ));
    app.key_vault.set_items("atlas-kv", Ok(read));
    assert_eq!(
        item_names(&app)[1],
        "brand-new",
        "the new certificate sorts above the one the cursor is on"
    );
    assert_eq!(
        app.key_vault.selected_item().map(|row| row.item.name),
        Some("wildcard".to_owned()),
        "the cursor keeps its item by name, wherever it now sorts"
    );
}

#[test]
fn r_inside_a_vault_asks_for_its_items_again() {
    let mut app = key_vault_app();
    press(&mut app, KeyCode::Enter);
    if let Some(ArmFocus::Vault(vault)) = app.key_vault.focus() {
        app.key_vault.set_items(&vault, Ok(Vec::new()));
    }
    assert_eq!(app.key_vault.focus(), None, "the vault's items are held");
    assert_eq!(
        press(&mut app, KeyCode::Char('r')),
        AppAction::Arm(ArmRequest::Refresh)
    );
    assert!(
        matches!(app.key_vault.focus(), Some(ArmFocus::Vault(_))),
        "and asked for again: {:?}",
        app.key_vault.focus()
    );
}

#[test]
fn a_value_is_never_hung_on_a_key_of_the_same_name() {
    let mut app = key_vault_app();
    press(&mut app, KeyCode::Enter);
    app.key_vault.set_items(
        "atlas-kv",
        Ok(vec![
            item(ItemKind::Secret, "dup", true, None),
            item(ItemKind::Key, "dup", true, None),
        ]),
    );
    app.key_vault.set_query("kind:key".to_owned());
    assert_eq!(
        app.key_vault.selected_item().map(|row| row.item.kind),
        Some(ItemKind::Key)
    );
    app.key_vault
        .set_revealed("atlas-kv", "dup", Ok(secret("hunter2")));
    assert!(
        app.key_vault.revealed().is_none(),
        "a key has no value to show, whatever a secret of the same name said"
    );
}

#[test]
fn walking_away_from_a_pending_reveal_stops_waiting_on_it() {
    let mut app = opened_on("db-password");
    assert!(matches!(
        press(&mut app, KeyCode::Char('R')),
        AppAction::Arm(ArmRequest::Reveal { .. })
    ));
    assert!(app.key_vault.busy());
    app.select_tab(TabId::WorkItems);
    assert!(!app.key_vault.reveal_pending());
    assert!(
        !app.key_vault.busy(),
        "nothing waits on a reveal walked away from"
    );
}

#[test]
fn a_disabled_certificate_is_not_counted_as_running_out() {
    let mut app = key_vault_app();
    app.key_vault.set_items(
        "atlas-kv",
        Ok(vec![
            item(ItemKind::Certificate, "live", true, Some(in_days(10))),
            item(ItemKind::Certificate, "dead", false, Some(in_days(10))),
        ]),
    );
    app.key_vault.set_items("labs-kv", Ok(Vec::new()));
    assert_eq!(app.key_vault.expiring_certificates(), 1);
}

#[test]
fn items_with_no_date_sort_last_whichever_way_the_column_is_turned() {
    let mut app = key_vault_app();
    press(&mut app, KeyCode::Enter);
    app.key_vault.set_items(
        "atlas-kv",
        Ok(vec![
            item(ItemKind::Secret, "undated", true, None),
            item(ItemKind::Secret, "soon", true, Some(in_days(5))),
            item(ItemKind::Secret, "later", true, Some(in_days(50))),
        ]),
    );
    let one_way = item_names(&app);
    app.key_vault.toggle_sort("expires");
    let other_way = item_names(&app);
    assert_eq!(
        one_way.last().map(String::as_str),
        Some("undated"),
        "{one_way:?}"
    );
    assert_eq!(
        other_way.last().map(String::as_str),
        Some("undated"),
        "{other_way:?}"
    );
    assert_eq!(one_way[0], other_way[1], "{one_way:?} vs {other_way:?}");
    assert_eq!(one_way[1], other_way[0]);
}
