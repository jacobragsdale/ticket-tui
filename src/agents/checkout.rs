//! Where an agent's checkout is: the clone the Repos tab would find, or the
//! path the file names, and — by default — a git worktree of its own for the
//! ticket's branch, so two agents in one repository never share a working
//! tree. Nothing here resets, cleans, deletes or pushes: a worktree is added,
//! a branch is made when there is none, and that is the whole of it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::local::{self, RepoKey, git};

/// Whether each launch gets a worktree of its own or works in the clone.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckoutPolicy {
    #[default]
    Worktree,
    Shared,
}

impl CheckoutPolicy {
    #[must_use]
    pub fn parse(word: Option<&str>) -> Self {
        match word {
            Some("shared") => Self::Shared,
            _ => Self::Worktree,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Worktree => "worktree",
            Self::Shared => "shared",
        }
    }
}

/// Where the agent works, and how it got there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Checkout {
    /// The clone the repository was found at.
    pub clone: PathBuf,
    /// The directory the agent is started in: the clone, or a worktree.
    pub workdir: PathBuf,
    pub branch: String,
    pub policy: CheckoutPolicy,
    /// What was done to get here, for the status line.
    pub note: String,
}

/// The clone of one repository on this machine: the path the file names for
/// it, else what the workspace scan claims for it. `None` is a repository
/// that is not here, which the caller turns into a clone.
#[must_use]
pub fn find_clone(
    workspace_root: Option<&Path>,
    path_override: Option<&Path>,
    repo: &RepoKey,
) -> Option<PathBuf> {
    if let Some(path) = path_override {
        return path.join(".git").exists().then(|| path.to_path_buf());
    }
    let root = workspace_root?;
    local::scan(root, std::slice::from_ref(repo))
        .into_iter()
        .find(|(id, _)| *id == repo.id)
        .map(|(_, found)| found.path)
}

/// Settles the checkout an agent is started in. With the shared policy that
/// is the clone as it stands, on whatever branch it is on. With the worktree
/// policy it is a worktree for `branch`: the one already holding that branch
/// when there is one — the clone itself included — else one added under
/// `<clone>/../.worktrees/<repo>/<branch>` from the branch as it exists
/// locally, on the remote, or newly made from `base`.
pub fn settle(
    clone: &Path,
    repo_name: &str,
    branch: &str,
    base: &str,
    policy: CheckoutPolicy,
    fetch_missing: bool,
) -> Result<Checkout> {
    // As git spells it, so a worktree read back from `git worktree list`
    // compares equal to one this made: on macOS `/var` is `/private/var`.
    let canonical = clone.canonicalize().unwrap_or_else(|_| clone.to_path_buf());
    let clone = canonical.as_path();
    if policy == CheckoutPolicy::Shared {
        let current = git(clone, &["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|out| out.trim().to_owned())
            .unwrap_or_default();
        return Ok(Checkout {
            clone: clone.to_path_buf(),
            workdir: clone.to_path_buf(),
            branch: current,
            policy,
            note: "in the clone as it stands".to_owned(),
        });
    }
    if let Some(path) = worktree_holding(clone, branch)? {
        return Ok(Checkout {
            clone: clone.to_path_buf(),
            workdir: path,
            branch: branch.to_owned(),
            policy,
            note: format!("in the worktree already on {branch}"),
        });
    }
    let path = worktree_path(clone, repo_name, branch);
    if path.exists() {
        bail!(
            "{} exists but is not a worktree of {}; move it aside",
            path.display(),
            clone.display()
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to make {}", parent.display()))?;
    }
    let path_text = path.to_string_lossy().into_owned();
    let local_branch = format!("refs/heads/{branch}");
    let remote_branch = format!("refs/remotes/origin/{branch}");
    let note = if ref_exists(clone, &local_branch) {
        git(clone, &["worktree", "add", &path_text, branch])?;
        format!("worktree added on the local branch {branch}")
    } else {
        // A branch linked from Azure DevOps was very likely made there
        // minutes ago, so the clone is given one chance to hear about it
        // before a competing branch of the same name is made here.
        if fetch_missing && !ref_exists(clone, &remote_branch) {
            let _ = local::remote_git(clone, &["fetch", "origin", branch]);
        }
        if ref_exists(clone, &remote_branch) {
            git(
                clone,
                &[
                    "worktree",
                    "add",
                    "--track",
                    "-b",
                    branch,
                    &path_text,
                    &format!("origin/{branch}"),
                ],
            )?;
            format!("worktree added tracking origin/{branch}")
        } else if fetch_missing {
            bail!(
                "{branch} is linked to the work item but is neither in the clone nor on origin; fetch it or unlink it first"
            );
        } else {
            let base = base.strip_prefix("refs/heads/").unwrap_or(base);
            let start = if ref_exists(clone, &format!("refs/remotes/origin/{base}")) {
                format!("origin/{base}")
            } else {
                base.to_owned()
            };
            git(
                clone,
                &["worktree", "add", "-b", branch, &path_text, &start],
            )?;
            format!("worktree added on new branch {branch} from {start}")
        }
    };
    Ok(Checkout {
        clone: clone.to_path_buf(),
        workdir: path,
        branch: branch.to_owned(),
        policy,
        note,
    })
}

/// `<clone>/../.worktrees/<repo>/<branch>`, beside the clone rather than in
/// it, and under a dot so the workspace scan never mistakes it for a clone.
#[must_use]
pub fn worktree_path(clone: &Path, repo_name: &str, branch: &str) -> PathBuf {
    clone
        .parent()
        .unwrap_or(clone)
        .join(".worktrees")
        .join(repo_name)
        .join(branch.replace('/', "-"))
}

/// The worktree of `clone` that has `branch` checked out — the clone itself
/// when it does — or `None`.
pub fn worktree_holding(clone: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let listing = git(clone, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktrees(&listing)
        .into_iter()
        .find(|(_, held)| held.as_deref() == Some(branch))
        .map(|(path, _)| path))
}

/// `git worktree list --porcelain`, read as (path, branch) pairs; a detached
/// worktree has no branch.
#[must_use]
pub fn parse_worktrees(listing: &str) -> Vec<(PathBuf, Option<String>)> {
    let mut found = Vec::new();
    let mut current: Option<(PathBuf, Option<String>)> = None;
    for line in listing.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(done) = current.take() {
                found.push(done);
            }
            current = Some((PathBuf::from(path), None));
        } else if let Some(branch) = line.strip_prefix("branch ")
            && let Some((_, held)) = current.as_mut()
        {
            *held = Some(
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_owned(),
            );
        }
    }
    if let Some(done) = current {
        found.push(done);
    }
    found
}

fn ref_exists(clone: &Path, reference: &str) -> bool {
    git(clone, &["rev-parse", "--verify", "--quiet", reference]).is_ok()
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;

    fn run(path: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A bare origin with `main`, and a clone of it under `root/work/name`.
    fn cloned(root: &Path, name: &str) -> PathBuf {
        let seed = root.join(format!("{name}-seed"));
        std::fs::create_dir_all(&seed).unwrap();
        run(&seed, &["init", "--initial-branch=main"]);
        run(&seed, &["config", "user.email", "t@example.com"]);
        run(&seed, &["config", "user.name", "T"]);
        std::fs::write(seed.join("README.md"), "one\n").unwrap();
        run(&seed, &["add", "."]);
        run(&seed, &["commit", "-m", "first"]);
        let bare = root.join(format!("{name}.git"));
        Command::new("git")
            .args(["clone", "--bare"])
            .arg(&seed)
            .arg(&bare)
            .output()
            .unwrap();
        let clone = root.join("work").join(name);
        std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
        Command::new("git")
            .arg("clone")
            .arg(&bare)
            .arg(&clone)
            .output()
            .unwrap();
        run(&clone, &["config", "user.email", "t@example.com"]);
        run(&clone, &["config", "user.name", "T"]);
        clone
    }

    #[test]
    fn worktree_listings_read_as_path_and_branch() {
        let listing = "worktree /src/pay\nHEAD abc\nbranch refs/heads/main\n\n\
                       worktree /src/.worktrees/pay/715-x\nHEAD def\nbranch refs/heads/715-x\n\n\
                       worktree /src/detached\nHEAD 123\ndetached\n";
        assert_eq!(
            parse_worktrees(listing),
            [
                (PathBuf::from("/src/pay"), Some("main".to_owned())),
                (
                    PathBuf::from("/src/.worktrees/pay/715-x"),
                    Some("715-x".to_owned())
                ),
                (PathBuf::from("/src/detached"), None),
            ]
        );
        assert_eq!(
            worktree_path(Path::new("/src/pay"), "pay", "feature/x"),
            PathBuf::from("/src/.worktrees/pay/feature-x")
        );
    }

    #[test]
    fn a_new_branch_gets_a_worktree_and_a_second_launch_reuses_it() {
        let dir = tempdir().unwrap();
        let clone = cloned(dir.path(), "pay");
        let first = settle(
            &clone,
            "pay",
            "715-fix",
            "refs/heads/main",
            CheckoutPolicy::Worktree,
            false,
        )
        .unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(
            first.workdir,
            root.join("work")
                .join(".worktrees")
                .join("pay")
                .join("715-fix")
        );
        assert!(first.workdir.join("README.md").exists());
        assert!(
            first.note.contains("new branch 715-fix from origin/main"),
            "{}",
            first.note
        );
        assert_eq!(
            git(&first.workdir, &["rev-parse", "--abbrev-ref", "HEAD"])
                .unwrap()
                .trim(),
            "715-fix"
        );
        // The clone is where it was, on main, untouched.
        assert_eq!(
            git(&clone, &["rev-parse", "--abbrev-ref", "HEAD"])
                .unwrap()
                .trim(),
            "main"
        );

        let again = settle(
            &clone,
            "pay",
            "715-fix",
            "refs/heads/main",
            CheckoutPolicy::Worktree,
            false,
        )
        .unwrap();
        assert_eq!(again.workdir, first.workdir);
        assert!(again.note.contains("already on 715-fix"), "{}", again.note);

        // The branch the clone itself is on is the clone.
        let main = settle(
            &clone,
            "pay",
            "main",
            "refs/heads/main",
            CheckoutPolicy::Worktree,
            false,
        )
        .unwrap();
        assert_eq!(main.workdir, clone.canonicalize().unwrap());

        // Shared is the clone, whatever branch it is on.
        let shared = settle(
            &clone,
            "pay",
            "715-fix",
            "main",
            CheckoutPolicy::Shared,
            false,
        )
        .unwrap();
        assert_eq!(shared.workdir, clone.canonicalize().unwrap());
        assert_eq!(shared.branch, "main");
    }

    #[test]
    fn a_branch_on_the_remote_is_tracked_and_a_linked_branch_nowhere_is_refused() {
        let dir = tempdir().unwrap();
        let clone = cloned(dir.path(), "pay");
        // Somebody made the branch on origin: the local clone does not know
        // it until it fetches, which the linked case does.
        let other = dir.path().join("other");
        Command::new("git")
            .arg("clone")
            .arg(dir.path().join("pay.git"))
            .arg(&other)
            .output()
            .unwrap();
        run(&other, &["config", "user.email", "t@example.com"]);
        run(&other, &["config", "user.name", "T"]);
        run(&other, &["checkout", "-b", "716-remote"]);
        std::fs::write(other.join("B.md"), "b\n").unwrap();
        run(&other, &["add", "."]);
        run(&other, &["commit", "-m", "remote work"]);
        run(&other, &["push", "-u", "origin", "716-remote"]);

        let tracked = settle(
            &clone,
            "pay",
            "716-remote",
            "main",
            CheckoutPolicy::Worktree,
            true,
        )
        .unwrap();
        assert!(
            tracked.workdir.join("B.md").exists(),
            "the remote work is in the worktree"
        );
        assert!(
            tracked.note.contains("tracking origin/716-remote"),
            "{}",
            tracked.note
        );

        let refused = settle(
            &clone,
            "pay",
            "717-nowhere",
            "main",
            CheckoutPolicy::Worktree,
            true,
        )
        .unwrap_err();
        assert!(
            format!("{refused:#}").contains("neither in the clone nor on origin"),
            "{refused:#}"
        );
        assert!(
            !worktree_path(&clone, "pay", "717-nowhere").exists(),
            "nothing was made for it"
        );
    }

    #[test]
    fn the_clone_is_the_override_or_what_the_scan_claims() {
        let dir = tempdir().unwrap();
        let clone = cloned(dir.path(), "pay");
        run(
            &clone,
            &[
                "remote",
                "set-url",
                "origin",
                "https://dev.azure.com/demo/atlas/_git/pay",
            ],
        );
        let key = RepoKey {
            id: "aaa".into(),
            remote: Some("demo/atlas/pay".into()),
            name: "pay".into(),
        };
        assert_eq!(
            find_clone(Some(&dir.path().join("work")), None, &key),
            Some(clone.clone())
        );
        assert_eq!(
            find_clone(Some(&clone), Some(&clone), &key),
            Some(clone.clone())
        );
        assert_eq!(
            find_clone(None, Some(&dir.path().join("nowhere")), &key),
            None,
            "an override that is not a clone finds nothing rather than something else"
        );
        assert_eq!(find_clone(None, None, &key), None);
    }
}
