//! The AKS tab's worker: what a pod is, how `kubectl` is asked about one, and
//! the thread that keeps asking while the tab is showing.
//!
//! Nothing here touches SQLite. A pod is read live, the way local git state
//! and live runs are: what a cluster holds is not the project's business, and
//! a read is cheap. The worker has its own thread and its own `kubectl`
//! processes, so a slow cluster never queues behind a pull, and one
//! `kubectl logs -f` child follows whichever pod the details pane is on.

use std::cell::Cell;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::config::Cluster;
use crate::filter::{FilterSchema, contains_ignore_case};
use crate::timestamp::Timestamp;
use crate::watch::Cadence;

/// How often each cluster's pods are read while the tab is showing.
pub const POD_CADENCE: Duration = Duration::from_secs(15);

/// The bound on every one-shot `kubectl` call. A cluster that cannot be
/// reached answers in ten seconds rather than never; the follow is the one
/// call that runs without it, because a stream is meant to last.
const REQUEST_TIMEOUT: &str = "--request-timeout=10s";

/// How much of a log a follow opens on.
const TAIL_LINES: &str = "--tail=500";

/// One pod, by where it lives.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct PodKey {
    /// The cluster's name in `config.toml`, not its context.
    pub cluster: String,
    pub namespace: String,
    pub name: String,
}

/// One container of a pod, as its status reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Container {
    pub name: String,
    pub image: String,
    pub ready: bool,
    pub restarts: u32,
    /// `Running`, or the reason it is waiting or has stopped:
    /// `CrashLoopBackOff`, `Completed`, `ExitCode:137`.
    pub state: String,
    /// Why it last stopped, and with what code, when it has stopped before.
    pub last_termination: Option<(String, i64)>,
}

/// One pod, as `kubectl get pods` would print it, with what the details pane
/// wants besides.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pod {
    pub key: PodKey,
    /// The STATUS word: `Running`, `CrashLoopBackOff`, `Init:1/2`, …
    pub status: String,
    /// Containers ready, and containers in the spec.
    pub ready: (usize, usize),
    pub restarts: u32,
    pub created: Option<Timestamp>,
    pub node: String,
    pub ip: String,
    /// What made it, as `(kind, name)`: `("Deployment", "orders-api")`.
    pub owner: Option<(String, String)>,
    pub containers: Vec<Container>,
    /// Every label, sorted by key.
    pub labels: Vec<(String, String)>,
}

impl Pod {
    /// One `items[]` entry of `kubectl get pods -o json`. `None` for an entry
    /// with no name or namespace, which is not a pod.
    #[must_use]
    pub fn from_json(cluster: &str, item: &Value) -> Option<Self> {
        let metadata = &item["metadata"];
        let name = metadata["name"].as_str()?;
        let namespace = metadata["namespace"].as_str()?;
        let statuses = item["status"]["containerStatuses"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let containers: Vec<Container> = item["spec"]["containers"]
            .as_array()
            .map(|specs| {
                specs
                    .iter()
                    .filter_map(|spec| {
                        let name = spec["name"].as_str()?;
                        let status = statuses
                            .iter()
                            .find(|status| status["name"].as_str() == Some(name));
                        Some(container(name, spec, status))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut labels: Vec<(String, String)> = metadata["labels"]
            .as_object()
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        labels.sort();
        Some(Self {
            key: PodKey {
                cluster: cluster.to_owned(),
                namespace: namespace.to_owned(),
                name: name.to_owned(),
            },
            status: status_word(item),
            ready: (
                containers.iter().filter(|held| held.ready).count(),
                containers.len(),
            ),
            restarts: containers.iter().map(|held| held.restarts).sum(),
            created: metadata["creationTimestamp"]
                .as_str()
                .and_then(|raw| Timestamp::parse(raw).ok()),
            node: item["spec"]["nodeName"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            ip: item["status"]["podIP"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            owner: owner_of(item),
            containers,
            labels,
        })
    }

    #[must_use]
    pub fn label(&self, key: &str) -> Option<&str> {
        self.labels
            .iter()
            .find(|(held, _)| held == key)
            .map(|(_, value)| value.as_str())
    }

    /// `1/2`, the READY column.
    #[must_use]
    pub fn ready_label(&self) -> String {
        format!("{}/{}", self.ready.0, self.ready.1)
    }

    /// Whether deleting it restarts anything: a pod with a controller is put
    /// back by that controller, a bare pod is simply gone.
    #[must_use]
    pub const fn restartable(&self) -> bool {
        self.owner.is_some()
    }

    /// Whether the STATUS word is one somebody has to look at.
    #[must_use]
    pub fn is_unhealthy(&self) -> bool {
        unhealthy_word(self.status.strip_prefix("Init:").unwrap_or(&self.status))
    }

    /// The glyph the conventions give the pod: `●` running and ready, `◐`
    /// on its way somewhere, `✓` finished, `✗` in trouble, `○` anything else.
    #[must_use]
    pub fn glyph(&self) -> &'static str {
        if self.is_unhealthy() {
            "\u{2717}"
        } else if self.status == "Running" && self.ready.1 > 0 && self.ready.0 == self.ready.1 {
            "\u{25cf}"
        } else if matches!(self.status.as_str(), "Completed" | "Succeeded") {
            "\u{2713}"
        } else if matches!(
            self.status.as_str(),
            "Running" | "Pending" | "ContainerCreating" | "PodInitializing" | "Terminating"
        ) || self.status.starts_with("Init:")
        {
            "\u{25d0}"
        } else {
            "\u{25cb}"
        }
    }

    /// The name of the container the log follows when nobody has chosen one.
    #[must_use]
    pub fn first_container(&self) -> Option<&str> {
        self.containers.first().map(|held| held.name.as_str())
    }

    /// What the repository that built it might be called: the app labels, then
    /// each image's last path segment without its registry, tag or digest.
    /// `myacr.azurecr.io/team/orders-api:1.2.3` reads `orders-api`.
    #[must_use]
    pub fn repo_candidates(&self) -> Vec<String> {
        let mut candidates: Vec<String> = Vec::new();
        let mut push = |candidate: Option<String>| {
            if let Some(candidate) = candidate.filter(|held| !held.is_empty())
                && !candidates
                    .iter()
                    .any(|held| held.eq_ignore_ascii_case(&candidate))
            {
                candidates.push(candidate);
            }
        };
        push(self.label("app.kubernetes.io/name").map(str::to_owned));
        push(self.label("app").map(str::to_owned));
        for held in &self.containers {
            push(image_name(&held.image));
        }
        candidates
    }
}

/// One container, joined from its spec and its status.
fn container(name: &str, spec: &Value, status: Option<&Value>) -> Container {
    let state = status.map(|status| &status["state"]);
    let word = state.map_or_else(
        || "Waiting".to_owned(),
        |state| {
            if !state["running"].is_null() {
                "Running".to_owned()
            } else if let Some(reason) = non_empty(&state["waiting"]["reason"]) {
                reason.to_owned()
            } else if !state["terminated"].is_null() {
                termination_word(&state["terminated"])
            } else {
                "Waiting".to_owned()
            }
        },
    );
    let last = status
        .map(|status| &status["lastState"]["terminated"])
        .filter(|terminated| !terminated.is_null())
        .map(|terminated| {
            (
                non_empty(&terminated["reason"])
                    .unwrap_or("Terminated")
                    .to_owned(),
                terminated["exitCode"].as_i64().unwrap_or_default(),
            )
        });
    Container {
        name: name.to_owned(),
        image: status
            .and_then(|status| non_empty(&status["image"]))
            .or_else(|| non_empty(&spec["image"]))
            .unwrap_or_default()
            .to_owned(),
        ready: status.is_some_and(|status| status["ready"].as_bool() == Some(true)),
        restarts: status
            .and_then(|status| status["restartCount"].as_u64())
            .and_then(|count| u32::try_from(count).ok())
            .unwrap_or_default(),
        state: word,
        last_termination: last,
    }
}

fn non_empty(value: &Value) -> Option<&str> {
    value.as_str().filter(|held| !held.is_empty())
}

/// What a stopped container says: its reason, or its exit code when it gave
/// none.
fn termination_word(terminated: &Value) -> String {
    non_empty(&terminated["reason"]).map_or_else(
        || {
            format!(
                "ExitCode:{}",
                terminated["exitCode"].as_i64().unwrap_or_default()
            )
        },
        str::to_owned,
    )
}

/// The STATUS word `kubectl get pods` prints, cut to the cases that come up:
/// the pod's own reason or phase, overridden by the first init container
/// still going, else by whatever the containers are waiting on or stopped
/// for, and `Terminating` over all of it once a delete is in.
// ponytail: skipped from kubectl's printPod — sidecar init containers,
// Signal:N, NotReady, NodeLost→Unknown, and the "(N ago)" restart suffix.
fn status_word(item: &Value) -> String {
    let status = &item["status"];
    let phase = status["phase"].as_str().unwrap_or("Unknown");
    let mut word = non_empty(&status["reason"]).unwrap_or(phase).to_owned();
    let init_total = item["spec"]["initContainers"]
        .as_array()
        .map_or(0, Vec::len);
    let mut initializing = false;
    for (index, held) in status["initContainerStatuses"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let state = &held["state"];
        let terminated = &state["terminated"];
        if !terminated.is_null() {
            if terminated["exitCode"].as_i64() == Some(0) {
                continue;
            }
            word = format!("Init:{}", termination_word(terminated));
        } else if let Some(reason) =
            non_empty(&state["waiting"]["reason"]).filter(|reason| *reason != "PodInitializing")
        {
            word = format!("Init:{reason}");
        } else {
            word = format!("Init:{index}/{init_total}");
        }
        initializing = true;
        break;
    }
    if !initializing {
        let mut has_running = false;
        // Back to front, the way kubectl reads them, so the first container's
        // reason is the one that stands.
        for held in status["containerStatuses"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
        {
            let state = &held["state"];
            if let Some(reason) = non_empty(&state["waiting"]["reason"]) {
                word = reason.to_owned();
            } else if !state["terminated"].is_null() {
                word = termination_word(&state["terminated"]);
            } else if held["ready"].as_bool() == Some(true) && !state["running"].is_null() {
                has_running = true;
            }
        }
        if word == "Completed" && has_running {
            word = "Running".to_owned();
        }
    }
    if !item["metadata"]["deletionTimestamp"].is_null() && !matches!(phase, "Succeeded" | "Failed")
    {
        word = "Terminating".to_owned();
    }
    word
}

/// Whether a STATUS word, with any `Init:` in front of it removed, is one
/// somebody has to look at.
fn unhealthy_word(word: &str) -> bool {
    matches!(
        word,
        "CrashLoopBackOff"
            | "Error"
            | "ImagePullBackOff"
            | "ErrImagePull"
            | "InvalidImageName"
            | "CreateContainerConfigError"
            | "CreateContainerError"
            | "OOMKilled"
            | "Evicted"
            | "Failed"
            | "ContainerStatusUnknown"
            | "Unknown"
    ) || word.starts_with("ExitCode:")
}

/// What made the pod. A ReplicaSet named after a pod-template hash is a
/// Deployment's, and is reported as that Deployment, which is the name that
/// means something.
// ponytail: a Job's CronJob is not resolved; a ReplicaSet with no hash label
// stays a ReplicaSet.
fn owner_of(item: &Value) -> Option<(String, String)> {
    let references = item["metadata"]["ownerReferences"].as_array()?;
    let owner = references
        .iter()
        .find(|reference| reference["controller"].as_bool() == Some(true))
        .or_else(|| references.first())?;
    let kind = owner["kind"].as_str()?;
    let name = owner["name"].as_str()?;
    if kind == "ReplicaSet"
        && let Some(hash) = non_empty(&item["metadata"]["labels"]["pod-template-hash"])
        && let Some(base) = name.strip_suffix(&format!("-{hash}"))
    {
        return Some(("Deployment".to_owned(), base.to_owned()));
    }
    Some((kind.to_owned(), name.to_owned()))
}

/// The name in an image reference: the last path segment, without the tag or
/// the digest.
#[must_use]
pub fn image_name(image: &str) -> Option<String> {
    let last = image.rsplit('/').next()?;
    let name = last.split('@').next()?.split(':').next()?;
    (!name.is_empty()).then(|| name.to_owned())
}

/// One pod as the table draws it and the filters read it: the pod, and the
/// repository on file that its image or app label names, when one does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PodRow {
    pub pod: Pod,
    pub repo: Option<String>,
}

impl PodRow {
    /// The pod with the repository looked up among `repos`, by name, the way
    /// the workspace scan claims a clone by its directory name.
    #[must_use]
    pub fn new(pod: Pod, repos: &[String]) -> Self {
        let repo = pod.repo_candidates().into_iter().find_map(|candidate| {
            repos
                .iter()
                .find(|repo| repo.eq_ignore_ascii_case(&candidate))
                .cloned()
        });
        Self { pod, repo }
    }

    /// What `owner:` filters on and the details pane shows: `orders-api`.
    #[must_use]
    pub fn owner_name(&self) -> &str {
        self.pod
            .owner
            .as_ref()
            .map_or("", |(_, name)| name.as_str())
    }

    /// Whether the words with no field in front of them are in this row.
    #[must_use]
    pub fn matches_fuzzy(&self, needle: &str) -> bool {
        contains_ignore_case(&self.pod.key.name, needle)
            || contains_ignore_case(&self.pod.key.namespace, needle)
            || contains_ignore_case(self.owner_name(), needle)
            || contains_ignore_case(self.repo.as_deref().unwrap_or_default(), needle)
    }
}

/// The AKS tab's filter grammar, which the CLI reads too.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PodSchema;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PodField {
    Cluster,
    Namespace,
    Status,
    Owner,
    Node,
    /// The `app` and `app.kubernetes.io/name` labels.
    App,
    Repo,
}

impl FilterSchema for PodSchema {
    type Field = PodField;
    type Row = PodRow;

    fn all() -> &'static [Self::Field] {
        &[
            PodField::Cluster,
            PodField::Namespace,
            PodField::Status,
            PodField::Owner,
            PodField::Node,
            PodField::App,
            PodField::Repo,
        ]
    }

    fn bar() -> &'static [Self::Field] {
        &[PodField::Cluster, PodField::Namespace, PodField::Status]
    }

    fn parse(name: &str) -> Option<Self::Field> {
        match name.to_ascii_lowercase().as_str() {
            "cluster" => Some(PodField::Cluster),
            "ns" | "namespace" => Some(PodField::Namespace),
            "status" | "phase" => Some(PodField::Status),
            "owner" | "deployment" => Some(PodField::Owner),
            "node" => Some(PodField::Node),
            "app" => Some(PodField::App),
            "repo" | "repository" => Some(PodField::Repo),
            _ => None,
        }
    }

    fn key(field: Self::Field) -> &'static str {
        match field {
            PodField::Cluster => "cluster",
            PodField::Namespace => "ns",
            PodField::Status => "status",
            PodField::Owner => "owner",
            PodField::Node => "node",
            PodField::App => "app",
            PodField::Repo => "repo",
        }
    }

    fn label(field: Self::Field) -> &'static str {
        match field {
            PodField::Cluster => "Cluster",
            PodField::Namespace => "Namespace",
            PodField::Status => "Status",
            PodField::Owner => "Owner",
            PodField::Node => "Node",
            PodField::App => "App",
            PodField::Repo => "Repository",
        }
    }

    fn values(field: Self::Field, row: &Self::Row) -> Vec<String> {
        match field {
            PodField::Cluster => vec![row.pod.key.cluster.clone()],
            PodField::Namespace => vec![row.pod.key.namespace.clone()],
            PodField::Status => vec![row.pod.status.clone()],
            PodField::Owner => vec![row.owner_name().to_owned()],
            PodField::Node => vec![row.pod.node.clone()],
            PodField::App => ["app", "app.kubernetes.io/name"]
                .into_iter()
                .filter_map(|key| row.pod.label(key).map(str::to_owned))
                .collect(),
            PodField::Repo => row.repo.clone().into_iter().collect(),
        }
    }
}

/// What the log pane is following: the pod, which of its containers, and
/// whether the one before the last restart rather than the one running.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogFollow {
    pub key: PodKey,
    pub container: Option<String>,
    pub previous: bool,
}

/// What the run tells the worker. Each is a statement about what is worth
/// doing, so the worker can be told the same thing twice without harm.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AksRequest {
    /// The clusters `config.toml` names, whenever the file changes.
    Clusters(Vec<Cluster>),
    TabShowing(bool),
    /// Follow one pod's log, dropping whatever was followed before.
    Follow(LogFollow),
    Unfollow,
    /// Read every cluster again now.
    Refresh,
    Describe(PodKey),
    /// `kubectl delete pod`, which is how a pod with a controller is restarted.
    Delete(PodKey),
    Stop,
}

/// What the worker sends back. None of it is written anywhere: the screen
/// shows it, and the next read replaces it.
#[derive(Debug)]
pub enum AksEvent {
    /// One `(cluster, namespace)` read, so an unreachable cluster blanks
    /// nothing else. `namespace: None` is every namespace at once.
    Pods {
        cluster: String,
        namespace: Option<String>,
        pods: Result<Vec<Pod>, String>,
    },
    /// Lines of the followed log. `finished` says the stream has ended — the
    /// pod went, the connection dropped, or `kubectl` refused — and when it
    /// refused, the last line says why.
    LogLines {
        target: LogFollow,
        lines: Vec<String>,
        finished: bool,
    },
    Described {
        key: PodKey,
        text: Result<Vec<String>, String>,
    },
    Deleted {
        key: PodKey,
        error: Option<String>,
    },
    Stopped,
}

/// A running `kubectl logs -f`: what to read, what it complained about, and
/// the process to kill when the pane moves on.
pub struct LogTail {
    pub child: Option<Child>,
    pub stdout: Box<dyn Read + Send>,
    pub stderr: Option<Box<dyn Read + Send>>,
}

impl LogTail {
    /// A stream with nothing in it.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            child: None,
            stdout: Box::new(std::io::empty()),
            stderr: None,
        }
    }
}

/// Where the worker reads from. `kubectl` in the app; a fake in the tests.
/// Everything but the pod list has a default that answers nothing, so a fake
/// implements what its test needs.
pub trait KubeSource: Send {
    fn pods(&self, cluster: &Cluster, namespace: Option<&str>) -> Result<Vec<Pod>>;

    fn describe(&self, _cluster: &Cluster, _key: &PodKey) -> Result<String> {
        Ok(String::new())
    }

    fn delete(&self, _cluster: &Cluster, _key: &PodKey) -> Result<()> {
        Ok(())
    }

    fn logs(&self, _cluster: &Cluster, _target: &LogFollow) -> Result<LogTail> {
        Ok(LogTail::empty())
    }
}

/// The real thing: `kubectl` on the path, with the context the cluster names.
pub struct Kubectl;

impl Kubectl {
    /// `kubectl --context C --request-timeout=10s …`: its output, or the one
    /// line of its complaint that says what to fix.
    fn run(context: &str, arguments: &[&str]) -> Result<String> {
        let output = Command::new("kubectl")
            .arg("--context")
            .arg(context)
            .arg(REQUEST_TIMEOUT)
            .args(arguments)
            .stdin(Stdio::null())
            .output();
        let output = match output {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bail!("kubectl is not installed or not on PATH")
            }
            Err(error) => return Err(error).context("kubectl could not be run"),
        };
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            bail!(
                "{}",
                kubectl_error(&String::from_utf8_lossy(&output.stderr))
            )
        }
    }
}

impl KubeSource for Kubectl {
    fn pods(&self, cluster: &Cluster, namespace: Option<&str>) -> Result<Vec<Pod>> {
        let mut arguments = vec!["get", "pods", "-o", "json"];
        match namespace {
            Some(namespace) => arguments.extend(["-n", namespace]),
            None => arguments.push("--all-namespaces"),
        }
        let raw = Self::run(&cluster.context, &arguments)?;
        let listed: Value = serde_json::from_str(&raw)
            .context("kubectl answered with something other than JSON")?;
        Ok(listed["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| Pod::from_json(&cluster.name, item))
            .collect())
    }

    fn describe(&self, cluster: &Cluster, key: &PodKey) -> Result<String> {
        Self::run(
            &cluster.context,
            &["describe", "pod", "-n", &key.namespace, &key.name],
        )
    }

    fn delete(&self, cluster: &Cluster, key: &PodKey) -> Result<()> {
        Self::run(
            &cluster.context,
            &[
                "delete",
                "pod",
                "-n",
                &key.namespace,
                &key.name,
                "--wait=false",
            ],
        )
        .map(drop)
    }

    /// `kubectl logs -f`, left running: no request timeout, because the
    /// stream is meant to last, and killing it is the bound.
    fn logs(&self, cluster: &Cluster, target: &LogFollow) -> Result<LogTail> {
        let mut command = Command::new("kubectl");
        command
            .arg("--context")
            .arg(&cluster.context)
            .args(["logs", "-n", &target.key.namespace, &target.key.name])
            .args(["--timestamps", TAIL_LINES, "-f"]);
        if let Some(container) = &target.container {
            command.args(["-c", container]);
        }
        if target.previous {
            command.arg("-p");
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    anyhow!("kubectl is not installed or not on PATH")
                } else {
                    anyhow!("kubectl could not be run: {error}")
                }
            })?;
        let stdout: Box<dyn Read + Send> = match child.stdout.take() {
            Some(stdout) => Box::new(stdout),
            None => Box::new(std::io::empty()),
        };
        let stderr = child
            .stderr
            .take()
            .map(|stderr| Box::new(stderr) as Box<dyn Read + Send>);
        Ok(LogTail {
            child: Some(child),
            stdout,
            stderr,
        })
    }
}

/// The one line of `kubectl`'s complaint that says what to fix. The client
/// logs a retry or two before it gives up, and puts a documentation link
/// after the reason, so neither the first line nor the last is the one.
#[must_use]
pub fn kubectl_error(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !is_klog(line))
        .collect();
    let chosen = lines
        .iter()
        .find(|line| line.contains("az login"))
        .or_else(|| {
            lines.iter().find(|line| {
                line.starts_with("error:")
                    || line.starts_with("Error from server")
                    || line.starts_with("Unable to connect")
            })
        })
        .or_else(|| lines.first());
    chosen.map_or_else(
        || "kubectl failed".to_owned(),
        |line| {
            line.strip_prefix("error:")
                .map_or_else(|| (*line).to_owned(), |rest| rest.trim().to_owned())
        },
    )
}

/// `E0830 12:00:00.000000   12345 round_trippers.go:…] …`: the client's own
/// log line, which says nothing a person can act on.
fn is_klog(line: &str) -> bool {
    let mut characters = line.chars();
    matches!(characters.next(), Some('E' | 'W' | 'I' | 'F'))
        && characters.by_ref().take(4).all(|c| c.is_ascii_digit())
        && characters.next() == Some(' ')
}

/// The worker's own state, apart from the thread it usually runs on, so a
/// test can drive it with a clock of its own.
pub struct PodWatcher {
    source: Box<dyn KubeSource>,
    events: Sender<AksEvent>,
    /// Each cluster and when it is next worth reading. One cadence each, so
    /// a dead cluster backing off never slows a live one.
    clusters: Vec<(Cluster, Cadence)>,
    /// The sweep under way: which cluster, the namespaces still to read, and
    /// whether one of them found the cluster unreachable. One namespace is
    /// read per poll, so a request never waits behind a whole sweep.
    sweep: Option<Sweep>,
    tab_showing: bool,
    /// The stream on, the process behind it, and the flag that tells its
    /// reader the pane has moved on.
    follow: Option<(LogFollow, Option<Child>, Arc<AtomicBool>)>,
}

impl PodWatcher {
    #[must_use]
    pub fn new(source: Box<dyn KubeSource>, events: Sender<AksEvent>) -> Self {
        Self {
            source,
            events,
            clusters: Vec::new(),
            sweep: None,
            tab_showing: false,
            follow: None,
        }
    }

    /// One request. Answers whether to keep going.
    pub fn handle(&mut self, request: AksRequest) -> bool {
        match request {
            AksRequest::Stop => return false,
            AksRequest::Clusters(clusters) => self.set_clusters(clusters),
            AksRequest::TabShowing(showing) => {
                self.tab_showing = showing;
                if showing {
                    self.read_at_once();
                }
            }
            AksRequest::Refresh => self.read_at_once(),
            AksRequest::Follow(target) => self.start_follow(target),
            AksRequest::Unfollow => self.unfollow(),
            AksRequest::Describe(key) => {
                let text = self
                    .cluster(&key.cluster)
                    .and_then(|cluster| self.source.describe(cluster, &key))
                    .map(|text| text.lines().map(str::to_owned).collect())
                    .map_err(|error| format!("{error:#}"));
                let _ = self.events.send(AksEvent::Described { key, text });
            }
            AksRequest::Delete(key) => {
                let error = self
                    .cluster(&key.cluster)
                    .and_then(|cluster| self.source.delete(cluster, &key))
                    .err()
                    .map(|error| format!("{error:#}"));
                if error.is_none() {
                    // The replacement is worth seeing at once, not in fifteen
                    // seconds.
                    if let Some((_, cadence)) = self
                        .clusters
                        .iter_mut()
                        .find(|(cluster, _)| cluster.name == key.cluster)
                    {
                        *cadence = Cadence::new(POD_CADENCE);
                    }
                }
                let _ = self.events.send(AksEvent::Deleted { key, error });
            }
        }
        true
    }

    /// The file's clusters, keeping the cadence of any already known so a
    /// theme edit does not read everything again.
    fn set_clusters(&mut self, clusters: Vec<Cluster>) {
        // The sweep indexes the old list.
        self.sweep = None;
        let known = std::mem::take(&mut self.clusters);
        self.clusters = clusters
            .into_iter()
            .map(|cluster| {
                let cadence = known
                    .iter()
                    .find(|(held, _)| *held == cluster)
                    .map_or_else(|| Cadence::new(POD_CADENCE), |(_, cadence)| *cadence);
                (cluster, cadence)
            })
            .collect();
        if let Some((target, _, _)) = &self.follow
            && self.cluster(&target.key.cluster).is_err()
        {
            self.unfollow();
        }
    }

    fn read_at_once(&mut self) {
        for (_, cadence) in &mut self.clusters {
            *cadence = Cadence::new(POD_CADENCE);
        }
    }

    fn cluster(&self, name: &str) -> Result<&Cluster> {
        self.clusters
            .iter()
            .map(|(cluster, _)| cluster)
            .find(|cluster| cluster.name == name)
            .ok_or_else(|| anyhow!("cluster {name} is no longer in config.toml"))
    }

    /// Reads one namespace of the cluster whose turn it is, starting a sweep
    /// of the first cluster that is due when none is under way. One read a
    /// call, so a follow, a describe or a delete sent during a sweep is taken
    /// between two reads rather than after the last. Nothing while the tab is
    /// hidden: a cluster nobody is looking at is not worth a request.
    pub fn poll(&mut self, now: Instant) {
        if !self.tab_showing {
            return;
        }
        if self.sweep.is_none() {
            let Some(index) = self
                .clusters
                .iter()
                .position(|(_, cadence)| cadence.is_due(now))
            else {
                return;
            };
            self.sweep = Some(Sweep {
                index,
                queue: self.clusters[index]
                    .0
                    .targets()
                    .into_iter()
                    .map(|namespace| namespace.map(str::to_owned))
                    .collect(),
                unreachable: false,
            });
        }
        let Self {
            source,
            events,
            clusters,
            sweep,
            ..
        } = self;
        let Some(under_way) = sweep.as_mut() else {
            return;
        };
        let Some((cluster, cadence)) = clusters.get_mut(under_way.index) else {
            *sweep = None;
            return;
        };
        if !under_way.queue.is_empty() {
            let namespace = under_way.queue.remove(0);
            let pods = source.pods(cluster, namespace.as_deref());
            let failed = pods.as_ref().err().map(|error| format!("{error:#}"));
            let _ = events.send(AksEvent::Pods {
                cluster: cluster.name.clone(),
                namespace,
                pods: pods.map_err(|error| format!("{error:#}")),
            });
            // A server that answered with a refusal for one namespace will
            // answer for the next; one that could not be reached will not,
            // and is not asked twice.
            if let Some(message) = failed
                && !message.starts_with("Error from server")
            {
                under_way.unreachable = true;
                under_way.queue.clear();
            }
        }
        if under_way.queue.is_empty() {
            if under_way.unreachable {
                cadence.stretch();
            } else {
                cadence.relax();
            }
            cadence.polled(now);
            *sweep = None;
        }
    }

    /// Every read that is due at `now`, for a test that wants the whole
    /// sweep in one call.
    #[cfg(test)]
    pub(crate) fn poll_all(&mut self, now: Instant) {
        while self.until_due(now) == Some(Duration::ZERO) {
            self.poll(now);
        }
    }

    /// How long until something is due, or `None` while nothing is: a hidden
    /// tab, or no clusters at all. A sweep under way is due at once.
    #[must_use]
    pub fn until_due(&self, now: Instant) -> Option<Duration> {
        if !self.tab_showing {
            return None;
        }
        if self.sweep.is_some() {
            return Some(Duration::ZERO);
        }
        self.clusters
            .iter()
            .map(|(_, cadence)| cadence.until_due(now))
            .min()
    }

    /// Opens one stream, closing whatever was open. What `kubectl` refuses
    /// goes into the pane where the user is looking, as the stream's one and
    /// only line.
    fn start_follow(&mut self, target: LogFollow) {
        self.unfollow();
        let tail = self
            .cluster(&target.key.cluster)
            .and_then(|cluster| self.source.logs(cluster, &target));
        match tail {
            Ok(LogTail {
                child,
                stdout,
                stderr,
            }) => {
                let cancelled = Arc::new(AtomicBool::new(false));
                self.follow = Some((target.clone(), child, Arc::clone(&cancelled)));
                let events = self.events.clone();
                let _ = thread::Builder::new()
                    .name("ticket-aks-log".into())
                    .spawn(move || stream(stdout, stderr, target, &cancelled, &events));
            }
            Err(error) => {
                let _ = self.events.send(AksEvent::LogLines {
                    target,
                    lines: vec![format!("\u{2026} {error:#}")],
                    finished: true,
                });
            }
        }
    }

    /// Closes the stream: the process is killed and reaped, and its reader
    /// told to say nothing more.
    fn unfollow(&mut self) {
        if let Some((_, child, cancelled)) = self.follow.take() {
            cancelled.store(true, Ordering::SeqCst);
            if let Some(mut child) = child {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    /// What the stream is on, for a test.
    #[cfg(test)]
    fn following(&self) -> Option<&LogFollow> {
        self.follow.as_ref().map(|(target, _, _)| target)
    }
}

/// One sweep of one cluster: what is left to read, and how it has gone.
struct Sweep {
    index: usize,
    queue: Vec<Option<String>>,
    unreachable: bool,
}

impl Drop for PodWatcher {
    fn drop(&mut self) {
        self.unfollow();
    }
}

/// The reader behind a follow: one event per line until the stream ends, then
/// one saying so, with whatever `kubectl` complained about on the way out.
// ponytail: one event per line; read with fill_buf and split if a chatty pod
// ever shows up in a profile.
fn stream(
    stdout: Box<dyn Read + Send>,
    stderr: Option<Box<dyn Read + Send>>,
    target: LogFollow,
    cancelled: &AtomicBool,
    events: &Sender<AksEvent>,
) {
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if cancelled.load(Ordering::SeqCst) {
            return;
        }
        let sent = events.send(AksEvent::LogLines {
            target: target.clone(),
            lines: vec![line],
            finished: false,
        });
        if sent.is_err() {
            return;
        }
    }
    if cancelled.load(Ordering::SeqCst) {
        return;
    }
    let mut complaint = String::new();
    if let Some(mut stderr) = stderr {
        let _ = stderr.read_to_string(&mut complaint);
    }
    let lines = if complaint.trim().is_empty() {
        Vec::new()
    } else {
        vec![format!("\u{2026} {}", kubectl_error(&complaint))]
    };
    let _ = events.send(AksEvent::LogLines {
        target,
        lines,
        finished: true,
    });
}

/// The handle the main thread holds: requests in, events out.
pub struct AksHandle {
    requests: Sender<AksRequest>,
    events: Receiver<AksEvent>,
    stopped: Cell<bool>,
}

impl AksHandle {
    /// Starts the worker on its own thread. It ends when the handle is
    /// dropped, and any stream it had open ends with it.
    pub fn spawn(source: Box<dyn KubeSource>) -> Result<Self> {
        let (request_sender, request_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("ticket-aks".into())
            .spawn(move || watch(PodWatcher::new(source, event_sender), &request_receiver))
            .context("failed to start the cluster worker")?;
        Ok(Self {
            requests: request_sender,
            events: event_receiver,
            stopped: Cell::new(false),
        })
    }

    /// Tells the worker what is worth doing. Fails only when it is gone.
    pub fn send(&self, request: AksRequest) -> Result<()> {
        self.requests
            .send(request)
            .context("the cluster worker stopped")
    }

    /// The next event, if one is waiting.
    pub fn try_event(&self) -> Option<AksEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                (!self.stopped.replace(true)).then_some(AksEvent::Stopped)
            }
        }
    }
}

/// The loop: read whatever is due, then wait until the next thing is or a
/// request arrives, whichever comes first.
fn watch(mut watcher: PodWatcher, requests: &Receiver<AksRequest>) {
    loop {
        watcher.poll(Instant::now());
        let wait = watcher
            .until_due(Instant::now())
            .unwrap_or(Duration::from_secs(3600));
        match requests.recv_timeout(wait) {
            Ok(request) => {
                if !watcher.handle(request) {
                    return;
                }
                // Everything else waiting is taken now, so a burst of requests
                // costs one poll rather than one each.
                while let Ok(request) = requests.try_recv() {
                    if !watcher.handle(request) {
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Cursor;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::filter::{MatchContext, parse_query};

    /// One pod as `kubectl get pods -o json` lists it, with the parts a case
    /// needs and nothing else.
    fn item(name: &str, extra: Value) -> Value {
        let mut base = json!({
            "metadata": {
                "name": name,
                "namespace": "orders",
                "creationTimestamp": "2026-08-30T10:00:00Z",
                "labels": {"app": "orders-api", "pod-template-hash": "7d9f5b"},
                "ownerReferences": [{"kind": "ReplicaSet", "name": "orders-api-7d9f5b", "controller": true}]
            },
            "spec": {
                "nodeName": "aks-nodepool1-0",
                "containers": [{"name": "api", "image": "myacr.azurecr.io/team/orders-api:1.2.3"}]
            },
            "status": {
                "phase": "Running",
                "podIP": "10.0.0.7",
                "containerStatuses": [{
                    "name": "api", "ready": true, "restartCount": 2,
                    "image": "myacr.azurecr.io/team/orders-api:1.2.3",
                    "state": {"running": {"startedAt": "2026-08-30T10:00:05Z"}},
                    "lastState": {"terminated": {"reason": "OOMKilled", "exitCode": 137}}
                }]
            }
        });
        merge(&mut base, extra);
        base
    }

    fn merge(base: &mut Value, extra: Value) {
        match (base, extra) {
            (Value::Object(base), Value::Object(extra)) => {
                for (key, value) in extra {
                    match base.get_mut(&key) {
                        Some(held) if held.is_object() && value.is_object() => merge(held, value),
                        _ => {
                            base.insert(key, value);
                        }
                    }
                }
            }
            (base, extra) => *base = extra,
        }
    }

    pub(crate) fn pod(cluster: &str, namespace: &str, name: &str, status: &str) -> Pod {
        Pod {
            key: PodKey {
                cluster: cluster.to_owned(),
                namespace: namespace.to_owned(),
                name: name.to_owned(),
            },
            status: status.to_owned(),
            ready: (1, 1),
            restarts: 0,
            created: Timestamp::parse("2026-08-30T10:00:00Z").ok(),
            node: "aks-nodepool1-0".to_owned(),
            ip: "10.0.0.7".to_owned(),
            owner: Some(("Deployment".to_owned(), "orders-api".to_owned())),
            containers: vec![Container {
                name: "api".to_owned(),
                image: "myacr.azurecr.io/team/orders-api:1.2.3".to_owned(),
                ready: true,
                restarts: 0,
                state: "Running".to_owned(),
                last_termination: None,
            }],
            labels: vec![("app".to_owned(), "orders-api".to_owned())],
        }
    }

    pub(crate) fn cluster(name: &str, namespaces: &[&str]) -> Cluster {
        Cluster {
            name: name.to_owned(),
            context: format!("aks-{name}"),
            namespaces: namespaces.iter().map(|held| (*held).to_owned()).collect(),
        }
    }

    #[test]
    fn a_pod_reads_its_ready_count_restarts_owner_node_and_containers_from_kubectls_json() {
        let pod = Pod::from_json("qa", &item("orders-api-7d9f5b-abc12", json!({}))).unwrap();
        assert_eq!(pod.key.cluster, "qa");
        assert_eq!(pod.key.namespace, "orders");
        assert_eq!(pod.key.name, "orders-api-7d9f5b-abc12");
        assert_eq!(pod.status, "Running");
        assert_eq!(pod.ready_label(), "1/1");
        assert_eq!(pod.restarts, 2);
        assert_eq!(pod.node, "aks-nodepool1-0");
        assert_eq!(pod.ip, "10.0.0.7");
        assert_eq!(
            pod.created.map(Timestamp::to_rfc3339).as_deref(),
            Some("2026-08-30T10:00:00Z")
        );
        assert_eq!(
            pod.owner,
            Some(("Deployment".to_owned(), "orders-api".to_owned()))
        );
        assert_eq!(pod.containers.len(), 1);
        assert_eq!(pod.containers[0].state, "Running");
        assert_eq!(
            pod.containers[0].last_termination,
            Some(("OOMKilled".to_owned(), 137))
        );
        assert_eq!(pod.label("app"), Some("orders-api"));
        assert!(pod.restartable());
        assert_eq!(pod.glyph(), "\u{25cf}");
        assert!(Pod::from_json("qa", &json!({"metadata": {}})).is_none());
    }

    #[test]
    fn the_status_word_follows_kubectl_for_running_pending_creating_crashloop_error_completed_terminating_and_init()
     {
        let cases = [
            (json!({}), "Running", "\u{25cf}"),
            (
                json!({"status": {"phase": "Pending", "containerStatuses": []}}),
                "Pending",
                "\u{25d0}",
            ),
            (
                json!({"status": {"phase": "Pending", "containerStatuses": [
                    {"name": "api", "ready": false, "state": {"waiting": {"reason": "ContainerCreating"}}}
                ]}}),
                "ContainerCreating",
                "\u{25d0}",
            ),
            (
                json!({"status": {"containerStatuses": [
                    {"name": "api", "ready": false, "restartCount": 9, "state": {"waiting": {"reason": "CrashLoopBackOff"}}}
                ]}}),
                "CrashLoopBackOff",
                "\u{2717}",
            ),
            (
                json!({"status": {"phase": "Failed", "containerStatuses": [
                    {"name": "api", "ready": false, "state": {"terminated": {"reason": "Error", "exitCode": 1}}}
                ]}}),
                "Error",
                "\u{2717}",
            ),
            (
                json!({"status": {"phase": "Failed", "containerStatuses": [
                    {"name": "api", "ready": false, "state": {"terminated": {"exitCode": 137}}}
                ]}}),
                "ExitCode:137",
                "\u{2717}",
            ),
            (
                json!({"status": {"phase": "Succeeded", "containerStatuses": [
                    {"name": "api", "ready": false, "state": {"terminated": {"reason": "Completed", "exitCode": 0}}}
                ]}}),
                "Completed",
                "\u{2713}",
            ),
            // A sidecar that finished beside a server still running reads as
            // running, the way kubectl puts it back.
            (
                json!({"spec": {"containers": [{"name": "api"}, {"name": "init-db"}]},
                       "status": {"containerStatuses": [
                    {"name": "api", "ready": true, "state": {"running": {}}},
                    {"name": "init-db", "ready": false, "state": {"terminated": {"reason": "Completed", "exitCode": 0}}}
                ]}}),
                "Running",
                "\u{25d0}",
            ),
            (
                json!({"metadata": {"deletionTimestamp": "2026-08-30T11:00:00Z"}}),
                "Terminating",
                "\u{25d0}",
            ),
            (
                json!({"spec": {"initContainers": [{"name": "migrate"}, {"name": "seed"}]},
                       "status": {"phase": "Pending", "initContainerStatuses": [
                    {"name": "migrate", "state": {"terminated": {"exitCode": 0}}},
                    {"name": "seed", "state": {"running": {}}}
                ]}}),
                "Init:1/2",
                "\u{25d0}",
            ),
            (
                json!({"spec": {"initContainers": [{"name": "migrate"}]},
                       "status": {"phase": "Pending", "initContainerStatuses": [
                    {"name": "migrate", "state": {"waiting": {"reason": "CrashLoopBackOff"}}}
                ]}}),
                "Init:CrashLoopBackOff",
                "\u{2717}",
            ),
            (
                json!({"status": {"phase": "Failed", "reason": "Evicted", "containerStatuses": []}}),
                "Evicted",
                "\u{2717}",
            ),
            (
                json!({"status": {"containerStatuses": [
                    {"name": "api", "ready": false, "state": {"waiting": {"reason": "ImagePullBackOff"}}}
                ]}}),
                "ImagePullBackOff",
                "\u{2717}",
            ),
        ];
        for (extra, word, glyph) in cases {
            let pod = Pod::from_json("qa", &item("p", extra)).unwrap();
            assert_eq!(pod.status, word);
            assert_eq!(pod.glyph(), glyph, "{word}");
        }
    }

    #[test]
    fn a_replica_set_owner_reads_as_its_deployment_when_the_template_hash_says_so() {
        let deployment = Pod::from_json("qa", &item("p", json!({}))).unwrap();
        assert_eq!(
            deployment.owner,
            Some(("Deployment".to_owned(), "orders-api".to_owned()))
        );
        let stateful = Pod::from_json(
            "qa",
            &item(
                "p",
                json!({"metadata": {"ownerReferences": [{"kind": "StatefulSet", "name": "redis", "controller": true}]}}),
            ),
        )
        .unwrap();
        assert_eq!(
            stateful.owner,
            Some(("StatefulSet".to_owned(), "redis".to_owned()))
        );
        let unhashed = Pod::from_json(
            "qa",
            &item(
                "p",
                json!({"metadata": {"labels": {"pod-template-hash": ""}, "ownerReferences": [{"kind": "ReplicaSet", "name": "orders-api-7d9f5b", "controller": true}]}}),
            ),
        )
        .unwrap();
        assert_eq!(
            unhashed.owner,
            Some(("ReplicaSet".to_owned(), "orders-api-7d9f5b".to_owned()))
        );
        let bare = Pod::from_json(
            "qa",
            &item("p", json!({"metadata": {"ownerReferences": []}})),
        )
        .unwrap();
        assert_eq!(bare.owner, None);
        assert!(!bare.restartable());
    }

    #[test]
    fn repo_candidates_are_the_app_labels_then_each_image_name_without_registry_tag_or_digest() {
        let mut pod = pod("qa", "orders", "p", "Running");
        pod.labels = vec![
            ("app".to_owned(), "orders".to_owned()),
            ("app.kubernetes.io/name".to_owned(), "orders-api".to_owned()),
        ];
        pod.containers.push(Container {
            name: "sidecar".to_owned(),
            image: "ghcr.io/x/envoy@sha256:abcdef".to_owned(),
            ready: true,
            restarts: 0,
            state: "Running".to_owned(),
            last_termination: None,
        });
        pod.containers.push(Container {
            name: "plain".to_owned(),
            image: "nginx".to_owned(),
            ready: true,
            restarts: 0,
            state: "Running".to_owned(),
            last_termination: None,
        });
        assert_eq!(
            pod.repo_candidates(),
            vec!["orders-api", "orders", "envoy", "nginx"]
        );
        assert_eq!(image_name("a/b/c:1.0"), Some("c".to_owned()));
        assert_eq!(image_name("c@sha256:ff"), Some("c".to_owned()));
        assert_eq!(image_name("registry:5000/c"), Some("c".to_owned()));
        assert_eq!(image_name(""), None);
        let row = PodRow::new(pod, &["Orders-API".to_owned(), "rust-game".to_owned()]);
        assert_eq!(row.repo.as_deref(), Some("Orders-API"));
    }

    #[test]
    fn the_grammar_narrows_by_cluster_status_and_app_and_the_rest_matches_the_name() {
        let rows = [
            PodRow::new(pod("qa", "orders", "orders-api-1", "Running"), &[]),
            PodRow::new(pod("qa", "orders", "orders-api-2", "CrashLoopBackOff"), &[]),
            PodRow::new(pod("prod", "billing", "billing-1", "Running"), &[]),
        ];
        let matching = |query: &str| -> Vec<String> {
            let parsed = parse_query::<PodSchema>(query);
            let context = MatchContext::now();
            rows.iter()
                .filter(|row| {
                    parsed.filters.matches_in(row, false, &context)
                        && row.matches_fuzzy(&parsed.fuzzy)
                })
                .map(|row| row.pod.key.name.clone())
                .collect()
        };
        assert_eq!(matching("cluster:qa"), vec!["orders-api-1", "orders-api-2"]);
        assert_eq!(matching("status:crashloopbackoff"), vec!["orders-api-2"]);
        assert_eq!(
            matching("app:orders-api ns:orders"),
            vec!["orders-api-1", "orders-api-2"]
        );
        assert_eq!(matching("billing"), vec!["billing-1"]);
        assert_eq!(matching("owner:orders-api cluster:prod"), vec!["billing-1"]);
    }

    #[test]
    fn kubectl_errors_read_as_the_one_line_that_says_what_to_fix() {
        assert_eq!(
            kubectl_error("error: context \"aks-qa\" does not exist\n"),
            "context \"aks-qa\" does not exist"
        );
        assert_eq!(
            kubectl_error(
                "E0830 12:00:00.000000   12345 memcache.go:265] couldn't get current server API group list\nUnable to connect to the server: getting credentials: exec: executable kubelogin not found\n\nIt looks like you are trying to use a client-go credential plugin\nSee https://kubernetes.io/docs/reference/access-authn-authz/authentication/#client-go-credential-plugins\n"
            ),
            "Unable to connect to the server: getting credentials: exec: executable kubelogin not found"
        );
        assert_eq!(
            kubectl_error(
                "ERROR: AADSTS700082: The refresh token has expired. Please run 'az login' to setup account.\nUnable to connect to the server: getting credentials: exec: executable kubelogin failed with exit code 1\n"
            ),
            "ERROR: AADSTS700082: The refresh token has expired. Please run 'az login' to setup account."
        );
        assert_eq!(
            kubectl_error(
                "Error from server (Forbidden): pods is forbidden: User \"j\" cannot list resource \"pods\" in API group \"\" in the namespace \"billing\"\n"
            ),
            "Error from server (Forbidden): pods is forbidden: User \"j\" cannot list resource \"pods\" in API group \"\" in the namespace \"billing\""
        );
        assert_eq!(
            kubectl_error(
                "E0830 12:00:00.000000   12345 round_trippers.go:1] x\nUnable to connect to the server: net/http: request canceled (Client.Timeout exceeded while awaiting headers)\n"
            ),
            "Unable to connect to the server: net/http: request canceled (Client.Timeout exceeded while awaiting headers)"
        );
        assert_eq!(kubectl_error("\n  \n"), "kubectl failed");
    }

    /// One `(cluster, namespace)` read and what it answers.
    type Answer = (String, Option<String>, Result<Vec<Pod>, String>);

    /// One `(cluster, namespace)` the fake was asked for.
    type Asked = (String, Option<String>);

    /// A source over canned answers, counting what it was asked.
    #[derive(Clone, Default)]
    pub(crate) struct FakeKube {
        /// What each `(cluster, namespace)` read answers with; a cluster
        /// with no entry answers nothing.
        pub answers: Arc<Mutex<Vec<Answer>>>,
        pub reads: Arc<Mutex<Vec<Asked>>>,
        pub describe_text: Arc<Mutex<String>>,
        pub delete_error: Arc<Mutex<Option<String>>>,
        pub deletes: Arc<Mutex<Vec<PodKey>>>,
        pub log_text: Arc<Mutex<String>>,
        pub follows: Arc<Mutex<Vec<LogFollow>>>,
    }

    impl FakeKube {
        pub(crate) fn answer(
            &self,
            cluster: &str,
            namespace: Option<&str>,
            pods: Result<Vec<Pod>, &str>,
        ) {
            self.answers.lock().unwrap().push((
                cluster.to_owned(),
                namespace.map(str::to_owned),
                pods.map_err(str::to_owned),
            ));
        }
    }

    impl KubeSource for FakeKube {
        fn pods(&self, cluster: &Cluster, namespace: Option<&str>) -> Result<Vec<Pod>> {
            self.reads
                .lock()
                .unwrap()
                .push((cluster.name.clone(), namespace.map(str::to_owned)));
            let answers = self.answers.lock().unwrap();
            let answer = answers
                .iter()
                .find(|(held, ns, _)| *held == cluster.name && ns.as_deref() == namespace)
                .map(|(_, _, pods)| pods.clone());
            match answer {
                Some(Ok(pods)) => Ok(pods),
                Some(Err(message)) => Err(anyhow!(message)),
                None => Ok(Vec::new()),
            }
        }

        fn describe(&self, _cluster: &Cluster, _key: &PodKey) -> Result<String> {
            Ok(self.describe_text.lock().unwrap().clone())
        }

        fn delete(&self, _cluster: &Cluster, key: &PodKey) -> Result<()> {
            self.deletes.lock().unwrap().push(key.clone());
            match self.delete_error.lock().unwrap().clone() {
                Some(message) => Err(anyhow!(message)),
                None => Ok(()),
            }
        }

        fn logs(&self, _cluster: &Cluster, target: &LogFollow) -> Result<LogTail> {
            self.follows.lock().unwrap().push(target.clone());
            if target.container.as_deref() == Some("missing") {
                bail!("container missing is not valid for pod {}", target.key.name);
            }
            Ok(LogTail {
                child: None,
                stdout: Box::new(Cursor::new(self.log_text.lock().unwrap().clone())),
                stderr: Some(Box::new(Cursor::new(String::new()))),
            })
        }
    }

    fn watcher(fake: &FakeKube) -> (PodWatcher, Receiver<AksEvent>) {
        let (sender, receiver) = mpsc::channel();
        (PodWatcher::new(Box::new(fake.clone()), sender), receiver)
    }

    fn drain(receiver: &Receiver<AksEvent>) -> Vec<AksEvent> {
        std::iter::from_fn(|| receiver.try_recv().ok()).collect()
    }

    fn pods_events(events: &[AksEvent]) -> Vec<(String, Option<String>, Result<usize, String>)> {
        events
            .iter()
            .filter_map(|event| match event {
                AksEvent::Pods {
                    cluster,
                    namespace,
                    pods,
                } => Some((
                    cluster.clone(),
                    namespace.clone(),
                    pods.as_ref().map(Vec::len).map_err(Clone::clone),
                )),
                _ => None,
            })
            .collect()
    }

    fn key(cluster: &str, name: &str) -> PodKey {
        PodKey {
            cluster: cluster.to_owned(),
            namespace: "orders".to_owned(),
            name: name.to_owned(),
        }
    }

    #[test]
    fn nothing_is_read_while_the_tab_is_hidden_and_every_namespace_is_read_the_moment_it_shows() {
        let fake = FakeKube::default();
        fake.answer(
            "qa",
            Some("orders"),
            Ok(vec![pod("qa", "orders", "a", "Running")]),
        );
        let (mut watcher, receiver) = watcher(&fake);
        watcher.handle(AksRequest::Clusters(vec![
            cluster("qa", &["orders", "billing"]),
            cluster("prod", &[]),
        ]));
        let start = Instant::now();
        watcher.poll_all(start);
        assert!(
            fake.reads.lock().unwrap().is_empty(),
            "hidden tab, no reads"
        );
        assert_eq!(watcher.until_due(start), None);

        watcher.handle(AksRequest::TabShowing(true));
        assert_eq!(watcher.until_due(start), Some(Duration::ZERO));
        watcher.poll_all(start);
        assert_eq!(
            *fake.reads.lock().unwrap(),
            vec![
                ("qa".to_owned(), Some("orders".to_owned())),
                ("qa".to_owned(), Some("billing".to_owned())),
                ("prod".to_owned(), None),
            ]
        );
        assert_eq!(
            pods_events(&drain(&receiver)),
            vec![
                ("qa".to_owned(), Some("orders".to_owned()), Ok(1)),
                ("qa".to_owned(), Some("billing".to_owned()), Ok(0)),
                ("prod".to_owned(), None, Ok(0)),
            ]
        );
        // Not again until the cadence is up.
        watcher.poll_all(start + Duration::from_secs(14));
        assert_eq!(fake.reads.lock().unwrap().len(), 3);
        assert_eq!(
            watcher.until_due(start + Duration::from_secs(14)),
            Some(Duration::from_secs(1))
        );
        watcher.poll_all(start + POD_CADENCE);
        assert_eq!(fake.reads.lock().unwrap().len(), 6);
        // Hidden again: quiet, whatever is due.
        watcher.handle(AksRequest::TabShowing(false));
        watcher.poll_all(start + POD_CADENCE * 3);
        assert_eq!(fake.reads.lock().unwrap().len(), 6);
    }

    #[test]
    fn each_cluster_and_namespace_answers_on_its_own_so_an_unreachable_cluster_blanks_nothing_else()
    {
        let fake = FakeKube::default();
        fake.answer(
            "qa",
            Some("orders"),
            Err("context \"aks-qa\" does not exist"),
        );
        fake.answer(
            "prod",
            Some("orders"),
            Ok(vec![pod("prod", "orders", "a", "Running")]),
        );
        let (mut watcher, receiver) = watcher(&fake);
        watcher.handle(AksRequest::Clusters(vec![
            cluster("qa", &["orders", "billing"]),
            cluster("prod", &["orders"]),
        ]));
        watcher.handle(AksRequest::TabShowing(true));
        let start = Instant::now();
        watcher.poll_all(start);
        // qa's second namespace was not asked: the cluster could not be
        // reached at all.
        assert_eq!(
            pods_events(&drain(&receiver)),
            vec![
                (
                    "qa".to_owned(),
                    Some("orders".to_owned()),
                    Err("context \"aks-qa\" does not exist".to_owned())
                ),
                ("prod".to_owned(), Some("orders".to_owned()), Ok(1)),
            ]
        );
        // The failing cluster backs off; the live one keeps its cadence.
        watcher.poll_all(start + POD_CADENCE);
        let reads = fake.reads.lock().unwrap().clone();
        assert_eq!(
            reads.iter().filter(|(cluster, _)| cluster == "qa").count(),
            1,
            "{reads:?}"
        );
        assert_eq!(
            reads
                .iter()
                .filter(|(cluster, _)| cluster == "prod")
                .count(),
            2,
            "{reads:?}"
        );
        watcher.poll_all(start + POD_CADENCE * 2);
        assert_eq!(
            fake.reads
                .lock()
                .unwrap()
                .iter()
                .filter(|(cluster, _)| cluster == "qa")
                .count(),
            2
        );
    }

    #[test]
    fn a_forbidden_namespace_does_not_stop_the_clusters_other_namespaces() {
        let fake = FakeKube::default();
        fake.answer(
            "qa",
            Some("billing"),
            Err("Error from server (Forbidden): pods is forbidden in the namespace \"billing\""),
        );
        fake.answer(
            "qa",
            Some("orders"),
            Ok(vec![pod("qa", "orders", "a", "Running")]),
        );
        let (mut watcher, receiver) = watcher(&fake);
        watcher.handle(AksRequest::Clusters(vec![cluster(
            "qa",
            &["billing", "orders"],
        )]));
        watcher.handle(AksRequest::TabShowing(true));
        let start = Instant::now();
        watcher.poll_all(start);
        let events = pods_events(&drain(&receiver));
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events[0].2.is_err());
        assert_eq!(events[1].2, Ok(1));
        // The server answered, so the cadence is not stretched.
        assert_eq!(watcher.until_due(start), Some(POD_CADENCE));
    }

    #[test]
    fn a_refresh_and_a_delete_read_the_list_again_at_once() {
        let fake = FakeKube::default();
        let (mut watcher, receiver) = watcher(&fake);
        watcher.handle(AksRequest::Clusters(vec![cluster("qa", &["orders"])]));
        watcher.handle(AksRequest::TabShowing(true));
        let start = Instant::now();
        watcher.poll_all(start);
        assert_eq!(fake.reads.lock().unwrap().len(), 1);
        watcher.handle(AksRequest::Refresh);
        assert_eq!(
            watcher.until_due(start + Duration::from_secs(1)),
            Some(Duration::ZERO)
        );
        watcher.poll_all(start + Duration::from_secs(1));
        assert_eq!(fake.reads.lock().unwrap().len(), 2);

        watcher.handle(AksRequest::Delete(key("qa", "a")));
        assert_eq!(*fake.deletes.lock().unwrap(), vec![key("qa", "a")]);
        assert_eq!(
            watcher.until_due(start + Duration::from_secs(2)),
            Some(Duration::ZERO)
        );
        watcher.poll_all(start + Duration::from_secs(2));
        assert_eq!(fake.reads.lock().unwrap().len(), 3);
        let deleted = drain(&receiver).into_iter().find_map(|event| match event {
            AksEvent::Deleted { key, error } => Some((key, error)),
            _ => None,
        });
        assert_eq!(deleted, Some((key("qa", "a"), None)));

        // A refusal comes back as kubectl's words, and reads nothing again.
        *fake.delete_error.lock().unwrap() = Some("pods \"a\" is forbidden".to_owned());
        watcher.handle(AksRequest::Delete(key("qa", "a")));
        assert_eq!(
            watcher.until_due(start + Duration::from_secs(3)),
            Some(POD_CADENCE - Duration::from_secs(1))
        );
        let deleted = drain(&receiver).into_iter().find_map(|event| match event {
            AksEvent::Deleted { error, .. } => Some(error),
            _ => None,
        });
        assert_eq!(deleted, Some(Some("pods \"a\" is forbidden".to_owned())));
        // A cluster the file no longer names is said so.
        watcher.handle(AksRequest::Delete(key("gone", "a")));
        let deleted = drain(&receiver).into_iter().find_map(|event| match event {
            AksEvent::Deleted { error, .. } => Some(error),
            _ => None,
        });
        assert_eq!(
            deleted,
            Some(Some("cluster gone is no longer in config.toml".to_owned()))
        );
    }

    /// Every event the stream sends, waited for until it says it finished.
    fn stream_events(receiver: &Receiver<AksEvent>) -> Vec<(LogFollow, Vec<String>, bool)> {
        let mut events = Vec::new();
        while let Ok(event) = receiver.recv_timeout(Duration::from_secs(5)) {
            if let AksEvent::LogLines {
                target,
                lines,
                finished,
            } = event
            {
                let done = finished;
                events.push((target, lines, finished));
                if done {
                    break;
                }
            }
        }
        events
    }

    #[test]
    fn following_a_pod_streams_its_lines_then_says_the_stream_ended() {
        let fake = FakeKube::default();
        *fake.log_text.lock().unwrap() =
            "2026-08-30T10:00:00Z hello\n2026-08-30T10:00:01Z world\n".to_owned();
        let (mut watcher, receiver) = watcher(&fake);
        watcher.handle(AksRequest::Clusters(vec![cluster("qa", &["orders"])]));
        let target = LogFollow {
            key: key("qa", "a"),
            container: Some("api".to_owned()),
            previous: false,
        };
        watcher.handle(AksRequest::Follow(target.clone()));
        assert_eq!(watcher.following(), Some(&target));
        let events = stream_events(&receiver);
        let lines: Vec<String> = events
            .iter()
            .flat_map(|(_, lines, _)| lines.clone())
            .collect();
        assert_eq!(
            lines,
            vec!["2026-08-30T10:00:00Z hello", "2026-08-30T10:00:01Z world"]
        );
        assert!(events.iter().all(|(held, _, _)| *held == target));
        assert_eq!(events.last().map(|(_, _, finished)| *finished), Some(true));
        assert_eq!(*fake.follows.lock().unwrap(), vec![target]);
    }

    #[test]
    fn following_another_pod_replaces_the_stream_and_a_bad_container_says_so_in_the_pane() {
        let fake = FakeKube::default();
        *fake.log_text.lock().unwrap() = "line\n".to_owned();
        let (mut watcher, receiver) = watcher(&fake);
        watcher.handle(AksRequest::Clusters(vec![cluster("qa", &["orders"])]));
        let first = LogFollow {
            key: key("qa", "a"),
            container: None,
            previous: false,
        };
        let second = LogFollow {
            key: key("qa", "b"),
            container: None,
            previous: true,
        };
        watcher.handle(AksRequest::Follow(first.clone()));
        let _ = stream_events(&receiver);
        watcher.handle(AksRequest::Follow(second.clone()));
        assert_eq!(watcher.following(), Some(&second));
        let events = stream_events(&receiver);
        assert!(
            events.iter().all(|(held, _, _)| *held == second),
            "{events:?}"
        );
        assert_eq!(*fake.follows.lock().unwrap(), vec![first, second.clone()]);

        watcher.handle(AksRequest::Unfollow);
        assert_eq!(watcher.following(), None);

        let bad = LogFollow {
            key: key("qa", "c"),
            container: Some("missing".to_owned()),
            previous: false,
        };
        watcher.handle(AksRequest::Follow(bad.clone()));
        let events = stream_events(&receiver);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, bad);
        assert_eq!(
            events[0].1,
            vec!["\u{2026} container missing is not valid for pod c"]
        );
        assert!(events[0].2);
        assert_eq!(watcher.following(), None);
    }

    #[test]
    fn a_describe_answers_with_its_text_and_a_missing_cluster_with_the_file() {
        let fake = FakeKube::default();
        *fake.describe_text.lock().unwrap() = "Name: a\nNamespace: orders\n".to_owned();
        let (mut watcher, receiver) = watcher(&fake);
        watcher.handle(AksRequest::Clusters(vec![cluster("qa", &["orders"])]));
        watcher.handle(AksRequest::Describe(key("qa", "a")));
        watcher.handle(AksRequest::Describe(key("prod", "a")));
        let described: Vec<(PodKey, Result<Vec<String>, String>)> = drain(&receiver)
            .into_iter()
            .filter_map(|event| match event {
                AksEvent::Described { key, text } => Some((key, text)),
                _ => None,
            })
            .collect();
        assert_eq!(
            described,
            vec![
                (
                    key("qa", "a"),
                    Ok(vec!["Name: a".to_owned(), "Namespace: orders".to_owned()])
                ),
                (
                    key("prod", "a"),
                    Err("cluster prod is no longer in config.toml".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn new_clusters_from_the_file_are_read_at_once_and_a_removed_one_ends_its_follow() {
        let fake = FakeKube::default();
        let (mut watcher, receiver) = watcher(&fake);
        watcher.handle(AksRequest::Clusters(vec![cluster("qa", &["orders"])]));
        watcher.handle(AksRequest::TabShowing(true));
        let start = Instant::now();
        watcher.poll_all(start);
        assert_eq!(fake.reads.lock().unwrap().len(), 1);
        // The same cluster again keeps its cadence; a new one is due at once.
        watcher.handle(AksRequest::Clusters(vec![
            cluster("qa", &["orders"]),
            cluster("prod", &["orders"]),
        ]));
        assert_eq!(
            watcher.until_due(start + Duration::from_secs(1)),
            Some(Duration::ZERO)
        );
        watcher.poll_all(start + Duration::from_secs(1));
        assert_eq!(
            *fake.reads.lock().unwrap(),
            vec![
                ("qa".to_owned(), Some("orders".to_owned())),
                ("prod".to_owned(), Some("orders".to_owned())),
            ]
        );
        let target = LogFollow {
            key: key("prod", "a"),
            container: None,
            previous: false,
        };
        watcher.handle(AksRequest::Follow(target.clone()));
        let _ = stream_events(&receiver);
        assert_eq!(watcher.following(), Some(&target));
        watcher.handle(AksRequest::Clusters(vec![cluster("qa", &["orders"])]));
        assert_eq!(watcher.following(), None);
        assert!(!watcher.handle(AksRequest::Stop));
    }

    #[test]
    fn the_handle_runs_the_worker_on_its_own_thread_and_says_once_when_it_stops() {
        let fake = FakeKube::default();
        fake.answer(
            "qa",
            Some("orders"),
            Ok(vec![pod("qa", "orders", "a", "Running")]),
        );
        let handle = AksHandle::spawn(Box::new(fake)).unwrap();
        handle
            .send(AksRequest::Clusters(vec![cluster("qa", &["orders"])]))
            .unwrap();
        handle.send(AksRequest::TabShowing(true)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = None;
        while Instant::now() < deadline && seen.is_none() {
            if let Some(AksEvent::Pods { cluster, pods, .. }) = handle.try_event() {
                seen = Some((cluster, pods.map(|pods| pods.len())));
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
        assert_eq!(seen, Some(("qa".to_owned(), Ok(1))));
        handle.send(AksRequest::Stop).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stopped = 0;
        while Instant::now() < deadline {
            match handle.try_event() {
                Some(AksEvent::Stopped) => {
                    stopped += 1;
                    break;
                }
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
        assert_eq!(stopped, 1);
        assert!(handle.try_event().is_none(), "Stopped is said once");
    }

    #[test]
    fn one_namespace_is_read_per_poll_so_a_request_never_waits_for_a_whole_sweep() {
        let fake = FakeKube::default();
        let (mut watcher, _receiver) = watcher(&fake);
        watcher.handle(AksRequest::Clusters(vec![
            cluster("qa", &["orders", "billing"]),
            cluster("prod", &["orders", "billing"]),
        ]));
        watcher.handle(AksRequest::TabShowing(true));
        let start = Instant::now();
        for expected in 1..=4 {
            assert_eq!(
                watcher.until_due(start),
                Some(Duration::ZERO),
                "read {expected} is due at once"
            );
            watcher.poll(start);
            assert_eq!(
                fake.reads.lock().unwrap().len(),
                expected,
                "one read a poll"
            );
        }
        assert_eq!(watcher.until_due(start), Some(POD_CADENCE));
        watcher.poll(start);
        assert_eq!(
            fake.reads.lock().unwrap().len(),
            4,
            "nothing more until the cadence is up"
        );
    }
}
