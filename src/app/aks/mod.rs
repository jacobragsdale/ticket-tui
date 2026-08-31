//! The AKS screen: every pod of every cluster `config.toml` names, in one
//! table, and what the details pane says about the one under the cursor.
//!
//! Nothing here is stored. A pod is read live, the way local git state is, and
//! the next read replaces it; the log the text pane tails and the description
//! it shows in its place live for as long as the cursor stays on the pod.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::pipelines::LOG_LINE_CAP;
use super::{AppAction, CopiedContent, Focus, ListCursor, Screen, Shell, TabId};
use crate::aks::{AksRequest, Cluster, LogFollow, Pod, PodKey, PodRow, PodSchema};
use crate::columns::{ColumnId, ColumnLayout, TableLayout};
use crate::command::{CommandId, command_for_key};
use crate::filter::{MatchContext, ParsedQuery, parse_query};
use crate::model::Jump;
use crate::pointer::{PointerTarget, ScrollState, ScrollSurface, TextEditor};
use crate::session::TabSession;
use crate::text_input::TextInput;

mod columns;
#[cfg(test)]
pub(crate) mod tests;

pub use columns::PodColumn;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AksMode {
    #[default]
    Browse,
    Search,
    /// `x` has been pressed once, and the modal is asking whether it meant it.
    ConfirmRestart,
}

/// What the text pane under the pod's details is showing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PaneText {
    /// The pod's log, tailed.
    #[default]
    Log,
    /// What `kubectl describe pod` said, in the log's place.
    Describe,
}

pub struct AksScreen {
    clusters: Vec<Cluster>,
    pods: Vec<Pod>,
    /// `(cluster, namespace, message)`, one per failed read, replaced by the
    /// next read of the same pair and dropped by one that succeeds.
    errors: Vec<(String, Option<String>, String)>,
    pub mode: AksMode,
    query: TextInput,
    pub layout: TableLayout<PodColumn>,
    pub sort: (PodColumn, bool),
    pub cursor: ListCursor,
    /// Which of the two the text pane is showing, and where it is scrolled to.
    /// One scroll for both: only one of them is on screen at a time.
    pane: PaneText,
    pub pane_scroll: ScrollState,
    /// Whether the log pane is pinned to the tail, and whether it has the whole
    /// details pane to itself.
    log_follow: bool,
    log_full: bool,
    /// Whether the stream has ended: the pod went, or `kubectl` refused.
    log_finished: bool,
    /// What the pane is tailing now, which is also what the worker is told to
    /// follow. The lines held are this target's and nobody else's.
    log_target: Option<LogFollow>,
    log_lines: Vec<String>,
    /// The container chosen with `C`, if the pod has one by that name, and
    /// whether `P` has asked for the run before the last restart.
    container: Option<String>,
    previous: bool,
    /// What `kubectl describe pod` said about one pod, and the pod a describe
    /// is out for.
    describe: Option<(PodKey, Result<Vec<String>, String>)>,
    describe_pending: Option<PodKey>,
    /// The pod the confirmation is about, and the one a delete is out for.
    /// Nothing is ever deleted without the first becoming the second.
    pub restarting: Option<PodKey>,
    deleting: Option<PodKey>,
    /// When the last read landed, which the table's status says: nothing here
    /// is stored, so how old the answer is matters.
    read_at: Option<Instant>,
    /// Whether the user has just asked for a read. The next failure after one
    /// is said out loud even if it says what the last one said.
    refresh_pending: bool,
    /// Whether anything has been read at all, which is what tells "nothing
    /// matches" from "nothing has come back yet".
    reads_seen: usize,
}

impl Default for AksScreen {
    fn default() -> Self {
        Self {
            clusters: Vec::new(),
            pods: Vec::new(),
            errors: Vec::new(),
            mode: AksMode::Browse,
            query: TextInput::default(),
            layout: TableLayout::default(),
            // By cluster, so the two clusters' pods do not interleave.
            sort: (PodColumn::Cluster, false),
            cursor: ListCursor::default(),
            pane: PaneText::default(),
            pane_scroll: ScrollState::default(),
            log_follow: true,
            log_full: false,
            log_finished: false,
            log_target: None,
            log_lines: Vec::new(),
            container: None,
            previous: false,
            describe: None,
            describe_pending: None,
            restarting: None,
            deleting: None,
            read_at: None,
            refresh_pending: false,
            reads_seen: 0,
        }
    }
}

impl AksScreen {
    /// The clusters the file names, as `config.toml` is read and whenever it
    /// changes. Pods of a cluster nobody names any more go with it.
    pub fn set_clusters(&mut self, clusters: Vec<Cluster>) {
        self.pods
            .retain(|pod| clusters.iter().any(|held| held.name == pod.key.cluster));
        self.errors
            .retain(|(cluster, _, _)| clusters.iter().any(|held| held.name == *cluster));
        self.clusters = clusters;
    }

    #[must_use]
    pub fn clusters(&self) -> &[Cluster] {
        &self.clusters
    }

    /// One `(cluster, namespace)` read. The pods it answers with replace the
    /// ones held for that pair and nothing else, so an unreachable cluster
    /// blanks no other; the cursor stays on the pod it was on.
    ///
    /// Answers with what is worth saying out loud: a refusal nobody has been
    /// told about yet, or any refusal at all after the user asked for a read.
    pub fn set_pods(
        &mut self,
        shell: &Shell,
        cluster: &str,
        namespace: Option<&str>,
        pods: Result<Vec<Pod>, String>,
    ) -> Option<String> {
        let selected = self.selected_key(shell);
        self.read_at = Some(Instant::now());
        self.reads_seen += 1;
        // The same read as this one: the same cluster, and the same namespace
        // when one was named at all.
        let same_read = |held: &str, held_namespace: Option<&str>| {
            held == cluster && (namespace.is_none() || held_namespace == namespace)
        };
        let toast = match pods {
            Ok(read) => {
                self.pods
                    .retain(|pod| !same_read(&pod.key.cluster, Some(&pod.key.namespace)));
                self.pods.extend(read);
                self.errors.retain(|(held, held_namespace, _)| {
                    !same_read(held, held_namespace.as_deref())
                });
                None
            }
            Err(message) => {
                let repeated = self
                    .errors
                    .iter()
                    .any(|(held, held_namespace, held_message)| {
                        same_read(held, held_namespace.as_deref()) && *held_message == message
                    });
                self.errors.retain(|(held, held_namespace, _)| {
                    !same_read(held, held_namespace.as_deref())
                });
                self.errors.push((
                    cluster.to_owned(),
                    namespace.map(str::to_owned),
                    message.clone(),
                ));
                let forced = std::mem::take(&mut self.refresh_pending);
                (!repeated || forced).then(|| format!("{cluster}: {message}"))
            }
        };
        // The rows the cursor is counted over have moved: it stays on its own
        // pod wherever that now sorts.
        let rows = self.visible_pods(shell);
        match selected.and_then(|key| rows.iter().position(|row| row.pod.key == key)) {
            Some(index) => self.cursor.focus(index),
            None => self.cursor.clamp(rows.len()),
        }
        toast
    }

    /// The pod the cursor is on, by key, so a re-read can find it again
    /// wherever it now sorts.
    fn selected_key(&self, shell: &Shell) -> Option<PodKey> {
        self.visible_pods(shell)
            .get(self.cursor.index)
            .map(|row| row.pod.key.clone())
    }

    fn row_count(&self, shell: &Shell) -> usize {
        self.visible_pods(shell).len()
    }

    #[must_use]
    pub fn query(&self) -> &str {
        self.query.text()
    }

    #[must_use]
    pub fn query_cursor(&self) -> usize {
        self.query.cursor()
    }

    pub fn set_query(&mut self, query: String) {
        self.query.set_text(query);
        self.cursor.reset();
    }

    /// Every pod the query leaves, in the order the table draws them.
    #[must_use]
    pub fn visible_pods(&self, shell: &Shell) -> Vec<PodRow> {
        self.rows(&repo_names(shell))
    }

    /// The same, against a given set of repository names.
    fn rows(&self, repos: &[String]) -> Vec<PodRow> {
        let parsed: ParsedQuery<PodSchema> = parse_query(self.query.text());
        let context = MatchContext::now();
        let mut rows: Vec<PodRow> = self
            .pods
            .iter()
            .map(|pod| PodRow::new(pod.clone(), repos))
            .filter(|row| {
                parsed.filters.matches_in(row, false, &context) && row.matches_fuzzy(&parsed.fuzzy)
            })
            .collect();
        let (column, descending) = self.sort;
        rows.sort_by(|left, right| {
            let ordering = columns::compare_pods(left, right, column);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        rows
    }

    #[must_use]
    pub fn selected_pod(&self, shell: &Shell) -> Option<PodRow> {
        self.visible_pods(shell).get(self.cursor.index).cloned()
    }

    /// What the log pane should be following: the pod under the cursor, the
    /// container chosen if it still has one by that name, and whether the run
    /// before the last restart was asked for. The run diffs this against what
    /// the worker was last told.
    #[must_use]
    pub fn log_target(&self, shell: &Shell) -> Option<LogFollow> {
        let row = self.selected_pod(shell)?;
        let container = self
            .container
            .as_ref()
            .filter(|name| {
                row.pod
                    .containers
                    .iter()
                    .any(|held| held.name == name.as_str())
            })
            .cloned();
        Some(LogFollow {
            key: row.pod.key.clone(),
            container,
            previous: self.previous,
        })
    }

    /// What the pane is on now, which is what the lines held belong to.
    #[must_use]
    pub fn following(&self) -> Option<&LogFollow> {
        self.log_target.as_ref()
    }

    /// Settles the pane on whatever the cursor is now over. A different target
    /// is a different stream: the lines held were the last one's, and the pane
    /// goes back to the tail of the new one. Another pod takes its container
    /// choice and its description with it.
    pub fn sync_focus(&mut self, shell: &Shell) {
        let selected = self.selected_pod(shell).map(|row| row.pod.key);
        if selected != self.log_target.as_ref().map(|target| target.key.clone()) {
            self.container = None;
            self.describe = None;
            self.pane = PaneText::Log;
        }
        let target = self.log_target(shell);
        if target == self.log_target {
            return;
        }
        self.log_target = target;
        self.log_lines.clear();
        self.log_finished = false;
        self.log_follow = true;
        self.pane_scroll = ScrollState::default();
    }

    /// Folds lines onto the end of the log. Lines for a stream the pane has
    /// already left are dropped rather than mixed into the one it is on.
    pub fn append_log(&mut self, target: &LogFollow, lines: Vec<String>, finished: bool) {
        if Some(target) != self.log_target.as_ref() {
            return;
        }
        self.log_lines.extend(lines);
        if self.log_lines.len() > LOG_LINE_CAP {
            // One more than the overflow, because the line saying what went
            // takes a place of its own.
            let skipped = self.log_lines.len() - LOG_LINE_CAP + 1;
            self.log_lines.drain(..skipped);
            self.log_lines
                .insert(0, format!("\u{2026} {skipped} earlier lines skipped"));
        }
        self.log_finished = finished;
        if self.log_follow {
            let viewport = self.pane_scroll.viewport.max(1);
            self.pane_scroll.content = self.log_lines.len();
            self.pane_scroll
                .scroll_to(self.log_lines.len().saturating_sub(viewport));
        }
    }

    /// What `kubectl describe pod` said. It reaches the pane only while the
    /// cursor is still on the pod that was asked about.
    pub fn set_description(&mut self, key: &PodKey, text: Result<Vec<String>, String>) {
        if self.describe_pending.as_ref() == Some(key) {
            self.describe_pending = None;
        }
        if self
            .log_target
            .as_ref()
            .is_some_and(|target| target.key == *key)
        {
            self.show_pane(PaneText::Describe);
        }
        self.describe = Some((key.clone(), text));
    }

    /// Moves the log to the next of the pod's containers, round to the first
    /// again. A pod with one container says so rather than doing nothing.
    pub fn next_container(&mut self, shell: &mut Shell) {
        let Some(row) = self.selected_pod(shell) else {
            return;
        };
        let names: Vec<&str> = row
            .pod
            .containers
            .iter()
            .map(|container| container.name.as_str())
            .collect();
        if names.len() < 2 {
            shell.set_status(format!("{} has one container", row.pod.key.name));
            return;
        }
        let current = self
            .container
            .as_ref()
            .and_then(|held| names.iter().position(|name| *name == held.as_str()))
            .unwrap_or(0);
        let next = names[(current + 1) % names.len()].to_owned();
        shell.set_status(format!("Following {next}"));
        self.container = Some(next);
        self.sync_focus(shell);
    }

    /// Turns the `-p` on the log the pane follows on or off: the run before
    /// the last restart is where a crash loop says why.
    pub fn toggle_previous(&mut self, shell: &mut Shell) {
        self.previous = !self.previous;
        shell.set_status(if self.previous {
            "Following the log from before the last restart"
        } else {
            "Following the running log"
        });
        self.sync_focus(shell);
    }

    /// Whether the log pane is pinned to the tail, which is what `End` puts it
    /// back to and scrolling takes it out of.
    #[must_use]
    pub const fn log_following(&self) -> bool {
        self.log_follow
    }

    pub const fn follow_log(&mut self, follow: bool) {
        self.log_follow = follow;
    }

    /// Whether the stream the pane is on has ended.
    #[must_use]
    pub const fn log_ended(&self) -> bool {
        self.log_finished
    }

    /// Whether the text pane has the whole details pane, which `l` toggles.
    #[must_use]
    pub const fn log_full_pane(&self) -> bool {
        self.log_full
    }

    pub const fn toggle_log_pane(&mut self) {
        self.log_full = !self.log_full;
    }

    #[must_use]
    pub const fn pane(&self) -> PaneText {
        self.pane
    }

    #[must_use]
    pub fn log_lines(&self) -> &[String] {
        &self.log_lines
    }

    /// What describe said about the pod under the cursor, when it has been
    /// asked at all.
    #[must_use]
    pub fn describe_lines(&self) -> Option<&Result<Vec<String>, String>> {
        self.describe.as_ref().map(|(_, text)| text)
    }

    /// Puts the text pane on one of the two and starts it at the top; the log
    /// finds its own tail again on the next frame.
    fn show_pane(&mut self, pane: PaneText) {
        self.pane = pane;
        self.pane_scroll.scroll_to(0);
    }

    /// How many pods are held, whatever the query leaves.
    #[must_use]
    pub fn pod_count(&self) -> usize {
        self.pods.len()
    }

    /// How many of them somebody has to look at, which is what the tab bar
    /// wears.
    #[must_use]
    pub fn unhealthy_count(&self) -> usize {
        self.pods.iter().filter(|pod| pod.is_unhealthy()).count()
    }

    /// What could not be read, one per `(cluster, namespace)`.
    #[must_use]
    pub fn errors(&self) -> &[(String, Option<String>, String)] {
        &self.errors
    }

    /// When the last read landed.
    #[must_use]
    pub const fn read_at(&self) -> Option<Instant> {
        self.read_at
    }

    /// Whether anything has come back yet, which tells an empty cluster from
    /// one nobody has heard from.
    #[must_use]
    pub const fn has_read(&self) -> bool {
        self.reads_seen > 0
    }

    /// Whether anything a person is waiting on is in flight, which is what
    /// makes a spinner turn. The pod list reads itself on a cadence, so it
    /// does not count.
    #[must_use]
    pub const fn busy(&self) -> bool {
        self.describe_pending.is_some() || self.deleting.is_some()
    }

    /// The pod the confirmation is about, which is what the modal names and
    /// whose owner it says will put a new one up.
    #[must_use]
    pub fn restarting_pod(&self) -> Option<&Pod> {
        let key = self.restarting.as_ref()?;
        self.pods.iter().find(|pod| pod.key == *key)
    }

    /// What the delete said. A refusal is the user's to see; a delete that
    /// went through is news, because the pod it names is on its way out and
    /// another is on its way in.
    pub fn delete_answered(&mut self, shell: &mut Shell, key: &PodKey, error: Option<String>) {
        if self.deleting.as_ref() == Some(key) {
            self.deleting = None;
        }
        match error {
            Some(message) => {
                shell.set_error(format!("Could not restart {}: {message}", key.name));
            }
            None => {
                let owner = self
                    .pods
                    .iter()
                    .find(|pod| pod.key == *key)
                    .and_then(|pod| pod.owner.as_ref())
                    .map(|(kind, name)| format!("; {kind} {name} is putting a new one up"))
                    .unwrap_or_default();
                shell.set_news(format!("Deleted {}{owner}", key.name));
            }
        }
    }

    /// Sorts by one column, turning the direction around when it is already
    /// the one sorted by, the way every other table does.
    pub fn toggle_sort(&mut self, key: &str) {
        if let Some(column) = PodColumn::from_key(key) {
            let (current, descending) = self.sort;
            self.sort = (column, if current == column { !descending } else { true });
        }
    }

    /// This tab's slice of the context file: what is configured, what the
    /// cursor is on, and what could not be read.
    #[must_use]
    pub fn agent_context(&self, shell: &Shell) -> crate::agent_context::AksContext {
        let rows = self.visible_pods(shell);
        crate::agent_context::AksContext {
            clusters: self
                .clusters
                .iter()
                .map(|cluster| cluster.name.clone())
                .collect(),
            selected: rows.get(self.cursor.index).map(pod_context),
            visible_rows: rows.len(),
            unhealthy: self.unhealthy_count(),
            following_log: self.log_target.as_ref().map(|target| {
                crate::agent_context::FollowingPodLogContext {
                    pod: target.key.name.clone(),
                    container: target.container.clone(),
                    previous: target.previous,
                    line_count: self.log_lines.len(),
                    following: self.log_follow,
                }
            }),
            errors: self
                .errors
                .iter()
                .map(|(cluster, namespace, message)| {
                    format!(
                        "{}: {message}",
                        where_it_failed(cluster, namespace.as_deref())
                    )
                })
                .collect(),
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.mode = AksMode::Browse,
            _ => {
                self.query.handle_key(key);
                self.cursor.reset();
            }
        }
        AppAction::None
    }

    fn handle_command_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        command_for_key(key, TabId::Aks).map_or(AppAction::None, |id| self.run_command(shell, id))
    }

    /// One command, whether a key, a chip in the details pane, or the palette
    /// asked for it.
    pub fn run_command(&mut self, shell: &mut Shell, id: CommandId) -> AppAction {
        match id {
            CommandId::Search => self.mode = AksMode::Search,
            // A pod has no page anywhere, so the key that opens one says what
            // does get you inside it.
            CommandId::Open => {
                shell.set_status("A pod has no page to open; s opens a shell in it");
            }
            // The sync key reads the clusters again rather than pulling from
            // Azure DevOps: nothing on this tab comes from there.
            CommandId::Sync => {
                self.refresh_pending = true;
                shell.set_status("Reading pods\u{2026}");
                return AppAction::Aks(AksRequest::Refresh);
            }
            // The text pane's two halves, and what the log follows.
            CommandId::ShowLogs => {
                self.show_pane(PaneText::Log);
                shell.focus = Focus::Details;
            }
            CommandId::DescribePod => {
                let Some(row) = self.selected_pod(shell) else {
                    shell.set_status("No pod is selected");
                    return AppAction::None;
                };
                let key = row.pod.key.clone();
                self.describe_pending = Some(key.clone());
                self.show_pane(PaneText::Describe);
                return AppAction::Aks(AksRequest::Describe(key));
            }
            CommandId::PreviousLogs => self.toggle_previous(shell),
            CommandId::NextContainer => self.next_container(shell),
            // `x` twice: the first opens the confirmation, the second is the
            // confirmation's own answer. Nothing else sends the delete.
            CommandId::RestartPod => {
                if self.mode == AksMode::ConfirmRestart {
                    return self.confirm_restart(shell);
                }
                self.confirm_restart_prompt(shell);
            }
            CommandId::ExecShell => return self.exec_shell(shell),
            CommandId::OpenRepo => return self.open_repo(shell),
            CommandId::CopyId => {
                let Some(row) = self.selected_pod(shell) else {
                    shell.set_error("No pod is selected");
                    return AppAction::None;
                };
                return AppAction::Copy {
                    text: format!("{}/{}", row.pod.key.namespace, row.pod.key.name),
                    content: CopiedContent::Id,
                };
            }
            CommandId::HistoryBack => return AppAction::HistoryBack,
            CommandId::HistoryForward => return AppAction::HistoryForward,
            CommandId::Quit => shell.should_quit = true,
            // The panes are the shell's: every tab shows the same two and
            // arranges them the same way.
            CommandId::ToggleDetails => shell.toggle_narrow_details(),
            CommandId::ResetPaneSplit => shell.reset_pane_split(),
            _ => {}
        }
        AppAction::None
    }

    /// The first `x`: asks, rather than deleting. A pod nothing put there is
    /// refused outright — deleting it would take it away for good rather than
    /// restart it.
    fn confirm_restart_prompt(&mut self, shell: &mut Shell) {
        let Some(row) = self.selected_pod(shell) else {
            shell.set_error("No pod is selected");
            return;
        };
        if !row.pod.restartable() {
            shell.set_error(format!(
                "{} has no controller to put it back; deleting it would not restart it",
                row.pod.key.name
            ));
            return;
        }
        self.restarting = Some(row.pod.key.clone());
        self.mode = AksMode::ConfirmRestart;
    }

    /// The second `x`, or the modal's own chip: the one place the delete is
    /// sent from.
    fn confirm_restart(&mut self, shell: &mut Shell) -> AppAction {
        self.mode = AksMode::Browse;
        let Some(key) = self.restarting.take() else {
            return AppAction::None;
        };
        shell.set_status(format!("Restarting {}\u{2026}", key.name));
        self.deleting = Some(key.clone());
        AppAction::Aks(AksRequest::Delete(key))
    }

    /// The confirmation's keys: `x` again means it, anything else does not.
    fn handle_restart_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        if key.code == KeyCode::Char('x') {
            return self.run_command(shell, CommandId::RestartPod);
        }
        self.close_overlay(shell);
        AppAction::None
    }

    /// `s`: the terminal goes to `kubectl exec` on the pod under the cursor,
    /// on the container the log is following.
    fn exec_shell(&mut self, shell: &mut Shell) -> AppAction {
        let Some(row) = self.selected_pod(shell) else {
            shell.set_error("No pod is selected");
            return AppAction::None;
        };
        let Some(cluster) = self
            .clusters
            .iter()
            .find(|cluster| cluster.name == row.pod.key.cluster)
        else {
            shell.set_error(format!(
                "cluster {} is no longer in config.toml",
                row.pod.key.cluster
            ));
            return AppAction::None;
        };
        AppAction::ExecShell {
            context: cluster.context.clone(),
            key: row.pod.key.clone(),
            container: self.container.clone(),
        }
    }

    /// `g`: the Repos tab, on the repository the pod's image or app label
    /// names. A pod nothing on file matches says what it did offer.
    fn open_repo(&mut self, shell: &mut Shell) -> AppAction {
        let Some(row) = self.selected_pod(shell) else {
            shell.set_error("No pod is selected");
            return AppAction::None;
        };
        if let Some(repo) = row.repo.clone() {
            return AppAction::Follow(Jump::Repo(repo));
        }
        let candidates = row.pod.repo_candidates();
        shell.set_error(if candidates.is_empty() {
            format!("{} names no repository", row.pod.key.name)
        } else {
            format!("No repository on file is called {}", candidates.join(", "))
        });
        AppAction::None
    }
}

/// Where one failed read was: `qa/orders`, or `qa` when every namespace was
/// asked for at once.
pub(crate) fn where_it_failed(cluster: &str, namespace: Option<&str>) -> String {
    namespace.map_or_else(
        || cluster.to_owned(),
        |namespace| format!("{cluster}/{namespace}"),
    )
}

/// The repositories on file, by name, which is what a pod's image or app label
/// is matched against.
fn repo_names(shell: &Shell) -> Vec<String> {
    shell.repos().iter().map(|repo| repo.name.clone()).collect()
}

/// One pod as an agent reads it.
fn pod_context(row: &PodRow) -> crate::agent_context::PodContext {
    crate::agent_context::PodContext {
        cluster: row.pod.key.cluster.clone(),
        namespace: row.pod.key.namespace.clone(),
        name: row.pod.key.name.clone(),
        status: row.pod.status.clone(),
        ready: row.pod.ready_label(),
        restarts: row.pod.restarts,
        node: row.pod.node.clone(),
        owner: row
            .pod
            .owner
            .as_ref()
            .map(|(kind, name)| format!("{kind}/{name}")),
        created_at: row.pod.created.map(crate::timestamp::Timestamp::to_rfc3339),
        containers: row
            .pod
            .containers
            .iter()
            .map(|container| crate::agent_context::ContainerContext {
                name: container.name.clone(),
                image: container.image.clone(),
                state: container.state.clone(),
                restarts: container.restarts,
            })
            .collect(),
        repo: row.repo.clone(),
    }
}

impl Screen for AksScreen {
    fn handle_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match self.mode {
            AksMode::Search => return self.handle_search_key(key),
            AksMode::ConfirmRestart => return self.handle_restart_key(shell, key),
            AksMode::Browse => {}
        }
        // With the details pane in hand the keys are the text pane's: it is
        // the only thing in there to move around in.
        if shell.focus == Focus::Details {
            match key.code {
                KeyCode::Char('l') => {
                    self.toggle_log_pane();
                    return AppAction::None;
                }
                KeyCode::End => {
                    self.follow_log(true);
                    return AppAction::None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.pane_scroll.scroll_by(1);
                    self.follow_log(false);
                    return AppAction::None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.pane_scroll.scroll_by(-1);
                    self.follow_log(false);
                    return AppAction::None;
                }
                KeyCode::PageUp => {
                    self.pane_scroll.scroll_by(-10);
                    self.follow_log(false);
                    return AppAction::None;
                }
                // Downwards by the page is towards the tail, which is where
                // following already has it: it keeps following.
                KeyCode::PageDown => {
                    self.pane_scroll.scroll_by(10);
                    return AppAction::None;
                }
                KeyCode::Tab | KeyCode::Esc => {
                    shell.focus_list();
                    return AppAction::None;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Tab => shell.toggle_focus(),
            KeyCode::Down | KeyCode::Char('j') => {
                let count = self.row_count(shell);
                self.cursor.move_by(1, count);
                self.sync_focus(shell);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let count = self.row_count(shell);
                self.cursor.move_by(-1, count);
                self.sync_focus(shell);
            }
            KeyCode::PageDown => {
                let count = self.row_count(shell);
                self.cursor.page(1, count);
                self.sync_focus(shell);
            }
            KeyCode::PageUp => {
                let count = self.row_count(shell);
                self.cursor.page(-1, count);
                self.sync_focus(shell);
            }
            KeyCode::Home => {
                self.cursor.focus(0);
                self.sync_focus(shell);
            }
            KeyCode::End => {
                let count = self.row_count(shell);
                self.cursor.move_by(isize::MAX, count);
                self.sync_focus(shell);
            }
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.cursor.reset();
                self.sync_focus(shell);
            }
            _ => return self.handle_command_key(shell, key),
        }
        AppAction::None
    }

    fn handle_paste(&mut self, _shell: &mut Shell, pasted: &str) {
        if self.mode == AksMode::Search {
            self.query.paste(pasted, true);
        }
    }

    fn activate_target(
        &mut self,
        shell: &mut Shell,
        target: PointerTarget,
        column: u16,
        row: u16,
    ) -> AppAction {
        match target {
            PointerTarget::TableRow { index } | PointerTarget::ToggleRowSelect { index } => {
                if index < self.row_count(shell) {
                    self.cursor.focus(index);
                    self.sync_focus(shell);
                }
                shell.focus = Focus::Tickets;
            }
            PointerTarget::FocusDetails => shell.focus = Focus::Details,
            // A click on one of the pod's containers is what the log follows.
            PointerTarget::TreeRow { index } => {
                shell.focus = Focus::Details;
                if let Some(row) = self.selected_pod(shell)
                    && let Some(container) = row.pod.containers.get(index)
                {
                    self.container = Some(container.name.clone());
                    self.sync_focus(shell);
                }
            }
            PointerTarget::SortHeader(key) => self.toggle_sort(key),
            // The repository line in the details pane is the shell's to follow.
            PointerTarget::Follow(jump) => return AppAction::Follow(jump),
            // The details pane's chips stand for the keys they name.
            PointerTarget::RunCommand(id) => return self.run_command(shell, id),
            PointerTarget::CloseOverlay | PointerTarget::DismissOverlay => {
                self.close_overlay(shell);
            }
            PointerTarget::SearchField => {
                self.mode = AksMode::Search;
                self.place_caret(shell, TextEditor::Search, column, row);
            }
            _ => {}
        }
        AppAction::None
    }

    fn place_caret(&mut self, _shell: &mut Shell, editor: TextEditor, column: u16, _row: u16) {
        if editor == TextEditor::Search {
            self.query.set_cursor(usize::from(column));
        }
    }

    fn close_overlay(&mut self, _shell: &mut Shell) {
        self.mode = AksMode::Browse;
        self.restarting = None;
    }

    fn active_editor(&self) -> Option<TextEditor> {
        (self.mode == AksMode::Search).then_some(TextEditor::Search)
    }

    fn scroll_state(&self, surface: ScrollSurface) -> ScrollState {
        match surface {
            ScrollSurface::Details => self.pane_scroll,
            _ => self.cursor.scroll,
        }
    }

    /// Scrolling the log by hand is what takes it out of follow mode, wherever
    /// the scroll came from — a key, the wheel, or the scrollbar thumb.
    fn scroll_state_mut(&mut self, surface: ScrollSurface) -> &mut ScrollState {
        match surface {
            ScrollSurface::Details => {
                self.log_follow = false;
                &mut self.pane_scroll
            }
            _ => &mut self.cursor.scroll,
        }
    }

    fn columns(&self) -> &dyn ColumnLayout {
        &self.layout
    }

    fn columns_mut(&mut self) -> &mut dyn ColumnLayout {
        &mut self.layout
    }

    /// `✗3` while three pods are in trouble.
    fn badge(&self) -> Option<String> {
        let unhealthy = self.unhealthy_count();
        (unhealthy > 0).then(|| format!("\u{2717}{unhealthy}"))
    }

    fn snapshot(&self) -> TabSession {
        TabSession {
            query: self.query.text().to_owned(),
            sort_field: self.sort.0.key().to_owned(),
            columns: self.layout.to_session_columns(),
            ..TabSession::default()
        }
    }

    fn restore(&mut self, _shell: &mut Shell, session: TabSession) {
        self.query = TextInput::new(session.query);
        if let Some(column) = PodColumn::from_key(&session.sort_field) {
            self.sort = (column, self.sort.1);
        }
        self.layout = TableLayout::from_session_columns(&session.columns);
    }

    fn footer_hint(&self, _shell: &Shell) -> &str {
        match self.mode {
            AksMode::Search => "←→ cursor  Ctrl-W delete word  Ctrl-U clear  Enter/Esc finish",
            AksMode::ConfirmRestart => "x restart it  Esc leave it",
            AksMode::Browse => {
                "↑↓/jk move  L logs  D describe  x restart  s shell  g repo  / search  ? help"
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
        crate::ui::aks::render(frame, self, shell, area);
    }
}
