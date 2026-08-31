//! What the Key Vault screen's two lists hold: a vault with what has been read
//! about it since, and one thing inside it with the vault it lives in. Both are
//! what the filters read and what the table draws.
//!
//! A row never carries a value. A listing does not bring one back, and the one
//! value a run ever reads lives on the screen for a minute and nowhere else.

use crate::arm::{Vault, VaultItem};
use crate::filter::contains_ignore_case;
use crate::timestamp::Timestamp;

/// How near an expiry has to be before it is worth a colour. Thirty days is
/// the notice a certificate renewal wants, and it is inclusive: the day it
/// falls on counts.
pub const EXPIRING_WITHIN: i64 = 30 * 24 * 60 * 60;

/// What an expiry is worth saying about it, or `None` for one far enough off
/// to be nobody's problem today.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Expiry {
    /// The date has been and gone.
    Past,
    /// Within [`EXPIRING_WITHIN`] of now.
    Soon,
}

impl Expiry {
    /// How an expiry reads against the clock. An item with no expiry never
    /// lapses, so it is nothing to say.
    #[must_use]
    pub fn of(expires: Option<Timestamp>, now: Timestamp) -> Option<Self> {
        let expires = expires?;
        if expires <= now {
            return Some(Self::Past);
        }
        (now.seconds_until(expires) <= EXPIRING_WITHIN).then_some(Self::Soon)
    }
}

/// One vault, with the size of its contents once they have been read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultRow {
    pub vault: Vault,
    /// How many things the vault holds, or `None` until they have been listed:
    /// a subscription listing carries no such count.
    pub items: Option<usize>,
}

impl VaultRow {
    /// Whether the fuzzy half of a query — the words with no field in front of
    /// them — is in this row.
    #[must_use]
    pub fn matches_fuzzy(&self, needle: &str) -> bool {
        contains_ignore_case(&self.vault.name, needle)
            || contains_ignore_case(&self.vault.resource_group, needle)
    }
}

/// One secret, key or certificate, with the name of the vault it is in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemRow {
    pub vault: String,
    pub item: VaultItem,
}

impl ItemRow {
    #[must_use]
    pub fn matches_fuzzy(&self, needle: &str) -> bool {
        contains_ignore_case(&self.item.name, needle)
            || contains_ignore_case(self.item.kind.as_str(), needle)
    }
}
