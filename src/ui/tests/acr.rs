//! The ACR tab: the registry table, the repositories under one, and the tags
//! and manifest the details pane draws.

use super::*;
use crate::app::acr::tests::acr_app;
use crate::app::{Focus, TabId};
use crate::arm::Manifest;
use crate::timestamp::ts;

/// The tab, drawn, with the ACR tab showing.
fn acr_text(width: u16, height: u16, app: &mut App) -> String {
    app.select_tab(TabId::Acr);
    render_text(width, height, app)
}

/// The app with the manifest of the tag the pane opens on already read.
fn with_manifest() -> App {
    let mut app = acr_app();
    app.acr.set_manifest(
        "atlas",
        "team/api",
        "sha256:aaaaaaaaaaaaaaaa",
        Ok(Manifest {
            digest: "sha256:aaaaaaaaaaaaaaaa".to_owned(),
            size: Some(41_234_567),
            created: Some(ts("2026-08-29T09:00:00Z")),
            architecture: "amd64".to_owned(),
            os: "linux".to_owned(),
        }),
    );
    app
}

fn open_repositories(app: &mut App) {
    app.select_tab(TabId::Acr);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
}

#[test]
fn the_table_lists_every_registry_with_where_it_lives() {
    let mut app = acr_app();
    let text = acr_text(140, 24, &mut app);

    assert!(
        text.contains("Resource group"),
        "the header names it: {text}"
    );
    assert!(text.contains("SKU"), "{text}");
    assert!(text.contains("Location"), "{text}");
    assert!(text.contains("atlas"), "{text}");
    assert!(text.contains("sandbox"), "{text}");
    assert!(text.contains("westeurope"), "{text}");
    assert!(pane_reads(&text, "Registries", "2 registries"), "{text}");

    // The details pane describes the one under the cursor, and the portal is
    // one line of it.
    assert!(text.contains("atlas.azurecr.io"), "{text}");
    assert!(text.contains("Portal:"), "{text}");
    assert!(text.contains("portal.azure.com"), "{text}");
}

#[test]
fn the_repositories_level_says_how_far_through_the_attributes_the_worker_is() {
    let mut app = acr_app();
    // A catalog whose attributes are still landing says so on the border.
    app.acr.set_repositories(
        "atlas",
        Ok(vec![
            crate::arm::Repository {
                name: "team/api".to_owned(),
                tags: Some(4),
                manifests: Some(4),
                updated: Some(ts("2026-08-29T09:00:00Z")),
            },
            crate::arm::Repository {
                name: "team/web".to_owned(),
                tags: None,
                manifests: None,
                updated: None,
            },
        ]),
    );
    open_repositories(&mut app);
    let text = render_text(140, 24, &mut app);

    assert!(text.contains("team/api"), "{text}");
    assert!(text.contains("team/web"), "{text}");
    assert!(
        text.contains("1 of 2 read"),
        "the border counts the attributes calls in: {text}"
    );

    // Once every one has landed the count goes away again.
    app.acr.set_repository(
        "atlas",
        Ok(crate::arm::Repository {
            name: "team/web".to_owned(),
            tags: Some(2),
            manifests: Some(2),
            updated: Some(ts("2026-08-28T09:00:00Z")),
        }),
    );
    let settled = render_text(140, 24, &mut app);
    assert!(!settled.contains("of 2 read"), "{settled}");
    assert!(pane_reads(&settled, "atlas", "2 repositories"), "{settled}");
}

#[test]
fn the_details_pane_draws_the_tags_newest_first_and_the_manifest_under_them() {
    let mut app = with_manifest();
    open_repositories(&mut app);
    let text = render_text(140, 30, &mut app);

    assert!(text.contains("Repository"), "the pane names itself: {text}");
    assert!(text.contains("atlas.azurecr.io/team/api"), "{text}");
    assert!(text.contains("Tags"), "{text}");
    assert!(text.contains("latest"), "{text}");
    assert!(text.contains("2026.8.1"), "{text}");
    assert!(
        text.contains("aaaaaaaaaaaa"),
        "a digest reads as its first twelve characters: {text}"
    );
    assert!(text.contains("Manifest"), "{text}");
    assert!(text.contains("linux/amd64"), "{text}");
    assert!(text.contains("39.3 MB"), "{text}");
    // The three chips, and the keys they stand for.
    assert!(text.contains("Copy pull"), "{text}");
    assert!(text.contains("Copy digest"), "{text}");
}

#[test]
fn the_chips_and_the_tag_rows_run_what_the_keys_do() {
    let mut app = with_manifest();
    open_repositories(&mut app);
    render_text(140, 30, &mut app);

    let chip = |app: &App, id: CommandId| {
        app.shell
            .hit_regions
            .find_target(
                |target| matches!(target, PointerTarget::RunCommand(found) if *found == id),
            )
            .unwrap_or_else(|| panic!("{id:?} has a chip"))
            .rect
    };
    let pull = chip(&app, CommandId::CopyId);
    match click(&mut app, pull.x + 1, pull.y) {
        crate::app::AppAction::Copy { text, .. } => {
            assert_eq!(text, "atlas.azurecr.io/team/api:latest");
        }
        other => panic!("the pull chip gave {other:?}"),
    }
    render_text(140, 30, &mut app);
    let digest = chip(&app, CommandId::CopyDigest);
    match click(&mut app, digest.x + 1, digest.y) {
        crate::app::AppAction::Copy { text, .. } => {
            assert_eq!(text, "sha256:aaaaaaaaaaaaaaaa");
        }
        other => panic!("the digest chip gave {other:?}"),
    }

    // A click on the second tag line moves the pane's own cursor onto it.
    render_text(140, 30, &mut app);
    let row = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::TreeRow { index: 1 }))
        .expect("the second tag has a row")
        .rect;
    click(&mut app, row.x + 2, row.y);
    assert_eq!(app.shell.focus, Focus::Details);
    assert_eq!(
        app.acr.selected_tag().map(|tag| tag.name).as_deref(),
        Some("2026.8.1")
    );

    // And the portal line up top opens the registry.
    app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
    render_text(140, 30, &mut app);
    let portal = chip(&app, CommandId::Open);
    assert!(matches!(
        click(&mut app, portal.x + 1, portal.y),
        crate::app::AppAction::OpenUrl(url) if url.ends_with("/registries/atlas")
    ));
}

#[test]
fn the_pane_says_why_when_there_is_nothing_to_read() {
    let mut app = App::new(Vec::new());
    app.shell.set_arm_state(Some(
        "no Azure subscription: pass --subscription".to_owned(),
    ));
    let text = acr_text(120, 20, &mut app);
    assert!(text.contains("no Azure subscription"), "{text}");
    assert!(pane_reads(&text, "Registries", "0 registries"), "{text}");
}
