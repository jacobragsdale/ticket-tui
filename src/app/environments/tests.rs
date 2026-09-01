//! The environments board, driven off `fixtures/kustomize`: the two overlays
//! checked in beside it are handed to the screen the way the local thread
//! hands them over, so nothing here needs `kubectl`.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::aks::PodKey;
use crate::app::App;
use crate::arm::{ItemKind, VaultItem};
use crate::model::{RunResult, RunStatus};
use crate::timestamp::{Timestamp, ts};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/kustomize")
}

fn environment(name: &str, vault: &str) -> Environment {
    Environment {
        name: name.to_owned(),
        overlays: vec![format!("overlays/{name}")],
        vault: Some(vault.to_owned()),
        ..Environment::default()
    }
}

pub(crate) fn deployment() -> Deployment {
    Deployment {
        repo: "deployment".to_owned(),
        clone: fixtures(),
        render: "true".to_owned(),
        environments: vec![environment("qa", "kv-qa"), environment("prod", "kv-prod")],
    }
}

fn rendered(name: &str) -> String {
    std::fs::read_to_string(fixtures().join(format!("rendered/{name}.yaml")))
        .expect("the checked-in render")
}

/// The run that built qa's `1.4.0`, so an image line reads back to a build.
fn build(id: i64, number: &str) -> crate::model::Run {
    crate::model::Run {
        id,
        pipeline_id: 4,
        build_number: number.to_owned(),
        status: RunStatus::Completed,
        result: Some(RunResult::Succeeded),
        source_branch: "refs/heads/main".into(),
        source_version: "abc1234def5678".into(),
        requested_for: None,
        reason: "individualCI".into(),
        pr_id: None,
        queue_time: Some(ts("2026-08-29T10:00:00Z")),
        start_time: Some(ts("2026-08-29T10:00:05Z")),
        finish_time: Some(ts("2026-08-29T10:04:17Z")),
        url: String::new(),
    }
}

/// An app whose board holds the fixture's two environments, rendered.
pub(crate) fn environments_app() -> App {
    let mut app = App::new(Vec::new());
    app.environments.set_deployment(Some(deployment()), None);
    app.environments
        .set_runs(vec![build(77, "1.4.0"), build(78, "1.3.9")]);
    // The renders the local thread would have sent back, one per overlay.
    let requests = app.environments.renders_due();
    for request in requests {
        let LocalRequest::Render {
            environment,
            overlay,
            ..
        } = request
        else {
            panic!("the board asks for renders and nothing else");
        };
        app.environments
            .set_rendered(&environment, &overlay, Ok(rendered(&environment)));
    }
    app.select_tab(TabId::Environments);
    app
}

fn press(app: &mut App, code: KeyCode) -> AppAction {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn services(app: &App) -> Vec<String> {
    app.environments
        .visible()
        .into_iter()
        .map(|row| row.workload)
        .collect()
}

fn cell(app: &App, service: &str, environment: usize) -> EnvCell {
    app.environments
        .visible()
        .into_iter()
        .find(|row| row.workload == service)
        .expect("the service is on the board")
        .cells
        .swap_remove(environment)
}

#[test]
fn the_board_lists_every_service_across_both_environments_with_what_each_runs() {
    let app = environments_app();

    assert_eq!(
        services(&app),
        vec!["billing-api", "orders-api", "reaper"],
        "one row per workload name, deduplicated across the two namespaces"
    );
    let row = app
        .environments
        .visible()
        .into_iter()
        .find(|row| row.workload == "orders-api")
        .expect("orders-api");
    assert_eq!(
        row.namespace, "shop-qa",
        "the namespace of the left-most environment that holds it"
    );
    assert_eq!(row.cells.len(), 2, "one cell per [[environments]]");
    assert_eq!(row.cells[0].tag.as_deref(), Some("1.4.0"));
    assert_eq!(row.cells[1].tag.as_deref(), Some("1.3.9"));
    assert!(row.cells[0].clean(), "qa is not missing anything");
}

#[test]
fn a_cell_counts_what_that_environment_would_be_missing() {
    let app = environments_app();

    // prod's ConfigMap never got the key qa's has.
    assert_eq!(cell(&app, "orders-api", 1).findings, 1);
    assert_eq!(cell(&app, "orders-api", 0).findings, 0);
    // And prod's provider produces a Secret without the key billing-api reads,
    // which is the workload's finding even though the provider is a document
    // of its own.
    assert_eq!(cell(&app, "billing-api", 1).findings, 1);
    assert_eq!(cell(&app, "billing-api", 0).findings, 0);
}

#[test]
fn the_vault_half_lands_on_the_service_that_pulls_the_object() {
    let mut app = environments_app();
    let now = Timestamp::now();
    let item = |name: &str, expires: Option<Timestamp>| VaultItem {
        kind: ItemKind::Secret,
        name: name.to_owned(),
        enabled: true,
        created: None,
        updated: None,
        expires,
        content_type: None,
        recovery_level: None,
    };
    // prod's vault holds one of the three objects its provider pulls, and the
    // one it holds is about to lapse.
    app.environments.set_vaults(vec![VaultNames::from_items(
        "kv-prod",
        &[item(
            "billing-signing-key",
            Some(now.plus_seconds(5 * 24 * 60 * 60)),
        )],
        now,
    )]);

    let prod = cell(&app, "billing-api", 1);
    assert_eq!(
        prod.expiring, 1,
        "the object in use that falls due is the \u{25c7} count"
    );
    assert_eq!(
        prod.findings, 3,
        "the Secret key, and the two objects the vault does not hold: {:?}",
        prod
    );
}

#[test]
fn h_and_l_move_the_column_and_the_frame_says_which_promotion_it_reads() {
    let mut app = environments_app();

    assert_eq!(
        app.environments.column(),
        1,
        "the board opens on the right-most environment, which is what a \
         promotion is read into"
    );
    assert_eq!(
        app.environments.promotion_label(),
        "Promotion \u{00b7} billing-api \u{00b7} qa \u{2192} prod"
    );

    press(&mut app, KeyCode::Char('h'));
    assert_eq!(app.environments.column(), 0);
    assert_eq!(
        app.environments.promotion_label(),
        "Promotion \u{00b7} billing-api \u{00b7} qa",
        "the first environment has nothing to its left to promote from"
    );
    press(&mut app, KeyCode::Char('l'));
    press(&mut app, KeyCode::Char('l'));
    assert_eq!(app.environments.column(), 1, "and the right end holds");
}

#[test]
fn the_details_pane_reads_qa_into_prod_for_the_service_under_the_cursor() {
    let mut app = environments_app();
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(
        app.environments
            .selected()
            .map(|row| row.workload)
            .as_deref(),
        Some("orders-api")
    );

    let lines = app.environments.detail_text(&app.shell);
    assert!(
        lines.contains(&"Missing in prod".to_owned()),
        "the cell's own findings sit above the diff: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("RATE_LIMIT_PER_MIN") && line.contains("missing")),
        "{lines:?}"
    );
    for section in ["Image", "Config", "Variables"] {
        assert!(
            lines.contains(&section.to_owned()),
            "the {section} section: {lines:?}"
        );
    }
    assert!(
        lines.contains(&"moves api from 1.3.9 to 1.4.0".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"adds RATE_LIMIT_PER_MIN to prod/orders-config".to_owned()),
        "the same words the pull request pre-flight uses: {lines:?}"
    );
    assert!(
        lines.contains(&"sets TRACE_SAMPLE_RATE on prod/api".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("env diff")),
        "and the git history is the CLI's, said on the line: {lines:?}"
    );
}

#[test]
fn every_line_that_names_a_thing_on_another_tab_goes_there() {
    let mut app = environments_app();
    let now = Timestamp::now();
    app.environments.set_vaults(vec![VaultNames::from_items(
        "kv-prod",
        &[VaultItem {
            kind: ItemKind::Secret,
            name: "billing-signing-key".to_owned(),
            enabled: true,
            created: None,
            updated: None,
            expires: Some(now.plus_seconds(5 * 24 * 60 * 60)),
            content_type: None,
            recovery_level: None,
        }],
        now,
    )]);
    // A pod running what prod runs, out of what the AKS tab has read.
    let pod = PodKey {
        cluster: "prod".to_owned(),
        namespace: "shop-prod".to_owned(),
        name: "billing-api-7d9".to_owned(),
    };
    app.shell.set_pod_images(vec![(
        "acrprod.azurecr.io/team/billing-api:2.0.1".to_owned(),
        pod.clone(),
    )]);

    let jumps = app.environments.jumps(&app.shell);
    assert!(
        jumps.contains(&Jump::Pod(pod)),
        "the pod the target environment is running: {jumps:?}"
    );
    assert!(
        jumps.iter().any(|jump| matches!(
            jump,
            Jump::VaultItem { vault, kind, name }
                if vault == "kv-prod" && kind == "secret" && name == "billing-legacy-token"
        )),
        "a vault line goes to the object itself: {jumps:?}"
    );
    assert!(
        jumps.iter().any(|jump| matches!(
            jump,
            Jump::VaultItem { name, .. } if name == "billing-signing-key"
        )),
        "and so does an expiry: {jumps:?}"
    );

    // The image line of a service whose arriving tag has a run on file.
    press(&mut app, KeyCode::Char('j'));
    let jumps = app.environments.jumps(&app.shell);
    assert!(
        jumps.contains(&Jump::Run(77)),
        "an image line reads the tag back to the build that made it: {jumps:?}"
    );

    // `g` answers with the line the pane's cursor is on.
    app.shell.focus = Focus::Details;
    let (first, _) = app
        .environments
        .follow_target(&app.shell)
        .expect("a line to go to");
    assert_eq!(first, jumps[0]);
    press(&mut app, KeyCode::Char('j'));
    let (second, _) = app
        .environments
        .follow_target(&app.shell)
        .expect("the next line");
    assert_eq!(second, jumps[1], "j walks the pane's own cursor");
}

#[test]
fn r_asks_for_one_render_per_overlay_and_reads_the_vaults_again() {
    let mut app = environments_app();
    assert!(
        app.environments.renders_due().is_empty(),
        "nothing is stale"
    );
    assert!(app.environments.take_stale_vaults().is_empty());

    let action = press(&mut app, KeyCode::Char('r'));
    assert!(
        matches!(
            action,
            AppAction::Arm(crate::arm_watch::ArmRequest::Refresh)
        ),
        "{action:?}"
    );
    assert_eq!(
        app.environments.take_stale_vaults(),
        vec!["kv-qa".to_owned(), "kv-prod".to_owned()],
        "the vaults the environments pull from are listed afresh too"
    );
    assert!(
        app.environments.take_stale_vaults().is_empty(),
        "and asked for once, so the worker is not told twice"
    );
    let requests = app.environments.renders_due();
    assert_eq!(requests.len(), 2, "one per overlay: {requests:?}");
    assert!(requests.iter().all(|request| matches!(
        request,
        LocalRequest::Render { command, .. } if command == "true"
    )));
    assert!(
        app.environments.busy(),
        "and the board waits on them rather than asking twice"
    );
    assert!(app.environments.renders_due().is_empty());
}

#[test]
fn a_pull_of_the_deployment_clone_renders_it_again() {
    let mut app = environments_app();
    assert!(app.environments.renders_due().is_empty());

    app.environments.repo_pulled("ticket-tui");
    assert!(
        app.environments.renders_due().is_empty(),
        "another repository is nothing to do with the overlays"
    );
    app.environments.repo_pulled("deployment");
    assert_eq!(app.environments.renders_due().len(), 2);
}

#[test]
fn the_badge_counts_the_environments_that_would_be_missing_something() {
    let app = environments_app();
    assert_eq!(
        Screen::badge(&app.environments).as_deref(),
        Some("\u{2717}1"),
        "prod is short and qa is not"
    );
    assert!(
        Screen::badge(&EnvironmentsScreen::default()).is_none(),
        "a run with no deployment repository badges nothing"
    );
}

#[test]
fn the_findings_filter_leaves_only_the_rows_something_is_missing_from() {
    let mut app = environments_app();
    assert!(!app.environments.findings_only());

    press(&mut app, KeyCode::Char('F'));
    assert!(app.environments.findings_only());
    assert_eq!(services(&app), vec!["billing-api", "orders-api"]);

    press(&mut app, KeyCode::Char('F'));
    assert!(!app.environments.findings_only());
    assert_eq!(services(&app).len(), 3);

    // And the search box narrows by service the way every other tab's does.
    app.environments.set_query("orders".to_owned());
    assert_eq!(services(&app), vec!["orders-api"]);
}

#[test]
fn with_no_deployment_repository_the_tab_says_so_and_where_it_looked() {
    let mut app = App::new(Vec::new());
    app.environments
        .set_deployment(None, Some("no clone of deployment in /srv/code".to_owned()));

    assert_eq!(
        app.environments.reason(),
        Some("no clone of deployment in /srv/code")
    );
    assert!(app.environments.visible().is_empty());
    assert!(app.environments.renders_due().is_empty());
    assert!(app.environments.detail_lines(&app.shell).is_empty());
}

#[test]
fn the_agent_context_names_the_environments_the_cell_and_the_diff() {
    let mut app = environments_app();
    press(&mut app, KeyCode::Char('j'));

    let context = app.agent_context();
    assert_eq!(context.active_tab, "environments");
    let environments = &context.environments;
    assert_eq!(environments.environments.len(), 2);
    assert_eq!(environments.environments[0].name, "qa");
    assert_eq!(
        environments.environments[1].vault.as_deref(),
        Some("kv-prod")
    );
    assert!(environments.environments[1].rendered);
    assert_eq!(environments.environments[0].findings, 0);
    assert_eq!(environments.environments[1].findings, 2);
    assert_eq!(environments.selected_service.as_deref(), Some("orders-api"));
    assert_eq!(environments.selected_environment.as_deref(), Some("prod"));
    assert!(
        environments
            .findings
            .iter()
            .any(|line| line.contains("RATE_LIMIT_PER_MIN")),
        "{:?}",
        environments.findings
    );
    let diff = environments.diff.as_ref().expect("qa into prod");
    assert_eq!((diff.from.as_str(), diff.to.as_str()), ("qa", "prod"));
    assert!(
        diff.lines
            .contains(&"adds RATE_LIMIT_PER_MIN to prod/orders-config".to_owned()),
        "{:?}",
        diff.lines
    );
    assert_eq!(environments.visible_rows, 3);
}
