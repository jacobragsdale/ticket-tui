//! The two filter grammars the Pipelines screen reads, one per level. Both are
//! ordinary [`FilterSchema`]s, so the search box, the chips and the facet bar
//! work the same way they do on work items.

use super::rows::{PipelineRow, RunRow, short_branch};
use crate::filter::{FilterSchema, MatchContext, Sentinel};
use crate::model::same_text;
use crate::timestamp::Timestamp;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PipelineSchema;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PipelineField {
    Name,
    Folder,
    Repo,
    /// How the pipeline's last run turned out, so `result:failed` is every
    /// pipeline that is currently red.
    Result,
}

impl FilterSchema for PipelineSchema {
    type Field = PipelineField;
    type Row = PipelineRow;

    fn all() -> &'static [Self::Field] {
        &[
            PipelineField::Name,
            PipelineField::Folder,
            PipelineField::Repo,
            PipelineField::Result,
        ]
    }

    fn bar() -> &'static [Self::Field] {
        &[PipelineField::Folder, PipelineField::Result]
    }

    fn parse(name: &str) -> Option<Self::Field> {
        match name.to_ascii_lowercase().as_str() {
            "name" | "pipeline" => Some(PipelineField::Name),
            "folder" | "path" => Some(PipelineField::Folder),
            "repo" | "repository" => Some(PipelineField::Repo),
            "result" => Some(PipelineField::Result),
            _ => None,
        }
    }

    fn key(field: Self::Field) -> &'static str {
        match field {
            PipelineField::Name => "name",
            PipelineField::Folder => "folder",
            PipelineField::Repo => "repo",
            PipelineField::Result => "result",
        }
    }

    fn label(field: Self::Field) -> &'static str {
        match field {
            PipelineField::Name => "Pipeline",
            PipelineField::Folder => "Folder",
            PipelineField::Repo => "Repository",
            PipelineField::Result => "Result",
        }
    }

    fn is_date(_field: Self::Field) -> bool {
        false
    }

    fn values(field: Self::Field, row: &Self::Row) -> Vec<String> {
        match field {
            PipelineField::Name => vec![row.pipeline.name.clone()],
            PipelineField::Folder => vec![row.pipeline.folder.clone()],
            PipelineField::Repo => vec![row.repo.clone()],
            PipelineField::Result => row
                .last_run
                .as_ref()
                .map(|run| {
                    run.result.map_or_else(
                        || run.status.as_str().to_owned(),
                        |result| result.as_str().to_owned(),
                    )
                })
                .into_iter()
                .collect(),
        }
    }

    fn date_value(_field: Self::Field, _row: &Self::Row) -> Option<Timestamp> {
        None
    }

    fn sentinel(_field: Self::Field, _value: &str) -> Option<Sentinel> {
        None
    }

    fn matches_sentinel(_sentinel: Sentinel, _row: &Self::Row, _context: &MatchContext) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RunSchema;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RunField {
    Pipeline,
    Branch,
    Result,
    Status,
    Reason,
    /// Who set it going, where `@me` is whoever the last sync signed in as.
    By,
}

impl FilterSchema for RunSchema {
    type Field = RunField;
    type Row = RunRow;

    fn all() -> &'static [Self::Field] {
        &[
            RunField::Pipeline,
            RunField::Branch,
            RunField::Result,
            RunField::Status,
            RunField::Reason,
            RunField::By,
        ]
    }

    fn bar() -> &'static [Self::Field] {
        &[RunField::Result, RunField::Branch, RunField::Reason]
    }

    fn parse(name: &str) -> Option<Self::Field> {
        match name.to_ascii_lowercase().as_str() {
            "pipeline" => Some(RunField::Pipeline),
            "branch" => Some(RunField::Branch),
            "result" => Some(RunField::Result),
            "status" => Some(RunField::Status),
            "reason" => Some(RunField::Reason),
            "by" | "requested" => Some(RunField::By),
            _ => None,
        }
    }

    fn key(field: Self::Field) -> &'static str {
        match field {
            RunField::Pipeline => "pipeline",
            RunField::Branch => "branch",
            RunField::Result => "result",
            RunField::Status => "status",
            RunField::Reason => "reason",
            RunField::By => "by",
        }
    }

    fn label(field: Self::Field) -> &'static str {
        match field {
            RunField::Pipeline => "Pipeline",
            RunField::Branch => "Branch",
            RunField::Result => "Result",
            RunField::Status => "Status",
            RunField::Reason => "Reason",
            RunField::By => "Requested by",
        }
    }

    fn is_date(_field: Self::Field) -> bool {
        false
    }

    fn values(field: Self::Field, row: &Self::Row) -> Vec<String> {
        match field {
            RunField::Pipeline => vec![row.pipeline.clone()],
            RunField::Branch => vec![short_branch(&row.run.source_branch)],
            RunField::Result => vec![row.result_word()],
            RunField::Status => vec![row.run.status.as_str().to_owned()],
            RunField::Reason => vec![row.run.reason.clone()],
            RunField::By => row.run.requested_for.clone().into_iter().collect(),
        }
    }

    fn date_value(_field: Self::Field, _row: &Self::Row) -> Option<Timestamp> {
        None
    }

    fn sentinel(field: Self::Field, value: &str) -> Option<Sentinel> {
        let name = value.strip_prefix('@')?.to_ascii_lowercase();
        match (field, name.as_str()) {
            (RunField::By, "me") => Some(Sentinel::Me),
            _ => None,
        }
    }

    fn matches_sentinel(sentinel: Sentinel, row: &Self::Row, context: &MatchContext) -> bool {
        match sentinel {
            Sentinel::Me => context.me.as_deref().is_some_and(|me| {
                row.run
                    .requested_for
                    .as_deref()
                    .is_some_and(|who| same_text(who, me))
            }),
            _ => false,
        }
    }
}
