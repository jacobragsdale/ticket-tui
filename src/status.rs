//! `ticket-tui status`: the tab badges, on one line, outside the app.
//!
//! ticket-tui is one pane among several, and the numbers on its tab bar are
//! wanted in the status bar or the shell prompt without switching to it. Every
//! figure here comes from SQLite alone — no network, no `az` —
//! except the two the database never holds, which are read out of the context
//! file a running TUI publishes. Nothing to say prints nothing, so a prompt
//! that calls this on every line stays clean.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::agent_context;
use crate::db::{self, SqliteTicketRepository};
use crate::filter::is_stale;
use crate::model::{self, StateCategory, Ticket};
use crate::timestamp::Timestamp;
use crate::ui::pipelines::relative_age;

/// What separates one segment from the next, as the TUI's own status bar
/// separates its parts.
const SEPARATOR: &str = " \u{00b7} ";

/// A day, which is how far back a failed run still counts as news.
const RECENT_FAILURE_SECONDS: i64 = 24 * 60 * 60;

/// Whether a running ticket-tui answered for the live reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextState {
    /// A context file whose process is still up: what it says is now.
    Live,
    /// A context file left behind by a run that is gone.
    Stale,
    /// No context file at all.
    Absent,
}

impl ContextState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Stale => "stale",
            Self::Absent => "absent",
        }
    }
}

/// Every figure one status line carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Status {
    doing: usize,
    stale: usize,
    review: usize,
    rejected: usize,
    live_runs: usize,
    failed_runs: usize,
    synced_at: Option<Timestamp>,
    /// How old the rows are, but only once that is past two refresh intervals
    /// and so worth saying out loud.
    synced_ago: Option<String>,
    context: ContextState,
}

impl Status {
    /// The line a prompt prints: the segments with something in them, in tab
    /// order, separated the way the status bar separates its parts. Nothing to
    /// say is the empty string, which the caller prints nothing for.
    #[must_use]
    pub fn line(&self) -> String {
        let mut segments: Vec<String> = Vec::new();
        for (label, count) in [
            ("doing", self.doing),
            ("stale", self.stale),
            ("review", self.review),
            ("rejected", self.rejected),
        ] {
            if count > 0 {
                segments.push(format!("{label} {count}"));
            }
        }
        if self.live_runs > 0 {
            segments.push(format!("\u{25d0} {}", self.live_runs));
        }
        if self.failed_runs > 0 {
            segments.push(format!("failed {}", self.failed_runs));
        }
        if let Some(ago) = &self.synced_ago {
            segments.push(format!("synced {ago} ago"));
        }
        segments.join(SEPARATOR)
    }

    /// The same reading as one object, with every figure named and the zeros
    /// left in, so a script does not have to parse the line.
    #[must_use]
    pub fn json(&self) -> Value {
        json!({
            "doing": self.doing,
            "stale": self.stale,
            "review": self.review,
            "rejected": self.rejected,
            "live_runs": self.live_runs,
            "failed_runs": self.failed_runs,
            "synced_at": self.synced_at.map(Timestamp::to_rfc3339),
            "context": self.context.as_str(),
        })
    }
}

/// Reads every figure out of one database and the context file beside it.
///
/// The reads are the ones the tabs already make, through the same functions
/// the badges call, so the line and the tab bar cannot drift. Nothing here
/// reaches the network, and the only process it ever starts is the `ps` that
/// asks whether a context file's owner is still up.
pub fn collect(
    repository: &SqliteTicketRepository,
    me: Option<&str>,
    stale_days: u16,
    refresh_seconds: u64,
    now: Timestamp,
) -> Result<Status> {
    let tickets = repository.load_all()?;
    let requests = repository.load_pull_requests()?;
    let runs = repository.load_runs()?;
    let context = read_context(&agent_context::path_for(repository.path()));
    let synced_at = db::data_modified(repository.path())
        .map(|modified| Timestamp::from_offset_date_time(OffsetDateTime::from(modified)));
    Ok(Status {
        doing: mine(&tickets, me)
            .filter(|ticket| StateCategory::of(&ticket.state) == StateCategory::InProgress)
            .count(),
        stale: mine(&tickets, me)
            .filter(|ticket| is_stale(ticket, stale_days, now))
            .count(),
        review: model::awaiting_review(&requests, me),
        rejected: model::rejected_of_mine(&requests, me),
        live_runs: model::live_runs(&runs),
        failed_runs: model::runs_failed_since(&runs, now.plus_seconds(-RECENT_FAILURE_SECONDS)),
        synced_at,
        synced_ago: late_sync(synced_at, refresh_seconds, now),
        context,
    })
}

/// The work items one person holds. Without a signed-in name nothing is
/// anybody's, so both counts that lean on this come back zero rather than
/// counting the whole project as yours.
fn mine<'a>(tickets: &'a [Ticket], me: Option<&'a str>) -> impl Iterator<Item = &'a Ticket> {
    tickets.iter().filter(move |ticket| {
        me.is_some_and(|me| {
            ticket
                .assigned_to
                .as_deref()
                .is_some_and(|assignee| model::same_text(assignee, me))
        })
    })
}

/// How old the rows are, said only once they are older than two refresh
/// intervals: a number that is merely a minute behind is not worth a word, and
/// one that is half an hour behind should not pass for current. A run with the
/// timer off (`--refresh 0`) has no interval to be late against.
fn late_sync(synced_at: Option<Timestamp>, refresh_seconds: u64, now: Timestamp) -> Option<String> {
    let synced_at = synced_at?;
    let limit = i64::try_from(refresh_seconds.checked_mul(2)?).ok()?;
    (limit > 0 && synced_at.seconds_until(now) > limit).then(|| relative_age(synced_at, now))
}

/// Whether a running TUI is publishing a context file beside the database. A
/// file whose process is gone says what that run last saw rather than what is
/// true now, so it counts as stale.
fn read_context(path: &Path) -> ContextState {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return ContextState::Absent;
    };
    let Ok(document) = serde_json::from_str::<Value>(&raw) else {
        return ContextState::Stale;
    };
    let alive = document
        .get("process_id")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .is_some_and(process_is_alive);
    if alive {
        ContextState::Live
    } else {
        ContextState::Stale
    }
}

/// Whether the process that wrote a context file is still up. Linux answers
/// out of `/proc`; macOS has no such directory, so `ps` is asked instead —
/// one spawn, and only when a context file exists at all.
fn process_is_alive(pid: u32) -> bool {
    if Path::new("/proc/self").is_dir() {
        return Path::new(&format!("/proc/{pid}")).exists();
    }
    Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The stale threshold `status` counts against: the same order the TUI
/// resolves it in, with the session standing in for what neither the flag nor
/// the environment named. A session file that will not parse is not worth
/// stopping a prompt for, so the built-in default answers instead.
#[must_use]
pub fn stale_days(asked: Option<u16>, database: &Path) -> u16 {
    asked.unwrap_or_else(|| {
        crate::session::load(&crate::session::path_for(database))
            .unwrap_or_default()
            .stale_days
    })
}

/// Prints what there is to say and nothing when there is nothing, so a prompt
/// that calls this on every line stays clean either way.
pub fn report(status: &Status, json: bool) -> Result<Option<String>> {
    if json {
        return Ok(Some(
            serde_json::to_string(&status.json()).context("failed to serialize the status")?,
        ));
    }
    let line = status.line();
    Ok((!line.is_empty()).then_some(line))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Identity, PrReviewer, PrStatus, PullRequest, Run, RunResult, RunStatus};
    use crate::timestamp::ts;
    use tempfile::{TempDir, tempdir};

    const NOW: &str = "2026-08-31T12:00:00Z";

    fn now() -> Timestamp {
        ts(NOW)
    }

    fn database() -> (TempDir, SqliteTicketRepository) {
        let directory = tempdir().unwrap();
        let repository =
            SqliteTicketRepository::open(directory.path().join("tickets.sqlite3")).unwrap();
        (directory, repository)
    }

    fn ticket(id: i64, state: &str, assignee: Option<&str>, changed: &str) -> Ticket {
        Ticket {
            state: state.into(),
            assigned_to: assignee.map(str::to_owned),
            changed_at: ts(changed),
            ..Ticket::fixture(id, "Something to do")
        }
    }

    fn request(id: i64, author: &str, reviewer: (&str, i8), status: PrStatus) -> PullRequest {
        PullRequest {
            repo_id: "repo".into(),
            id,
            title: format!("PR {id}"),
            description: String::new(),
            status,
            is_draft: false,
            created_by: Identity::new(author, None),
            created_at: Some(ts("2026-08-30T12:00:00Z")),
            closed_at: None,
            source_ref: "refs/heads/feature".into(),
            target_ref: "refs/heads/main".into(),
            merge_status: "succeeded".into(),
            last_merge_source_commit: "abc".into(),
            auto_complete_set_by: None,
            url: String::new(),
            reviewers: vec![PrReviewer {
                id: reviewer.0.into(),
                display_name: reviewer.0.into(),
                unique_name: None,
                vote: reviewer.1,
                is_required: true,
            }],
            work_items: Vec::new(),
            build: None,
            threads: Vec::new(),
        }
    }

    fn run(id: i64, status: RunStatus, result: Option<RunResult>, finished: Option<&str>) -> Run {
        Run {
            id,
            pipeline_id: 1,
            build_number: format!("2026083{id}.1"),
            status,
            result,
            source_branch: "refs/heads/main".into(),
            source_version: "abc".into(),
            requested_for: None,
            reason: "manual".into(),
            pr_id: None,
            queue_time: None,
            start_time: None,
            finish_time: finished.map(ts),
            url: String::new(),
        }
    }

    /// A database with one of everything the line can say, so each segment has
    /// a fixture behind it.
    fn filled() -> (TempDir, SqliteTicketRepository) {
        let (directory, mut repository) = database();
        for ticket in [
            ticket(1, "Doing", Some("Jacob Ragsdale"), NOW),
            ticket(2, "Doing", Some("Jacob Ragsdale"), NOW),
            ticket(3, "Doing", Some("Avery Chen"), NOW),
            ticket(4, "To Do", Some("Jacob Ragsdale"), "2026-08-01T12:00:00Z"),
            ticket(5, "Done", Some("Jacob Ragsdale"), "2026-01-01T12:00:00Z"),
        ] {
            repository.upsert(&ticket, &[], &[]).unwrap();
        }
        repository
            .replace_pull_requests(&[
                request(11, "Avery Chen", ("Jacob Ragsdale", 0), PrStatus::Active),
                request(12, "Avery Chen", ("Jacob Ragsdale", 0), PrStatus::Active),
                request(13, "Jacob Ragsdale", ("Avery Chen", -10), PrStatus::Active),
                // Closed work is nobody's to answer for.
                request(
                    14,
                    "Jacob Ragsdale",
                    ("Avery Chen", -10),
                    PrStatus::Completed,
                ),
                request(15, "Avery Chen", ("Jacob Ragsdale", 0), PrStatus::Abandoned),
            ])
            .unwrap();
        repository
            .replace_runs(&[
                run(21, RunStatus::InProgress, None, None),
                run(
                    22,
                    RunStatus::Completed,
                    Some(RunResult::Failed),
                    Some("2026-08-31T09:00:00Z"),
                ),
                // Yesterday's failure is not news.
                run(
                    23,
                    RunStatus::Completed,
                    Some(RunResult::Failed),
                    Some("2026-08-29T09:00:00Z"),
                ),
                run(
                    24,
                    RunStatus::Completed,
                    Some(RunResult::Succeeded),
                    Some("2026-08-31T10:00:00Z"),
                ),
            ])
            .unwrap();
        (directory, repository)
    }

    fn write_context(repository: &SqliteTicketRepository, pid: u32) {
        let document = json!({
            "schema_version": 4,
            "process_id": pid,
            "updated_at": NOW,
        });
        std::fs::write(
            agent_context::path_for(repository.path()),
            document.to_string(),
        )
        .unwrap();
    }

    #[test]
    fn every_segment_has_a_fixture_behind_it_and_the_zeros_stay_off_the_line() {
        let (_directory, repository) = filled();
        write_context(&repository, std::process::id());

        let status = collect(&repository, Some("Jacob Ragsdale"), 14, 60, now()).unwrap();

        assert_eq!(
            status.line(),
            "doing 2 \u{00b7} stale 1 \u{00b7} review 2 \u{00b7} rejected 1 \u{00b7} \u{25d0} 1 \
             \u{00b7} failed 1",
            "the counts are the ones the tab bar badges, in tab order"
        );
        assert_eq!(status.context, ContextState::Live);
    }

    #[test]
    fn nothing_to_say_prints_nothing_so_a_prompt_stays_clean() {
        let (_directory, repository) = database();

        let status = collect(&repository, Some("Jacob Ragsdale"), 14, 60, now()).unwrap();

        assert_eq!(status.line(), "");
        assert_eq!(report(&status, false).unwrap(), None);
        assert_eq!(status.context, ContextState::Absent);
    }

    #[test]
    fn the_json_names_every_figure_and_leaves_the_zeros_in() {
        let (_directory, repository) = filled();
        write_context(&repository, std::process::id());

        let status = collect(&repository, Some("Jacob Ragsdale"), 14, 60, now()).unwrap();
        let reading: Value =
            serde_json::from_str(&report(&status, true).unwrap().unwrap()).unwrap();

        assert_eq!(reading["doing"], json!(2));
        assert_eq!(reading["stale"], json!(1));
        assert_eq!(reading["review"], json!(2));
        assert_eq!(reading["rejected"], json!(1));
        assert_eq!(reading["live_runs"], json!(1));
        assert_eq!(reading["failed_runs"], json!(1));
        assert_eq!(reading["context"], json!("live"));
        assert!(reading["synced_at"].is_string(), "{reading}");

        let (_empty_directory, empty) = database();
        let nothing = collect(&empty, None, 14, 60, now()).unwrap();
        let reading = nothing.json();
        assert_eq!(reading["doing"], json!(0), "the zeros stay in the object");
        assert_eq!(reading["context"], json!("absent"));
    }

    #[test]
    fn a_context_file_whose_process_is_gone_says_stale_and_answers_for_nothing() {
        let (_directory, repository) = filled();
        // Pid 1 is init and always up, so a pid nothing owns has to be found
        // another way: u32::MAX is above every pid_max either platform allows.
        write_context(&repository, u32::MAX);

        let status = collect(&repository, Some("Jacob Ragsdale"), 14, 60, now()).unwrap();

        assert_eq!(status.context, ContextState::Stale);
    }

    #[test]
    fn the_synced_suffix_shows_up_once_the_rows_are_past_two_refresh_intervals() {
        let synced = ts("2026-08-31T11:46:00Z");

        assert_eq!(
            late_sync(Some(synced), 60, now()),
            Some("14m".to_owned()),
            "fourteen minutes is well past two minutes"
        );
        assert_eq!(
            late_sync(Some(ts("2026-08-31T11:59:00Z")), 60, now()),
            None,
            "a minute behind is current enough to say nothing about"
        );
        assert_eq!(
            late_sync(Some(synced), 0, now()),
            None,
            "a run with the timer off has no interval to be late against"
        );
        assert_eq!(late_sync(None, 60, now()), None);
    }

    #[test]
    fn without_a_signed_in_name_nothing_is_counted_as_yours() {
        let (_directory, repository) = filled();

        let status = collect(&repository, None, 14, 60, now()).unwrap();

        assert_eq!((status.doing, status.stale), (0, 0));
        assert_eq!((status.review, status.rejected), (0, 0));
        assert_eq!(
            status.live_runs, 1,
            "a run going is going whoever is signed in"
        );
    }
}
