//! The local side of the Repos tab: which of the project's repositories are
//! checked out on this machine, what state each is in, and the three git
//! commands the tab runs — clone, fetch and pull.
//!
//! It has a thread of its own rather than sharing the sync worker's, so a
//! clone that takes a minute never holds up an edit, and it never fetches
//! behind your back: a status read is `git status`, nothing more.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result};

use crate::model::{GitJob, LocalRepo};

/// How long a transfer may crawl below 1 KB/s before git gives up on it. The
/// TUI has no way to interrupt a git command, so one that stalls has to end
/// itself.
const STALL_SECONDS: &str = "30";

/// What the scan matches a directory against: the repository, the
/// `org/project/name` its remote normalises to, and what it is called.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoKey {
    pub id: String,
    pub remote: Option<String>,
    pub name: String,
}

/// What the local side can be asked to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalRequest {
    /// Read the workspace: which of these repositories are here, and how each
    /// stands.
    Scan {
        workspace: PathBuf,
        repos: Vec<RepoKey>,
    },
    /// `git clone <url> <workspace>/<name>`.
    Clone {
        repo_id: String,
        url: String,
        into: PathBuf,
    },
    Fetch {
        repo_id: String,
        path: PathBuf,
    },
    /// `git pull --ff-only`, which is the only pull this offers: anything a
    /// fast-forward cannot do belongs in a real git client.
    Pull {
        repo_id: String,
        path: PathBuf,
    },
    /// Render one kustomize overlay of the deployment clone. It sits here
    /// rather than on the sync worker for the same reason a clone does: a
    /// render shells out to `kubectl` and takes as long as it takes, and an
    /// edit may not wait on it.
    Render {
        environment: String,
        clone: PathBuf,
        overlay: String,
        command: String,
    },
    /// Pre-fly one pull request against the deployment repository: what the
    /// head it was read at would leave an environment missing. It sits here
    /// for the same reason a render does — a fetch, a worktree and a `kubectl`
    /// per overlay take as long as they take.
    Preflight {
        id: i64,
        /// The head it is flown at, which is what the answer is cached under.
        commit: String,
        source: String,
        target: String,
        deployment: crate::preflight::Deployment,
    },
    Stop,
}

/// What the local side has found or done.
#[derive(Clone, Debug)]
pub enum LocalEvent {
    /// The workspace as it stands, by repository id.
    Scanned(Vec<(String, LocalRepo)>),
    /// A git command has started on one repository.
    Started {
        repo_id: String,
        job: GitJob,
    },
    /// It finished. `message` is what to say; `error` is whether it failed.
    Finished {
        repo_id: String,
        job: GitJob,
        message: String,
        error: bool,
    },
    /// One overlay, rendered: the YAML, or the one line of the renderer's
    /// complaint that says what to fix.
    Rendered {
        environment: String,
        overlay: String,
        rendered: Result<String, String>,
    },
    /// One pull request, pre-flown: what its overlays would be missing, or the
    /// one line saying why it could not be looked at.
    Preflighted {
        id: i64,
        commit: String,
        found: Result<crate::preflight::Report, String>,
    },
    Stopped,
}

/// The handle the main thread holds.
pub struct LocalHandle {
    requests: Sender<LocalRequest>,
    events: Receiver<LocalEvent>,
    stopped: std::cell::Cell<bool>,
}

impl LocalHandle {
    /// Starts the thread. It ends when the handle is dropped.
    pub fn spawn() -> Result<Self> {
        let (request_sender, request_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("ticket-local".into())
            .spawn(move || work(&request_receiver, &event_sender))
            .context("failed to start the local repositories thread")?;
        Ok(Self {
            requests: request_sender,
            events: event_receiver,
            stopped: std::cell::Cell::new(false),
        })
    }

    pub fn send(&self, request: LocalRequest) -> Result<()> {
        self.requests
            .send(request)
            .context("the local repositories thread stopped")
    }

    pub fn try_event(&self) -> Option<LocalEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                (!self.stopped.replace(true)).then_some(LocalEvent::Stopped)
            }
        }
    }
}

fn work(requests: &Receiver<LocalRequest>, events: &Sender<LocalEvent>) {
    while let Ok(request) = requests.recv() {
        let sent = match request {
            LocalRequest::Stop => return,
            LocalRequest::Scan { workspace, repos } => {
                events.send(LocalEvent::Scanned(scan(&workspace, &repos)))
            }
            LocalRequest::Clone { repo_id, url, into } => {
                run_job(events, &repo_id, GitJob::Cloning, || clone(&url, &into))
            }
            LocalRequest::Fetch { repo_id, path } => {
                run_job(events, &repo_id, GitJob::Fetching, || {
                    remote_git(&path, &["fetch", "--prune"]).map(|_| "Fetched".to_owned())
                })
            }
            LocalRequest::Pull { repo_id, path } => {
                run_job(events, &repo_id, GitJob::Pulling, || {
                    remote_git(&path, &["pull", "--ff-only"]).map(|_| "Pulled".to_owned())
                })
            }
            LocalRequest::Render {
                environment,
                clone,
                overlay,
                command,
            } => events.send(LocalEvent::Rendered {
                rendered: crate::kustomize::render(&clone, &overlay, &command)
                    .map_err(|error| last_line(&format!("{error:#}"))),
                environment,
                overlay,
            }),
            LocalRequest::Preflight {
                id,
                commit,
                source,
                target,
                deployment,
            } => events.send(LocalEvent::Preflighted {
                found: crate::preflight::run(&deployment, &source, &target, &commit)
                    .map_err(|error| last_line(&format!("{error:#}"))),
                id,
                commit,
            }),
        };
        if sent.is_err() {
            return;
        }
    }
}

/// Runs one git job, saying so before and after.
fn run_job(
    events: &Sender<LocalEvent>,
    repo_id: &str,
    job: GitJob,
    run: impl FnOnce() -> Result<String>,
) -> Result<(), mpsc::SendError<LocalEvent>> {
    events.send(LocalEvent::Started {
        repo_id: repo_id.to_owned(),
        job,
    })?;
    let (message, error) = match run() {
        Ok(message) => (message, false),
        Err(error) => (last_line(&format!("{error:#}")), true),
    };
    events.send(LocalEvent::Finished {
        repo_id: repo_id.to_owned(),
        job,
        message,
        error,
    })
}

/// git says the most useful thing last.
fn last_line(message: &str) -> String {
    message
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or(message)
        .trim()
        .to_owned()
}

/// Every repository in `workspace` that is one of `repos`, with its state.
/// A workspace that is not there is not an error: it is answered with nothing,
/// and the tab says where it looked.
///
/// A directory is claimed by its `origin` first. One that no remote claimed is
/// then offered to the repository of the same name, because a project whose
/// repositories are mirrored somewhere else — the origin here is GitHub's —
/// is still the code you have on this machine; the details pane says where
/// such a clone's origin actually points.
#[must_use]
pub fn scan(workspace: &Path, repos: &[RepoKey]) -> Vec<(String, LocalRepo)> {
    let Ok(entries) = std::fs::read_dir(workspace) else {
        return Vec::new();
    };
    let mut clones: Vec<(PathBuf, String)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.join(".git").exists() {
            continue;
        }
        if let Ok(origin) = git(&path, &["remote", "get-url", "origin"]) {
            clones.push((path, origin.trim().to_owned()));
        }
    }
    let mut claimed: Vec<(String, PathBuf, String)> = Vec::new();
    for (path, origin) in &clones {
        let Some(key) = normalise_remote(origin) else {
            continue;
        };
        if let Some(repo) = repos.iter().find(|repo| {
            repo.remote
                .as_ref()
                .is_some_and(|remote| remote.eq_ignore_ascii_case(&key))
        }) {
            claimed.push((repo.id.clone(), path.clone(), origin.clone()));
        }
    }
    for (path, origin) in &clones {
        if claimed.iter().any(|(_, held, _)| held == path) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(repo) = repos.iter().find(|repo| {
            repo.name.eq_ignore_ascii_case(name) && !claimed.iter().any(|(id, _, _)| *id == repo.id)
        }) {
            claimed.push((repo.id.clone(), path.clone(), origin.clone()));
        }
    }
    let mut found: Vec<(String, LocalRepo)> = claimed
        .into_iter()
        .filter_map(|(id, path, origin)| read_status(&path, &origin).map(|local| (id, local)))
        .collect();
    found.sort_by(|left, right| left.0.cmp(&right.0));
    found
}

/// `git status --porcelain=v2 --branch`, read into the four things the column
/// says. A directory git will not talk about is left out rather than guessed.
#[must_use]
pub fn read_status(path: &Path, origin: &str) -> Option<LocalRepo> {
    let output = git(path, &["status", "--porcelain=v2", "--branch"]).ok()?;
    let mut branch = String::new();
    let mut dirty = false;
    let (mut ahead, mut behind) = (0, 0);
    for line in output.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            branch = head.trim().to_owned();
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            let mut parts = ab.split_whitespace();
            ahead = parts
                .next()
                .and_then(|value| value.trim_start_matches('+').parse().ok())
                .unwrap_or(0);
            behind = parts
                .next()
                .and_then(|value| value.trim_start_matches('-').parse().ok())
                .unwrap_or(0);
        } else if !line.starts_with('#') && !line.trim().is_empty() {
            dirty = true;
        }
    }
    Some(LocalRepo {
        path: path.to_path_buf(),
        origin: origin.to_owned(),
        branch,
        dirty,
        ahead,
        behind,
        busy: None,
    })
}

/// `org/project/name`, whichever way the remote is spelled. Anything that is
/// not an Azure DevOps remote answers with nothing.
#[must_use]
pub fn normalise_remote(remote: &str) -> Option<String> {
    let remote = remote.trim_end_matches('/').trim_end_matches(".git");
    // git@ssh.dev.azure.com:v3/org/project/name
    if let Some((_, tail)) = remote.split_once(":v3/") {
        return (tail.split('/').count() == 3).then(|| tail.to_owned());
    }
    // https://[user@]dev.azure.com/org/project/_git/name, and the older
    // https://org.visualstudio.com/project/_git/name.
    let (_, tail) = remote.split_once("://")?;
    let (host, path) = tail.split_once('/')?;
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let git_at = parts.iter().position(|part| *part == "_git")?;
    let name = parts.get(git_at + 1)?;
    match parts.as_slice() {
        [organization, project, ..] if git_at >= 2 => {
            Some(format!("{organization}/{project}/{name}"))
        }
        [project, ..] if git_at == 1 => {
            let organization = host.split('@').next_back()?.split('.').next()?;
            Some(format!("{organization}/{project}/{name}"))
        }
        _ => None,
    }
}

/// `git clone <url> <into>`, which is the one command that runs outside a
/// repository.
fn clone(url: &str, into: &Path) -> Result<String> {
    if into.exists() {
        anyhow::bail!("{} already exists", into.display());
    }
    let parent = into
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has nowhere to go", into.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to make {}", parent.display()))?;
    let mut command = Command::new("git");
    command.arg("clone").arg(url).arg(into);
    run_remote(command, url)?;
    Ok(format!(
        "Cloned {}",
        into.file_name().unwrap_or_default().to_string_lossy()
    ))
}

/// One git command inside one repository that talks to its `origin`.
pub(crate) fn remote_git(path: &Path, arguments: &[&str]) -> Result<String> {
    let origin = git(path, &["remote", "get-url", "origin"])
        .map(|url| url.trim().to_owned())
        .unwrap_or_default();
    let mut command = Command::new("git");
    command.arg("-C").arg(path).args(arguments);
    run_remote(command, &origin)
}

/// Runs a git command that reaches a remote, on the terms the TUI can live
/// with: git and ssh may never prompt — the terminal is the TUI's, and a
/// question asked on it is a command that hangs for ever — a transfer that
/// stalls ends itself, and an Azure DevOps remote over https is signed with
/// the login the sync already has, so no key or credential helper has to be
/// set up first.
fn run_remote(mut command: Command, remote: &str) -> Result<String> {
    command
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", batch_ssh_command());
    let mut config: Vec<(String, String)> = vec![
        ("http.lowSpeedLimit".to_owned(), "1000".to_owned()),
        ("http.lowSpeedTime".to_owned(), STALL_SECONDS.to_owned()),
    ];
    let mut login_error = None;
    if let Some(host) = azure_https_host(remote) {
        match crate::azure::authorization_header() {
            Ok(authorization) => {
                // Scoped to the host, so the token is never offered to any
                // other remote, and passed as configuration in the
                // environment rather than on the command line, where `ps`
                // would show it.
                config.push((
                    format!("http.{host}.extraheader"),
                    format!("AUTHORIZATION: {authorization}"),
                ));
                config.push((
                    format!("http.{host}.extraheader"),
                    "X-VSS-ForceMsaPassThrough: true".to_owned(),
                ));
            }
            Err(error) => login_error = Some(error),
        }
    }
    command.env("GIT_CONFIG_COUNT", config.len().to_string());
    for (index, (key, value)) in config.into_iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{index}"), key)
            .env(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
    let output = command.output().context("git could not be run")?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // A remote that wanted a login it could not ask for is reported as the
    // login that is missing, not as the prompt git was not allowed to show.
    if stderr.contains("terminal prompts disabled") || stderr.contains("Permission denied") {
        let hint = match login_error {
            Some(error) => format!("{error:#}"),
            None => "Azure DevOps refused the login; run `az login` or set AZURE_DEVOPS_EXT_PAT"
                .to_owned(),
        };
        anyhow::bail!("{}\n{hint}", stderr.trim());
    }
    anyhow::bail!("{stderr}")
}

/// The ssh git runs, told never to ask anything: an unknown key or a missing
/// one fails at once, with ssh's own words, instead of waiting on a prompt
/// nobody can see. Whatever `GIT_SSH_COMMAND` already says is kept in front.
fn batch_ssh_command() -> String {
    let base = std::env::var("GIT_SSH_COMMAND")
        .ok()
        .filter(|command| !command.trim().is_empty())
        .unwrap_or_else(|| "ssh".to_owned());
    format!("{base} -o BatchMode=yes -o ConnectTimeout=20")
}

/// `https://host` for an Azure DevOps remote reached over https — the scope
/// the login header is attached under — and nothing for any other remote.
#[must_use]
pub fn azure_https_host(remote: &str) -> Option<String> {
    let (scheme, tail) = remote.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = tail.split('/').next()?;
    let host = authority.rsplit('@').next()?.to_ascii_lowercase();
    let azure = host == "dev.azure.com"
        || host.ends_with(".dev.azure.com")
        || host.ends_with(".visualstudio.com");
    azure.then(|| format!("https://{host}"))
}

/// One git command inside one repository that stays on this machine.
pub(crate) fn git(path: &Path, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(arguments)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("git could not be run")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr))
    }
}

/// Where clones are looked for and made: the flag, then the variable, then
/// `~/Development`.
#[must_use]
pub fn workspace_root(flag: Option<PathBuf>) -> Option<PathBuf> {
    flag.or_else(|| std::env::var_os("TICKET_TUI_WORKSPACE").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Development")))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn every_way_a_remote_is_spelled_reads_as_one_repository() {
        for remote in [
            "https://dev.azure.com/jacobragsdale/development/_git/ticket-tui",
            "https://jacobragsdale@dev.azure.com/jacobragsdale/development/_git/ticket-tui",
            "https://dev.azure.com/jacobragsdale/development/_git/ticket-tui.git",
            "git@ssh.dev.azure.com:v3/jacobragsdale/development/ticket-tui",
        ] {
            assert_eq!(
                normalise_remote(remote).as_deref(),
                Some("jacobragsdale/development/ticket-tui"),
                "{remote}"
            );
        }
        assert_eq!(
            normalise_remote("https://jacobragsdale.visualstudio.com/development/_git/ticket-tui")
                .as_deref(),
            Some("jacobragsdale/development/ticket-tui"),
            "the older host spells the organization in front"
        );
        assert_eq!(normalise_remote("git@github.com:jacob/other.git"), None);
    }

    #[test]
    fn only_an_azure_devops_https_remote_is_signed_with_the_login() {
        assert_eq!(
            azure_https_host(
                "https://jacobragsdale@dev.azure.com/jacobragsdale/development/_git/x"
            )
            .as_deref(),
            Some("https://dev.azure.com"),
            "the user in front of the host is not part of the scope"
        );
        assert_eq!(
            azure_https_host("https://jacobragsdale.visualstudio.com/development/_git/x")
                .as_deref(),
            Some("https://jacobragsdale.visualstudio.com")
        );
        for remote in [
            "git@ssh.dev.azure.com:v3/jacobragsdale/development/x",
            "https://github.com/jacobragsdale/x.git",
            "file:///tmp/x.git",
            "",
        ] {
            assert_eq!(azure_https_host(remote), None, "{remote}");
        }
    }

    /// A bare repository with one commit, which the clones below come from.
    fn origin(root: &Path, name: &str) -> PathBuf {
        let work = root.join(format!("{name}-work"));
        fs::create_dir_all(&work).unwrap();
        run(&work, &["init", "--initial-branch=main"]);
        run(&work, &["config", "user.email", "test@example.com"]);
        run(&work, &["config", "user.name", "Test"]);
        fs::write(work.join("README.md"), "one\n").unwrap();
        run(&work, &["add", "."]);
        run(&work, &["commit", "-m", "first"]);
        let bare = root.join(format!("{name}.git"));
        Command::new("git")
            .args(["clone", "--bare"])
            .arg(&work)
            .arg(&bare)
            .output()
            .unwrap();
        bare
    }

    /// One repository as the scan is told about it.
    fn key(id: &str, name: &str) -> RepoKey {
        RepoKey {
            id: id.to_owned(),
            remote: Some(format!("demo/atlas/{name}")),
            name: name.to_owned(),
        }
    }

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

    /// Sets `origin` to a remote that reads as an Azure DevOps one, so the
    /// scan matches it the way it would a real clone.
    fn pretend_azure(path: &Path, name: &str) {
        run(
            path,
            &[
                "remote",
                "set-url",
                "origin",
                &format!("https://dev.azure.com/demo/atlas/_git/{name}"),
            ],
        );
    }

    #[test]
    fn the_scan_reads_the_branch_the_dirt_and_how_far_behind_each_clone_is() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let bare = origin(root, "ticket-tui");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        // One clean clone, and one that is dirty and a commit behind.
        for name in ["ticket-tui", "skillbook"] {
            let output = Command::new("git")
                .args(["clone"])
                .arg(&bare)
                .arg(workspace.join(name))
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        let dirty = workspace.join("skillbook");
        fs::write(dirty.join("README.md"), "changed\n").unwrap();
        // A commit lands on the remote, so the clone is one behind.
        let pusher = workspace.join("ticket-tui");
        run(&pusher, &["config", "user.email", "test@example.com"]);
        run(&pusher, &["config", "user.name", "Test"]);
        fs::write(pusher.join("NOTES.md"), "two\n").unwrap();
        run(&pusher, &["add", "."]);
        run(&pusher, &["commit", "-m", "second"]);
        run(&pusher, &["push", "origin", "main"]);
        // And one more it keeps to itself, so it reads as a commit ahead.
        fs::write(pusher.join("LATER.md"), "three\n").unwrap();
        run(&pusher, &["add", "."]);
        run(&pusher, &["commit", "-m", "third"]);
        run(&dirty, &["fetch"]);
        // Only now do the remotes read as Azure DevOps ones: the git above
        // has to talk to the bare repository next door.
        for name in ["ticket-tui", "skillbook"] {
            pretend_azure(&workspace.join(name), name);
        }

        let repos = vec![
            key("aaa-111", "ticket-tui"),
            key("bbb-222", "skillbook"),
            key("ccc-333", "home-server"),
        ];
        let found = scan(&workspace, &repos);

        assert_eq!(found.len(), 2, "one nobody has here is simply not there");
        let ticket_tui = &found
            .iter()
            .find(|(id, _)| id == "aaa-111")
            .expect("the clean clone")
            .1;
        assert_eq!(ticket_tui.branch, "main");
        assert!(!ticket_tui.dirty);
        assert_eq!(ticket_tui.ahead, 1, "the commit it pushed is still ahead");

        let skillbook = &found
            .iter()
            .find(|(id, _)| id == "bbb-222")
            .expect("the dirty clone")
            .1;
        assert!(skillbook.dirty, "an uncommitted change is dirt");
        assert_eq!(skillbook.behind, 1, "and it is a commit behind");
    }

    #[test]
    fn a_clone_whose_origin_is_somewhere_else_is_claimed_by_its_name() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let bare = origin(root, "ticket-tui");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        for name in ["ticket-tui", "skillbook"] {
            Command::new("git")
                .arg("clone")
                .arg(&bare)
                .arg(workspace.join(name))
                .output()
                .unwrap();
        }
        // Mirrored on GitHub, which is where these origins point.
        for name in ["ticket-tui", "skillbook"] {
            run(
                &workspace.join(name),
                &[
                    "remote",
                    "set-url",
                    "origin",
                    &format!("https://github.com/jacobragsdale/{name}.git"),
                ],
            );
        }

        let found = scan(&workspace, &[key("aaa-111", "ticket-tui")]);
        assert_eq!(found.len(), 1, "only the one the project knows about");
        let (id, local) = &found[0];
        assert_eq!(id, "aaa-111");
        assert_eq!(local.branch, "main");
        assert!(
            local.origin.contains("github.com"),
            "and it carries where its origin really points: {}",
            local.origin
        );

        // A remote that does match wins the directory it names, so a name
        // that happens to collide cannot take it.
        run(
            &workspace.join("skillbook"),
            &[
                "remote",
                "set-url",
                "origin",
                "https://dev.azure.com/demo/atlas/_git/ticket-tui",
            ],
        );
        let found = scan(&workspace, &[key("aaa-111", "ticket-tui")]);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].1.path.file_name().unwrap(),
            "skillbook",
            "the remote is what the repository is, whatever the directory is called"
        );
    }

    #[test]
    fn a_clone_lands_in_the_workspace_and_a_failing_one_says_what_git_said() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let bare = origin(root, "ticket-tui");
        let workspace = root.join("workspace");
        let into = workspace.join("ticket-tui");

        let message = clone(&format!("file://{}", bare.display()), &into).expect("the clone");
        assert_eq!(message, "Cloned ticket-tui");
        let status = read_status(&into, "").expect("a clone has a status");
        assert_eq!(status.branch, "main");
        assert!(!status.dirty);

        let refused = clone(&format!("file://{}", bare.display()), &into)
            .expect_err("cloning over one already there is refused");
        assert!(refused.to_string().contains("already exists"), "{refused}");

        let missing = clone("file:///nowhere/at/all.git", &workspace.join("other"))
            .expect_err("a clone of nothing fails");
        assert!(
            format!("{missing:#}").contains("repository"),
            "git's own words come back: {missing:#}"
        );
    }

    #[test]
    fn a_pull_clears_what_it_was_behind_and_a_diverged_one_is_refused() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let bare = origin(root, "ticket-tui");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let clone_path = workspace.join("ticket-tui");
        Command::new("git")
            .arg("clone")
            .arg(&bare)
            .arg(&clone_path)
            .output()
            .unwrap();
        run(&clone_path, &["config", "user.email", "test@example.com"]);
        run(&clone_path, &["config", "user.name", "Test"]);

        // Somebody else pushes.
        let other = root.join("other");
        Command::new("git")
            .arg("clone")
            .arg(&bare)
            .arg(&other)
            .output()
            .unwrap();
        run(&other, &["config", "user.email", "test@example.com"]);
        run(&other, &["config", "user.name", "Test"]);
        fs::write(other.join("NOTES.md"), "theirs\n").unwrap();
        run(&other, &["add", "."]);
        run(&other, &["commit", "-m", "theirs"]);
        run(&other, &["push", "origin", "main"]);

        run(&clone_path, &["fetch"]);
        assert_eq!(read_status(&clone_path, "").unwrap().behind, 1);
        git(&clone_path, &["pull", "--ff-only"]).expect("a fast-forward pull");
        let status = read_status(&clone_path, "").unwrap();
        assert_eq!((status.ahead, status.behind), (0, 0), "it is level now");

        // Both sides move: the pull can no longer fast-forward.
        fs::write(clone_path.join("MINE.md"), "mine\n").unwrap();
        run(&clone_path, &["add", "."]);
        run(&clone_path, &["commit", "-m", "mine"]);
        fs::write(other.join("THEIRS.md"), "theirs again\n").unwrap();
        run(&other, &["add", "."]);
        run(&other, &["commit", "-m", "theirs again"]);
        run(&other, &["push", "origin", "main"]);
        run(&clone_path, &["fetch"]);

        let refused = git(&clone_path, &["pull", "--ff-only"])
            .expect_err("a diverged pull cannot fast-forward");
        assert!(
            format!("{refused:#}").to_lowercase().contains("fast"),
            "git says why: {refused:#}"
        );
    }
}
