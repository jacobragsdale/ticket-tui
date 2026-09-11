//! Herdr, driven through its own CLI. Every call is one `herdr` process:
//! the arguments go out as argv — never through a shell — and the answer is
//! the JSON it prints. The commands this uses were read off `herdr 0.9.0`
//! (`herdr --skill` and the bare command groups; `herdr <cmd> --help` prints
//! only the top page); nothing here is guessed from a sidebar.
//!
//! [`HerdrApi`] is the seam the tests stand a fake behind: one method, one
//! process, so the fake only has to answer argv with the JSON Herdr would.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;

/// One `herdr` invocation, answered with the `result` object it printed.
pub trait HerdrApi: Send {
    fn call(&self, args: &[&str]) -> Result<Value>;
}

/// The real binary, `herdr` on the path unless told otherwise. With
/// `TICKET_TUI_HERDR_SESSION` set, every call goes to that named session
/// rather than the default one — which is how a launch is tried against a
/// server of its own.
pub struct HerdrCli {
    binary: PathBuf,
    session: Option<String>,
    /// The most one call may take before it is given up on.
    cap: Duration,
}

/// The most one `herdr` call may take. `agent start` may legitimately wait a
/// minute for a cold CLI, so this sits above that; a server that has stopped
/// answering would otherwise hold the agent thread, and every later `w`,
/// for ever.
const CALL_CAP: Duration = Duration::from_secs(90);

impl Default for HerdrCli {
    fn default() -> Self {
        Self {
            session: std::env::var("TICKET_TUI_HERDR_SESSION")
                .ok()
                .filter(|name| !name.trim().is_empty()),
            ..Self::at(Path::new("herdr"))
        }
    }
}

impl HerdrCli {
    #[must_use]
    pub fn at(binary: &Path) -> Self {
        Self {
            binary: binary.to_path_buf(),
            session: None,
            cap: CALL_CAP,
        }
    }
}

impl HerdrApi for HerdrCli {
    fn call(&self, args: &[&str]) -> Result<Value> {
        let mut command = Command::new(&self.binary);
        if let Some(session) = &self.session {
            command.arg("--session").arg(session);
        }
        let mut child = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("{} could not be run", self.binary.display()))?;
        // Drained as they come, so a long answer never fills a pipe and
        // stalls the child while this waits for it.
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let deadline = Instant::now() + self.cap;
        let status = loop {
            if let Some(status) = child.try_wait().context("waiting for herdr")? {
                break Some(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let Some(status) = status else {
            // Not joined: whatever the killed child left holding the pipes
            // ends on its own, and there is nothing to read from it anyway.
            return Err(HerdrError {
                code: "no_answer".to_owned(),
                message: format!(
                    "herdr {} gave no answer in {} s",
                    args.iter().take(2).copied().collect::<Vec<_>>().join(" "),
                    self.cap.as_secs()
                ),
            }
            .into());
        };
        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        parse_response(&stdout, &stderr, status.code())
    }
}

/// Reads a child's stream to the end on a thread of its own.
fn drain<R: Read + Send + 'static>(reader: Option<R>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut reader) = reader {
            let _ = reader.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// What Herdr refused, as it said it: `server_not_running`,
/// `agent_not_ready`, `agent_blocked`… A usage error — which is also what an
/// id Herdr cannot resolve gets — is `usage`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HerdrError {
    pub code: String,
    pub message: String,
}

impl HerdrError {
    /// Whether this is Herdr not knowing the thing that was named, which for
    /// a lookup is an answer rather than a failure.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.code == "usage" || self.code.ends_with("not_found")
    }
}

impl fmt::Display for HerdrError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "herdr: {}: {}", self.code, self.message)
    }
}

impl std::error::Error for HerdrError {}

/// Reads one invocation's answer. Herdr prints `{"id", "result"}` on success
/// and `{"id", "error": {"code", "message"}}` for a refusal — on stdout with
/// exit 0 when the server is down, on stderr with exit 1 for most others —
/// and a plain `unknown command` on stderr with exit 2 for an argument it
/// could not resolve.
pub fn parse_response(stdout: &str, stderr: &str, exit: Option<i32>) -> Result<Value> {
    for text in [stdout, stderr] {
        if let Ok(document) = serde_json::from_str::<Value>(text.trim()) {
            if let Some(error) = document.get("error") {
                return Err(HerdrError {
                    code: error["code"].as_str().unwrap_or("error").to_owned(),
                    message: error["message"].as_str().unwrap_or("no message").to_owned(),
                }
                .into());
            }
            if exit == Some(0) {
                return Ok(document.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }
    let message = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    Err(HerdrError {
        code: if exit == Some(2) { "usage" } else { "error" }.to_owned(),
        message: message.lines().next().unwrap_or("no output").to_owned(),
    }
    .into())
}

/// The refusal an error carries, when it is Herdr's.
fn herdr_error(error: &anyhow::Error) -> Option<&HerdrError> {
    error.downcast_ref::<HerdrError>()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tab {
    pub id: String,
    pub workspace_id: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pane {
    pub id: String,
    pub tab_id: String,
    pub workspace_id: String,
    /// The kind of agent in it — `claude`, `copilot`… — or nothing.
    pub agent: Option<String>,
    pub cwd: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Agent {
    pub pane_id: String,
    pub tab_id: String,
    pub workspace_id: String,
    pub kind: String,
    pub name: Option<String>,
    pub status: String,
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

fn optional(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn workspace_of(value: &Value) -> Workspace {
    Workspace {
        id: text(value, "workspace_id"),
        label: text(value, "label"),
    }
}

fn tab_of(value: &Value) -> Tab {
    Tab {
        id: text(value, "tab_id"),
        workspace_id: text(value, "workspace_id"),
        label: text(value, "label"),
    }
}

fn pane_of(value: &Value) -> Pane {
    Pane {
        id: text(value, "pane_id"),
        tab_id: text(value, "tab_id"),
        workspace_id: text(value, "workspace_id"),
        agent: optional(value, "agent"),
        cwd: text(value, "cwd"),
    }
}

fn agent_of(value: &Value) -> Agent {
    Agent {
        pane_id: text(value, "pane_id"),
        tab_id: text(value, "tab_id"),
        workspace_id: text(value, "workspace_id"),
        kind: text(value, "agent"),
        name: optional(value, "name"),
        status: text(value, "agent_status"),
    }
}

fn list(value: &Value, key: &str) -> Vec<Value> {
    value[key].as_array().cloned().unwrap_or_default()
}

/// The commands the launch needs, typed. Every `get` answers `None` for a
/// thing Herdr does not know rather than failing, so a stale id reads as
/// stale; every other refusal is an error in Herdr's own words.
pub struct Herdr {
    api: Box<dyn HerdrApi>,
}

/// How long `agent start` may wait for the CLI to come up ready. Copilot and
/// Cursor both take a few seconds; a minute leaves room for a cold start.
const START_TIMEOUT_MS: &str = "60000";

/// How long `agent prompt --wait` is given. Herdr's own five-second gate for
/// the agent to start working sits inside it; the rest is for a turn that
/// settles at once, which is not waited for beyond this.
const PROMPT_TIMEOUT_MS: &str = "10000";

/// How long an agent nudged with Enter is given to start working.
const NUDGE_TIMEOUT_MS: &str = "5000";

impl Herdr {
    #[must_use]
    pub fn new(api: Box<dyn HerdrApi>) -> Self {
        Self { api }
    }

    fn lookup(&self, args: &[&str]) -> Result<Option<Value>> {
        match self.api.call(args) {
            Ok(value) => Ok(Some(value)),
            Err(error) if herdr_error(&error).is_some_and(HerdrError::is_not_found) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn workspaces(&self) -> Result<Vec<Workspace>> {
        let result = self.api.call(&["workspace", "list"])?;
        Ok(list(&result, "workspaces")
            .iter()
            .map(workspace_of)
            .collect())
    }

    pub fn workspace(&self, id: &str) -> Result<Option<Workspace>> {
        Ok(self
            .lookup(&["workspace", "get", id])?
            .map(|result| workspace_of(&result["workspace"])))
    }

    /// Makes a workspace with `label`, whose first tab and pane open in
    /// `cwd`, and shows it: a launch is the user asking to go there. Answers
    /// the workspace, that tab and that pane.
    pub fn create_workspace(&self, label: &str, cwd: &Path) -> Result<(Workspace, Tab, Pane)> {
        let cwd = cwd.to_string_lossy();
        let result = self.api.call(&[
            "workspace",
            "create",
            "--label",
            label,
            "--cwd",
            &cwd,
            "--focus",
        ])?;
        Ok((
            workspace_of(&result["workspace"]),
            tab_of(&result["tab"]),
            pane_of(&result["root_pane"]),
        ))
    }

    pub fn tabs(&self, workspace: &str) -> Result<Vec<Tab>> {
        let result = self.api.call(&["tab", "list", "--workspace", workspace])?;
        Ok(list(&result, "tabs").iter().map(tab_of).collect())
    }

    pub fn tab(&self, id: &str) -> Result<Option<Tab>> {
        Ok(self
            .lookup(&["tab", "get", id])?
            .map(|result| tab_of(&result["tab"])))
    }

    pub fn create_tab(&self, workspace: &str, label: &str, cwd: &Path) -> Result<(Tab, Pane)> {
        let cwd = cwd.to_string_lossy();
        let result = self.api.call(&[
            "tab",
            "create",
            "--workspace",
            workspace,
            "--label",
            label,
            "--cwd",
            &cwd,
            "--focus",
        ])?;
        Ok((tab_of(&result["tab"]), pane_of(&result["root_pane"])))
    }

    pub fn rename_tab(&self, id: &str, label: &str) -> Result<()> {
        self.api.call(&["tab", "rename", id, label]).map(drop)
    }

    pub fn panes(&self, workspace: &str) -> Result<Vec<Pane>> {
        let result = self.api.call(&["pane", "list", "--workspace", workspace])?;
        Ok(list(&result, "panes").iter().map(pane_of).collect())
    }

    pub fn pane(&self, id: &str) -> Result<Option<Pane>> {
        Ok(self
            .lookup(&["pane", "get", id])?
            .map(|result| pane_of(&result["pane"])))
    }

    /// Whether `pane` can take an agent for `workdir`: nothing but a shell
    /// prompt is in it, and that shell is in `workdir`. At a prompt the shell
    /// itself is the foreground process group (Herdr 0.9 lists it), so a pane
    /// somebody is running a server or an editor in is not free whatever the
    /// agent detector says. A shell somewhere else is left alone: an agent
    /// started in it would work there, not in the checkout.
    pub fn pane_free_in(&self, pane: &Pane, workdir: &Path) -> Result<bool> {
        if pane.agent.is_some() || !same_directory(Path::new(&pane.cwd), workdir) {
            return Ok(false);
        }
        let Some(result) = self.lookup(&["pane", "process-info", "--pane", &pane.id])? else {
            return Ok(false);
        };
        let info = &result["process_info"];
        let shell = info["shell_pid"].as_u64();
        Ok(match info["foreground_process_group_id"].as_u64() {
            Some(group) => Some(group) == shell,
            None => list(info, "foreground_processes")
                .iter()
                .all(|process| process["pid"].as_u64() == shell),
        })
    }

    /// A new pane to the right of `pane`, opened in `cwd`.
    pub fn split_right(&self, pane: &str, cwd: &Path) -> Result<Pane> {
        let cwd = cwd.to_string_lossy();
        let result = self.api.call(&[
            "pane",
            "split",
            pane,
            "--direction",
            "right",
            "--cwd",
            &cwd,
            "--focus",
        ])?;
        Ok(pane_of(&result["pane"]))
    }

    pub fn rename_pane(&self, id: &str, label: &str) -> Result<()> {
        self.api.call(&["pane", "rename", id, label]).map(drop)
    }

    pub fn agents(&self) -> Result<Vec<Agent>> {
        let result = self.api.call(&["agent", "list"])?;
        Ok(list(&result, "agents").iter().map(agent_of).collect())
    }

    /// The agent a name or a pane id points at, or nothing when no live agent
    /// answers to it.
    pub fn agent(&self, target: &str) -> Result<Option<Agent>> {
        Ok(self
            .lookup(&["agent", "get", target])?
            .map(|result| agent_of(&result["agent"])))
    }

    /// Starts `kind` in `pane` under `name`, with `args` handed to the CLI
    /// after Herdr's `--`. Herdr answers once the CLI is ready for input; one
    /// that is slower than its timeout is waited for once more, since the
    /// name is registered either way.
    pub fn start_agent(&self, name: &str, kind: &str, pane: &str, args: &[String]) -> Result<()> {
        let mut argv = vec![
            "agent",
            "start",
            name,
            "--kind",
            kind,
            "--pane",
            pane,
            "--timeout",
            START_TIMEOUT_MS,
        ];
        if !args.is_empty() {
            argv.push("--");
            argv.extend(args.iter().map(String::as_str));
        }
        match self.api.call(&argv) {
            Ok(_) => Ok(()),
            Err(error)
                if herdr_error(&error).is_some_and(|error| error.code == "agent_not_ready") =>
            {
                self.api
                    .call(&[
                        "agent",
                        "wait",
                        name,
                        "--until",
                        "idle",
                        "--timeout",
                        START_TIMEOUT_MS,
                    ])
                    .map(drop)
                    .with_context(|| format!("{name} started but never came ready"))
            }
            Err(error) => Err(error),
        }
    }

    /// Sends `text` to the agent as one message, followed by Enter, and
    /// waits for Herdr to see it taken: `working` or `blocked` within its
    /// gate. A turn that has not settled by the timeout was taken all the
    /// same, so `timeout` is success. The text is one argv element: whatever
    /// a ticket's title holds, no shell reads it.
    ///
    /// A prompt Herdr reports stalled — Cursor Agent has been seen to take a
    /// multi-line paste but not the Enter after it — is given one Enter, and
    /// if that is not taken either, is reported delivered but unconfirmed in
    /// the answer rather than sent again: a stall does not prove the text
    /// never landed, and a second copy in the box would be worse than none.
    pub fn prompt(&self, target: &str, text: &str) -> Result<Option<String>> {
        let refusal = |error: &anyhow::Error| herdr_error(error).map(|held| held.code.clone());
        let sent = self.api.call(&[
            "agent",
            "prompt",
            target,
            text,
            "--wait",
            "--timeout",
            PROMPT_TIMEOUT_MS,
        ]);
        match sent {
            Ok(_) => return Ok(None),
            Err(error) if refusal(&error).as_deref() == Some("timeout") => return Ok(None),
            Err(error) if refusal(&error).as_deref() == Some("agent_prompt_stalled") => {}
            Err(error) => return Err(error),
        }
        self.api.call(&["agent", "send-keys", target, "enter"])?;
        let taken = self.api.call(&[
            "agent",
            "wait",
            target,
            "--until",
            "working",
            "--until",
            "blocked",
            "--timeout",
            NUDGE_TIMEOUT_MS,
        ]);
        match taken {
            Ok(_) => Ok(None),
            Err(error) if refusal(&error).as_deref() == Some("timeout") => Ok(Some(
                "the prompt is in its input box but was not taken; press Enter in the pane"
                    .to_owned(),
            )),
            Err(error) => Err(error),
        }
    }

    pub fn focus_agent(&self, target: &str) -> Result<()> {
        self.api.call(&["agent", "focus", target]).map(drop)
    }

    pub fn focus_tab(&self, id: &str) -> Result<()> {
        self.api.call(&["tab", "focus", id]).map(drop)
    }

    pub fn focus_workspace(&self, id: &str) -> Result<()> {
        self.api.call(&["workspace", "focus", id]).map(drop)
    }

    /// A name no live agent holds: `base`, then `base-2`, `base-3`…
    pub fn free_agent_name(&self, base: &str) -> Result<String> {
        let taken: Vec<String> = self
            .agents()?
            .into_iter()
            .filter_map(|agent| agent.name)
            .collect();
        if !taken.iter().any(|name| name == base) {
            return Ok(base.to_owned());
        }
        Ok((2..)
            .map(|n| format!("{base}-{n}"))
            .find(|candidate| !taken.contains(candidate))
            .expect("the counter is unbounded"))
    }
}

/// Whether two paths name one directory, as the filesystem spells them.
fn same_directory(left: &Path, right: &Path) -> bool {
    let spell = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    spell(left) == spell(right)
}

/// Herdr's own rule for an agent name: `[a-z][a-z0-9_-]{0,31}`. A work item
/// number always fits it.
#[must_use]
pub fn agent_name_for(work_item: i64) -> String {
    format!("wi-{work_item}")
}

/// Whether this process is inside a Herdr pane, which is the only place a
/// launch may target: Herdr says so through `HERDR_ENV=1`.
#[must_use]
pub fn inside_herdr() -> bool {
    std::env::var("HERDR_ENV").is_ok_and(|value| value == "1")
}

#[cfg(test)]
pub(crate) mod fake {
    //! Herdr stood in for: an in-memory workspace tree answering the same
    //! argv the real CLI takes, with the same JSON shapes, and a log of every
    //! call so a test can say what was asked for and in what order.

    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;

    #[derive(Clone, Debug, Default)]
    pub struct FakePane {
        pub id: String,
        pub tab_id: String,
        pub workspace_id: String,
        pub agent: Option<String>,
        pub agent_name: Option<String>,
        pub cwd: String,
        pub label: Option<String>,
        /// Whether something other than a shell prompt is in the foreground.
        pub busy: bool,
        pub prompts: Vec<String>,
        /// The shell's pid, which is the foreground process group at a prompt.
        pub pid: u64,
    }

    #[derive(Clone, Debug, Default)]
    pub struct FakeState {
        pub workspaces: Vec<(String, String)>,
        pub tabs: Vec<(String, String, String)>,
        pub panes: Vec<FakePane>,
        pub focused: Option<String>,
        pub calls: Vec<Vec<String>>,
        pub next: usize,
        /// Calls starting with a prefix are refused with its error.
        pub failing: Vec<(String, HerdrError)>,
        /// Whether the server is down: every call is refused.
        pub down: bool,
        /// Whether a started agent comes up blocked: it is registered under
        /// its name, as Herdr does, but `agent start` answers `agent_not_ready`.
        pub start_blocked: bool,
        /// Whether a prompt lands in the box without being taken: it is
        /// recorded, and `agent prompt --wait` answers `agent_prompt_stalled`.
        pub prompt_stalls: bool,
    }

    /// The fake, shared between the test and the thread under test.
    #[derive(Clone, Default)]
    pub struct FakeHerdr {
        pub state: Arc<Mutex<FakeState>>,
    }

    impl FakeHerdr {
        pub fn with(state: FakeState) -> Self {
            Self {
                state: Arc::new(Mutex::new(state)),
            }
        }

        pub fn state(&self) -> FakeState {
            self.state.lock().unwrap().clone()
        }

        pub fn calls(&self) -> Vec<String> {
            self.state()
                .calls
                .iter()
                .map(|call| call.join(" "))
                .collect()
        }

        pub fn fail(&self, prefix: &str, code: &str, message: &str) {
            self.state.lock().unwrap().failing.push((
                prefix.to_owned(),
                HerdrError {
                    code: code.to_owned(),
                    message: message.to_owned(),
                },
            ));
        }

        pub fn clear_failure(&self) {
            self.state.lock().unwrap().failing.clear();
        }
    }

    impl FakeState {
        pub fn add_workspace(&mut self, label: &str) -> String {
            self.next += 1;
            let id = format!("w{}", self.next);
            self.workspaces.push((id.clone(), label.to_owned()));
            id
        }

        pub fn add_tab(&mut self, workspace: &str, label: &str) -> String {
            self.next += 1;
            let id = format!("{workspace}:t{}", self.next);
            self.tabs
                .push((id.clone(), workspace.to_owned(), label.to_owned()));
            id
        }

        pub fn add_pane(&mut self, tab: &str, cwd: &str, agent: Option<&str>) -> String {
            self.next += 1;
            let workspace = tab.split(':').next().unwrap_or_default().to_owned();
            let id = format!("{workspace}:p{}", self.next);
            self.panes.push(FakePane {
                id: id.clone(),
                tab_id: tab.to_owned(),
                workspace_id: workspace,
                agent: agent.map(str::to_owned),
                cwd: cwd.to_owned(),
                pid: 1000 + self.next as u64,
                ..FakePane::default()
            });
            id
        }

        fn workspace_json(&self, id: &str, label: &str) -> Value {
            json!({"workspace_id": id, "label": label, "focused": self.focused.as_deref() == Some(id)})
        }

        fn tab_json(&self, id: &str, workspace: &str, label: &str) -> Value {
            json!({"tab_id": id, "workspace_id": workspace, "label": label})
        }

        fn pane_json(pane: &FakePane) -> Value {
            json!({
                "pane_id": pane.id, "tab_id": pane.tab_id, "workspace_id": pane.workspace_id,
                "agent": pane.agent, "cwd": pane.cwd, "agent_status": pane.agent.as_ref().map(|_| "idle"),
                "name": pane.agent_name,
            })
        }

        fn pane_by_target(&self, target: &str) -> Option<&FakePane> {
            self.panes
                .iter()
                .find(|pane| pane.id == target || pane.agent_name.as_deref() == Some(target))
        }

        fn pane_by_target_mut(&mut self, target: &str) -> Option<&mut FakePane> {
            self.panes
                .iter_mut()
                .find(|pane| pane.id == target || pane.agent_name.as_deref() == Some(target))
        }

        fn flag<'a>(args: &'a [&str], flag: &str) -> Option<&'a str> {
            args.iter()
                .position(|arg| *arg == flag)
                .and_then(|at| args.get(at + 1))
                .copied()
        }

        fn answer(&mut self, args: &[&str]) -> Result<Value> {
            let unknown = || {
                Err(HerdrError {
                    code: "usage".into(),
                    message: format!("unknown command: {}", args.join(" ")),
                }
                .into())
            };
            match args {
                ["workspace", "list"] => Ok(
                    json!({"workspaces": self.workspaces.iter().map(|(id, label)| self.workspace_json(id, label)).collect::<Vec<_>>()}),
                ),
                ["workspace", "get", id] => {
                    match self.workspaces.iter().find(|(held, _)| held == id) {
                        Some((id, label)) => {
                            Ok(json!({"workspace": self.workspace_json(id, label)}))
                        }
                        None => unknown(),
                    }
                }
                ["workspace", "create", ..] => {
                    let label = Self::flag(args, "--label").unwrap_or("1").to_owned();
                    let cwd = Self::flag(args, "--cwd").unwrap_or("/").to_owned();
                    let workspace = self.add_workspace(&label);
                    let tab = self.add_tab(&workspace, "1");
                    let pane = self.add_pane(&tab, &cwd, None);
                    let pane = self.panes.iter().find(|held| held.id == pane).unwrap();
                    Ok(json!({
                        "workspace": self.workspace_json(&workspace, &label),
                        "tab": self.tab_json(&tab, &workspace, "1"),
                        "root_pane": Self::pane_json(pane),
                    }))
                }
                ["tab", "list", "--workspace", workspace] => Ok(
                    json!({"tabs": self.tabs.iter().filter(|(_, held, _)| held == workspace).map(|(id, workspace, label)| self.tab_json(id, workspace, label)).collect::<Vec<_>>()}),
                ),
                ["tab", "get", id] => match self.tabs.iter().find(|(held, _, _)| held == id) {
                    Some((id, workspace, label)) => {
                        Ok(json!({"tab": self.tab_json(id, workspace, label)}))
                    }
                    None => unknown(),
                },
                ["tab", "create", ..] => {
                    let workspace = Self::flag(args, "--workspace")
                        .unwrap_or_default()
                        .to_owned();
                    if !self.workspaces.iter().any(|(id, _)| *id == workspace) {
                        return unknown();
                    }
                    let label = Self::flag(args, "--label").unwrap_or("1").to_owned();
                    let cwd = Self::flag(args, "--cwd").unwrap_or("/").to_owned();
                    let tab = self.add_tab(&workspace, &label);
                    let pane = self.add_pane(&tab, &cwd, None);
                    let pane = self.panes.iter().find(|held| held.id == pane).unwrap();
                    Ok(
                        json!({"tab": self.tab_json(&tab, &workspace, &label), "root_pane": Self::pane_json(pane)}),
                    )
                }
                ["workspace", "focus", id] => {
                    if self.workspaces.iter().any(|(held, _)| held == id) {
                        self.focused = Some((*id).to_owned());
                        Ok(json!({}))
                    } else {
                        unknown()
                    }
                }
                ["tab", "rename", id, label] => {
                    match self.tabs.iter_mut().find(|(held, _, _)| held == id) {
                        Some(tab) => {
                            tab.2 = (*label).to_owned();
                            Ok(json!({}))
                        }
                        None => unknown(),
                    }
                }
                ["tab", "focus", id] => {
                    if self.tabs.iter().any(|(held, _, _)| held == id) {
                        self.focused = Some((*id).to_owned());
                        Ok(json!({}))
                    } else {
                        unknown()
                    }
                }
                ["pane", "list", "--workspace", workspace] => Ok(
                    json!({"panes": self.panes.iter().filter(|pane| pane.workspace_id == *workspace).map(Self::pane_json).collect::<Vec<_>>()}),
                ),
                ["pane", "get", id] => match self.panes.iter().find(|pane| pane.id == *id) {
                    Some(pane) => Ok(json!({"pane": Self::pane_json(pane)})),
                    None => unknown(),
                },
                ["pane", "process-info", "--pane", id] => {
                    match self.panes.iter().find(|pane| pane.id == *id) {
                        Some(pane) => {
                            // As Herdr 0.9 reports it: at a prompt the shell
                            // itself is the foreground; else what is in it.
                            let (name, pid) = match (&pane.agent, pane.busy) {
                                (Some(agent), _) => (agent.clone(), pane.pid + 1),
                                (None, true) => ("vim".to_owned(), pane.pid + 1),
                                (None, false) => ("zsh".to_owned(), pane.pid),
                            };
                            Ok(json!({"process_info": {
                                "pane_id": id,
                                "shell_pid": pane.pid,
                                "foreground_process_group_id": pid,
                                "foreground_processes": [{"name": name, "pid": pid}],
                            }}))
                        }
                        None => unknown(),
                    }
                }
                ["pane", "split", id, ..] => {
                    let Some(origin) = self.panes.iter().find(|pane| pane.id == *id).cloned()
                    else {
                        return unknown();
                    };
                    let cwd = Self::flag(args, "--cwd").unwrap_or(&origin.cwd).to_owned();
                    let pane = self.add_pane(&origin.tab_id, &cwd, None);
                    let pane = self.panes.iter().find(|held| held.id == pane).unwrap();
                    Ok(json!({"pane": Self::pane_json(pane)}))
                }
                ["pane", "rename", id, label] => {
                    match self.panes.iter_mut().find(|pane| pane.id == *id) {
                        Some(pane) => {
                            pane.label = Some((*label).to_owned());
                            Ok(json!({}))
                        }
                        None => unknown(),
                    }
                }
                ["agent", "list"] => Ok(
                    json!({"agents": self.panes.iter().filter(|pane| pane.agent.is_some()).map(Self::pane_json).collect::<Vec<_>>()}),
                ),
                ["agent", "get", target] => match self
                    .pane_by_target(target)
                    .filter(|pane| pane.agent.is_some())
                {
                    Some(pane) => Ok(json!({"agent": Self::pane_json(pane)})),
                    None => unknown(),
                },
                ["agent", "start", name, "--kind", kind, "--pane", pane, ..] => {
                    let name = (*name).to_owned();
                    let kind = (*kind).to_owned();
                    if self
                        .panes
                        .iter()
                        .any(|held| held.agent_name.as_deref() == Some(&name))
                    {
                        return Err(HerdrError {
                            code: "agent_name_taken".into(),
                            message: name,
                        }
                        .into());
                    }
                    let blocked = self.start_blocked;
                    match self.panes.iter_mut().find(|held| held.id == *pane) {
                        Some(held) if held.agent.is_none() && !held.busy => {
                            held.agent = Some(kind);
                            held.agent_name = Some(name.clone());
                            if blocked {
                                return Err(HerdrError {
                                    code: "agent_not_ready".into(),
                                    message: format!("{name} is blocked during startup"),
                                }
                                .into());
                            }
                            Ok(json!({"agent": Self::pane_json(held)}))
                        }
                        Some(_) => Err(HerdrError {
                            code: "pane_busy".into(),
                            message: "pane is not at a shell prompt".into(),
                        }
                        .into()),
                        None => unknown(),
                    }
                }
                ["agent", "prompt", target, text, ..] => {
                    let stalls = self.prompt_stalls;
                    match self
                        .pane_by_target_mut(target)
                        .filter(|pane| pane.agent.is_some())
                    {
                        Some(pane) => {
                            pane.prompts.push((*text).to_owned());
                            if stalls {
                                return Err(HerdrError {
                                    code: "agent_prompt_stalled".into(),
                                    message: "no activity followed the prompt".into(),
                                }
                                .into());
                            }
                            Ok(json!({}))
                        }
                        None => unknown(),
                    }
                }
                ["agent", "send-keys", target, ..] => match self.pane_by_target(target) {
                    Some(_) => Ok(json!({"type": "ok"})),
                    None => unknown(),
                },
                ["agent", "wait", target, ..] => match self.pane_by_target(target) {
                    Some(_) => Ok(json!({"agent_status": "idle"})),
                    None => unknown(),
                },
                ["agent", "focus", target] => match self
                    .pane_by_target(target)
                    .filter(|pane| pane.agent.is_some())
                {
                    Some(pane) => {
                        self.focused = Some(pane.id.clone());
                        Ok(json!({}))
                    }
                    None => unknown(),
                },
                _ => unknown(),
            }
        }
    }

    impl HerdrApi for FakeHerdr {
        fn call(&self, args: &[&str]) -> Result<Value> {
            let mut state = self.state.lock().unwrap();
            state
                .calls
                .push(args.iter().map(|arg| (*arg).to_owned()).collect());
            if state.down {
                return Err(HerdrError {
                    code: "server_not_running".into(),
                    message: "no herdr server is running".into(),
                }
                .into());
            }
            let joined = args.join(" ");
            if let Some((_, error)) = state
                .failing
                .iter()
                .find(|(prefix, _)| joined.starts_with(prefix.as_str()))
            {
                return Err(error.clone().into());
            }
            state.answer(args)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_is_its_result_and_a_refusal_is_its_code() {
        let ok = parse_response(
            r#"{"id":"cli:workspace:list","result":{"type":"workspace_list","workspaces":[]}}"#,
            "",
            Some(0),
        )
        .unwrap();
        assert_eq!(ok["type"], "workspace_list");

        // The server being down is JSON on stdout with exit 0.
        let down = parse_response(
            r#"{"id":"x","error":{"code":"server_not_running","message":"no herdr server"}}"#,
            "",
            Some(0),
        )
        .unwrap_err();
        let refusal = down.downcast_ref::<HerdrError>().unwrap();
        assert_eq!(refusal.code, "server_not_running");
        assert!(!refusal.is_not_found());

        // An id Herdr cannot resolve is a usage error on stderr with exit 2.
        let unknown = parse_response(
            "",
            "unknown command: pane get wH:p9\nrun 'herdr --help'",
            Some(2),
        )
        .unwrap_err();
        let refusal = unknown.downcast_ref::<HerdrError>().unwrap();
        assert_eq!(refusal.code, "usage");
        assert!(refusal.is_not_found());
        assert_eq!(refusal.message, "unknown command: pane get wH:p9");

        // A refusal on stderr with exit 1 keeps its own code.
        let blocked = parse_response(
            "",
            r#"{"id":"x","error":{"code":"agent_blocked","message":"waiting on an approval"}}"#,
            Some(1),
        )
        .unwrap_err();
        assert_eq!(
            blocked.downcast_ref::<HerdrError>().unwrap().code,
            "agent_blocked"
        );
    }

    /// The real client over a script standing in for the binary, so the
    /// argv, the JSON and the exit codes are read the way Herdr writes them.
    #[cfg(unix)]
    #[test]
    fn the_binary_is_run_with_argv_and_read_back_as_json() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("herdr");
        let log = dir.path().join("calls.log");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\ncase \"$1 $2\" in\n\
                 'workspace list') echo '{{\"id\":\"1\",\"result\":{{\"workspaces\":[{{\"workspace_id\":\"w1\",\"label\":\"Pay\"}}]}}}}' ;;\n\
                 'pane get') echo 'unknown command: pane get' >&2; exit 2 ;;\n\
                 'agent get') echo '{{\"id\":\"1\",\"result\":{{\"agent\":{{\"pane_id\":\"w1:p1\",\"agent\":\"copilot\",\"agent_status\":\"working\"}}}}}}' ;;\n\
                 'agent prompt') printf '%s' \"$4\" > {prompt}; echo '{{\"id\":\"1\",\"result\":{{}}}}' ;;\n\
                 'agent wait') sleep 5 ;;\n\
                 *) echo '{{\"id\":\"1\",\"error\":{{\"code\":\"agent_blocked\",\"message\":\"no\"}}}}' >&2; exit 1 ;;\n\
                 esac\n",
                log = log.display(),
                prompt = dir.path().join("prompt.txt").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let herdr = Herdr::new(Box::new(HerdrCli::at(&script)));

        assert_eq!(
            herdr.workspaces().unwrap(),
            [Workspace {
                id: "w1".into(),
                label: "Pay".into()
            }]
        );
        assert_eq!(
            herdr.pane("w1:p9").unwrap(),
            None,
            "exit 2 reads as not found"
        );
        // A prompt with every shell metacharacter in it lands verbatim: it is
        // one argv element, and nothing between here and the pane is a shell.
        let hostile = "Fix `rm -rf /`; echo $(whoami) \"quoted\" 'single' && done";
        herdr.prompt("wi-715", hostile).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("prompt.txt")).unwrap(),
            hostile
        );
        let refused = herdr.focus_agent("wi-715").unwrap_err();
        assert_eq!(
            refused.downcast_ref::<HerdrError>().unwrap().code,
            "agent_blocked"
        );
        // A call that never answers is given up on at the cap, not waited for.
        let mut capped = HerdrCli::at(&script);
        capped.cap = Duration::from_millis(300);
        let started = Instant::now();
        let hung = capped.call(&["agent", "wait", "wi-715"]).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3));
        let refusal = hung.downcast_ref::<HerdrError>().unwrap();
        assert_eq!(refusal.code, "no_answer");
        assert!(
            refusal.message.contains("agent wait"),
            "{}",
            refusal.message
        );
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            calls.starts_with("workspace list\npane get w1:p9\n"),
            "{calls}"
        );
    }

    #[test]
    fn the_fake_answers_the_argv_the_launch_uses() {
        let fake = fake::FakeHerdr::default();
        let herdr = Herdr::new(Box::new(fake.clone()));
        assert!(herdr.workspaces().unwrap().is_empty());
        let (workspace, tab, pane) = herdr
            .create_workspace("Payments", Path::new("/src/pay"))
            .unwrap();
        assert_eq!(workspace.label, "Payments");
        assert_eq!(tab.workspace_id, workspace.id);
        assert_eq!(pane.tab_id, tab.id);
        assert!(herdr.pane_free_in(&pane, Path::new("/src/pay")).unwrap());
        assert!(
            !herdr.pane_free_in(&pane, Path::new("/src/other")).unwrap(),
            "a shell in another directory is not free for this one"
        );
        herdr
            .start_agent(
                "wi-715",
                "copilot",
                &pane.id,
                &["--model".into(), "x".into()],
            )
            .unwrap();
        assert!(
            !herdr
                .pane_free_in(
                    &herdr.pane(&pane.id).unwrap().unwrap(),
                    Path::new("/src/pay")
                )
                .unwrap()
        );
        assert_eq!(
            herdr.agent("wi-715").unwrap().map(|agent| agent.kind),
            Some("copilot".into())
        );
        assert_eq!(herdr.free_agent_name("wi-715").unwrap(), "wi-715-2");
        assert_eq!(herdr.agent("wi-999").unwrap(), None);

        // A prompt that is taken, or whose turn merely has not settled, is
        // sent once and said to be. One that stalls gets one Enter; if that
        // is not taken either, it is still not sent again.
        assert_eq!(herdr.prompt("wi-715", "go").unwrap(), None);
        fake.fail("agent prompt", "timeout", "still working");
        assert_eq!(herdr.prompt("wi-715", "go").unwrap(), None);
        fake.clear_failure();
        fake.state.lock().unwrap().prompt_stalls = true;
        assert_eq!(herdr.prompt("wi-715", "go").unwrap(), None);
        fake.fail("agent wait", "timeout", "still idle");
        let note = herdr.prompt("wi-715", "go").unwrap().unwrap();
        assert!(note.contains("press Enter"), "{note}");
        fake.clear_failure();
        fake.fail("agent prompt", "agent_blocked", "an approval is up");
        assert!(herdr.prompt("wi-715", "go").is_err());
        let calls = fake.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("agent prompt wi-715 go --wait --timeout"))
                .count(),
            5
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("agent send-keys wi-715 enter"))
                .count(),
            2,
            "one Enter per stall, never more"
        );
        let right = herdr.split_right(&pane.id, Path::new("/src/pay2")).unwrap();
        assert_eq!(right.tab_id, tab.id);
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.starts_with("agent start wi-715 --kind copilot --pane"))
        );
    }
}
