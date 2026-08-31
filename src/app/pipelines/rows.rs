//! What the Pipelines screen's two lists hold: a definition with the run that
//! says how it last went, and one run with the pipeline it belongs to. Both
//! are what the filters read and what the table draws.

use crate::model::{Pipeline, Run, RunResult, RunStatus};
use crate::timestamp::Timestamp;

/// One pipeline, with the run that says how it last went.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PipelineRow {
    pub pipeline: Pipeline,
    pub last_run: Option<Run>,
    /// What the repository it builds is called, or its GUID when the pull has
    /// not brought the repositories down yet.
    pub repo: String,
}

impl PipelineRow {
    /// The branch its last run was on, falling back to the branch it is
    /// configured to build.
    #[must_use]
    pub fn branch(&self) -> String {
        self.last_run.as_ref().map_or_else(
            || short_branch(self.pipeline.default_branch.as_deref().unwrap_or_default()),
            |run| short_branch(&run.source_branch),
        )
    }

    /// Whether the fuzzy half of a query — the words with no field in front of
    /// them — is in this row.
    #[must_use]
    pub fn matches_fuzzy(&self, needle: &str) -> bool {
        crate::filter::contains_ignore_case(&self.pipeline.name, needle)
            || crate::filter::contains_ignore_case(&self.pipeline.folder, needle)
            || crate::filter::contains_ignore_case(&self.repo, needle)
    }
}

/// One run, with the name of the pipeline it belongs to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunRow {
    pub run: Run,
    pub pipeline: String,
}

impl RunRow {
    #[must_use]
    pub fn branch(&self) -> String {
        short_branch(&self.run.source_branch)
    }

    /// The word `result:` filters on: how a finished run turned out, or where
    /// a run still going has got to.
    #[must_use]
    pub fn result_word(&self) -> String {
        self.run.result.map_or_else(
            || self.run.status.as_str().to_owned(),
            |result| result.as_str().to_owned(),
        )
    }

    /// How long a finished run took.
    #[must_use]
    pub fn finished_duration(&self) -> Option<i64> {
        let start = self.run.start_time?;
        let finish = self.run.finish_time?;
        Some(start.seconds_until(finish))
    }

    /// How long the run has taken so far, which for a run still going is
    /// measured against `now` and is why the cell ticks.
    #[must_use]
    pub fn duration_seconds(&self, now: Timestamp) -> Option<i64> {
        let start = self.run.start_time?;
        let end = self.run.finish_time.unwrap_or(now);
        Some(start.seconds_until(end).max(0))
    }

    #[must_use]
    pub fn matches_fuzzy(&self, needle: &str) -> bool {
        crate::filter::contains_ignore_case(&self.run.build_number, needle)
            || crate::filter::contains_ignore_case(&self.pipeline, needle)
            || crate::filter::contains_ignore_case(&self.branch(), needle)
            || crate::filter::contains_ignore_case(
                self.run.requested_for.as_deref().unwrap_or_default(),
                needle,
            )
    }
}

/// `refs/heads/main` is `main`; anything else is left as it is, because a tag
/// or a pull request ref says something the branch name alone does not.
#[must_use]
pub fn short_branch(reference: &str) -> String {
    reference
        .strip_prefix("refs/heads/")
        .unwrap_or(reference)
        .to_owned()
}

/// The glyph the conventions give a run, whatever the state is called.
#[must_use]
pub const fn run_glyph(status: RunStatus, result: Option<RunResult>) -> &'static str {
    match (status, result) {
        (RunStatus::InProgress | RunStatus::Cancelling, _) => "\u{25d0}",
        (RunStatus::NotStarted | RunStatus::Postponed, _) => "\u{25cb}",
        (_, Some(RunResult::Succeeded)) => "\u{2713}",
        (_, Some(RunResult::PartiallySucceeded)) => "\u{25d1}",
        (_, Some(RunResult::Failed)) => "\u{2717}",
        (_, Some(RunResult::Canceled)) => "\u{2298}",
        (_, None) => "\u{25cb}",
    }
}

/// `1m 04s`, the way the conventions ask durations to read.
#[must_use]
pub fn duration_label(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (minutes, seconds) = (seconds / 60, seconds % 60);
    if minutes >= 60 {
        format!("{}h {:02}m", minutes / 60, minutes % 60)
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}
