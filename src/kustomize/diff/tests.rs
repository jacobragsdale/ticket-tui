//! The promotion diff over the fixture under `fixtures/kustomize`, whose qa
//! and prod differ in the planted ways, and the image half over runs and pull
//! requests made here. Nothing runs `kubectl` or git: the render is checked in
//! and the `git log` is a slice of lines.

use std::path::PathBuf;

use super::*;
use crate::model::{Identity, PrStatus, PullRequest, Run, RunStatus};

/// One environment of the fixture, from the rendered YAML checked in beside it.
fn fixture(environment: &str) -> EnvManifest {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("fixtures/kustomize/rendered/{environment}.yaml"));
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    super::super::parse(environment, &yaml)
}

fn service<'a>(diff: &'a PromotionDiff, name: &str) -> &'a ServiceDiff {
    diff.services
        .iter()
        .find(|held| held.workload == name)
        .unwrap_or_else(|| panic!("no service called {name} in {diff:?}"))
}

/// `object name (side)`, which is every entry as the block prints it.
fn entries(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| {
            format!(
                "{} {} {}",
                entry.object,
                entry.name,
                if entry.side == Side::From {
                    "from"
                } else {
                    "to"
                }
            )
        })
        .collect()
}

fn run(id: i64, build_number: &str, commit: &str) -> Run {
    Run {
        id,
        pipeline_id: 7,
        build_number: build_number.to_owned(),
        status: RunStatus::Completed,
        result: None,
        source_branch: "refs/heads/main".to_owned(),
        source_version: commit.to_owned(),
        requested_for: None,
        reason: "individualCI".to_owned(),
        pr_id: None,
        queue_time: None,
        start_time: None,
        finish_time: None,
        url: String::new(),
    }
}

fn request(id: i64, commit: &str, work_items: &[i64]) -> PullRequest {
    PullRequest {
        repo_id: "orders-guid".to_owned(),
        id,
        title: format!("Pull request {id}"),
        description: String::new(),
        status: PrStatus::Completed,
        is_draft: false,
        created_by: Identity::new("Avery Chen".to_owned(), None),
        created_at: None,
        closed_at: None,
        source_ref: "refs/heads/feature".to_owned(),
        target_ref: "refs/heads/main".to_owned(),
        merge_status: "succeeded".to_owned(),
        last_merge_source_commit: commit.to_owned(),
        auto_complete_set_by: None,
        url: String::new(),
        reviewers: Vec::new(),
        work_items: work_items.to_vec(),
        build: None,
        threads: Vec::new(),
    }
}

#[test]
fn the_planted_key_variable_and_image_differences_are_what_orders_reads_as() {
    let (qa, prod) = (fixture("qa"), fixture("prod"));
    let diff = diff(&qa, &prod, Some("orders"));
    assert_eq!(diff.from, "qa");
    assert_eq!(diff.to, "prod");
    assert_eq!(
        diff.services.len(),
        1,
        "a service names the workload it is a part of: {diff:?}"
    );

    let orders = service(&diff, "orders-api");
    assert_eq!(orders.kind, "Deployment");
    assert!(orders.only_in.is_none(), "it is in both");
    // The key qa's overlay patches in and prod's never got.
    assert_eq!(
        entries(&orders.keys),
        ["orders-config RATE_LIMIT_PER_MIN from"]
    );
    // The variable qa's overlay sets, by name and never by value.
    assert_eq!(entries(&orders.variables), ["api TRACE_SAMPLE_RATE from"]);
    assert!(!format!("{orders:?}").contains("0.25"), "{orders:?}");
    // Both containers are on a newer tag in qa.
    assert_eq!(
        orders
            .images
            .iter()
            .map(|image| format!(
                "{} {} \u{2192} {}",
                image.container,
                image.from.as_deref().unwrap_or("\u{2014}"),
                image.to.as_deref().unwrap_or("\u{2014}")
            ))
            .collect::<Vec<_>>(),
        ["api 1.4.0 \u{2192} 1.3.9", "migrate 1.4.0 \u{2192} 1.3.9"]
    );
    assert!(
        orders.images.iter().all(|image| image.history.is_none()),
        "the pure half reads no database"
    );
}

#[test]
fn the_vault_object_prod_pulls_and_the_secret_key_qa_produces_are_both_named_and_marked() {
    let (qa, prod) = (fixture("qa"), fixture("prod"));
    let diff = diff(&qa, &prod, Some("billing"));
    let billing = service(&diff, "billing-api");
    // prod's SecretProviderClass pulls one object qa's does not, which is the
    // reverse entry a line marks `only in prod`.
    assert_eq!(
        entries(&billing.vault_objects),
        ["kv-prod billing-legacy-token to"]
    );
    assert_eq!(diff.environment(Side::To), "prod");
    // And qa's produces a Secret key prod's does not.
    assert_eq!(entries(&billing.keys), ["billing-kv SIGNING_KEY from"]);
    // The registry differs but the tag does not, so the image says nothing.
    assert!(billing.images.is_empty(), "{:?}", billing.images);
}

#[test]
fn a_workload_only_in_one_environment_is_the_whole_of_what_that_service_says() {
    let mut qa = fixture("qa");
    qa.workloads.push(
        super::super::parse(
            "qa",
            concat!(
                "apiVersion: apps/v1\nkind: StatefulSet\nmetadata:\n  name: ledger\n",
                "  namespace: shop-qa\nspec:\n  template:\n    spec:\n",
                "      containers:\n      - name: ledger\n        image: acrqa.azurecr.io/team/ledger:0.1.0\n",
            ),
        )
        .workloads
        .remove(0),
    );
    let diff = diff(&qa, &fixture("prod"), None);
    let ledger = service(&diff, "ledger");
    assert_eq!(ledger.only_in, Some(Side::From));
    assert_eq!(ledger.kind, "StatefulSet");
    assert_eq!(diff.environment(ledger.only_in.unwrap()), "qa");
    assert!(ledger.images.is_empty() && ledger.keys.is_empty());

    // And the other way round, with the same workload read from prod.
    let reversed = diff_of(&fixture("prod"), &qa);
    assert_eq!(service(&reversed, "ledger").only_in, Some(Side::To));
}

/// The whole diff, unnarrowed, which two tests read.
fn diff_of(from: &EnvManifest, to: &EnvManifest) -> PromotionDiff {
    diff(from, to, None)
}

#[test]
fn an_environment_against_itself_is_identical_and_a_service_no_workload_is_called_is_empty() {
    let qa = fixture("qa");
    assert!(diff_of(&qa, &qa).services.is_empty(), "identical");
    assert!(
        diff(&qa, &fixture("prod"), Some("nothing-of-the-sort"))
            .services
            .is_empty()
    );
    // Unnarrowed, the two environments differ over all three: the CronJob's
    // own tag never moved, but it reads the ConfigMap the qa overlay patched,
    // and a shared ConfigMap that differs is every reader's business.
    let all = diff_of(&qa, &fixture("prod"));
    assert_eq!(
        all.services
            .iter()
            .map(|service| service.workload.as_str())
            .collect::<Vec<_>>(),
        ["billing-api", "orders-api", "reaper"]
    );
    let reaper = service(&all, "reaper");
    assert!(reaper.images.is_empty() && reaper.variables.is_empty());
    assert_eq!(
        entries(&reaper.keys),
        ["orders-config RATE_LIMIT_PER_MIN from"]
    );
}

#[test]
fn a_tag_is_read_back_by_build_number_then_by_commit_then_by_the_registrys_revision() {
    let runs = vec![
        run(31, "20260831.2", "9f1c2d3a4b5c6d7e8f90112233445566778899aa"),
        run(30, "20260830.1", "1234567890abcdef1234567890abcdef12345678"),
    ];

    // The build number, which is what a pipeline usually tags with.
    let matched = read_back("20260830.1", &runs, None).expect("the run of that build");
    assert_eq!((matched.id, matched.matched), (30, Match::BuildNumber));

    // The commit, where the tag is its head.
    let matched = read_back("9f1c2d3", &runs, None).expect("the run of that commit");
    assert_eq!((matched.id, matched.matched), (31, Match::Commit));
    assert_eq!(matched.commit, "9f1c2d3a4b5c6d7e8f90112233445566778899aa");

    // Neither, so the annotation the registry carries answers instead.
    assert!(read_back("1.4.0", &runs, None).is_none());
    let matched = read_back("1.4.0", &runs, Some("1234567890abcdef")).expect("the annotated run");
    assert_eq!((matched.id, matched.matched), (30, Match::Revision));
    // A version is never mistaken for a commit, on either rule.
    assert!(read_back("1.4.0", &runs, Some("2.0.0")).is_none());
    assert!(read_back("", &runs, None).is_none());
}

#[test]
fn the_pull_requests_between_two_runs_are_the_ones_the_log_holds_and_they_carry_their_work_items() {
    let runs = vec![
        run(31, "1.4.0", "9f1c2d3a4b5c6d7e8f90112233445566778899aa"),
        run(30, "1.3.9", "1234567890abcdef1234567890abcdef12345678"),
    ];
    let requests = vec![
        request(812, "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1", &[642]),
        // Squashed, so its own commit is nowhere in the history and only the
        // subject Azure DevOps wrote names it.
        request(815, "bbbb222bbbb222bbbb222bbbb222bbbb222bbbb2", &[650]),
        request(820, "cccc333cccc333cccc333cccc333cccc333cccc3", &[655, 642]),
        // Merged long before the range, and in another repository besides.
        {
            let mut older = request(700, "dddd444dddd444dddd444dddd444dddd444dddd4", &[600]);
            older.repo_id = "billing-guid".to_owned();
            older
        },
    ];
    let log = |older: &str, newer: &str| {
        assert_eq!(older, "1234567890abcdef1234567890abcdef12345678");
        assert_eq!(newer, "9f1c2d3a4b5c6d7e8f90112233445566778899aa");
        Ok(vec![
            "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1 Merge the first one".to_owned(),
            "eeee555eeee555eeee555eeee555eeee555eeee5 Merged PR 815: Squash the second".to_owned(),
            "cccc333cccc333cccc333cccc333cccc333cccc3 Merge the third one".to_owned(),
        ])
    };
    let change = ImageChange {
        container: "api".to_owned(),
        from: Some("1.4.0".to_owned()),
        to: Some("1.3.9".to_owned()),
        history: None,
    };
    let read = history(
        &change,
        &runs,
        &requests,
        Some("orders-guid"),
        &|_| None,
        &log,
    );
    assert_eq!(read.pull_requests, [812, 815, 820]);
    assert_eq!(read.work_items, [642, 650, 655]);
    assert_eq!(read.from.as_ref().map(|run| run.id), Some(31));
    assert_eq!(read.to.as_ref().map(|run| run.id), Some(30));
    assert_eq!(
        read.to_string(),
        "3 PRs behind: !812 !815 !820 \u{2014} #642 #650 #655"
    );

    // No clone to read the history in is the line the reader gets instead.
    let unread = history(&change, &runs, &requests, None, &|_| None, &|_, _| {
        Err("no clone of orders to read the history from".to_owned())
    });
    assert_eq!(
        unread.to_string(),
        "no clone of orders to read the history from"
    );
    assert_eq!(
        unread.from.as_ref().map(|run| run.id),
        Some(31),
        "read back all the same"
    );

    // A tag no run on file accounts for says so rather than guessing.
    let unknown = ImageChange {
        to: Some("0.0.1".to_owned()),
        ..change.clone()
    };
    let unknown = history(&unknown, &runs, &requests, None, &|_| None, &log);
    assert_eq!(unknown.to_string(), "no run on file for 0.0.1");
    // And an empty range is one PR fewer than none at all.
    let quiet = history_of(&change, &runs, &requests);
    assert_eq!(quiet.to_string(), "no pull request between them");
    assert_eq!(quiet.pull_requests, [0i64; 0]);
}

/// The same history with an empty log, which two assertions read.
fn history_of(change: &ImageChange, runs: &[Run], requests: &[PullRequest]) -> ImageHistory {
    history(change, runs, requests, None, &|_| None, &|_, _| {
        Ok(Vec::new())
    })
}

#[test]
fn one_pull_request_reads_as_one_and_a_tag_is_whatever_the_registry_versions() {
    let runs = vec![
        run(31, "1.4.0", "9f1c2d3a4b5c6d7e8f90112233445566778899aa"),
        run(30, "1.3.9", "1234567890abcdef1234567890abcdef12345678"),
    ];
    let requests = vec![request(
        812,
        "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1",
        &[],
    )];
    let change = ImageChange {
        container: "api".to_owned(),
        from: Some("1.4.0".to_owned()),
        to: Some("1.3.9".to_owned()),
        history: None,
    };
    let one = history(&change, &runs, &requests, None, &|_| None, &|_, _| {
        Ok(vec![
            "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1 One".to_owned(),
        ])
    });
    assert_eq!(one.to_string(), "1 PR behind: !812");

    assert_eq!(tag("acrqa.azurecr.io/team/orders-api:1.4.0"), "1.4.0");
    assert_eq!(
        tag("registry:5000/team/orders-api"),
        "registry:5000/team/orders-api"
    );
    assert_eq!(
        tag("acrqa.azurecr.io/team/orders-api@sha256:abc123"),
        "sha256:abc123"
    );
    assert_eq!(tag("orders-api"), "orders-api");
}

/// Two services that happen to share a name in two namespaces are two rows;
/// and qa and prod keeping namespaces of their own — which the fixture does —
/// still pair up by name and kind.
#[test]
fn a_name_shared_across_namespaces_is_two_services_and_differing_namespaces_still_pair() {
    const ORDERS: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata: {name: orders, namespace: alpha}
spec: {template: {spec: {containers: [{name: api, image: r/orders:1}]}}}
---
apiVersion: apps/v1
kind: Deployment
metadata: {name: orders, namespace: beta}
spec: {template: {spec: {containers: [{name: api, image: r/orders:BETA}]}}}
";
    let from = super::super::parse("qa", &ORDERS.replace("BETA", "2"));
    let to = super::super::parse("prod", &ORDERS.replace("BETA", "9"));
    let read = diff(&from, &to, None);
    assert_eq!(read.services.len(), 1, "{read:?}");
    assert_eq!(read.services[0].namespace, "beta");
    assert_eq!(
        (
            read.services[0].images[0].from.as_deref(),
            read.services[0].images[0].to.as_deref()
        ),
        (Some("2"), Some("9"))
    );

    let read = diff(&fixture("qa"), &fixture("prod"), Some("orders-api"));
    assert!(
        read.services.iter().all(|held| held.only_in.is_none()),
        "shop-qa and shop-prod are the same service: {read:?}"
    );
}
