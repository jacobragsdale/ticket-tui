//! What the Pull requests table holds: one pull request with the name of the
//! repository it is against.

use crate::model::{PrStatus, PullRequest};
use crate::timestamp::Timestamp;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrRow {
    pub request: PullRequest,
    /// What the repository is called, or its GUID before the pull has brought
    /// the repositories down.
    pub repo: String,
}

impl PrRow {
    #[must_use]
    pub fn source_branch(&self) -> String {
        short_ref(&self.request.source_ref)
    }

    #[must_use]
    pub fn target_branch(&self) -> String {
        short_ref(&self.request.target_ref)
    }

    /// `feature/x → main`, which is what the column says.
    #[must_use]
    pub fn branches(&self) -> String {
        format!("{} \u{2192} {}", self.source_branch(), self.target_branch())
    }

    /// The votes as a run of glyphs, required reviewers first, which is what
    /// the Votes column paints.
    #[must_use]
    pub fn vote_glyphs(&self) -> Vec<(&'static str, i8)> {
        let mut reviewers: Vec<_> = self.request.reviewers.iter().collect();
        reviewers.sort_by_key(|reviewer| !reviewer.is_required);
        reviewers
            .into_iter()
            .map(|reviewer| (reviewer.glyph(), reviewer.vote))
            .collect()
    }

    /// The sum of the votes, for ordering: what is closest to ready first.
    #[must_use]
    pub fn vote_total(&self) -> i32 {
        self.request
            .reviewers
            .iter()
            .map(|reviewer| i32::from(reviewer.vote))
            .sum()
    }

    /// How the build reads: the policy's word, `conflicts` when the merge is
    /// blocked, or nothing at all.
    #[must_use]
    pub fn build_word(&self) -> String {
        if self.request.has_conflicts() {
            return "conflicts".to_owned();
        }
        self.request
            .build
            .as_ref()
            .map_or_else(String::new, |build| build.status.clone())
    }

    /// What the Age column measures: when it closed, or when it was raised.
    #[must_use]
    pub fn changed_at(&self) -> Option<Timestamp> {
        self.request.closed_at.or(self.request.created_at)
    }

    /// The word `status:` filters on.
    #[must_use]
    pub const fn status_word(&self) -> &'static str {
        self.request.status.as_str()
    }

    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(
            self.request.status,
            PrStatus::Completed | PrStatus::Abandoned
        )
    }

    #[must_use]
    pub fn matches_fuzzy(&self, needle: &str) -> bool {
        let needle = needle.trim().to_lowercase();
        needle.is_empty()
            || self.request.title.to_lowercase().contains(&needle)
            || self.request.id.to_string().contains(&needle)
            || self.repo.to_lowercase().contains(&needle)
            || self
                .request
                .created_by
                .display_name
                .to_lowercase()
                .contains(&needle)
            || self.source_branch().to_lowercase().contains(&needle)
    }
}

/// `refs/heads/feature/x` is `feature/x`; anything else is left as it is.
#[must_use]
pub fn short_ref(reference: &str) -> String {
    reference
        .strip_prefix("refs/heads/")
        .unwrap_or(reference)
        .to_owned()
}
