//! The two filter grammars the Key Vault screen reads, one per level. Both are
//! ordinary [`FilterSchema`]s, so the search box and the chips work the same
//! way they do on work items — `expires:` included, which is a date and reads
//! the comparisons every other date field does.

use super::rows::{ItemRow, VaultRow};
use crate::filter::FilterSchema;
use crate::timestamp::Timestamp;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VaultSchema;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum VaultField {
    Name,
    ResourceGroup,
    Location,
}

impl FilterSchema for VaultSchema {
    type Field = VaultField;
    type Row = VaultRow;

    fn all() -> &'static [Self::Field] {
        &[
            VaultField::Name,
            VaultField::ResourceGroup,
            VaultField::Location,
        ]
    }

    fn bar() -> &'static [Self::Field] {
        &[VaultField::ResourceGroup, VaultField::Location]
    }

    fn parse(name: &str) -> Option<Self::Field> {
        match name.to_ascii_lowercase().as_str() {
            "name" | "vault" => Some(VaultField::Name),
            "rg" | "group" | "resourcegroup" => Some(VaultField::ResourceGroup),
            "location" | "region" => Some(VaultField::Location),
            _ => None,
        }
    }

    fn key(field: Self::Field) -> &'static str {
        match field {
            VaultField::Name => "name",
            VaultField::ResourceGroup => "rg",
            VaultField::Location => "location",
        }
    }

    fn label(field: Self::Field) -> &'static str {
        match field {
            VaultField::Name => "Vault",
            VaultField::ResourceGroup => "Resource group",
            VaultField::Location => "Location",
        }
    }

    fn values(field: Self::Field, row: &Self::Row) -> Vec<String> {
        match field {
            VaultField::Name => vec![row.vault.name.clone()],
            VaultField::ResourceGroup => vec![row.vault.resource_group.clone()],
            VaultField::Location => vec![row.vault.location.clone()],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ItemSchema;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ItemField {
    Name,
    Kind,
    Enabled,
    /// A date, compared rather than enumerated. `expires:<+30d` is everything
    /// falling due inside a month, which is the question this tab exists for.
    Expires,
}

impl FilterSchema for ItemSchema {
    type Field = ItemField;
    type Row = ItemRow;

    fn all() -> &'static [Self::Field] {
        &[
            ItemField::Name,
            ItemField::Kind,
            ItemField::Enabled,
            ItemField::Expires,
        ]
    }

    fn bar() -> &'static [Self::Field] {
        &[ItemField::Kind, ItemField::Enabled]
    }

    fn parse(name: &str) -> Option<Self::Field> {
        match name.to_ascii_lowercase().as_str() {
            "name" | "item" => Some(ItemField::Name),
            "kind" | "type" => Some(ItemField::Kind),
            "enabled" => Some(ItemField::Enabled),
            "expires" | "expiry" => Some(ItemField::Expires),
            _ => None,
        }
    }

    fn key(field: Self::Field) -> &'static str {
        match field {
            ItemField::Name => "name",
            ItemField::Kind => "kind",
            ItemField::Enabled => "enabled",
            ItemField::Expires => "expires",
        }
    }

    fn label(field: Self::Field) -> &'static str {
        match field {
            ItemField::Name => "Name",
            ItemField::Kind => "Kind",
            ItemField::Enabled => "Enabled",
            ItemField::Expires => "Expires",
        }
    }

    fn is_date(field: Self::Field) -> bool {
        field == ItemField::Expires
    }

    /// `yes` and `no` are what the column reads, and `true` and `false` are
    /// what the vault's own listing calls it; both are accepted so neither
    /// spelling is a query that quietly matches nothing.
    fn values(field: Self::Field, row: &Self::Row) -> Vec<String> {
        match field {
            ItemField::Name => vec![row.item.name.clone()],
            ItemField::Kind => vec![row.item.kind.as_str().to_owned()],
            ItemField::Enabled => {
                if row.item.enabled {
                    vec!["yes".to_owned(), "true".to_owned()]
                } else {
                    vec!["no".to_owned(), "false".to_owned()]
                }
            }
            // Compared rather than enumerated: `field_matches` answers for a
            // date before it reaches here.
            ItemField::Expires => Vec::new(),
        }
    }

    fn date_value(field: Self::Field, row: &Self::Row) -> Option<Timestamp> {
        match field {
            ItemField::Expires => row.item.expires,
            _ => None,
        }
    }
}
