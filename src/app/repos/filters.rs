//! The Repos tab's filter grammar.

use super::rows::RepoRow;
use crate::filter::{FilterSchema, MatchContext, Sentinel};
use crate::timestamp::Timestamp;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RepoSchema;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RepoField {
    Name,
    Branch,
    /// `cloned`, `dirty`, `ahead`, `behind`, `missing`.
    Local,
    Disabled,
}

impl FilterSchema for RepoSchema {
    type Field = RepoField;
    type Row = RepoRow;

    fn all() -> &'static [Self::Field] {
        &[
            RepoField::Name,
            RepoField::Branch,
            RepoField::Local,
            RepoField::Disabled,
        ]
    }

    fn bar() -> &'static [Self::Field] {
        &[RepoField::Local, RepoField::Branch]
    }

    fn parse(name: &str) -> Option<Self::Field> {
        match name.to_ascii_lowercase().as_str() {
            "name" | "repo" => Some(RepoField::Name),
            "branch" | "default" => Some(RepoField::Branch),
            "local" => Some(RepoField::Local),
            "disabled" => Some(RepoField::Disabled),
            _ => None,
        }
    }

    fn key(field: Self::Field) -> &'static str {
        match field {
            RepoField::Name => "name",
            RepoField::Branch => "branch",
            RepoField::Local => "local",
            RepoField::Disabled => "disabled",
        }
    }

    fn label(field: Self::Field) -> &'static str {
        match field {
            RepoField::Name => "Name",
            RepoField::Branch => "Default branch",
            RepoField::Local => "Local",
            RepoField::Disabled => "Disabled",
        }
    }

    fn is_date(_field: Self::Field) -> bool {
        false
    }

    fn values(field: Self::Field, row: &Self::Row) -> Vec<String> {
        match field {
            RepoField::Name => vec![row.repo.name.clone()],
            RepoField::Branch => vec![row.branch()],
            RepoField::Local => row.local_words(),
            RepoField::Disabled => {
                vec![if row.repo.is_disabled { "yes" } else { "no" }.to_owned()]
            }
        }
    }

    fn date_value(_field: Self::Field, _row: &Self::Row) -> Option<Timestamp> {
        None
    }

    fn sentinel(_field: Self::Field, _value: &str) -> Option<Sentinel> {
        None
    }

    fn matches_sentinel(
        _field: Self::Field,
        _sentinel: Sentinel,
        _row: &Self::Row,
        _context: &MatchContext,
    ) -> bool {
        false
    }
}
