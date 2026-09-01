//! A deployment repository built in a temp directory: `fixtures/kustomize` on
//! `main`, and a branch that takes a key out of qa's ConfigMap.
//!
//! The renderer is a one-line shell script that cats the rendered YAML checked
//! in beside the overlays, read out of whichever tree it is pointed at — so
//! the suite needs no `kubectl`, and a branch that changes the render is a
//! branch that changes what the check sees.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::kustomize::{Missing, Reference, Source};

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

/// The repository the tests fly at: the two commits `main` holds, and the
/// branch that takes the key out.
struct Fixture {
    directory: TempDir,
    root: String,
    main: String,
    branch: String,
    render: String,
}

impl Fixture {
    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn deployment(&self) -> Deployment {
        Deployment {
            repo: "deployment".to_owned(),
            clone: self.path().to_path_buf(),
            render: self.render.clone(),
            environments: vec![environment("qa", "kv-qa")],
        }
    }
}

fn git(at: &Path, arguments: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(at)
        .args(arguments)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .output()
        .expect("git could not be run");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("the directory");
    for entry in fs::read_dir(from).expect("the fixture").flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("the file");
        }
    }
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("a temp directory");
    let path = directory.path().to_path_buf();
    copy_tree(&fixtures(), &path);
    // Written into the first commit so two of these built in the same second
    // never share a commit — and so never share a scratch worktree.
    fs::write(path.join("fixture.txt"), path.display().to_string()).expect("the marker");
    let script = path.join("render.sh");
    fs::write(
        &script,
        "cat \"$1/../../rendered/$(basename \"$1\").yaml\"\n",
    )
    .expect("the renderer");
    git(&path, &["init", "--initial-branch=main"]);
    git(&path, &["add", "-A"]);
    git(&path, &["commit", "-m", "the fixture as it stands"]);
    let root = git(&path, &["rev-parse", "HEAD"]);

    let kustomization = path.join("overlays/qa/kustomization.yaml");
    let note = |text: &str| {
        let held = fs::read_to_string(&kustomization).expect("the kustomization");
        fs::write(&kustomization, format!("{held}\n# {text}\n")).expect("the kustomization");
    };
    note("qa is looked after here");
    git(&path, &["commit", "-am", "say where qa is looked after"]);
    let main = git(&path, &["rev-parse", "HEAD"]);

    git(&path, &["checkout", "-b", "drop-the-key"]);
    let rendered = path.join("rendered/qa.yaml");
    let yaml = fs::read_to_string(&rendered)
        .expect("the render")
        .replace("  RATE_LIMIT_PER_MIN: \"60\"\n", "");
    fs::write(&rendered, yaml).expect("the render");
    note("the rate limit moved to the vault");
    git(&path, &["commit", "-am", "take the rate limit out of qa"]);
    let branch = git(&path, &["rev-parse", "HEAD"]);
    git(&path, &["checkout", "main"]);

    Fixture {
        directory,
        root,
        main,
        branch,
        render: format!("sh {}", script.display()),
    }
}

#[test]
fn a_change_walks_up_to_its_overlay_and_a_change_under_base_is_under_every_one() {
    let fixture = fixture();
    let environments = [environment("qa", "kv-qa"), environment("prod", "kv-prod")];
    let qa = ("qa".to_owned(), "overlays/qa".to_owned());
    let prod = ("prod".to_owned(), "overlays/prod".to_owned());

    assert_eq!(
        touched(
            fixture.path(),
            &environments,
            &["overlays/qa/orders-config-patch.yaml".to_owned()]
        ),
        vec![qa.clone()],
        "a file walks up to the nearest kustomization, which is one overlay"
    );
    assert_eq!(
        touched(
            fixture.path(),
            &environments,
            &["base/orders-config.yaml".to_owned()]
        ),
        vec![qa, prod],
        "a change under base/ is under every overlay of every environment"
    );
    assert!(
        touched(fixture.path(), &environments, &["fixture.txt".to_owned()]).is_empty(),
        "a file no kustomization is above touches no overlay at all"
    );
}

#[test]
fn the_branch_would_leave_a_key_missing_and_the_same_overlay_on_main_would_not() {
    let fixture = fixture();
    let deployment = fixture.deployment();

    let flown =
        run(&deployment, "drop-the-key", "main", &fixture.branch).expect("the branch flies");
    assert_eq!(
        flown.rendered,
        vec![("qa".to_owned(), "overlays/qa".to_owned())],
        "only the overlay it touches is rendered"
    );
    assert_eq!(flown.missing(), 1, "{:?}", flown.findings);
    let said = flown.findings[0].to_string();
    assert!(
        said.contains("qa") && said.contains("RATE_LIMIT_PER_MIN") && said.contains("missing"),
        "{said}"
    );

    // The same overlay as main has it renders with nothing missing.
    let held = run(&deployment, "main", &fixture.root, &fixture.main).expect("main flies");
    assert_eq!(held.rendered.len(), 1, "the same overlay was rendered");
    assert!(held.findings.is_empty(), "{:?}", held.findings);
}

#[test]
fn the_scratch_worktree_goes_even_when_the_render_fails() {
    let fixture = fixture();
    let mut deployment = fixture.deployment();
    deployment.render = "false".to_owned();

    let refused = run(&deployment, "drop-the-key", "main", &fixture.branch)
        .expect_err("a renderer that refuses is an error");
    assert!(
        format!("{refused:#}").contains("overlays/qa"),
        "{refused:#}"
    );

    let worktrees = local::git(fixture.path(), &["worktree", "list"]).expect("git worktree list");
    assert_eq!(
        worktrees.lines().count(),
        1,
        "only the clone itself is left: {worktrees}"
    );
}

#[test]
fn a_missing_key_points_at_the_vault_the_environment_pulls_from() {
    let fixture = fixture();
    let deployment = fixture.deployment();
    let secret = Finding {
        environment: "qa".to_owned(),
        namespace: "shop-qa".to_owned(),
        workload: "billing-api".to_owned(),
        kind: "Deployment".to_owned(),
        container: Some("api".to_owned()),
        reference: Reference {
            source: Source::Env {
                var: "SIGNING_KEY".to_owned(),
            },
            object: ObjectKind::Secret,
            name: "billing-kv".to_owned(),
            key: Some("SIGNING_KEY".to_owned()),
            optional: false,
        },
        missing: Missing::Key,
        vault: None,
    };
    let mut configmap = secret.clone();
    configmap.reference.object = ObjectKind::ConfigMap;

    let report = Report {
        rendered: vec![("qa".to_owned(), "overlays/qa".to_owned())],
        findings: vec![secret, configmap],
    };
    let notes = report.notes(&deployment);
    assert_eq!(notes.len(), 2);
    assert_eq!(
        notes[0].jump,
        Some(Jump::Vault("kv-qa".to_owned())),
        "a secret a key is missing from is a question for the vault"
    );
    assert_eq!(
        notes[1].jump, None,
        "a ConfigMap has no tab to jump to, so it is a plain line"
    );
    assert!(notes.iter().all(|note| note.mark == Mark::Missing));

    let clean = Report {
        rendered: vec![("qa".to_owned(), "overlays/qa".to_owned())],
        findings: Vec::new(),
    };
    let notes = clean.notes(&deployment);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].mark, Mark::Clean);
    assert!(notes[0].text.contains("renders clean"), "{}", notes[0].text);
}
