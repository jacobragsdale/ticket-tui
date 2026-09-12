//! `w`: work with a coding agent on the selected work item. The screen
//! settles what it can without asking — the repository the work item is
//! linked to, the workspace the file routes it to, the default provider —
//! asks through one small picker for whatever it cannot, and hands the
//! finished plan to the agent thread. A work item with a live agent goes
//! back to it instead. Nothing is sent blind: the thread prepares the
//! prompt first and it opens here, in the prompt editor, for the user to
//! read and change before `Ctrl-S` launches — or copies — it.

use super::pickers::fuzzy_contains;
use super::*;
use crate::agents::{
    AgentEvent, AgentRequest, AgentSession, Invocation, LaunchPlan, Provider, RelatedBrief,
    RepoBrief, TicketBrief, repository_links,
};
use crate::text_input::wrap_with_cursor;

/// What the picker is asking for.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AgentChoice {
    #[default]
    Repo,
    Workspace,
    Provider,
}

/// The one picker every launch question goes through: a list of names under
/// a filter, and which question it is answering.
#[derive(Clone, Debug, Default)]
pub struct AgentPicker {
    pub choice: AgentChoice,
    pub items: Vec<String>,
    pub query: TextInput,
    pub cursor: ListCursor,
}

/// A launch being put together: what is settled, and what is still to ask.
#[derive(Clone, Debug, Default)]
pub struct AgentFlow {
    pub key: Option<TicketKey>,
    pub repo: Option<Repo>,
    pub workspace: Option<String>,
    pub provider: Option<Provider>,
    pub force_new: bool,
    pub copy_only: bool,
}

/// The prompt editor: the plan a launch (or a copy) is waiting on, and the
/// prompt as it stands. `generated` is what ticket-tui wrote, which `Ctrl-R`
/// brings back; the text differing from it is what the title calls edited.
#[derive(Clone, Debug)]
pub struct HandoffEditor {
    pub plan: Box<LaunchPlan>,
    pub copy_only: bool,
    pub input: TextInput,
    pub generated: String,
    /// The width the rows were last wrapped to, which is what `↑`/`↓` move
    /// by. Zero until the modal has drawn once.
    pub width: u16,
    /// Whether the next frame scrolls to the caret: set by every key the
    /// editor takes, cleared by the frame that obeyed it, so the wheel can
    /// scroll away from it in between.
    pub follow_cursor: bool,
}

impl HandoffEditor {
    /// Whether the text differs from what ticket-tui generated.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.input.text() != self.generated
    }

    /// The draft's key: the work item and the repository the prompt names.
    #[must_use]
    pub fn draft_key(&self) -> (TicketKey, String) {
        (
            TicketKey {
                organization: self.plan.ticket.organization.clone(),
                id: self.plan.ticket.id,
            },
            self.plan.repo.id.clone(),
        )
    }

    /// What the modal is titled: whom the prompt is for, and whether it has
    /// been changed.
    #[must_use]
    pub fn title(&self) -> String {
        let edited = if self.is_dirty() {
            " \u{00b7} edited"
        } else {
            ""
        };
        if self.copy_only {
            format!(
                " Prompt to copy for #{} \u{00b7} {}{edited} ",
                self.plan.ticket.id, self.plan.repo.name
            )
        } else {
            format!(
                " Prompt for {} on #{} \u{00b7} {}{edited} ",
                self.plan.provider.label(),
                self.plan.ticket.id,
                self.plan.repo.name
            )
        }
    }

    /// What the footer says while the editor is open.
    #[must_use]
    pub const fn hint(&self) -> &'static str {
        if self.copy_only {
            "Enter newline  Ctrl-S copy  Ctrl-R regenerate  Esc keep draft  Ctrl-U clear"
        } else {
            "Enter newline  Ctrl-S launch  Ctrl-R regenerate  Esc keep draft  Ctrl-U clear"
        }
    }
}

/// Whether a launch of `repo` clones it first: there is a workspace to
/// clone into, no verified clone is in it, and config.toml names no path
/// for it either.
#[must_use]
pub fn repo_needs_clone(shell: &Shell, repo: &Repo) -> bool {
    shell.workspace().is_some()
        && !shell.has_clone(&repo.id)
        && shell.agent_settings.herdr.path_for(&repo.name).is_none()
}

impl WorkItemsScreen {
    /// The agent sessions on file, as the thread last reported them.
    #[must_use]
    pub fn agent_sessions(&self) -> &[AgentSession] {
        &self.agent_sessions
    }

    /// The live agent on one work item, if the thread knows of one.
    #[must_use]
    pub fn live_agent_for(&self, key: &TicketKey) -> Option<&AgentSession> {
        self.agent_sessions
            .iter()
            .rev()
            .find(|session| session.is_complete() && session.ticket_key() == *key)
    }

    /// The prompt the latest agent on one work item was given, live or
    /// not: a launch that failed after its handoff still has one.
    #[must_use]
    pub fn agent_prompt_for(&self, key: &TicketKey) -> Option<&AgentSession> {
        self.agent_sessions
            .iter()
            .rev()
            .find(|session| session.ticket_key() == *key && session.prompt.is_some())
    }

    /// Show agent prompt: the stored opening prompt of the selected work
    /// item's agent, in the help's scrolling box.
    pub(super) fn show_agent_prompt(&mut self, shell: &mut Shell) -> AppAction {
        let Some(ticket) = self.selected_ticket() else {
            shell.set_error("No work item is selected");
            return AppAction::None;
        };
        if self.agent_prompt_for(&ticket.key).is_none() {
            shell.set_error(format!(
                "No agent prompt on file for #{}; w launches an agent with one",
                ticket.key.id
            ));
            return AppAction::None;
        }
        self.help.scroll_to(0);
        self.mode = WorkItemMode::AgentPrompt;
        AppAction::None
    }

    /// What the agent thread is doing for this screen, while it is.
    #[must_use]
    pub fn agent_busy(&self) -> Option<&str> {
        self.agent_pending.as_deref()
    }

    /// `w`, and the two palette verbs beside it. `force_new` asks for a
    /// provider and starts beside a live agent rather than returning to it;
    /// `copy_only` writes the handoff and puts the prompt on the clipboard.
    pub(super) fn work_with_agent(
        &mut self,
        shell: &mut Shell,
        force_new: bool,
        copy_only: bool,
    ) -> AppAction {
        let Some(ticket) = self.selected_ticket() else {
            shell.set_error("No work item is selected");
            return AppAction::None;
        };
        let key = ticket.key.clone();
        if !copy_only
            && !force_new
            && let Some(session) = self.live_agent_for(&key)
        {
            shell.set_status(format!(
                "Returning to the {} agent on #{}\u{2026}",
                session.provider.label(),
                key.id
            ));
            return AppAction::Agent(AgentRequest::Return(session.id.clone()));
        }
        if !copy_only && !shell.inside_herdr {
            shell.set_error(
                "Not inside Herdr: start ticket-tui in a Herdr pane to launch an agent, or use Copy agent prompt",
            );
            return AppAction::None;
        }
        if let Some(busy) = &self.agent_pending {
            shell.set_error(format!("Wait for the agent thread: {busy}"));
            return AppAction::None;
        }
        self.agent_flow = AgentFlow {
            key: Some(key),
            force_new,
            copy_only,
            ..AgentFlow::default()
        };
        self.continue_agent_flow(shell)
    }

    /// Settles the next open question of the flow, asks it through the
    /// picker when it cannot, and hands the plan over once none is left.
    fn continue_agent_flow(&mut self, shell: &mut Shell) -> AppAction {
        let Some(key) = self.agent_flow.key.clone() else {
            return AppAction::None;
        };
        if self.agent_flow.repo.is_none() {
            let enabled: Vec<Repo> = shell
                .repos()
                .iter()
                .filter(|repo| !repo.is_disabled)
                .cloned()
                .collect();
            if enabled.is_empty() {
                shell.set_error("No repository is on file; sync first");
                return AppAction::None;
            }
            let linked = self.linked_repo_ids(&key);
            let candidates: Vec<Repo> = enabled
                .iter()
                .filter(|repo| linked.contains(&repo.id))
                .cloned()
                .collect();
            match candidates.as_slice() {
                [one] => self.agent_flow.repo = Some(one.clone()),
                [] => {
                    self.open_agent_picker(
                        AgentChoice::Repo,
                        enabled.iter().map(|repo| repo.name.clone()).collect(),
                    );
                    return AppAction::None;
                }
                several => {
                    self.open_agent_picker(
                        AgentChoice::Repo,
                        several.iter().map(|repo| repo.name.clone()).collect(),
                    );
                    return AppAction::None;
                }
            }
        }
        let repo = self.agent_flow.repo.clone().expect("settled above");
        if !self.agent_flow.copy_only && self.agent_flow.workspace.is_none() {
            match shell.agent_settings.herdr.workspace_for(&repo.name) {
                Some(name) => self.agent_flow.workspace = Some(name.to_owned()),
                None => {
                    self.open_agent_picker(
                        AgentChoice::Workspace,
                        shell.agent_settings.herdr.workspace_names(),
                    );
                    // The live workspaces join the list when Herdr answers.
                    return AppAction::Agent(AgentRequest::Workspaces);
                }
            }
        }
        if self.agent_flow.provider.is_none() {
            if self.agent_flow.force_new && !self.agent_flow.copy_only {
                self.open_agent_picker(
                    AgentChoice::Provider,
                    Provider::ALL
                        .iter()
                        .map(|provider| provider.kind().to_owned())
                        .collect(),
                );
                return AppAction::None;
            }
            self.agent_flow.provider = Some(shell.agent_settings.default);
        }
        let Some(plan) = self.launch_plan(shell) else {
            return AppAction::None;
        };
        let copy_only = self.agent_flow.copy_only;
        let id = plan.ticket.id;
        // The prompt comes back to be seen before anything is launched or
        // copied; a clone, when one is needed, happens on the way.
        let status = if let Some(root) = repo_needs_clone(shell, &repo)
            .then(|| shell.workspace())
            .flatten()
        {
            format!(
                "Cloning {} into {}, then preparing the prompt for #{id}\u{2026}",
                repo.name,
                root.display()
            )
        } else {
            format!("Preparing the prompt for #{id}\u{2026}")
        };
        shell.set_status(status.clone());
        self.agent_pending = Some(status);
        self.agent_flow = AgentFlow::default();
        AppAction::Agent(AgentRequest::Prepare {
            plan: Box::new(plan),
            copy_only,
        })
    }

    /// Opens the prompt editor on what the thread prepared: a draft kept
    /// for the same work item and repository first, else the prompt
    /// offered. Whatever overlay is open closes the way `Esc` closes it,
    /// its draft kept. The caret starts at the end, where a note goes; the
    /// view starts at the top, where the reading does.
    fn open_handoff(
        &mut self,
        shell: &mut Shell,
        plan: Box<LaunchPlan>,
        prompt: String,
        generated: String,
        copy_only: bool,
    ) {
        if self.mode != WorkItemMode::Browse {
            self.close_overlay(shell);
        }
        let key = (
            TicketKey {
                organization: plan.ticket.organization.clone(),
                id: plan.ticket.id,
            },
            plan.repo.id.clone(),
        );
        let text = self.handoff_drafts.get(&key).cloned().unwrap_or(prompt);
        self.handoff = Some(HandoffEditor {
            plan,
            copy_only,
            input: TextInput::new(text),
            generated,
            width: 0,
            follow_cursor: false,
        });
        self.help.scroll_to(0);
        self.mode = WorkItemMode::Handoff;
    }

    pub(super) fn handle_handoff_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        let page = self.help.viewport.max(1);
        match key.code {
            KeyCode::Esc => self.close_handoff(shell),
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.send_handoff(shell);
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(editor) = self.handoff.as_mut() {
                    editor.input = TextInput::new(editor.generated.clone());
                    editor.input.set_cursor(0);
                    editor.follow_cursor = true;
                    shell.set_status("The generated prompt is back");
                }
            }
            // Nowhere to move focus to inside one box.
            KeyCode::Tab => {}
            _ => {
                if let Some(editor) = self.handoff.as_mut() {
                    let width = usize::from(editor.width);
                    match key.code {
                        KeyCode::Enter => editor.input.insert_newline(),
                        KeyCode::Up => editor.input.move_up(width),
                        KeyCode::Down => editor.input.move_down(width),
                        // A screenful moves the caret rather than the view,
                        // so the next keystroke does not yank the view back.
                        KeyCode::PageUp => (0..page).for_each(|_| editor.input.move_up(width)),
                        KeyCode::PageDown => {
                            (0..page).for_each(|_| editor.input.move_down(width));
                        }
                        _ => {
                            editor.input.handle_key(key);
                        }
                    }
                    editor.follow_cursor = true;
                }
            }
        }
        AppAction::None
    }

    /// Closes the editor without sending. An edited prompt is kept as a
    /// draft for the next `w` on the same work item and repository; one
    /// put back to the generated text drops any draft held for it.
    pub(super) fn close_handoff(&mut self, shell: &mut Shell) {
        if let Some(editor) = self.handoff.take() {
            if editor.is_dirty() {
                shell.set_status(format!(
                    "Prompt draft kept on #{} \u{2014} w brings it back",
                    editor.plan.ticket.id
                ));
                self.handoff_drafts
                    .insert(editor.draft_key(), editor.input.text().to_owned());
            } else {
                self.handoff_drafts.remove(&editor.draft_key());
            }
        }
        self.mode = WorkItemMode::Browse;
    }

    /// `Ctrl-S`, and the primary button: sends the prompt as it stands — the
    /// launch, or the copy. An empty one is refused with the editor left
    /// open. The text is kept as a draft until the thread says it landed,
    /// so a launch that fails at any stage gives it back.
    pub(super) fn send_handoff(&mut self, shell: &mut Shell) -> AppAction {
        let Some(editor) = self.handoff.take() else {
            self.mode = WorkItemMode::Browse;
            return AppAction::None;
        };
        let text = editor.input.text().to_owned();
        if text.trim().is_empty() {
            shell.set_error("The prompt is empty; Ctrl-R brings the generated one back");
            self.handoff = Some(editor);
            return AppAction::None;
        }
        self.mode = WorkItemMode::Browse;
        self.handoff_drafts.insert(editor.draft_key(), text.clone());
        let copy_only = editor.copy_only;
        let mut plan = editor.plan;
        plan.prompt = Some(text);
        let id = plan.ticket.id;
        let status = if copy_only {
            format!("Copying the prompt for #{id}\u{2026}")
        } else {
            format!(
                "Launching {} on #{id} in {}\u{2026}",
                plan.provider.label(),
                plan.workspace
            )
        };
        shell.set_status(status.clone());
        self.agent_pending = Some(status);
        AppAction::Agent(if copy_only {
            AgentRequest::Prompt(plan)
        } else {
            AgentRequest::Launch(plan)
        })
    }

    /// Puts the editor's caret where a click landed on one of its rows:
    /// `row` is the wrapped row, `column` the cell along it.
    pub(super) fn place_handoff_caret(&mut self, row: usize, column: u16) {
        let Some(editor) = self.handoff.as_mut() else {
            return;
        };
        let layout = wrap_with_cursor(
            editor.input.text(),
            editor.input.cursor(),
            usize::from(editor.width),
        );
        if let Some((start, text)) = layout.rows.get(row) {
            editor
                .input
                .set_cursor(start + usize::from(column).min(text.chars().count()));
            editor.follow_cursor = true;
        }
    }

    fn linked_repo_ids(&self, key: &TicketKey) -> Vec<String> {
        let artifacts = self.artifacts_for(key);
        crate::agents::linked_repositories(&artifacts.iter().collect::<Vec<_>>())
    }

    fn open_agent_picker(&mut self, choice: AgentChoice, items: Vec<String>) {
        self.agent_picker = AgentPicker {
            choice,
            items,
            ..AgentPicker::default()
        };
        self.mode = WorkItemMode::AgentPicker;
    }

    /// What the picker lists under its query: the items that match, an exact
    /// match first. For a workspace, a typed name no workspace has is offered
    /// as itself, since `Enter` on it makes the workspace.
    #[must_use]
    pub fn agent_matches(&self) -> Vec<String> {
        let picker = &self.agent_picker;
        let query = picker.query.text().trim();
        let mut matches: Vec<String> = picker
            .items
            .iter()
            .filter(|item| query.is_empty() || fuzzy_contains(item, query))
            .cloned()
            .collect();
        matches.sort_by_key(|item| !item.eq_ignore_ascii_case(query));
        if picker.choice == AgentChoice::Workspace
            && !query.is_empty()
            && !matches.iter().any(|item| item.eq_ignore_ascii_case(query))
        {
            matches.push(query.to_owned());
        }
        matches
    }

    pub(super) fn handle_agent_picker_key(
        &mut self,
        shell: &mut Shell,
        key: KeyEvent,
    ) -> AppAction {
        match key.code {
            KeyCode::Esc => {
                self.mode = WorkItemMode::Browse;
                self.agent_flow = AgentFlow::default();
            }
            KeyCode::Up => self.move_agent_selection(-1),
            KeyCode::Down => self.move_agent_selection(1),
            KeyCode::PageUp => self.move_agent_selection(-5),
            KeyCode::PageDown => self.move_agent_selection(5),
            KeyCode::Enter => {
                return self.choose_agent_option(shell, self.agent_picker.cursor.index, false);
            }
            // Ctrl-S on a workspace: use it, and write the routing to
            // config.toml so it is not asked again.
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.choose_agent_option(shell, self.agent_picker.cursor.index, true);
            }
            _ => {
                let before = self.agent_picker.query.text().to_owned();
                self.agent_picker.query.handle_key(key);
                if self.agent_picker.query.text() != before {
                    self.agent_picker.cursor.reset();
                }
            }
        }
        AppAction::None
    }

    fn move_agent_selection(&mut self, delta: isize) {
        let count = self.agent_matches().len();
        self.agent_picker.cursor.move_by(delta, count);
    }

    /// `Enter` on one row of the picker, which answers the question it was
    /// asking and carries the flow on.
    pub(super) fn choose_agent_option(
        &mut self,
        shell: &mut Shell,
        index: usize,
        remember: bool,
    ) -> AppAction {
        let Some(chosen) = self.agent_matches().get(index).cloned() else {
            return AppAction::None;
        };
        match self.agent_picker.choice {
            AgentChoice::Repo => {
                let Some(repo) = shell
                    .repos()
                    .iter()
                    .find(|repo| repo.name == chosen)
                    .cloned()
                else {
                    return AppAction::None;
                };
                self.agent_flow.repo = Some(repo);
            }
            AgentChoice::Workspace => {
                if remember && let Some(repo) = self.agent_flow.repo.clone() {
                    let path = shell.config_path.clone();
                    match crate::config::remember_workspace_in(&path, &chosen, &repo.name) {
                        Ok(()) => {
                            shell.agent_settings.herdr.workspaces.push(
                                crate::config::HerdrWorkspace {
                                    name: chosen.clone(),
                                    repos: vec![repo.name.clone()],
                                },
                            );
                            shell.set_news(format!(
                                "Remembered {} \u{2192} {chosen} in {}",
                                repo.name,
                                path.display()
                            ));
                        }
                        Err(error) => shell.set_error(format!(
                            "Could not remember the routing: {error:#}; launching anyway"
                        )),
                    }
                }
                self.agent_flow.workspace = Some(chosen);
            }
            AgentChoice::Provider => {
                self.agent_flow.provider = Provider::parse(&chosen);
            }
        }
        self.mode = WorkItemMode::Browse;
        self.continue_agent_flow(shell)
    }

    /// Everything the agent thread needs, off the rows this screen holds.
    fn launch_plan(&self, shell: &Shell) -> Option<LaunchPlan> {
        let flow = &self.agent_flow;
        let key = flow.key.clone()?;
        let ticket = self.ticket_by_key(&key)?;
        let repo = flow.repo.clone()?;
        let provider = flow.provider.unwrap_or(shell.agent_settings.default);
        let target = shell.sync_target.as_ref()?;
        let artifacts = self.artifacts_for(&key);
        let (linked_branch, pull_request) =
            repository_links(&artifacts.iter().collect::<Vec<_>>(), &repo.id, &|id| {
                let (_, status) = shell.pull_request_label(id)?;
                let (source, url) = shell.pull_request_source(id)?;
                Some((status, source.to_owned(), url.to_owned()))
            });
        let brief = TicketBrief::from_graph(
            ticket,
            &self.graph,
            &|key| {
                self.ticket_by_key(key).map(|relative| RelatedBrief {
                    id: relative.key.id,
                    work_item_type: relative.work_item_type.clone(),
                    title: relative.title.clone(),
                    state: relative.state.clone(),
                })
            },
            &|repo_id| shell.repo_name(repo_id),
            &|id| {
                shell
                    .pull_request_label(id)
                    .map(|(title, status)| (title.to_owned(), status.as_str().to_owned()))
            },
        );
        Some(LaunchPlan {
            ticket: brief,
            repo: RepoBrief {
                id: repo.id.clone(),
                name: repo.name.clone(),
                remote_url: repo.remote_url.clone(),
                ssh_url: repo.ssh_url.clone(),
                web_url: repo.web_url.clone(),
                default_branch: repo.default_branch.clone(),
            },
            linked_branch,
            pull_request,
            provider,
            args: shell.agent_settings.args_for(provider).to_vec(),
            policy: shell.agent_settings.policy,
            workspace: flow.workspace.clone().unwrap_or_default(),
            workspace_root: shell.workspace().map(std::path::Path::to_path_buf),
            path_override: shell
                .agent_settings
                .herdr
                .path_for(&repo.name)
                .map(std::path::Path::to_path_buf),
            invocation: Invocation {
                database: shell.database_path.clone(),
                organization: target.organization.clone(),
                project: target.project.clone(),
                code_project: target.code_project.clone(),
                binary: std::env::current_exe().ok(),
            },
            handoff_dir: shell
                .database_path
                .parent()
                .map_or_else(|| PathBuf::from("handoffs"), |dir| dir.join("handoffs")),
            force_new: flow.force_new,
            note: None,
            prompt: None,
        })
    }

    /// What the agent thread said. The prompt to copy comes back to the
    /// caller, which owns the clipboard.
    pub fn apply_agent_event(&mut self, shell: &mut Shell, event: AgentEvent) -> Option<String> {
        match event {
            AgentEvent::Sessions(sessions) => self.agent_sessions = sessions,
            AgentEvent::Prepared {
                plan,
                prompt,
                generated,
                copy_only,
            } => {
                self.agent_pending = None;
                self.open_handoff(shell, plan, prompt, generated, copy_only);
            }
            AgentEvent::Launched { session, note } => {
                self.agent_pending = None;
                self.handoff_drafts
                    .remove(&(session.ticket_key(), session.repo_id.clone()));
                shell.set_news(format!(
                    "{} is on #{} in {} \u{203a} {} \u{2014} {note}",
                    session.provider.label(),
                    session.work_item,
                    session.workspace,
                    session.repo_name
                ));
            }
            AgentEvent::Failed {
                work_item,
                stage,
                message,
                session,
            } => {
                self.agent_pending = None;
                let resume = if session.is_some() {
                    " \u{2014} w carries on from there"
                } else {
                    ""
                };
                shell.set_error(format!(
                    "#{work_item}: {} failed: {message}{resume}",
                    stage.label()
                ));
            }
            AgentEvent::Returned { work_item } => {
                shell.set_status(format!("Back to the agent on #{work_item}"));
            }
            AgentEvent::Stale { work_item, reason } => {
                shell.set_error(format!(
                    "The agent on #{work_item} is gone: {reason} \u{2014} w starts another"
                ));
            }
            AgentEvent::Prompt {
                work_item,
                text,
                context,
            } => {
                self.agent_pending = None;
                self.handoff_drafts
                    .retain(|(key, _), _| key.id != work_item);
                shell.set_news(format!(
                    "Prompt for #{work_item} copied; the context is at {}",
                    context.display()
                ));
                return Some(text);
            }
            AgentEvent::Workspaces(names) => {
                if self.mode == WorkItemMode::AgentPicker
                    && self.agent_picker.choice == AgentChoice::Workspace
                {
                    for name in names {
                        if !self
                            .agent_picker
                            .items
                            .iter()
                            .any(|held| held.eq_ignore_ascii_case(&name))
                        {
                            self.agent_picker.items.push(name);
                        }
                    }
                }
            }
            AgentEvent::Stopped => {
                self.agent_pending = None;
                shell.set_error("The agent thread stopped; restart ticket-tui to launch agents");
            }
        }
        None
    }
}
