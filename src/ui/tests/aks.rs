//! The AKS tab: the pod table, the details pane, and the tab bar's fifth seat.

use super::*;
use crate::aks::tests::pod;
use crate::app::aks::tests::{aks_app, sidecar_app};
use crate::app::{Jump, TabId};

/// The tab, drawn, with the AKS tab showing.
fn aks_text(width: u16, height: u16, app: &mut App) -> String {
    app.select_tab(TabId::Aks);
    render_text(width, height, app)
}

#[test]
fn the_bar_seats_a_fifth_tab_at_every_breakpoint_and_the_digit_reaches_it() {
    let mut app = aks_app();
    for width in [120, 90, 70] {
        let text = render_text(width, 30, &mut app);
        let bar = text.lines().next().expect("a tab bar row").to_owned();
        assert!(bar.contains("5 AKS"), "{width} columns: {bar}");
    }
    // Narrower than the eight names, the numbers stand alone and stay
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
        &mut app.shell,
        "qa",
        Some("orders"),
        Err("Unable to connect to the server: dial tcp: i/o timeout".to_owned()),
    );
    // Tall enough for the pane's Problems section, under the pod's fields
    // and containers, to be on screen.
    let text = aks_text(150, 60, &mut app);

    assert!(
        text.contains("1 problem"),
        "the border counts the refusal: {text}"
    );
    assert!(
        text.contains("Unable to connect"),
        "and the pane says which cluster and why: {text}"
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
        &mut empty.shell,
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
        .set_pods(&mut app.shell, "qa", Some("orders"), Ok(vec![ageless]));

    let text = aks_text(140, 24, &mut app);
    assert!(text.contains("orders-api-7d9f5b-zzz99"), "{text}");
}

/// The lines the followed pod's log answers with, as `kubectl --timestamps`
/// hands them over.
fn tail(app: &mut App, lines: &[&str], finished: bool) {
    let target = app
        .aks
        .following()
        .cloned()
        .expect("a pod under the cursor");
    app.aks.append_log(
        &target,
        lines.iter().map(|line| (*line).to_owned()).collect(),
        finished,
    );
}

#[test]
fn the_log_pane_tails_the_selected_pod_and_says_what_it_is_following() {
    let mut app = aks_app();
    aks_text(170, 40, &mut app);
    tail(
        &mut app,
        &[
            "2026-08-30T10:00:00Z starting up",
            "2026-08-30T10:00:01Z WARN the disk is filling",
            "2026-08-30T10:00:02Z ERROR connection refused",
        ],
        false,
    );

    let text = render_text(170, 40, &mut app);
    assert!(
        text.contains("orders-api-7d9f5b-abc12 \u{00b7} api \u{00b7} 3 lines"),
        "the title names the pod, the container and the size: {text}"
    );
    assert!(text.contains("following"), "{text}");
    assert!(text.contains("starting up"), "{text}");
    assert!(
        text.contains("10:00:00"),
        "the timestamp is kept, dimmed: {text}"
    );
    assert!(text.contains("connection refused"), "{text}");

    // A stream that has stopped has nothing left to wait for.
    tail(&mut app, &[], true);
    let ended = render_text(170, 40, &mut app);
    assert!(ended.contains("ended"), "{ended}");
}

#[test]
fn scrolling_the_pod_log_leaves_follow_mode_and_end_goes_back_to_it() {
    let mut app = aks_app();
    aks_text(170, 40, &mut app);
    let lines: Vec<String> = (1..=200).map(|line| format!("line {line}")).collect();
    tail(
        &mut app,
        &lines.iter().map(String::as_str).collect::<Vec<_>>(),
        false,
    );
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    let text = render_text(170, 40, &mut app);
    assert!(
        text.contains("line 200"),
        "following shows the tail: {text}"
    );

    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
    let text = render_text(170, 40, &mut app);
    assert!(
        text.contains("scrolled") && !text.contains("line 200"),
        "scrolling up by hand leaves follow mode: {text}"
    );

    app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    let text = render_text(170, 40, &mut app);
    assert!(
        text.contains("following") && text.contains("line 200"),
        "{text}"
    );

    // The wheel over the pane leaves it just as a key does.
    let pane = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::FocusDetails))
        .expect("the text pane takes the wheel")
        .rect;
    app.handle_mouse(mouse(MouseEventKind::ScrollUp, pane.x + 2, pane.y + 2));
    let text = render_text(170, 40, &mut app);
    assert!(text.contains("scrolled"), "{text}");
    assert!(!app.aks.log_following());
}

#[test]
fn d_shows_the_description_in_the_logs_place_and_l_brings_the_log_back() {
    let mut app = aks_app();
    aks_text(170, 40, &mut app);
    tail(&mut app, &["starting up"], false);

    app.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE));
    let waiting = render_text(170, 40, &mut app);
    assert!(
        waiting.contains("Describe \u{00b7} orders-api-7d9f5b-abc12"),
        "{waiting}"
    );
    assert!(waiting.contains("Describing\u{2026}"), "{waiting}");

    let key = app
        .aks
        .selected_pod(&app.shell)
        .expect("a pod")
        .pod
        .key
        .clone();
    app.aks.set_description(
        &key,
        Ok(vec![
            "Name:         orders-api-7d9f5b-abc12".to_owned(),
            "Node:         aks-nodepool1-0".to_owned(),
        ]),
    );
    let described = render_text(170, 40, &mut app);
    assert!(
        described.contains("Name:         orders-api"),
        "{described}"
    );
    assert!(
        !described.contains("starting up"),
        "the log is put aside while describe has the pane: {described}"
    );

    app.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE));
    let back = render_text(170, 40, &mut app);
    assert!(back.contains("starting up"), "{back}");
}

#[test]
fn l_gives_the_text_pane_the_whole_details_pane() {
    let mut app = aks_app();
    aks_text(170, 40, &mut app);
    tail(&mut app, &["starting up"], false);
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    let split = render_text(170, 40, &mut app);
    assert!(
        split.contains("Containers") && split.contains("starting up"),
        "{split}"
    );

    app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    let whole = render_text(170, 40, &mut app);
    assert!(whole.contains("starting up"), "{whole}");
    assert!(
        !whole.contains("Containers"),
        "the pod's own details step aside: {whole}"
    );
}

#[test]
fn clicking_a_container_line_picks_the_one_the_log_follows() {
    let mut app = sidecar_app();
    let text = aks_text(170, 40, &mut app);
    assert!(
        text.contains("\u{203a} api"),
        "the followed container is marked: {text}"
    );

    let line = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::TreeRow { index: 1 }))
        .expect("every container line is clickable")
        .rect;
    click(&mut app, line.x + 4, line.y);

    let text = render_text(170, 40, &mut app);
    assert_eq!(
        app.aks
            .following()
            .and_then(|target| target.container.clone())
            .as_deref(),
        Some("istio-proxy")
    );
    assert!(text.contains("\u{203a} istio-proxy"), "{text}");
    assert!(
        text.contains("orders-api-7d9f5b-abc12 \u{00b7} istio-proxy"),
        "{text}"
    );
}

#[test]
fn the_details_pane_offers_the_four_things_a_pod_answers_to() {
    let mut app = aks_app();
    let text = aks_text(150, 40, &mut app);

    for label in [" Logs ", " Describe ", " Shell ", " Restart "] {
        assert!(text.contains(label), "{label} is a button: {text}");
    }
    assert!(
        text.contains("Repository: orders-api  g"),
        "and the repository line says which key follows it: {text}"
    );

    let name = app
        .aks
        .selected_pod(&app.shell)
        .expect("a pod under the cursor")
        .pod
        .key
        .name;
    let restart = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::RunCommand(CommandId::RestartPod)))
        .expect("the pane offers the restart")
        .rect;
    click(&mut app, restart.x + 1, restart.y);

    let confirm = render_text(150, 40, &mut app);
    assert!(confirm.contains(&format!("Restart {name}?")), "{confirm}");
    assert!(
        confirm.contains("Deployment orders-api replaces it"),
        "the confirmation names what puts a new pod up: {confirm}"
    );
    assert!(
        confirm.contains(" Restart   Leave it "),
        "both answers are chips on one row: {confirm}"
    );
    assert!(
        confirm.contains("x again to restart it"),
        "and the keys stand beside them: {confirm}"
    );
}

#[test]
fn the_restart_confirm_answers_the_mouse_and_leave_it_deletes_nothing() {
    let mut app = aks_app();
    aks_text(150, 40, &mut app);
    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    render_text(150, 40, &mut app);

    let restart = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::RunCommand(CommandId::RestartPod)))
        .expect("the modal confirms");
    assert_eq!(
        restart.layer,
        PointerLayer::Modal,
        "over everything behind it"
    );
    let leave = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::CloseOverlay))
        .expect("and leaves it alone");
    assert_eq!(leave.layer, PointerLayer::Modal);
    let rect = leave.rect;

    let action = click(&mut app, rect.x + 1, rect.y);

    assert_eq!(action, crate::app::AppAction::None, "nothing was sent");
    assert!(app.aks.restarting.is_none());
    assert!(!app.aks.busy());
    let text = render_text(150, 40, &mut app);
    assert!(!text.contains("Restart pod "), "the modal is gone: {text}");
}

#[test]
fn the_restart_confirm_takes_the_whole_pointer() {
    let mut app = aks_app();
    aks_text(150, 40, &mut app);
    // The details pane's own Restart chip, before the modal is up.
    let chip = target_rect(&app, |target| {
        matches!(target, PointerTarget::RunCommand(CommandId::RestartPod))
    });
    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    render_text(150, 40, &mut app);
    // A click where that chip was closes the confirm and deletes nothing:
    // the modal has the whole pointer.
    let action = click(&mut app, chip.x + 1, chip.y);
    assert_eq!(action, crate::app::AppAction::None, "nothing was sent");
    assert_eq!(app.aks.mode, crate::app::aks::AksMode::Browse);
    assert!(app.aks.restarting.is_none());
}

#[test]
fn a_query_that_matches_nothing_says_so_even_while_a_cluster_is_down() {
    let mut app = aks_app();
    app.aks.set_pods(
        &mut app.shell,
        "qa",
        Some("orders"),
        Err("context \"aks-qa\" does not exist".to_owned()),
    );
    app.aks.set_query("nothing-by-this-name".to_owned());
    let text = aks_text(150, 40, &mut app);
    assert!(text.contains("No pods match"), "{text}");
    assert!(
        text.contains("1 problem"),
        "the refusal is a count on the border: {text}"
    );
}
