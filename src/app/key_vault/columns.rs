//! The two column sets the Key Vault screen draws, and how each orders its
//! rows. Both are ordinary [`ColumnId`]s, so the table, the header sorting and
//! the Columns overlay come for nothing.

use std::cmp::Ordering;

use super::rows::{ItemRow, VaultRow};
use crate::columns::ColumnId;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VaultColumn {
    #[default]
    Name,
    ResourceGroup,
    Location,
    Sku,
}

impl ColumnId for VaultColumn {
    fn all() -> &'static [Self] {
        &[Self::Name, Self::ResourceGroup, Self::Location, Self::Sku]
    }

    fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::ResourceGroup => "group",
            Self::Location => "location",
            Self::Sku => "sku",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Vault",
            Self::ResourceGroup => "Resource group",
            Self::Location => "Location",
            Self::Sku => "SKU",
        }
    }

    fn default_width(self) -> u16 {
        match self {
            Self::Name => 0,
            Self::ResourceGroup => 20,
            Self::Location => 12,
            Self::Sku => 10,
        }
    }

    fn default_visible(self) -> bool {
        true
    }

    fn right_aligned(self) -> bool {
        false
    }

    fn pinned(self) -> bool {
        self == Self::Name
    }

    fn flexible(self) -> bool {
        self == Self::Name
    }

    /// A vault name is at most 24 characters, and shorter than that in
    /// practice.
    fn min_flexible_width(self) -> u16 {
        18
    }
}

/// One table over the three kinds a vault holds, because that is how a person
/// looks for one: by name, not by which listing it came out of.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ItemColumn {
    Kind,
    #[default]
    Name,
    Enabled,
    Updated,
    Expires,
}

impl ColumnId for ItemColumn {
    fn all() -> &'static [Self] {
        &[
            Self::Kind,
            Self::Name,
            Self::Enabled,
            Self::Updated,
            Self::Expires,
        ]
    }

    fn key(self) -> &'static str {
        match self {
            Self::Kind => "kind",
            Self::Name => "name",
            Self::Enabled => "enabled",
            Self::Updated => "updated",
            Self::Expires => "expires",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Kind => "Kind",
            Self::Name => "Name",
            Self::Enabled => "Enabled",
            Self::Updated => "Updated",
            Self::Expires => "Expires",
        }
    }

    fn default_width(self) -> u16 {
        match self {
            Self::Kind => 8,
            Self::Name => 0,
            Self::Enabled => 8,
            Self::Updated | Self::Expires => 22,
        }
    }

    fn default_visible(self) -> bool {
        true
    }

    fn right_aligned(self) -> bool {
        false
    }

    fn pinned(self) -> bool {
        self == Self::Name
    }

    fn flexible(self) -> bool {
        self == Self::Name
    }

    /// A vault item's name is a word or two with hyphens in it.
    fn min_flexible_width(self) -> u16 {
        18
    }
}

pub(super) fn compare_vaults(left: &VaultRow, right: &VaultRow, column: VaultColumn) -> Ordering {
    match column {
        VaultColumn::Name => compare_text(&left.vault.name, &right.vault.name),
        VaultColumn::ResourceGroup => {
            compare_text(&left.vault.resource_group, &right.vault.resource_group)
        }
        VaultColumn::Location => compare_text(&left.vault.location, &right.vault.location),
        VaultColumn::Sku => compare_text(&left.vault.sku, &right.vault.sku),
    }
}

/// Orders two items. An item with no expiry sorts last while the column is the
/// way round it opens in — soonest first, so what is about to lapse is at the
/// top — and the name breaks a tie, because two certificates renewed together
/// expire together.
pub(super) fn compare_items(left: &ItemRow, right: &ItemRow, column: ItemColumn) -> Ordering {
    match column {
        ItemColumn::Kind => left
            .item
            .kind
            .as_str()
            .cmp(right.item.kind.as_str())
            .then_with(|| compare_text(&left.item.name, &right.item.name)),
        ItemColumn::Name => compare_text(&left.item.name, &right.item.name),
        ItemColumn::Enabled => left.item.enabled.cmp(&right.item.enabled),
        ItemColumn::Updated => left.item.updated.cmp(&right.item.updated),
        ItemColumn::Expires => left
            .item
            .expires
            .is_none()
            .cmp(&right.item.expires.is_none())
            .then_with(|| left.item.expires.cmp(&right.item.expires))
            .then_with(|| compare_text(&left.item.name, &right.item.name)),
    }
}

fn compare_text(left: &str, right: &str) -> Ordering {
    left.to_lowercase().cmp(&right.to_lowercase())
}
