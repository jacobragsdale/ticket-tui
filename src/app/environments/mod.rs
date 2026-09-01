//! The Environments screen: every service the deployment repository declares,
//! across every environment it declares them for, and what each environment is
//! missing.
//!
//! The four `env` subcommands answer the same questions one at a time; this is
//! the glance. One row per service, one column per `[[environments]]`, each
//! cell the image tag with what that environment would be short of, and the
//! details pane the promotion diff from the column to the left of the cursor
//! into the one under it — which is the question asked at a keyboard,
//! mid-task, about one service: is prod ready for this?
//!
//! Nothing here is stored, and nothing here is on a timer. A render is
//! `kubectl kustomize` over a clone, and a clone only changes when somebody
//! pushes: the overlays are rendered when the tab is opened, when `r` asks,
//! and when a `git pull` on the Repos tab moves the deployment clone.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::{AppAction, Focus, ListCursor, Screen, Shell, TabId};
use crate::columns::{ColumnId, ColumnLayout, TableLayout};
use crate::command::{CommandId, command_for_key};
use crate::config::Environment;
use crate::filter::{MatchContext, ParsedQuery, parse_query};
use crate::kustomize::diff::{self, ImageChange, Names, ServiceDiff};
use crate::kustomize::{EnvManifest, Finding, Missing, VaultNames, Workload, check_with};
use crate::local::LocalRequest;
use crate::model::{Jump, Run};
use crate::pointer::{PointerTarget, ScrollState, ScrollSurface, TextEditor};
use crate::preflight::{Deployment, finding_jump};
use crate::session::TabSession;
use crate::text_input::TextInput;

mod columns;
mod filters;
pub mod rows;
#[cfg(test)]
pub(crate) mod tests;

pub use columns::EnvColumn;
pub use filters::{ServiceField, ServiceSchema};
pub use rows::{EnvCell, ServiceRow};

/// What the board says in place of the pull requests it cannot list. The git
/// history behind an image gap is a `git log` in the service's own clone, and
/// a render pass is no place to shell out: the CLI reads it in full.
const NO_HISTORY: &str = "run `ticket-tui env diff` for the pull requests between them";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EnvMode {
    #[default]
    Browse,
    Search,
}

/// How one line of the details pane reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffLineKind {
    /// A `── Image ──` rule.
    Section,
    /// One thing the promotion would do.
    Entry,
    /// Something the environment under the cursor is missing.
    Missing,
    /// A word about why there is nothing more to say.
    Note,
}

/// One line of the details pane: what it says, how it reads, and where it goes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
    pub jump: Option<Jump>,
}

impl DiffLine {
    fn new(kind: DiffLineKind, text: String) -> Self {
        Self {
            kind,
            text,
            jump: None,
        }
    }

    fn going(kind: DiffLineKind, text: String, jump: Option<Jump>) -> Self {
        Self { kind, text, jump }
    }
}

/// The Environments tab's state: what `config.toml` names, what the overlays
/// have rendered to, and where the two cursors have got to.
pub struct EnvironmentsScreen {
    /// The deployment repository and the environments it declares, as
    /// `config.toml` was last read. `None` when the file names none or there
    /// is no clone of it here, which is what `reason` says.
    deployment: Option<Deployment>,
    /// The one line saying why there is nothing to draw, and where it looked.
    reason: Option<String>,
    /// Each environment as its overlays render it, by name. An environment
    /// that refused is not here at all.
    manifests: Vec<(String, EnvManifest)>,
    /// The overlays out at the local thread, as `(environment, overlay)`.
    pending: Vec<(String, String)>,
    /// Why one environment could not be rendered, the renderer's own line.
    errors: Vec<(String, String)>,
    /// What each environment's vault holds, as the Key Vault tab read it.
    vaults: Vec<VaultNames>,
    /// What every environment asks for that it does not answer.
    findings: Vec<Finding>,
    /// The runs on file, for reading an image tag back to the build that made
    /// it. Handed in with every snapshot, the way the repositories are.
    runs: Vec<Run>,
    /// Whether the overlays are worth rendering again: set when the tab is
    /// first opened, by `r`, and by a pull of the deployment clone.
    stale: bool,
    /// Whether the vaults are worth listing again. `r` sets it: a render as
    /// new as the repository wants a vault listing as new as the vault, and
    /// the listing belongs to the tab that reads vaults.
    stale_vaults: bool,
    pub mode: EnvMode,
    query: TextInput,
    pub layout: TableLayout<EnvColumn>,
    pub cursor: ListCursor,
    /// Which environment column the cursor is on, which is the environment the
    /// details pane promotes *into*.
    column: usize,
    pub details: ScrollState,
    /// Which jumpable line of the details pane `g` and `Enter` follow.
    pub jump_cursor: usize,
}

impl Default for EnvironmentsScreen {
    fn default() -> Self {
        Self {
            deployment: None,
            reason: None,
            manifests: Vec::new(),
            pending: Vec::new(),
            errors: Vec::new(),
            vaults: Vec::new(),
            findings: Vec::new(),
            runs: Vec::new(),
            stale: false,
            stale_vaults: false,
            mode: EnvMode::Browse,
            query: TextInput::default(),
            layout: TableLayout::default(),
            cursor: ListCursor::default(),
            // The right-most environment is the one the question is about:
            // prod is what a promotion is read into.
            column: usize::MAX,
            details: ScrollState::default(),
            jump_cursor: 0,
        }
    }
}

impl EnvironmentsScreen {
    /// What `config.toml` names, as it was last read, and the one line saying
    /// why there is nothing at all when there is nothing. A file naming a
    /// different repository throws away what was rendered from the old one.
    pub fn set_deployment(&mut self, deployment: Option<Deployment>, reason: Option<String>) {
        if self.deployment != deployment {
            self.manifests.clear();
            self.errors.clear();
            self.findings.clear();
            self.pending.clear();
            self.stale = deployment.is_some();
        }
        self.deployment = deployment;
        self.reason = reason;
    }

    /// The runs the last pull left, for the image half of a promotion.
    pub fn set_runs(&mut self, runs: Vec<Run>) {
        self.runs = runs;
    }

    /// What the vaults hold, as whoever reads vaults read them. Names, kinds
    /// and dates: no value reaches here, on the rule the Key Vault tab keeps.
    pub fn set_vaults(&mut self, vaults: Vec<VaultNames>) {
        if self.vaults != vaults {
            self.vaults = vaults;
            self.recheck();
        }
    }

    /// The vaults the environments pull from, so whoever reads vaults knows
    /// which are worth reading for this tab.
    #[must_use]
    pub fn vault_names(&self) -> Vec<String> {
        self.environments()
            .iter()
            .filter_map(|environment| environment.vault.clone())
            .collect()
    }

    /// The environments `config.toml` declares, in the order it lists them,
    /// which is the order of the columns.
    #[must_use]
    pub fn environments(&self) -> &[Environment] {
        self.deployment
            .as_ref()
            .map_or(&[], |deployment| deployment.environments.as_slice())
    }

    /// Why there is nothing to draw, when there is nothing: no `[deployment]`,
    /// no `[[environments]]`, or no clone of the repository on this machine —
    /// in the line that says where it looked, the Repos tab's rule.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.deployment
            .is_none()
            .then_some(self.reason.as_deref())?
    }

    /// Renders every environment again the next time the tab is looked at.
    /// The repository changes when somebody pushes, so a pull of the
    /// deployment clone is the natural moment.
    pub fn invalidate(&mut self) {
        self.stale = self.deployment.is_some();
    }

    /// The vaults to drop and list again, answered once: `r` asks for them,
    /// and the read is whoever reads vaults' to make.
    pub fn take_stale_vaults(&mut self) -> Vec<String> {
        if !std::mem::take(&mut self.stale_vaults) {
            return Vec::new();
        }
        self.vault_names()
    }

    /// The same, when the repository that was pulled is the deployment one.
    pub fn repo_pulled(&mut self, repo: &str) {
        if self
            .deployment
            .as_ref()
            .is_some_and(|deployment| deployment.covers(repo))
        {
            self.invalidate();
        }
    }

    /// One render per overlay of every environment, when the board is looking
    /// at overlays it has not rendered and nothing is already out. Never on a
    /// timer: this is called when the tab is showing.
    pub fn renders_due(&mut self) -> Vec<LocalRequest> {
        let Some(deployment) = self.deployment.clone() else {
            return Vec::new();
        };
        if !self.stale || !self.pending.is_empty() {
            return Vec::new();
        }
        self.stale = false;
        self.manifests.clear();
        self.errors.clear();
        let mut requests = Vec::new();
        for environment in &deployment.environments {
            let overlays: Vec<String> = environment
                .overlays
                .iter()
                .flat_map(|pattern| crate::kustomize::expand(&deployment.clone, pattern))
                .collect();
            if overlays.is_empty() {
                self.errors.push((
                    environment.name.clone(),
                    format!(
                        "no overlay in {} matches {}",
                        deployment.clone.display(),
                        environment.overlays.join(", ")
                    ),
                ));
                continue;
            }
            self.manifests.push((
                environment.name.clone(),
                EnvManifest::named(&environment.name),
            ));
            for overlay in overlays {
                self.pending
                    .push((environment.name.clone(), overlay.clone()));
                requests.push(LocalRequest::Render {
                    environment: environment.name.clone(),
                    clone: deployment.clone.clone(),
                    overlay,
                    command: deployment.render.clone(),
                });
            }
        }
        self.recheck();
        requests
    }

    /// One overlay, rendered — or the one line of the renderer's complaint,
    /// which takes the whole environment out: an environment half rendered
    /// would report everything the missing half defines as missing.
    pub fn set_rendered(
        &mut self,
        environment: &str,
        overlay: &str,
        rendered: Result<String, String>,
    ) {
        self.pending
            .retain(|(held, held_overlay)| held != environment || held_overlay != overlay);
        match rendered {
            Ok(yaml) => {
                let mut parsed = crate::kustomize::parse(environment, &yaml);
                parsed.overlays.push(overlay.to_owned());
                if let Some((_, manifest)) = self
                    .manifests
                    .iter_mut()
                    .find(|(held, _)| held == environment)
                {
                    manifest.absorb(parsed);
                    manifest.tidy();
                }
            }
            Err(message) => {
                self.manifests.retain(|(held, _)| held != environment);
                self.errors.retain(|(held, _)| held != environment);
                self.errors.push((environment.to_owned(), message));
            }
        }
        self.recheck();
    }

    /// Whether a render is out, which is what makes a spinner turn.
    #[must_use]
    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Everything every environment asks for that it does not answer, the
    /// vault half included wherever the vault has been read.
    fn recheck(&mut self) {
        let findings: Vec<Finding> = self
            .manifests
            .iter()
            .flat_map(|(name, manifest)| check_with(manifest, self.vault_of(name)))
            .collect();
        self.findings = findings;
        self.clamp();
    }

    /// What the environment's own vault holds, where somebody has read it.
    fn vault_of(&self, environment: &str) -> Option<&VaultNames> {
        let named = self
            .environments()
            .iter()
            .find(|held| held.name == environment)?
            .vault
            .as_deref()?;
        self.vaults
            .iter()
            .find(|held| held.vault.eq_ignore_ascii_case(named))
    }

    #[must_use]
    fn manifest(&self, environment: &str) -> Option<&EnvManifest> {
        self.manifests
            .iter()
            .find(|(held, _)| held == environment)
            .map(|(_, manifest)| manifest)
    }

    /// Why one environment's column is blank, if it is.
    #[must_use]
    pub fn error(&self, environment: &str) -> Option<&str> {
        self.errors
            .iter()
            .find(|(held, _)| held == environment)
            .map(|(_, message)| message.as_str())
    }

    /// Every service across every environment, one row each, in name order.
    /// A workload of the same name in two environments is one service however
    /// its namespaces differ, which is what makes the row a promotion.
    #[must_use]
    pub fn rows(&self) -> Vec<ServiceRow> {
        let mut names: Vec<&str> = Vec::new();
        for environment in self.environments() {
            let Some(manifest) = self.manifest(&environment.name) else {
                continue;
            };
            for workload in &manifest.workloads {
                if !names.contains(&workload.name.as_str()) {
                    names.push(&workload.name);
                }
            }
        }
        names.sort_unstable();
        names
            .into_iter()
            .map(|name| {
                let held = self.environments().iter().find_map(|environment| {
                    self.manifest(&environment.name)?
                        .workloads
                        .iter()
                        .find(|workload| workload.name == name)
                });
                ServiceRow {
                    workload: name.to_owned(),
                    kind: held.map(|held| held.kind.clone()).unwrap_or_default(),
                    namespace: held.map(|held| held.namespace.clone()).unwrap_or_default(),
                    cells: self
                        .environments()
                        .iter()
                        .map(|environment| self.cell(environment, name))
                        .collect(),
                }
            })
            .collect()
    }

    /// What one environment says about one service.
    fn cell(&self, environment: &Environment, service: &str) -> EnvCell {
        let Some(manifest) = self.manifest(&environment.name) else {
            return EnvCell::default();
        };
        let Some(workload) = manifest
            .workloads
            .iter()
            .find(|workload| workload.name == service)
        else {
            return EnvCell {
                rendered: true,
                ..EnvCell::default()
            };
        };
        let mine = self.owned(&environment.name, manifest, workload);
        EnvCell {
            tag: Some(image_tag(workload)),
            findings: mine
                .iter()
                .filter(|finding| !matches!(finding.missing, Missing::Expiring { .. }))
                .count(),
            expiring: mine
                .iter()
                .filter(|finding| matches!(finding.missing, Missing::Expiring { .. }))
                .count(),
            rendered: true,
        }
    }

    /// The findings one workload owns in one environment: its own, and those
    /// of the providers it mounts or reads a Secret from — a vault object the
    /// prod vault never got is the service's problem, not the provider's.
    fn owned<'a>(
        &'a self,
        environment: &str,
        manifest: &EnvManifest,
        workload: &Workload,
    ) -> Vec<&'a Finding> {
        let providers: Vec<&str> = diff::providers(manifest, workload)
            .iter()
            .map(|provider| provider.name.as_str())
            .collect();
        self.findings
            .iter()
            .filter(|finding| {
                finding.environment == environment
                    && (finding.workload == workload.name
                        || providers.contains(&finding.workload.as_str()))
            })
            .collect()
    }

    /// Every row the query leaves, in the order the board draws them.
    #[must_use]
    pub fn visible(&self) -> Vec<ServiceRow> {
        let parsed: ParsedQuery<ServiceSchema> = parse_query(self.query.text());
        let context = MatchContext::now();
        self.rows()
            .into_iter()
            .filter(|row| {
                parsed.filters.matches_in(row, false, &context) && row.matches_fuzzy(&parsed.fuzzy)
            })
            .collect()
    }

    #[must_use]
    pub fn selected(&self) -> Option<ServiceRow> {
        self.visible().get(self.cursor.index).cloned()
    }

    /// The environment column the cursor is on, clamped to what the file
    /// declares: the board opens on the right-most, which is where a promotion
    /// is read into.
    #[must_use]
    pub fn column(&self) -> usize {
        self.column.min(self.environments().len().saturating_sub(1))
    }

    /// The environment the details pane promotes into.
    #[must_use]
    pub fn target(&self) -> Option<&Environment> {
        self.environments().get(self.column())
    }

    /// The one to its left, which is what a promotion comes from.
    #[must_use]
    pub fn source(&self) -> Option<&Environment> {
        self.environments().get(self.column().checked_sub(1)?)
    }

    /// What the details pane's frame says: the pane's own name, then the
    /// promotion it is reading — `Promotion · orders-api · qa → prod`.
    #[must_use]
    pub fn promotion_label(&self) -> String {
        let Some(row) = self.selected() else {
            return "Promotion".to_owned();
        };
        match (self.source(), self.target()) {
            (Some(source), Some(target)) => format!(
                "Promotion \u{00b7} {} \u{00b7} {} \u{2192} {}",
                row.workload, source.name, target.name
            ),
            (None, Some(target)) => {
                format!(
                    "Promotion \u{00b7} {} \u{00b7} {}",
                    row.workload, target.name
                )
            }
            _ => format!("Promotion \u{00b7} {}", row.workload),
        }
    }

    /// Moves the column cursor, which is what `h` and `l` do.
    pub fn move_column(&mut self, delta: isize) {
        let count = self.environments().len();
        if count == 0 {
            return;
        }
        self.column = self
            .column()
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        self.jump_cursor = 0;
        self.details.scroll_to(0);
    }

    /// Puts the column cursor on one environment, which is what a click on a
    /// cell does alongside moving the row cursor.
    pub fn focus_column(&mut self, index: usize) {
        if index < self.environments().len() {
            self.column = index;
            self.jump_cursor = 0;
        }
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
        self.jump_cursor = 0;
    }

    /// Whether the `Findings` filter is on, which is what the chip draws.
    #[must_use]
    pub fn findings_only(&self) -> bool {
        parse_query::<ServiceSchema>(self.query.text())
            .filters
            .contains(ServiceField::Findings, "yes")
    }

    /// The `Findings` chip: narrows the board to the rows something is missing
    /// from, and puts them back.
    pub fn toggle_findings(&mut self) {
        let mut parsed = parse_query::<ServiceSchema>(self.query.text());
        if self.findings_only() {
            parsed.filters.remove(ServiceField::Findings, "yes");
        } else {
            parsed.filters.insert(ServiceField::Findings, "yes");
        }
        self.set_query(crate::filter::format_query(&parsed.filters, &parsed.fuzzy));
    }

    /// Keeps both cursors on rows that are still there after a render.
    fn clamp(&mut self) {
        let count = self.visible().len();
        self.cursor.clamp(count);
    }

    /// What the details pane draws, line by line: what the environment under
    /// the cursor is missing, and then the promotion into it from the column
    /// to its left.
    #[must_use]
    pub fn detail_lines(&self, shell: &Shell) -> Vec<DiffLine> {
        let mut lines = Vec::new();
        let (Some(row), Some(target)) = (self.selected(), self.target()) else {
            return lines;
        };
        let missing: Vec<&Finding> = self
            .manifest(&target.name)
            .and_then(|manifest| {
                let workload = manifest
                    .workloads
                    .iter()
                    .find(|held| held.name == row.workload)?;
                Some(self.owned(&target.name, manifest, workload))
            })
            .unwrap_or_default();
        let expiring = |finding: &&Finding| matches!(finding.missing, Missing::Expiring { .. });
        let (lapsing, short): (Vec<&Finding>, Vec<&Finding>) =
            missing.into_iter().partition(|finding| expiring(finding));
        push_findings(
            &mut lines,
            format!("Missing in {}", target.name),
            short.into_iter(),
            self.environments(),
        );
        self.push_promotion(&mut lines, &row, target, shell);
        push_findings(
            &mut lines,
            "Expiry".to_owned(),
            lapsing.into_iter(),
            self.environments(),
        );
        lines
    }

    /// The promotion half of the pane: the two renders read against each
    /// other, in the wording the pull request pre-flight uses.
    fn push_promotion(
        &self,
        lines: &mut Vec<DiffLine>,
        row: &ServiceRow,
        target: &Environment,
        shell: &Shell,
    ) {
        let Some(source) = self.source() else {
            lines.push(DiffLine::new(
                DiffLineKind::Note,
                format!(
                    "{} is the first environment; l reads the next one",
                    target.name
                ),
            ));
            return;
        };
        let (Some(from), Some(to)) = (self.manifest(&source.name), self.manifest(&target.name))
        else {
            lines.push(DiffLine::new(
                DiffLineKind::Note,
                format!(
                    "{} or {} did not render, so there is nothing to compare",
                    source.name, target.name
                ),
            ));
            return;
        };
        let promotion = diff::diff(from, to, Some(&row.workload));
        let Some(service) = promotion
            .services
            .iter()
            .find(|held| held.workload == row.workload)
        else {
            lines.push(DiffLine::new(
                DiffLineKind::Note,
                format!(
                    "{} and {} say the same thing about {}",
                    source.name, target.name, row.workload
                ),
            ));
            self.push_pod(lines, to, &row.workload, shell);
            return;
        };
        let mut section = None;
        for line in diff::promotion_lines(&promotion, service) {
            if section != Some(line.section) {
                section = Some(line.section);
                lines.push(DiffLine::new(
                    DiffLineKind::Section,
                    line.section.label().to_owned(),
                ));
            }
            let jump = match &line.names {
                Names::Container(container) => self.image_run(service, container),
                Names::VaultObject { vault, object } => Some(Jump::VaultItem {
                    vault: vault.clone(),
                    kind: vault_object_kind(from, to, object),
                    name: object.clone(),
                }),
                Names::Nothing => None,
            };
            lines.push(DiffLine::going(DiffLineKind::Entry, line.text, jump));
            if let Names::Container(container) = &line.names {
                lines.push(DiffLine::new(
                    DiffLineKind::Note,
                    self.image_note(service, container),
                ));
            }
        }
        self.push_pod(lines, to, &row.workload, shell);
    }

    /// The pod the target environment is actually running, out of what the AKS
    /// tab has already read. Nothing here asks a cluster for more.
    fn push_pod(
        &self,
        lines: &mut Vec<DiffLine>,
        manifest: &EnvManifest,
        service: &str,
        shell: &Shell,
    ) {
        let Some(workload) = manifest.workloads.iter().find(|held| held.name == service) else {
            return;
        };
        let images: Vec<String> = workload
            .containers
            .iter()
            .map(|container| container.image.clone())
            .collect();
        if let Some(key) = shell.pod_running(&images) {
            lines.push(DiffLine::going(
                DiffLineKind::Entry,
                format!("running as {}/{}", key.namespace, key.name),
                Some(Jump::Pod(key.clone())),
            ));
        }
    }

    /// The run the tag arriving was built by, where one is on file.
    fn image_run(&self, service: &ServiceDiff, container: &str) -> Option<Jump> {
        let change = image_change(service, container)?;
        let arriving = change.from.as_deref()?;
        diff::read_back(arriving, &self.runs, None).map(|run| Jump::Run(run.id))
    }

    /// What is worth saying under an image line: the build the tag arriving
    /// came out of, or why there is no build to name.
    fn image_note(&self, service: &ServiceDiff, container: &str) -> String {
        let Some(change) = image_change(service, container) else {
            return NO_HISTORY.to_owned();
        };
        let history = diff::history(change, &self.runs, &[], None, &|_| None, &|_, _| {
            Err(NO_HISTORY.to_owned())
        });
        history.to_string()
    }

    /// This tab's slice of the context file: the environments, what the two
    /// cursors are on, what that cell is missing, and the diff being read.
    #[must_use]
    pub fn agent_context(&self, shell: &Shell) -> crate::agent_context::EnvironmentsContext {
        let rows = self.visible();
        let selected = rows.get(self.cursor.index);
        let lines = self.detail_lines(shell);
        crate::agent_context::EnvironmentsContext {
            // Where `g` goes from here is `App`'s to work out.
            follow: None,
            reason: self.reason().map(str::to_owned),
            environments: self
                .environments()
                .iter()
                .enumerate()
                .map(
                    |(index, environment)| crate::agent_context::EnvironmentContext {
                        name: environment.name.clone(),
                        vault: environment.vault.clone(),
                        overlays: environment.overlays.clone(),
                        rendered: self.manifest(&environment.name).is_some(),
                        error: self.error(&environment.name).map(str::to_owned),
                        findings: rows
                            .iter()
                            .filter_map(|row| row.cells.get(index))
                            .map(|cell| cell.findings)
                            .sum(),
                        expiring: rows
                            .iter()
                            .filter_map(|row| row.cells.get(index))
                            .map(|cell| cell.expiring)
                            .sum(),
                    },
                )
                .collect(),
            selected_service: selected.map(|row| row.workload.clone()),
            selected_environment: self.target().map(|environment| environment.name.clone()),
            findings: lines
                .iter()
                .filter(|line| line.kind == DiffLineKind::Missing)
                .map(|line| line.text.clone())
                .collect(),
            diff: self.source().zip(self.target()).map(|(source, target)| {
                crate::agent_context::PromotionContext {
                    from: source.name.clone(),
                    to: target.name.clone(),
                    lines: lines
                        .iter()
                        .filter(|line| line.kind == DiffLineKind::Entry)
                        .map(|line| line.text.clone())
                        .collect(),
                }
            }),
            visible_rows: rows.len(),
        }
    }

    /// One command, whether a key, a chip or the palette asked for it.
    pub fn run_command(&mut self, shell: &mut Shell, id: CommandId) -> AppAction {
        match id {
            CommandId::Search => self.mode = EnvMode::Search,
            CommandId::ToggleFindings => self.toggle_findings(),
            // Nothing on this tab comes from Azure DevOps, so the sync key
            // renders the overlays again and asks for the vaults afresh.
            CommandId::Sync => {
                self.invalidate();
                self.stale_vaults = self.deployment.is_some();
                shell.set_status("Rendering the overlays\u{2026}");
                return AppAction::Arm(crate::arm_watch::ArmRequest::Refresh);
            }
            CommandId::Open => return self.open_in_browser(shell),
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

    /// What `o` opens: the deployment repository's own page, which is the one
    /// thing on this tab with a page at all — an overlay is a file.
    fn open_in_browser(&self, shell: &mut Shell) -> AppAction {
        let url = self.deployment.as_ref().and_then(|deployment| {
            shell
                .repos()
                .iter()
                .find(|repo| repo.name.eq_ignore_ascii_case(&deployment.repo))
                .map(|repo| repo.web_url.clone())
                .filter(|url| !url.is_empty())
        });
        match url {
            Some(url) => AppAction::OpenUrl(url),
            None => {
                shell.set_error("No deployment repository to open here");
                AppAction::None
            }
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.mode = EnvMode::Browse,
            _ => {
                self.query.handle_key(key);
                self.cursor.reset();
                self.jump_cursor = 0;
            }
        }
        AppAction::None
    }

    /// The details pane: `j`/`k` walk the lines that go somewhere, and `Enter`
    /// follows the one they are on. A pane with none scrolls instead.
    fn handle_details_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        let jumps = self.jumps(shell);
        self.jump_cursor = self.jump_cursor.min(jumps.len().saturating_sub(1));
        match key.code {
            KeyCode::Tab | KeyCode::Esc => shell.focus_list(),
            KeyCode::Down | KeyCode::Char('j') => {
                if jumps.is_empty() {
                    self.details.scroll_by(1);
                } else {
                    self.jump_cursor = (self.jump_cursor + 1).min(jumps.len() - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if jumps.is_empty() {
                    self.details.scroll_by(-1);
                } else {
                    self.jump_cursor = self.jump_cursor.saturating_sub(1);
                }
            }
            KeyCode::Home => self.jump_cursor = 0,
            KeyCode::End => self.jump_cursor = jumps.len().saturating_sub(1),
            KeyCode::Enter => {
                if let Some(jump) = jumps.get(self.jump_cursor) {
                    return AppAction::Follow(jump.clone());
                }
            }
            KeyCode::Char('h') => self.move_column(-1),
            KeyCode::Char('l') => self.move_column(1),
            _ => {
                return command_for_key(key, TabId::Environments)
                    .map_or(AppAction::None, |id| self.run_command(shell, id));
            }
        }
        AppAction::None
    }

    /// Every line of the details pane that goes somewhere, in the order it is
    /// drawn: what `j`/`k` walk and what `g` answers with.
    #[must_use]
    pub fn jumps(&self, shell: &Shell) -> Vec<Jump> {
        self.detail_lines(shell)
            .into_iter()
            .filter_map(|line| line.jump)
            .collect()
    }
}

/// The tag one workload runs: the first container that is not an init one,
/// which is the service itself.
fn image_tag(workload: &Workload) -> String {
    workload
        .containers
        .iter()
        .find(|container| !container.init)
        .or_else(|| workload.containers.first())
        .map_or_else(
            || "\u{2014}".to_owned(),
            |container| diff::tag(&container.image).to_owned(),
        )
}

fn image_change<'a>(service: &'a ServiceDiff, container: &str) -> Option<&'a ImageChange> {
    service
        .images
        .iter()
        .find(|change| change.container == container)
}

/// What the Key Vault tab files one vault object under, out of whichever side
/// of the promotion pulls it. An `ExternalSecret` says no kind, and a secret is
/// what the vault is asked for first.
fn vault_object_kind(from: &EnvManifest, to: &EnvManifest, object: &str) -> String {
    [from, to]
        .into_iter()
        .flat_map(|manifest| &manifest.providers)
        .flat_map(|provider| &provider.objects)
        .find(|held| held.name == object)
        .and_then(|held| held.kind.clone())
        .map_or_else(
            || "secret".to_owned(),
            |kind| {
                if kind == "certificate" {
                    "cert".to_owned()
                } else {
                    kind
                }
            },
        )
}

/// One section of findings, left out altogether when there are none.
fn push_findings<'a>(
    lines: &mut Vec<DiffLine>,
    title: String,
    findings: impl Iterator<Item = &'a Finding>,
    environments: &[Environment],
) {
    let mut written = false;
    for finding in findings {
        if !written {
            written = true;
            lines.push(DiffLine::new(DiffLineKind::Section, title.clone()));
        }
        lines.push(DiffLine::going(
            DiffLineKind::Missing,
            finding.to_string(),
            finding_jump(environments, finding),
        ));
    }
}

impl Screen for EnvironmentsScreen {
    fn handle_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        if self.mode == EnvMode::Search {
            return self.handle_search_key(key);
        }
        if shell.focus == Focus::Details {
            return self.handle_details_key(shell, key);
        }
        let count = self.visible().len();
        match key.code {
            KeyCode::Tab => shell.toggle_focus(),
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor.move_by(1, count);
                self.jump_cursor = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor.move_by(-1, count);
                self.jump_cursor = 0;
            }
            KeyCode::PageDown => self.cursor.page(1, count),
            KeyCode::PageUp => self.cursor.page(-1, count),
            KeyCode::Home => self.cursor.focus(0),
            KeyCode::End => self.cursor.move_by(isize::MAX, count),
            // The column cursor is the promotion: `prod` under it, `qa` to its
            // left, and the details pane reads one into the other.
            KeyCode::Left | KeyCode::Char('h') => self.move_column(-1),
            KeyCode::Right | KeyCode::Char('l') => self.move_column(1),
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.cursor.reset();
            }
            _ => {
                return command_for_key(key, TabId::Environments)
                    .map_or(AppAction::None, |id| self.run_command(shell, id));
            }
        }
        AppAction::None
    }

    fn handle_paste(&mut self, _shell: &mut Shell, pasted: &str) {
        if self.mode == EnvMode::Search {
            self.query.paste(pasted, true);
        }
    }

    fn activate_target(
        &mut self,
        shell: &mut Shell,
        target: PointerTarget,
        column: u16,
        _row: u16,
    ) -> AppAction {
        match target {
            // A click on a cell settles both cursors: the row it is on and the
            // environment it is under, which is the promotion it asks about.
            PointerTarget::TableCell { row, column } => {
                if row < self.visible().len() {
                    self.cursor.focus(row);
                }
                self.focus_column(column);
                shell.focus = Focus::Tickets;
            }
            PointerTarget::TableRow { index } | PointerTarget::ToggleRowSelect { index } => {
                if index < self.visible().len() {
                    self.cursor.focus(index);
                }
                shell.focus = Focus::Tickets;
            }
            PointerTarget::FocusDetails => shell.focus = Focus::Details,
            // The details pane's chips stand for the keys they name.
            PointerTarget::RunCommand(id) => return self.run_command(shell, id),
            // A click both settles the pane's cursor on the line and follows
            // it, so `[` back and `Enter` again land where the eye did.
            PointerTarget::Follow(jump) => {
                if let Some(index) = self.jumps(shell).iter().position(|held| *held == jump) {
                    self.jump_cursor = index;
                }
                return AppAction::Follow(jump);
            }
            PointerTarget::SearchField => {
                self.mode = EnvMode::Search;
                self.query.set_cursor(usize::from(column));
            }
            PointerTarget::CloseOverlay | PointerTarget::DismissOverlay => {
                self.close_overlay(shell);
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
        self.mode = EnvMode::Browse;
    }

    fn active_editor(&self) -> Option<TextEditor> {
        (self.mode == EnvMode::Search).then_some(TextEditor::Search)
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

    /// Where the line under the details cursor points: a run, a vault object,
    /// or the pod running it. The board's rows are services rather than things
    /// another tab holds, so the pane is where a jump comes from.
    fn follow_target(&self, shell: &Shell) -> Result<(Jump, &'static str), String> {
        let jumps = self.jumps(shell);
        if jumps.is_empty() {
            return Err(match self.selected() {
                Some(row) => format!("Nothing on {} to go to from here", row.workload),
                None => "No service is selected".to_owned(),
            });
        }
        let jump = jumps
            .get(self.jump_cursor)
            .or_else(|| jumps.first())
            .cloned()
            .ok_or_else(|| "Nothing to go to from here".to_owned())?;
        let noun = match jump {
            Jump::Run(_) => "run",
            Jump::Pod(_) => "pod",
            Jump::VaultItem { .. } => "vault item",
            _ => "vault",
        };
        Ok((jump, noun))
    }

    fn columns(&self) -> &dyn ColumnLayout {
        &self.layout
    }

    fn columns_mut(&mut self) -> &mut dyn ColumnLayout {
        &mut self.layout
    }

    fn snapshot(&self) -> TabSession {
        TabSession {
            query: self.query.text().to_owned(),
            sort_field: EnvColumn::Service.key().to_owned(),
            columns: self.layout.to_session_columns(),
            ..TabSession::default()
        }
    }

    fn restore(&mut self, _shell: &mut Shell, session: TabSession) {
        self.query = TextInput::new(session.query);
        self.layout = TableLayout::from_session_columns(&session.columns);
    }

    /// `✗N`: how many environments would be missing something, so the bar says
    /// "prod is not ready" from any tab.
    fn badge(&self) -> Option<String> {
        let short = self
            .environments()
            .iter()
            .filter(|environment| {
                self.findings.iter().any(|finding| {
                    finding.environment == environment.name
                        && !matches!(finding.missing, Missing::Expiring { .. })
                })
            })
            .count();
        (short > 0).then(|| format!("\u{2717}{short}"))
    }

    fn footer_hint(&self, _shell: &Shell) -> &str {
        match self.mode {
            EnvMode::Search => "←→ cursor  Ctrl-W delete word  Ctrl-U clear  Enter/Esc finish",
            EnvMode::Browse => {
                "↑↓/jk move  ←→/hl environment  / search  F findings  Tab diff  r render  ? help"
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
        crate::ui::environments::render(frame, self, shell, area);
    }
}

/// What the promotion diff of one service reads as, so a test can name a line
/// without drawing one.
#[cfg(test)]
impl EnvironmentsScreen {
    pub(crate) fn detail_text(&self, shell: &Shell) -> Vec<String> {
        self.detail_lines(shell)
            .into_iter()
            .map(|line| line.text)
            .collect()
    }
}
