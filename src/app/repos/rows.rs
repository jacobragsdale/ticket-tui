//! One repository as the Repos table sees it: what Azure DevOps says about it,
//! what the other tabs have against it, and what it looks like on this machine.

use crate::model::{ArtifactLink, LocalRepo, Repo, Run, StateCategory, Ticket, TicketKey};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoRow {
    pub repo: Repo,
    pub local: Option<LocalRepo>,
    pub pull_requests: usize,
    pub pipelines: usize,
    /// How many open work items are linked to it, through a branch, a pull
    /// request or a commit.
    pub work_items: usize,
    /// The work item whose branch the clone is checked out on, if any.
    pub branch_item: Option<i64>,
    /// How the repository's pipelines are going: the worst of their last runs,
    /// or `None` while none of them has run.
    pub build: Option<Run>,
}

/// One open work item linked to a repository, as the Repos tab and
/// `ticket-tui repos` both read it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoWorkItem {
    pub repo_id: String,
    pub key: TicketKey,
    pub title: String,
    /// The branch its Branch link names, when it has one.
    pub branch: Option<String>,
}

/// The open work items each repository is linked to, once per (repository,
/// work item) however many links say so, a Branch link's name kept when
/// there is one. Ordered by repository, then work item.
#[must_use]
pub fn repo_work_items(artifacts: &[ArtifactLink], tickets: &[Ticket]) -> Vec<RepoWorkItem> {
    let mut items: Vec<RepoWorkItem> = Vec::new();
    for link in artifacts {
        let Some(repo_id) = link.kind.repo_id() else {
            continue;
        };
        let Some(ticket) = tickets.iter().find(|ticket| ticket.key == link.work_item) else {
            continue;
        };
        if StateCategory::of(&ticket.state).is_done() {
            continue;
        }
        let branch = match &link.kind {
            crate::model::ArtifactKind::Branch { name, .. } => Some(name.clone()),
            _ => None,
        };
        match items
            .iter_mut()
            .find(|item| item.repo_id == repo_id && item.key == link.work_item)
        {
            Some(item) => {
                if item.branch.is_none() {
                    item.branch = branch;
                }
            }
            None => items.push(RepoWorkItem {
                repo_id: repo_id.to_owned(),
                key: ticket.key.clone(),
                title: ticket.title.clone(),
                branch,
            }),
        }
    }
    items.sort_by(|left, right| {
        left.repo_id
            .cmp(&right.repo_id)
            .then_with(|| left.key.id.cmp(&right.key.id))
    });
    items
}

/// The work item whose branch a clone is on: the one linked to this
/// repository by a branch of exactly that name.
#[must_use]
pub fn branch_item(
    items: &[RepoWorkItem],
    repo_id: &str,
    local: Option<&LocalRepo>,
) -> Option<i64> {
    let branch = local.map(|local| local.branch.as_str())?;
    items
        .iter()
        .find(|item| item.repo_id == repo_id && item.branch.as_deref() == Some(branch))
        .map(|item| item.key.id)
}

impl RepoRow {
    /// The default branch without its ref prefix.
    #[must_use]
    pub fn branch(&self) -> String {
        self.repo
            .default_branch
            .as_deref()
            .unwrap_or_default()
            .strip_prefix("refs/heads/")
            .unwrap_or_else(|| self.repo.default_branch.as_deref().unwrap_or_default())
            .to_owned()
    }

    /// The words `local:` filters on.
    #[must_use]
    pub fn local_words(&self) -> Vec<String> {
        let Some(local) = self.local.as_ref() else {
            return vec!["missing".to_owned()];
        };
        let mut words = vec!["cloned".to_owned()];
        if local.dirty {
            words.push("dirty".to_owned());
        }
        if local.ahead > 0 {
            words.push("ahead".to_owned());
        }
        if local.behind > 0 {
            words.push("behind".to_owned());
        }
        words
    }

    #[must_use]
    pub fn matches_fuzzy(&self, needle: &str) -> bool {
        crate::filter::contains_ignore_case(&self.repo.name, needle)
            || crate::filter::contains_ignore_case(&self.branch(), needle)
    }
}
