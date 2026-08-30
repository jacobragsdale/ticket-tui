//! The Repos table's columns.

use std::cmp::Ordering;

use super::rows::RepoRow;
use crate::columns::ColumnId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepoColumn {
    Name,
    DefaultBranch,
    PullRequests,
    Pipelines,
    Local,
}

impl ColumnId for RepoColumn {
    fn all() -> &'static [Self] {
        &[
            Self::Name,
            Self::DefaultBranch,
            Self::PullRequests,
            Self::Pipelines,
            Self::Local,
        ]
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|column| column.key() == key)
    }

    fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::DefaultBranch => "branch",
            Self::PullRequests => "prs",
            Self::Pipelines => "pipelines",
            Self::Local => "local",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::DefaultBranch => "Default branch",
            Self::PullRequests => "PRs",
            Self::Pipelines => "Pipelines",
            Self::Local => "Local",
        }
    }

    fn default_width(self) -> u16 {
        match self {
            Self::Name => 0,
            Self::DefaultBranch => 18,
            Self::PullRequests | Self::Pipelines => 10,
            Self::Local => 20,
        }
    }

    fn default_visible(self) -> bool {
        true
    }

    fn right_aligned(self) -> bool {
        matches!(self, Self::PullRequests | Self::Pipelines)
    }

    fn pinned(self) -> bool {
        matches!(self, Self::Name)
    }

    fn flexible(self) -> bool {
        matches!(self, Self::Name)
    }

    /// A repository name is a slug, not a sentence.
    fn min_flexible_width(self) -> u16 {
        16
    }
}

pub(super) fn compare(left: &RepoRow, right: &RepoRow, column: RepoColumn) -> Ordering {
    match column {
        RepoColumn::Name => left
            .repo
            .name
            .to_lowercase()
            .cmp(&right.repo.name.to_lowercase()),
        RepoColumn::DefaultBranch => left
            .branch()
            .to_lowercase()
            .cmp(&right.branch().to_lowercase()),
        RepoColumn::PullRequests => left.pull_requests.cmp(&right.pull_requests),
        RepoColumn::Pipelines => left.pipelines.cmp(&right.pipelines),
        // Cloned first, then whatever is not here: what you can act on leads.
        RepoColumn::Local => left.local.is_none().cmp(&right.local.is_none()),
    }
}
