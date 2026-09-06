//! Linking a work item to a branch of a repository: `L`, which asks for the
//! repository, then for the branch — one the repository has, or a new name
//! to make at the head of its default branch.

use super::pickers::fuzzy_contains;
use std::collections::BTreeMap;

use super::*;

/// The two-step picker `L` opens. `repo` is `None` while the repositories are
/// listed and `Some` once one is chosen and its branches are wanted.
#[derive(Clone, Debug, Default)]
pub struct LinkPicker {
    /// The work item being linked, and its title, which the offered branch
    /// name is made from.
    pub item: Option<TicketKey>,
    pub title: String,
    /// The repositories to choose from: the project's, less the disabled.
    pub repos: Vec<Repo>,
    /// The chosen repository, as an index into `repos`.
    pub repo: Option<usize>,
    /// The chosen repository's branches, once the worker has read them.
    pub branches: Vec<String>,
    pub loaded: bool,
    pub query: TextInput,
    pub cursor: ListCursor,
}

impl LinkPicker {
    /// The repository chosen, once one is.
    #[must_use]
    pub fn chosen(&self) -> Option<&Repo> {
        self.repo.and_then(|index| self.repos.get(index))
    }
}

impl WorkItemsScreen {
    /// `L`: the repositories the selected work item could be linked to.
    pub(super) fn open_link_picker(&mut self, shell: &mut Shell) {
        let Some(ticket) = self.selected_ticket() else {
            shell.set_error("No work item is selected");
            return;
        };
        let repos: Vec<Repo> = shell
            .repos()
            .iter()
            .filter(|repo| !repo.is_disabled)
            .cloned()
            .collect();
        if repos.is_empty() {
            shell.set_error("No repository is on file; sync first");
            return;
        }
        self.link_picker = LinkPicker {
            item: Some(ticket.key.clone()),
            title: ticket.title.clone(),
            repos,
            ..LinkPicker::default()
        };
        self.mode = WorkItemMode::LinkPicker;
    }

    /// What the picker lists under its query: repository names, then the
    /// chosen repository's branches. A branch spelt exactly as typed comes
    /// first, so `main` is under the cursor rather than `maintenance`.
    #[must_use]
    pub fn link_matches(&self) -> Vec<String> {
        let picker = &self.link_picker;
        let query = picker.query.text().trim();
        let names: Vec<&str> = match picker.repo {
            None => picker.repos.iter().map(|repo| repo.name.as_str()).collect(),
            Some(_) => picker.branches.iter().map(String::as_str).collect(),
        };
        let mut matches: Vec<String> = names
            .into_iter()
            .filter(|name| query.is_empty() || fuzzy_contains(name, query))
            .map(str::to_owned)
            .collect();
        matches.sort_by_key(|name| name != query);
        matches
    }

    pub(super) fn handle_link_picker_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.link_picker_back(),
            KeyCode::Up => self.move_link_selection(-1),
            KeyCode::Down => self.move_link_selection(1),
            KeyCode::PageUp => self.move_link_selection(-5),
            KeyCode::PageDown => self.move_link_selection(5),
            KeyCode::Enter => return self.choose_link(shell, self.link_picker.cursor.index),
            // Everything else is typing, the way it is in the parent picker.
            _ => {
                let before = self.link_picker.query.text().to_owned();
                self.link_picker.query.handle_key(key);
                if self.link_picker.query.text() != before {
                    self.link_picker.cursor.reset();
                }
            }
        }
        AppAction::None
    }

    /// `Esc`: from the branches back to the repositories, and from those out.
    fn link_picker_back(&mut self) {
        if self.link_picker.repo.is_some() {
            self.link_picker.repo = None;
            self.link_picker.branches.clear();
            self.link_picker.loaded = false;
            self.link_picker.query = TextInput::default();
            self.link_picker.cursor.reset();
        } else {
            self.mode = WorkItemMode::Browse;
        }
    }

    fn move_link_selection(&mut self, delta: isize) {
        let count = self.link_matches().len();
        self.link_picker.cursor.move_by(delta, count);
    }

    /// `Enter`: a repository chosen asks for its branches; a branch chosen is
    /// linked; a name no branch has is made at the head of the default branch,
    /// then linked. Nothing is sent until the branches have been read, or a
    /// name that is there would be made again.
    pub(super) fn choose_link(&mut self, shell: &mut Shell, index: usize) -> AppAction {
        let chosen = self.link_matches().get(index).cloned();
        let Some(item) = self.link_picker.item.clone() else {
            self.mode = WorkItemMode::Browse;
            return AppAction::None;
        };
        let Some(repo_index) = self.link_picker.repo else {
            let picker = &mut self.link_picker;
            let Some(position) =
                chosen.and_then(|name| picker.repos.iter().position(|repo| repo.name == name))
            else {
                return AppAction::None;
            };
            picker.repo = Some(position);
            picker.query = TextInput::new(branch_name(item.id, &picker.title));
            picker.branches.clear();
            picker.loaded = false;
            picker.cursor.reset();
            return AppAction::FetchBranches(picker.repos[position].id.clone());
        };
        let repo = self.link_picker.repos[repo_index].clone();
        if !self.link_picker.loaded {
            shell.set_status(format!(
                "Still reading the branches of {}\u{2026}",
                repo.name
            ));
            return AppAction::None;
        }
        let (branch, create_from) = match chosen {
            Some(branch) => (branch, None),
            None => {
                let typed = self.link_picker.query.text().trim();
                let typed = typed
                    .strip_prefix("refs/heads/")
                    .unwrap_or(typed)
                    .to_owned();
                if typed.is_empty() {
                    return AppAction::None;
                }
                let Some(from) = repo.default_branch.clone() else {
                    shell.set_error(format!(
                        "{} has no default branch to branch from",
                        repo.name
                    ));
                    return AppAction::None;
                };
                (typed, Some(from))
            }
        };
        self.mode = WorkItemMode::Browse;
        shell.set_status(format!(
            "Linking #{} to {}/{branch}\u{2026}",
            item.id, repo.name
        ));
        AppAction::LinkBranch {
            repo_id: repo.id,
            branch,
            work_item: item.id,
            create_from,
        }
    }

    /// The branches the worker read, for the picker waiting on them. Any
    /// other repository's, or a picker no longer open, are let go: a stale
    /// list is what would make `Enter` remake a branch that exists.
    pub fn set_branches(&mut self, repo_id: &str, branches: &[String]) {
        if self.mode == WorkItemMode::LinkPicker
            && self
                .link_picker
                .chosen()
                .is_some_and(|repo| repo.id == repo_id)
        {
            self.link_picker.branches = branches.to_vec();
            self.link_picker.loaded = true;
            self.link_picker.cursor.reset();
        }
    }

    /// The repositories each work item is linked to, by name, off the graph's
    /// artifact links: what `repo:` matches and the Repo column shows.
    #[must_use]
    pub fn repos_by_item(&self, shell: &Shell) -> BTreeMap<i64, Vec<String>> {
        crate::filter::repos_by_item(&self.graph.artifacts, |id| shell.repo_name(id))
    }

    /// A branch link Azure DevOps took. The work item's own Related section
    /// follows at the pull booked for it.
    pub fn apply_branch_link(
        &self,
        shell: &mut Shell,
        work_item: i64,
        repo_id: &str,
        branch: &str,
    ) {
        shell.set_status(format!(
            "#{work_item} linked to {}/{branch}",
            shell.repo_name(repo_id)
        ));
    }
}

/// The branch a work item's own work goes on when nobody names one:
/// `{id}-{slug}`, the slug being the title lowercased with every run of
/// characters that are not ASCII letters or digits made one `-`, at most
/// forty characters, so `#715 Fix the thing!` is `715-fix-the-thing`.
#[must_use]
pub fn branch_name(id: i64, title: &str) -> String {
    let mut slug = String::new();
    for ch in title.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.chars().take(40).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        id.to_string()
    } else {
        format!("{id}-{slug}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_name_is_the_id_and_a_forty_character_slug() {
        assert_eq!(branch_name(715, "Fix the thing!"), "715-fix-the-thing");
        assert_eq!(
            branch_name(
                715,
                "  Réécrire — the (whole) sync/path, again & again & again "
            ),
            "715-r-crire-the-whole-sync-path-again-again",
            "non-ASCII letters go, runs of punctuation are one dash, and the slug stops at forty"
        );
        assert_eq!(
            branch_name(715, "???"),
            "715",
            "a title with no letters is the id alone"
        );
    }
}
