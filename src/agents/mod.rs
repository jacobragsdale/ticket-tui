//! `w` on a work item: a coding CLI in a Herdr pane, in the right repository,
//! with the ticket already in front of it. ticket-tui settles the checkout,
//! finds or makes the workspace, the tab and the pane, starts the agent,
//! sends the opening prompt and remembers just enough to come back to it.
//! Herdr owns the terminals; the agent's own CLI owns the conversation.
//!
//! Everything that runs a process happens on a thread of its own, the way
//! the local git thread does, so a slow `agent start` never holds the screen.

pub mod checkout;
pub mod handoff;
pub mod herdr;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub use checkout::{Checkout, CheckoutPolicy};
use herdr::{Herdr, HerdrApi, agent_name_for};

use crate::config::Config;
use crate::local::RepoKey;
use crate::model::TicketKey;

/// The `[agents]` and `[herdr]` tables as the screen reads them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentSettings {
    pub default: Provider,
    pub policy: CheckoutPolicy,
    pub copilot_args: Vec<String>,
    pub cursor_args: Vec<String>,
    pub herdr: crate::config::Herdr,
}

impl AgentSettings {
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self {
            default: config
                .agents
                .default
                .as_deref()
                .and_then(Provider::parse)
                .unwrap_or_default(),
            policy: CheckoutPolicy::parse(config.agents.checkout.as_deref()),
            copilot_args: config.agents.copilot.args.clone(),
            cursor_args: config.agents.cursor.args.clone(),
            herdr: config.herdr.clone(),
        }
    }

    #[must_use]
    pub fn args_for(&self, provider: Provider) -> &[String] {
        match provider {
            Provider::Copilot => &self.copilot_args,
            Provider::Cursor => &self.cursor_args,
        }
    }
}

/// The coding CLIs a launch can start, as Herdr names their kinds.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Copilot,
    #[default]
    Cursor,
}

impl Provider {
    pub const ALL: [Self; 2] = [Self::Copilot, Self::Cursor];

    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "copilot" => Some(Self::Copilot),
            "cursor" => Some(Self::Cursor),
            _ => None,
        }
    }

    /// The `--kind` Herdr takes.
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Copilot => "copilot",
            Self::Cursor => "cursor",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Copilot => "Copilot",
            Self::Cursor => "Cursor",
        }
    }
}

/// One relative of the ticket, as the handoff names it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RelatedBrief {
    pub id: i64,
    pub work_item_type: String,
    pub title: String,
    pub state: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommentBrief {
    pub author: Option<String>,
    pub at: String,
    pub text: String,
}

/// The ticket as the handoff describes it: text, not HTML, and only this
/// work item's own relatives and comments.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TicketBrief {
    pub organization: String,
    pub project: String,
    pub id: i64,
    pub revision: i64,
    pub work_item_type: String,
    pub state: String,
    pub title: String,
    pub url: String,
    pub assigned_to: Option<String>,
    pub tags: Vec<String>,
    pub description: String,
    pub acceptance_criteria: String,
    pub parent: Option<RelatedBrief>,
    pub children: Vec<RelatedBrief>,
    pub comments: Vec<CommentBrief>,
    /// Its artifact links, each already worded.
    pub links: Vec<String>,
}

impl TicketBrief {
    /// The brief off the rows on file: the ticket, its parent and children
    /// by name, its own comments, and its links in words. `relative` names
    /// another work item, `repo_name` a repository GUID, and `pull_request`
    /// a pull request's title and status when the database holds it.
    pub fn from_graph(
        ticket: &crate::model::Ticket,
        graph: &crate::model::TicketGraph,
        relative: &dyn Fn(&TicketKey) -> Option<RelatedBrief>,
        repo_name: &dyn Fn(&str) -> String,
        pull_request: &dyn Fn(i64) -> Option<(String, String)>,
    ) -> Self {
        let key = &ticket.key;
        Self {
            organization: key.organization.clone(),
            project: ticket.project.clone(),
            id: key.id,
            revision: ticket.revision,
            work_item_type: ticket.work_item_type.clone(),
            state: ticket.state.clone(),
            title: ticket.title.clone(),
            url: ticket.web_url.clone(),
            assigned_to: ticket.assigned_to.clone(),
            tags: ticket.tags.clone(),
            description: rich_text(&ticket.description_html, &ticket.description),
            acceptance_criteria: rich_text(
                &ticket.acceptance_criteria_html,
                &ticket.acceptance_criteria,
            ),
            parent: graph.parents_of(key).first().and_then(relative),
            children: graph.children_of(key).iter().filter_map(relative).collect(),
            comments: graph
                .comments_for(key)
                .into_iter()
                .map(|comment| CommentBrief {
                    author: comment.author.clone(),
                    at: comment.created_at.to_rfc3339(),
                    text: comment.text.clone(),
                })
                .collect(),
            links: graph
                .artifacts_for(key)
                .into_iter()
                .map(|link| link_words(link, repo_name, pull_request))
                .collect(),
        }
    }
}

/// A rich-text field as Markdown, from the HTML when there is any and the
/// flattened text otherwise.
fn rich_text(html: &str, text: &str) -> String {
    if html.trim().is_empty() {
        text.to_owned()
    } else {
        crate::markdown::html_to_markdown(html)
    }
}

/// One artifact link in words, with what the database says about its far end.
fn link_words(
    link: &crate::model::ArtifactLink,
    repo_name: &dyn Fn(&str) -> String,
    pull_request: &dyn Fn(i64) -> Option<(String, String)>,
) -> String {
    use crate::model::ArtifactKind;
    match &link.kind {
        ArtifactKind::Branch { repo_id, name } => {
            format!("Branch {name} in {}", repo_name(repo_id))
        }
        ArtifactKind::PullRequest { repo_id, id } => match pull_request(*id) {
            Some((title, status)) => {
                format!(
                    "Pull request !{id} in {} ({status}): {title}",
                    repo_name(repo_id)
                )
            }
            None => format!("Pull request !{id} in {}", repo_name(repo_id)),
        },
        ArtifactKind::Commit { repo_id, sha } => format!(
            "Commit {} in {}",
            sha.get(..8).unwrap_or(sha),
            repo_name(repo_id)
        ),
        ArtifactKind::Build(id) => format!("Build {id}"),
    }
}

/// The repositories a work item's links name, most telling first: a branch
/// pins it to one, a pull request nearly as well, a commit last.
#[must_use]
pub fn linked_repositories(artifacts: &[&crate::model::ArtifactLink]) -> Vec<String> {
    use crate::model::ArtifactKind;
    let mut ids: Vec<String> = Vec::new();
    for wanted in [0, 1, 2] {
        for link in artifacts {
            let repo = match (&link.kind, wanted) {
                (ArtifactKind::Branch { repo_id, .. }, 0)
                | (ArtifactKind::PullRequest { repo_id, .. }, 1)
                | (ArtifactKind::Commit { repo_id, .. }, 2) => repo_id,
                _ => continue,
            };
            if !ids.contains(repo) {
                ids.push(repo.clone());
            }
        }
    }
    ids
}

/// The branch and open pull request a work item's links name in one
/// repository: the branch to work on, and the pull request to carry on with
/// rather than open beside. `pull_request` answers a pull request's status,
/// source ref and URL when the database holds it.
pub fn repository_links(
    artifacts: &[&crate::model::ArtifactLink],
    repo_id: &str,
    pull_request: &dyn Fn(i64) -> Option<(crate::model::PrStatus, String, String)>,
) -> (Option<String>, Option<(i64, String, String)>) {
    use crate::model::ArtifactKind;
    let branch = artifacts.iter().find_map(|link| match &link.kind {
        ArtifactKind::Branch {
            repo_id: held,
            name,
        } if held == repo_id => Some(name.clone()),
        _ => None,
    });
    let open = artifacts
        .iter()
        .filter_map(|link| match &link.kind {
            ArtifactKind::PullRequest { repo_id: held, id } if held == repo_id => Some(*id),
            _ => None,
        })
        .filter_map(|id| pull_request(id).map(|(status, source, url)| (id, status, source, url)))
        .filter(|(_, status, _, _)| !status.is_closed())
        .max_by_key(|(id, ..)| *id)
        .map(|(id, _, source, url)| {
            (
                id,
                source
                    .strip_prefix("refs/heads/")
                    .unwrap_or(&source)
                    .to_owned(),
                url,
            )
        });
    (branch, open)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RepoBrief {
    pub id: String,
    pub name: String,
    pub remote_url: String,
    pub ssh_url: String,
    pub web_url: String,
    pub default_branch: Option<String>,
}

/// How the agent reaches ticket-tui: the flags that address this database
/// and this pair of projects, and the binary itself.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Invocation {
    pub database: PathBuf,
    pub organization: String,
    pub project: String,
    pub code_project: String,
    pub binary: Option<PathBuf>,
}

/// Everything a launch needs, gathered on the main thread from what the
/// screen holds, so the agent thread reads no database.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchPlan {
    pub ticket: TicketBrief,
    pub repo: RepoBrief,
    /// The branch the work item is linked to in this repository, if any.
    pub linked_branch: Option<String>,
    /// An open pull request linked to it in this repository: its id, source
    /// branch and URL.
    pub pull_request: Option<(i64, String, String)>,
    pub provider: Provider,
    pub args: Vec<String>,
    pub policy: CheckoutPolicy,
    /// The Herdr workspace label the repository's tab belongs in.
    pub workspace: String,
    pub workspace_root: Option<PathBuf>,
    pub path_override: Option<PathBuf>,
    pub invocation: Invocation,
    pub handoff_dir: PathBuf,
    /// Start a new session even though one is live for this ticket.
    pub force_new: bool,
    pub note: Option<String>,
}

impl LaunchPlan {
    /// The branch the agent works on: the linked one, the open pull request's,
    /// or `{id}-{slug}` from the title.
    #[must_use]
    pub fn branch(&self) -> String {
        self.linked_branch
            .clone()
            .or_else(|| {
                self.pull_request
                    .as_ref()
                    .map(|(_, source, _)| source.clone())
            })
            .unwrap_or_else(|| {
                crate::app::work_items::branch_name(self.ticket.id, &self.ticket.title)
            })
    }

    fn repo_key(&self) -> RepoKey {
        RepoKey {
            id: self.repo.id.clone(),
            remote: crate::local::normalise_remote(&self.repo.remote_url),
            name: self.repo.name.clone(),
        }
    }
}

/// The steps of a launch, in order; a failure names the one it stopped at.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Checkout,
    Handoff,
    Workspace,
    Tab,
    Pane,
    Agent,
    Prompt,
    Focus,
}

impl Stage {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Checkout => "settling the checkout",
            Self::Handoff => "writing the handoff",
            Self::Workspace => "finding the workspace",
            Self::Tab => "finding the repository tab",
            Self::Pane => "finding a pane",
            Self::Agent => "starting the agent",
            Self::Prompt => "sending the opening prompt",
            Self::Focus => "focusing the agent",
        }
    }
}

/// What is remembered about one launched agent: enough to come back to it,
/// to validate it, and to resume a launch that stopped part way. Ids are the
/// ones Herdr answered with; the labels are for people.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: String,
    pub organization: String,
    pub project: String,
    pub work_item: i64,
    pub repo_id: String,
    pub repo_name: String,
    pub workdir: PathBuf,
    pub branch: String,
    pub policy: CheckoutPolicy,
    pub provider: Provider,
    pub workspace: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub tab_id: Option<String>,
    #[serde(default)]
    pub pane_id: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub context_path: Option<PathBuf>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub prompt_sent: bool,
    pub started_at: String,
}

impl AgentSession {
    #[must_use]
    pub fn ticket_key(&self) -> TicketKey {
        TicketKey {
            organization: self.organization.clone(),
            id: self.work_item,
        }
    }

    /// Whether the launch got all the way: an agent is up and has its prompt.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.agent_name.is_some() && self.prompt_sent
    }

    /// What the pane is labelled: the provider and the ticket, so a tab full
    /// of agents reads `Copilot · #715 | Cursor · #722`.
    #[must_use]
    pub fn pane_label(&self) -> String {
        format!("{} \u{00b7} #{}", self.provider.label(), self.work_item)
    }
}

/// Where the sessions are kept: beside the database, as the session file is.
#[must_use]
pub fn store_path(database: &Path) -> PathBuf {
    let mut file_name = database
        .file_stem()
        .map_or_else(|| "tickets".into(), |stem| stem.to_os_string());
    file_name.push(".agents.json");
    database.with_file_name(file_name)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreDocument {
    schema_version: u8,
    #[serde(default)]
    sessions: Vec<AgentSession>,
}

/// The sessions on file, written after every stage so a launch that stops
/// half way is picked up where it stopped rather than started over.
#[derive(Debug)]
pub struct SessionStore {
    path: PathBuf,
    pub sessions: Vec<AgentSession>,
}

impl SessionStore {
    pub fn load(path: &Path) -> Result<Self> {
        let sessions = match std::fs::read_to_string(path) {
            Ok(raw) => {
                serde_json::from_str::<StoreDocument>(&raw)
                    .with_context(|| format!("failed to parse {}", path.display()))?
                    .sessions
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self {
            path: path.to_path_buf(),
            sessions,
        })
    }

    pub fn save(&self) -> Result<()> {
        let document = StoreDocument {
            schema_version: 1,
            sessions: self.sessions.clone(),
        };
        let raw = serde_json::to_string_pretty(&document)
            .context("failed to serialize agent sessions")?;
        crate::session::write_atomically(&self.path, raw.as_bytes())
            .with_context(|| format!("writing {}", self.path.display()))
    }

    pub fn upsert(&mut self, session: AgentSession) -> Result<()> {
        match self.sessions.iter_mut().find(|held| held.id == session.id) {
            Some(held) => *held = session,
            None => self.sessions.push(session),
        }
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<()> {
        self.sessions.retain(|held| held.id != id);
        self.save()
    }

    /// The unfinished session for this ticket in this repository, which a
    /// launch resumes rather than duplicating what it already made.
    fn pending_for(&self, plan: &LaunchPlan) -> Option<AgentSession> {
        self.sessions
            .iter()
            .find(|held| {
                held.work_item == plan.ticket.id
                    && held.organization == plan.ticket.organization
                    && held.repo_id == plan.repo.id
                    && !held.is_complete()
            })
            .cloned()
    }
}

/// What the agent thread can be asked to do.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentRequest {
    /// Launch an agent for the plan, or carry on a launch of the same ticket
    /// and repository that stopped part way.
    Launch(Box<LaunchPlan>),
    /// Focus the live agent a session names, after checking it is still
    /// there.
    Return(String),
    /// Write the handoff and answer with the opening prompt, for a terminal
    /// the user already has open. Nothing is launched and no worktree is made.
    Prompt(Box<LaunchPlan>),
    /// The labels of the workspaces Herdr has, for the picker.
    Workspaces,
    /// Check every remembered session against Herdr and drop the ones whose
    /// agent is gone.
    Refresh,
    Stop,
}

/// What it answers.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    /// The sessions as they stand, after anything that changed them.
    Sessions(Vec<AgentSession>),
    Launched {
        session: Box<AgentSession>,
        note: String,
    },
    /// A launch stopped at `stage`. `session` is what was made before it did,
    /// which the next launch of the same ticket picks up.
    Failed {
        work_item: i64,
        stage: Stage,
        message: String,
        session: Option<Box<AgentSession>>,
    },
    Returned {
        work_item: i64,
    },
    /// A remembered agent is not there any more, and why not.
    Stale {
        work_item: i64,
        reason: String,
    },
    Prompt {
        work_item: i64,
        text: String,
        context: PathBuf,
    },
    Workspaces(Vec<String>),
    Stopped,
}

/// The handle the main thread holds.
pub struct AgentHandle {
    requests: Sender<AgentRequest>,
    events: Receiver<AgentEvent>,
    stopped: std::cell::Cell<bool>,
}

impl AgentHandle {
    /// Starts the thread over `herdr` and the store at `store`. It ends when
    /// the handle is dropped.
    pub fn spawn(store: PathBuf, herdr: Box<dyn HerdrApi>) -> Result<Self> {
        let (request_sender, request_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("ticket-agents".into())
            .spawn(move || work(&store, Herdr::new(herdr), &request_receiver, &event_sender))
            .context("failed to start the agent launch thread")?;
        Ok(Self {
            requests: request_sender,
            events: event_receiver,
            stopped: std::cell::Cell::new(false),
        })
    }

    pub fn send(&self, request: AgentRequest) -> Result<()> {
        self.requests
            .send(request)
            .context("the agent launch thread stopped")
    }

    pub fn try_event(&self) -> Option<AgentEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                (!self.stopped.replace(true)).then_some(AgentEvent::Stopped)
            }
        }
    }
}

fn work(
    store_path: &Path,
    herdr: Herdr,
    requests: &Receiver<AgentRequest>,
    events: &Sender<AgentEvent>,
) {
    let mut store = match SessionStore::load(store_path) {
        Ok(store) => store,
        Err(error) => {
            let _ = events.send(AgentEvent::Failed {
                work_item: 0,
                stage: Stage::Handoff,
                message: format!("{error:#}"),
                session: None,
            });
            SessionStore {
                path: store_path.to_path_buf(),
                sessions: Vec::new(),
            }
        }
    };
    if events
        .send(AgentEvent::Sessions(store.sessions.clone()))
        .is_err()
    {
        return;
    }
    while let Ok(request) = requests.recv() {
        let sent = match request {
            AgentRequest::Stop => return,
            AgentRequest::Launch(plan) => {
                let outcome = launch(&herdr, &mut store, &plan);
                let event = match outcome {
                    Ok((session, note)) => AgentEvent::Launched {
                        session: Box::new(session),
                        note,
                    },
                    Err(failure) => AgentEvent::Failed {
                        work_item: plan.ticket.id,
                        stage: failure.stage,
                        message: format!("{:#}", failure.error),
                        session: failure.session,
                    },
                };
                events
                    .send(AgentEvent::Sessions(store.sessions.clone()))
                    .and_then(|()| events.send(event))
            }
            AgentRequest::Return(id) => {
                let event = match return_to(&herdr, &mut store, &id) {
                    Ok(work_item) => AgentEvent::Returned { work_item },
                    Err(Returned::Stale { work_item, reason }) => {
                        AgentEvent::Stale { work_item, reason }
                    }
                    Err(Returned::Failed(error)) => AgentEvent::Failed {
                        work_item: 0,
                        stage: Stage::Focus,
                        message: format!("{error:#}"),
                        session: None,
                    },
                };
                events
                    .send(AgentEvent::Sessions(store.sessions.clone()))
                    .and_then(|()| events.send(event))
            }
            AgentRequest::Prompt(plan) => events.send(match prompt_only(&plan) {
                Ok((text, context)) => AgentEvent::Prompt {
                    work_item: plan.ticket.id,
                    text,
                    context,
                },
                Err(failure) => AgentEvent::Failed {
                    work_item: plan.ticket.id,
                    stage: failure.stage,
                    message: format!("{:#}", failure.error),
                    session: None,
                },
            }),
            AgentRequest::Workspaces => events.send(AgentEvent::Workspaces(
                herdr
                    .workspaces()
                    .map(|found| found.into_iter().map(|workspace| workspace.label).collect())
                    .unwrap_or_default(),
            )),
            AgentRequest::Refresh => {
                let mut sent = Ok(());
                for session in store.sessions.clone() {
                    if !session.is_complete() {
                        continue;
                    }
                    if let Ok(Some(reason)) = validate(&herdr, &session) {
                        let _ = store.remove(&session.id);
                        sent = events.send(AgentEvent::Stale {
                            work_item: session.work_item,
                            reason,
                        });
                    }
                }
                sent.and_then(|()| events.send(AgentEvent::Sessions(store.sessions.clone())))
            }
        };
        if sent.is_err() {
            return;
        }
    }
}

/// Why a launch stopped, at which step, and what it had made by then.
#[derive(Debug)]
pub struct LaunchFailure {
    pub stage: Stage,
    pub error: anyhow::Error,
    pub session: Option<Box<AgentSession>>,
}

fn fail(stage: Stage, error: anyhow::Error, session: Option<AgentSession>) -> LaunchFailure {
    LaunchFailure {
        stage,
        error,
        session: session.map(Box::new),
    }
}

/// The checkout a plan settles on. A repository that is not here is cloned
/// first, into the workspace under its own name — where `C` on the Repos tab
/// puts it — and the note says so.
fn settle_checkout(plan: &LaunchPlan, for_launch: bool) -> Result<Checkout> {
    let mut cloned = None;
    let clone = match checkout::find_clone(
        plan.workspace_root.as_deref(),
        plan.path_override.as_deref(),
        &plan.repo_key(),
    )? {
        Some(clone) => clone,
        None => {
            let clone = clone_missing(plan)?;
            cloned = Some(clone.clone());
            clone
        }
    };
    // As git spells it, so the copy case names the same path a launch would.
    let clone = clone.canonicalize().unwrap_or(clone);
    let branch = plan.branch();
    let base = plan
        .repo
        .default_branch
        .clone()
        .unwrap_or_else(|| "main".to_owned());
    let mut checkout = if for_launch {
        let checkout = checkout::settle(
            &clone,
            &plan.repo.name,
            &branch,
            &base,
            plan.policy,
            plan.linked_branch.is_some() || plan.pull_request.is_some(),
        )?;
        // Herdr, handed a directory that is not there, opens the pane in the
        // home directory and says nothing; better refused here.
        anyhow::ensure!(
            checkout.workdir.is_dir(),
            "{} is not a directory; the agent would start in the wrong place",
            checkout.workdir.display()
        );
        checkout
    } else {
        // For a prompt to copy, no worktree is made: the terminal the user
        // has open is wherever it is, so the handoff names the clone.
        Checkout {
            workdir: clone.clone(),
            clone,
            branch,
            policy: CheckoutPolicy::Shared,
            note: "the clone; make a worktree yourself if you want one".to_owned(),
        }
    };
    if let Some(path) = cloned {
        checkout.note = format!("cloned into {}; {}", path.display(), checkout.note);
    }
    Ok(checkout)
}

/// Clones a repository that is not here into the workspace under its own
/// name, and answers the path. A path `[herdr.paths]` names that is not a
/// clone is refused rather than cloned over: it was named on purpose.
fn clone_missing(plan: &LaunchPlan) -> Result<PathBuf> {
    if let Some(path) = &plan.path_override {
        bail!(
            "{} is not a clone of {}; fix or drop its [herdr.paths] entry",
            path.display(),
            plan.repo.name
        );
    }
    let Some(root) = &plan.workspace_root else {
        bail!(
            "{} is not cloned here, and no workspace is configured to clone it into",
            plan.repo.name
        );
    };
    let Some(url) = crate::local::clone_url(&plan.repo.remote_url, &plan.repo.ssh_url) else {
        bail!(
            "{} is not cloned here, and Azure DevOps gave it no clone URL",
            plan.repo.name
        );
    };
    let into = root.join(&plan.repo.name);
    crate::local::clone(&url, &into)
        .with_context(|| format!("cloning {} into {}", plan.repo.name, into.display()))?;
    Ok(into)
}

/// Writes the handoff and answers with the prompt, for the clipboard.
pub fn prompt_only(plan: &LaunchPlan) -> Result<(String, PathBuf), LaunchFailure> {
    let checkout =
        settle_checkout(plan, false).map_err(|error| fail(Stage::Checkout, error, None))?;
    let files = handoff::write(&plan.handoff_dir, plan, &checkout)
        .map_err(|error| fail(Stage::Handoff, error, None))?;
    Ok((
        handoff::opening_prompt(plan, &checkout, &files),
        files.context,
    ))
}

/// The whole launch, one stage at a time, each written to the store before
/// the next begins. A stored session for the same ticket and repository that
/// never finished is carried on: what it already made is checked and kept,
/// and the prompt is sent once.
pub fn launch(
    herdr: &Herdr,
    store: &mut SessionStore,
    plan: &LaunchPlan,
) -> Result<(AgentSession, String), LaunchFailure> {
    let checkout =
        settle_checkout(plan, true).map_err(|error| fail(Stage::Checkout, error, None))?;
    if plan.policy == CheckoutPolicy::Shared
        && let Some(other) = store.sessions.iter().find(|held| {
            held.repo_id == plan.repo.id
                && held.workdir == checkout.workdir
                && held.is_complete()
                && !(held.work_item == plan.ticket.id && !plan.force_new)
        })
    {
        return Err(fail(
            Stage::Checkout,
            anyhow::anyhow!(
                "#{} already has a {} agent in {}; the shared checkout takes one at a time",
                other.work_item,
                other.provider.label(),
                checkout.workdir.display()
            ),
            None,
        ));
    }
    let mut session = store.pending_for(plan).unwrap_or_else(|| AgentSession {
        id: (1..)
            .map(|n| format!("{}-{}-{n}", plan.ticket.organization, plan.ticket.id))
            .find(|candidate| !store.sessions.iter().any(|held| held.id == *candidate))
            .expect("the counter is unbounded"),
        organization: plan.ticket.organization.clone(),
        project: plan.ticket.project.clone(),
        work_item: plan.ticket.id,
        repo_id: plan.repo.id.clone(),
        repo_name: plan.repo.name.clone(),
        workdir: checkout.workdir.clone(),
        branch: checkout.branch.clone(),
        policy: checkout.policy,
        provider: plan.provider,
        workspace: plan.workspace.clone(),
        workspace_id: None,
        tab_id: None,
        pane_id: None,
        agent_name: None,
        context_path: None,
        prompt: None,
        prompt_sent: false,
        started_at: crate::timestamp::Timestamp::now().to_rfc3339(),
    });
    let mut notes = vec![checkout.note.clone()];

    let files = handoff::write(&plan.handoff_dir, plan, &checkout)
        .map_err(|error| fail(Stage::Handoff, error, Some(session.clone())))?;
    session.context_path = Some(files.context.clone());
    if !session.prompt_sent {
        session.prompt = Some(handoff::opening_prompt(plan, &checkout, &files));
    }
    let save = |store: &mut SessionStore, session: &AgentSession, stage: Stage| {
        store
            .upsert(session.clone())
            .map_err(|error| fail(stage, error, Some(session.clone())))
    };
    save(store, &session, Stage::Handoff)?;

    // What was remembered is only trusted once Herdr still knows it, from
    // the workspace down: a tab whose workspace is gone is gone with it.
    revalidate_pending(herdr, &mut session)
        .map_err(|error| fail(Stage::Workspace, error, Some(session.clone())))?;

    if session.workspace_id.is_none() {
        let workdir = checkout.workdir.clone();
        let found = find_workspace(herdr, &plan.workspace)
            .map_err(|error| fail(Stage::Workspace, error, Some(session.clone())))?;
        match found {
            Some(workspace) => session.workspace_id = Some(workspace.id),
            None => {
                let (workspace, tab, pane) = herdr
                    .create_workspace(&plan.workspace, &workdir)
                    .map_err(|error| fail(Stage::Workspace, error, Some(session.clone())))?;
                // A new workspace's first tab is the repository's tab.
                let _ = herdr.rename_tab(&tab.id, &plan.repo.name);
                session.workspace_id = Some(workspace.id);
                session.tab_id = Some(tab.id);
                session.pane_id = Some(pane.id);
                notes.push(format!("made workspace {}", plan.workspace));
            }
        }
        save(store, &session, Stage::Workspace)?;
    }
    let workspace_id = session.workspace_id.clone().expect("settled above");

    if session.tab_id.is_none() {
        let tabs = herdr
            .tabs(&workspace_id)
            .map_err(|error| fail(Stage::Tab, error, Some(session.clone())))?;
        // Another session of the same repository says which tab is its own
        // when two carry the name; else the name decides.
        let known: Vec<&str> = store
            .sessions
            .iter()
            .filter(|held| held.repo_id == plan.repo.id && held.id != session.id)
            .filter_map(|held| held.tab_id.as_deref())
            .collect();
        let tab = tabs
            .iter()
            .find(|tab| known.contains(&tab.id.as_str()))
            .or_else(|| {
                tabs.iter()
                    .find(|tab| tab.label.eq_ignore_ascii_case(&plan.repo.name))
            })
            .cloned();
        match tab {
            Some(tab) => session.tab_id = Some(tab.id),
            None => {
                let (tab, pane) = herdr
                    .create_tab(&workspace_id, &plan.repo.name, &checkout.workdir)
                    .map_err(|error| fail(Stage::Tab, error, Some(session.clone())))?;
                session.tab_id = Some(tab.id);
                session.pane_id = Some(pane.id);
                notes.push(format!("made tab {}", plan.repo.name));
            }
        }
        save(store, &session, Stage::Tab)?;
    }
    let tab_id = session.tab_id.clone().expect("settled above");

    if session.pane_id.is_none() {
        let panes: Vec<herdr::Pane> = herdr
            .panes(&workspace_id)
            .map_err(|error| fail(Stage::Pane, error, Some(session.clone())))?
            .into_iter()
            .filter(|pane| pane.tab_id == tab_id)
            .collect();
        let mut free = None;
        for pane in &panes {
            if herdr
                .pane_free_in(pane, &checkout.workdir)
                .map_err(|error| fail(Stage::Pane, error, Some(session.clone())))?
            {
                free = Some(pane.id.clone());
                break;
            }
        }
        match (free, panes.last()) {
            (Some(pane), _) => {
                session.pane_id = Some(pane);
                notes.push("in a free pane".to_owned());
            }
            (None, Some(last)) => {
                let pane = herdr
                    .split_right(&last.id, &checkout.workdir)
                    .map_err(|error| fail(Stage::Pane, error, Some(session.clone())))?;
                session.pane_id = Some(pane.id);
                notes.push("in a new pane to the right".to_owned());
            }
            (None, None) => {
                return Err(fail(
                    Stage::Pane,
                    anyhow::anyhow!("tab {tab_id} has no panes"),
                    Some(session.clone()),
                ));
            }
        }
        save(store, &session, Stage::Pane)?;
    }
    let pane_id = session.pane_id.clone().expect("settled above");

    // On screen now, before the CLI is even started: the tab is what the
    // user is waiting for, and an agent takes seconds to come up.
    if let Some(tab_id) = &session.tab_id {
        let _ = herdr.focus_workspace(&workspace_id);
        let _ = herdr.focus_tab(tab_id);
    }

    if session.agent_name.is_none() {
        let name = herdr
            .free_agent_name(&agent_name_for(plan.ticket.id))
            .map_err(|error| fail(Stage::Agent, error, Some(session.clone())))?;
        // Herdr holds the name from the moment the start is asked for, even
        // when the CLI is slow to come ready. Remembered first, a retry finds
        // the agent that did start rather than starting another beside it.
        session.agent_name = Some(name.clone());
        save(store, &session, Stage::Agent)?;
        herdr
            .start_agent(&name, plan.provider.kind(), &pane_id, &plan.args)
            .map_err(|error| fail(Stage::Agent, error, Some(session.clone())))?;
        let _ = herdr.rename_pane(&pane_id, &session.pane_label());
    }
    let agent_name = session.agent_name.clone().expect("settled above");

    if !session.prompt_sent {
        let prompt = session.prompt.clone().unwrap_or_default();
        let unconfirmed = herdr
            .prompt(&agent_name, &prompt)
            .map_err(|error| fail(Stage::Prompt, error, Some(session.clone())))?;
        session.prompt_sent = true;
        save(store, &session, Stage::Prompt)?;
        notes.extend(unconfirmed);
    }

    // The tab is already showing; this lands the keyboard on the agent.
    if let Err(error) = herdr.focus_agent(&agent_name) {
        notes.push(format!("could not focus it: {error:#}"));
    }
    Ok((session, notes.join("; ")))
}

/// Puts the agent on screen: its workspace, then its tab, then the pane.
/// `agent focus` alone lands the pane's focus without switching what the
/// client is showing, so the two above it are asked for explicitly.
fn bring_to_front(herdr: &Herdr, session: &AgentSession) -> Result<()> {
    if let Some(workspace_id) = &session.workspace_id {
        herdr.focus_workspace(workspace_id)?;
    }
    if let Some(tab_id) = &session.tab_id {
        herdr.focus_tab(tab_id)?;
    }
    let name = session.agent_name.as_deref().unwrap_or_default();
    herdr.focus_agent(name)
}

/// The live workspace with this label, or nothing; two of them is a
/// question for the user rather than a coin toss.
fn find_workspace(herdr: &Herdr, label: &str) -> Result<Option<herdr::Workspace>> {
    let matching: Vec<herdr::Workspace> = herdr
        .workspaces()?
        .into_iter()
        .filter(|workspace| workspace.label.eq_ignore_ascii_case(label))
        .collect();
    match matching.len() {
        0 => Ok(None),
        1 => Ok(matching.into_iter().next()),
        _ => bail!("two Herdr workspaces are labelled {label}; rename one of them"),
    }
}

/// Drops the ids of a half-made session that Herdr no longer knows, from
/// the outside in, so the launch remakes only what is gone. A pane that has
/// since been taken by something else is dropped too.
fn revalidate_pending(herdr: &Herdr, session: &mut AgentSession) -> Result<()> {
    if let Some(workspace_id) = session.workspace_id.clone()
        && herdr.workspace(&workspace_id)?.is_none()
    {
        session.workspace_id = None;
        session.tab_id = None;
        session.pane_id = None;
        session.agent_name = None;
    }
    if let Some(tab_id) = session.tab_id.clone()
        && herdr
            .tab(&tab_id)?
            .is_none_or(|tab| Some(&tab.workspace_id) != session.workspace_id.as_ref())
    {
        session.tab_id = None;
        session.pane_id = None;
        session.agent_name = None;
    }
    if let Some(pane_id) = session.pane_id.clone() {
        let pane = herdr.pane(&pane_id)?;
        let usable = match (&pane, &session.agent_name) {
            (Some(pane), Some(name)) => herdr
                .agent(&pane.id)?
                .is_some_and(|agent| agent.name.as_deref().is_none_or(|held| held == name)),
            (Some(pane), None) => {
                Some(&pane.tab_id) == session.tab_id.as_ref()
                    && herdr.pane_free_in(pane, &session.workdir)?
            }
            (None, _) => false,
        };
        if !usable {
            session.pane_id = None;
            session.agent_name = None;
        }
    }
    Ok(())
}

/// Whether a complete session's agent is still where it was: `None` while it
/// is, else why it is not.
pub fn validate(herdr: &Herdr, session: &AgentSession) -> Result<Option<String>> {
    let (Some(pane_id), Some(name)) = (&session.pane_id, &session.agent_name) else {
        return Ok(Some("the launch never finished".to_owned()));
    };
    let Some(agent) = herdr.agent(pane_id)? else {
        return Ok(Some(format!("no agent is in pane {pane_id} any more")));
    };
    if agent.kind != session.provider.kind() {
        return Ok(Some(format!(
            "pane {pane_id} now hosts {} rather than {}",
            agent.kind,
            session.provider.label()
        )));
    }
    if agent.name.as_deref().is_some_and(|held| held != name) {
        return Ok(Some(format!(
            "pane {pane_id} now hosts the agent {} rather than {name}",
            agent.name.unwrap_or_default()
        )));
    }
    Ok(None)
}

/// Why a return did not happen: the agent is gone, or Herdr did not answer.
#[derive(Debug)]
pub enum Returned {
    Stale { work_item: i64, reason: String },
    Failed(anyhow::Error),
}

/// Focuses a remembered agent once Herdr has confirmed it is the one that
/// was launched. One that is gone is forgotten and said to be.
pub fn return_to(herdr: &Herdr, store: &mut SessionStore, id: &str) -> Result<i64, Returned> {
    let Some(session) = store.sessions.iter().find(|held| held.id == id).cloned() else {
        return Err(Returned::Failed(anyhow::anyhow!(
            "that agent session is no longer on file"
        )));
    };
    match validate(herdr, &session) {
        Ok(None) => {}
        Ok(Some(reason)) => {
            let _ = store.remove(id);
            return Err(Returned::Stale {
                work_item: session.work_item,
                reason,
            });
        }
        Err(error) => return Err(Returned::Failed(error)),
    }
    bring_to_front(herdr, &session).map_err(Returned::Failed)?;
    Ok(session.work_item)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::tempdir;

    use super::herdr::fake::{FakeHerdr, FakeState};
    use super::*;

    fn run(path: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(arguments)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {arguments:?}");
    }

    /// A clone of `name` under `root/work`, with an Azure DevOps origin.
    fn clone_under(root: &Path, name: &str) -> PathBuf {
        let bare = root.join(format!("{name}.git"));
        let seed = root.join(format!("{name}-seed"));
        std::fs::create_dir_all(&seed).unwrap();
        run(&seed, &["init", "--initial-branch=main"]);
        run(&seed, &["config", "user.email", "t@example.com"]);
        run(&seed, &["config", "user.name", "T"]);
        std::fs::write(seed.join("README.md"), "one\n").unwrap();
        run(&seed, &["add", "."]);
        run(&seed, &["commit", "-m", "first"]);
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
        run(
            &clone,
            &[
                "remote",
                "set-url",
                "origin",
                &format!("https://dev.azure.com/demo/atlas/_git/{name}"),
            ],
        );
        clone
    }

    fn plan_in(root: &Path, id: i64, repo: &str) -> LaunchPlan {
        let mut plan = handoff::tests::plan();
        plan.ticket.id = id;
        plan.ticket.organization = "demo".into();
        plan.ticket.title = format!("Ticket {id}");
        plan.repo = RepoBrief {
            id: format!("id-{repo}"),
            name: repo.into(),
            remote_url: format!("https://dev.azure.com/demo/atlas/_git/{repo}"),
            ssh_url: String::new(),
            web_url: String::new(),
            default_branch: Some("refs/heads/main".into()),
        };
        plan.linked_branch = None;
        plan.pull_request = None;
        plan.workspace = "Payments".into();
        plan.workspace_root = Some(root.join("work"));
        plan.path_override = None;
        plan.handoff_dir = root.join("handoffs");
        plan.note = None;
        plan
    }

    fn store_in(root: &Path) -> SessionStore {
        SessionStore::load(&root.join("tickets.agents.json")).unwrap()
    }

    fn session_for(id: &str, work_item: i64) -> AgentSession {
        AgentSession {
            id: id.into(),
            organization: "demo".into(),
            project: "atlas".into(),
            work_item,
            repo_id: "id-pay".into(),
            repo_name: "pay".into(),
            workdir: PathBuf::from("/work/pay"),
            branch: format!("{work_item}-ticket"),
            policy: CheckoutPolicy::Worktree,
            provider: Provider::Cursor,
            workspace: "Payments".into(),
            workspace_id: None,
            tab_id: None,
            pane_id: None,
            agent_name: None,
            context_path: None,
            prompt: None,
            prompt_sent: false,
            started_at: "2026-09-07T09:00:00Z".into(),
        }
    }

    #[test]
    fn the_store_is_rewritten_whole_and_a_write_that_cannot_finish_leaves_the_last_one() {
        let dir = tempdir().unwrap();
        let mut store = store_in(dir.path());
        store.upsert(session_for("s1", 715)).unwrap();
        store.upsert(session_for("s2", 722)).unwrap();
        store.remove("s1").unwrap();
        let reloaded = store_in(dir.path());
        assert_eq!(reloaded.sessions, [session_for("s2", 722)]);
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["tickets.agents.json"], "no temporary file is left");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let writable = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
            if std::fs::File::create(dir.path().join("probe")).is_ok() {
                // root writes anywhere, so there is no failure to stage.
                std::fs::set_permissions(dir.path(), writable).unwrap();
                return;
            }
            let error = store.upsert(session_for("s3", 730)).unwrap_err();
            std::fs::set_permissions(dir.path(), writable).unwrap();
            assert!(
                format!("{error:#}").contains("tickets.agents.json"),
                "{error:#}"
            );
            assert_eq!(
                store_in(dir.path()).sessions,
                [session_for("s2", 722)],
                "the file holds the last store that was written whole"
            );
        }
    }

    #[test]
    fn a_first_launch_makes_the_workspace_the_tab_and_the_agent_lazily() {
        let dir = tempdir().unwrap();
        clone_under(dir.path(), "pay");
        let fake = FakeHerdr::default();
        let herdr = Herdr::new(Box::new(fake.clone()));
        let mut store = store_in(dir.path());
        let plan = plan_in(dir.path(), 715, "pay");

        let (session, note) = launch(&herdr, &mut store, &plan).unwrap();
        assert!(session.is_complete());
        assert_eq!(session.workspace, "Payments");
        assert_eq!(session.agent_name.as_deref(), Some("wi-715"));
        assert_eq!(
            session.workdir,
            dir.path()
                .canonicalize()
                .unwrap()
                .join("work/.worktrees/pay/715-ticket-715")
        );
        assert!(note.contains("made workspace Payments"), "{note}");
        assert!(
            note.contains("made tab pay") || note.contains("worktree added"),
            "{note}"
        );

        let state = fake.state();
        assert_eq!(state.workspaces, [("w1".to_owned(), "Payments".to_owned())]);
        assert_eq!(
            state.tabs.len(),
            1,
            "the new workspace's first tab is the repository's"
        );
        assert_eq!(state.tabs[0].2, "pay");
        let pane = &state.panes[0];
        assert_eq!(pane.agent.as_deref(), Some("copilot"));
        assert_eq!(pane.label.as_deref(), Some("Copilot \u{00b7} #715"));
        assert_eq!(pane.cwd, session.workdir.to_string_lossy());
        assert_eq!(pane.prompts.len(), 1);
        assert!(pane.prompts[0].starts_with("Work item #715 in demo/development"));
        assert!(pane.prompts[0].contains("SKILL.md"));
        assert_eq!(state.focused.as_deref(), Some(pane.id.as_str()));
        // Persisted with Herdr's own ids.
        let reloaded = store_in(dir.path());
        assert_eq!(reloaded.sessions, std::slice::from_ref(&session));
        assert_eq!(
            reloaded.sessions[0].pane_id.as_deref(),
            Some(pane.id.as_str())
        );
        assert!(
            session
                .context_path
                .as_ref()
                .unwrap()
                .starts_with(dir.path().join("handoffs")),
            "the handoff is outside the repository"
        );
        assert!(
            !dir.path().join("work/pay/context.md").exists()
                && !session.workdir.join("context.md").exists()
        );
        // Nothing was pushed: origin has only main.
        let refs = Command::new("git")
            .args(["--git-dir"])
            .arg(dir.path().join("pay.git"))
            .args(["for-each-ref", "--format=%(refname)"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&refs.stdout).trim(),
            "refs/heads/main"
        );
    }

    #[test]
    fn a_second_ticket_in_the_same_repository_splits_to_the_right_and_a_free_pane_is_reused() {
        let dir = tempdir().unwrap();
        let clone = clone_under(dir.path(), "pay").canonicalize().unwrap();
        let fake = FakeHerdr::default();
        let herdr = Herdr::new(Box::new(fake.clone()));
        let mut store = store_in(dir.path());
        launch(&herdr, &mut store, &plan_in(dir.path(), 715, "pay")).unwrap();
        let mut second = plan_in(dir.path(), 722, "pay");
        second.provider = Provider::Cursor;
        let (session, note) = launch(&herdr, &mut store, &second).unwrap();
        assert!(note.contains("in a new pane to the right"), "{note}");
        let state = fake.state();
        assert_eq!(state.workspaces.len(), 1, "no second workspace");
        assert_eq!(state.tabs.len(), 1, "no second tab");
        assert_eq!(state.panes.len(), 2);
        assert_eq!(state.panes[1].tab_id, state.tabs[0].0);
        assert_eq!(state.panes[1].agent.as_deref(), Some("cursor"));
        assert_eq!(
            state.panes[1].label.as_deref(),
            Some("Cursor \u{00b7} #722")
        );
        assert_ne!(
            session.workdir, store.sessions[0].workdir,
            "each ticket its own worktree"
        );
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.starts_with("pane split") && call.contains("--direction right"))
        );

        // The user opens a plain shell pane in that tab, somewhere else: it
        // is left alone, and the launch splits again.
        let tab = state.tabs[0].0.clone();
        let elsewhere = fake.state.lock().unwrap().add_pane(&tab, "/anywhere", None);
        let (third, note) = launch(&herdr, &mut store, &plan_in(dir.path(), 730, "pay")).unwrap();
        assert_ne!(third.pane_id.as_deref(), Some(elsewhere.as_str()));
        assert!(note.contains("in a new pane to the right"), "{note}");

        // A shell pane already in the ticket's worktree is taken.
        let worktree_of = |id: i64| {
            checkout::worktree_path(
                &clone,
                "pay",
                &crate::app::work_items::branch_name(id, &format!("Ticket {id}")),
            )
        };
        let free =
            fake.state
                .lock()
                .unwrap()
                .add_pane(&tab, &worktree_of(731).to_string_lossy(), None);
        let (fourth, note) = launch(&herdr, &mut store, &plan_in(dir.path(), 731, "pay")).unwrap();
        assert_eq!(fourth.pane_id.as_deref(), Some(free.as_str()));
        assert!(note.contains("in a free pane"), "{note}");

        // A pane with a process in the foreground is not free, wherever it is.
        let busy =
            fake.state
                .lock()
                .unwrap()
                .add_pane(&tab, &worktree_of(732).to_string_lossy(), None);
        fake.state
            .lock()
            .unwrap()
            .panes
            .iter_mut()
            .find(|pane| pane.id == busy)
            .unwrap()
            .busy = true;
        let (fifth, _) = launch(&herdr, &mut store, &plan_in(dir.path(), 732, "pay")).unwrap();
        assert_ne!(fifth.pane_id.as_deref(), Some(busy.as_str()));
    }

    #[test]
    fn a_second_repository_in_the_same_workspace_gets_its_own_tab() {
        let dir = tempdir().unwrap();
        clone_under(dir.path(), "pay");
        clone_under(dir.path(), "settle");
        let fake = FakeHerdr::default();
        let herdr = Herdr::new(Box::new(fake.clone()));
        let mut store = store_in(dir.path());
        launch(&herdr, &mut store, &plan_in(dir.path(), 715, "pay")).unwrap();
        launch(&herdr, &mut store, &plan_in(dir.path(), 716, "settle")).unwrap();
        let state = fake.state();
        assert_eq!(state.workspaces.len(), 1);
        let labels: Vec<&str> = state
            .tabs
            .iter()
            .map(|(_, _, label)| label.as_str())
            .collect();
        assert_eq!(labels, ["pay", "settle"]);
    }

    #[test]
    fn an_existing_workspace_and_tab_are_found_by_label_and_a_duplicate_label_is_refused() {
        let dir = tempdir().unwrap();
        let clone = clone_under(dir.path(), "pay").canonicalize().unwrap();
        let mut state = FakeState::default();
        let other = state.add_workspace("Other");
        state.add_tab(&other, "1");
        let payments = state.add_workspace("payments");
        let tab = state.add_tab(&payments, "Pay");
        // A shell already in the ticket's worktree, as one left by an agent
        // that exited would be.
        let worktree = checkout::worktree_path(
            &clone,
            "pay",
            &crate::app::work_items::branch_name(715, "Ticket 715"),
        );
        let root = state.add_pane(&tab, &worktree.to_string_lossy(), None);
        let fake = FakeHerdr::with(state);
        let herdr = Herdr::new(Box::new(fake.clone()));
        let mut store = store_in(dir.path());
        let (session, _) = launch(&herdr, &mut store, &plan_in(dir.path(), 715, "pay")).unwrap();
        assert_eq!(session.workspace_id.as_deref(), Some(payments.as_str()));
        assert_eq!(session.tab_id.as_deref(), Some(tab.as_str()));
        assert_eq!(session.pane_id.as_deref(), Some(root.as_str()));
        assert!(
            !fake
                .calls()
                .iter()
                .any(|call| call.starts_with("workspace create") || call.starts_with("tab create"))
        );

        fake.state.lock().unwrap().add_workspace("Payments");
        let failure = launch(&herdr, &mut store, &plan_in(dir.path(), 716, "pay")).unwrap_err();
        assert_eq!(failure.stage, Stage::Workspace);
        assert!(
            format!("{:#}", failure.error).contains("two Herdr workspaces are labelled Payments")
        );
    }

    #[test]
    fn a_launch_that_stops_part_way_is_resumed_without_duplicates_or_a_second_prompt() {
        let dir = tempdir().unwrap();
        clone_under(dir.path(), "pay");
        let fake = FakeHerdr::default();
        let herdr = Herdr::new(Box::new(fake.clone()));
        let mut store = store_in(dir.path());
        let plan = plan_in(dir.path(), 715, "pay");

        // The agent will not start.
        fake.fail("agent start", "agent_not_ready", "copilot never came up");
        fake.fail("agent wait", "timeout", "still not ready");
        let failure = launch(&herdr, &mut store, &plan).unwrap_err();
        assert_eq!(failure.stage, Stage::Agent);
        let partial = *failure.session.expect("what was made is kept");
        assert!(
            partial.workspace_id.is_some() && partial.tab_id.is_some() && partial.pane_id.is_some()
        );
        assert_eq!(
            partial.agent_name.as_deref(),
            Some("wi-715"),
            "the name Herdr was asked for is remembered"
        );
        assert_eq!(
            store_in(dir.path()).sessions,
            std::slice::from_ref(&partial)
        );

        // Second try: same workspace, tab and pane; the agent starts; the
        // prompt then fails.
        fake.clear_failure();
        fake.fail("agent prompt", "agent_blocked", "an approval is up");
        let failure = launch(&herdr, &mut store, &plan).unwrap_err();
        assert_eq!(failure.stage, Stage::Prompt);
        let partial = *failure.session.unwrap();
        assert_eq!(partial.agent_name.as_deref(), Some("wi-715"));
        assert!(!partial.prompt_sent);
        let state = fake.state();
        assert_eq!(
            (state.workspaces.len(), state.tabs.len(), state.panes.len()),
            (1, 1, 1)
        );

        // Third try: only the prompt is sent, once.
        fake.clear_failure();
        let (session, _) = launch(&herdr, &mut store, &plan).unwrap();
        assert!(session.is_complete());
        let state = fake.state();
        assert_eq!(
            (state.workspaces.len(), state.tabs.len(), state.panes.len()),
            (1, 1, 1)
        );
        assert_eq!(state.panes[0].prompts.len(), 1);
        assert_eq!(store.sessions.len(), 1);
        assert_eq!(
            fake.calls()
                .iter()
                .filter(|call| call.starts_with("agent start"))
                .count(),
            2,
            "one refused start, one that took"
        );

        // A fourth launch of the same ticket, forced, is a new session beside
        // the first rather than a resend into it.
        let mut again = plan.clone();
        again.force_new = true;
        let (second, _) = launch(&herdr, &mut store, &again).unwrap();
        assert_ne!(second.id, session.id);
        assert_eq!(second.agent_name.as_deref(), Some("wi-715-2"));
        assert_eq!(fake.state().panes.len(), 2);
    }

    #[test]
    fn an_agent_slow_to_come_ready_is_kept_rather_than_started_twice() {
        let dir = tempdir().unwrap();
        clone_under(dir.path(), "pay");
        let fake = FakeHerdr::default();
        let herdr = Herdr::new(Box::new(fake.clone()));
        let mut store = store_in(dir.path());
        let plan = plan_in(dir.path(), 715, "pay");
        // The CLI comes up stopped at a dialog of its own — a trust question,
        // a login. That needs a person, so it is said at once rather than
        // waited out; Herdr holds the name all the same.
        fake.state.lock().unwrap().start_blocked = true;
        let failure = launch(&herdr, &mut store, &plan).unwrap_err();
        assert_eq!(failure.stage, Stage::Agent);
        let message = format!("{:#}", failure.error);
        assert!(
            message.contains("waiting for an answer in its pane"),
            "{message}"
        );
        assert_eq!(
            failure.session.unwrap().agent_name.as_deref(),
            Some("wi-715")
        );

        // The user answers it. The retry prompts that agent: no second
        // start, no second pane.
        {
            let mut state = fake.state.lock().unwrap();
            state.start_blocked = false;
            for pane in &mut state.panes {
                pane.blocked = false;
            }
        }
        let (session, _) = launch(&herdr, &mut store, &plan).unwrap();
        assert!(session.is_complete());
        assert_eq!(session.agent_name.as_deref(), Some("wi-715"));
        let state = fake.state();
        assert_eq!(state.panes.len(), 1);
        assert_eq!(state.panes[0].prompts.len(), 1);
        assert_eq!(
            fake.calls()
                .iter()
                .filter(|call| call.starts_with("agent start"))
                .count(),
            1
        );
    }

    #[test]
    fn a_start_that_fails_says_what_the_pane_shows() {
        let dir = tempdir().unwrap();
        clone_under(dir.path(), "pay");
        let fake = FakeHerdr::default();
        let herdr = Herdr::new(Box::new(fake.clone()));
        let mut store = store_in(dir.path());
        fake.fail(
            "agent start",
            "timeout",
            "timed out waiting for agent startup",
        );
        fake.state.lock().unwrap().screen =
            "❯ cursor-agent --model auto\nzsh: command not found: cursor-agent\n\n❯ ".into();
        let failure = launch(&herdr, &mut store, &plan_in(dir.path(), 715, "pay")).unwrap_err();
        assert_eq!(failure.stage, Stage::Agent);
        assert!(
            failure.session.unwrap().pane_id.is_some(),
            "the pane stands"
        );
        let message = format!("{:#}", failure.error);
        assert!(message.contains("the pane shows: "), "{message}");
        assert!(
            message.contains("zsh: command not found: cursor-agent"),
            "{message}"
        );
        assert!(
            message.contains("timed out waiting for agent startup"),
            "{message}"
        );
    }

    #[test]
    fn a_prompt_the_agent_did_not_take_is_said_and_not_sent_again() {
        let dir = tempdir().unwrap();
        clone_under(dir.path(), "pay");
        let fake = FakeHerdr::default();
        let herdr = Herdr::new(Box::new(fake.clone()));
        let mut store = store_in(dir.path());
        fake.state.lock().unwrap().prompt_stalls = true;
        fake.fail("agent wait", "timeout", "still idle");
        let (session, note) = launch(&herdr, &mut store, &plan_in(dir.path(), 715, "pay")).unwrap();
        assert!(session.is_complete(), "the text is in the box: not resent");
        assert!(note.contains("press Enter in the pane"), "{note}");
        assert_eq!(fake.state().panes[0].prompts.len(), 1);
    }

    #[test]
    fn a_half_made_session_whose_tab_was_closed_remakes_only_the_tab() {
        let dir = tempdir().unwrap();
        clone_under(dir.path(), "pay");
        let fake = FakeHerdr::default();
        let herdr = Herdr::new(Box::new(fake.clone()));
        let mut store = store_in(dir.path());
        let plan = plan_in(dir.path(), 715, "pay");
        fake.fail("agent start", "pane_busy", "not at a prompt");
        let failure = launch(&herdr, &mut store, &plan).unwrap_err();
        let partial = *failure.session.unwrap();
        // The tab goes away between tries.
        {
            let mut state = fake.state.lock().unwrap();
            let tab = partial.tab_id.clone().unwrap();
            state.tabs.retain(|(id, _, _)| *id != tab);
            state.panes.retain(|pane| pane.tab_id != tab);
        }
        fake.clear_failure();
        let (session, note) = launch(&herdr, &mut store, &plan).unwrap();
        assert_eq!(
            session.workspace_id, partial.workspace_id,
            "the workspace stood"
        );
        assert_ne!(session.tab_id, partial.tab_id);
        assert!(note.contains("made tab pay"), "{note}");
        assert_eq!(fake.state().workspaces.len(), 1);
    }

    #[test]
    fn returning_focuses_a_live_agent_and_says_so_when_it_is_gone() {
        let dir = tempdir().unwrap();
        clone_under(dir.path(), "pay");
        let fake = FakeHerdr::default();
        let herdr = Herdr::new(Box::new(fake.clone()));
        let mut store = store_in(dir.path());
        let (session, _) = launch(&herdr, &mut store, &plan_in(dir.path(), 715, "pay")).unwrap();
        fake.state.lock().unwrap().focused = None;
        assert_eq!(validate(&herdr, &session).unwrap(), None);
        assert!(matches!(
            return_to(&herdr, &mut store, &session.id),
            Ok(715)
        ));
        assert_eq!(fake.state().focused, session.pane_id);

        // The agent exits: the pane is a shell again.
        {
            let mut state = fake.state.lock().unwrap();
            let pane = state
                .panes
                .iter_mut()
                .find(|pane| Some(&pane.id) == session.pane_id.as_ref())
                .unwrap();
            pane.agent = None;
            pane.agent_name = None;
        }
        assert_eq!(
            validate(&herdr, &session).unwrap().as_deref(),
            Some(
                format!(
                    "no agent is in pane {} any more",
                    session.pane_id.as_deref().unwrap()
                )
                .as_str()
            )
        );
        match return_to(&herdr, &mut store, &session.id) {
            Err(Returned::Stale { work_item, reason }) => {
                assert_eq!(work_item, 715);
                assert!(reason.contains("no agent is in pane"), "{reason}");
            }
            _ => panic!("a gone agent is stale"),
        }
        assert!(store.sessions.is_empty(), "and forgotten");

        // A pane taken over by another kind of agent is not ours either.
        let (session, _) = launch(&herdr, &mut store, &plan_in(dir.path(), 716, "pay")).unwrap();
        {
            let mut state = fake.state.lock().unwrap();
            let pane = state
                .panes
                .iter_mut()
                .find(|pane| Some(&pane.id) == session.pane_id.as_ref())
                .unwrap();
            pane.agent = Some("claude".into());
        }
        assert!(
            validate(&herdr, &session)
                .unwrap()
                .unwrap()
                .contains("now hosts claude")
        );

        // A Herdr that cannot be reached is an error, not a stale session.
        fake.state.lock().unwrap().down = true;
        assert!(validate(&herdr, &session).is_err());
        assert!(matches!(
            return_to(&herdr, &mut store, &session.id),
            Err(Returned::Failed(_))
        ));
        assert_eq!(
            store.sessions.len(),
            1,
            "nothing was forgotten on a Herdr that did not answer"
        );
    }

    #[test]
    fn a_missing_clone_is_cloned_first_and_a_shared_checkout_in_use_is_refused() {
        let dir = tempdir().unwrap();
        let fake = FakeHerdr::default();
        let herdr = Herdr::new(Box::new(fake.clone()));
        let mut store = store_in(dir.path());

        // No clone and nothing to clone from: refused before Herdr is touched.
        let mut plan = plan_in(dir.path(), 715, "pay");
        plan.repo.remote_url = String::new();
        let failure = launch(&herdr, &mut store, &plan).unwrap_err();
        assert_eq!(failure.stage, Stage::Checkout);
        let message = format!("{:#}", failure.error);
        assert!(
            message.contains("pay is not cloned here") && message.contains("no clone URL"),
            "{message}"
        );
        assert!(fake.calls().is_empty());
        assert!(store.sessions.is_empty());

        // With a URL, the launch clones it into the workspace under its own
        // name — where C on the Repos tab puts it — and carries on.
        let clone = clone_under(dir.path(), "pay");
        std::fs::remove_dir_all(&clone).unwrap();
        let mut plan = plan_in(dir.path(), 715, "pay");
        plan.repo.remote_url = dir.path().join("pay.git").to_string_lossy().into_owned();
        let (session, note) = launch(&herdr, &mut store, &plan).unwrap();
        assert!(
            note.starts_with(&format!("cloned into {}; worktree added", clone.display())),
            "{note}"
        );
        assert!(clone.join(".git").exists());
        assert!(session.workdir.join("README.md").exists());
        assert_eq!(fake.state().panes[0].prompts.len(), 1);
        // As the real thing would be: its origin is the repository.
        run(
            &clone,
            &[
                "remote",
                "set-url",
                "origin",
                "https://dev.azure.com/demo/atlas/_git/pay",
            ],
        );

        let mut shared = plan_in(dir.path(), 715, "pay");
        shared.policy = CheckoutPolicy::Shared;
        let (session, _) = launch(&herdr, &mut store, &shared).unwrap();
        assert_eq!(session.workdir, clone.canonicalize().unwrap());
        assert_eq!(session.branch, "main");
        let mut other = plan_in(dir.path(), 716, "pay");
        other.policy = CheckoutPolicy::Shared;
        let failure = launch(&herdr, &mut store, &other).unwrap_err();
        assert_eq!(failure.stage, Stage::Checkout);
        assert!(format!("{:#}", failure.error).contains("#715 already has a Copilot agent"));
    }

    #[test]
    fn a_prompt_to_copy_writes_the_handoff_and_makes_no_worktree() {
        let dir = tempdir().unwrap();
        let clone = clone_under(dir.path(), "pay");
        let plan = plan_in(dir.path(), 715, "pay");
        let (text, context) = prompt_only(&plan).unwrap();
        assert!(context.exists());
        assert!(
            text.contains(&format!(
                "checked out at {}",
                clone.canonicalize().unwrap().display()
            )),
            "{text}"
        );
        assert!(text.contains("shared"), "{text}");
        assert!(
            !dir.path().join("work/.worktrees").exists(),
            "no worktree for a copy"
        );

        // Without the clone, the copy clones too: the handoff has to name a
        // path that is there.
        std::fs::remove_dir_all(&clone).unwrap();
        let mut plan = plan_in(dir.path(), 716, "pay");
        plan.repo.remote_url = dir.path().join("pay.git").to_string_lossy().into_owned();
        let (text, _) = prompt_only(&plan).unwrap();
        assert!(clone.join(".git").exists(), "cloned for the copy");
        assert!(
            text.contains(&format!(
                "checked out at {}",
                clone.canonicalize().unwrap().display()
            )),
            "{text}"
        );
        assert!(!dir.path().join("work/.worktrees").exists());
    }

    #[test]
    fn the_thread_answers_over_its_channel() {
        let dir = tempdir().unwrap();
        clone_under(dir.path(), "pay");
        let fake = FakeHerdr::default();
        let handle =
            AgentHandle::spawn(dir.path().join("tickets.agents.json"), Box::new(fake)).unwrap();
        let wait = |handle: &AgentHandle| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                if let Some(event) = handle.try_event() {
                    return event;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the agent thread timed out"
                );
                thread::yield_now();
            }
        };
        assert!(matches!(wait(&handle), AgentEvent::Sessions(sessions) if sessions.is_empty()));
        handle.send(AgentRequest::Workspaces).unwrap();
        assert!(matches!(wait(&handle), AgentEvent::Workspaces(names) if names.is_empty()));
        handle
            .send(AgentRequest::Launch(Box::new(plan_in(
                dir.path(),
                715,
                "pay",
            ))))
            .unwrap();
        assert!(matches!(wait(&handle), AgentEvent::Sessions(sessions) if sessions.len() == 1));
        assert!(
            matches!(wait(&handle), AgentEvent::Launched { session, .. } if session.work_item == 715)
        );
        handle.send(AgentRequest::Refresh).unwrap();
        assert!(matches!(wait(&handle), AgentEvent::Sessions(sessions) if sessions.len() == 1));
        handle.send(AgentRequest::Stop).unwrap();
        assert!(matches!(wait(&handle), AgentEvent::Stopped));
    }
}
