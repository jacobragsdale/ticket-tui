use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::aks::tests::{cluster, pod};
use crate::app::App;
use crate::app::repos::tests::repo;

/// A pod in trouble, which is what the badge counts and the glyph paints.
fn crashing(cluster: &str, name: &str) -> Pod {
    let mut pod = pod(cluster, "orders", name, "CrashLoopBackOff");
    pod.ready = (0, 1);
    pod.restarts = 9;
    pod
}

/// An app whose AKS tab holds two clusters, four pods, and the repository one
/// of them was built from.
pub(crate) fn aks_app() -> App {
    let mut app = App::new(Vec::new());
    app.shell
        .set_repos(vec![repo("aaa-111", "orders-api", false)]);
    app.repos.set_repos(&app.shell);
    app.aks.set_clusters(vec![
        cluster("qa", &["orders"]),
        cluster("prod", &["orders"]),
    ]);
    app.aks.set_pods(
        &app.shell,
        "qa",
        Some("orders"),
        Ok(vec![
            pod("qa", "orders", "orders-api-7d9f5b-abc12", "Running"),
            crashing("qa", "orders-api-7d9f5b-def34"),
        ]),
    );
    app.aks.set_pods(
        &app.shell,
        "prod",
        Some("orders"),
        Ok(vec![
            pod("prod", "orders", "orders-api-9a1c2d-ghi56", "Running"),
            pod("prod", "orders", "billing-worker-1", "Completed"),
        ]),
    );
    app.select_tab(TabId::Aks);
    app
}

fn names(app: &App) -> Vec<String> {
    app.aks
        .visible_pods(&app.shell)
        .into_iter()
        .map(|row| row.pod.key.name)
        .collect()
}

#[test]
fn every_clusters_pods_share_one_table_and_the_query_narrows_it_by_cluster_and_status() {
    let mut app = aks_app();

    assert_eq!(app.aks.pod_count(), 4);
    let rows = app.aks.visible_pods(&app.shell);
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter()
            .map(|row| row.pod.key.cluster.as_str())
            .collect::<Vec<_>>(),
        ["prod", "prod", "qa", "qa"],
        "cluster ascending is what the table opens on"
    );
    assert_eq!(
        rows.first().and_then(|row| row.repo.clone()),
        Some("orders-api".to_owned()),
        "a pod whose image names a repository on file carries it"
    );

    app.aks
        .set_query("cluster:qa status:crashloopbackoff".to_owned());
    assert_eq!(names(&app), ["orders-api-7d9f5b-def34"]);

    app.aks.set_query("billing".to_owned());
    assert_eq!(
        names(&app),
        ["billing-worker-1"],
        "the rest matches the name"
    );
}

#[test]
fn a_re_read_in_another_order_leaves_the_cursor_on_the_pod_it_was_on() {
    let mut app = aks_app();
    let chosen = names(&app)[3].clone();
    app.aks.cursor.focus(3);

    // The same pods, listed the other way round, plus one that sorts ahead of
    // both and pushes the chosen row down a line.
    app.aks.set_pods(
        &app.shell,
        "qa",
        Some("orders"),
        Ok(vec![
            crashing("qa", "orders-api-7d9f5b-def34"),
            pod("qa", "orders", "orders-api-7d9f5b-abc12", "Running"),
            pod("qa", "orders", "orders-api-7d9f5b-aaa01", "Running"),
        ]),
    );

    assert_eq!(app.aks.pod_count(), 5);
    assert_eq!(app.aks.cursor.index, 4, "the row moved down");
    assert_eq!(
        app.aks
            .selected_pod(&app.shell)
            .map(|row| row.pod.key.name.clone()),
        Some(chosen),
        "the hand stayed on its own pod wherever it now sorts"
    );

    // A read that takes the pod away pulls the cursor back onto the list.
    app.aks.cursor.focus(4);
    app.aks
        .set_pods(&app.shell, "qa", Some("orders"), Ok(Vec::<Pod>::new()));
    assert_eq!(app.aks.pod_count(), 2);
    assert!(app.aks.cursor.index < 2, "{}", app.aks.cursor.index);
}

#[test]
fn an_unreadable_cluster_says_so_once_and_leaves_the_other_clusters_rows() {
    let mut app = aks_app();

    let toast = app.aks.set_pods(
        &app.shell,
        "qa",
        Some("orders"),
        Err("context \"aks-qa\" does not exist".to_owned()),
    );
    assert_eq!(
        toast.as_deref(),
        Some("qa: context \"aks-qa\" does not exist"),
        "the first refusal is worth saying out loud"
    );
    assert_eq!(
        app.aks.errors().len(),
        1,
        "one message per (cluster, namespace)"
    );
    assert_eq!(
        names(&app)
            .iter()
            .filter(|name| name.starts_with("orders-api-9a1c2d"))
            .count(),
        1,
        "the cluster that answered keeps its rows"
    );

    let again = app.aks.set_pods(
        &app.shell,
        "qa",
        Some("orders"),
        Err("context \"aks-qa\" does not exist".to_owned()),
    );
    assert_eq!(again, None, "the same refusal is not said twice");

    // Asking for a read makes the next refusal news again, however old it is.
    app.aks.run_command(&mut app.shell, CommandId::Sync);
    let forced = app.aks.set_pods(
        &app.shell,
        "qa",
        Some("orders"),
        Err("context \"aks-qa\" does not exist".to_owned()),
    );
    assert!(forced.is_some(), "a read the user asked for reports itself");

    let recovered = app.aks.set_pods(
        &app.shell,
        "qa",
        Some("orders"),
        Ok(vec![pod(
            "qa",
            "orders",
            "orders-api-7d9f5b-abc12",
            "Running",
        )]),
    );
    assert_eq!(recovered, None);
    assert!(
        app.aks.errors().is_empty(),
        "a read that worked clears what the last one said"
    );
}

#[test]
fn the_badge_counts_the_pods_somebody_has_to_look_at() {
    let mut app = aks_app();

    assert_eq!(Screen::badge(&app.aks), Some("\u{2717}1".to_owned()));

    app.aks
        .set_pods(&app.shell, "qa", Some("orders"), Ok(Vec::<Pod>::new()));
    assert_eq!(
        Screen::badge(&app.aks),
        None,
        "a tab with nothing wrong wears nothing"
    );
}

#[test]
fn the_agent_context_names_the_selected_pod_and_the_tab_it_is_on() {
    let mut app = aks_app();
    app.aks.set_query("cluster:qa".to_owned());
    app.aks.cursor.focus(1);

    let context = app.agent_context();
    assert_eq!(context.active_tab, "aks");
    assert_eq!(context.aks.clusters, ["qa", "prod"]);
    assert_eq!(context.aks.visible_rows, 2);
    assert_eq!(context.aks.unhealthy, 1);
    let selected = context.aks.selected.expect("a pod under the cursor");
    assert_eq!(selected.name, "orders-api-7d9f5b-def34");
    assert_eq!(selected.cluster, "qa");
    assert_eq!(selected.namespace, "orders");
    assert_eq!(selected.status, "CrashLoopBackOff");
    assert_eq!(selected.ready, "0/1");
    assert_eq!(selected.restarts, 9);
    assert_eq!(selected.owner.as_deref(), Some("Deployment/orders-api"));
    assert_eq!(selected.repo.as_deref(), Some("orders-api"));
    assert_eq!(selected.containers.len(), 1);
    assert_eq!(selected.containers[0].name, "api");
}

#[test]
fn the_session_puts_back_the_query_the_sort_and_the_columns() {
    let mut app = aks_app();
    app.aks.set_query("status:running".to_owned());
    app.aks.toggle_sort("restarts");
    app.aks.layout.columns[0].width = 33;
    let session = app.snapshot_session();

    let mut reopened = App::new(Vec::new());
    reopened.restore_session(session);

    assert_eq!(reopened.tab, TabId::Aks);
    assert_eq!(reopened.aks.query(), "status:running");
    assert_eq!(reopened.aks.sort.0, PodColumn::Restarts);
    assert_eq!(reopened.aks.layout.columns[0].width, 33);
}

#[test]
fn the_sync_key_asks_the_cluster_worker_to_read_again() {
    let mut app = aks_app();

    let action = app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));

    assert_eq!(action, AppAction::Aks(AksRequest::Refresh));
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("Reading pods\u{2026}")
    );
}

#[test]
fn a_cluster_the_file_no_longer_names_takes_its_pods_with_it() {
    let mut app = aks_app();

    app.aks.set_clusters(vec![cluster("qa", &["orders"])]);

    assert_eq!(app.aks.pod_count(), 2);
    assert!(
        names(&app)
            .iter()
            .all(|name| name.starts_with("orders-api-7d9f5b")),
        "{:?}",
        names(&app)
    );
}
