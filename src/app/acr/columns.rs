//! The two column sets the ACR screen draws, and how each orders its rows.
//! Both are ordinary [`ColumnId`]s, so the table, the header sorting and the
//! Columns overlay come for nothing.

use std::cmp::Ordering;

use super::rows::{RegistryRow, RepositoryRow};
use crate::columns::ColumnId;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RegistryColumn {
    #[default]
    Name,
    ResourceGroup,
    Sku,
    Location,
    LoginServer,
}

impl ColumnId for RegistryColumn {
    fn all() -> &'static [Self] {
        &[
            Self::Name,
            Self::ResourceGroup,
            Self::Sku,
            Self::Location,
            Self::LoginServer,
        ]
    }

    fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::ResourceGroup => "group",
            Self::Sku => "sku",
            Self::Location => "location",
            Self::LoginServer => "login-server",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Registry",
            Self::ResourceGroup => "Resource group",
            Self::Sku => "SKU",
            Self::Location => "Location",
            Self::LoginServer => "Login server",
        }
    }

    fn default_width(self) -> u16 {
        match self {
            Self::Name => 0,
            Self::ResourceGroup => 20,
            Self::Sku => 8,
            Self::Location => 12,
            Self::LoginServer => 28,
        }
    }

    /// The login server is the registry's own name with a suffix on it, so it
    /// is worth asking for rather than worth the width by default.
    fn default_visible(self) -> bool {
        self != Self::LoginServer
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

    /// A registry name is shorter than a work item's title.
    fn min_flexible_width(self) -> u16 {
        18
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RepositoryColumn {
    #[default]
    Name,
    Tags,
    Updated,
}

impl ColumnId for RepositoryColumn {
    fn all() -> &'static [Self] {
        &[Self::Name, Self::Tags, Self::Updated]
    }

    fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Tags => "tags",
            Self::Updated => "updated",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Repository",
            Self::Tags => "Tags",
            Self::Updated => "Updated",
        }
    }

    fn default_width(self) -> u16 {
        match self {
            Self::Name => 0,
            Self::Tags => 7,
            Self::Updated => 22,
        }
    }

    fn default_visible(self) -> bool {
        true
    }

    fn right_aligned(self) -> bool {
        self == Self::Tags
    }

    fn pinned(self) -> bool {
        self == Self::Name
    }

    fn flexible(self) -> bool {
        self == Self::Name
    }

    /// A repository name carries its team's path in front of it.
    fn min_flexible_width(self) -> u16 {
        20
    }
}

pub(super) fn compare_registries(
    left: &RegistryRow,
    right: &RegistryRow,
    column: RegistryColumn,
) -> Ordering {
    match column {
        RegistryColumn::Name => compare_text(&left.registry.name, &right.registry.name),
        RegistryColumn::ResourceGroup => compare_text(
            &left.registry.resource_group,
            &right.registry.resource_group,
        ),
        RegistryColumn::Sku => compare_text(&left.registry.sku, &right.registry.sku),
        RegistryColumn::Location => compare_text(&left.registry.location, &right.registry.location),
        RegistryColumn::LoginServer => {
            compare_text(&left.registry.login_server, &right.registry.login_server)
        }
    }
}

/// Orders two repositories. A repository whose attributes have not landed yet
/// has no count and no stamp to compare, and sorts last whichever way the
/// column is turned, the way a pipeline that has never run does.
pub(super) fn compare_repositories(
    left: &RepositoryRow,
    right: &RepositoryRow,
    column: RepositoryColumn,
) -> Ordering {
    match column {
        RepositoryColumn::Name => compare_text(&left.repository.name, &right.repository.name),
        RepositoryColumn::Tags => left.repository.tags.cmp(&right.repository.tags),
        RepositoryColumn::Updated => left.repository.updated.cmp(&right.repository.updated),
    }
}

fn compare_text(left: &str, right: &str) -> Ordering {
    left.to_lowercase().cmp(&right.to_lowercase())
}
