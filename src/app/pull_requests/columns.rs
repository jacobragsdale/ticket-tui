//! The Pull requests table's columns, and how each orders its rows.

use std::cmp::Ordering;

use super::rows::PrRow;
use crate::columns::ColumnId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrColumn {
    Id,
    Title,
    Repo,
    Branches,
    Author,
    Votes,
    Build,
    Age,
}

impl ColumnId for PrColumn {
    fn all() -> &'static [Self] {
        &[
            Self::Id,
            Self::Title,
            Self::Repo,
            Self::Branches,
            Self::Author,
            Self::Votes,
            Self::Build,
            Self::Age,
        ]
    }

    fn key(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Title => "title",
            Self::Repo => "repo",
            Self::Branches => "branches",
            Self::Author => "author",
            Self::Votes => "votes",
            Self::Build => "build",
            Self::Age => "age",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Id => "ID",
            Self::Title => "Title",
            Self::Repo => "Repo",
            Self::Branches => "Source → Target",
            Self::Author => "Author",
            Self::Votes => "Votes",
            Self::Build => "Build",
            Self::Age => "Age",
        }
    }

    fn default_width(self) -> u16 {
        match self {
            Self::Title => 0,
            Self::Id => 7,
            Self::Repo => 14,
            // `feature/tabs → main` and a little more.
            Self::Branches => 22,
            Self::Author => 16,
            Self::Votes => 8,
            Self::Build => 12,
            Self::Age => 8,
        }
    }

    fn default_visible(self) -> bool {
        true
    }

    fn right_aligned(self) -> bool {
        matches!(self, Self::Age)
    }

    fn pinned(self) -> bool {
        matches!(self, Self::Id | Self::Title)
    }

    fn flexible(self) -> bool {
        matches!(self, Self::Title)
    }
}

pub(super) fn compare(left: &PrRow, right: &PrRow, column: PrColumn) -> Ordering {
    match column {
        PrColumn::Id => left.request.id.cmp(&right.request.id),
        PrColumn::Title => text(&left.request.title, &right.request.title),
        PrColumn::Repo => text(&left.repo, &right.repo),
        PrColumn::Branches => text(&left.source_branch(), &right.source_branch()),
        PrColumn::Author => text(
            &left.request.created_by.display_name,
            &right.request.created_by.display_name,
        ),
        // Most approved first, so what is ready to go rises.
        PrColumn::Votes => left.vote_total().cmp(&right.vote_total()),
        PrColumn::Build => text(&left.build_word(), &right.build_word()),
        PrColumn::Age => left.changed_at().cmp(&right.changed_at()),
    }
}

fn text(left: &str, right: &str) -> Ordering {
    left.to_lowercase().cmp(&right.to_lowercase())
}
