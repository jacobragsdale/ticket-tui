//! The two fixed columns of the environments board. Everything to their right
//! is one column per `[[environments]]`, which no static column set can name:
//! the environments come out of `config.toml`, so the board lays those out
//! itself and only these two go through the Columns overlay.

use crate::columns::ColumnId;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EnvColumn {
    #[default]
    Service,
    Namespace,
}

impl ColumnId for EnvColumn {
    fn all() -> &'static [Self] {
        &[Self::Service, Self::Namespace]
    }

    fn key(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Namespace => "namespace",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Service => "Service",
            Self::Namespace => "Namespace",
        }
    }

    fn default_width(self) -> u16 {
        match self {
            Self::Service => 0,
            Self::Namespace => 14,
        }
    }

    fn default_visible(self) -> bool {
        true
    }

    fn right_aligned(self) -> bool {
        false
    }

    fn pinned(self) -> bool {
        self == Self::Service
    }

    fn flexible(self) -> bool {
        self == Self::Service
    }

    /// A workload name is shorter than a work item's title.
    fn min_flexible_width(self) -> u16 {
        16
    }
}
