use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::aks::Container;
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

/// The one qa pod, with a sidecar beside it, so `C` and a click on a container
/// line have somewhere to go. The cursor is left on it.
pub(crate) fn sidecar_app() -> App {
    let mut app = aks_app();
    let mut sidecar = pod("qa", "orders", "orders-api-7d9f5b-abc12", "Running");
    sidecar.containers.push(Container {
        name: "istio-proxy".to_owned(),
        image: "docker.io/istio/proxyv2:1.20".to_owned(),
        ready: true,
        restarts: 0,
        state: "Running".to_owned(),
        last_termination: None,
    });
    app.aks
        .set_pods(&app.shell, "qa", Some("orders"), Ok(vec![sidecar]));
    let index = names(&app)
        .iter()
        .position(|name| name == "orders-api-7d9f5b-abc12")
        .expect("the pod with the sidecar is on the table");
    app.aks.cursor.focus(index);
    settle(&mut app);
    app
}

/// The pane settled on whatever the cursor is over, which is what the draw and
/// the run's poll both do before anything is followed.
fn settle(app: &mut App) {
    app.aks.sync_focus(&app.shell);
}

/// What the pane is following, which is what lines have to be addressed to.
fn target(app: &App) -> LogFollow {
    app.aks
        .following()
        .cloned()
        .expect("a pod under the cursor")
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

#[test]
fn the_log_pane_holds_the_lines_of_the_stream_it_is_on_and_drops_any_others() {
    let mut app = aks_app();
    settle(&mut app);
    let following = target(&app);

    app.aks
        .append_log(&following, vec!["starting".to_owned()], false);
    app.aks
        .append_log(&following, vec!["listening".to_owned()], false);
    assert_eq!(app.aks.log_lines(), ["starting", "listening"]);
    assert!(app.aks.log_following());
    assert!(!app.aks.log_ended());

    // Another pod's stream, still in flight when the pane moved on.
    let stale = LogFollow {
        key: PodKey {
            cluster: "prod".to_owned(),
            namespace: "orders".to_owned(),
            name: "orders-api-9a1c2d-ghi56".to_owned(),
        },
        container: None,
        previous: false,
    };
    app.aks
        .append_log(&stale, vec!["not mine".to_owned()], false);
    assert_eq!(app.aks.log_lines(), ["starting", "listening"]);

    app.aks.append_log(&following, Vec::new(), true);
    assert!(app.aks.log_ended(), "the stream said it was over");
}

#[test]
fn a_pod_log_past_the_cap_keeps_the_tail_and_says_how_much_it_dropped() {
    let mut app = aks_app();
    settle(&mut app);
    let following = target(&app);

    let lines: Vec<String> = (1..=20_010).map(|line| format!("line {line}")).collect();
    app.aks.append_log(&following, lines, false);

    let held = app.aks.log_lines();
    assert_eq!(held.len(), 20_000, "the cap holds");
    assert!(
        held[0].contains("earlier lines skipped"),
        "and says what went: {}",
        held[0]
    );
    assert_eq!(held.last().map(String::as_str), Some("line 20010"));
}

#[test]
fn moving_the_cursor_to_another_pod_starts_that_pods_log_from_nothing() {
    let mut app = aks_app();
    settle(&mut app);
    let following = target(&app);
    app.aks
        .append_log(&following, vec!["starting".to_owned()], false);

    app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));

    assert!(
        app.aks.log_lines().is_empty(),
        "the lines were the last pod's"
    );
    assert_ne!(
        target(&app).key,
        following.key,
        "and the stream moved with it"
    );
}

#[test]
fn c_follows_the_next_container_and_a_pod_with_one_says_so() {
    let mut app = sidecar_app();
    assert_eq!(
        target(&app).container,
        None,
        "the first, until one is chosen"
    );
    let following = target(&app);
    app.aks
        .append_log(&following, vec!["starting".to_owned()], false);

    app.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE));

    assert_eq!(target(&app).container.as_deref(), Some("istio-proxy"));
    assert!(
        app.aks.log_lines().is_empty(),
        "another container is another stream"
    );

    // Round again, and the pane is back on the first.
    app.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE));
    assert_eq!(target(&app).container.as_deref(), Some("api"));

    let mut alone = aks_app();
    settle(&mut alone);
    let name = target(&alone).key.name;
    alone.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE));
    assert_eq!(
        alone.shell.notification().map(|(text, _)| text),
        Some(format!("{name} has one container").as_str())
    );
    assert_eq!(target(&alone).container, None);
}

#[test]
fn p_asks_for_the_log_from_before_the_last_restart() {
    let mut app = aks_app();
    settle(&mut app);
    let following = target(&app);
    app.aks
        .append_log(&following, vec!["starting".to_owned()], false);

    app.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE));

    assert!(target(&app).previous);
    assert!(
        app.aks.log_lines().is_empty(),
        "the run before the last restart is another stream"
    );

    app.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE));
    assert!(!target(&app).previous);
}

#[test]
fn d_asks_for_the_description_and_l_puts_the_log_back() {
    let mut app = aks_app();
    settle(&mut app);
    let key = target(&app).key;

    let action = app.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE));

    assert_eq!(action, AppAction::Aks(AksRequest::Describe(key.clone())));
    assert_eq!(app.aks.pane(), PaneText::Describe);
    assert!(app.aks.busy(), "a describe a person is waiting on spins");
    assert!(app.aks.describe_lines().is_none());

    app.aks
        .set_description(&key, Ok(vec![format!("Name:  {}", key.name)]));
    assert!(!app.aks.busy());
    assert_eq!(app.aks.pane(), PaneText::Describe);
    assert!(matches!(app.aks.describe_lines(), Some(Ok(lines)) if lines.len() == 1));

    app.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE));
    assert_eq!(app.aks.pane(), PaneText::Log);
    assert_eq!(app.shell.focus, Focus::Details, "and the pane has the keys");

    // A description belongs to the pod it was asked about.
    app.shell.focus_list();
    app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    assert!(app.aks.describe_lines().is_none());
}

#[test]
fn the_agent_context_says_which_log_the_pane_is_following() {
    let mut app = sidecar_app();
    app.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE));
    let following = target(&app);
    app.aks
        .append_log(&following, vec!["starting".to_owned()], false);

    let context = app.agent_context();
    let log = context.aks.following_log.expect("a log under the cursor");
    assert_eq!(log.pod, "orders-api-7d9f5b-abc12");
    assert_eq!(log.container.as_deref(), Some("istio-proxy"));
    assert!(!log.previous);
    assert_eq!(log.line_count, 1);
    assert!(log.following);
}

/// One key, pressed on whatever tab is showing.
fn press(app: &mut App, code: KeyCode) -> AppAction {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// A pod nothing put there: a shell somebody left running, which a delete
/// would take away for good rather than restart.
fn bare(name: &str) -> Pod {
    let mut pod = pod("qa", "orders", name, "Running");
    pod.owner = None;
    pod
}

/// Moves the cursor onto one pod by name, the way a person would.
fn select(app: &mut App, name: &str) {
    let index = names(app)
        .iter()
        .position(|held| held == name)
        .expect("the pod is on the table");
    app.aks.cursor.focus(index);
    settle(app);
}

#[test]
fn x_asks_once_more_and_the_second_x_sends_the_delete_that_restarts_the_pod() {
    let mut app = aks_app();
    settle(&mut app);
    let key = target(&app).key;

    assert_eq!(press(&mut app, KeyCode::Char('x')), AppAction::None);
    assert_eq!(app.aks.mode, AksMode::ConfirmRestart, "it asks first");
    assert_eq!(app.aks.restarting.as_ref(), Some(&key));
    assert!(!app.aks.busy(), "and nothing is out yet");

    let action = press(&mut app, KeyCode::Char('x'));

    assert_eq!(action, AppAction::Aks(AksRequest::Delete(key.clone())));
    assert_eq!(app.aks.mode, AksMode::Browse);
    assert!(app.aks.restarting.is_none());
    assert!(app.aks.busy(), "a delete a person is waiting on spins");
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some(format!("Restarting {}\u{2026}", key.name).as_str())
    );

    app.aks.delete_answered(&mut app.shell, &key, None);
    assert!(!app.aks.busy());
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some(
            format!(
                "Deleted {}; Deployment orders-api is putting a new one up",
                key.name
            )
            .as_str()
        ),
        "the news says what is putting a new one up"
    );
}

#[test]
fn esc_leaves_the_pod_where_it_is_and_a_pod_with_no_controller_is_never_asked_about() {
    let mut app = aks_app();
    settle(&mut app);
    press(&mut app, KeyCode::Char('x'));

    assert_eq!(press(&mut app, KeyCode::Esc), AppAction::None);
    assert_eq!(app.aks.mode, AksMode::Browse);
    assert!(app.aks.restarting.is_none());
    assert!(!app.aks.busy(), "nothing was sent");

    app.aks
        .set_pods(&app.shell, "qa", Some("orders"), Ok(vec![bare("debug")]));
    select(&mut app, "debug");

    assert_eq!(press(&mut app, KeyCode::Char('x')), AppAction::None);
    assert_eq!(app.aks.mode, AksMode::Browse, "nothing was opened");
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("debug has no controller to put it back; deleting it would not restart it")
    );
}

#[test]
fn a_delete_that_was_refused_says_so_and_stops_the_spinner() {
    let mut app = aks_app();
    settle(&mut app);
    let key = target(&app).key;
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Char('x'));
    assert!(app.aks.busy());

    app.aks.delete_answered(
        &mut app.shell,
        &key,
        Some("pods is forbidden: User cannot delete".to_owned()),
    );

    assert!(!app.aks.busy());
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some(
            format!(
                "Could not restart {}: pods is forbidden: User cannot delete",
                key.name
            )
            .as_str()
        )
    );
}

#[test]
fn s_hands_the_terminal_to_kubectl_exec_on_the_container_the_log_is_on() {
    let mut app = sidecar_app();
    press(&mut app, KeyCode::Char('C'));

    let action = press(&mut app, KeyCode::Char('s'));

    assert_eq!(
        action,
        AppAction::ExecShell {
            context: "aks-qa".to_owned(),
            key: PodKey {
                cluster: "qa".to_owned(),
                namespace: "orders".to_owned(),
                name: "orders-api-7d9f5b-abc12".to_owned(),
            },
            container: Some("istio-proxy".to_owned()),
        },
        "the kubeconfig context is what kubectl is told, not the cluster's name"
    );

    let mut empty = App::new(Vec::new());
    empty.aks.set_clusters(vec![cluster("qa", &["orders"])]);
    empty.select_tab(TabId::Aks);
    assert_eq!(press(&mut empty, KeyCode::Char('s')), AppAction::None);
    assert_eq!(
        empty.shell.notification().map(|(text, _)| text),
        Some("No pod is selected")
    );
}

#[test]
fn g_goes_to_the_repository_the_image_names_and_says_what_it_tried_when_none_matches() {
    let mut app = aks_app();
    settle(&mut app);

    assert_eq!(
        app.aks.run_command(&mut app.shell, CommandId::OpenRepo),
        AppAction::Follow(Jump::Repo("orders-api".to_owned()))
    );
    press(&mut app, KeyCode::Char('g'));
    assert_eq!(app.tab, TabId::Repos, "and the key lands on that tab");

    app.select_tab(TabId::Aks);
    let mut stranger = pod("qa", "orders", "payments-7f-aaa", "Running");
    stranger.labels = vec![("app".to_owned(), "payments".to_owned())];
    stranger.containers[0].image = "myacr.azurecr.io/team/payments:4".to_owned();
    app.aks
        .set_pods(&app.shell, "qa", Some("orders"), Ok(vec![stranger]));
    select(&mut app, "payments-7f-aaa");

    assert_eq!(press(&mut app, KeyCode::Char('g')), AppAction::None);
    assert_eq!(app.tab, TabId::Aks, "nowhere to go");
    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("No repository on file is called payments")
    );
}

#[test]
fn y_copies_the_pod_the_way_kubectl_would_be_given_it() {
    let mut app = aks_app();
    select(&mut app, "billing-worker-1");

    let action = press(&mut app, KeyCode::Char('y'));

    assert_eq!(
        action,
        AppAction::Copy {
            text: "orders/billing-worker-1".to_owned(),
            content: CopiedContent::Id,
        }
    );
}

#[test]
fn o_says_what_does_get_you_inside_a_pod() {
    let mut app = aks_app();

    press(&mut app, KeyCode::Char('o'));

    assert_eq!(
        app.shell.notification().map(|(text, _)| text),
        Some("A pod has no page to open; s opens a shell in it")
    );
}

#[test]
fn narrowing_a_clusters_namespaces_drops_the_pods_it_no_longer_reads() {
    let mut app = aks_app();
    app.aks.set_pods(
        &app.shell,
        "qa",
        Some("billing"),
        Ok(vec![pod("qa", "billing", "billing-api-1", "Running")]),
    );
    assert!(names(&app).iter().any(|name| name == "billing-api-1"));
    app.aks
        .set_clusters(vec![cluster("qa", &["orders"]), cluster("prod", &[])]);
    assert!(
        !names(&app).iter().any(|name| name == "billing-api-1"),
        "a namespace the file no longer names goes: {:?}",
        names(&app)
    );
    // A cluster reading every namespace keeps them all.
    app.aks.set_pods(
        &app.shell,
        "prod",
        Some("billing"),
        Ok(vec![pod("prod", "billing", "billing-api-2", "Running")]),
    );
    app.aks
        .set_clusters(vec![cluster("qa", &["orders"]), cluster("prod", &[])]);
    assert!(names(&app).iter().any(|name| name == "billing-api-2"));
}

#[test]
fn a_describe_that_lands_after_the_cursor_moved_is_dropped_and_l_in_the_meantime_stands() {
    let mut app = aks_app();
    app.select_tab(TabId::Aks);
    settle(&mut app);
    let first = target(&app).key;
    assert_eq!(
        app.aks.run_command(&mut app.shell, CommandId::DescribePod),
        AppAction::Aks(AksRequest::Describe(first.clone()))
    );
    // `L` before the answer: the pane is the log's again and stays so.
    app.aks.run_command(&mut app.shell, CommandId::ShowLogs);
    app.aks
        .set_description(&first, Ok(vec!["Name: first".to_owned()]));
    assert_eq!(app.aks.pane(), PaneText::Log, "an answer switches nothing");
    assert!(app.aks.describe_lines().is_some(), "but it is held for D");

    // Asked again, then moved on: the old pod's answer is dropped. `D` put
    // the keyboard on the pane, so it goes back to the list first.
    app.aks.run_command(&mut app.shell, CommandId::DescribePod);
    app.shell.focus = Focus::Tickets;
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    settle(&mut app);
    let second = target(&app).key;
    assert_ne!(first, second);
    app.aks
        .set_description(&first, Ok(vec!["Name: first".to_owned()]));
    assert!(
        app.aks.describe_lines().is_none(),
        "{:?}",
        app.aks.describe_lines()
    );
    app.aks
        .set_description(&second, Ok(vec!["Name: second".to_owned()]));
    assert!(app.aks.describe_lines().is_some());
}

#[test]
fn the_skipped_line_count_keeps_counting_past_the_cap() {
    let mut app = aks_app();
    settle(&mut app);
    let target = target(&app);
    let lines: Vec<String> = (0..LOG_LINE_CAP + 2).map(|i| format!("line {i}")).collect();
    app.aks.append_log(&target, lines, false);
    assert_eq!(app.aks.log_lines().len(), LOG_LINE_CAP);
    assert_eq!(app.aks.log_lines()[0], "\u{2026} 3 earlier lines skipped");
    app.aks
        .append_log(&target, vec!["one more".to_owned()], false);
    assert_eq!(app.aks.log_lines()[0], "\u{2026} 4 earlier lines skipped");
    assert_eq!(app.aks.log_lines().len(), LOG_LINE_CAP);
}

#[test]
fn a_refused_request_leaves_nothing_waiting() {
    let mut app = aks_app();
    settle(&mut app);
    app.aks.run_command(&mut app.shell, CommandId::DescribePod);
    assert!(app.aks.busy());
    app.aks.request_refused();
    assert!(!app.aks.busy());
}

#[test]
fn g_records_the_pod_so_the_history_comes_back_to_it() {
    let mut app = aks_app();
    app.relate_repos(&[]);
    app.select_tab(TabId::Aks);
    settle(&mut app);
    let index = app
        .aks
        .visible_pods(&app.shell)
        .iter()
        .position(|row| row.repo.is_some())
        .expect("a pod with a repository on file");
    app.aks.cursor.focus(index);
    settle(&mut app);
    let key = target(&app).key;
    let AppAction::Follow(jump) = app.aks.run_command(&mut app.shell, CommandId::OpenRepo) else {
        panic!("g did not follow");
    };
    assert!(app.follow(&jump), "the repository is on file");
    assert_eq!(app.tab, TabId::Repos);
    app.history_back();
    assert_eq!(app.tab, TabId::Aks, "[ comes back to the pod");
    assert_eq!(
        app.aks.selected_pod(&app.shell).map(|row| row.pod.key),
        Some(key)
    );
}

#[test]
fn a_global_key_or_a_digit_does_not_reach_past_the_restart_confirm() {
    let mut app = aks_app();
    app.select_tab(TabId::Aks);
    settle(&mut app);
    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(app.aks.mode, AksMode::ConfirmRestart);
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
    assert!(
        !app.shell_overlay_open(),
        "the columns editor does not open over an armed confirm"
    );
    assert_eq!(
        app.aks.mode,
        AksMode::Browse,
        "c answered the confirm: it is left"
    );
    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
    assert_eq!(
        app.tab,
        TabId::Aks,
        "a digit does not switch tabs past it either"
    );
    assert_eq!(app.aks.mode, AksMode::Browse);
    assert!(app.aks.restarting.is_none());
}
