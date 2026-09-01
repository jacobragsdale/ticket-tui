use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::app::App;
use crate::arm::Vault;
use crate::timestamp::ts;

pub(crate) fn registry(name: &str, group: &str, location: &str, sku: &str) -> Registry {
    Registry {
        id: format!(
            "/subscriptions/sub-1/resourceGroups/{group}/providers/Microsoft.ContainerRegistry/registries/{name}"
        ),
        name: name.to_owned(),
        resource_group: group.to_owned(),
        location: location.to_owned(),
        sku: sku.to_owned(),
        login_server: format!("{name}.azurecr.io"),
    }
}

fn repository(name: &str, tags: Option<u64>, updated: Option<&str>) -> Repository {
    Repository {
        name: name.to_owned(),
        tags,
        manifests: tags,
        updated: updated.map(ts),
    }
}

fn tag(name: &str, digest: &str, created: &str) -> Tag {
    Tag {
        name: name.to_owned(),
        digest: digest.to_owned(),
        created: Some(ts(created)),
        updated: Some(ts(created)),
    }
}

fn inventory() -> Inventory {
    Inventory {
        registries: vec![
            registry("atlas", "platform", "westeurope", "Premium"),
            registry("sandbox", "labs", "northeurope", "Basic"),
        ],
        vaults: vec![Vault {
            id: "/subscriptions/sub-1/resourceGroups/platform/providers/Microsoft.KeyVault/vaults/atlas-kv".to_owned(),
            name: "atlas-kv".to_owned(),
            resource_group: "platform".to_owned(),
            location: "westeurope".to_owned(),
            sku: "standard".to_owned(),
            uri: "https://atlas-kv.vault.azure.net/".to_owned(),
        }],
    }
}

/// An app whose ACR tab holds two registries, one of them with a catalog and
/// one repository's tags read.
pub(crate) fn acr_app() -> App {
    let mut app = App::new(Vec::new());
    app.acr.set_inventory(Ok(inventory()));
    app.acr.set_repositories(
        "atlas",
        Ok(vec![
            repository("team/api", Some(4), Some("2026-08-29T09:00:00Z")),
            repository("team/web", Some(2), Some("2026-08-28T09:00:00Z")),
        ]),
    );
    app.acr.set_tags(
        "atlas",
        "team/api",
        Ok(vec![
            tag(
                "2026.8.1",
                "sha256:bbbbbbbbbbbbbbbb",
                "2026-08-20T09:00:00Z",
            ),
            tag("latest", "sha256:aaaaaaaaaaaaaaaa", "2026-08-29T09:00:00Z"),
        ]),
    );
    app.select_tab(TabId::Acr);
    app
}

fn press(app: &mut App, code: KeyCode) -> AppAction {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn registry_names(app: &App) -> Vec<String> {
    app.acr
        .visible_registries()
        .into_iter()
        .map(|row| row.registry.name)
        .collect()
}

fn repository_names(app: &App) -> Vec<String> {
    app.acr
        .visible_repositories()
        .into_iter()
        .map(|row| row.repository.name)
        .collect()
}

#[test]
fn the_table_lists_every_registry_the_subscription_holds_and_the_query_narrows_it() {
    let mut app = acr_app();
    assert_eq!(registry_names(&app), vec!["atlas", "sandbox"]);
    assert_eq!(
        app.acr.vaults().len(),
        1,
        "the one query answers for both tabs, so the vaults are kept"
    );

    app.acr.set_query("rg:labs".to_owned());
    assert_eq!(registry_names(&app), vec!["sandbox"]);
    app.acr.set_query("location:westeurope".to_owned());
    assert_eq!(registry_names(&app), vec!["atlas"]);
    app.acr.set_query("sku:basic".to_owned());
    assert_eq!(registry_names(&app), vec!["sandbox"]);
    // The fuzzy half reads the login server as well as the name.
    app.acr.set_query("atlas.azurecr".to_owned());
    assert_eq!(registry_names(&app), vec!["atlas"]);
    app.acr.set_query(String::new());

    // A registry with no catalog read has no count to show yet.
    let counts: Vec<Option<usize>> = app
        .acr
        .visible_registries()
        .into_iter()
        .map(|row| row.repositories)
        .collect();
    assert_eq!(counts, vec![Some(2), None]);
}

#[test]
fn enter_drills_into_a_registry_and_h_comes_back_with_the_level_one_query_intact() {
    let mut app = acr_app();
    app.acr.set_query("name:atlas".to_owned());
    press(&mut app, KeyCode::Enter);

    assert_eq!(*app.acr.level(), Level::Repositories("atlas".to_owned()));
    assert_eq!(repository_names(&app), vec!["team/api", "team/web"]);
    assert!(
        app.acr.query().is_empty(),
        "the repositories level opens on its own query"
    );
    app.acr.set_query("web".to_owned());
    assert_eq!(repository_names(&app), vec!["team/web"]);

    press(&mut app, KeyCode::Char('h'));
    assert_eq!(*app.acr.level(), Level::Registries);
    assert_eq!(
        app.acr.query(),
        "name:atlas",
        "each level keeps the query it was left with"
    );

    // And back down again: the repositories level kept its own.
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.acr.query(), "web");
}

#[test]
fn a_repositorys_tags_and_manifest_land_in_the_details_pane() {
    let mut app = acr_app();
    press(&mut app, KeyCode::Enter);
    app.acr.sync_focus();

    let names: Vec<String> = app
        .acr
        .shown_tags()
        .into_iter()
        .map(|tag| tag.name)
        .collect();
    assert_eq!(
        names,
        vec!["latest", "2026.8.1"],
        "newest first, whatever order they came in"
    );
    assert!(app.acr.shown_manifest().is_none());

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
    let manifest = app.acr.shown_manifest().expect("the manifest landed");
    assert_eq!(manifest.size, Some(41_234_567));
    assert_eq!(manifest.os, "linux");

    // The tag cursor moves with j and k once the details pane has the focus.
    app.shell.focus = Focus::Details;
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(
        app.acr.selected_tag().map(|tag| tag.name).as_deref(),
        Some("2026.8.1")
    );
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(
        app.acr.selected_tag().map(|tag| tag.name).as_deref(),
        Some("latest")
    );
}

#[test]
fn y_copies_the_pull_reference_and_d_the_digest() {
    let mut app = acr_app();
    // Up top there is no repository, so the login server is the reference.
    match press(&mut app, KeyCode::Char('y')) {
        AppAction::Copy { text, content } => {
            assert_eq!(text, "atlas.azurecr.io");
            assert_eq!(content, CopiedContent::Id);
        }
        other => panic!("y up top gave {other:?}"),
    }

    press(&mut app, KeyCode::Enter);
    app.acr.sync_focus();
    match press(&mut app, KeyCode::Char('y')) {
        AppAction::Copy { text, .. } => {
            assert_eq!(text, "atlas.azurecr.io/team/api:latest");
        }
        other => panic!("y on a tag gave {other:?}"),
    }
    match press(&mut app, KeyCode::Char('D')) {
        AppAction::Copy { text, .. } => assert_eq!(text, "sha256:aaaaaaaaaaaaaaaa"),
        other => panic!("D gave {other:?}"),
    }

    // A repository whose tags nobody has read yet is copied without one.
    app.acr.cursor_mut().focus(1);
    app.acr.sync_focus();
    match press(&mut app, KeyCode::Char('y')) {
        AppAction::Copy { text, .. } => assert_eq!(text, "atlas.azurecr.io/team/web"),
        other => panic!("y on a repository with no tags gave {other:?}"),
    }
}

#[test]
fn both_cursors_stay_where_they_were_when_a_read_replaces_the_lists() {
    let mut app = acr_app();
    app.acr.cursor_mut().focus(1);
    assert_eq!(
        app.acr.selected_registry().map(|row| row.registry.name),
        Some("sandbox".to_owned())
    );

    // A read that puts a new registry in front of it: the hand does not move.
    let mut read = inventory();
    read.registries.insert(
        0,
        registry("aardvark", "platform", "westeurope", "Standard"),
    );
    app.acr.set_inventory(Ok(read));
    assert_eq!(registry_names(&app), vec!["aardvark", "atlas", "sandbox"]);
    assert_eq!(
        app.acr.selected_registry().map(|row| row.registry.name),
        Some("sandbox".to_owned())
    );

    app.acr.cursor_mut().focus(1);
    press(&mut app, KeyCode::Enter);
    app.acr.cursor_mut().focus(1);
    assert_eq!(
        app.acr.selected_repository().map(|row| row.repository.name),
        Some("team/web".to_owned())
    );
    app.acr.set_repositories(
        "atlas",
        Ok(vec![
            repository("team/api", Some(4), Some("2026-08-29T09:00:00Z")),
            repository("team/db", Some(9), Some("2026-08-30T09:00:00Z")),
            repository("team/web", Some(2), Some("2026-08-28T09:00:00Z")),
        ]),
    );
    assert_eq!(
        app.acr.selected_repository().map(|row| row.repository.name),
        Some("team/web".to_owned()),
        "the cursor keeps its repository by name, wherever it now sorts"
    );
}

#[test]
fn r_asks_the_worker_for_a_fresh_read_and_o_opens_the_registry_in_the_portal() {
    let mut app = acr_app();
    assert_eq!(
        press(&mut app, KeyCode::Char('r')),
        AppAction::Arm(ArmRequest::Refresh)
    );
    match press(&mut app, KeyCode::Char('o')) {
        AppAction::OpenUrl(url) => {
            assert_eq!(
                url,
                "https://portal.azure.com/#resource/subscriptions/sub-1/resourceGroups/platform/providers/Microsoft.ContainerRegistry/registries/atlas"
            );
        }
        other => panic!("o gave {other:?}"),
    }

    // Down a level, `o` still opens the registry the repositories belong to.
    press(&mut app, KeyCode::Enter);
    assert!(matches!(
        press(&mut app, KeyCode::Char('o')),
        AppAction::OpenUrl(url) if url.ends_with("/registries/atlas")
    ));
}

#[test]
fn the_focus_walks_from_the_catalog_to_the_tags_to_the_manifest_and_then_rests() {
    let mut app = App::new(Vec::new());
    app.acr.set_inventory(Ok(inventory()));
    app.select_tab(TabId::Acr);
    assert_eq!(
        app.acr.focus(),
        None,
        "a registry under the cursor up top is nothing to read"
    );

    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.acr.focus(),
        Some(ArmFocus::Registry("atlas".to_owned())),
        "the open level wants the catalog"
    );
    assert!(app.acr.busy());

    app.acr
        .set_repositories("atlas", Ok(vec![repository("team/api", None, None)]));
    app.acr.sync_focus();
    assert_eq!(
        app.acr.focus(),
        Some(ArmFocus::Repository {
            registry: "atlas".to_owned(),
            name: "team/api".to_owned(),
        })
    );

    app.acr.set_tags(
        "atlas",
        "team/api",
        Ok(vec![tag(
            "latest",
            "sha256:aaaaaaaaaaaaaaaa",
            "2026-08-29T09:00:00Z",
        )]),
    );
    assert_eq!(
        app.acr.focus(),
        Some(ArmFocus::Tag {
            registry: "atlas".to_owned(),
            repo: "team/api".to_owned(),
            digest: "sha256:aaaaaaaaaaaaaaaa".to_owned(),
        })
    );

    app.acr.set_manifest(
        "atlas",
        "team/api",
        "sha256:aaaaaaaaaaaaaaaa",
        Err("the registry refused".to_owned()),
    );
    assert_eq!(
        app.acr.focus(),
        None,
        "a refusal is an answer: nothing asks for it again"
    );
    assert_eq!(app.acr.arm_error(), Some("the registry refused"));
    assert!(!app.acr.busy(), "a refusal standing stops the spinner");
}

#[test]
fn a_registry_and_a_repository_are_both_places_to_jump_to_and_come_back_from() {
    let mut app = acr_app();
    press(&mut app, KeyCode::Enter);
    app.acr.cursor_mut().focus(1);
    let here = app.acr.here(&app.shell).expect("a place to come back to");
    assert_eq!(
        here,
        Jump::Repository {
            registry: "atlas".to_owned(),
            name: "team/web".to_owned(),
        }
    );
    // The session writes these out and reads them back.
    let written = serde_json::to_string(&here).unwrap();
    assert_eq!(
        serde_json::from_str::<Jump>(&written).unwrap(),
        here,
        "{written}"
    );
    assert!(here.describe().contains("team/web"));

    app.select_tab(TabId::WorkItems);
    assert!(app.follow(&Jump::Registry("sandbox".to_owned())));
    assert_eq!(app.tab, TabId::Acr);
    assert_eq!(*app.acr.level(), Level::Registries);
    assert_eq!(
        app.acr.selected_registry().map(|row| row.registry.name),
        Some("sandbox".to_owned())
    );

    // A jump beats a filter: a query hiding the target is cleared rather than
    // reported as a missing row.
    app.acr.set_query("name:sandbox".to_owned());
    assert!(app.follow(&here));
    assert_eq!(*app.acr.level(), Level::Repositories("atlas".to_owned()));
    assert_eq!(
        app.acr.selected_repository().map(|row| row.repository.name),
        Some("team/web".to_owned())
    );

    app.select_tab(TabId::WorkItems);
    assert!(
        !app.follow(&Jump::Registry("nowhere".to_owned())),
        "a registry the subscription does not hold is not switched to"
    );
    assert_eq!(app.tab, TabId::WorkItems);
}

#[test]
fn the_agent_context_names_the_registry_the_repository_and_the_tag() {
    let mut app = acr_app();
    let top = app.acr.agent_context();
    assert_eq!(top.level, "registries");
    assert_eq!(top.visible_rows, 2);
    let selected = top.selected_registry.expect("a registry under the cursor");
    assert_eq!(selected.name, "atlas");
    assert_eq!(selected.resource_group, "platform");
    assert_eq!(selected.sku, "Premium");
    assert_eq!(selected.location, "westeurope");
    assert_eq!(selected.login_server, "atlas.azurecr.io");
    assert!(selected.portal_url.contains("portal.azure.com"));
    assert!(top.selected_repository.is_none());

    press(&mut app, KeyCode::Enter);
    app.acr.sync_focus();
    let down = app.acr.agent_context();
    assert_eq!(down.level, "repositories");
    assert_eq!(down.visible_rows, 2);
    let repository = down.selected_repository.expect("a repository");
    assert_eq!(repository.name, "team/api");
    assert_eq!(repository.tags, Some(4));
    assert!(repository.updated.is_some());
    let tag = down.selected_tag.expect("a tag");
    assert_eq!(tag.name, "latest");
    assert_eq!(tag.digest, "sha256:aaaaaaaaaaaaaaaa");
}

#[test]
fn a_refusal_is_shown_once_and_leaves_what_was_read_standing() {
    let mut app = acr_app();
    assert_eq!(
        app.acr.set_inventory(Err("run `az login`".to_owned())),
        Some("run `az login`".to_owned())
    );
    assert_eq!(
        app.acr.set_inventory(Err("run `az login`".to_owned())),
        None,
        "the same refusal twice running is not worth saying twice"
    );
    assert_eq!(
        registry_names(&app),
        vec!["atlas", "sandbox"],
        "a failed read leaves the last good one on screen"
    );
    assert_eq!(app.acr.arm_error(), Some("run `az login`"));

    // Anything read clears it again.
    app.acr.set_inventory(Ok(inventory()));
    assert_eq!(app.acr.arm_error(), None);
}

#[test]
fn r_inside_a_registry_asks_for_its_catalog_again() {
    let mut app = acr_app();
    press(&mut app, KeyCode::Enter);
    app.acr.set_repositories(
        "atlas",
        Ok(vec![repository(
            "team/api",
            Some(3),
            Some("2026-08-29T09:00:00Z"),
        )]),
    );
    app.acr.sync_focus();
    assert_ne!(
        app.acr.focus(),
        Some(ArmFocus::Registry("atlas".to_owned())),
        "the catalog is held"
    );
    assert_eq!(
        press(&mut app, KeyCode::Char('r')),
        AppAction::Arm(ArmRequest::Refresh)
    );
    assert_eq!(
        app.acr.focus(),
        Some(ArmFocus::Registry("atlas".to_owned())),
        "and asked for again"
    );
}

#[test]
fn repositories_with_no_stamp_sort_last_whichever_way_the_column_is_turned() {
    let mut app = acr_app();
    press(&mut app, KeyCode::Enter);
    app.acr.set_repositories(
        "atlas",
        Ok(vec![
            repository("team/unread", None, None),
            repository("team/old", Some(1), Some("2026-01-01T00:00:00Z")),
            repository("team/new", Some(2), Some("2026-08-01T00:00:00Z")),
        ]),
    );
    app.acr.toggle_sort("updated");
    let one_way = repository_names(&app);
    app.acr.toggle_sort("updated");
    let other_way = repository_names(&app);
    assert_eq!(
        one_way.last().map(String::as_str),
        Some("team/unread"),
        "{one_way:?}"
    );
    assert_eq!(
        other_way.last().map(String::as_str),
        Some("team/unread"),
        "{other_way:?}"
    );
    assert_eq!(one_way[0], other_way[1], "{one_way:?} vs {other_way:?}");
}

#[test]
fn g_goes_to_the_pod_running_the_tag_the_details_pane_is_on() {
    use crate::aks::tests::{cluster, pod};
    use crate::app::{Jump, Screen};

    let mut app = acr_app();
    app.aks
        .set_clusters(vec![cluster("qa", &["orders"])], &mut app.shell);
    let mut running = pod("qa", "orders", "api-7d9f5b-abc12", "Running");
    running.containers[0].image = "atlas.azurecr.io/team/api:latest".to_owned();
    app.aks
        .set_pods(&mut app.shell, "qa", Some("orders"), Ok(vec![running]));
    app.select_tab(TabId::Acr);

    press(&mut app, KeyCode::Enter);
    app.acr.sync_focus();
    assert_eq!(
        Screen::follow_target(&app.acr, &app.shell),
        Ok((
            Jump::Pod(crate::aks::PodKey {
                cluster: "qa".to_owned(),
                namespace: "orders".to_owned(),
                name: "api-7d9f5b-abc12".to_owned(),
            }),
            "pod"
        ))
    );
    press(&mut app, KeyCode::Char('g'));
    assert_eq!(app.tab, TabId::Aks);
    assert_eq!(
        app.aks.selected_pod(&app.shell).map(|row| row.pod.key.name),
        Some("api-7d9f5b-abc12".to_owned())
    );

    // The tag below it is not running anywhere the clusters have been read.
    app.select_tab(TabId::Acr);
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(press(&mut app, KeyCode::Char('g')), AppAction::None);
    assert_eq!(app.tab, TabId::Acr);
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("No pod runs team/api:2026.8.1")
    );
}
