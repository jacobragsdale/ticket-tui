//! Desktop notifications: the one command `config.toml` names, and the edges
//! worth running it for.
//!
//! ```toml
//! [notify]
//! command = "notify-send {title} {body}"
//! ```
//!
//! The command is run through `sh -c`, detached, with its output thrown away.
//! `{title}` and `{body}` are replaced by the value **as one complete
//! single-quoted shell word** — `it's` goes in as `'it'\''s'` — so whatever is
//! in the text, the command receives it verbatim as one argument. Write the
//! placeholders where an argument goes and quote nothing around them.
//!
//! Nothing here decides *when* to fire. The diffs below say what has changed
//! since the last read of each source, and every one of them takes the last
//! read as `Option`: `None` is the first read of the run, which is the
//! baseline, so a vote that landed while ticket-tui was not running is not
//! announced when it starts.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::aks::{Pod, PodKey};
use crate::model::{Approval, PrStatus, PullRequest, same_text};

/// One notification: the headline, and the line under it. The status line
/// says the same two, joined, so the app and the desktop never disagree.
pub type Notice = (String, String);

/// What a notification is handed to. The real one shells out; a test records.
type Sink = Box<dyn FnMut(&str, &str) -> io::Result<()> + Send>;

/// What a recording notifier writes down.
pub type Recorded = Arc<Mutex<Vec<Notice>>>;

/// The `[notify]` command, ready to run. Without that table there is nothing
/// to run and firing does nothing at all.
pub struct Notifier {
    sink: Sink,
    /// Whether a command that would not start has been reported. It is said
    /// once: one that cannot be spawned will not spawn on the next build
    /// either, and a toast a minute is worse than no notifications.
    reported: bool,
}

impl Default for Notifier {
    fn default() -> Self {
        Self {
            sink: Box::new(|_, _| Ok(())),
            reported: false,
        }
    }
}

impl fmt::Debug for Notifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Notifier")
    }
}

impl Notifier {
    /// The notifier `[notify] command` asks for. `None` — no table — is the
    /// one that does nothing.
    #[must_use]
    pub fn new(command: Option<String>) -> Self {
        let Some(command) = command else {
            return Self::default();
        };
        // Nothing is waited on: a notifier that takes a second must not hold
        // the frame up. The finished ones are reaped on the way past instead,
        // so a run that fires all day leaves nothing behind.
        let mut running: Vec<Child> = Vec::new();
        Self {
            sink: Box::new(move |title, body| {
                running.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
                running.push(
                    Command::new("sh")
                        .arg("-c")
                        .arg(fill(&command, title, body))
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()?,
                );
                Ok(())
            }),
            reported: false,
        }
    }

    /// A notifier that writes down what it was handed instead of running
    /// anything, with the log it writes to. What the tests fire through.
    #[must_use]
    pub fn recording() -> (Self, Recorded) {
        let log = Recorded::default();
        let sink = Arc::clone(&log);
        (
            Self {
                sink: Box::new(move |title, body| {
                    sink.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((title.to_owned(), body.to_owned()));
                    Ok(())
                }),
                reported: false,
            },
            log,
        )
    }

    /// Says one notice on the desktop. Hands back the line to put in the
    /// status bar when the command would not start, which is said once a run.
    pub fn fire(&mut self, title: &str, body: &str) -> Option<String> {
        let error = (self.sink)(title, body).err()?;
        (!std::mem::replace(&mut self.reported, true))
            .then(|| format!("Could not run the [notify] command: {error}"))
    }
}

/// The command with `{title}` and `{body}` filled in, each as one shell word.
fn fill(command: &str, title: &str, body: &str) -> String {
    command
        .replace("{title}", &quote(title))
        .replace("{body}", &quote(body))
}

/// One value as a single `sh` word: wrapped in single quotes, with every
/// single quote in it closed, escaped and reopened. Nothing inside is a
/// variable, a glob or a word break, whatever it holds.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// What a pull request looked like on the last read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrMark {
    status: PrStatus,
    /// Each reviewer's vote, by reviewer id.
    votes: Vec<(String, i8)>,
    /// Whether it was waiting on my vote, by the rule the tab badge counts by.
    wants_me: bool,
}

/// Every pull request as the last read left it, by id.
pub type PrMarks = HashMap<i64, PrMark>;

/// What one read of the pull requests is worth saying, against the read before
/// it, and the marks to hold on to. On one you wrote: a vote landing or
/// changing, and the pull request completing or being abandoned. On anybody
/// else's: it turning up wanting a vote from you it has not had.
#[must_use]
pub fn pull_request_news(
    previous: Option<&PrMarks>,
    requests: &[PullRequest],
    me: Option<&str>,
) -> (PrMarks, Vec<Notice>) {
    let marks: PrMarks = requests
        .iter()
        .map(|request| (request.id, mark(request, me)))
        .collect();
    let Some(previous) = previous else {
        return (marks, Vec::new());
    };
    let mut news = Vec::new();
    for request in requests {
        let mark = &marks[&request.id];
        let was = previous.get(&request.id);
        let mine = me.is_some_and(|me| same_text(&request.created_by.display_name, me));
        if let Some(was) = was.filter(|_| mine) {
            if was.status != mark.status && mark.status.is_closed() {
                news.push((
                    format!("!{} {}", request.id, mark.status.as_str()),
                    request.title.clone(),
                ));
            } else if let Some(voted) = changed_vote(was, request) {
                news.push((voted, request.title.clone()));
            }
        }
        // One that has just arrived wanting a vote counts too: `to_review`
        // rose, whether by a new pull request or by being added to an old one.
        if mark.wants_me && !was.is_some_and(|was| was.wants_me) {
            news.push((
                format!("!{} wants your review", request.id),
                request.title.clone(),
            ));
        }
    }
    (marks, news)
}

/// How one pull request stands now.
fn mark(request: &PullRequest, me: Option<&str>) -> PrMark {
    let waiting = |vote: i8, name: &str| {
        vote == 0 && me.is_some_and(|me| same_text(name, me)) && !request.status.is_closed()
    };
    PrMark {
        status: request.status,
        votes: request
            .reviewers
            .iter()
            .map(|reviewer| (reviewer.id.clone(), reviewer.vote))
            .collect(),
        wants_me: request
            .reviewers
            .iter()
            .any(|reviewer| waiting(reviewer.vote, &reviewer.display_name)),
    }
}

/// The first reviewer whose vote is not what it was, as a headline. A reviewer
/// who was not there last time counts from no vote: added with nothing cast
/// is not news, added with an approval in the same step — which is how
/// `az repos pr set-vote` lands one — is the approval.
fn changed_vote(was: &PrMark, request: &PullRequest) -> Option<String> {
    let reviewer = request.reviewers.iter().find(|reviewer| {
        let before = was
            .votes
            .iter()
            .find(|(id, _)| *id == reviewer.id)
            .map_or(0, |(_, vote)| *vote);
        before != reviewer.vote
    })?;
    let id = request.id;
    let who = &reviewer.display_name;
    Some(if reviewer.vote == 0 {
        format!("!{id} vote cleared by {who}")
    } else {
        format!("!{id} {} by {who}", reviewer.word())
    })
}

/// Every pod of one cluster and namespace as the last read left it: how often
/// it had restarted, and whether it was crash-looping.
pub type PodMarks = HashMap<PodKey, (u32, bool)>;

/// What one read of a cluster's pods is worth saying, against the read before
/// it of the same cluster and namespace, and the marks to hold on to.
#[must_use]
pub fn pod_news(previous: Option<&PodMarks>, pods: &[Pod]) -> (PodMarks, Vec<Notice>) {
    let marks: PodMarks = pods
        .iter()
        .map(|pod| (pod.key.clone(), (pod.restarts, crash_looping(pod))))
        .collect();
    let Some(previous) = previous else {
        return (marks, Vec::new());
    };
    let mut news = Vec::new();
    for pod in pods {
        // A pod that was not there last time has not restarted since — but it
        // can already be crash-looping, and that is news the moment it lands.
        let (was_restarts, was_looping) = previous
            .get(&pod.key)
            .copied()
            .unwrap_or((pod.restarts, false));
        let headline = if crash_looping(pod) && !was_looping {
            format!("{} is crash-looping", pod.key.name)
        } else if pod.restarts > was_restarts {
            format!("{} restarted ({} in all)", pod.key.name, pod.restarts)
        } else {
            continue;
        };
        news.push((
            headline,
            format!(
                "{} \u{00b7} {} \u{00b7} {}",
                pod.key.cluster, pod.key.namespace, pod.status
            ),
        ));
    }
    (marks, news)
}

/// Whether the pod's STATUS word is a crash loop — `Init:CrashLoopBackOff`
/// counts, because a container that will not start is a container that will
/// not start.
fn crash_looping(pod: &Pod) -> bool {
    pod.status.contains("CrashLoopBackOff")
}

/// The approvals that were not in the read before this one, and the ids to
/// hold on to.
#[must_use]
pub fn approval_news(
    previous: Option<&HashSet<String>>,
    approvals: &[Approval],
) -> (HashSet<String>, Vec<Notice>) {
    let ids: HashSet<String> = approvals
        .iter()
        .map(|approval| approval.id.clone())
        .collect();
    let Some(previous) = previous else {
        return (ids, Vec::new());
    };
    let news = approvals
        .iter()
        .filter(|approval| !previous.contains(&approval.id))
        .map(|approval| {
            let what = if approval.build_number.is_empty() {
                approval.pipeline.clone()
            } else {
                format!("Build {}", approval.build_number)
            };
            (
                format!("{what} waits on your approval"),
                format!("{} \u{00b7} {}", approval.pipeline, approval.stage),
            )
        })
        .collect();
    (ids, news)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Identity, PrReviewer};

    /// What the command receives for one value, read back out of `sh` itself.
    fn round_trip(value: &str) -> String {
        let output = Command::new("sh")
            .arg("-c")
            .arg(fill("printf '%s' {title}", value, ""))
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn a_value_reaches_the_command_verbatim_whatever_quotes_it_holds() {
        for value in [
            "Run 20260831.4 succeeded",
            "he said \"hi\"",
            "it's fine",
            "both ' and \" at once",
            "$HOME `whoami` * ; rm -rf /",
        ] {
            assert_eq!(round_trip(value), value, "{value}");
        }
    }

    /// The words the documented command hands over, read back out of `sh`
    /// with `printf` standing in for `osascript` — which would otherwise put a
    /// notification on the screen of whoever runs the tests.
    #[test]
    fn the_documented_command_hands_over_the_title_and_the_body_whole() {
        let template = concat!(
            "printf '%s\n' -e 'on run argv' ",
            "-e 'display notification (item 2 of argv) with title (item 1 of argv)' ",
            "-e 'end run' {title} {body}"
        );
        let output = Command::new("sh")
            .arg("-c")
            .arg(fill(template, "he said \"hi\"", "it's fine"))
            .output()
            .unwrap();
        let printed = String::from_utf8(output.stdout).unwrap();
        let words: Vec<&str> = printed.lines().collect();
        assert_eq!(
            &words[words.len() - 2..],
            ["he said \"hi\"", "it's fine"],
            "{printed}"
        );
    }

    #[test]
    fn a_command_that_will_not_start_is_said_once() {
        let mut notifier = Notifier::new(Some("exit 0".to_owned()));
        assert_eq!(notifier.fire("Build 1", "ok"), None, "sh is always there");

        let mut broken = Notifier {
            sink: Box::new(|_, _| Err(io::Error::other("no such file"))),
            reported: false,
        };
        assert_eq!(
            broken.fire("Build 1", "ok"),
            Some("Could not run the [notify] command: no such file".to_owned())
        );
        assert_eq!(broken.fire("Build 2", "ok"), None, "and not again");
    }

    /// The real spawner, end to end: `sh` runs the filled command on its own
    /// while the caller walks away, and both values arrive whole.
    #[test]
    fn the_command_runs_detached_and_is_handed_the_values_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("said.txt");
        let mut notifier = Notifier::new(Some(format!(
            "printf '%s\\n%s\\n' {{title}} {{body}} > {}",
            path.display()
        )));
        assert_eq!(notifier.fire("he said \"hi\"", "it's fine"), None);
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(&path)
                && text.lines().count() == 2
            {
                assert_eq!(text, "he said \"hi\"\nit's fine\n");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the command never wrote the file");
    }

    #[test]
    fn no_table_runs_nothing() {
        assert_eq!(Notifier::new(None).fire("Build 1", "ok"), None);
    }

    fn reviewer(id: &str, name: &str, vote: i8) -> PrReviewer {
        PrReviewer {
            id: id.to_owned(),
            display_name: name.to_owned(),
            unique_name: None,
            vote,
            is_required: false,
        }
    }

    fn pull_request(author: &str, reviewers: Vec<PrReviewer>) -> PullRequest {
        PullRequest {
            repo_id: "repo".to_owned(),
            id: 812,
            title: "Tidy the watcher".to_owned(),
            description: String::new(),
            status: PrStatus::Active,
            is_draft: false,
            created_by: Identity {
                display_name: author.to_owned(),
                unique_name: None,
            },
            created_at: None,
            closed_at: None,
            source_ref: "refs/heads/feature".to_owned(),
            target_ref: "refs/heads/main".to_owned(),
            merge_status: "succeeded".to_owned(),
            last_merge_source_commit: String::new(),
            auto_complete_set_by: None,
            url: String::new(),
            reviewers,
            work_items: Vec::new(),
            build: None,
            threads: Vec::new(),
        }
    }

    #[test]
    fn the_first_read_of_the_pull_requests_is_the_baseline_and_the_same_read_twice_says_nothing() {
        let requests = vec![pull_request("Jacob", vec![reviewer("d", "Dana Ali", 0)])];
        let (marks, news) = pull_request_news(None, &requests, Some("Dana Ali"));
        assert!(
            news.is_empty(),
            "a queue that was already there is not news"
        );

        let (again, news) = pull_request_news(Some(&marks), &requests, Some("Dana Ali"));
        assert!(news.is_empty(), "nothing moved");
        assert_eq!(again, marks);
    }

    #[test]
    fn a_reviewer_added_with_a_vote_in_one_step_is_that_vote_and_one_added_without_is_nothing() {
        let before = vec![pull_request("Jacob", Vec::new())];
        let (marks, _) = pull_request_news(None, &before, Some("Jacob"));

        let quiet = vec![pull_request("Jacob", vec![reviewer("d", "Dana Ali", 0)])];
        let (marks, news) = pull_request_news(Some(&marks), &quiet, Some("Jacob"));
        assert!(news.is_empty(), "added, nothing cast: {news:?}");

        let voted = vec![pull_request("Jacob", vec![reviewer("e", "Eli Park", 10)])];
        let (_, news) = pull_request_news(Some(&marks), &voted, Some("Jacob"));
        assert_eq!(
            news,
            [(
                "!812 approved by Eli Park".to_owned(),
                "Tidy the watcher".to_owned()
            )]
        );
    }

    #[test]
    fn a_vote_landing_on_one_you_wrote_is_news_and_so_is_it_closing() {
        let before = vec![pull_request("Jacob", vec![reviewer("d", "Dana Ali", 0)])];
        let (marks, _) = pull_request_news(None, &before, Some("Jacob"));

        let after = vec![pull_request("Jacob", vec![reviewer("d", "Dana Ali", 10)])];
        let (marks, news) = pull_request_news(Some(&marks), &after, Some("Jacob"));
        assert_eq!(
            news,
            [(
                "!812 approved by Dana Ali".to_owned(),
                "Tidy the watcher".to_owned()
            )]
        );

        let mut completed = after.clone();
        completed[0].status = PrStatus::Completed;
        let (_, news) = pull_request_news(Some(&marks), &completed, Some("Jacob"));
        assert_eq!(
            news.first().map(|(title, _)| title.as_str()),
            Some("!812 completed")
        );

        // Somebody else's pull request, voted on: not yours to hear about.
        let theirs = vec![pull_request("Dana Ali", vec![reviewer("j", "Jacob", 10)])];
        let (marks, _) = pull_request_news(None, &theirs, Some("Jacob"));
        let mut voted = theirs.clone();
        voted[0].reviewers[0].vote = -10;
        let (_, news) = pull_request_news(Some(&marks), &voted, Some("Jacob"));
        assert!(news.is_empty(), "{news:?}");
    }

    #[test]
    fn one_that_turns_up_wanting_your_review_is_news_once() {
        let (marks, _) = pull_request_news(None, &[], Some("Jacob"));
        let requests = vec![pull_request("Dana Ali", vec![reviewer("j", "Jacob", 0)])];
        let (marks, news) = pull_request_news(Some(&marks), &requests, Some("Jacob"));
        assert_eq!(
            news,
            [(
                "!812 wants your review".to_owned(),
                "Tidy the watcher".to_owned()
            )]
        );

        let (_, news) = pull_request_news(Some(&marks), &requests, Some("Jacob"));
        assert!(
            news.is_empty(),
            "the same queue on the next pull is not news"
        );
    }

    fn pod(name: &str, status: &str, restarts: u32) -> Pod {
        Pod {
            key: PodKey {
                cluster: "qa".to_owned(),
                namespace: "orders".to_owned(),
                name: name.to_owned(),
            },
            status: status.to_owned(),
            ready: (1, 1),
            restarts,
            created: None,
            node: String::new(),
            ip: String::new(),
            owner: None,
            containers: Vec::new(),
            labels: Vec::new(),
        }
    }

    #[test]
    fn a_pod_restarting_or_starting_to_crash_loop_is_news_and_the_first_read_is_not() {
        let first = vec![pod("orders-api-1", "CrashLoopBackOff", 4)];
        let (marks, news) = pod_news(None, &first);
        assert!(news.is_empty(), "what was already broken is not news");

        let (marks, news) = pod_news(Some(&marks), &first);
        assert!(news.is_empty(), "the same read again says nothing");

        let restarted = vec![pod("orders-api-1", "CrashLoopBackOff", 5)];
        let (marks, news) = pod_news(Some(&marks), &restarted);
        assert_eq!(
            news,
            [(
                "orders-api-1 restarted (5 in all)".to_owned(),
                "qa \u{00b7} orders \u{00b7} CrashLoopBackOff".to_owned()
            )]
        );

        // A healthy one that starts crash-looping, and a new one that lands
        // crash-looping already.
        let mut running = restarted.clone();
        running[0].status = "Running".to_owned();
        let (marks, _) = pod_news(Some(&marks), &running);
        let looping = vec![
            pod("orders-api-1", "CrashLoopBackOff", 5),
            pod("orders-api-2", "Init:CrashLoopBackOff", 0),
        ];
        let (_, news) = pod_news(Some(&marks), &looping);
        assert_eq!(
            news.iter()
                .map(|(title, _)| title.as_str())
                .collect::<Vec<_>>(),
            [
                "orders-api-1 is crash-looping",
                "orders-api-2 is crash-looping"
            ]
        );
    }

    fn approval(id: &str) -> Approval {
        Approval {
            id: id.to_owned(),
            pipeline: "Nightly".to_owned(),
            run_id: Some(7),
            build_number: "20260831.4".to_owned(),
            stage: "Deploy prod".to_owned(),
            instructions: String::new(),
            requested_at: None,
        }
    }

    #[test]
    fn an_approval_that_was_not_there_before_is_news() {
        let (seen, news) = approval_news(None, &[approval("a")]);
        assert!(news.is_empty(), "the queue at startup is not news");
        let (seen, news) = approval_news(Some(&seen), &[approval("a"), approval("b")]);
        assert_eq!(
            news,
            [(
                "Build 20260831.4 waits on your approval".to_owned(),
                "Nightly \u{00b7} Deploy prod".to_owned()
            )]
        );
        let (_, news) = approval_news(Some(&seen), &[approval("a"), approval("b")]);
        assert!(news.is_empty());
    }
}
