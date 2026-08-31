//! The Key Vault screen: the subscription's vaults, what each of them holds,
//! and — for one minute, in one place — the value of a single secret.
//!
//! Nothing here is stored. What a subscription holds is read live by
//! [`crate::arm_watch`], and the next read replaces it. A secret's value is
//! held more carefully still: it never reaches SQLite, the session file, the
//! context file, a notification or a log line, it is dropped when the cursor
//! moves off the item or a minute goes by, and [`crate::arm::Secret`] will not
//! print itself even into a panic. The only two places it is ever read out of
//! are the line that draws it and the key that copies it.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::{AppAction, CopiedContent, Focus, ListCursor, Screen, Shell, TabId};
use crate::arm::{Inventory, ItemKind, Secret, Vault, VaultItem, portal_url};
use crate::arm_watch::{ArmFocus, ArmRequest};
use crate::columns::{ColumnId, ColumnLayout, TableLayout};
use crate::command::{CommandId, command_for_key};
use crate::filter::{MatchContext, ParsedQuery, parse_query};
use crate::model::Jump;
use crate::pointer::{PointerTarget, ScrollState, ScrollSurface, TextEditor};
use crate::session::TabSession;
use crate::text_input::TextInput;
use crate::timestamp::Timestamp;

mod columns;
mod filters;
pub mod rows;
#[cfg(test)]
pub(crate) mod tests;

pub use columns::{ItemColumn, VaultColumn};
pub use filters::{ItemSchema, VaultSchema};
pub use rows::{Expiry, ItemRow, VaultRow};

/// How long a revealed value stays on screen. Long enough to read it or paste
/// it somewhere, short enough that a terminal left unattended is not a copy of
/// the vault.
pub const REVEAL_FOR: Duration = Duration::from_secs(60);

/// Which of the two lists is showing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Level {
    /// Every vault the subscription holds.
    #[default]
    Vaults,
    /// What one vault holds, by its name, which `Backspace` or `h` goes back
    /// up from.
    Items(String),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VaultMode {
    #[default]
    Browse,
    Search,
}

/// The one value on screen: the item it belongs to, and how long is left
/// before the screen forgets it.
#[derive(Clone, Copy, Debug)]
pub struct Revealed<'a> {
    pub vault: &'a str,
    pub name: &'a str,
    pub value: &'a Secret,
    pub clears_in: Duration,
}

/// The Key Vault tab's state: what has been read, how each level is arranged,
/// and where the two cursors and the details pane have got to.
pub struct KeyVaultScreen {
    /// The vaults as last read. They arrive with the registries — one query
    /// answers for both ARM tabs.
    vaults: Vec<Vault>,
    /// What each vault holds, by vault name. An entry with an empty list is a
    /// vault that has been answered, which is what tells an empty vault from
    /// one nobody has asked about.
    items: Vec<(String, Vec<VaultItem>)>,
    level: Level,
    pub mode: VaultMode,
    /// One query per level: going down into a vault and back up again puts
    /// each list back the way it was left.
    vault_query: TextInput,
    item_query: TextInput,
    pub vaults_layout: TableLayout<VaultColumn>,
    pub items_layout: TableLayout<ItemColumn>,
    pub vault_sort: (VaultColumn, bool),
    pub item_sort: (ItemColumn, bool),
    pub vault_cursor: ListCursor,
    pub item_cursor: ListCursor,
    pub details: ScrollState,
    /// The one value the run is showing: which item it belongs to, what it is,
    /// and when it went up. Never written anywhere else, and gone a minute
    /// after that instant.
    revealed: Option<(String, String, Secret, Instant)>,
    /// The reveal asked for and not yet answered, which is the only thing the
    /// tab spins for besides a listing.
    reveal_pending: Option<(String, String)>,
    /// The last refusal, which the details pane shows until something is read.
    error: Option<String>,
}

impl Default for KeyVaultScreen {
    fn default() -> Self {
        Self {
            vaults: Vec::new(),
            items: Vec::new(),
            level: Level::Vaults,
            mode: VaultMode::Browse,
            vault_query: TextInput::default(),
            item_query: TextInput::default(),
            vaults_layout: TableLayout::default(),
            items_layout: TableLayout::default(),
            // By name up top; inside a vault, whatever lapses first, which is
            // the only reason to look at this list in a hurry.
            vault_sort: (VaultColumn::Name, false),
            item_sort: (ItemColumn::Expires, false),
            vault_cursor: ListCursor::default(),
            item_cursor: ListCursor::default(),
            details: ScrollState::default(),
            revealed: None,
            reveal_pending: None,
            error: None,
        }
    }
}

impl KeyVaultScreen {
    /// What the subscription answered with. The cursor stays on the vault it
    /// was on, by name, wherever that now sorts.
    pub fn set_inventory(&mut self, inventory: Result<Inventory, String>) -> Option<String> {
        let selected = self.selected_vault().map(|row| row.vault.name);
        let inventory = match inventory {
            Ok(inventory) => inventory,
            Err(message) => return self.set_arm_error(message),
        };
        self.error = None;
        self.vaults = inventory.vaults;
        let rows = self.visible_vaults();
        match selected.and_then(|name| rows.iter().position(|row| row.vault.name == name)) {
            Some(index) => self.vault_cursor.focus(index),
            None => self.vault_cursor.clamp(rows.len()),
        }
        None
    }

    /// Everything one vault holds: its secrets, its keys and its certificates,
    /// by name and never by value. A refusal leaves whatever was read before
    /// standing, and answers the vault so nothing waits on it for ever.
    pub fn set_items(
        &mut self,
        vault: &str,
        items: Result<Vec<VaultItem>, String>,
    ) -> Option<String> {
        let selected = self
            .selected_item()
            .map(|row| (row.item.kind, row.item.name));
        match items {
            Ok(listed) => {
                self.error = None;
                self.items.retain(|(held, _)| held != vault);
                self.items.push((vault.to_owned(), listed));
            }
            Err(message) => {
                let said = self.set_arm_error(message);
                if !self.items.iter().any(|(held, _)| held == vault) {
                    self.items.push((vault.to_owned(), Vec::new()));
                }
                return said;
            }
        }
        let rows = self.visible_items();
        match selected.and_then(|(kind, name)| {
            rows.iter()
                .position(|row| row.item.kind == kind && row.item.name == name)
        }) {
            Some(index) => self.item_cursor.focus(index),
            None => self.item_cursor.clamp(rows.len()),
        }
        None
    }

    /// One secret's value, back from the worker. A refusal is said out loud; a
    /// value that arrived after the cursor moved on is dropped rather than
    /// shown against the wrong item.
    pub fn set_revealed(
        &mut self,
        vault: &str,
        name: &str,
        value: Result<Secret, String>,
    ) -> Option<String> {
        if self
            .reveal_pending
            .as_ref()
            .is_some_and(|(held, held_name)| held == vault && held_name == name)
        {
            self.reveal_pending = None;
        }
        let secret = match value {
            Ok(secret) => secret,
            Err(message) => return self.set_arm_error(message),
        };
        self.error = None;
        if self
            .selected_item()
            .is_some_and(|row| row.vault == vault && row.item.name == name)
        {
            self.revealed = Some((vault.to_owned(), name.to_owned(), secret, Instant::now()));
        }
        None
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

    /// What is worth one read right now, which is what the worker is told: the
    /// contents of the vault that is open, until they have come back.
    #[must_use]
    pub fn focus(&self) -> Option<ArmFocus> {
        let Level::Items(vault) = &self.level else {
            return None;
        };
        (!self.items.iter().any(|(held, _)| held == vault)).then(|| ArmFocus::Vault(vault.clone()))
    }

    /// Whether a read is in flight, which is what makes a spinner turn.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.reveal_pending.is_some() || (self.error.is_none() && self.focus().is_some())
    }

    /// The value on screen this minute, if one is.
    #[must_use]
    pub fn revealed(&self) -> Option<Revealed<'_>> {
        let (vault, name, value, shown_at) = self.revealed.as_ref()?;
        Some(Revealed {
            vault,
            name,
            value,
            clears_in: REVEAL_FOR.saturating_sub(shown_at.elapsed()),
        })
    }

    /// Whether the item under the cursor is the one whose value is showing,
    /// which is the only thing the context file says about a reveal.
    #[must_use]
    pub fn is_revealed(&self, vault: &str, name: &str) -> bool {
        self.revealed
            .as_ref()
            .is_some_and(|(held, held_name, _, _)| held == vault && held_name == name)
    }

    /// Whether a reveal has been asked for and not yet answered, which is what
    /// the pane draws dots for.
    #[must_use]
    pub fn reveal_pending(&self) -> bool {
        self.reveal_pending.is_some()
    }

    /// Drops the value on screen. Called by everything that means the person
    /// has looked away: a cursor move, a level change, `Esc`, leaving the tab,
    /// a refresh, and the minute running out.
    pub fn clear_reveal(&mut self) {
        self.revealed = None;
    }

    /// Clears a value whose minute is up, and says whether it did — which is
    /// what tells the loop to paint the pane without it.
    pub fn tick(&mut self) -> bool {
        let expired = self
            .revealed
            .as_ref()
            .is_some_and(|(_, _, _, shown_at)| shown_at.elapsed() >= REVEAL_FOR);
        if expired {
            self.clear_reveal();
        }
        expired
    }

    /// Puts the clock forward on whatever is showing, so a test can watch the
    /// minute run out without waiting one.
    #[cfg(test)]
    pub(crate) fn age_reveal(&mut self, by: Duration) {
        if let Some((_, _, _, shown_at)) = &mut self.revealed
            && let Some(earlier) = shown_at.checked_sub(by)
        {
            *shown_at = earlier;
        }
    }

    /// How long until the value on screen clears itself, so the loop wakes to
    /// take it away rather than leaving it up until the next keystroke.
    #[must_use]
    pub fn next_wakeup(&self) -> Option<Duration> {
        self.revealed().map(|revealed| revealed.clears_in)
    }

    #[must_use]
    pub const fn level(&self) -> &Level {
        &self.level
    }

    /// The vault whose items are showing, if that level is.
    #[must_use]
    pub fn open_vault(&self) -> Option<&Vault> {
        let Level::Items(name) = &self.level else {
            return None;
        };
        self.vaults.iter().find(|vault| vault.name == *name)
    }

    /// Every vault the query leaves, in the order the table draws them.
    #[must_use]
    pub fn visible_vaults(&self) -> Vec<VaultRow> {
        let parsed: ParsedQuery<VaultSchema> = parse_query(self.vault_query.text());
        let context = MatchContext::now();
        let mut rows: Vec<VaultRow> = self
            .vaults
            .iter()
            .map(|vault| VaultRow {
                items: self
                    .items
                    .iter()
                    .find(|(held, _)| *held == vault.name)
                    .map(|(_, listed)| listed.len()),
                vault: vault.clone(),
            })
            .filter(|row| {
                parsed.filters.matches_in(row, false, &context) && row.matches_fuzzy(&parsed.fuzzy)
            })
            .collect();
        let (column, descending) = self.vault_sort;
        rows.sort_by(|left, right| {
            let ordering = columns::compare_vaults(left, right, column);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        rows
    }

    /// Everything in the open vault the query leaves, all three kinds in one
    /// list.
    #[must_use]
    pub fn visible_items(&self) -> Vec<ItemRow> {
        let Level::Items(vault) = &self.level else {
            return Vec::new();
        };
        let parsed: ParsedQuery<ItemSchema> = parse_query(self.item_query.text());
        let context = MatchContext::now();
        let mut rows: Vec<ItemRow> = self
            .items
            .iter()
            .find(|(held, _)| held == vault)
            .map(|(_, listed)| listed.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|item| ItemRow {
                vault: vault.clone(),
                item: item.clone(),
            })
            .filter(|row| {
                parsed.filters.matches_in(row, false, &context) && row.matches_fuzzy(&parsed.fuzzy)
            })
            .collect();
        let (column, descending) = self.item_sort;
        rows.sort_by(|left, right| {
            let ordering = columns::compare_items(left, right, column);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        rows
    }

    /// The vault the details pane is describing: the one under the cursor up
    /// top, or the one whose items are open.
    #[must_use]
    pub fn selected_vault(&self) -> Option<VaultRow> {
        match &self.level {
            Level::Vaults => self.visible_vaults().get(self.vault_cursor.index).cloned(),
            Level::Items(name) => self
                .visible_vaults()
                .into_iter()
                .find(|row| row.vault.name == *name),
        }
    }

    /// The item under the cursor, on the level that has them.
    #[must_use]
    pub fn selected_item(&self) -> Option<ItemRow> {
        self.visible_items().get(self.item_cursor.index).cloned()
    }

    /// Certificates within thirty days of expiring, across every vault whose
    /// contents have been read. What the tab bar badges, and the one number
    /// this tab exists to put in front of somebody.
    #[must_use]
    pub fn expiring_certificates(&self) -> usize {
        let now = Timestamp::now();
        self.items
            .iter()
            .flat_map(|(_, items)| items)
            .filter(|item| {
                item.kind == ItemKind::Certificate && Expiry::of(item.expires, now).is_some()
            })
            .count()
    }

    /// Down into whatever the cursor is on.
    pub fn open_items(&mut self) {
        let Some(row) = self.visible_vaults().get(self.vault_cursor.index).cloned() else {
            return;
        };
        self.clear_reveal();
        self.level = Level::Items(row.vault.name);
        self.item_cursor.reset();
        self.details.scroll_to(0);
    }

    /// Back up to the vaults.
    pub fn close_items(&mut self) {
        self.clear_reveal();
        self.level = Level::Vaults;
        self.details.scroll_to(0);
    }

    /// How many rows the level showing has, which is what its cursor clamps to.
    fn row_count(&self) -> usize {
        match self.level {
            Level::Vaults => self.visible_vaults().len(),
            Level::Items(_) => self.visible_items().len(),
        }
    }

    /// The cursor of whichever level is showing.
    pub const fn cursor_mut(&mut self) -> &mut ListCursor {
        match self.level {
            Level::Vaults => &mut self.vault_cursor,
            Level::Items(_) => &mut self.item_cursor,
        }
    }

    #[must_use]
    pub const fn cursor(&self) -> &ListCursor {
        match self.level {
            Level::Vaults => &self.vault_cursor,
            Level::Items(_) => &self.item_cursor,
        }
    }

    #[must_use]
    pub fn query(&self) -> &str {
        match self.level {
            Level::Vaults => self.vault_query.text(),
            Level::Items(_) => self.item_query.text(),
        }
    }

    #[must_use]
    pub fn query_cursor(&self) -> usize {
        match self.level {
            Level::Vaults => self.vault_query.cursor(),
            Level::Items(_) => self.item_query.cursor(),
        }
    }

    /// Sets the query of whichever level is showing, as typing does.
    pub fn set_query(&mut self, query: String) {
        self.query_mut().set_text(query);
        self.cursor_mut().reset();
        self.clear_reveal();
    }

    fn query_mut(&mut self) -> &mut TextInput {
        match self.level {
            Level::Vaults => &mut self.vault_query,
            Level::Items(_) => &mut self.item_query,
        }
    }

    /// Sorts by one column, turning the direction around when it is already
    /// the one sorted by, the way every other table does.
    pub fn toggle_sort(&mut self, key: &str) {
        self.clear_reveal();
        match self.level {
            Level::Vaults => {
                if let Some(column) = VaultColumn::from_key(key) {
                    let (current, descending) = self.vault_sort;
                    self.vault_sort = (column, if current == column { !descending } else { true });
                }
            }
            Level::Items(_) => {
                if let Some(column) = ItemColumn::from_key(key) {
                    let (current, descending) = self.item_sort;
                    self.item_sort = (column, if current == column { !descending } else { true });
                }
            }
        }
    }

    /// What `y` copies: the item under the cursor, or the vault up top. Never
    /// the value — that is `Y`, and only while it is showing.
    #[must_use]
    pub fn copied_name(&self) -> Option<String> {
        match self.selected_item() {
            Some(row) => Some(row.item.name),
            None => self.selected_vault().map(|row| row.vault.name),
        }
    }

    /// What `o` opens: the vault the pane is describing, in the portal.
    #[must_use]
    pub fn open_in_browser(&self, shell: &mut Shell) -> AppAction {
        match self.selected_vault() {
            Some(row) => AppAction::OpenUrl(portal_url(&row.vault.id)),
            None => {
                shell.set_error("No vault to open here");
                AppAction::None
            }
        }
    }

    /// `R`: the value of the secret under the cursor, read once, now. Only a
    /// secret has one to show — a key's private half never leaves the vault,
    /// and a certificate is fetched as a file rather than read out loud.
    fn reveal(&mut self, shell: &mut Shell) -> AppAction {
        let Some(row) = self.selected_item() else {
            shell.set_error("No secret here to show");
            return AppAction::None;
        };
        if row.item.kind != ItemKind::Secret {
            shell.set_error("Only a secret has a value to show");
            return AppAction::None;
        }
        self.clear_reveal();
        self.reveal_pending = Some((row.vault.clone(), row.item.name.clone()));
        AppAction::Arm(ArmRequest::Reveal {
            vault: row.vault,
            name: row.item.name,
        })
    }

    /// `Y`: the value on screen, and only while it is on screen.
    fn copy_value(&mut self, shell: &mut Shell) -> AppAction {
        match &self.revealed {
            Some((_, _, secret, _)) => AppAction::CopySecret(secret.clone()),
            None => {
                shell.set_error("Nothing is revealed to copy");
                AppAction::None
            }
        }
    }

    /// This tab's slice of the context file: which level it is on, what the
    /// cursor is on, and how many certificates are running out. There is no
    /// field for a value and there is not meant to be one.
    #[must_use]
    pub fn agent_context(&self) -> crate::agent_context::KeyVaultContext {
        let visible_rows = self.row_count();
        crate::agent_context::KeyVaultContext {
            level: match self.level {
                Level::Vaults => "vaults",
                Level::Items(_) => "items",
            }
            .to_owned(),
            selected_vault: self
                .selected_vault()
                .map(|row| crate::agent_context::VaultContext {
                    name: row.vault.name.clone(),
                    resource_group: row.vault.resource_group.clone(),
                    location: row.vault.location.clone(),
                    sku: row.vault.sku.clone(),
                    uri: row.vault.uri.clone(),
                    portal_url: portal_url(&row.vault.id),
                }),
            selected_item: self
                .selected_item()
                .map(|row| crate::agent_context::VaultItemContext {
                    revealed: self.is_revealed(&row.vault, &row.item.name),
                    kind: row.item.kind.as_str().to_owned(),
                    name: row.item.name,
                    enabled: row.item.enabled,
                    updated: row.item.updated.map(Timestamp::to_rfc3339),
                    expires: row.item.expires.map(Timestamp::to_rfc3339),
                }),
            visible_rows,
            expiring_certificates: self.expiring_certificates(),
        }
    }

    /// One command, whether a key, a chip in the details pane, or the palette
    /// asked for it.
    pub fn run_command(&mut self, shell: &mut Shell, id: CommandId) -> AppAction {
        match id {
            CommandId::Search => self.mode = VaultMode::Search,
            CommandId::Open => return self.open_in_browser(shell),
            // Nothing on this tab comes from Azure DevOps, so the sync key
            // reads the subscription again.
            CommandId::Sync => {
                self.clear_reveal();
                shell.set_status("Reading vaults\u{2026}");
                return AppAction::Arm(ArmRequest::Refresh);
            }
            CommandId::RevealSecret => return self.reveal(shell),
            CommandId::CopyValue => return self.copy_value(shell),
            CommandId::CopyId => {
                return match self.copied_name() {
                    Some(text) => AppAction::Copy {
                        text,
                        content: CopiedContent::Id,
                    },
                    None => {
                        shell.set_error("Nothing here to copy");
                        AppAction::None
                    }
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

    fn handle_search_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.mode = VaultMode::Browse,
            _ => {
                self.clear_reveal();
                self.query_mut().handle_key(key);
                self.cursor_mut().reset();
            }
        }
        AppAction::None
    }

    /// Puts the cursor on one row, wherever the click or the key came from.
    /// The value on screen goes with it: it belongs to the item that was under
    /// the cursor, and that is no longer this one.
    fn focus_row(&mut self, index: usize) {
        self.clear_reveal();
        if index < self.row_count() {
            self.cursor_mut().focus(index);
        }
    }

    /// Puts the vaults level on one vault, clearing the query when that is
    /// what is hiding it. Answers whether the subscription holds it.
    fn select_vault(&mut self, name: &str) -> bool {
        if !self.vaults.iter().any(|vault| vault.name == name) {
            return false;
        }
        let position = |screen: &Self| {
            screen
                .visible_vaults()
                .iter()
                .position(|row| row.vault.name == name)
        };
        self.close_items();
        let index = match position(self) {
            Some(index) => index,
            None => {
                self.vault_query.clear();
                match position(self) {
                    Some(index) => index,
                    None => return false,
                }
            }
        };
        self.vault_cursor.focus(index);
        true
    }
}

impl Screen for KeyVaultScreen {
    fn handle_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        if self.mode == VaultMode::Search {
            return self.handle_search_key(key);
        }
        // Every cursor key takes the value on screen with it: it belongs to
        // the row the cursor is leaving.
        match key.code {
            KeyCode::Tab => shell.toggle_focus(),
            KeyCode::Down | KeyCode::Char('j') => {
                let count = self.row_count();
                self.clear_reveal();
                self.cursor_mut().move_by(1, count);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let count = self.row_count();
                self.clear_reveal();
                self.cursor_mut().move_by(-1, count);
            }
            KeyCode::PageDown => {
                let count = self.row_count();
                self.clear_reveal();
                self.cursor_mut().page(1, count);
            }
            KeyCode::PageUp => {
                let count = self.row_count();
                self.clear_reveal();
                self.cursor_mut().page(-1, count);
            }
            KeyCode::Home => self.focus_row(0),
            KeyCode::End => {
                let count = self.row_count();
                self.clear_reveal();
                self.cursor_mut().move_by(isize::MAX, count);
            }
            // Down into what a vault holds; inside one already, Enter has
            // nowhere further to go.
            KeyCode::Enter if self.level == Level::Vaults => self.open_items(),
            KeyCode::Backspace | KeyCode::Char('h') => self.close_items(),
            // The one key that takes a value off the screen without moving
            // anything, which is what somebody reaches for when a colleague
            // walks up.
            KeyCode::Esc => {
                self.clear_reveal();
                if !self.query().is_empty() {
                    self.query_mut().clear();
                    self.cursor_mut().reset();
                }
            }
            _ => {
                return command_for_key(key, TabId::KeyVault)
                    .map_or(AppAction::None, |id| self.run_command(shell, id));
            }
        }
        AppAction::None
    }

    fn handle_paste(&mut self, _shell: &mut Shell, pasted: &str) {
        if self.mode == VaultMode::Search {
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
                self.focus_row(index);
                shell.focus = Focus::Tickets;
            }
            PointerTarget::OpenInBrowser { index } => {
                self.focus_row(index);
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
                self.mode = VaultMode::Search;
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

    /// Also what leaving the tab runs, which is why the value goes with it.
    fn close_overlay(&mut self, _shell: &mut Shell) {
        self.mode = VaultMode::Browse;
        self.clear_reveal();
    }

    fn active_editor(&self) -> Option<TextEditor> {
        (self.mode == VaultMode::Search).then_some(TextEditor::Search)
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

    /// An item when the tab is showing one, the vault itself otherwise.
    fn here(&self, _shell: &Shell) -> Option<Jump> {
        match &self.level {
            Level::Vaults => self.selected_vault().map(|row| Jump::Vault(row.vault.name)),
            Level::Items(vault) => Some(self.selected_item().map_or_else(
                || Jump::Vault(vault.clone()),
                |row| Jump::VaultItem {
                    vault: vault.clone(),
                    kind: row.item.kind.as_str().to_owned(),
                    name: row.item.name,
                },
            )),
        }
    }

    fn select(&mut self, _shell: &mut Shell, jump: &Jump) -> bool {
        match jump {
            Jump::Vault(name) => self.select_vault(name),
            Jump::VaultItem { vault, kind, name } => {
                if !self.select_vault(vault) {
                    return false;
                }
                self.level = Level::Items(vault.clone());
                let position = |screen: &Self| {
                    screen
                        .visible_items()
                        .iter()
                        .position(|row| row.item.kind.as_str() == kind && row.item.name == *name)
                };
                let index = match position(self) {
                    Some(index) => index,
                    // On file but filtered out: the reference wins over the
                    // query, which is cleared rather than reported as a
                    // missing row.
                    None => {
                        self.item_query.clear();
                        match position(self) {
                            Some(index) => index,
                            // The vault has not been listed yet, so the level
                            // opens on it and the worker fills it in.
                            None => {
                                self.item_cursor.reset();
                                return true;
                            }
                        }
                    }
                };
                self.item_cursor.focus(index);
                true
            }
            _ => false,
        }
    }

    fn columns(&self) -> &dyn ColumnLayout {
        match self.level {
            Level::Vaults => &self.vaults_layout,
            Level::Items(_) => &self.items_layout,
        }
    }

    fn columns_mut(&mut self) -> &mut dyn ColumnLayout {
        match self.level {
            Level::Vaults => &mut self.vaults_layout,
            Level::Items(_) => &mut self.items_layout,
        }
    }

    /// What the session keeps: the top level's query, order and columns. A
    /// value is not part of it, and neither is the vault that was open — a
    /// reveal is worth one minute, not one run to the next.
    fn snapshot(&self) -> TabSession {
        TabSession {
            query: self.vault_query.text().to_owned(),
            sort_field: self.vault_sort.0.key().to_owned(),
            columns: self.vaults_layout.to_session_columns(),
            ..TabSession::default()
        }
    }

    fn restore(&mut self, _shell: &mut Shell, session: TabSession) {
        self.vault_query = TextInput::new(session.query);
        if let Some(column) = VaultColumn::from_key(&session.sort_field) {
            self.vault_sort = (column, self.vault_sort.1);
        }
        self.vaults_layout = TableLayout::from_session_columns(&session.columns);
    }

    /// `◇3`: certificates a month or less from lapsing, wherever they are.
    fn badge(&self) -> Option<String> {
        let expiring = self.expiring_certificates();
        (expiring > 0).then(|| format!("\u{25c7}{expiring}"))
    }

    fn footer_hint(&self, _shell: &Shell) -> &str {
        match (self.mode, &self.level) {
            (VaultMode::Search, _) => {
                "←→ cursor  Ctrl-W delete word  Ctrl-U clear  Enter/Esc finish"
            }
            (VaultMode::Browse, Level::Vaults) => {
                "↑↓/jk move  Enter open  / search  y copy  o portal  r refresh  ? help"
            }
            (VaultMode::Browse, Level::Items(_)) => {
                "↑↓/jk move  h back  R reveal  Y copy value  y copy name  o portal  ? help"
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
        crate::ui::key_vault::render(frame, self, shell, area);
    }
}
