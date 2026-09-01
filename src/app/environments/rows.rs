//! What one row of the environments board is: one service, and what each
//! environment says about it.

use crate::filter::contains_ignore_case;

/// What one environment says about one service. A cell that was never
/// rendered is not the same thing as an environment that does not deploy the
/// service, and the board colours the two apart.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EnvCell {
    /// The image tag the environment runs, and nothing where it has no such
    /// workload.
    pub tag: Option<String>,
    /// What this workload asks for in this environment that is not there,
    /// expiries apart: `env check`'s repository half and its vault half both.
    pub findings: usize,
    /// Vault objects it pulls that fall due inside the Key Vault tab's own
    /// thirty days.
    pub expiring: usize,
    /// Whether the environment's overlays rendered at all.
    pub rendered: bool,
}

impl EnvCell {
    /// Whether the environment holds this service and has nothing to say
    /// against it.
    #[must_use]
    pub const fn clean(&self) -> bool {
        self.rendered && self.tag.is_some() && self.findings == 0 && self.expiring == 0
    }
}

/// One service across every environment: the workload's name, where it lives,
/// and one cell per `[[environments]]` in file order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceRow {
    pub workload: String,
    pub kind: String,
    /// The namespace of the left-most environment that holds it. A service
    /// usually lands in a namespace of its own per environment, so one is
    /// enough to say what it is called; the diff names both.
    pub namespace: String,
    pub cells: Vec<EnvCell>,
}

impl ServiceRow {
    /// How many things this service is missing, across every environment,
    /// which is what the `Findings` filter reads.
    #[must_use]
    pub fn findings(&self) -> usize {
        self.cells.iter().map(|cell| cell.findings).sum()
    }

    /// Whether the fuzzy half of a query — the words with no field in front of
    /// them — is in this row.
    #[must_use]
    pub fn matches_fuzzy(&self, needle: &str) -> bool {
        contains_ignore_case(&self.workload, needle)
            || contains_ignore_case(&self.namespace, needle)
    }
}
