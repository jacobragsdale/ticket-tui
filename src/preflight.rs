//! Pre-flight: what a pull request against the deployment repository would
//! leave an environment missing, answered while it is still a pull request.
//!
//! The cheapest moment to catch a missing key is before the merge. The head
//! the pull request was read at is checked out into a scratch worktree of its
//! own, only the overlays the change touches are rendered there, and the same
//! check `ticket-tui env check` runs is run over the result — the branch's
//! tree rather than the clone's, and nothing of the clone's own state is
//! disturbed.
//!
//! It never blocks. A pull request may be approved or completed with findings;
//! the pane says what will be missing and the vote is the reviewer's. A gate
//! belongs in the deployment repository's own pipeline, where `env check`
//! exits 1.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{Config, Environment};
use crate::kustomize::{self, EnvManifest, Finding, ObjectKind};
use crate::local;
use crate::model::Jump;

/// The names kustomize reads a directory's own file under, in its own order.
const KUSTOMIZATION: [&str; 3] = ["kustomization.yaml", "kustomization.yml", "Kustomization"];

/// What a pre-flight needs, settled once when `config.toml` is read: the
/// repository `[deployment]` names, the clone of it on this machine, what
/// renders an overlay, and the environments to check.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Deployment {
    /// The repository, as the Repos tab names it.
    pub repo: String,
    pub clone: PathBuf,
    pub render: String,
    pub environments: Vec<Environment>,
}

impl Deployment {
    /// What the file names, or nothing at all: no `[deployment]`, no
    /// environments to check, or no clone of it on this machine — in which
    /// case there is nothing to pre-fly and the column stays blank.
    ///
    /// ponytail: the clone is found once per read of `config.toml`, so a
    /// deployment repository cloned mid-run is picked up when the file is next
    /// touched. Rescan per pre-flight if that ever bites.
    #[must_use]
    pub fn resolve(config: &Config, workspace: Option<&Path>) -> Option<Self> {
        let deployment = config.deployment.as_ref()?;
        if config.environments.is_empty() {
            return None;
        }
        Some(Self {
            repo: deployment.repo.clone(),
            clone: kustomize::deployment_clone(config, workspace).ok()?,
            render: deployment
                .render
                .clone()
                .unwrap_or_else(|| kustomize::DEFAULT_RENDER.to_owned()),
            environments: config.environments.clone(),
        })
    }

    /// Whether a pull request against this repository is one to pre-fly.
    #[must_use]
    pub fn covers(&self, repo: &str) -> bool {
        repo.eq_ignore_ascii_case(&self.repo)
    }
}

/// Where one pull request's pre-flight has got to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Preflight {
    /// The worktree is out and the overlays are rendering.
    Running,
    Ready(Report),
    /// It could not be looked at, in the one line that says why: the head is
    /// not here, or the renderer refused an overlay.
    Failed(String),
}

/// What one pre-flight found: which overlays were rendered, and what they ask
/// for that they do not answer.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Report {
    /// The overlays rendered, as `(environment, overlay)`, in the order the
    /// file lists the environments.
    pub rendered: Vec<(String, String)>,
    pub findings: Vec<Finding>,
}

/// What one line of the Pre-flight section is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mark {
    Running,
    /// An overlay that renders with nothing missing.
    Clean,
    Missing,
    Failed,
}

/// One line of the Pre-flight section: what it says, and where it points when
/// it names something another tab holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Note {
    pub mark: Mark,
    pub text: String,
    pub jump: Option<Jump>,
}

impl Note {
    fn plain(mark: Mark, text: String) -> Self {
        Self {
            mark,
            text,
            jump: None,
        }
    }
}

impl Report {
    /// The Pre-flight section, line by line: for each environment the change
    /// touches, either what it would be missing or that its overlays render
    /// clean.
    #[must_use]
    pub fn notes(&self, deployment: &Deployment) -> Vec<Note> {
        if self.rendered.is_empty() {
            return vec![Note::plain(
                Mark::Clean,
                "Nothing it changes reaches an environment".to_owned(),
            )];
        }
        let mut notes = Vec::new();
        let mut said: Vec<&str> = Vec::new();
        for (environment, overlay) in &self.rendered {
            let mine: Vec<&Finding> = self
                .findings
                .iter()
                .filter(|finding| finding.environment == *environment)
                .collect();
            if mine.is_empty() {
                notes.push(Note::plain(
                    Mark::Clean,
                    format!("{environment} {overlay} renders clean"),
                ));
                continue;
            }
            // The findings are the environment's rather than one overlay's, so
            // they are said once however many of its overlays were rendered.
            if said.contains(&environment.as_str()) {
                continue;
            }
            said.push(environment);
            notes.extend(mine.into_iter().map(|finding| Note {
                mark: Mark::Missing,
                text: finding.to_string(),
                jump: vault_jump(deployment, finding),
            }));
        }
        notes.extend(promotion(self).into_iter().flatten());
        notes
    }

    /// How many things would be missing, which is what the column counts.
    #[must_use]
    pub fn missing(&self) -> usize {
        self.findings.len()
    }
}

/// What this pull request adds to the environments it touches: the target
/// branch's render of the same overlay against the source's, in the words of
/// the board — `this pull request adds RATE_LIMIT_PER_MIN to prod/orders-config`.
///
/// TODO(#747): `env diff` lands the comparison itself; this is the hook it is
/// wired into, and until then the pane says only what would be missing.
fn promotion(_report: &Report) -> Option<Vec<Note>> {
    None
}

/// Where a finding points. A key an overlay never defines is a question for
/// the vault the environment pulls its secrets from, which is the tab that
/// answers it.
///
/// TODO(#746): the vault check names the vault object itself, which is a
/// `Jump::VaultItem` rather than the vault as a whole.
fn vault_jump(deployment: &Deployment, finding: &Finding) -> Option<Jump> {
    if finding.reference.object != ObjectKind::Secret {
        return None;
    }
    deployment
        .environments
        .iter()
        .find(|environment| environment.name == finding.environment)?
        .vault
        .clone()
        .map(Jump::Vault)
}

/// One pull request, pre-flown: fetch what it is, put its head in a scratch
/// worktree, render the overlays it touches there and check them. The worktree
/// goes however this leaves.
pub fn run(deployment: &Deployment, source: &str, target: &str, commit: &str) -> Result<Report> {
    let clone = deployment.clone.as_path();
    // Best effort: what has to be here is the head the row was read at, and
    // `git worktree add` says precisely when it is not. A clone with no remote
    // to reach — the fixture repository the tests build — pre-flies from what
    // it already has.
    let _ = local::remote_git(clone, &["fetch", "origin", source, target]);
    let changed = changed_files(clone, target, commit)?;
    let scratch = Scratch::add(clone, commit)?;
    let mut rendered = Vec::new();
    let mut manifests: BTreeMap<String, EnvManifest> = BTreeMap::new();
    for (environment, overlay) in touched(&scratch.path, &deployment.environments, &changed) {
        let yaml = kustomize::render(&scratch.path, &overlay, &deployment.render)?;
        let mut parsed = kustomize::parse(&environment, &yaml);
        parsed.overlays.push(overlay.clone());
        manifests
            .entry(environment.clone())
            .or_insert_with(|| EnvManifest::named(&environment))
            .absorb(parsed);
        rendered.push((environment, overlay));
    }
    let mut findings = Vec::new();
    for manifest in manifests.values_mut() {
        manifest.tidy();
        findings.extend(kustomize::check(manifest));
    }
    Ok(Report { rendered, findings })
}

/// Which overlays a change puts in question. Each changed file walks up to its
/// nearest `kustomization.yaml`, and that directory is an overlay when an
/// environment's globs name it; a file under `base/` is under everything, so it
/// puts every overlay of every environment in question.
#[must_use]
pub fn touched(
    tree: &Path,
    environments: &[Environment],
    changed: &[String],
) -> Vec<(String, String)> {
    let all: Vec<(String, String)> = environments
        .iter()
        .flat_map(|environment| {
            environment
                .overlays
                .iter()
                .flat_map(|pattern| kustomize::expand(tree, pattern))
                .map(|overlay| (environment.name.clone(), overlay))
        })
        .collect();
    if changed.iter().any(|file| under_base(file)) {
        return all;
    }
    let mut hit: Vec<String> = changed
        .iter()
        .filter_map(|file| nearest_kustomization(tree, file))
        .collect();
    hit.sort();
    hit.dedup();
    all.into_iter()
        .filter(|(_, overlay)| hit.contains(overlay))
        .collect()
}

/// Whether a path is under a `base/` directory, which every overlay that
/// builds on it renders.
fn under_base(file: &str) -> bool {
    let mut parts: Vec<&str> = file.split('/').collect();
    parts.pop();
    parts.contains(&"base")
}

/// The nearest directory above a file that kustomize would build, relative to
/// the tree; nothing when no directory above it has a kustomization at all.
fn nearest_kustomization(tree: &Path, file: &str) -> Option<String> {
    let mut parts: Vec<&str> = file.split('/').collect();
    parts.pop();
    while !parts.is_empty() {
        let directory = parts.join("/");
        if KUSTOMIZATION
            .iter()
            .any(|name| tree.join(&directory).join(name).is_file())
        {
            return Some(directory);
        }
        parts.pop();
    }
    None
}

/// What the pull request changes: everything between where it left the target
/// branch and the head it was read at.
fn changed_files(clone: &Path, target: &str, commit: &str) -> Result<Vec<String>> {
    let range = format!("{}...{commit}", target_ref(clone, target));
    let output = local::git(clone, &["diff", "--name-only", &range])
        .with_context(|| format!("could not read what {commit} changes"))?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The target branch as this clone has it: the remote-tracking ref the fetch
/// just moved, or the local branch in a clone with no remote.
fn target_ref(clone: &Path, target: &str) -> String {
    let remote = format!("origin/{target}");
    let commit = format!("{remote}^{{commit}}");
    if local::git(clone, &["rev-parse", "--verify", "--quiet", &commit]).is_ok() {
        remote
    } else {
        target.to_owned()
    }
}

/// The scratch worktree the render happens in, removed however the pre-flight
/// leaves: a render that fails, a check that panics, an early return.
struct Scratch<'a> {
    clone: &'a Path,
    path: PathBuf,
}

impl<'a> Scratch<'a> {
    fn add(clone: &'a Path, commit: &str) -> Result<Self> {
        let name: String = commit
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(12)
            .collect();
        let path = std::env::temp_dir().join(format!(
            "ticket-tui-preflight-{}-{name}",
            std::process::id()
        ));
        // Whatever a run that was killed left behind is git's to forget before
        // the path can be used again.
        let _ = local::git(clone, &["worktree", "prune"]);
        let _ = std::fs::remove_dir_all(&path);
        local::git(
            clone,
            &[
                "worktree",
                "add",
                "--detach",
                &path.to_string_lossy(),
                commit,
            ],
        )
        .with_context(|| format!("could not check {commit} out"))?;
        Ok(Self { clone, path })
    }
}

impl Drop for Scratch<'_> {
    fn drop(&mut self) {
        let _ = local::git(
            self.clone,
            &[
                "worktree",
                "remove",
                "--force",
                &self.path.to_string_lossy(),
            ],
        );
    }
}

#[cfg(test)]
mod tests;
