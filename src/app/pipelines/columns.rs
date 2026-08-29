//! The two column sets the Pipelines screen draws, and how each orders its
//! rows. Both are ordinary [`ColumnId`]s, so they get #663's table, header
//! sorting and Columns overlay for nothing.

use std::cmp::Ordering;

use super::rows::{PipelineRow, RunRow};
use crate::columns::ColumnId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PipelineColumn {
    Name,
    Folder,
    LastRun,
    Branch,
    Age,
}

impl ColumnId for PipelineColumn {
    fn all() -> &'static [Self] {
        &[
            Self::Name,
            Self::Folder,
            Self::LastRun,
            Self::Branch,
            Self::Age,
        ]
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|column| column.key() == key)
    }

    fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Folder => "folder",
            Self::LastRun => "last-run",
            Self::Branch => "branch",
            Self::Age => "age",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Pipeline",
            Self::Folder => "Folder",
            Self::LastRun => "Last run",
            Self::Branch => "Branch",
            Self::Age => "Age",
        }
    }

    fn default_width(self) -> u16 {
        match self {
            Self::Name => 0,
            Self::Folder => 16,
            Self::LastRun => 22,
            Self::Branch => 18,
            Self::Age => 8,
        }
    }

    fn default_visible(self) -> bool {
        true
    }

    fn right_aligned(self) -> bool {
        matches!(self, Self::Age)
    }

    fn pinned(self) -> bool {
        matches!(self, Self::Name)
    }

    fn flexible(self) -> bool {
        matches!(self, Self::Name)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunColumn {
    Run,
    Result,
    Branch,
    Reason,
    By,
    Duration,
    Age,
}

impl ColumnId for RunColumn {
    fn all() -> &'static [Self] {
        &[
            Self::Run,
            Self::Result,
            Self::Branch,
            Self::Reason,
            Self::By,
            Self::Duration,
            Self::Age,
        ]
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|column| column.key() == key)
    }

    fn key(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Result => "result",
            Self::Branch => "branch",
            Self::Reason => "reason",
            Self::By => "by",
            Self::Duration => "duration",
            Self::Age => "age",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Run => "Run",
            Self::Result => "Result",
            Self::Branch => "Branch",
            Self::Reason => "Reason",
            Self::By => "By",
            Self::Duration => "Duration",
            Self::Age => "Age",
        }
    }

    fn default_width(self) -> u16 {
        match self {
            Self::Run => 0,
            Self::Result => 14,
            Self::Branch => 18,
            Self::Reason => 12,
            Self::By => 16,
            Self::Duration => 10,
            Self::Age => 8,
        }
    }

    fn default_visible(self) -> bool {
        true
    }

    fn right_aligned(self) -> bool {
        matches!(self, Self::Duration | Self::Age)
    }

    fn pinned(self) -> bool {
        matches!(self, Self::Run)
    }

    fn flexible(self) -> bool {
        matches!(self, Self::Run)
    }
}

/// Orders two pipelines by one column. A pipeline that has never run sorts
/// last whichever way the column is turned, the way a work item with no
/// children sorts last on Progress.
pub(super) fn compare_pipelines(
    left: &PipelineRow,
    right: &PipelineRow,
    column: PipelineColumn,
) -> Ordering {
    match column {
        PipelineColumn::Name => compare_text(&left.pipeline.name, &right.pipeline.name),
        PipelineColumn::Folder => compare_text(&left.pipeline.folder, &right.pipeline.folder),
        PipelineColumn::Branch => compare_text(&left.branch(), &right.branch()),
        PipelineColumn::LastRun | PipelineColumn::Age => {
            let key = |row: &PipelineRow| row.last_run.as_ref().map(|run| run.id);
            key(left).cmp(&key(right))
        }
    }
}

pub(super) fn compare_runs(left: &RunRow, right: &RunRow, column: RunColumn) -> Ordering {
    match column {
        RunColumn::Run | RunColumn::Age => left.run.id.cmp(&right.run.id),
        RunColumn::Result => compare_text(&left.result_word(), &right.result_word()),
        RunColumn::Branch => compare_text(&left.branch(), &right.branch()),
        RunColumn::Reason => compare_text(&left.run.reason, &right.run.reason),
        RunColumn::By => compare_text(
            left.run.requested_for.as_deref().unwrap_or_default(),
            right.run.requested_for.as_deref().unwrap_or_default(),
        ),
        // A run still going has no duration to compare, so it leads: it is the
        // one worth looking at.
        RunColumn::Duration => match (left.finished_duration(), right.finished_duration()) {
            (Some(left), Some(right)) => left.cmp(&right),
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (None, None) => Ordering::Equal,
        },
    }
}

fn compare_text(left: &str, right: &str) -> Ordering {
    left.to_lowercase().cmp(&right.to_lowercase())
}
