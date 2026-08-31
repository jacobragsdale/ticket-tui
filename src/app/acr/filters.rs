//! The two filter grammars the ACR screen reads, one per level. Both are
//! ordinary [`FilterSchema`]s, so the search box, the chips and the facet bar
//! work the same way they do on work items.

use super::rows::{RegistryRow, RepositoryRow};
use crate::filter::FilterSchema;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RegistrySchema;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RegistryField {
    Name,
    ResourceGroup,
    Sku,
    Location,
}

impl FilterSchema for RegistrySchema {
    type Field = RegistryField;
    type Row = RegistryRow;

    fn all() -> &'static [Self::Field] {
        &[
            RegistryField::Name,
            RegistryField::ResourceGroup,
            RegistryField::Sku,
            RegistryField::Location,
        ]
    }

    fn bar() -> &'static [Self::Field] {
        &[
            RegistryField::ResourceGroup,
            RegistryField::Sku,
            RegistryField::Location,
        ]
    }

    fn parse(name: &str) -> Option<Self::Field> {
        match name.to_ascii_lowercase().as_str() {
            "name" | "registry" => Some(RegistryField::Name),
            "rg" | "group" | "resourcegroup" => Some(RegistryField::ResourceGroup),
            "sku" => Some(RegistryField::Sku),
            "location" | "region" => Some(RegistryField::Location),
            _ => None,
        }
    }

    fn key(field: Self::Field) -> &'static str {
        match field {
            RegistryField::Name => "name",
            RegistryField::ResourceGroup => "rg",
            RegistryField::Sku => "sku",
            RegistryField::Location => "location",
        }
    }

    fn label(field: Self::Field) -> &'static str {
        match field {
            RegistryField::Name => "Registry",
            RegistryField::ResourceGroup => "Resource group",
            RegistryField::Sku => "SKU",
            RegistryField::Location => "Location",
        }
    }

    fn values(field: Self::Field, row: &Self::Row) -> Vec<String> {
        match field {
            RegistryField::Name => vec![row.registry.name.clone()],
            RegistryField::ResourceGroup => vec![row.registry.resource_group.clone()],
            RegistryField::Sku => vec![row.registry.sku.clone()],
            RegistryField::Location => vec![row.registry.location.clone()],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RepositorySchema;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RepositoryField {
    Name,
}

impl FilterSchema for RepositorySchema {
    type Field = RepositoryField;
    type Row = RepositoryRow;

    fn all() -> &'static [Self::Field] {
        &[RepositoryField::Name]
    }

    /// A catalog has one field worth a facet, and it is the one already in the
    /// search box, so the bar stays empty.
    fn bar() -> &'static [Self::Field] {
        &[]
    }

    fn parse(name: &str) -> Option<Self::Field> {
        match name.to_ascii_lowercase().as_str() {
            "name" | "repo" | "repository" => Some(RepositoryField::Name),
            _ => None,
        }
    }

    fn key(_field: Self::Field) -> &'static str {
        "name"
    }

    fn label(_field: Self::Field) -> &'static str {
        "Repository"
    }

    fn values(_field: Self::Field, row: &Self::Row) -> Vec<String> {
        vec![row.repository.name.clone()]
    }
}
