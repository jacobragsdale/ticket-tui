//! What the ACR screen's two lists hold: a registry with what has been read
//! about it since, and one repository with the registry it lives in. Both are
//! what the filters read and what the table draws.

use crate::arm::{Registry, Repository};
use crate::filter::contains_ignore_case;

/// One registry, with the size of its catalog once that has come back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryRow {
    pub registry: Registry,
    /// How many repositories the catalog listed, or `None` until it has been
    /// read: a subscription listing carries no such count.
    pub repositories: Option<usize>,
}

impl RegistryRow {
    /// Whether the fuzzy half of a query — the words with no field in front of
    /// them — is in this row.
    #[must_use]
    pub fn matches_fuzzy(&self, needle: &str) -> bool {
        contains_ignore_case(&self.registry.name, needle)
            || contains_ignore_case(&self.registry.login_server, needle)
    }
}

/// One repository, with the name of the registry it is in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryRow {
    pub registry: String,
    pub repository: Repository,
}

impl RepositoryRow {
    #[must_use]
    pub fn matches_fuzzy(&self, needle: &str) -> bool {
        contains_ignore_case(&self.repository.name, needle)
            || contains_ignore_case(&self.registry, needle)
    }
}

/// A digest as a row shows it: `sha256:` dropped and cut to twelve
/// characters, which is what a person reads one by.
#[must_use]
pub fn short_digest(digest: &str) -> String {
    digest
        .split_once(':')
        .map_or(digest, |(_, hex)| hex)
        .chars()
        .take(12)
        .collect()
}
