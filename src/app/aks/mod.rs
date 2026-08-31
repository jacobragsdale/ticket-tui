//! The AKS screen: every pod of every cluster `config.toml` names, in one
//! table, and what the details pane says about the one under the cursor.
//!
//! Nothing here is stored. A pod is read live, the way local git state is, and
//! the next read replaces it — the log tail, describe and the verbs arrive
//! with the tickets that draw them.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::{AppAction, Focus, ListCursor, Screen, Shell, TabId};
use crate::aks::{AksRequest, Cluster, Pod, PodKey, PodRow, PodSchema};
use crate::columns::{ColumnId, ColumnLayout, TableLayout};
use crate::command::{CommandId, command_for_key};
use crate::filter::{MatchContext, ParsedQuery, parse_query};
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
    pub details: ScrollState,
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
            details: ScrollState::default(),
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
        cluster: &str,
        namespace: Option<&str>,
        pods: Result<Vec<Pod>, String>,
    ) -> Option<String> {
        let selected = self.selected_key();
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
        let rows = self.rows(&[]);
        match selected.and_then(|key| rows.iter().position(|row| row.pod.key == key)) {
            Some(index) => self.cursor.focus(index),
            None => self.cursor.clamp(rows.len()),
        }
        toast
    }

    /// The pod the cursor is on, looked up without the shell's repositories:
    /// they name the Repo column and the `repo:` filter, and neither changes
    /// which pod is under the hand.
    fn selected_key(&self) -> Option<PodKey> {
        self.rows(&[])
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

    /// Whether anything is in flight, which is what makes a spinner turn. The
    /// pod list reads itself on a cadence; describe and restart, which a
    /// person waits on, arrive with the tickets that send them.
    #[must_use]
    pub const fn busy(&self) -> bool {
        false
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
            // A pod has no page anywhere; #723 gives `o` something to do.
            CommandId::Open => shell.set_status("A pod has no page to open"),
            // The sync key reads the clusters again rather than pulling from
            // Azure DevOps: nothing on this tab comes from there.
            CommandId::Sync => {
                self.refresh_pending = true;
                shell.set_status("Reading pods\u{2026}");
                return AppAction::Aks(AksRequest::Refresh);
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
        if self.mode == AksMode::Search {
            return self.handle_search_key(key);
        }
        match key.code {
            KeyCode::Tab => shell.toggle_focus(),
            KeyCode::Down | KeyCode::Char('j') => {
                let count = self.row_count(shell);
                self.cursor.move_by(1, count);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let count = self.row_count(shell);
                self.cursor.move_by(-1, count);
            }
            KeyCode::PageDown => {
                let count = self.row_count(shell);
                self.cursor.page(1, count);
            }
            KeyCode::PageUp => {
                let count = self.row_count(shell);
                self.cursor.page(-1, count);
            }
            KeyCode::Home => self.cursor.focus(0),
            KeyCode::End => {
                let count = self.row_count(shell);
                self.cursor.move_by(isize::MAX, count);
            }
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.cursor.reset();
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
                }
                shell.focus = Focus::Tickets;
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
    }

    fn active_editor(&self) -> Option<TextEditor> {
        (self.mode == AksMode::Search).then_some(TextEditor::Search)
    }

    fn scroll_state(&self, surface: ScrollSurface) -> ScrollState {
        match surface {
            ScrollSurface::Details => self.details,
            _ => self.cursor.scroll,
        }
    }

    fn scroll_state_mut(&mut self, surface: ScrollSurface) -> &mut ScrollState {
        match surface {
            ScrollSurface::Details => &mut self.details,
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
            AksMode::Browse => "↑↓/jk move  / search  r refresh  c columns  ? help",
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
        crate::ui::aks::render(frame, self, shell, area);
    }
}
