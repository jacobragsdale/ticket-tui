//! One repository as the Repos table sees it: what Azure DevOps says about it,
//! what the other tabs have against it, and what it looks like on this machine.

use crate::model::{LocalRepo, Repo, Run};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoRow {
    pub repo: Repo,
    pub local: Option<LocalRepo>,
    pub pull_requests: usize,
    pub pipelines: usize,
    /// How the repository's pipelines are going: the worst of their last runs,
    /// or `None` while none of them has run.
    pub build: Option<Run>,
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
