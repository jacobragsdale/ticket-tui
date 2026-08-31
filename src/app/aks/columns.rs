//! The columns the AKS table draws, and how each orders its rows. An ordinary
//! [`ColumnId`], so the table, the header sorting and the Columns overlay come
//! for nothing.

use std::cmp::Ordering;

use crate::aks::PodRow;
use crate::columns::ColumnId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PodColumn {
    Name,
    Cluster,
    Namespace,
    Ready,
    Status,
    Restarts,
    Age,
    Node,
    Repo,
}

impl ColumnId for PodColumn {
    fn all() -> &'static [Self] {
        &[
            Self::Name,
            Self::Cluster,
            Self::Namespace,
            Self::Ready,
            Self::Status,
            Self::Restarts,
            Self::Age,
            Self::Node,
            Self::Repo,
        ]
    }

    fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Cluster => "cluster",
            Self::Namespace => "ns",
            Self::Ready => "ready",
            Self::Status => "status",
            Self::Restarts => "restarts",
            Self::Age => "age",
            Self::Node => "node",
            Self::Repo => "repo",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Pod",
            Self::Cluster => "Cluster",
            Self::Namespace => "Namespace",
            Self::Ready => "Ready",
            Self::Status => "Status",
            Self::Restarts => "Restarts",
            Self::Age => "Age",
            Self::Node => "Node",
            Self::Repo => "Repository",
        }
    }

    fn default_width(self) -> u16 {
        match self {
            Self::Name => 0,
            Self::Cluster | Self::Restarts => 8,
            Self::Namespace => 14,
            Self::Ready => 6,
            Self::Status | Self::Repo => 18,
            Self::Age => 7,
            Self::Node => 20,
        }
    }

    /// The node a pod landed on and the repository that built it are worth
    /// asking for, not worth the width by default.
    fn default_visible(self) -> bool {
        !matches!(self, Self::Node | Self::Repo)
    }

    fn right_aligned(self) -> bool {
        matches!(self, Self::Ready | Self::Restarts | Self::Age)
    }

    fn pinned(self) -> bool {
        matches!(self, Self::Name)
    }

    fn flexible(self) -> bool {
        matches!(self, Self::Name)
    }

    /// A pod's name is its deployment's plus two hashes, which is about as
    /// much room as a work item's title asks for.
    fn min_flexible_width(self) -> u16 {
        24
    }
}

/// Orders two pods by one column, and by where they live whenever the column
/// itself cannot tell them apart: a list re-read every fifteen seconds must
/// not shuffle rows that are equal.
pub(super) fn compare_pods(left: &PodRow, right: &PodRow, column: PodColumn) -> Ordering {
    let ordering = match column {
        PodColumn::Name => compare_text(&left.pod.key.name, &right.pod.key.name),
        PodColumn::Cluster => compare_text(&left.pod.key.cluster, &right.pod.key.cluster),
        PodColumn::Namespace => compare_text(&left.pod.key.namespace, &right.pod.key.namespace),
        PodColumn::Node => compare_text(&left.pod.node, &right.pod.node),
        PodColumn::Repo => compare_text(
            left.repo.as_deref().unwrap_or_default(),
            right.repo.as_deref().unwrap_or_default(),
        ),
        PodColumn::Status => compare_text(&left.pod.status, &right.pod.status),
        // How much of a pod is up first, then how big it is: `0/1` before
        // `1/2` before `2/2`.
        PodColumn::Ready => left
            .pod
            .ready
            .cmp(&right.pod.ready)
            .then_with(|| left.pod.ready.1.cmp(&right.pod.ready.1)),
        PodColumn::Restarts => left.pod.restarts.cmp(&right.pod.restarts),
        // Newest is greatest, so turning the column round puts the pod that
        // just landed on top. One with no timestamp has no age and sorts last.
        PodColumn::Age => match (left.pod.created, right.pod.created) {
            (Some(left), Some(right)) => left.cmp(&right),
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (None, None) => Ordering::Equal,
        },
    };
    ordering.then_with(|| {
        compare_text(&left.pod.key.cluster, &right.pod.key.cluster)
            .then_with(|| compare_text(&left.pod.key.namespace, &right.pod.key.namespace))
            .then_with(|| compare_text(&left.pod.key.name, &right.pod.key.name))
    })
}

fn compare_text(left: &str, right: &str) -> Ordering {
    left.to_lowercase().cmp(&right.to_lowercase())
}
