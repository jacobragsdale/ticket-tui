//! What the fixture under `fixtures/kustomize` declares, and what a check of
//! it finds. Everything but the one `#[ignore]`d test reads the rendered YAML
//! checked in beside the overlays, so the suite needs no `kubectl`.

use std::path::PathBuf;

use super::*;

/// `fixtures/kustomize`, wherever the repository is checked out.
fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/kustomize")
}

/// One environment of the fixture, from the rendered YAML checked in beside it.
fn fixture(environment: &str) -> EnvManifest {
    let path = fixtures().join(format!("rendered/{environment}.yaml"));
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    parse(environment, &yaml)
}

/// One workload of a manifest, by name.
fn workload<'a>(manifest: &'a EnvManifest, name: &str) -> &'a Workload {
    manifest
        .workloads
        .iter()
        .find(|held| held.name == name)
        .unwrap_or_else(|| panic!("no workload called {name}"))
}

fn container<'a>(workload: &'a Workload, name: &str) -> &'a Container {
    workload
        .containers
        .iter()
        .find(|held| held.name == name)
        .unwrap_or_else(|| panic!("no container called {name}"))
}

#[test]
fn every_kind_of_reference_a_container_makes_is_read_out_of_the_render() {
    let manifest = fixture("qa");
    let orders = workload(&manifest, "orders-api");
    assert_eq!(orders.kind, "Deployment");
    assert_eq!(orders.namespace, "shop-qa");

    let api = container(orders, "api");
    assert_eq!(api.image, "acrqa.azurecr.io/team/orders-api:1.4.0");
    assert!(!api.init);

    // A key out of a ConfigMap, a key out of a Secret, and the whole of a
    // ConfigMap through envFrom.
    assert!(api.references.contains(&Reference {
        source: Source::Env {
            var: "RATE_LIMIT_PER_MIN".to_owned()
        },
        object: ObjectKind::ConfigMap,
        name: "orders-config".to_owned(),
        key: Some("RATE_LIMIT_PER_MIN".to_owned()),
        optional: false,
    }));
    assert!(
        api.references
            .iter()
            .any(|held| held.object == ObjectKind::Secret
                && held.name.starts_with("orders-runtime-")
                && held.key.as_deref() == Some("SESSION_KEY"))
    );
    assert!(api.references.contains(&Reference {
        source: Source::EnvFrom,
        object: ObjectKind::ConfigMap,
        name: "orders-config".to_owned(),
        key: None,
        optional: false,
    }));

    // The init container is found too, and marked as one.
    let migrate = container(orders, "migrate");
    assert!(migrate.init);
    assert_eq!(migrate.references.len(), 1);
    assert_eq!(migrate.references[0].key.as_deref(), Some("LOG_LEVEL"));

    // A volume names the ConfigMap and the one key its `items` takes.
    assert_eq!(
        orders.volumes,
        vec![Reference {
            source: Source::Volume {
                volume: "banner".to_owned()
            },
            object: ObjectKind::ConfigMap,
            name: "orders-config".to_owned(),
            key: Some("BANNER".to_owned()),
            optional: false,
        }]
    );

    // And the CSI volume names the class it mounts.
    let billing = workload(&manifest, "billing-api");
    assert_eq!(
        billing.volumes,
        vec![Reference {
            source: Source::Volume {
                volume: "vault".to_owned()
            },
            object: ObjectKind::SecretProviderClass,
            name: "billing-vault".to_owned(),
            key: None,
            optional: false,
        }]
    );
}

#[test]
fn optional_is_carried_through_and_a_generated_name_is_the_hashed_one() {
    let manifest = fixture("qa");
    let api = container(workload(&manifest, "orders-api"), "api");
    let flags = api
        .references
        .iter()
        .find(|held| held.name == "orders-flags")
        .expect("the optional reference is read like any other");
    assert!(flags.optional, "optional: true survives the render");

    // The secretGenerator's Secret is hash-suffixed, and the Deployment's own
    // reference to it was rewritten to match — which is the whole reason the
    // render is read rather than the templates.
    let secret = manifest
        .secrets
        .iter()
        .find(|held| held.name.starts_with("orders-runtime-"))
        .expect("the generated Secret");
    assert!(
        secret.name.len() > "orders-runtime-".len(),
        "{}",
        secret.name
    );
    assert_eq!(secret.keys, vec!["SESSION_KEY".to_owned()]);
    assert!(api.references.iter().any(|held| held.name == secret.name));
}

#[test]
fn a_cronjobs_containers_are_found_under_its_job_template() {
    let manifest = fixture("qa");
    let reaper = workload(&manifest, "reaper");
    assert_eq!(reaper.kind, "CronJob");
    let container = container(reaper, "reaper");
    assert_eq!(container.image, "acrqa.azurecr.io/team/reaper:0.9.0");
    assert!(container.references.contains(&Reference {
        source: Source::EnvFrom,
        object: ObjectKind::Secret,
        name: "reaper-external".to_owned(),
        key: None,
        optional: false,
    }));
}

#[test]
fn both_providers_reduce_to_the_objects_they_pull_and_the_keys_they_produce() {
    let manifest = fixture("prod");

    let class = manifest
        .providers
        .iter()
        .find(|held| held.kind == "SecretProviderClass")
        .expect("the SecretProviderClass");
    assert_eq!(class.vault.as_deref(), Some("kv-prod"));
    // The nested `objects` YAML — a string of strings — parses with the same
    // parser the document did.
    assert_eq!(
        class
            .objects
            .iter()
            .map(|object| object.name.as_str())
            .collect::<Vec<_>>(),
        [
            "billing-signing-key",
            "billing-db-password",
            "billing-legacy-token"
        ]
    );
    assert_eq!(class.objects[0].kind.as_deref(), Some("secret"));
    assert_eq!(class.objects[0].alias.as_deref(), Some("SIGNING_KEY"));
    assert_eq!(
        class.produces,
        vec![KeyedObject {
            namespace: "shop-prod".to_owned(),
            name: "billing-kv".to_owned(),
            keys: vec!["DB_PASSWORD".to_owned()],
        }],
        "prod's overlay produces one of the two keys qa's does"
    );
    assert!(!class.whole);

    let external = manifest
        .providers
        .iter()
        .find(|held| held.kind == "ExternalSecret")
        .expect("the ExternalSecret");
    // The vault comes from the SecretStore it names, not from the object.
    assert_eq!(external.vault.as_deref(), Some("kv-prod"));
    assert_eq!(
        external
            .objects
            .iter()
            .map(|object| object.name.as_str())
            .collect::<Vec<_>>(),
        ["reaper-token", "reaper-config"]
    );
    assert_eq!(external.produces[0].name, "reaper-external");
    assert_eq!(external.produces[0].keys, vec!["REAPER_TOKEN".to_owned()]);
    assert!(
        external.whole,
        "dataFrom pulls an object whole, so its keys are not knowable here"
    );
}

#[test]
fn objects_written_as_plain_mappings_parse_as_well_as_ones_written_as_strings() {
    let objects = vault_objects(
        "array:\n  - objectName: db-password\n    objectType: secret\n    objectAlias: DB\n",
    );
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].name, "db-password");
    assert_eq!(objects[0].alias.as_deref(), Some("DB"));
    assert!(vault_objects("not: an array").is_empty());
    assert!(vault_objects("::::").is_empty());
}

#[test]
fn a_document_of_an_unknown_kind_is_skipped_rather_than_refused() {
    let manifest = parse(
        "qa",
        concat!(
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: orders-api\n",
            "---\n",
            "apiVersion: networking.k8s.io/v1\nkind: NetworkPolicy\nmetadata:\n  name: deny\n",
            "---\n",
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: kept\ndata:\n  A: b\n",
            "---\n",
        ),
    );
    assert!(manifest.workloads.is_empty());
    assert_eq!(manifest.config_maps.len(), 1, "the one kind it knows");
    assert_eq!(manifest.config_maps[0].keys, vec!["A".to_owned()]);
    // The fixture carries a Service of its own, and it is nowhere in the model.
    assert!(
        fixture("qa")
            .workloads
            .iter()
            .all(|held| held.kind != "Service")
    );
}

#[test]
fn a_block_scalar_and_a_base64_value_are_read_as_keys_and_never_as_values() {
    let manifest = fixture("qa");
    let config = manifest
        .config_maps
        .iter()
        .find(|held| held.name == "orders-config")
        .expect("the ConfigMap");
    assert_eq!(
        config.keys,
        vec![
            "BANNER".to_owned(),
            "LOG_LEVEL".to_owned(),
            "RATE_LIMIT_PER_MIN".to_owned()
        ]
    );
    // Nothing the render holds a value for reaches the model.
    let printed = serde_json::to_string(&manifest).unwrap();
    for value in ["two lines of it", "info", "cGxhY2Vob2xkZXI=", "placeholder"] {
        assert!(!printed.contains(value), "{value} reached the model");
    }
}

#[test]
fn a_render_that_fails_surfaces_the_renderers_own_last_word() {
    let directory = tempfile::tempdir().unwrap();
    let error = render(directory.path(), "nowhere", DEFAULT_RENDER)
        .expect_err("an overlay that is not there will not render");
    let message = format!("{error:#}");
    assert!(message.starts_with("nowhere: "), "{message}");
    assert!(
        message.to_lowercase().contains("no such file")
            || message.to_lowercase().contains("unable to find"),
        "kubectl's own words come back: {message}"
    );

    let missing = render(
        directory.path(),
        "nowhere",
        "definitely-not-a-renderer build",
    )
    .expect_err("a renderer that is not installed says so");
    assert_eq!(
        format!("{missing:#}"),
        "definitely-not-a-renderer is not installed or not on PATH"
    );
}

#[test]
fn globs_match_per_service_overlays_and_a_plain_path_is_itself() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for service in ["orders", "billing", "reaper"] {
        for environment in ["qa", "prod"] {
            std::fs::create_dir_all(
                root.join(format!("services/{service}/overlays/{environment}")),
            )
            .unwrap();
        }
    }
    std::fs::create_dir_all(root.join("base")).unwrap();

    assert_eq!(
        expand(root, "services/*/overlays/prod"),
        [
            "services/billing/overlays/prod",
            "services/orders/overlays/prod",
            "services/reaper/overlays/prod",
        ],
        "every service's, in a stable order"
    );
    assert_eq!(
        expand(root, "services/orders/overlays/qa"),
        ["services/orders/overlays/qa"],
        "a pattern with no star is the one path it spells"
    );
    assert!(
        expand(root, "services/*/overlays/dev").is_empty(),
        "a pattern nothing matches is no overlays rather than a guess"
    );
    assert!(
        expand(root, "base/kustomization.yaml").is_empty(),
        "only directories are overlays"
    );

    assert!(segment_matches("*", "anything"));
    assert!(segment_matches("orders-*", "orders-api"));
    assert!(segment_matches("*-api", "orders-api"));
    assert!(segment_matches("o*s-*i", "orders-api"));
    assert!(segment_matches("orders", "orders"));
    assert!(!segment_matches("orders", "orders-api"));
    assert!(!segment_matches("*-api", "orders-worker"));
    assert!(!segment_matches("a*a", "a"));
}

#[test]
fn a_missing_deployment_table_or_clone_reads_as_off_and_says_where_it_looked() {
    let directory = tempfile::tempdir().unwrap();
    let empty = crate::config::parse("").unwrap();
    assert_eq!(
        format!(
            "{:#}",
            deployment_clone(&empty, Some(directory.path())).unwrap_err()
        ),
        "no [deployment] in config.toml; add one naming the deployment repository"
    );

    let named = crate::config::parse("[deployment]\nrepo = \"deployment\"\n").unwrap();
    let error = format!(
        "{:#}",
        deployment_clone(&named, Some(directory.path())).unwrap_err()
    );
    assert!(
        error.starts_with("no clone of deployment in ")
            && error.ends_with(&directory.path().display().to_string()),
        "{error}"
    );
    assert!(
        format!("{:#}", deployment_clone(&named, None).unwrap_err()).contains("no workspace"),
        "with nowhere to look it says so rather than looking in the wrong place"
    );
}

#[test]
fn the_environments_of_the_file_parse_and_a_nameless_one_is_refused() {
    let config = crate::config::parse(concat!(
        "[deployment]\nrepo = \"deployment\"\nrender = \"kustomize build\"\n\n",
        "[[environments]]\nname = \"qa\"\noverlays = [\"overlays/qa\"]\nvault = \"kv-qa\"\n",
        "registry = \"acrqa\"\ncluster = \"qa\"\n\n",
        "[[environments]]\nname = \"prod\"\noverlays = [\"services/*/overlays/prod\"]\n",
    ))
    .unwrap();
    let deployment = config.deployment.as_ref().unwrap();
    assert_eq!(deployment.repo, "deployment");
    assert_eq!(deployment.render.as_deref(), Some("kustomize build"));
    assert_eq!(config.environments.len(), 2);
    assert_eq!(config.environments[0].vault.as_deref(), Some("kv-qa"));
    assert_eq!(
        config.environments[1].overlays,
        ["services/*/overlays/prod"]
    );
    assert_eq!(config.environments[1].vault, None);

    // Left out is the whole point: a file that says nothing about deployment
    // leaves the feature off.
    assert_eq!(crate::config::parse("[theme]\n").unwrap().deployment, None);
    assert!(
        crate::config::parse("[theme]\n")
            .unwrap()
            .environments
            .is_empty()
    );

    for (source, complaint) in [
        (
            "[deployment]\nrepo = \" \"\n",
            "deployment.repo is blank; give it a value or leave the table out",
        ),
        (
            "[[environments]]\nname = \"\"\noverlays = [\"a\"]\n",
            "environments[0] needs a name",
        ),
        (
            "[[environments]]\nname = \"qa\"\noverlays = []\n",
            "environments[0] needs at least one overlay",
        ),
        (
            "[[environments]]\nname = \"qa\"\noverlays = [\"a\"]\n[[environments]]\nname = \"qa\"\noverlays = [\"b\"]\n",
            "two environments are called \"qa\"",
        ),
    ] {
        assert_eq!(
            format!("{:#}", crate::config::parse(source).unwrap_err()),
            complaint
        );
    }
}

#[test]
fn prods_two_planted_findings_are_found_and_named_and_qa_is_clean() {
    let findings = check(&fixture("prod"));
    assert_eq!(
        findings.iter().map(ToString::to_string).collect::<Vec<_>>(),
        [
            "prod shop-prod/billing-api Deployment env SIGNING_KEY \u{2190} secret billing-kv key SIGNING_KEY missing",
            "prod shop-prod/orders-api Deployment env RATE_LIMIT_PER_MIN \u{2190} configmap orders-config key RATE_LIMIT_PER_MIN missing",
        ]
    );
    assert_eq!(findings[0].missing, Missing::Key);
    assert_eq!(findings[0].container.as_deref(), Some("api"));
    assert_eq!(findings[1].workload, "orders-api");

    assert!(
        check(&fixture("qa")).is_empty(),
        "{:?}",
        check(&fixture("qa"))
    );
}

#[test]
fn what_a_provider_produces_counts_as_present_and_the_vault_object_it_lacks_is_not_this_check() {
    let prod = fixture("prod");
    // billing-kv is nowhere in prod's rendered Secrets; the SecretProviderClass
    // is the only thing that says it will exist.
    assert!(!prod.secrets.iter().any(|held| held.name == "billing-kv"));
    let findings = check(&prod);
    assert!(
        !findings
            .iter()
            .any(|held| held.reference.key.as_deref() == Some("DB_PASSWORD")),
        "a key the class produces is not a finding"
    );
    // billing-legacy-token is asked of a vault that does not hold it, which is
    // #746's to find against the Key Vault tab and not this check's.
    assert!(
        prod.providers.iter().any(|provider| provider
            .objects
            .iter()
            .any(|object| object.name == "billing-legacy-token")),
        "it is in the render"
    );
    assert!(
        !findings
            .iter()
            .any(|held| held.to_string().contains("legacy")),
        "and `env check` stays quiet about it"
    );
    // dataFrom means the ExternalSecret's keys are unknowable, so nothing that
    // reads one is called missing.
    let reaper = check(&prod)
        .into_iter()
        .find(|held| held.workload == "reaper");
    assert!(reaper.is_none(), "{reaper:?}");
}

#[test]
fn an_absent_object_a_missing_class_and_an_optional_reference_read_as_they_should() {
    let manifest = parse(
        "qa",
        concat!(
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n  namespace: n\n",
            "spec:\n  template:\n    spec:\n      containers:\n      - name: web\n",
            "        image: web:1\n",
            "        envFrom:\n        - configMapRef:\n            name: nowhere\n",
            "        - secretRef:\n            name: also-nowhere\n            optional: true\n",
            "        env:\n        - name: A\n          valueFrom:\n",
            "            secretKeyRef:\n              name: gone\n              key: A\n",
            "      volumes:\n      - name: whole\n        configMap:\n          name: nothing\n",
            "      - name: vault\n        csi:\n          volumeAttributes:\n",
            "            secretProviderClass: not-here\n",
        ),
    );
    let lines: Vec<String> = check(&manifest).iter().map(ToString::to_string).collect();
    assert_eq!(
        lines,
        [
            "qa n/web Deployment env A \u{2190} secret gone missing",
            "qa n/web Deployment envFrom \u{2190} configmap nowhere missing",
            "qa n/web Deployment volume whole \u{2190} configmap nothing missing",
            "qa n/web Deployment volume vault \u{2190} secretproviderclass not-here missing",
        ],
        "the optional envFrom is silent, and everything else names itself"
    );
    assert!(
        check(&manifest)
            .iter()
            .all(|held| held.missing == Missing::Object)
    );
}

/// The one test that runs the real renderer: it re-renders the fixture and
/// compares it to the YAML checked in beside it, so a kustomize that changes
/// its output is caught rather than quietly parsed. `cargo test -- --ignored
/// kustomize` runs it.
#[test]
#[ignore = "needs kubectl on PATH"]
fn the_checked_in_render_is_what_kubectl_kustomize_still_produces() {
    for environment in ["qa", "prod"] {
        let rendered = render(
            &fixtures(),
            &format!("overlays/{environment}"),
            DEFAULT_RENDER,
        )
        .expect("kubectl kustomize");
        let stored =
            std::fs::read_to_string(fixtures().join(format!("rendered/{environment}.yaml")))
                .unwrap();
        assert_eq!(
            rendered, stored,
            "fixtures/kustomize/rendered/{environment}.yaml is stale; re-render it"
        );
        assert_eq!(parse(environment, &rendered), fixture(environment));
    }
}
