//! The Pull requests tab's filter grammar.

use super::rows::PrRow;
use crate::filter::{FilterSchema, MatchContext, Sentinel};
use crate::model::same_text;
use crate::timestamp::Timestamp;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PrSchema;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PrField {
    Repo,
    Author,
    /// Somebody asked to review it, where `@me` is the signed-in user.
    Reviewer,
    /// The signed-in user's own vote: `approved`, `suggestions`, `waiting`,
    /// `rejected`, `none`.
    Vote,
    Status,
    Target,
    Source,
    Draft,
    /// What the branch policy's build says: `succeeded`, `running`, `failed`,
    /// or `none` for a pull request no build gates.
    Build,
}

impl FilterSchema for PrSchema {
    type Field = PrField;
    type Row = PrRow;

    fn all() -> &'static [Self::Field] {
        &[
            PrField::Repo,
            PrField::Author,
            PrField::Reviewer,
            PrField::Vote,
            PrField::Status,
            PrField::Target,
            PrField::Source,
            PrField::Draft,
            PrField::Build,
        ]
    }

    fn bar() -> &'static [Self::Field] {
        &[
            PrField::Repo,
            PrField::Author,
            PrField::Vote,
            PrField::Status,
        ]
    }

    fn parse(name: &str) -> Option<Self::Field> {
        match name.to_ascii_lowercase().as_str() {
            "repo" | "repository" => Some(PrField::Repo),
            "author" | "by" => Some(PrField::Author),
            "reviewer" => Some(PrField::Reviewer),
            "vote" => Some(PrField::Vote),
            "status" | "state" => Some(PrField::Status),
            "target" | "into" => Some(PrField::Target),
            "source" | "from" | "branch" => Some(PrField::Source),
            "draft" => Some(PrField::Draft),
            "build" => Some(PrField::Build),
            _ => None,
        }
    }

    fn key(field: Self::Field) -> &'static str {
        match field {
            PrField::Repo => "repo",
            PrField::Author => "author",
            PrField::Reviewer => "reviewer",
            PrField::Vote => "vote",
            PrField::Status => "status",
            PrField::Target => "target",
            PrField::Source => "source",
            PrField::Draft => "draft",
            PrField::Build => "build",
        }
    }

    fn label(field: Self::Field) -> &'static str {
        match field {
            PrField::Repo => "Repository",
            PrField::Author => "Author",
            PrField::Reviewer => "Reviewer",
            PrField::Vote => "My vote",
            PrField::Status => "Status",
            PrField::Target => "Target",
            PrField::Source => "Source",
            PrField::Draft => "Draft",
            PrField::Build => "Build",
        }
    }

    fn is_date(_field: Self::Field) -> bool {
        false
    }

    fn values(field: Self::Field, row: &Self::Row) -> Vec<String> {
        match field {
            PrField::Repo => vec![row.repo.clone()],
            PrField::Author => vec![row.request.created_by.display_name.clone()],
            PrField::Reviewer => row
                .request
                .reviewers
                .iter()
                .map(|reviewer| reviewer.display_name.clone())
                .collect(),
            // Whose vote `vote:` means is settled at match time by the
            // sentinel; the plain values are every vote on it.
            PrField::Vote => row
                .request
                .reviewers
                .iter()
                .map(|reviewer| vote_word(reviewer.vote).to_owned())
                .collect(),
            PrField::Status => vec![row.status_word().to_owned()],
            PrField::Target => vec![row.target_branch()],
            PrField::Source => vec![row.source_branch()],
            PrField::Draft => vec![if row.request.is_draft { "yes" } else { "no" }.to_owned()],
            PrField::Build => vec![if row.build_word().is_empty() {
                "none".to_owned()
            } else {
                row.build_word()
            }],
        }
    }

    fn date_value(_field: Self::Field, _row: &Self::Row) -> Option<Timestamp> {
        None
    }

    fn sentinel(field: Self::Field, value: &str) -> Option<Sentinel> {
        let name = value.strip_prefix('@')?.to_ascii_lowercase();
        match (field, name.as_str()) {
            (PrField::Author | PrField::Reviewer, "me") => Some(Sentinel::Me),
            _ => None,
        }
    }

    /// `author:@me` is what I raised; `reviewer:@me` is what is asking me.
    fn matches_sentinel(
        field: Self::Field,
        sentinel: Sentinel,
        row: &Self::Row,
        context: &MatchContext,
    ) -> bool {
        let Sentinel::Me = sentinel else {
            return false;
        };
        let Some(me) = context.me.as_deref() else {
            return false;
        };
        match field {
            PrField::Author => same_text(&row.request.created_by.display_name, me),
            PrField::Reviewer => row
                .request
                .reviewers
                .iter()
                .any(|reviewer| same_text(&reviewer.display_name, me)),
            _ => false,
        }
    }
}

/// What a vote is called in a query.
#[must_use]
pub const fn vote_word(vote: i8) -> &'static str {
    match vote {
        10 => "approved",
        5 => "suggestions",
        -5 => "waiting",
        -10 => "rejected",
        _ => "none",
    }
}
