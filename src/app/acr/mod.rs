//! The ACR screen: the subscription's container registries, the repositories
//! in the one chosen, and the tags of the repository the details pane is on.
//!
//! Nothing here is stored. What a subscription holds is read live by
//! [`crate::arm_watch`], and the next read replaces it: a registry's catalog
//! is not the project's business, and it changes without anyone editing a work
//! item.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::{AppAction, CopiedContent, Focus, ListCursor, Screen, Shell, TabId};
use crate::arm::{Inventory, Manifest, Registry, Repository, Tag, Vault, portal_url};
use crate::arm_watch::{ArmFocus, ArmRequest};
use crate::columns::{ColumnId, ColumnLayout, TableLayout};
use crate::command::{CommandId, command_for_key};
use crate::filter::{MatchContext, ParsedQuery, parse_query};
use crate::model::Jump;
use crate::pointer::{PointerTarget, ScrollState, ScrollSurface, TextEditor};
use crate::session::TabSession;

mod columns;
mod filters;
pub mod rows;
#[cfg(test)]
pub(crate) mod tests;

pub use columns::{RegistryColumn, RepositoryColumn};
pub use filters::{RegistrySchema, RepositorySchema};
pub use rows::{RegistryRow, RepositoryRow};

use crate::text_input::TextInput;

/// Which of the two lists is showing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Level {
    /// Every registry the subscription holds.
    #[default]
    Registries,
    /// The repositories of one registry, by its name, which `Backspace` or `h`
    /// goes back up from.
    Repositories(String),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AcrMode {
    #[default]
    Browse,
    Search,
}

/// The ACR tab's state: what has been read, how each level is arranged, and
/// where the two cursors and the details pane have got to.
pub struct AcrScreen {
    /// The subscription as last read. The vaults travel with the registries —
    /// one query answers for both tabs — and C1 reads them from here.
    inventory: Inventory,
    /// Each registry's catalog, by registry name. An entry with an empty list
    /// is a catalog that has been answered, which is what tells an empty
    /// registry from one nobody has asked about.
    repositories: Vec<(String, Vec<Repository>)>,
    /// The tags read per `(registry, repository)`, newest first.
    tags: Vec<(String, String, Vec<Tag>)>,
    /// The manifests asked for, by `(registry, repository, digest)`. `None` is
    /// one that was asked for and refused, which is still an answer.
    manifests: Vec<(String, String, String, Option<Manifest>)>,
    level: Level,
    pub mode: AcrMode,
    /// One query per level: going down into a registry's repositories and back
    /// up again puts each list back the way it was left.
    registry_query: TextInput,
    repository_query: TextInput,
    pub registries_layout: TableLayout<RegistryColumn>,
    pub repositories_layout: TableLayout<RepositoryColumn>,
    pub registry_sort: (RegistryColumn, bool),
    pub repository_sort: (RepositoryColumn, bool),
    pub registry_cursor: ListCursor,
    pub repository_cursor: ListCursor,
    pub details: ScrollState,
    /// The repository the details pane is showing and where its tag cursor is.
    focused: Option<(String, usize)>,
    /// The last refusal, which the details pane shows until something is read.
    error: Option<String>,
}

impl Default for AcrScreen {
    fn default() -> Self {
        Self {
            inventory: Inventory::default(),
            repositories: Vec::new(),
            tags: Vec::new(),
            manifests: Vec::new(),
            level: Level::Registries,
            mode: AcrMode::Browse,
            registry_query: TextInput::default(),
            repository_query: TextInput::default(),
            registries_layout: TableLayout::default(),
            repositories_layout: TableLayout::default(),
            // By name up top, newest first inside a registry, which is the
            // order each list is read in.
            registry_sort: (RegistryColumn::Name, false),
            repository_sort: (RepositoryColumn::Updated, true),
            registry_cursor: ListCursor::default(),
            repository_cursor: ListCursor::default(),
            details: ScrollState::default(),
            focused: None,
            error: None,
        }
    }
}

impl AcrScreen {
    /// What the subscription answered with. The cursor stays on the registry
    /// it was on, by name, wherever that now sorts.
    pub fn set_inventory(&mut self, inventory: Result<Inventory, String>) -> Option<String> {
        let selected = self.selected_registry().map(|row| row.registry.name);
        let inventory = match inventory {
            Ok(inventory) => inventory,
            Err(message) => return self.set_arm_error(message),
        };
        self.error = None;
        self.inventory = inventory;
        let rows = self.visible_registries();
        match selected.and_then(|name| rows.iter().position(|row| row.registry.name == name)) {
            Some(index) => self.registry_cursor.focus(index),
            None => self.registry_cursor.clamp(rows.len()),
        }
        None
    }

    /// One registry's catalog: names and nothing else, which is all a catalog
    /// listing carries. A refusal leaves whatever was read before standing,
    /// and answers the registry so nothing waits on it for ever.
    pub fn set_repositories(
        &mut self,
        registry: &str,
        repositories: Result<Vec<Repository>, String>,
    ) -> Option<String> {
        let selected = self.selected_repository().map(|row| row.repository.name);
        match repositories {
            Ok(listed) => {
                self.error = None;
                self.repositories.retain(|(held, _)| held != registry);
                self.repositories.push((registry.to_owned(), listed));
            }
            Err(message) => {
                let said = self.set_arm_error(message);
                if !self.repositories.iter().any(|(held, _)| held == registry) {
                    self.repositories.push((registry.to_owned(), Vec::new()));
                }
                return said;
            }
        }
        let rows = self.visible_repositories();
        match selected.and_then(|name| rows.iter().position(|row| row.repository.name == name)) {
            Some(index) => self.repository_cursor.focus(index),
            None => self.repository_cursor.clamp(rows.len()),
        }
        None
    }

    /// One repository's counts and stamp, folded into the row the catalog
    /// left. One of these lands per repository, so the table fills in.
    pub fn set_repository(
        &mut self,
        registry: &str,
        repository: Result<Repository, String>,
    ) -> Option<String> {
        let read = match repository {
            Ok(read) => read,
            Err(message) => return self.set_arm_error(message),
        };
        self.error = None;
        if let Some((_, held)) = self
            .repositories
            .iter_mut()
            .find(|(name, _)| name == registry)
            && let Some(row) = held.iter_mut().find(|row| row.name == read.name)
        {
            *row = read;
        }
        None
    }

    /// One repository's tags, newest first whatever order they came in.
    pub fn set_tags(
        &mut self,
        registry: &str,
        repo: &str,
        tags: Result<Vec<Tag>, String>,
    ) -> Option<String> {
        let mut read = match tags {
            Ok(read) => read,
            Err(message) => {
                let said = self.set_arm_error(message);
                if !self.has_tags(registry, repo) {
                    self.tags
                        .push((registry.to_owned(), repo.to_owned(), Vec::new()));
                }
                return said;
            }
        };
        self.error = None;
        read.sort_by_key(|tag| std::cmp::Reverse(tag.created));
        self.tags
            .retain(|(held, held_repo, _)| held != registry || held_repo != repo);
        self.tags.push((registry.to_owned(), repo.to_owned(), read));
        self.clamp_tag_cursor();
        None
    }

    /// What one tag points at. A refusal is recorded as an answer too: the
    /// pane says so rather than asking for ever.
    pub fn set_manifest(
        &mut self,
        registry: &str,
        repo: &str,
        digest: &str,
        manifest: Result<Manifest, String>,
    ) -> Option<String> {
        let (read, said) = match manifest {
            Ok(read) => {
                self.error = None;
                (Some(read), None)
            }
            Err(message) => (None, self.set_arm_error(message)),
        };
        self.manifests.retain(|(held, held_repo, held_digest, _)| {
            held != registry || held_repo != repo || held_digest != digest
        });
        self.manifests.push((
            registry.to_owned(),
            repo.to_owned(),
            digest.to_owned(),
            read,
        ));
        // Only the repository on screen and the one before it are worth
        // keeping; the rest are a read away.
        if self.manifests.len() > 64 {
            self.manifests.remove(0);
        }
        said
    }

    /// Why nothing could be read, for the details pane. Answers with what is
    /// worth saying out loud: the same refusal twice running is not.
    pub fn set_arm_error(&mut self, message: String) -> Option<String> {
        let repeated = self.error.as_deref() == Some(message.as_str());
        self.error = Some(message.clone());
        (!repeated).then_some(message)
    }

    /// The refusal standing, if one is.
    #[must_use]
    pub fn arm_error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The vaults the same inventory carried, which the Key Vault tab reads
    /// rather than asking for a second time.
    #[must_use]
    pub fn vaults(&self) -> &[Vault] {
        &self.inventory.vaults
    }

    /// What is worth one read right now, which is what the worker is told.
    /// Each answer moves this on to the next thing, so one focus at a time
    /// walks the catalog, then the tags, then the manifest.
    #[must_use]
    pub fn focus(&self) -> Option<ArmFocus> {
        let Level::Repositories(registry) = &self.level else {
            return None;
        };
        if !self.repositories.iter().any(|(held, _)| held == registry) {
            return Some(ArmFocus::Registry(registry.clone()));
        }
        let repo = self.selected_repository()?.repository.name;
        if !self.has_tags(registry, &repo) {
            return Some(ArmFocus::Repository {
                registry: registry.clone(),
                name: repo,
            });
        }
        let digest = self.selected_tag()?.digest;
        if digest.is_empty() || self.asked_for_manifest(registry, &repo, &digest) {
            return None;
        }
        Some(ArmFocus::Tag {
            registry: registry.clone(),
            repo,
            digest,
        })
    }

    /// Whether a read is in flight, which is what makes a spinner turn. A
    /// refusal is standing and nothing is: the pane says why instead.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.error.is_none() && self.focus().is_some()
    }

    #[must_use]
    pub const fn level(&self) -> &Level {
        &self.level
    }

    /// The registry whose repositories are showing, if that level is.
    #[must_use]
    pub fn open_registry(&self) -> Option<&Registry> {
        let Level::Repositories(name) = &self.level else {
            return None;
        };
        self.inventory
            .registries
            .iter()
            .find(|registry| registry.name == *name)
    }

    /// Every registry the query leaves, in the order the table draws them.
    #[must_use]
    pub fn visible_registries(&self) -> Vec<RegistryRow> {
        let parsed: ParsedQuery<RegistrySchema> = parse_query(self.registry_query.text());
        let context = MatchContext::now();
        let mut rows: Vec<RegistryRow> = self
            .inventory
            .registries
            .iter()
            .map(|registry| RegistryRow {
                repositories: self
                    .repositories
                    .iter()
                    .find(|(held, _)| *held == registry.name)
                    .map(|(_, listed)| listed.len()),
                registry: registry.clone(),
            })
            .filter(|row| {
                parsed.filters.matches_in(row, false, &context) && row.matches_fuzzy(&parsed.fuzzy)
            })
            .collect();
        let (column, descending) = self.registry_sort;
        rows.sort_by(|left, right| {
            let ordering = columns::compare_registries(left, right, column);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        rows
    }

    /// Every repository of the open registry the query leaves.
    #[must_use]
    pub fn visible_repositories(&self) -> Vec<RepositoryRow> {
        let Level::Repositories(registry) = &self.level else {
            return Vec::new();
        };
        let parsed: ParsedQuery<RepositorySchema> = parse_query(self.repository_query.text());
        let context = MatchContext::now();
        let mut rows: Vec<RepositoryRow> = self
            .repositories
            .iter()
            .find(|(held, _)| held == registry)
            .map(|(_, listed)| listed.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|repository| RepositoryRow {
                registry: registry.clone(),
                repository: repository.clone(),
            })
            .filter(|row| {
                parsed.filters.matches_in(row, false, &context) && row.matches_fuzzy(&parsed.fuzzy)
            })
            .collect();
        let (column, descending) = self.repository_sort;
        rows.sort_by(|left, right| {
            let ordering = columns::compare_repositories(left, right, column);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        // A repository whose attributes have not landed has nothing to
        // compare, and sorts last whichever way the column is turned.
        match column {
            RepositoryColumn::Tags => rows.sort_by_key(|row| row.repository.tags.is_none()),
            RepositoryColumn::Updated => {
                rows.sort_by_key(|row| row.repository.updated.is_none());
            }
            RepositoryColumn::Name => {}
        }
        rows
    }

    /// The registry the details pane is describing: the one under the cursor
    /// up top, or the one whose repositories are open.
    #[must_use]
    pub fn selected_registry(&self) -> Option<RegistryRow> {
        match &self.level {
            Level::Registries => self
                .visible_registries()
                .get(self.registry_cursor.index)
                .cloned(),
            Level::Repositories(name) => self
                .visible_registries()
                .into_iter()
                .find(|row| row.registry.name == *name),
        }
    }

    /// The repository under the cursor, on the level that has them.
    #[must_use]
    pub fn selected_repository(&self) -> Option<RepositoryRow> {
        self.visible_repositories()
            .get(self.repository_cursor.index)
            .cloned()
    }

    /// The tags read for one repository, newest first.
    #[must_use]
    pub fn tags(&self, registry: &str, repo: &str) -> &[Tag] {
        self.tags
            .iter()
            .find(|(held, held_repo, _)| held == registry && held_repo == repo)
            .map_or(&[], |(_, _, tags)| tags.as_slice())
    }

    /// The tags of whichever repository the details pane is on.
    #[must_use]
    pub fn shown_tags(&self) -> Vec<Tag> {
        let Level::Repositories(registry) = &self.level else {
            return Vec::new();
        };
        let Some((repo, _)) = &self.focused else {
            return Vec::new();
        };
        self.tags(registry, repo).to_vec()
    }

    /// Where the details pane's tag cursor is.
    #[must_use]
    pub fn tag_cursor(&self) -> usize {
        self.focused.as_ref().map_or(0, |(_, index)| *index)
    }

    /// The tag the details pane's cursor is on.
    #[must_use]
    pub fn selected_tag(&self) -> Option<Tag> {
        self.shown_tags().get(self.tag_cursor()).cloned()
    }

    /// What one tag points at, once it has been read.
    #[must_use]
    pub fn manifest(&self, registry: &str, repo: &str, digest: &str) -> Option<&Manifest> {
        self.manifests
            .iter()
            .find(|(held, held_repo, held_digest, _)| {
                held == registry && held_repo == repo && held_digest == digest
            })
            .and_then(|(_, _, _, manifest)| manifest.as_ref())
    }

    /// The manifest of the tag the details pane is on.
    #[must_use]
    pub fn shown_manifest(&self) -> Option<&Manifest> {
        let Level::Repositories(registry) = &self.level else {
            return None;
        };
        let (repo, _) = self.focused.as_ref()?;
        self.manifest(registry, repo, &self.selected_tag()?.digest)
    }

    /// How many of the open registry's repositories have had their attributes
    /// read, and how many there are, for the table's border.
    #[must_use]
    pub fn attributes_read(&self) -> (usize, usize) {
        let Level::Repositories(registry) = &self.level else {
            return (0, 0);
        };
        self.repositories
            .iter()
            .find(|(held, _)| held == registry)
            .map_or((0, 0), |(_, listed)| {
                (
                    listed
                        .iter()
                        .filter(|repository| repository.tags.is_some())
                        .count(),
                    listed.len(),
                )
            })
    }

    /// Puts the details pane on whatever the cursor is now over, which is what
    /// decides whose tags are worth reading.
    pub fn sync_focus(&mut self) {
        let repo = self.selected_repository().map(|row| row.repository.name);
        if self.focused.as_ref().map(|(name, _)| name.clone()) != repo {
            self.focused = repo.map(|name| (name, 0));
        }
    }

    /// Moves the tag cursor, which is what `j` and `k` do while the details
    /// pane has the focus.
    pub fn move_tag_cursor(&mut self, delta: isize) {
        let count = self.shown_tags().len();
        let Some((_, index)) = &mut self.focused else {
            return;
        };
        if count == 0 {
            return;
        }
        *index = index
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
    }

    fn clamp_tag_cursor(&mut self) {
        let count = self.shown_tags().len();
        if let Some((_, index)) = &mut self.focused {
            *index = (*index).min(count.saturating_sub(1));
        }
    }

    /// Down into the repositories of whatever the cursor is on.
    pub fn open_repositories(&mut self) {
        let Some(row) = self
            .visible_registries()
            .get(self.registry_cursor.index)
            .cloned()
        else {
            return;
        };
        self.level = Level::Repositories(row.registry.name);
        self.repository_cursor.reset();
        self.details.scroll_to(0);
        self.sync_focus();
    }

    /// Back up to the registries.
    pub fn close_repositories(&mut self) {
        self.level = Level::Registries;
        self.details.scroll_to(0);
        self.focused = None;
    }

    /// How many rows the level showing has, which is what its cursor clamps to.
    fn row_count(&self) -> usize {
        match self.level {
            Level::Registries => self.visible_registries().len(),
            Level::Repositories(_) => self.visible_repositories().len(),
        }
    }

    /// The cursor of whichever level is showing.
    pub const fn cursor_mut(&mut self) -> &mut ListCursor {
        match self.level {
            Level::Registries => &mut self.registry_cursor,
            Level::Repositories(_) => &mut self.repository_cursor,
        }
    }

    #[must_use]
    pub const fn cursor(&self) -> &ListCursor {
        match self.level {
            Level::Registries => &self.registry_cursor,
            Level::Repositories(_) => &self.repository_cursor,
        }
    }

    #[must_use]
    pub fn query(&self) -> &str {
        match self.level {
            Level::Registries => self.registry_query.text(),
            Level::Repositories(_) => self.repository_query.text(),
        }
    }

    #[must_use]
    pub fn query_cursor(&self) -> usize {
        match self.level {
            Level::Registries => self.registry_query.cursor(),
            Level::Repositories(_) => self.repository_query.cursor(),
        }
    }

    /// Sets the query of whichever level is showing, as typing does.
    pub fn set_query(&mut self, query: String) {
        self.query_mut().set_text(query);
        self.cursor_mut().reset();
    }

    fn query_mut(&mut self) -> &mut TextInput {
        match self.level {
            Level::Registries => &mut self.registry_query,
            Level::Repositories(_) => &mut self.repository_query,
        }
    }

    /// Sorts by one column, turning the direction around when it is already
    /// the one sorted by, the way every other table does.
    pub fn toggle_sort(&mut self, key: &str) {
        match self.level {
            Level::Registries => {
                if let Some(column) = RegistryColumn::from_key(key) {
                    let (current, descending) = self.registry_sort;
                    self.registry_sort =
                        (column, if current == column { !descending } else { true });
                }
            }
            Level::Repositories(_) => {
                if let Some(column) = RepositoryColumn::from_key(key) {
                    let (current, descending) = self.repository_sort;
                    self.repository_sort =
                        (column, if current == column { !descending } else { true });
                }
            }
        }
    }

    fn has_tags(&self, registry: &str, repo: &str) -> bool {
        self.tags
            .iter()
            .any(|(held, held_repo, _)| held == registry && held_repo == repo)
    }

    fn asked_for_manifest(&self, registry: &str, repo: &str, digest: &str) -> bool {
        self.manifests
            .iter()
            .any(|(held, held_repo, held_digest, _)| {
                held == registry && held_repo == repo && held_digest == digest
            })
    }

    /// What `y` copies: the pull reference of the tag under the details
    /// cursor, the repository without one, or the login server up top.
    #[must_use]
    pub fn pull_reference(&self) -> Option<String> {
        let server = self.selected_registry()?.registry.login_server;
        let Some(repo) = self.selected_repository() else {
            return Some(server);
        };
        Some(match self.selected_tag() {
            Some(tag) => format!("{server}/{}:{}", repo.repository.name, tag.name),
            None => format!("{server}/{}", repo.repository.name),
        })
    }

    /// What `o` opens: the registry the pane is describing, in the portal.
    #[must_use]
    pub fn open_in_browser(&self, shell: &mut Shell) -> AppAction {
        match self.selected_registry() {
            Some(row) => AppAction::OpenUrl(portal_url(&row.registry.id)),
            None => {
                shell.set_error("No registry to open here");
                AppAction::None
            }
        }
    }

    /// This tab's slice of the context file: which level it is on, and what
    /// the two cursors and the details pane are showing.
    #[must_use]
    pub fn agent_context(&self) -> crate::agent_context::AcrContext {
        let visible_rows = self.row_count();
        crate::agent_context::AcrContext {
            // Where `g` goes from here is `App`'s to work out.
            follow: None,
            level: match self.level {
                Level::Registries => "registries",
                Level::Repositories(_) => "repositories",
            }
            .to_owned(),
            selected_registry: self.selected_registry().map(|row| {
                crate::agent_context::RegistryContext {
                    name: row.registry.name.clone(),
                    resource_group: row.registry.resource_group.clone(),
                    sku: row.registry.sku.clone(),
                    location: row.registry.location.clone(),
                    login_server: row.registry.login_server.clone(),
                    portal_url: portal_url(&row.registry.id),
                }
            }),
            selected_repository: self.selected_repository().map(|row| {
                crate::agent_context::RepositoryContext {
                    name: row.repository.name,
                    tags: row.repository.tags,
                    updated: row
                        .repository
                        .updated
                        .map(crate::timestamp::Timestamp::to_rfc3339),
                }
            }),
            selected_tag: self
                .selected_tag()
                .map(|tag| crate::agent_context::TagContext {
                    name: tag.name,
                    digest: tag.digest,
                    created: tag.created.map(crate::timestamp::Timestamp::to_rfc3339),
                }),
            visible_rows,
        }
    }

    /// One command, whether a key, a chip in the details pane, or the palette
    /// asked for it.
    pub fn run_command(&mut self, shell: &mut Shell, id: CommandId) -> AppAction {
        match id {
            CommandId::Search => self.mode = AcrMode::Search,
            CommandId::Open => return self.open_in_browser(shell),
            // Nothing on this tab comes from Azure DevOps, so the sync key
            // reads the subscription again.
            CommandId::Sync => {
                // The open registry is read again too: what it holds is
                // dropped, so the focus asks for it afresh.
                if let Level::Repositories(registry) = self.level.clone() {
                    self.repositories.retain(|(held, _)| *held != registry);
                    self.tags.retain(|(held, ..)| *held != registry);
                    self.manifests.retain(|(held, ..)| *held != registry);
                }
                shell.set_status("Reading registries\u{2026}");
                return AppAction::Arm(ArmRequest::Refresh);
            }
            CommandId::CopyId => return self.copy(shell, self.pull_reference()),
            CommandId::CopyDigest => {
                let digest = self.selected_tag().map(|tag| tag.digest);
                return self.copy(shell, digest.filter(|digest| !digest.is_empty()));
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

    fn copy(&self, shell: &mut Shell, text: Option<String>) -> AppAction {
        match text {
            Some(text) => AppAction::Copy {
                text,
                content: CopiedContent::Id,
            },
            None => {
                shell.set_error("Nothing here to copy");
                AppAction::None
            }
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.mode = AcrMode::Browse,
            _ => {
                self.query_mut().handle_key(key);
                self.cursor_mut().reset();
                self.sync_focus();
            }
        }
        AppAction::None
    }
}

impl Screen for AcrScreen {
    fn handle_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        if self.mode == AcrMode::Search {
            return self.handle_search_key(key);
        }
        // The tag table lives in the details pane, so its cursor moves with
        // the same keys once the focus is over there.
        if shell.focus == Focus::Details {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.move_tag_cursor(1);
                    return AppAction::None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.move_tag_cursor(-1);
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
                let count = self.row_count();
                self.cursor_mut().move_by(1, count);
                self.sync_focus();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let count = self.row_count();
                self.cursor_mut().move_by(-1, count);
                self.sync_focus();
            }
            KeyCode::PageDown => {
                let count = self.row_count();
                self.cursor_mut().page(1, count);
                self.sync_focus();
            }
            KeyCode::PageUp => {
                let count = self.row_count();
                self.cursor_mut().page(-1, count);
                self.sync_focus();
            }
            KeyCode::Home => {
                self.cursor_mut().focus(0);
                self.sync_focus();
            }
            KeyCode::End => {
                let count = self.row_count();
                self.cursor_mut().move_by(isize::MAX, count);
                self.sync_focus();
            }
            // Down into a registry's repositories; on the repositories
            // already, Enter has nowhere further to go.
            KeyCode::Enter if self.level == Level::Registries => self.open_repositories(),
            KeyCode::Backspace | KeyCode::Char('h') => self.close_repositories(),
            KeyCode::Esc if !self.query().is_empty() => {
                self.query_mut().clear();
                self.cursor_mut().reset();
                self.sync_focus();
            }
            _ => {
                return command_for_key(key, TabId::Acr)
                    .map_or(AppAction::None, |id| self.run_command(shell, id));
            }
        }
        AppAction::None
    }

    fn handle_paste(&mut self, _shell: &mut Shell, pasted: &str) {
        if self.mode == AcrMode::Search {
            self.query_mut().paste(pasted, true);
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
                if index < self.row_count() {
                    self.cursor_mut().focus(index);
                    self.sync_focus();
                }
                shell.focus = Focus::Tickets;
            }
            // A click in the tag table picks the tag the manifest is read for.
            PointerTarget::TreeRow { index } => {
                shell.focus = Focus::Details;
                let count = self.shown_tags().len();
                if let Some((_, held)) = &mut self.focused
                    && index < count
                {
                    *held = index;
                }
            }
            PointerTarget::OpenInBrowser { index } => {
                if index < self.row_count() {
                    self.cursor_mut().focus(index);
                    self.sync_focus();
                }
                return self.open_in_browser(shell);
            }
            PointerTarget::SortHeader(key) => self.toggle_sort(key),
            PointerTarget::OpenSelectedUrl => return self.open_in_browser(shell),
            // The details pane's chips stand for the keys they name.
            PointerTarget::RunCommand(id) => return self.run_command(shell, id),
            PointerTarget::CloseOverlay | PointerTarget::DismissOverlay => {
                self.close_overlay(shell);
            }
            PointerTarget::SearchField => {
                self.mode = AcrMode::Search;
                self.place_caret(shell, TextEditor::Search, column, row);
            }
            _ => {}
        }
        AppAction::None
    }

    fn place_caret(&mut self, _shell: &mut Shell, editor: TextEditor, column: u16, _row: u16) {
        if editor == TextEditor::Search {
            self.query_mut().set_cursor(usize::from(column));
        }
    }

    fn close_overlay(&mut self, _shell: &mut Shell) {
        self.mode = AcrMode::Browse;
    }

    fn active_editor(&self) -> Option<TextEditor> {
        (self.mode == AcrMode::Search).then_some(TextEditor::Search)
    }

    fn scroll_state(&self, surface: ScrollSurface) -> ScrollState {
        match surface {
            ScrollSurface::Details => self.details,
            _ => self.cursor().scroll,
        }
    }

    fn scroll_state_mut(&mut self, surface: ScrollSurface) -> &mut ScrollState {
        match surface {
            ScrollSurface::Details => &mut self.details,
            _ => &mut self.cursor_mut().scroll,
        }
    }

    /// The pod running the tag the details pane is on, out of the clusters
    /// the AKS tab has already read. Nothing here asks a cluster for more:
    /// the tag is one `y` away, and a read nobody asked for is a read.
    fn follow_target(&self, shell: &Shell) -> Result<(Jump, &'static str), String> {
        let registry = self
            .selected_registry()
            .ok_or_else(|| "No registry is selected".to_owned())?;
        let repository = self
            .selected_repository()
            .ok_or_else(|| "Open a registry first".to_owned())?;
        let tag = self
            .selected_tag()
            .ok_or_else(|| format!("No tag is selected in {}", repository.repository.name))?;
        let server = registry.registry.login_server;
        let name = repository.repository.name;
        let references = [
            format!("{server}/{name}:{}", tag.name),
            format!("{server}/{name}@{}", tag.digest),
        ];
        shell
            .pod_running(&references, None)
            .map(|key| (Jump::Pod(key.clone()), "pod"))
            .ok_or_else(|| format!("No pod runs {name}:{}", tag.name))
    }

    /// A repository when the tab is showing one, the registry itself
    /// otherwise.
    fn here(&self, _shell: &Shell) -> Option<Jump> {
        match &self.level {
            Level::Registries => self
                .selected_registry()
                .map(|row| Jump::Registry(row.registry.name)),
            Level::Repositories(registry) => Some(self.selected_repository().map_or_else(
                || Jump::Registry(registry.clone()),
                |row| Jump::Repository {
                    registry: registry.clone(),
                    name: row.repository.name,
                },
            )),
        }
    }

    fn select(&mut self, _shell: &mut Shell, jump: &Jump) -> bool {
        match jump {
            Jump::Registry(name) => self.select_registry(name),
            Jump::Repository { registry, name } => {
                if !self.select_registry(registry) {
                    return false;
                }
                self.level = Level::Repositories(registry.clone());
                let position = |screen: &Self| {
                    screen
                        .visible_repositories()
                        .iter()
                        .position(|row| row.repository.name == *name)
                };
                let index = match position(self) {
                    Some(index) => index,
                    // On file but filtered out: the reference wins over the
                    // query, which is cleared rather than reported as a
                    // missing row.
                    None => {
                        self.repository_query.clear();
                        match position(self) {
                            Some(index) => index,
                            // The catalog has not been read yet, so the level
                            // opens on it and the worker fills it in.
                            None => {
                                self.repository_cursor.reset();
                                self.sync_focus();
                                return true;
                            }
                        }
                    }
                };
                self.repository_cursor.focus(index);
                self.sync_focus();
                true
            }
            _ => false,
        }
    }

    fn columns(&self) -> &dyn ColumnLayout {
        match self.level {
            Level::Registries => &self.registries_layout,
            Level::Repositories(_) => &self.repositories_layout,
        }
    }

    fn columns_mut(&mut self) -> &mut dyn ColumnLayout {
        match self.level {
            Level::Registries => &mut self.registries_layout,
            Level::Repositories(_) => &mut self.repositories_layout,
        }
    }

    fn snapshot(&self) -> TabSession {
        TabSession {
            query: self.registry_query.text().to_owned(),
            sort_field: self.registry_sort.0.key().to_owned(),
            columns: self.registries_layout.to_session_columns(),
            ..TabSession::default()
        }
    }

    fn restore(&mut self, _shell: &mut Shell, session: TabSession) {
        self.registry_query = TextInput::new(session.query);
        if let Some(column) = RegistryColumn::from_key(&session.sort_field) {
            self.registry_sort = (column, self.registry_sort.1);
        }
        self.registries_layout = TableLayout::from_session_columns(&session.columns);
    }

    fn footer_hint(&self, _shell: &Shell) -> &str {
        match (self.mode, &self.level) {
            (AcrMode::Search, _) => "←→ cursor  Ctrl-W delete word  Ctrl-U clear  Enter/Esc finish",
            (AcrMode::Browse, Level::Registries) => {
                "↑↓/jk move  Enter repositories  / search  y copy  o portal  r refresh  ? help"
            }
            (AcrMode::Browse, Level::Repositories(_)) => {
                "↑↓/jk move  h back  Tab tags  y copy pull  D copy digest  o portal  ? help"
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
        crate::ui::acr::render(frame, self, shell, area);
    }
}

impl AcrScreen {
    /// Puts the registries level on one registry, clearing the query when that
    /// is what is hiding it. Answers whether the subscription holds it.
    fn select_registry(&mut self, name: &str) -> bool {
        if !self
            .inventory
            .registries
            .iter()
            .any(|registry| registry.name == name)
        {
            return false;
        }
        let position = |screen: &Self| {
            screen
                .visible_registries()
                .iter()
                .position(|row| row.registry.name == name)
        };
        self.close_repositories();
        let index = match position(self) {
            Some(index) => index,
            None => {
                self.registry_query.clear();
                match position(self) {
                    Some(index) => index,
                    None => return false,
                }
            }
        };
        self.registry_cursor.focus(index);
        true
    }
}
