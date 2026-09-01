//! What one environment has that another has not: the promotion diff.
//!
//! `diff` is pure over two manifests — no clone, no database, no network — and
//! answers the question a promotion asks before anything is broken: which
//! ConfigMap and Secret keys, which vault objects, which variables, and which
//! image. Names only, as everywhere else here.
//!
//! The image is the one half the repository cannot answer on its own. A tag is
//! read back to the run that built it — by build number, else by the commit it
//! names, else by the `org.opencontainers.image.revision` the registry
//! annotates it with, which the caller reads because nothing in this file
//! touches a network — and the two runs' commits are read back to the pull
//! requests merged between them, and those to the work items they close. That
//! half takes its `git log` as lines from a closure, so it is as testable as
//! the rest.

use std::collections::BTreeSet;

use serde::Serialize;

use super::{Container, EnvManifest, ObjectKind, Provider, Workload};
use crate::model::{PullRequest, Run};

/// Which of the two environments something is in. A diff is read left to
/// right: `To` is the reverse entry, the one a line marks `only in prod`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    From,
    To,
}

/// Two environments, and every service they say something different about.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct PromotionDiff {
    /// The environment promoted from, by name.
    pub from: String,
    /// The environment promoted into, by name.
    pub to: String,
    /// One entry per workload that differs; a service with nothing to say is
    /// left out, so an empty list is `identical`.
    pub services: Vec<ServiceDiff>,
}

impl PromotionDiff {
    /// The environment one side is, by name.
    #[must_use]
    pub fn environment(&self, side: Side) -> &str {
        match side {
            Side::From => &self.from,
            Side::To => &self.to,
        }
    }
}

/// One service, and everything the two environments say differently about it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ServiceDiff {
    /// The workload's name, which is what the service is called on both sides.
    pub workload: String,
    pub kind: String,
    /// Set when the workload is in one environment only, which is the whole of
    /// what there is to say about it.
    pub only_in: Option<Side>,
    /// One entry per container whose image tag differs.
    pub images: Vec<ImageChange>,
    /// Keys of the ConfigMaps and Secrets the workload references that one
    /// environment holds and the other does not. An object one side has no
    /// copy of at all is `env check`'s finding rather than a diff.
    pub keys: Vec<Entry>,
    /// Vault objects the workload's providers pull on one side only.
    pub vault_objects: Vec<Entry>,
    /// Variable names one side's container sets and the other's does not.
    pub variables: Vec<Entry>,
}

impl ServiceDiff {
    /// Whether the two environments say the same thing about this service.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.only_in.is_none()
            && self.images.is_empty()
            && self.keys.is_empty()
            && self.vault_objects.is_empty()
            && self.variables.is_empty()
    }
}

/// One name one environment has and the other has not.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Entry {
    /// Where it is written: the ConfigMap or Secret a key is in, the vault a
    /// vault object is pulled from — the provider, where the provider names no
    /// vault — or the container a variable is set on.
    pub object: String,
    /// The key, vault object or variable, by its own name.
    pub name: String,
    /// The environment that has it.
    pub side: Side,
}

/// One container's image tag, on both sides.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ImageChange {
    pub container: String,
    /// The tag `from` runs, and nothing where that environment has no such
    /// container.
    pub from: Option<String>,
    pub to: Option<String>,
    /// What the gap between the two tags reads as, once the runs, the pull
    /// requests and the clone have been read. `diff` leaves it empty: nothing
    /// there opens a database.
    pub history: Option<ImageHistory>,
}

/// How a tag was read back to a run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Match {
    /// The tag is the run's build number.
    BuildNumber,
    /// The tag is the head of the commit the run built.
    Commit,
    /// The registry's `org.opencontainers.image.revision` annotation is.
    Revision,
}

impl std::fmt::Display for Match {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::BuildNumber => "build number",
            Self::Commit => "commit",
            Self::Revision => "revision",
        })
    }
}

/// The run one tag was built by.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ImageRun {
    /// The run's id, which is what `ticket-tui runs show` takes.
    pub id: i64,
    pub pipeline_id: i64,
    pub build_number: String,
    /// The commit it built, as the run reports it.
    pub commit: String,
    pub matched: Match,
}

impl ImageRun {
    fn new(run: &Run, matched: Match) -> Self {
        Self {
            id: run.id,
            pipeline_id: run.pipeline_id,
            build_number: run.build_number.clone(),
            commit: run.source_version.clone(),
            matched,
        }
    }
}

/// The image gap read back to what made it: the two runs, the pull requests
/// merged between their commits, and the work items those close.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ImageHistory {
    /// The run the `from` tag was built by, where one is on file.
    pub from: Option<ImageRun>,
    pub to: Option<ImageRun>,
    /// The pull requests the target environment is behind by, by id.
    pub pull_requests: Vec<i64>,
    /// The work items those pull requests close, by id.
    pub work_items: Vec<i64>,
    /// Why there is no list, when there is none: no clone to read the history
    /// from, no run on file for a tag, or whatever git said.
    pub note: Option<String>,
}

impl std::fmt::Display for ImageHistory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(note) = &self.note {
            return formatter.write_str(note);
        }
        if self.pull_requests.is_empty() {
            return formatter.write_str("no pull request between them");
        }
        write!(
            formatter,
            "{} PR{} behind: {}",
            self.pull_requests.len(),
            if self.pull_requests.len() == 1 {
                ""
            } else {
                "s"
            },
            join(&self.pull_requests, '!')
        )?;
        if !self.work_items.is_empty() {
            write!(formatter, " \u{2014} {}", join(&self.work_items, '#'))?;
        }
        Ok(())
    }
}

/// `!812 !815 !820`, `#642 #650`.
fn join(ids: &[i64], sigil: char) -> String {
    ids.iter()
        .map(|id| format!("{sigil}{id}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What `from` has that `to` has not, and the reverse marked as such, per
/// workload present in either. Pure: everything it answers with is in the two
/// manifests. `service` narrows it to the workloads whose name matches, which
/// is how one promotion is read without the rest of the environment.
#[must_use]
pub fn diff(from: &EnvManifest, to: &EnvManifest, service: Option<&str>) -> PromotionDiff {
    let mut services: Vec<ServiceDiff> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for workload in from.workloads.iter().chain(&to.workloads) {
        if !names(service, &workload.name) || seen.contains(&workload.name.as_str()) {
            continue;
        }
        seen.push(&workload.name);
        let here = workload_named(from, &workload.name);
        let there = workload_named(to, &workload.name);
        let service = match (here, there) {
            (Some(here), Some(there)) => ServiceDiff {
                workload: here.name.clone(),
                kind: here.kind.clone(),
                only_in: None,
                images: images(here, there),
                keys: keys(from, here, to, there),
                vault_objects: vault_objects(from, here, to, there),
                variables: variables(here, there),
            },
            // The chain reads `from` first, so the workload in hand is the one
            // environment's own account of it.
            (here, _) => ServiceDiff {
                workload: workload.name.clone(),
                kind: workload.kind.clone(),
                only_in: Some(if here.is_some() { Side::From } else { Side::To }),
                images: Vec::new(),
                keys: Vec::new(),
                vault_objects: Vec::new(),
                variables: Vec::new(),
            },
        };
        if !service.is_empty() {
            services.push(service);
        }
    }
    PromotionDiff {
        from: from.environment.clone(),
        to: to.environment.clone(),
        services,
    }
}

/// Whether one workload is the service the command named. A name that is not
/// the whole of the workload's still matches it — `orders` is `orders-api` —
/// because that is what a service is called out here.
fn names(service: Option<&str>, workload: &str) -> bool {
    service.is_none_or(|held| {
        workload.eq_ignore_ascii_case(held)
            || workload
                .to_ascii_lowercase()
                .contains(&held.to_ascii_lowercase())
    })
}

fn workload_named<'a>(manifest: &'a EnvManifest, name: &str) -> Option<&'a Workload> {
    manifest.workloads.iter().find(|held| held.name == name)
}

/// One container's image tag on each side, for every container whose tag
/// differs at all.
fn images(here: &Workload, there: &Workload) -> Vec<ImageChange> {
    let mut changes: Vec<ImageChange> = Vec::new();
    for container in here.containers.iter().chain(&there.containers) {
        if changes.iter().any(|held| held.container == container.name) {
            continue;
        }
        let (from, to) = (
            container_named(here, &container.name).map(|held| tag(&held.image).to_owned()),
            container_named(there, &container.name).map(|held| tag(&held.image).to_owned()),
        );
        if from != to {
            changes.push(ImageChange {
                container: container.name.clone(),
                from,
                to,
                history: None,
            });
        }
    }
    changes
}

fn container_named<'a>(workload: &'a Workload, name: &str) -> Option<&'a Container> {
    workload.containers.iter().find(|held| held.name == name)
}

/// What an image runs, which is the part a registry versions: the digest where
/// one is pinned, else whatever follows the last colon — and the whole of the
/// image where that colon is the registry's own port.
#[must_use]
pub fn tag(image: &str) -> &str {
    if let Some((_, digest)) = image.split_once('@') {
        return digest;
    }
    match image.rsplit_once(':') {
        Some((_, tag)) if !tag.contains('/') => tag,
        _ => image,
    }
}

/// Keys of the ConfigMaps and Secrets the workload references, on the side
/// that has them. An object one environment has no copy of at all is left to
/// `env check`, and one a provider pulls whole has keys neither side knows.
fn keys(from: &EnvManifest, here: &Workload, to: &EnvManifest, there: &Workload) -> Vec<Entry> {
    let mut objects = referenced(here);
    for held in referenced(there) {
        if !objects.contains(&held) {
            objects.push(held);
        }
    }
    let mut entries = Vec::new();
    for (object, name) in objects {
        let (Some(mine), Some(theirs)) = (
            object_keys(from, &here.namespace, object, &name),
            object_keys(to, &there.namespace, object, &name),
        ) else {
            continue;
        };
        entries.extend(one_sided(&mine, &theirs, &name));
    }
    entries
}

/// Every ConfigMap and Secret one workload names, once each, in the order it
/// names them. A `SecretProviderClass` holds no keys and is not one.
fn referenced(workload: &Workload) -> Vec<(ObjectKind, String)> {
    let mut objects: Vec<(ObjectKind, String)> = Vec::new();
    let references = workload
        .containers
        .iter()
        .flat_map(|container| &container.references)
        .chain(&workload.volumes);
    for reference in references {
        let held = (reference.object, reference.name.clone());
        if reference.object != ObjectKind::SecretProviderClass && !objects.contains(&held) {
            objects.push(held);
        }
    }
    objects
}

/// The keys one environment gives one object, and nothing at all where it has
/// no such object or pulls it whole.
fn object_keys(
    manifest: &EnvManifest,
    namespace: &str,
    object: ObjectKind,
    name: &str,
) -> Option<Vec<String>> {
    match object {
        ObjectKind::Secret => {
            let (keys, whole) = manifest.secret_keys(namespace, name)?;
            (!whole).then(|| keys.iter().map(|key| (*key).to_owned()).collect())
        }
        _ => {
            let mut keys: Vec<String> = Vec::new();
            let mut found = false;
            for held in &manifest.config_maps {
                if held.is(namespace, name) {
                    found = true;
                    keys.extend(held.keys.iter().cloned());
                }
            }
            found.then_some(keys)
        }
    }
}

/// Vault objects the workload's own providers pull, on the side that pulls
/// them. What a provider is the workload's is what it mounts and what it
/// produces.
fn vault_objects(
    from: &EnvManifest,
    here: &Workload,
    to: &EnvManifest,
    there: &Workload,
) -> Vec<Entry> {
    let (mine, theirs) = (
        pulled(&providers(from, here)),
        pulled(&providers(to, there)),
    );
    let mut entries: Vec<Entry> = Vec::new();
    for (side, held, other) in [(Side::From, &mine, &theirs), (Side::To, &theirs, &mine)] {
        for (vault, name) in held {
            let known = other.iter().any(|(_, held)| held == name)
                || entries
                    .iter()
                    .any(|entry| entry.side == side && entry.name == *name);
            if !known {
                entries.push(Entry {
                    object: vault.clone(),
                    name: name.clone(),
                    side,
                });
            }
        }
    }
    entries
}

/// What a set of providers asks its vaults for: the vault it is asked of — the
/// provider's own name, where it names no vault — and the object.
fn pulled(providers: &[&Provider]) -> Vec<(String, String)> {
    providers
        .iter()
        .flat_map(|provider| {
            let vault = provider
                .vault
                .clone()
                .unwrap_or_else(|| provider.name.clone());
            provider
                .objects
                .iter()
                .map(move |object| (vault.clone(), object.name.clone()))
        })
        .collect()
}

/// The providers one workload reaches: the `SecretProviderClass` it mounts,
/// and whatever produces a Secret it reads.
pub(crate) fn providers<'a>(manifest: &'a EnvManifest, workload: &Workload) -> Vec<&'a Provider> {
    let referenced = referenced(workload);
    let mounted: Vec<&str> = workload
        .containers
        .iter()
        .flat_map(|container| &container.references)
        .chain(&workload.volumes)
        .filter(|reference| reference.object == ObjectKind::SecretProviderClass)
        .map(|reference| reference.name.as_str())
        .collect();
    manifest
        .providers
        .iter()
        .filter(|provider| {
            mounted.contains(&provider.name.as_str())
                || provider.produces.iter().any(|produced| {
                    referenced.iter().any(|(object, name)| {
                        *object == ObjectKind::Secret && *name == produced.name
                    })
                })
        })
        .collect()
}

/// Variable names one container sets and the other does not, per container the
/// two environments both have.
fn variables(here: &Workload, there: &Workload) -> Vec<Entry> {
    let mut entries = Vec::new();
    for container in &here.containers {
        let Some(other) = container_named(there, &container.name) else {
            continue;
        };
        entries.extend(one_sided(
            &container.env_names,
            &other.env_names,
            &container.name,
        ));
    }
    entries
}

/// What one list has that the other has not, `from` first and the reverse
/// marked, sorted and without repeats.
fn one_sided(mine: &[String], theirs: &[String], object: &str) -> Vec<Entry> {
    let (left, right): (BTreeSet<&str>, BTreeSet<&str>) = (
        mine.iter().map(String::as_str).collect(),
        theirs.iter().map(String::as_str).collect(),
    );
    let entry = |name: &&str, side| Entry {
        object: object.to_owned(),
        name: (*name).to_owned(),
        side,
    };
    let mut entries: Vec<Entry> = left
        .difference(&right)
        .map(|name| entry(name, Side::From))
        .collect();
    entries.extend(right.difference(&left).map(|name| entry(name, Side::To)));
    entries
}

/// Which part of a promotion one line belongs to. The board draws a rule per
/// section; the pre-flight, which says the same things as sentences, ignores
/// them.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    /// The workload itself, which one side has and the other has not.
    Service,
    Image,
    Secrets,
    Config,
    Variables,
}

impl Section {
    /// What the rule over the section says.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Service => "Service",
            Self::Image => "Image",
            Self::Secrets => "Secrets",
            Self::Config => "Config",
            Self::Variables => "Variables",
        }
    }
}

/// What one line names that another tab holds, so a reader with tabs can point
/// at it and one without can ignore it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Names {
    Nothing,
    /// The vault, and the object pulled from it.
    VaultObject {
        vault: String,
        object: String,
    },
    /// The container whose image moves, which a caller with the runs on file
    /// reads back to the build that made it.
    Container(String),
}

/// One line of a promotion, in the words both readers use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromotionLine {
    pub section: Section,
    pub text: String,
    pub names: Names,
}

impl PromotionLine {
    fn plain(section: Section, text: String) -> Self {
        Self {
            section,
            text,
            names: Names::Nothing,
        }
    }
}

/// One service's promotion, line by line, in the one wording the environments
/// board and the pull request pre-flight both read.
///
/// A diff is handed in as `diff(what is arriving, what is there)`: the board
/// reads qa into prod, the pre-flight reads the source branch into the target,
/// and both come out as what the change would add to, and take away from, the
/// environment `diff.to` names. `Side::From` is therefore always the arriving
/// side, which is why one function serves both.
#[must_use]
pub fn promotion_lines(diff: &PromotionDiff, service: &ServiceDiff) -> Vec<PromotionLine> {
    let mut lines = Vec::new();
    let place = |object: &str| format!("{}/{object}", diff.to);
    if let Some(side) = service.only_in {
        let verb = if side == Side::From {
            "adds"
        } else {
            "removes"
        };
        lines.push(PromotionLine::plain(
            Section::Service,
            format!("{verb} {} {}", service.kind, service.workload),
        ));
        return lines;
    }
    for change in &service.images {
        let text = match (&change.from, &change.to) {
            (Some(arriving), Some(there)) => {
                format!("moves {} from {there} to {arriving}", change.container)
            }
            (Some(arriving), None) => format!("adds {} at {arriving}", change.container),
            (None, _) => format!("removes {}", change.container),
        };
        lines.push(PromotionLine {
            section: Section::Image,
            text,
            names: Names::Container(change.container.clone()),
        });
    }
    for entry in &service.vault_objects {
        let verb = if entry.side == Side::From {
            "pulls"
        } else {
            "stops pulling"
        };
        lines.push(PromotionLine {
            section: Section::Secrets,
            text: format!("{verb} {} from {}", entry.name, entry.object),
            names: Names::VaultObject {
                vault: entry.object.clone(),
                object: entry.name.clone(),
            },
        });
    }
    for entry in &service.keys {
        let text = if entry.side == Side::From {
            format!("adds {} to {}", entry.name, place(&entry.object))
        } else {
            format!("removes {} from {}", entry.name, place(&entry.object))
        };
        lines.push(PromotionLine::plain(Section::Config, text));
    }
    for entry in &service.variables {
        let text = if entry.side == Side::From {
            format!("sets {} on {}", entry.name, place(&entry.object))
        } else {
            format!("unsets {} on {}", entry.name, place(&entry.object))
        };
        lines.push(PromotionLine::plain(Section::Variables, text));
    }
    lines
}

/// One tag read back to the run that built it: the run whose build number it
/// is, else the run whose commit it is the head of, else the run the
/// registry's `org.opencontainers.image.revision` annotation names — in that
/// order, over every run, so a newer run's build number always beats an older
/// run's commit. The annotation is handed in: nothing here reads a registry.
#[must_use]
pub fn read_back(tag: &str, runs: &[Run], revision: Option<&str>) -> Option<ImageRun> {
    if tag.is_empty() {
        return None;
    }
    for rule in [Match::BuildNumber, Match::Commit, Match::Revision] {
        if let Some(run) = runs.iter().find(|run| holds(rule, tag, run, revision)) {
            return Some(ImageRun::new(run, rule));
        }
    }
    None
}

fn holds(rule: Match, tag: &str, run: &Run, revision: Option<&str>) -> bool {
    match rule {
        Match::BuildNumber => run.build_number == tag,
        Match::Commit => commit_like(tag) && run.source_version.starts_with(tag),
        Match::Revision => {
            revision.is_some_and(|held| commit_like(held) && run.source_version.starts_with(held))
        }
    }
}

/// Whether a string could be the head of a commit at all, so that `1.4.0` is
/// never read as one.
fn commit_like(held: &str) -> bool {
    held.len() >= 7 && held.chars().all(|character| character.is_ascii_hexdigit())
}

/// A `git log <older>..<newer>` in the service's clone, a line each of commit
/// and subject — or the one line to print in place of a list, which is what a
/// repository with no clone, or a commit git has never heard of, comes back as.
pub type GitLog<'a> = &'a dyn Fn(&str, &str) -> Result<Vec<String>, String>;

/// One image gap, read back to what made it. `log(a, b)` is `git log a..b` in
/// the service's clone, a line each of `<commit> <subject>`; whatever it cannot
/// do — no clone, no such commit — comes back as the note the line prints
/// instead of a list.
pub fn history(
    change: &ImageChange,
    runs: &[Run],
    requests: &[PullRequest],
    repo: Option<&str>,
    revision: &dyn Fn(&str) -> Option<String>,
    log: GitLog<'_>,
) -> ImageHistory {
    let (Some(from_tag), Some(to_tag)) = (&change.from, &change.to) else {
        return ImageHistory::default();
    };
    let mut history = ImageHistory {
        from: read_back(from_tag, runs, revision(from_tag).as_deref()),
        to: read_back(to_tag, runs, revision(to_tag).as_deref()),
        ..ImageHistory::default()
    };
    let (Some(from), Some(to)) = (&history.from, &history.to) else {
        let unread = if history.from.is_none() {
            from_tag
        } else {
            to_tag
        };
        history.note = Some(format!("no run on file for {unread}"));
        return history;
    };
    // The range is what the target environment has not got: the commit it runs
    // first, the commit the source runs second.
    match log(&to.commit, &from.commit) {
        Ok(lines) => {
            let found = between(&lines, requests, repo);
            history.pull_requests = found.iter().map(|request| request.id).collect();
            history.work_items = found
                .iter()
                .flat_map(|request| request.work_items.iter().copied())
                .collect::<BTreeSet<i64>>()
                .into_iter()
                .collect();
        }
        Err(why) => history.note = Some(why),
    }
    history
}

/// The pull requests one `git log a..b` holds, oldest id first. A stored pull
/// request is in the range when the range holds the commit it was last read
/// at, or when a subject names it the way Azure DevOps writes a merge —
/// `Merged PR 812: …` — which is what a squash leaves behind instead of the
/// commit.
#[must_use]
pub fn between<'a>(
    lines: &[String],
    requests: &'a [PullRequest],
    repo: Option<&str>,
) -> Vec<&'a PullRequest> {
    let mut commits: Vec<&str> = Vec::new();
    let mut merged: BTreeSet<i64> = BTreeSet::new();
    for line in lines {
        let (commit, subject) = line
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((line.trim(), ""));
        if !commit.is_empty() {
            commits.push(commit);
        }
        if let Some(id) = merged_pull_request(subject) {
            merged.insert(id);
        }
    }
    let mut found: Vec<&PullRequest> = requests
        .iter()
        .filter(|request| repo.is_none_or(|held| request.repo_id == held))
        .filter(|request| {
            merged.contains(&request.id)
                || commits
                    .iter()
                    .any(|commit| same_commit(commit, &request.last_merge_source_commit))
        })
        .collect();
    found.sort_by_key(|request| request.id);
    found
}

/// `812` out of `Merged PR 812: Split the files`.
fn merged_pull_request(subject: &str) -> Option<i64> {
    subject
        .strip_prefix("Merged PR ")?
        .split(|character: char| !character.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// Whether two commits are the same one written to different lengths, which is
/// what a stored abbreviation and a full `git log` hash are.
fn same_commit(left: &str, right: &str) -> bool {
    commit_like(left) && commit_like(right) && (left.starts_with(right) || right.starts_with(left))
}

#[cfg(test)]
mod tests;
