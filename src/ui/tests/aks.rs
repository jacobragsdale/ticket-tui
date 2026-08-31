//! The AKS tab: the pod table, the details pane, and the tab bar's fifth seat.

use super::*;
use crate::aks::tests::pod;
use crate::app::aks::tests::aks_app;
use crate::app::{Jump, TabId};

/// The tab, drawn, with the AKS tab showing.
fn aks_text(width: u16, height: u16, app: &mut App) -> String {
    app.select_tab(TabId::Aks);
    render_text(width, height, app)
}

#[test]
fn the_bar_seats_a_fifth_tab_at_every_breakpoint_and_the_digit_reaches_it() {
    let mut app = aks_app();
    for width in [120, 90, 60] {
        let text = render_text(width, 30, &mut app);
        let bar = text.lines().next().expect("a tab bar row").to_owned();
        assert!(bar.contains("5 AKS"), "{width} columns: {bar}");
    }
    // Narrower than the five names, the numbers stand alone and stay
    // clickable.
    let narrow = render_text(40, 30, &mut app);
    let bar = narrow.lines().next().expect("a tab bar row");
    assert!(bar.contains(" 5 "), "{bar}");
    assert!(
        app.shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::SelectTab { index: 4 }))
            .is_some(),
        "the fifth tab keeps its click target when the names shorten"
    );

    app.select_tab(TabId::WorkItems);
    app.handle_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE));
    assert_eq!(app.tab, TabId::Aks);
}

#[test]
fn the_table_lists_every_clusters_pods_with_what_each_is_doing() {
    let mut app = aks_app();
    let text = aks_text(140, 24, &mut app);

    assert!(text.contains("Cluster"), "the header names them: {text}");
    assert!(text.contains("Namespace"), "{text}");
    assert!(text.contains("Restarts"), "{text}");
    assert!(text.contains("orders-api-7d9f5b-abc12"), "{text}");
    assert!(text.contains("orders-api-9a1c2d-ghi56"), "{text}");
    assert!(
        text.contains("\u{25cf} Running"),
        "a pod that is up is a filled circle: {text}"
    );
    assert!(
        text.contains("\u{2717} CrashLoopBackOff"),
        "and one in trouble wears the cross: {text}"
    );
    assert!(pane_reads(&text, "Pods", "4 pods"), "{text}");
    let header = text
        .lines()
        .find(|line| line.contains("Cluster"))
        .expect("a header row");
    assert!(
        !header.contains("Node") && !header.contains("Repository"),
        "the node and the repository are off the table until somebody asks: {header}"
    );
}

#[test]
fn typing_a_filter_into_the_search_box_narrows_the_table() {
    let mut app = aks_app();
    aks_text(140, 24, &mut app);

    for character in "/cluster:qa status:crashloopbackoff".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    let text = render_text(140, 24, &mut app);

    assert!(text.contains("orders-api-7d9f5b-def34"), "{text}");
    assert!(!text.contains("orders-api-9a1c2d-ghi56"), "{text}");
    assert!(pane_reads(&text, "Pods", "1 pods"), "{text}");
}

#[test]
fn the_details_pane_heads_the_pod_with_where_it_runs_and_what_is_in_it() {
    let mut app = aks_app();
    let text = aks_text(150, 40, &mut app);

    assert!(text.contains("orders-api-7d9f5b-abc12"), "{text}");
    assert!(text.contains("qa \u{00b7} orders"), "{text}");
    assert!(text.contains("Deployment/orders-api"), "the owner: {text}");
    assert!(text.contains("aks-nodepool1-0"), "the node: {text}");
    assert!(text.contains("10.0.0.7"), "the address: {text}");
    assert!(text.contains("Containers"), "{text}");
    assert!(
        text.contains("myacr.azurecr.io/team/orders-api:1.2.3"),
        "each container names its image: {text}"
    );
}

#[test]
fn the_repository_line_of_a_matched_pod_jumps_to_the_repos_tab() {
    let mut app = aks_app();
    aks_text(150, 40, &mut app);

    let link = app
        .shell
        .hit_regions
        .find_target(
            |target| matches!(target, PointerTarget::Follow(Jump::Repo(name)) if name == "orders-api"),
        )
        .expect("a pod whose image names a repository links to it")
        .rect;

    click(&mut app, link.x + 12, link.y);
    assert_eq!(app.tab, TabId::Repos);
}

#[test]
fn an_unreadable_cluster_says_what_it_said_on_the_table_and_in_the_pane() {
    let mut app = aks_app();
    app.aks.set_pods(
        &app.shell,
        "qa",
        Some("orders"),
        Err("Unable to connect to the server: dial tcp: i/o timeout".to_owned()),
    );
    let text = aks_text(150, 40, &mut app);

    assert!(
        text.contains("qa/orders: Unable to connect"),
        "the border says which cluster and why: {text}"
    );
    assert!(
        text.contains("orders-api-9a1c2d-ghi56"),
        "and the cluster that answered keeps its rows: {text}"
    );

    // With nothing to select at all, the pane is the list of complaints.
    let mut empty = App::new(Vec::new());
    empty
        .aks
        .set_clusters(vec![crate::aks::tests::cluster("qa", &["orders"])]);
    let waiting = aks_text(150, 30, &mut empty);
    assert!(waiting.contains("Reading qa"), "{waiting}");

    empty.aks.set_pods(
        &empty.shell,
        "qa",
        Some("orders"),
        Err("context does not exist".to_owned()),
    );
    let failed = render_text(150, 30, &mut empty);
    assert!(
        failed.contains("qa/orders: context does not exist"),
        "{failed}"
    );

    let mut bare = App::new(Vec::new());
    let unconfigured = aks_text(150, 30, &mut bare);
    assert!(
        unconfigured.contains("No clusters configured"),
        "{unconfigured}"
    );
}

#[test]
fn a_header_click_turns_the_sort_around_and_the_columns_overlay_edits_this_tabs_layout() {
    let mut app = aks_app();
    aks_text(140, 24, &mut app);

    let header = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::SortHeader("name")))
        .expect("the header sorts")
        .rect;
    click(&mut app, header.x, header.y);
    assert_eq!(
        app.aks
            .visible_pods(&app.shell)
            .first()
            .map(|row| row.pod.key.name.clone()),
        Some("orders-api-9a1c2d-ghi56".to_owned()),
        "descending by name puts the last pod first"
    );

    // `c` opens the shared overlay over this tab, and it edits these columns.
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
    let text = render_text(140, 30, &mut app);
    assert!(text.contains("Namespace"), "{text}");
    assert_eq!(app.tab, TabId::Aks, "the overlay opens over the tab");

    let toggle = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::ColumnToggle { index: 2 }))
        .expect("every column can be turned off")
        .rect;
    click(&mut app, toggle.x, toggle.y);
    assert!(
        !app.aks.layout.columns[2].visible,
        "the AKS columns are the ones the overlay edited"
    );
}

#[test]
fn a_pod_with_no_timestamp_still_draws_a_row() {
    let mut app = aks_app();
    let mut ageless = pod("qa", "orders", "orders-api-7d9f5b-zzz99", "Pending");
    ageless.created = None;
    app.aks
        .set_pods(&app.shell, "qa", Some("orders"), Ok(vec![ageless]));

    let text = aks_text(140, 24, &mut app);
    assert!(text.contains("orders-api-7d9f5b-zzz99"), "{text}");
}
