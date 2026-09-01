//! Pre-flight: what a pull request against the deployment repository would
//! leave an environment missing, answered while it is still a pull request.
//!
//! The cheapest moment to catch a missing key is before the merge. The head
//! the pull request was read at is checked out into a scratch worktree of its
//! own, only the overlays the change touches are rendered there, and the same
//! check `ticket-tui env check` runs is run over the result — the branch's
//! tree rather than the clone's, and nothing of the clone's own state is
//! disturbed. The target branch's own render of the same overlays goes into a
//! second scratch worktree beside it, so the pane can also say what the merge
//! would *change*, not only what it would leave missing.
//!
//! Nothing here reaches a vault. What the environment's vault holds is read on
//! the app side, by the tab that reads vaults, and handed in as names — so the
//! half that needs a token is somebody else's, and this stays a function of a
//! repository and a commit.
//!
//! It never blocks. A pull request may be approved or completed with findings;
//! the pane says what will be missing and the vote is the reviewer's. A gate
//! belongs in the deployment repository's own pipeline, where `env check`
//! exits 1.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{Config, Environment};
use crate::kustomize::diff::{self, Names};
use crate::kustomize::{self, EnvManifest, Finding, Missing, ObjectKind, Source, VaultNames};
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

/// What one pre-flight found: which overlays were rendered, what they ask for
/// that they do not answer, and the two renders that say what the merge would
/// change.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Report {
    /// The overlays rendered, as `(environment, overlay)`, in the order the
    /// file lists the environments.
    pub rendered: Vec<(String, String)>,
    pub findings: Vec<Finding>,
    /// Each environment the change touches as the target branch has it, and
    /// as the head the pull request was read at has it. The two read against
    /// each other are what merging would do; `before` is empty when the target
    /// branch could not be rendered, and the pane then says only what would be
    /// missing.
    pub before: Vec<(String, EnvManifest)>,
    pub after: Vec<(String, EnvManifest)>,
    /// The vaults the findings were answered against. An environment whose
    /// vault is not here was checked against its own repository alone, which
    /// is what the Key Vault tab not having read it yet leaves.
    pub vaults: Vec<String>,
}

/// What one line of the Pre-flight section is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mark {
    Running,
    /// An overlay that renders with nothing missing.
    Clean,
    Missing,
    /// A vault object in use that is about to lapse: said, not counted.
    Expiring,
    /// Something the merge would change rather than leave missing.
    Change,
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
                mark: if matches!(finding.missing, crate::kustomize::Missing::Expiring { .. }) {
                    Mark::Expiring
                } else {
                    Mark::Missing
                },
                text: finding.to_string(),
                jump: finding_jump(&deployment.environments, finding),
            }));
        }
        notes.extend(self.unread_vaults(deployment));
        notes.extend(promotion(self).into_iter().flatten());
        notes
    }

    /// The environments whose vault nobody has read, said once each: the
    /// overlays answered for themselves and the vault half is still open.
    fn unread_vaults(&self, deployment: &Deployment) -> Vec<Note> {
        let mut said: Vec<&str> = Vec::new();
        let mut notes = Vec::new();
        for (environment, _) in &self.rendered {
            let Some(vault) = deployment
                .environments
                .iter()
                .find(|held| held.name == *environment)
                .and_then(|held| held.vault.as_deref())
            else {
                continue;
            };
            if said.contains(&environment.as_str())
                || self
                    .vaults
                    .iter()
                    .any(|held| held.eq_ignore_ascii_case(vault))
            {
                continue;
            }
            said.push(environment);
            notes.push(Note::plain(
                Mark::Failed,
                format!("{environment}: {vault} not read, so the overlays answer alone"),
            ));
        }
        notes
    }

    /// How many things would be missing, which is what the column counts.
    #[must_use]
    pub fn missing(&self) -> usize {
        // What the merge would leave missing; an object that is merely
        // expiring is a line in the pane, not a count in the column.
        self.findings
            .iter()
            .filter(|finding| {
                !matches!(finding.missing, crate::kustomize::Missing::Expiring { .. })
            })
            .count()
    }
}

/// What this pull request would change in the environments it touches: the
/// target branch's render of the same overlays read against the head's, in the
/// words the environments board uses — `this pull request adds
/// RATE_LIMIT_PER_MIN to prod/orders-config`.
///
/// The diff is taken as `diff(what is arriving, what is there)`, which is the
/// way round [`diff::promotion_lines`] reads for the board too, so one wording
/// serves both.
fn promotion(report: &Report) -> Option<Vec<Note>> {
    let mut notes = Vec::new();
    for (environment, after) in &report.after {
        let Some((_, before)) = report.before.iter().find(|(held, _)| held == environment) else {
            continue;
        };
        let promotion = diff::diff(after, before, None);
        for service in &promotion.services {
            notes.extend(
                diff::promotion_lines(&promotion, service)
                    .into_iter()
                    .map(|line| Note {
                        mark: Mark::Change,
                        text: format!("this pull request {}", line.text),
                        // The vault as a whole: a line saying an object is now
                        // pulled names the vault to go and look in, and the kind
                        // it is filed under is the vault's own business.
                        jump: match line.names {
                            Names::VaultObject { vault, .. } => Some(Jump::Vault(vault)),
                            _ => None,
                        },
                    }),
            );
        }
    }
    (!notes.is_empty()).then_some(notes)
}

/// Where a finding points. The vault half names the object itself, which is a
/// row on the Key Vault tab; the repository half names a Secret, which is a
/// question for the vault the environment pulls its secrets from as a whole.
#[must_use]
pub fn finding_jump(environments: &[Environment], finding: &Finding) -> Option<Jump> {
    if finding.reference.object == ObjectKind::Vault {
        let vault = finding.vault.clone()?;
        // A provider pulling from the wrong vault names that vault rather than
        // an object in it, and an `ExternalSecret` says no kind at all, so
        // neither is a row this can point at.
        return Some(
            match (finding.missing, item_kind(&finding.reference.source)) {
                (Missing::WrongVault, _) | (_, None) => Jump::Vault(vault),
                (_, Some(kind)) => Jump::VaultItem {
                    vault,
                    kind,
                    name: finding.reference.name.clone(),
                },
            },
        );
    }
    if finding.reference.object != ObjectKind::Secret {
        return None;
    }
    environments
        .iter()
        .find(|environment| environment.name == finding.environment)?
        .vault
        .clone()
        .map(Jump::Vault)
}

/// What the Key Vault tab files a vault object under, out of the `objectType`
/// a provider wrote. `certificate` and `cert` are the same kind spelled two
/// ways; anything else is a kind this cannot name.
fn item_kind(source: &Source) -> Option<String> {
    let Source::Vault { kind } = source else {
        return None;
    };
    match kind.as_deref()? {
        "secret" => Some("secret".to_owned()),
        "key" => Some("key".to_owned()),
        "cert" | "certificate" => Some("cert".to_owned()),
        _ => None,
    }
}

/// One pull request, pre-flown: fetch what it is, put its head in a scratch
/// worktree, render the overlays it touches there, render the same overlays as
/// the target branch has them beside it, and check the result. `vaults` is
/// what whoever has already read a vault holds of it, by name — nothing here
/// reaches one — and an environment with none is answered against its own
/// repository alone. Both worktrees go however this leaves.
pub fn run(
    deployment: &Deployment,
    source: &str,
    target: &str,
    commit: &str,
    vaults: &[VaultNames],
) -> Result<Report> {
    let clone = deployment.clone.as_path();
    // Best effort: what has to be here is the head the row was read at, and
    // `git worktree add` says precisely when it is not. A clone with no remote
    // to reach — the fixture repository the tests build — pre-flies from what
    // it already has.
    let _ = local::remote_git(clone, &["fetch", "origin", source, target]);
    // The target as a commit rather than as a name: a scratch worktree is
    // named after what it holds, and two of them may not share a name.
    let target = commit_of(clone, &target_ref(clone, target));
    let changed = changed_files(clone, &target, commit)?;
    let scratch = Scratch::add(clone, commit)?;
    let rendered = touched(&scratch.path, &deployment.environments, &changed);
    let after = render_touched(deployment, &scratch.path, &rendered)?;
    // What the target branch already says, so the pane can name the change
    // rather than only the gap. A branch whose overlays will not render there
    // — one this pull request adds — leaves this empty, and the promotion half
    // is simply not said.
    let before = if target == commit {
        after.clone()
    } else {
        Scratch::add(clone, &target)
            .ok()
            .and_then(|base| render_touched(deployment, &base.path, &rendered).ok())
            .unwrap_or_default()
    };
    let mut findings = Vec::new();
    let mut read = Vec::new();
    for (environment, manifest) in &after {
        let vault = deployment
            .environments
            .iter()
            .find(|held| held.name == *environment)
            .and_then(|held| held.vault.as_deref())
            .and_then(|name| {
                vaults
                    .iter()
                    .find(|held| held.vault.eq_ignore_ascii_case(name))
            });
        if let Some(vault) = vault {
            read.push(vault.vault.clone());
        }
        findings.extend(kustomize::check_with(manifest, vault));
    }
    Ok(Report {
        rendered,
        findings,
        before,
        after,
        vaults: read,
    })
}

/// Every overlay in `touched`, rendered out of one tree and unioned into one
/// manifest per environment.
fn render_touched(
    deployment: &Deployment,
    tree: &Path,
    touched: &[(String, String)],
) -> Result<Vec<(String, EnvManifest)>> {
    let mut manifests: BTreeMap<String, EnvManifest> = BTreeMap::new();
    for (environment, overlay) in touched {
        let yaml = kustomize::render(tree, overlay, &deployment.render)?;
        let mut parsed = kustomize::parse(environment, &yaml);
        parsed.overlays.push(overlay.clone());
        manifests
            .entry(environment.clone())
            .or_insert_with(|| EnvManifest::named(environment))
            .absorb(parsed);
    }
    Ok(manifests
        .into_iter()
        .map(|(name, mut manifest)| {
            manifest.tidy();
            (name, manifest)
        })
        .collect())
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
/// branch and the head it was read at. `target` is the ref as this clone has
/// it, which [`target_ref`] has already settled.
fn changed_files(clone: &Path, target: &str, commit: &str) -> Result<Vec<String>> {
    let range = format!("{target}...{commit}");
    let output = local::git(clone, &["diff", "--name-only", &range])
        .with_context(|| format!("could not read what {commit} changes"))?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// What a ref points at, or the ref itself where git will not say — which is a
/// ref that is not here at all, and `git worktree add` says so better than
/// this could.
fn commit_of(clone: &Path, reference: &str) -> String {
    local::git(clone, &["rev-parse", reference])
        .ok()
        .map(|head| head.trim().to_owned())
        .filter(|head| !head.is_empty())
        .unwrap_or_else(|| reference.to_owned())
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
        let mine = format!("ticket-tui-preflight-{}-", std::process::id());
        let path = std::env::temp_dir().join(format!("{mine}{name}"));
        // Whatever a run that was killed left behind — under any pid but this
        // one's, whose flights are live — goes first, and then git forgets the
        // registrations whose directories are gone.
        if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
            for entry in entries.flatten() {
                let held = entry.file_name();
                let held = held.to_string_lossy();
                if held.starts_with("ticket-tui-preflight-") && !held.starts_with(&mine) {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
        let _ = std::fs::remove_dir_all(&path);
        let _ = local::git(clone, &["worktree", "prune"]);
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
