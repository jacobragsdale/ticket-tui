//! The command line: the flags every run takes, and the subcommands that let
//! an agent read and change work items without opening the TUI.
//!
//! A bare invocation still opens the TUI, which is what `ticket-tui` has
//! always been. Every subcommand does one thing and exits. The reads answer
//! from SQLite and never touch the network; the writes go out over the same
//! trait-backed source the TUI's sync worker uses and store the copy Azure
//! DevOps answers with, so a running TUI picks the change up from the database
//! it is already watching.

use std::fs;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueHint};
use serde::Serialize;
use serde_json::Value;

use crate::app::pipelines::RunSchema;
use crate::app::pipelines::rows::{RunRow, duration_label, run_glyph, short_branch};
use crate::app::pull_requests::{PrRow, PrSchema};
use crate::app::repos::{RepoRow, RepoSchema};
use crate::azure::{self, AzureClient, AzureConfig};
use crate::classification::{self, NodeKind};
use crate::config::Config;
use crate::db::{self, SqliteTicketRepository, default_database_path};
use crate::edit::{FieldEdit, normalize_tags, revision_test};
use crate::filter::{FilterField, MatchContext, ParsedQuery, WorkItemSchema, parse_query};
use crate::local;
use crate::markdown;
use crate::model::{
    Approval, CommentRecord, CompletionOptions, Identity, MergeStrategy, Pipeline, PullRequest,
    Run, RunResult, Ticket, TicketKey, TimelineKind, TimelineRecord, same_text,
};
use crate::search;
use crate::status;
use crate::sync::{self, AzureConnector, PrAction, SyncMode, SyncOutcome, WorkItemSource};
use crate::timestamp::Timestamp;
use crate::ui::repos::size_label;
use crate::watch::{LIVE_RUNS_CADENCE, LOG_CADENCE, PipelineSource};

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// SQLite database to open instead of the platform data-directory default
    #[arg(long, global = true, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub database: Option<PathBuf>,
    /// Azure DevOps organization (slug or URL); defaults to TICKET_TUI_ORG,
    /// then config.toml, then `az devops configure`
    #[arg(long, global = true, value_name = "ORG")]
    pub org: Option<String>,
    /// Azure DevOps project the work items live in; defaults to
    /// TICKET_TUI_PROJECT, then config.toml, then `az devops configure`
    #[arg(long, global = true, value_name = "PROJECT")]
    pub project: Option<String>,
    /// Azure DevOps project the repositories, pull requests and pipelines live
    /// in; defaults to TICKET_TUI_CODE_PROJECT, then config.toml, then
    /// whatever --project settled on
    #[arg(long, global = true, value_name = "PROJECT")]
    pub code_project: Option<String>,
    /// Seconds between background pulls from Azure DevOps, 0 to turn the timer
    /// off; defaults to TICKET_TUI_REFRESH or 60
    #[arg(long, value_name = "SECONDS")]
    pub refresh: Option<u64>,
    /// Extra WIQL condition ANDed into every pull, narrowing a large project;
    /// defaults to TICKET_TUI_QUERY, then config.toml
    #[arg(long, value_name = "WIQL")]
    pub query: Option<String>,
    /// The team whose slice of the project to work in: its areas narrow every
    /// pull, its members fill the assignee picker, and its sprint is what
    /// `@current` means; defaults to TICKET_TUI_TEAM, then config.toml
    #[arg(long, global = true, value_name = "TEAM")]
    pub team: Option<String>,
    /// Days a work item may sit untouched before the Changed column flags it
    /// as stale; defaults to TICKET_TUI_STALE_DAYS, then whatever the session
    /// remembers, then 14
    #[arg(long, value_name = "DAYS")]
    pub stale_days: Option<u16>,
    /// Colour theme: terminal, terminal-light, mono, or custom (the palette in
    /// config.toml); defaults to TICKET_TUI_THEME, then what config.toml says
    #[arg(long, value_name = "NAME")]
    pub theme: Option<String>,
    /// Directory the Repos tab looks for clones in and makes new ones under;
    /// defaults to TICKET_TUI_WORKSPACE, then config.toml, then ~/Development
    #[arg(long, global = true, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub workspace: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// What `config.toml` says, for everything the flags and the environment
    /// left unsaid: flag, then `TICKET_TUI_*`, then the file, and whatever
    /// none of the three name is left for the Azure CLI to answer. `env` is
    /// handed in rather than read here, so the order can be tested without an
    /// environment to set.
    #[must_use]
    pub fn with_file_defaults(
        mut self,
        file: &Config,
        env: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let variable = |key: &str| {
            env(key)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let settled = |flag: Option<String>, key: &str, file: Option<&String>| {
            flag.or_else(|| variable(key)).or_else(|| file.cloned())
        };
        self.org = settled(self.org, "TICKET_TUI_ORG", file.devops.org.as_ref());
        self.project = settled(
            self.project,
            "TICKET_TUI_PROJECT",
            file.devops.project.as_ref(),
        );
        self.code_project = settled(
            self.code_project,
            "TICKET_TUI_CODE_PROJECT",
            file.devops.code_project.as_ref(),
        );
        self.query = settled(self.query, "TICKET_TUI_QUERY", file.devops.query.as_ref());
        self.team = settled(self.team, "TICKET_TUI_TEAM", file.devops.team.as_ref());
        self.workspace = self
            .workspace
            .or_else(|| variable("TICKET_TUI_WORKSPACE").map(PathBuf::from))
            .or_else(|| file.devops.workspace.clone());
        self
    }
}

/// One thing to do and then exit. The flags above still apply: `--database`,
/// `--org`, `--project` and `--code-project` may be written either side of the
/// subcommand, and what none of them says `config.toml` answers for.
#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Pull work items from Azure DevOps into the database and exit
    Sync {
        /// Replace every stored work item rather than pulling only what has
        /// changed since the last pull
        #[arg(long)]
        full: bool,
    },
    /// Print one work item from the database, without touching the network
    Show {
        /// The work item to print
        id: i64,
        /// Print it as a JSON object rather than as a block of text
        #[arg(long)]
        json: bool,
    },
    /// Print work items from the database, without touching the network
    List {
        /// The TUI's own filter grammar, such as `state:doing assignee:@me`;
        /// anything that is not a `field:value` pair is matched fuzzily
        #[arg(long, value_name = "QUERY")]
        query: Option<String>,
        /// Print the rows as a JSON array rather than as a table
        #[arg(long)]
        json: bool,
    },
    /// Change one work item's fields in Azure DevOps
    Edit(EditArgs),
    /// Leave one comment on a work item
    Comment {
        /// The work item to comment on
        id: i64,
        /// What the comment says, as plain text. `-` reads the body from
        /// standard input instead — `cargo test | tail -30 | ticket-tui
        /// comment 642 -` — which is also what leaving it out does when
        /// something is piped in. A piped body is posted as a code block, so
        /// its columns stay lined up.
        text: Option<String>,
    },
    /// Add a work item to the project
    Create(CreateArgs),
    /// Read the project's Git repositories and the clones on this machine
    #[command(subcommand)]
    Repos(ReposCommand),
    /// Read and act on pull requests
    #[command(subcommand)]
    Prs(PrsCommand),
    /// Print the project's build definitions
    Pipelines {
        #[arg(long)]
        json: bool,
    },
    /// Read, start and stop pipeline runs
    #[command(subcommand)]
    Runs(RunsCommand),
    /// Read and answer the approvals a run is waiting on
    #[command(subcommand)]
    Approvals(ApprovalsCommand),
    /// Print the tab badges as one line, for a status bar or a shell prompt
    Status {
        /// Print every figure as a JSON object, zeros and all, rather than as
        /// the line
        #[arg(long)]
        json: bool,
    },
    /// Print the project's teams, one name a line, to copy into config.toml
    Teams,
}

/// The Pipelines tab, without the tab. `list` answers from the database; every
/// other form reads or writes Azure DevOps, because a timeline, a log and a
/// run's own progress are not things a pull stores.
#[derive(Clone, Debug, Subcommand)]
pub enum RunsCommand {
    /// Print runs, newest first
    List {
        /// Only this pipeline's runs, by name
        #[arg(long, value_name = "NAME")]
        pipeline: Option<String>,
        /// The Pipelines tab's run grammar: `pipeline:`, `status:`, `result:`,
        /// `branch:`, `by:` and `reason:`
        #[arg(long, value_name = "QUERY")]
        query: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print one run and its timeline
    Show {
        id: i64,
        #[arg(long)]
        json: bool,
    },
    /// Print one node's log
    Logs {
        id: i64,
        /// The job whose log to print, by name
        #[arg(long, value_name = "NAME", conflicts_with = "task")]
        job: Option<String>,
        /// The task whose log to print, by name
        #[arg(long, value_name = "NAME")]
        task: Option<String>,
        /// Keep printing as the node writes, until it finishes
        #[arg(long)]
        follow: bool,
    },
    /// Start one pipeline on one branch
    Trigger {
        /// The pipeline's name, as the project spells it
        pipeline: String,
        /// The branch to build, with or without `refs/heads/`
        #[arg(long, value_name = "NAME")]
        branch: String,
        /// Tail the deepest running node's log until the run finishes
        #[arg(long)]
        follow: bool,
    },
    /// Stop one run
    Cancel { id: i64 },
    /// Retry the jobs that failed in one run
    Retry { id: i64 },
    /// Wait for one run to finish, exiting 0 succeeded, 1 failed, 2 canceled,
    /// 3 partially succeeded
    Wait { id: i64 },
}

#[derive(Clone, Debug, Subcommand)]
pub enum ApprovalsCommand {
    /// Print the approvals the project is waiting on
    List {
        #[arg(long)]
        json: bool,
    },
    /// Approve one
    Approve {
        id: String,
        #[arg(long, value_name = "TEXT")]
        comment: Option<String>,
    },
    /// Reject one
    Reject {
        id: String,
        #[arg(long, value_name = "TEXT")]
        comment: Option<String>,
    },
}

/// The Repos tab, without the tab. Both reads answer from the database and the
/// workspace; neither touches the network.
#[derive(Clone, Debug, Subcommand)]
pub enum ReposCommand {
    /// Print the project's repositories
    List {
        /// The Repos tab's filter grammar: `name:`, `branch:`, `local:` and
        /// `disabled:`; anything else is matched fuzzily
        #[arg(long, value_name = "QUERY")]
        query: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print one repository, its URLs and the clone on this machine
    Show {
        /// The repository's name, as the project spells it
        name: String,
        #[arg(long)]
        json: bool,
    },
}

/// The Pull requests tab, without the tab. The two reads answer from the
/// database; every other form writes to Azure DevOps and stores the copy it
/// answers with, so a running TUI picks the change up from the database it is
/// already watching.
#[derive(Clone, Debug, Subcommand)]
pub enum PrsCommand {
    /// Print pull requests
    List {
        /// The Pull requests tab's filter grammar: `repo:`, `author:`,
        /// `reviewer:` (`@me`), `vote:`, `status:`, `target:`, `source:`,
        /// `draft:` and `build:`; anything else is matched fuzzily
        #[arg(long, value_name = "QUERY")]
        query: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print one pull request: its reviewers, work items, build and discussion
    Show {
        id: i64,
        #[arg(long)]
        json: bool,
    },
    /// Record your own vote
    Vote {
        id: i64,
        /// `approve`, `suggest`, `wait`, `reject`, or `none` to withdraw
        vote: String,
    },
    /// Complete it, merging the source branch into the target
    Complete {
        id: i64,
        /// How to merge: `squash` (the default), `merge` or `rebase`
        #[arg(long, value_name = "STRATEGY")]
        strategy: Option<String>,
        /// Leave the source branch in place rather than deleting it
        #[arg(long)]
        keep_source: bool,
        /// Leave the linked work items in the state they are in
        #[arg(long)]
        no_transition: bool,
    },
    /// Abandon it
    Abandon { id: i64 },
    /// Turn auto-complete on or off
    Autocomplete {
        id: i64,
        /// `on` or `off`
        state: String,
    },
    /// Leave one comment, as a thread of its own
    Comment {
        id: i64,
        /// What the comment says, as plain text. `-` reads the body from
        /// standard input instead, as does leaving it out when something is
        /// piped in; a piped body is posted as a code block.
        text: Option<String>,
    },
}

#[derive(Args, Clone, Debug)]
pub struct EditArgs {
    /// The work item to change
    pub id: i64,
    /// The state to move it to, such as `Doing`
    #[arg(long, value_name = "STATE")]
    pub state: Option<String>,
    /// Display name or sign-in address; an empty value takes the work item off
    /// whoever holds it, and `@me` means the signed-in user
    #[arg(long, value_name = "ASSIGNEE")]
    pub assignee: Option<String>,
    /// The priority to set, as the process template numbers them
    #[arg(long, value_name = "PRIORITY")]
    pub priority: Option<i64>,
    /// Full iteration path, such as `atlas\Sprint 1`
    #[arg(long, value_name = "ITERATION")]
    pub iteration: Option<String>,
    /// Full area path, such as `atlas\Platform`
    #[arg(long, value_name = "AREA")]
    pub area: Option<String>,
    /// A new title for the work item
    #[arg(long, value_name = "TITLE")]
    pub title: Option<String>,
    /// Comma-separated tag list, which replaces the tags the work item has
    #[arg(long, value_name = "TAGS")]
    pub tags: Option<String>,
    /// A file of Markdown to write over the description
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub description_file: Option<PathBuf>,
}

#[derive(Args, Clone, Debug)]
pub struct CreateArgs {
    /// Work item type, such as `Issue`, `Task`, or `User Story`
    #[arg(long = "type", value_name = "TYPE")]
    pub work_item_type: String,
    /// What the new work item is called
    #[arg(long, value_name = "TITLE")]
    pub title: String,
    /// The work item this one hangs under
    #[arg(long, value_name = "ID")]
    pub parent: Option<i64>,
    /// Full iteration path, such as `atlas\Sprint 1`
    #[arg(long, value_name = "ITERATION")]
    pub iteration: Option<String>,
    /// Display name or sign-in address, or `@me` for the signed-in user
    #[arg(long, value_name = "ASSIGNEE")]
    pub assignee: Option<String>,
    /// The priority to set, as the process template numbers them
    #[arg(long, value_name = "PRIORITY")]
    pub priority: Option<i64>,
    /// Comma-separated tag list
    #[arg(long, value_name = "TAGS")]
    pub tags: Option<String>,
}

/// Runs one subcommand to completion and prints what it did. Everything it
/// reports goes to standard output; a failure comes back as an error, which
/// the caller prints and exits non-zero on.
pub fn run(cli: &Cli, command: &Command) -> Result<()> {
    let database = cli.database.clone().unwrap_or_else(default_database_path);
    match command {
        Command::Sync { full } => run_sync(cli, database, *full),
        Command::Show { id, json } => run_show(&database, *id, *json),
        Command::List { query, json } => run_list(&database, query.as_deref(), *json),
        Command::Edit(args) => run_edit(cli, &database, args),
        Command::Comment { id, text } => run_comment(cli, &database, *id, text.as_deref()),
        Command::Create(args) => run_create(cli, &database, args),
        Command::Repos(command) => run_repos(cli, &database, command),
        Command::Prs(command) => run_prs(cli, &database, command),
        Command::Pipelines { json } => run_pipelines(&database, *json),
        Command::Runs(command) => run_runs(cli, &database, command),
        Command::Approvals(command) => run_approvals(cli, command),
        Command::Status { json } => run_status(cli, &database, *json),
        Command::Teams => run_teams(cli),
    }
}

/// The project's teams as Azure DevOps names them, one a line, so `team` in
/// `config.toml` can be written exactly as the project spells it.
fn run_teams(cli: &Cli) -> Result<()> {
    for team in connect(cli)?.fetch_teams()? {
        println!("{team}");
    }
    Ok(())
}

/// The tab badges on one line, from SQLite and the context file beside it and
/// nothing else. It prints nothing when there is nothing to say, so a shell
/// prompt can call it every time it draws.
fn run_status(cli: &Cli, database: &Path, json: bool) -> Result<()> {
    let repository = open_database(database)?;
    let me = resolve_me(
        repository.meta(db::ME_DISPLAY_NAME_KEY)?,
        std::env::var("TICKET_TUI_ME").ok(),
    );
    let asked = resolve_stale_days(cli.stale_days, std::env::var("TICKET_TUI_STALE_DAYS").ok())?;
    let status = status::collect(
        &repository,
        me.as_deref(),
        status::stale_days(asked, database),
        resolve_refresh(cli.refresh, std::env::var("TICKET_TUI_REFRESH").ok())?,
        Timestamp::now(),
    )?;
    if let Some(line) = status::report(&status, json)? {
        emit(&line);
    }
    Ok(())
}

/// Who `@me` is: the display name the last sync recorded, overridden by
/// `TICKET_TUI_ME` for anyone whose profile name differs from the name their
/// work items are assigned to. Blank values count as unset.
#[must_use]
pub fn resolve_me(stored: Option<String>, env: Option<String>) -> Option<String> {
    [env, stored]
        .into_iter()
        .flatten()
        .map(|name| name.trim().to_owned())
        .find(|name| !name.is_empty())
}

/// How often the background pull runs when nothing says otherwise.
pub const DEFAULT_REFRESH_SECONDS: u64 = 60;

/// How often the background pull runs: `--refresh`, then `TICKET_TUI_REFRESH`,
/// then a minute. A variable that is not a number of seconds is a startup
/// error rather than a silent fall back to the default, because a typo there
/// would otherwise change how often the TUI reaches Azure DevOps and say
/// nothing about it. `status` reads it the same way, so the age it calls stale
/// is the age this run calls stale.
pub fn resolve_refresh(flag: Option<u64>, env: Option<String>) -> Result<u64> {
    if let Some(seconds) = flag {
        return Ok(seconds);
    }
    let Some(raw) = env else {
        return Ok(DEFAULT_REFRESH_SECONDS);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(DEFAULT_REFRESH_SECONDS);
    }
    trimmed
        .parse()
        .with_context(|| format!("TICKET_TUI_REFRESH is not a number of seconds: {trimmed}"))
}

/// How long a work item may sit untouched before the Changed column flags it:
/// `--stale-days`, then `TICKET_TUI_STALE_DAYS`, and `None` when neither was
/// given, which leaves whatever the session remembers standing. A variable
/// that is not a number of days is a startup error naming it, the way
/// `TICKET_TUI_REFRESH` is: a typo there would otherwise change which rows are
/// flagged and say nothing about it.
pub fn resolve_stale_days(flag: Option<u16>, env: Option<String>) -> Result<Option<u16>> {
    if let Some(days) = flag {
        return Ok(Some(days));
    }
    let Some(raw) = env else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse()
        .map(Some)
        .with_context(|| format!("TICKET_TUI_STALE_DAYS is not a number of days: {trimmed}"))
}

/// One pull, run to completion, reporting what moved. The database is opened
/// here first so a file that does not exist yet is created and given its
/// schema; the pull itself takes its own connection.
fn run_sync(cli: &Cli, database: PathBuf, full: bool) -> Result<()> {
    let config = AzureConfig::resolve(
        cli.org.clone(),
        cli.project.clone(),
        cli.code_project.clone(),
        cli.query.clone(),
        cli.team.clone(),
    )?;
    let repository = SqliteTicketRepository::open(&database)?;
    guard_stored_project(
        repository
            .meta(db::ORGANIZATION_KEY)?
            .zip(repository.meta(db::PROJECT_KEY)?),
        &config,
        full,
    )?;
    drop(repository);
    let outcome = sync::pull_once(
        database,
        Box::new(AzureConnector::new(config.clone())),
        full,
    );
    emit(&sync_report(&outcome, &config)?);
    Ok(())
}

/// Refuses to pull into a database another project filled, unless the pull is
/// the full one that replaces every row: an incremental pull would leave two
/// projects' work items side by side, and a full pull is how a database is
/// deliberately pointed somewhere else. It is the guard the TUI puts on the
/// database it opens, made to hold for a pull with no TUI behind it. A database
/// from before the project was recorded adopts whatever pulls it.
fn guard_stored_project(
    stored: Option<(String, String)>,
    config: &AzureConfig,
    full: bool,
) -> Result<()> {
    let Some((organization, project)) = stored else {
        return Ok(());
    };
    if full || (organization == config.organization && project == config.project) {
        return Ok(());
    }
    bail!(
        "database holds {organization}/{project}; pass --database for another project or --full to replace it"
    )
}

/// What one pull says for itself. A pull that reached Azure DevOps and found
/// nothing is not a failure and does not read like one; anything that stopped
/// it is an error, so the exit status says so too.
fn sync_report(outcome: &SyncOutcome, config: &AzureConfig) -> Result<String> {
    let summary = match outcome {
        SyncOutcome::Pulled {
            mode,
            count,
            snapshot,
        } => sync::pull_summary(*mode, *count, sync::PulledExtras::of(snapshot)),
        SyncOutcome::Unchanged => {
            sync::pull_summary(SyncMode::Incremental, 0, sync::PulledExtras::default())
        }
        SyncOutcome::Failed(message) => bail!("{message}"),
        SyncOutcome::Throttled { retry_after } => bail!(
            "Azure DevOps is throttling requests; try again in {}s",
            retry_after.as_secs()
        ),
    };
    Ok(format!(
        "{summary} from {}/{}",
        config.organization, config.project
    ))
}

fn run_show(database: &Path, id: i64, json: bool) -> Result<()> {
    let repository = open_database(database)?;
    let ticket = repository
        .load_all()?
        .into_iter()
        .find(|ticket| ticket.key.id == id)
        .with_context(|| {
            format!(
                "work item {id} is not in {}; run `ticket-tui sync`",
                database.display()
            )
        })?;
    emit(&if json {
        to_json(&TicketJson::detailed(&ticket))?
    } else {
        describe(&ticket)
    });
    Ok(())
}

fn run_list(database: &Path, query: Option<&str>, json: bool) -> Result<()> {
    let repository = open_database(database)?;
    let me = resolve_me(
        repository.meta(db::ME_DISPLAY_NAME_KEY)?,
        std::env::var("TICKET_TUI_ME").ok(),
    );
    let (context, tree) = match_context(&repository, me)?;
    let rows = select(repository.load_all()?, query, &context, tree)?;
    emit(&if json {
        let rows: Vec<TicketJson<'_>> = rows.iter().map(TicketJson::row).collect();
        to_json(&rows)?
    } else {
        tabulate(&rows)
    });
    Ok(())
}

/// The work items one `--query` names, newest change first so the same
/// invocation twice running answers in the same order. The grammar is the
/// TUI's own: `field:value` pairs narrow, and whatever is left over is matched
/// fuzzily and orders the rows by how well it matched.
fn select(
    tickets: Vec<Ticket>,
    query: Option<&str>,
    context: &MatchContext,
    tree: IterationTree,
) -> Result<Vec<Ticket>> {
    let Some(query) = query else {
        return Ok(by_recency(tickets));
    };
    let parsed = parse_query::<WorkItemSchema>(query);
    refuse_unresolvable_sentinels(&parsed, context, tree)?;
    let matching: Vec<Ticket> = tickets
        .into_iter()
        // Nothing is bookmarked out here: bookmarks live in the TUI's session
        // file, which a one-shot read has no business reaching into.
        .filter(|ticket| parsed.filters.matches_in(ticket, false, context))
        .collect();
    if parsed.fuzzy.is_empty() {
        return Ok(by_recency(matching));
    }
    Ok(search::rank(&matching, &parsed.fuzzy)
        .into_iter()
        .map(|found| matching[found.ticket_index].clone())
        .collect())
}

/// Newest change first, and by id when two work items changed in the same
/// second, which is the order the table opens on.
fn by_recency(mut tickets: Vec<Ticket>) -> Vec<Ticket> {
    tickets.sort_by(|left, right| {
        right
            .changed_at
            .cmp(&left.changed_at)
            .then(right.key.id.cmp(&left.key.id))
    });
    tickets
}

/// What the query's sentinels stand for out here: the name the last sync
/// recorded (or `TICKET_TUI_ME`) for `assignee:@me`, and the sprint containing
/// today for `iteration:@current`, read from the same cached trees the TUI's
/// pickers use. Built the same way `App::match_context` builds it, so a query
/// means the same thing from the command line as it does in the search box.
fn match_context(
    repository: &SqliteTicketRepository,
    me: Option<String>,
) -> Result<(MatchContext, IterationTree)> {
    let today = Timestamp::now().date();
    let nodes = repository.load_classification_nodes()?;
    let tree = if nodes.iter().any(|node| node.kind == NodeKind::Iteration) {
        IterationTree::Cached
    } else {
        IterationTree::Unread
    };
    let current_iteration =
        classification::current_iteration(&nodes, today).map(|node| node.path.clone());
    Ok((
        MatchContext::now()
            .with_me(me)
            .with_current_iteration(current_iteration),
        tree,
    ))
}

/// Whether a run has the iteration tree to resolve `@current` against, which is
/// what separates "nobody has pulled it yet" from "no sprint is scheduled
/// around today" when the sentinel comes up empty.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IterationTree {
    Cached,
    Unread,
}

/// A sentinel the context cannot fill in matches nothing, which from a
/// one-shot command reads exactly like an empty backlog. Out here that is worth
/// saying rather than answering with a blank list: the TUI can show the query
/// beside its chips, but a pipe cannot.
fn refuse_unresolvable_sentinels(
    parsed: &ParsedQuery<WorkItemSchema>,
    context: &MatchContext,
    tree: IterationTree,
) -> Result<()> {
    if context.me.is_none() && parsed.filters.contains(FilterField::Assignee, "@me") {
        bail!("no signed-in name to resolve @me; run `ticket-tui sync` once or set TICKET_TUI_ME");
    }
    if context.current_iteration.is_none()
        && parsed.filters.contains(FilterField::Iteration, "@current")
    {
        match tree {
            IterationTree::Unread => bail!(
                "no iteration tree to resolve iteration:@current against; run `ticket-tui sync` once"
            ),
            // The tree is here and still nothing contains today, which on a
            // project whose sprints carry no dates is every day: say which of
            // the two it is rather than sending them back to `sync`.
            IterationTree::Cached => bail!(
                "no sprint's dates contain today, so iteration:@current names nothing; give the iteration start and finish dates in Azure DevOps, or name the sprint"
            ),
        }
    }
    Ok(())
}

/// Writes the fields the invocation named, all in one document, and stores the
/// copy Azure DevOps answers with.
fn run_edit(cli: &Cli, database: &Path, args: &EditArgs) -> Result<()> {
    let mut repository = open_database(database)?;
    let me = resolve_me(
        repository.meta(db::ME_DISPLAY_NAME_KEY)?,
        std::env::var("TICKET_TUI_ME").ok(),
    );
    let edits = field_edits(args, &repository.load_identities()?, me.as_deref())?;
    if edits.is_empty() {
        bail!(
            "nothing to change; pass at least one of --state, --assignee, --priority, --iteration, --area, --title, --tags, --description-file"
        );
    }
    let client = connect(cli)?;
    let key = key_for(&client, args.id);
    let ticket = apply_edits(&client, &mut repository, &key, &edits)?;
    let summary = edits
        .iter()
        .map(FieldEdit::summary)
        .collect::<Vec<_>>()
        .join(", ");
    emit(&format!(
        "#{} rev {}: {summary}",
        ticket.key.id, ticket.revision
    ));
    Ok(())
}

/// One document holding every field the invocation named, and the row Azure
/// DevOps answered with written back to the database.
///
/// The revision the database holds leads the document, the way an edit made in
/// the TUI leads with the revision the row was read at, so a work item that
/// moved on since the last pull is refused rather than overwritten. A work item
/// the database has never seen goes out without a test: there is no revision to
/// claim it was read at.
fn apply_edits(
    source: &dyn WorkItemSource,
    repository: &mut SqliteTicketRepository,
    key: &TicketKey,
    edits: &[FieldEdit],
) -> Result<Ticket> {
    let document = edit_document(repository.revision_of(key)?, edits);
    let (ticket, relations, artifacts) = source
        .patch_work_item(key.id, &document)
        .map_err(|error| conflict_advice(error, key.id))?;
    repository.upsert(&ticket, &relations, &artifacts)?;
    Ok(ticket)
}

/// The JSON Patch document one `edit` sends: the revision test, when there is
/// a revision to test, then the operations behind each field.
fn edit_document(expected_revision: Option<i64>, edits: &[FieldEdit]) -> Vec<Value> {
    let mut document: Vec<Value> = expected_revision.map(revision_test).into_iter().collect();
    for edit in edits {
        document.extend(edit.patch());
    }
    document
}

/// A refused write that means the work item moved on says what to do about it;
/// every other refusal is reported as Azure DevOps worded it.
fn conflict_advice(error: anyhow::Error, id: i64) -> anyhow::Error {
    if azure::is_write_conflict(&error) {
        return error.context(format!(
            "#{id} changed in Azure DevOps since the last sync; run `ticket-tui sync` and try again"
        ));
    }
    error
}

/// Every field change the invocation asked for, in the order the flags are
/// documented, so a notification about several of them reads the same way
/// twice.
fn field_edits(
    args: &EditArgs,
    identities: &[Identity],
    me: Option<&str>,
) -> Result<Vec<FieldEdit>> {
    let mut edits = Vec::new();
    if let Some(state) = &args.state {
        edits.push(FieldEdit::state(state.trim()));
    }
    if let Some(assignee) = &args.assignee {
        edits.push(assignee_edit(assignee, identities, me)?);
    }
    if let Some(priority) = args.priority {
        edits.push(FieldEdit::priority(priority));
    }
    if let Some(iteration) = &args.iteration {
        edits.push(FieldEdit::iteration(iteration.trim()));
    }
    if let Some(area) = &args.area {
        edits.push(FieldEdit::area(area.trim()));
    }
    if let Some(title) = &args.title {
        let title = title.trim();
        if title.is_empty() {
            bail!("a work item cannot be given an empty title");
        }
        edits.push(FieldEdit::title(title));
    }
    if let Some(tags) = &args.tags {
        edits.push(FieldEdit::tags(&tag_list(tags)));
    }
    if let Some(path) = &args.description_file {
        edits.push(FieldEdit::description(&description_html(path)?));
    }
    Ok(edits)
}

/// The operations that set a new work item's fields. A creation carries no
/// revision test and no parent — the parent is a link, which
/// [`crate::azure::create_document`] appends — so this is the field half of the
/// document and nothing else.
fn create_edits(
    args: &CreateArgs,
    identities: &[Identity],
    me: Option<&str>,
) -> Result<Vec<FieldEdit>> {
    let title = args.title.trim();
    if title.is_empty() {
        bail!("a work item cannot be created without a title");
    }
    let mut edits = vec![FieldEdit::title(title)];
    if let Some(assignee) = &args.assignee {
        edits.push(assignee_edit(assignee, identities, me)?);
    }
    if let Some(priority) = args.priority {
        edits.push(FieldEdit::priority(priority));
    }
    if let Some(iteration) = &args.iteration {
        edits.push(FieldEdit::iteration(iteration.trim()));
    }
    if let Some(tags) = &args.tags {
        edits.push(FieldEdit::tags(&tag_list(tags)));
    }
    Ok(edits)
}

/// The operations behind a list of field edits, in order.
fn patch_ops(edits: &[FieldEdit]) -> Vec<Value> {
    edits.iter().flat_map(FieldEdit::patch).collect()
}

/// Who a `--assignee` names. A name the database already knows is written by
/// the address the assignee picker would have used, and anything else goes out
/// as it was typed for Azure DevOps to resolve; `@me` is the signed-in user,
/// and an empty name takes the work item off whoever holds it.
fn assignee_edit(raw: &str, identities: &[Identity], me: Option<&str>) -> Result<FieldEdit> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(FieldEdit::unassign());
    }
    let name = if raw.eq_ignore_ascii_case("@me") {
        me.context(
            "no signed-in name to resolve @me; run `ticket-tui sync` once or set TICKET_TUI_ME",
        )?
    } else {
        raw
    };
    Ok(identities
        .iter()
        .find(|identity| {
            identity.display_name.eq_ignore_ascii_case(name)
                || identity
                    .unique_name
                    .as_deref()
                    .is_some_and(|unique| unique.eq_ignore_ascii_case(name))
        })
        .map_or_else(
            || FieldEdit::assignee(name, None),
            |identity| FieldEdit::assignee(&identity.display_name, identity.unique_name.as_deref()),
        ))
}

/// The tag list as `System.Tags` holds it. Tags are typed with commas on the
/// command line, because a semicolon is the shell's business, and stored the
/// way the tags prompt stores them.
fn tag_list(raw: &str) -> String {
    normalize_tags(&raw.replace(',', ";"))
}

/// A description written as Markdown, read back as the HTML Azure DevOps
/// stores — the same conversion the Actions menu's description editor makes.
fn description_html(path: &Path) -> Result<String> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read the description from {}", path.display()))?;
    Ok(markdown::markdown_to_html(&markdown::saved_markdown(&raw)))
}

/// The most one comment may carry. Past this it is a log, and the part worth
/// reading is at one end of it rather than spread over the whole.
const COMMENT_LIMIT: i64 = 64 * 1024;

/// The body one comment carries, and where it came from.
///
/// A body piped in is program output — a test tail, a log — so it is posted as
/// a fenced code block, which is what keeps its columns lined up in the portal
/// and in the TUI's own comment view. A body typed as an argument is a
/// sentence and stays plain text, as it always has.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CommentBody {
    text: String,
    fenced: bool,
}

impl CommentBody {
    /// The rich text a work-item comment is stored as: a `<pre>` block for
    /// piped output, the paragraph the browser writes for anything typed.
    fn html(&self) -> String {
        if self.fenced {
            markdown::markdown_to_html(&self.markdown())
        } else {
            azure::comment_html(&self.text)
        }
    }

    /// The Markdown a pull-request thread takes — that API stores Markdown
    /// rather than HTML, so the fence goes out as a fence.
    fn markdown(&self) -> String {
        if self.fenced {
            // A fence one backtick longer than any run inside the body, so
            // a log that quotes a code block cannot close it early.
            let longest = self
                .text
                .split(|held| held != '`')
                .map(str::len)
                .max()
                .unwrap_or(0);
            let fence = "`".repeat(longest.max(2) + 1);
            format!("{fence}\n{}\n{fence}", self.text)
        } else {
            self.text.clone()
        }
    }
}

/// What one `comment` posts. The argument as typed, or standard input when the
/// argument is `-` — and when there is no argument at all but something is
/// piped in, which is the same thing said shorter. A terminal on the other end
/// of standard input means nobody piped anything, so that is a usage error
/// naming both forms rather than a wait for input nobody will type.
fn comment_body(
    argument: Option<&str>,
    stdin: &mut impl Read,
    stdin_is_tty: bool,
) -> Result<CommentBody> {
    match argument {
        Some("-") => piped_comment(stdin),
        Some(text) => {
            if text.trim().is_empty() {
                bail!("a comment cannot be empty");
            }
            Ok(CommentBody {
                text: text.to_owned(),
                fenced: false,
            })
        }
        None if stdin_is_tty => bail!(
            "a comment needs a body: `comment ID \"text\"`, or `... | comment ID -` to read it from standard input"
        ),
        None => piped_comment(stdin),
    }
}

/// Standard input, read to end of file. The trailing newline a pipe always
/// carries is taken off; the tabs a test runner lines its output up with are
/// left exactly as they came.
fn piped_comment(stdin: &mut impl Read) -> Result<CommentBody> {
    let mut raw = String::new();
    // Read one byte past the limit and no further: a log of any size is
    // refused without first being held in memory.
    stdin
        .take(u64::try_from(COMMENT_LIMIT).unwrap_or(u64::MAX) + 1)
        .read_to_string(&mut raw)
        .context("failed to read the comment from standard input")?;
    if i64::try_from(raw.len()).unwrap_or(i64::MAX) > COMMENT_LIMIT {
        bail!(
            "that is a log, not a comment: more than {}. Pipe it through tail",
            size_label(COMMENT_LIMIT)
        );
    }
    let text = raw.trim_end().to_owned();
    if text.trim().is_empty() {
        bail!("a comment cannot be empty");
    }
    Ok(CommentBody { text, fenced: true })
}

fn run_comment(cli: &Cli, database: &Path, id: i64, text: Option<&str>) -> Result<()> {
    let stdin = std::io::stdin();
    let body = comment_body(text, &mut stdin.lock(), stdin.is_terminal())?;
    let mut repository = open_database(database)?;
    let client = connect(cli)?;
    let key = key_for(&client, id);
    let comment = post_comment(&client, &mut repository, &key, &body)?;
    emit(&format!(
        "#{} comment {} posted",
        comment.ticket.id, comment.comment_id
    ));
    Ok(())
}

/// Posts one comment and stores it. Nothing is written locally unless the post
/// landed, and the row lands on the work item the request named whatever the
/// answer says it is about.
fn post_comment(
    source: &dyn WorkItemSource,
    repository: &mut SqliteTicketRepository,
    key: &TicketKey,
    body: &CommentBody,
) -> Result<CommentRecord> {
    let posted = source.post_comment(key.id, &body.html())?;
    let comment = CommentRecord {
        ticket: key.clone(),
        ..posted
    };
    repository.insert_comment(&comment)?;
    Ok(comment)
}

fn run_create(cli: &Cli, database: &Path, args: &CreateArgs) -> Result<()> {
    let mut repository = open_database(database)?;
    let me = resolve_me(
        repository.meta(db::ME_DISPLAY_NAME_KEY)?,
        std::env::var("TICKET_TUI_ME").ok(),
    );
    let edits = create_edits(args, &repository.load_identities()?, me.as_deref())?;
    let client = connect(cli)?;
    let ticket = create_work_item(&client, &mut repository, args, &edits)?;
    emit(&format!(
        "#{} rev {}: {} {}",
        ticket.key.id, ticket.revision, ticket.work_item_type, ticket.title
    ));
    Ok(())
}

/// Adds one work item and stores the copy Azure DevOps answered with, links
/// and all, so the parent it was created under is in the graph the TUI draws.
fn create_work_item(
    source: &dyn WorkItemSource,
    repository: &mut SqliteTicketRepository,
    args: &CreateArgs,
    edits: &[FieldEdit],
) -> Result<Ticket> {
    let (ticket, relations, artifacts) =
        source.create_work_item(args.work_item_type.trim(), &patch_ops(edits), args.parent)?;
    repository.upsert(&ticket, &relations, &artifacts)?;
    Ok(ticket)
}

/// Opens Azure DevOps for a subcommand that writes. An unresolved organization
/// is a hard error here: a write has nowhere else to go.
fn connect(cli: &Cli) -> Result<AzureClient> {
    AzureClient::connect(AzureConfig::resolve(
        cli.org.clone(),
        cli.project.clone(),
        cli.code_project.clone(),
        cli.query.clone(),
        cli.team.clone(),
    )?)
}

fn key_for(client: &AzureClient, id: i64) -> TicketKey {
    TicketKey {
        organization: client.config().organization.clone(),
        id,
    }
}

/// The database every subcommand but `sync` works against. It is opened
/// without touching its schema: a one-shot read or a single-field write has no
/// business rebuilding — and so emptying — a file a running TUI owns. A file
/// that is not there is said so plainly rather than created empty and reported
/// as a project with no work in it.
fn open_database(database: &Path) -> Result<SqliteTicketRepository> {
    if !database.exists() {
        bail!(
            "no database at {}; run `ticket-tui sync` to pull one",
            database.display()
        );
    }
    SqliteTicketRepository::open_existing(database)
}

/// One work item as `--json` spells it: the fields a caller acts on, under the
/// names the filter grammar uses rather than the Azure DevOps reference names.
#[derive(Debug, Serialize)]
struct TicketJson<'a> {
    id: i64,
    organization: &'a str,
    project: &'a str,
    rev: i64,
    #[serde(rename = "type")]
    work_item_type: &'a str,
    title: &'a str,
    state: &'a str,
    assignee: Option<&'a str>,
    priority: Option<i64>,
    area: &'a str,
    iteration: &'a str,
    tags: &'a [String],
    created: String,
    changed: String,
    url: &'a str,
    /// Only `show` carries the body: a list of five hundred rows is no place
    /// for five hundred descriptions.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
}

impl<'a> TicketJson<'a> {
    fn row(ticket: &'a Ticket) -> Self {
        Self {
            id: ticket.key.id,
            organization: &ticket.key.organization,
            project: &ticket.project,
            rev: ticket.revision,
            work_item_type: &ticket.work_item_type,
            title: &ticket.title,
            state: &ticket.state,
            assignee: ticket.assigned_to.as_deref(),
            priority: ticket.priority,
            area: &ticket.area_path,
            iteration: &ticket.iteration_path,
            tags: &ticket.tags,
            created: ticket.created_at.to_rfc3339(),
            changed: ticket.changed_at.to_rfc3339(),
            url: &ticket.web_url,
            description: None,
        }
    }

    fn detailed(ticket: &'a Ticket) -> Self {
        Self {
            description: Some(&ticket.description),
            ..Self::row(ticket)
        }
    }
}

fn to_json(value: &impl Serialize) -> Result<String> {
    serde_json::to_string_pretty(value).context("failed to write the work items as JSON")
}

/// One work item as a block of text: what it is on the first line, what it
/// says on the second, its planning fields under that, and its description
/// last.
fn describe(ticket: &Ticket) -> String {
    let mut lines = vec![
        format!(
            "#{} {} · {} · rev {}",
            ticket.key.id, ticket.work_item_type, ticket.state, ticket.revision
        ),
        ticket.title.clone(),
        String::new(),
    ];
    for (label, value) in [
        ("Assignee", ticket.assigned_to.clone().unwrap_or_default()),
        (
            "Priority",
            ticket.priority.map(|p| p.to_string()).unwrap_or_default(),
        ),
        ("Area", ticket.area_path.clone()),
        ("Iteration", ticket.iteration_path.clone()),
        ("Tags", ticket.tags.join("; ")),
        ("Created", ticket.created_at.to_rfc3339()),
        ("Changed", ticket.changed_at.to_rfc3339()),
        ("URL", ticket.web_url.clone()),
    ] {
        if !value.is_empty() {
            lines.push(format!("{label:<10}{value}"));
        }
    }
    if !ticket.description.trim().is_empty() {
        lines.push(String::new());
        lines.push(ticket.description.trim_end().to_owned());
    }
    lines.join("\n")
}

/// The rows as a table: id, state, type, assignee, title, each column as wide
/// as its widest value. An empty result is a line saying so rather than
/// nothing at all, which reads as a run that failed.
fn tabulate(tickets: &[Ticket]) -> String {
    if tickets.is_empty() {
        return "no matching work items".to_owned();
    }
    let cells: Vec<[String; 5]> = tickets
        .iter()
        .map(|ticket| {
            [
                format!("#{}", ticket.key.id),
                ticket.state.clone(),
                ticket.work_item_type.clone(),
                ticket.assigned_to.clone().unwrap_or_else(|| "—".into()),
                ticket.title.clone(),
            ]
        })
        .collect();
    let mut widths = [0usize; 5];
    for row in &cells {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    cells
        .iter()
        .map(|row| {
            let mut line = String::new();
            for (index, cell) in row.iter().enumerate() {
                // The last column is the title, which is not padded: trailing
                // space on every line is noise in a diff and in a pipe.
                if index + 1 == row.len() {
                    line.push_str(cell);
                } else {
                    let padding = widths[index] - cell.chars().count();
                    line.push_str(cell);
                    line.push_str(&" ".repeat(padding + 2));
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Says one thing on standard output. A closed pipe — `ticket-tui list | head`
/// — is not an error worth reporting: there is nobody left to report it to.
/// One repository as `--json` prints it.
#[derive(Serialize)]
struct RepoJson<'a> {
    id: &'a str,
    name: &'a str,
    project: &'a str,
    default_branch: String,
    is_disabled: bool,
    pull_requests: usize,
    pipelines: usize,
    web_url: &'a str,
    remote_url: &'a str,
    ssh_url: &'a str,
    local: Option<LocalJson<'a>>,
}

#[derive(Serialize)]
struct LocalJson<'a> {
    path: String,
    origin: &'a str,
    branch: &'a str,
    dirty: bool,
    ahead: u32,
    behind: u32,
}

impl<'a> From<&'a RepoRow> for RepoJson<'a> {
    fn from(row: &'a RepoRow) -> Self {
        Self {
            id: &row.repo.id,
            name: &row.repo.name,
            project: &row.repo.project,
            default_branch: row.branch(),
            is_disabled: row.repo.is_disabled,
            pull_requests: row.pull_requests,
            pipelines: row.pipelines,
            web_url: &row.repo.web_url,
            remote_url: &row.repo.remote_url,
            ssh_url: &row.repo.ssh_url,
            local: row.local.as_ref().map(|local| LocalJson {
                path: local.path.display().to_string(),
                origin: &local.origin,
                branch: &local.branch,
                dirty: local.dirty,
                ahead: local.ahead,
                behind: local.behind,
            }),
        }
    }
}

/// One pull request as `--json` prints it. `list` prints the row; `show` adds
/// the reviewers, the work items and the discussion, the way the work-item
/// commands keep descriptions out of a list of five hundred.
#[derive(Serialize)]
struct PrJson<'a> {
    id: i64,
    repo: &'a str,
    title: &'a str,
    author: &'a str,
    status: &'a str,
    is_draft: bool,
    source: String,
    target: String,
    merge_status: &'a str,
    auto_complete: bool,
    created: Option<String>,
    closed: Option<String>,
    url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<PrBuildJson<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reviewers: Option<Vec<ReviewerJson<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    work_items: Option<&'a [i64]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    threads: Option<Vec<ThreadJson<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
}

#[derive(Serialize)]
struct PrBuildJson<'a> {
    status: &'a str,
    run_id: Option<i64>,
}

#[derive(Serialize)]
struct ReviewerJson<'a> {
    name: &'a str,
    vote: i8,
    is_required: bool,
}

#[derive(Serialize)]
struct ThreadJson<'a> {
    id: i64,
    author: &'a str,
    text: &'a str,
    status: &'a str,
    published: Option<String>,
}

impl<'a> PrJson<'a> {
    fn row(row: &'a PrRow) -> Self {
        let request = &row.request;
        Self {
            id: request.id,
            repo: &row.repo,
            title: &request.title,
            author: &request.created_by.display_name,
            status: request.status.as_str(),
            is_draft: request.is_draft,
            source: row.source_branch(),
            target: row.target_branch(),
            merge_status: &request.merge_status,
            auto_complete: request.auto_complete_set_by.is_some(),
            created: request.created_at.map(|at| at.to_rfc3339()),
            closed: request.closed_at.map(|at| at.to_rfc3339()),
            url: &request.url,
            build: request.build.as_ref().map(|build| PrBuildJson {
                status: &build.status,
                run_id: build.run_id,
            }),
            reviewers: None,
            work_items: None,
            threads: None,
            description: None,
        }
    }

    fn full(row: &'a PrRow) -> Self {
        let request = &row.request;
        Self {
            reviewers: Some(
                request
                    .reviewers
                    .iter()
                    .map(|reviewer| ReviewerJson {
                        name: &reviewer.display_name,
                        vote: reviewer.vote,
                        is_required: reviewer.is_required,
                    })
                    .collect(),
            ),
            work_items: Some(&request.work_items),
            threads: Some(
                request
                    .threads
                    .iter()
                    .map(|thread| ThreadJson {
                        id: thread.id,
                        author: &thread.author,
                        text: &thread.text,
                        status: &thread.status,
                        published: thread.published_at.map(|at| at.to_rfc3339()),
                    })
                    .collect(),
            ),
            description: Some(&request.description),
            ..Self::row(row)
        }
    }
}

/// The Repos tab's two reads. The workspace is read for both, because which
/// repositories are on this machine is the question the tab exists to answer
/// and a `git status` costs no network.
fn run_repos(cli: &Cli, database: &Path, command: &ReposCommand) -> Result<()> {
    let repository = open_database(database)?;
    let repos = repository.load_repos()?;
    let requests = repository.load_pull_requests()?;
    let pipelines = repository.load_pipelines()?;
    let local = local::workspace_root(cli.workspace.clone())
        .map(|workspace| {
            local::scan(
                &workspace,
                &repos
                    .iter()
                    .map(|repo| local::RepoKey {
                        id: repo.id.clone(),
                        remote: local::normalise_remote(&repo.remote_url),
                        name: repo.name.clone(),
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default();
    let rows: Vec<RepoRow> = repos
        .iter()
        .map(|repo| RepoRow {
            local: local
                .iter()
                .find(|(id, _)| *id == repo.id)
                .map(|(_, found)| found.clone()),
            pull_requests: requests
                .iter()
                .filter(|request| request.repo_id == repo.id && !request.status.is_closed())
                .count(),
            pipelines: pipelines
                .iter()
                .filter(|pipeline| pipeline.repo_id.as_deref() == Some(repo.id.as_str()))
                .count(),
            repo: repo.clone(),
        })
        .collect();
    match command {
        ReposCommand::List { query, json } => {
            let rows = filter_repos(rows, query.as_deref());
            emit(&if *json {
                to_json(&rows.iter().map(RepoJson::from).collect::<Vec<_>>())?
            } else {
                tabulate_repos(&rows)
            });
        }
        ReposCommand::Show { name, json } => {
            let row = rows
                .into_iter()
                .find(|row| same_text(&row.repo.name, name))
                .with_context(|| format!("no repository called {name} is in the database"))?;
            emit(&if *json {
                to_json(&RepoJson::from(&row))?
            } else {
                describe_repo(&row)
            });
        }
    }
    Ok(())
}

/// The rows one `--query` names, in the order the tab lists them.
fn filter_repos(rows: Vec<RepoRow>, query: Option<&str>) -> Vec<RepoRow> {
    let Some(query) = query else {
        return rows;
    };
    let parsed = parse_query::<RepoSchema>(query);
    let context = MatchContext::now();
    rows.into_iter()
        .filter(|row| {
            parsed.filters.matches_in(row, false, &context) && row.matches_fuzzy(&parsed.fuzzy)
        })
        .collect()
}

fn tabulate_repos(rows: &[RepoRow]) -> String {
    if rows.is_empty() {
        return "no matching repositories".to_owned();
    }
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            vec![
                row.repo.name.clone(),
                row.branch(),
                row.pull_requests.to_string(),
                row.pipelines.to_string(),
                local_words(row),
            ]
        })
        .collect();
    columns(&cells)
}

/// The Local column, in words rather than glyphs: a pipe reads better than a
/// terminal here.
fn local_words(row: &RepoRow) -> String {
    let Some(local) = row.local.as_ref() else {
        return "—".to_owned();
    };
    let mut state = local.branch.clone();
    if local.dirty {
        state.push_str(" dirty");
    }
    if local.ahead > 0 {
        state.push_str(&format!(" +{}", local.ahead));
    }
    if local.behind > 0 {
        state.push_str(&format!(" -{}", local.behind));
    }
    state
}

fn describe_repo(row: &RepoRow) -> String {
    let mut lines = vec![
        format!("{} ({})", row.repo.name, row.repo.project),
        String::new(),
        format!("Default branch  {}", row.branch()),
        format!("Pull requests   {}", row.pull_requests),
        format!("Pipelines       {}", row.pipelines),
        format!("Web             {}", row.repo.web_url),
        format!("HTTPS           {}", row.repo.remote_url),
        format!("SSH             {}", row.repo.ssh_url),
    ];
    if row.repo.is_disabled {
        lines.push("Disabled        yes".to_owned());
    }
    lines.push(String::new());
    match row.local.as_ref() {
        Some(local) => {
            lines.push(format!("Local           {}", local.path.display()));
            lines.push(format!("                {}", local_words(row)));
            lines.push(format!("Origin          {}", local.origin));
        }
        None => lines.push("Local           not on this machine".to_owned()),
    }
    lines.join("\n")
}

/// The Pull requests tab, without the tab: two reads and five writes.
fn run_prs(cli: &Cli, database: &Path, command: &PrsCommand) -> Result<()> {
    match command {
        PrsCommand::List { query, json } => {
            let repository = open_database(database)?;
            let rows = pr_rows(&repository)?;
            let me = resolve_me(
                repository.meta(db::ME_DISPLAY_NAME_KEY)?,
                std::env::var("TICKET_TUI_ME").ok(),
            );
            let rows = filter_pull_requests(rows, query.as_deref(), me)?;
            emit(&if *json {
                to_json(&rows.iter().map(PrJson::row).collect::<Vec<_>>())?
            } else {
                tabulate_pull_requests(&rows)
            });
            Ok(())
        }
        PrsCommand::Show { id, json } => {
            let repository = open_database(database)?;
            let row = find_pull_request(&repository, *id)?;
            emit(&if *json {
                to_json(&PrJson::full(&row))?
            } else {
                describe_pull_request(&row)
            });
            Ok(())
        }
        PrsCommand::Vote { id, vote } => run_pr_vote(cli, database, *id, vote),
        PrsCommand::Complete {
            id,
            strategy,
            keep_source,
            no_transition,
        } => {
            let strategy = merge_strategy(strategy.as_deref())?;
            let repository = open_database(database)?;
            let row = find_pull_request(&repository, *id)?;
            let options = CompletionOptions {
                strategy,
                delete_source: !keep_source,
                transition_work_items: !no_transition,
                // The head the stored copy was read at: a source branch that
                // has moved since is a merge Azure DevOps should refuse.
                last_merge_source_commit: row.request.last_merge_source_commit.clone(),
            };
            run_pr_action(cli, database, *id, PrAction::Complete(options), "completed")
        }
        PrsCommand::Abandon { id } => {
            run_pr_action(cli, database, *id, PrAction::Abandon, "abandoned")
        }
        PrsCommand::Autocomplete { id, state } => {
            let on = match state.to_ascii_lowercase().as_str() {
                "on" | "yes" | "true" => true,
                "off" | "no" | "false" => false,
                other => bail!("auto-complete is `on` or `off`, not {other}"),
            };
            run_pr_action(
                cli,
                database,
                *id,
                PrAction::AutoComplete(on),
                if on {
                    "set to complete automatically"
                } else {
                    "no longer completing automatically"
                },
            )
        }
        PrsCommand::Comment { id, text } => run_pr_comment(cli, database, *id, text.as_deref()),
    }
}

/// Every stored pull request, with the repository name the table shows.
fn pr_rows(repository: &SqliteTicketRepository) -> Result<Vec<PrRow>> {
    let repos = repository.load_repos()?;
    Ok(repository
        .load_pull_requests()?
        .into_iter()
        .map(|request| PrRow {
            repo: repos
                .iter()
                .find(|repo| repo.id == request.repo_id)
                .map_or_else(|| request.repo_id.clone(), |repo| repo.name.clone()),
            request,
        })
        .collect())
}

fn find_pull_request(repository: &SqliteTicketRepository, id: i64) -> Result<PrRow> {
    pr_rows(repository)?
        .into_iter()
        .find(|row| row.request.id == id)
        .with_context(|| format!("no pull request !{id} is in the database"))
}

fn filter_pull_requests(
    rows: Vec<PrRow>,
    query: Option<&str>,
    me: Option<String>,
) -> Result<Vec<PrRow>> {
    let Some(query) = query else {
        return Ok(rows);
    };
    let parsed = parse_query::<PrSchema>(query);
    let context = MatchContext::now().with_me(me);
    if context.me.is_none() && query.contains("@me") {
        bail!(
            "`@me` cannot be resolved: no signed-in user is stored, so set TICKET_TUI_ME or sync"
        );
    }
    Ok(rows
        .into_iter()
        .filter(|row| {
            parsed.filters.matches_in(row, false, &context) && row.matches_fuzzy(&parsed.fuzzy)
        })
        .collect())
}

fn tabulate_pull_requests(rows: &[PrRow]) -> String {
    if rows.is_empty() {
        return "no matching pull requests".to_owned();
    }
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            vec![
                format!("!{}", row.request.id),
                row.repo.clone(),
                row.request.created_by.display_name.clone(),
                vote_summary(row),
                if row.build_word().is_empty() {
                    "—".to_owned()
                } else {
                    row.build_word()
                },
                row.request.title.clone(),
            ]
        })
        .collect();
    columns(&cells)
}

/// The votes as a count rather than a run of glyphs: `2/3` is two of the three
/// reviewers approving, and `0/0` is nobody asked.
fn vote_summary(row: &PrRow) -> String {
    let approved = row
        .request
        .reviewers
        .iter()
        .filter(|reviewer| reviewer.vote > 0)
        .count();
    format!("{approved}/{}", row.request.reviewers.len())
}

fn describe_pull_request(row: &PrRow) -> String {
    let request = &row.request;
    let mut lines = vec![
        format!(
            "!{} {} · {} · {}",
            request.id,
            request.status.as_str(),
            if request.is_draft { "draft" } else { "ready" },
            row.repo
        ),
        request.title.clone(),
        String::new(),
        format!("Author          {}", request.created_by.display_name),
        format!("Branches        {}", row.branches()),
        format!("Merge           {}", request.merge_status),
        format!("URL             {}", request.url),
    ];
    if let Some(build) = request.build.as_ref() {
        lines.push(format!(
            "Build           {}{}",
            build.status,
            build
                .run_id
                .map(|id| format!(" (run {id})"))
                .unwrap_or_default()
        ));
    }
    if request.auto_complete_set_by.is_some() {
        lines.push("Auto-complete   on".to_owned());
    }
    lines.push(String::new());
    lines.push("Reviewers".to_owned());
    if request.reviewers.is_empty() {
        lines.push("  nobody asked".to_owned());
    }
    for reviewer in &request.reviewers {
        lines.push(format!(
            "  {} {}{}",
            reviewer.glyph(),
            reviewer.display_name,
            if reviewer.is_required {
                " (required)"
            } else {
                ""
            }
        ));
    }
    if !request.work_items.is_empty() {
        lines.push(String::new());
        lines.push("Work items".to_owned());
        for id in &request.work_items {
            lines.push(format!("  #{id}"));
        }
    }
    if !request.threads.is_empty() {
        lines.push(String::new());
        lines.push("Discussion".to_owned());
        for thread in &request.threads {
            lines.push(format!("  {} · {}", thread.author, thread.text));
        }
    }
    lines.join("\n")
}

/// `approve`, `suggest`, `wait`, `reject`, `none`, on the API's own scale.
fn parse_vote(word: &str) -> Result<i8> {
    Ok(match word.to_ascii_lowercase().as_str() {
        "approve" | "approved" => 10,
        "suggest" | "suggestions" => 5,
        "wait" | "waiting" => -5,
        "reject" | "rejected" => -10,
        "none" | "reset" | "clear" => 0,
        other => bail!("a vote is approve, suggest, wait, reject or none, not {other}"),
    })
}

fn merge_strategy(word: Option<&str>) -> Result<MergeStrategy> {
    Ok(
        match word.unwrap_or("squash").to_ascii_lowercase().as_str() {
            "squash" => MergeStrategy::Squash,
            "merge" | "no-fast-forward" => MergeStrategy::Merge,
            "rebase" => MergeStrategy::Rebase,
            other => bail!("a strategy is squash, merge or rebase, not {other}"),
        },
    )
}

/// One vote, written as whoever is signed in. Their own id is read once and
/// kept in `sync_meta`, the same key the TUI's worker fills.
fn run_pr_vote(cli: &Cli, database: &Path, id: i64, word: &str) -> Result<()> {
    let vote = parse_vote(word)?;
    let mut repository = open_database(database)?;
    let row = find_pull_request(&repository, id)?;
    let client = connect(cli)?;
    let reviewer = match repository.meta(db::ME_ID_KEY)? {
        Some(reviewer) => reviewer,
        None => {
            let reviewer = client
                .my_id()?
                .context("Azure DevOps did not say who is signed in")?;
            repository.set_meta(db::ME_ID_KEY, &reviewer)?;
            reviewer
        }
    };
    client.vote_pull_request(&row.request.repo_id, id, &reviewer, vote)?;
    // A vote is not answered with the pull request, so the stored copy is
    // amended in place: the next pull brings back whatever else moved.
    let mut request = row.request;
    if let Some(existing) = request
        .reviewers
        .iter_mut()
        .find(|held| held.id == reviewer)
    {
        existing.vote = vote;
    } else {
        // The endpoint adds a voter who was not a reviewer, so the stored
        // copy does too, under the name the sync recorded for them.
        request.reviewers.push(crate::model::PrReviewer {
            id: reviewer,
            display_name: repository
                .meta(db::ME_DISPLAY_NAME_KEY)?
                .unwrap_or_else(|| "me".to_owned()),
            unique_name: None,
            vote,
            is_required: false,
        });
    }
    store_pull_request(&mut repository, request)?;
    emit(&format!("!{id} vote: {word}"));
    Ok(())
}

/// Complete, abandon, or set auto-complete, storing the copy Azure DevOps
/// answers with so a running TUI sees it on its next reload.
fn run_pr_action(cli: &Cli, database: &Path, id: i64, action: PrAction, said: &str) -> Result<()> {
    let mut repository = open_database(database)?;
    let row = find_pull_request(&repository, id)?;
    let client = connect(cli)?;
    let me = match action {
        PrAction::AutoComplete(true) => Some(match repository.meta(db::ME_ID_KEY)? {
            Some(reviewer) => reviewer,
            None => {
                let reviewer = client
                    .my_id()?
                    .context("Azure DevOps did not say who is signed in")?;
                repository.set_meta(db::ME_ID_KEY, &reviewer)?;
                reviewer
            }
        }),
        _ => None,
    };
    let updated = client.pull_request_action(&row.request.repo_id, id, action, me.as_deref())?;
    store_pull_request(&mut repository, updated)?;
    emit(&format!("!{id} {said}"));
    Ok(())
}

/// One comment, as a thread of its own. It is stored with the pull request,
/// so the TUI's discussion shows it without waiting for a pull.
fn run_pr_comment(cli: &Cli, database: &Path, id: i64, text: Option<&str>) -> Result<()> {
    let stdin = std::io::stdin();
    let body = comment_body(text, &mut stdin.lock(), stdin.is_terminal())?;
    let mut repository = open_database(database)?;
    let row = find_pull_request(&repository, id)?;
    let client = connect(cli)?;
    let thread = client.comment_on_pull_request(&row.request.repo_id, id, &body.markdown())?;
    let mut request = row.request;
    request.threads.push(thread);
    store_pull_request(&mut repository, request)?;
    emit(&format!("!{id} comment posted"));
    Ok(())
}

/// Puts one pull request back among the stored ones, leaving the rest as they
/// are. The threads a read never brought down are kept.
fn store_pull_request(
    repository: &mut SqliteTicketRepository,
    mut updated: PullRequest,
) -> Result<()> {
    let mut stored = repository.load_pull_requests()?;
    if let Some(existing) = stored.iter().find(|held| held.id == updated.id)
        && updated.threads.is_empty()
    {
        updated.threads = existing.threads.clone();
    }
    stored.retain(|held| held.id != updated.id);
    stored.push(updated);
    repository.replace_pull_requests(&stored)?;
    Ok(())
}

/// The project's build definitions, from the database.
fn run_pipelines(database: &Path, json: bool) -> Result<()> {
    let repository = open_database(database)?;
    let pipelines = repository.load_pipelines()?;
    let runs = repository.load_runs()?;
    emit(&if json {
        to_json(
            &pipelines
                .iter()
                .map(|pipeline| PipelineJson::new(pipeline, &runs))
                .collect::<Vec<_>>(),
        )?
    } else if pipelines.is_empty() {
        "no pipelines".to_owned()
    } else {
        columns(
            &pipelines
                .iter()
                .map(|pipeline| {
                    let last = runs.iter().find(|run| run.pipeline_id == pipeline.id);
                    vec![
                        pipeline.id.to_string(),
                        pipeline.folder.clone(),
                        last.map_or_else(|| "\u{2014}".to_owned(), run_word),
                        pipeline.name.clone(),
                    ]
                })
                .collect::<Vec<_>>(),
        )
    });
    Ok(())
}

/// How a run turned out, in the words `result:` filters on.
fn run_word(run: &Run) -> String {
    run.result.map_or_else(
        || run.status.as_str().to_owned(),
        |result| result.as_str().to_owned(),
    )
}

/// The Pipelines tab's runs. `list` reads the database; everything else talks
/// to Azure DevOps, because a timeline, a log and a run's own progress are not
/// things a pull stores.
fn run_runs(cli: &Cli, database: &Path, command: &RunsCommand) -> Result<()> {
    match command {
        RunsCommand::List {
            pipeline,
            query,
            json,
        } => {
            let repository = open_database(database)?;
            let rows = run_rows(&repository)?;
            let me = resolve_me(
                repository.meta(db::ME_DISPLAY_NAME_KEY)?,
                std::env::var("TICKET_TUI_ME").ok(),
            );
            let rows = filter_runs(rows, pipeline.as_deref(), query.as_deref(), me);
            emit(&if *json {
                to_json(&rows.iter().map(RunJson::from).collect::<Vec<_>>())?
            } else {
                tabulate_runs(&rows)
            });
            Ok(())
        }
        RunsCommand::Show { id, json } => {
            let client = connect(cli)?;
            let run = client
                .fetch_run(*id)?
                .with_context(|| format!("Azure DevOps has no run {id}"))?;
            let timeline = client.fetch_timeline(*id).unwrap_or_default();
            let pipeline = pipeline_name(database, run.pipeline_id);
            emit(&if *json {
                to_json(&RunShowJson::new(&run, &pipeline, &timeline))?
            } else {
                describe_run(&run, &pipeline, &timeline)
            });
            Ok(())
        }
        RunsCommand::Logs {
            id,
            job,
            task,
            follow,
        } => {
            let client = connect(cli)?;
            print_log(
                &client,
                *id,
                job.as_deref().or(task.as_deref()),
                *follow,
                &sleep,
            )
        }
        RunsCommand::Trigger {
            pipeline,
            branch,
            follow,
        } => {
            let repository = open_database(database)?;
            let definition = repository
                .load_pipelines()?
                .into_iter()
                .find(|held| same_text(&held.name, pipeline))
                .with_context(|| format!("no pipeline called {pipeline} is in the database"))?;
            let client = connect(cli)?;
            let branch = if branch.starts_with("refs/") {
                branch.clone()
            } else {
                format!("refs/heads/{branch}")
            };
            let run = client.start_run(definition.id, &branch)?;
            emit(&format!(
                "run {} queued: {} on {}",
                run.id,
                definition.name,
                short_branch(&branch)
            ));
            if *follow {
                print_log(&client, run.id, None, true, &sleep)?;
                report_run(&wait_for_run(&client, run.id, &sleep)?);
            }
            Ok(())
        }
        RunsCommand::Cancel { id } => {
            let run = connect(cli)?.patch_run(*id, false)?;
            emit(&format!("run {} {}", run.id, run.status.as_str()));
            Ok(())
        }
        RunsCommand::Retry { id } => {
            let run = connect(cli)?.patch_run(*id, true)?;
            emit(&format!("run {} retried: {}", run.id, run.status.as_str()));
            Ok(())
        }
        RunsCommand::Wait { id } => report_run(&wait_for_run(&connect(cli)?, *id, &sleep)?),
    }
}

/// What a blocking command does between polls. A test hands in a sleep that
/// does not sleep.
fn sleep(wait: Duration) {
    std::thread::sleep(wait);
}

/// Says how the run went and exits with the code that says it again: 0
/// succeeded, 1 failed, 2 canceled, 3 partially succeeded, so a script can
/// branch on it without parsing anything.
fn report_run(run: &Run) -> ! {
    emit(&format!(
        "run {} {} · {}",
        run.id,
        run_word(run),
        run.build_number
    ));
    std::process::exit(match run.result {
        Some(RunResult::Succeeded) => 0,
        Some(RunResult::PartiallySucceeded) => 3,
        Some(RunResult::Canceled) => 2,
        _ => 1,
    })
}

/// Polls one run until it stops, at the watcher's own live cadence.
fn wait_for_run(source: &dyn PipelineSource, id: i64, rest: &dyn Fn(Duration)) -> Result<Run> {
    loop {
        let run = source
            .run(id)?
            .with_context(|| format!("Azure DevOps has no run {id}"))?;
        if !run.status.is_live() {
            return Ok(run);
        }
        rest(source.throttled_for().unwrap_or(LIVE_RUNS_CADENCE));
    }
}

/// Prints one node's log, and keeps printing while `follow` and the node is
/// still writing. With no node named it takes the deepest one running, which
/// is what the tab's own log pane shows — and, while following, moves on to
/// the next node as each finishes, so `--follow` reads the whole run rather
/// than its first task. A run that has not written anything yet is waited
/// for rather than refused: one just queued has no timeline at all.
fn print_log(
    source: &dyn PipelineSource,
    run_id: i64,
    node: Option<&str>,
    follow: bool,
    rest: &dyn Fn(Duration),
) -> Result<()> {
    let mut from_line = 0;
    // The log being printed, so a move to the next node starts it from the top
    // and says whose it is.
    let mut printing: Option<i64> = None;
    loop {
        let timeline = source.timeline(run_id)?;
        let record = match node {
            Some(name) => timeline
                .iter()
                .find(|record| same_text(&record.name, name))
                .with_context(|| format!("run {run_id} has no node called {name}"))?,
            None => {
                let chosen = timeline
                    .iter()
                    .rfind(|record| record.log_id.is_some() && record.state.is_live())
                    .or_else(|| timeline.iter().rfind(|record| record.log_id.is_some()));
                match chosen {
                    Some(record) => record,
                    None if follow && timeline.iter().all(|record| record.state.is_live())
                        || (follow && timeline.is_empty()) =>
                    {
                        rest(LOG_CADENCE);
                        continue;
                    }
                    None => bail!("run {run_id} has written no log yet"),
                }
            }
        };
        let Some(log_id) = record.log_id else {
            if !follow {
                bail!("{} has written no log", record.name);
            }
            rest(LOG_CADENCE);
            continue;
        };
        if printing != Some(log_id) {
            if node.is_none() && printing.is_some() {
                emit(&format!("--- {}", record.name));
            }
            printing = Some(log_id);
            from_line = 0;
        }
        for line in source.log_lines(run_id, log_id, from_line)? {
            emit(&line);
            from_line += 1;
        }
        if !follow {
            return Ok(());
        }
        if !record.state.is_live() {
            // A named node is done when it is done; an unnamed one hands over
            // to whatever is running next, until nothing is.
            let more_to_come = node.is_none()
                && timeline
                    .iter()
                    .any(|record| record.state.is_live() && record.log_id.is_some());
            if !more_to_come {
                return Ok(());
            }
        }
        rest(source.throttled_for().unwrap_or(LOG_CADENCE));
    }
}

/// Every stored run, newest first, with the pipeline name the table shows.
fn run_rows(repository: &SqliteTicketRepository) -> Result<Vec<RunRow>> {
    let pipelines = repository.load_pipelines()?;
    Ok(repository
        .load_runs()?
        .into_iter()
        .map(|run| RunRow {
            pipeline: pipelines
                .iter()
                .find(|pipeline| pipeline.id == run.pipeline_id)
                .map_or_else(
                    || run.pipeline_id.to_string(),
                    |pipeline| pipeline.name.clone(),
                ),
            run,
        })
        .collect())
}

fn filter_runs(
    rows: Vec<RunRow>,
    pipeline: Option<&str>,
    query: Option<&str>,
    me: Option<String>,
) -> Vec<RunRow> {
    let context = MatchContext::now().with_me(me);
    let parsed = query.map(parse_query::<RunSchema>);
    rows.into_iter()
        .filter(|row| pipeline.is_none_or(|name| same_text(&row.pipeline, name)))
        .filter(|row| {
            parsed.as_ref().is_none_or(|parsed| {
                parsed.filters.matches_in(row, false, &context) && row.matches_fuzzy(&parsed.fuzzy)
            })
        })
        .collect()
}

fn tabulate_runs(rows: &[RunRow]) -> String {
    if rows.is_empty() {
        return "no matching runs".to_owned();
    }
    columns(
        &rows
            .iter()
            .map(|row| {
                vec![
                    row.run.id.to_string(),
                    row.run.build_number.clone(),
                    run_word(&row.run),
                    row.branch(),
                    row.pipeline.clone(),
                ]
            })
            .collect::<Vec<_>>(),
    )
}

fn pipeline_name(database: &Path, id: i64) -> String {
    open_database(database)
        .and_then(|repository| repository.load_pipelines())
        .ok()
        .and_then(|pipelines| {
            pipelines
                .into_iter()
                .find(|pipeline| pipeline.id == id)
                .map(|pipeline| pipeline.name)
        })
        .unwrap_or_else(|| id.to_string())
}

/// The run's header, then its timeline as the tab draws it: stages, the jobs
/// in them, the tasks in those, each with the glyph its state earns.
fn describe_run(run: &Run, pipeline: &str, timeline: &[TimelineRecord]) -> String {
    let mut lines = vec![
        format!(
            "{} run {} · {} · {}",
            run_glyph(run.status, run.result),
            run.id,
            run_word(run),
            run.build_number
        ),
        format!("{pipeline} on {}", short_branch(&run.source_branch)),
        String::new(),
        format!(
            "Requested by    {}",
            run.requested_for.as_deref().unwrap_or("—")
        ),
        format!("Reason          {}", run.reason),
        format!(
            "Started         {}",
            run.start_time
                .map_or_else(|| "—".to_owned(), |at| at.to_rfc3339())
        ),
        format!(
            "Finished        {}",
            run.finish_time
                .map_or_else(|| "—".to_owned(), |at| at.to_rfc3339())
        ),
        format!("URL             {}", run.url),
    ];
    if !timeline.is_empty() {
        lines.push(String::new());
        lines.push("Timeline".to_owned());
        for record in timeline {
            let depth = match record.kind {
                TimelineKind::Stage => 0,
                TimelineKind::Job | TimelineKind::Checkpoint => 1,
                TimelineKind::Task => 2,
            };
            let seconds = match (record.start, record.finish) {
                (Some(start), Some(finish)) => Some(start.seconds_until(finish).max(0)),
                _ => None,
            };
            lines.push(format!(
                "  {}{} {}{}",
                "  ".repeat(depth),
                run_glyph(record.state, record.result),
                record.name,
                seconds.map_or_else(String::new, |seconds| format!(
                    "  {}",
                    duration_label(seconds)
                ))
            ));
        }
    }
    lines.join("\n")
}

/// The approvals a run is waiting on, and the two answers.
fn run_approvals(cli: &Cli, command: &ApprovalsCommand) -> Result<()> {
    let client = connect(cli)?;
    match command {
        ApprovalsCommand::List { json } => {
            let approvals = client.fetch_approvals()?;
            emit(&if *json {
                to_json(&approvals.iter().map(ApprovalJson::from).collect::<Vec<_>>())?
            } else if approvals.is_empty() {
                "no pending approvals".to_owned()
            } else {
                columns(
                    &approvals
                        .iter()
                        .map(|approval| {
                            vec![
                                approval.id.clone(),
                                approval.build_number.clone(),
                                approval.stage.clone(),
                                approval.pipeline.clone(),
                            ]
                        })
                        .collect::<Vec<_>>(),
                )
            });
            Ok(())
        }
        ApprovalsCommand::Approve { id, comment } => {
            client.answer_approval(id, true, comment.as_deref().unwrap_or_default())?;
            emit(&format!("approval {id} approved"));
            Ok(())
        }
        ApprovalsCommand::Reject { id, comment } => {
            client.answer_approval(id, false, comment.as_deref().unwrap_or_default())?;
            emit(&format!("approval {id} rejected"));
            Ok(())
        }
    }
}

#[derive(Serialize)]
struct PipelineJson<'a> {
    id: i64,
    name: &'a str,
    folder: &'a str,
    repo_id: Option<&'a str>,
    queue_status: &'a str,
    url: &'a str,
    last_run: Option<i64>,
}

impl<'a> PipelineJson<'a> {
    fn new(pipeline: &'a Pipeline, runs: &[Run]) -> Self {
        Self {
            id: pipeline.id,
            name: &pipeline.name,
            folder: &pipeline.folder,
            repo_id: pipeline.repo_id.as_deref(),
            queue_status: &pipeline.queue_status,
            url: &pipeline.url,
            last_run: runs
                .iter()
                .find(|run| run.pipeline_id == pipeline.id)
                .map(|run| run.id),
        }
    }
}

#[derive(Serialize)]
struct RunJson<'a> {
    id: i64,
    pipeline: &'a str,
    pipeline_id: i64,
    build_number: &'a str,
    status: &'a str,
    result: Option<&'a str>,
    branch: String,
    requested_for: Option<&'a str>,
    reason: &'a str,
    queued: Option<String>,
    started: Option<String>,
    finished: Option<String>,
    url: &'a str,
}

impl<'a> From<&'a RunRow> for RunJson<'a> {
    fn from(row: &'a RunRow) -> Self {
        let run = &row.run;
        Self {
            id: run.id,
            pipeline: &row.pipeline,
            pipeline_id: run.pipeline_id,
            build_number: &run.build_number,
            status: run.status.as_str(),
            result: run.result.map(RunResult::as_str),
            branch: row.branch(),
            requested_for: run.requested_for.as_deref(),
            reason: &run.reason,
            queued: run.queue_time.map(|at| at.to_rfc3339()),
            started: run.start_time.map(|at| at.to_rfc3339()),
            finished: run.finish_time.map(|at| at.to_rfc3339()),
            url: &run.url,
        }
    }
}

/// `runs show --json`: the run, and the timeline flat with each node's parent
/// named, which is what a tree is on the wire.
#[derive(Serialize)]
struct RunShowJson<'a> {
    #[serde(flatten)]
    run: RunJson<'a>,
    timeline: Vec<TimelineJson<'a>>,
}

#[derive(Serialize)]
struct TimelineJson<'a> {
    id: &'a str,
    parent_id: Option<&'a str>,
    kind: &'a str,
    name: &'a str,
    state: &'a str,
    result: Option<&'a str>,
    log_id: Option<i64>,
    issues: usize,
}

impl<'a> RunShowJson<'a> {
    fn new(run: &'a Run, pipeline: &'a str, timeline: &'a [TimelineRecord]) -> Self {
        Self {
            run: RunJson {
                id: run.id,
                pipeline,
                pipeline_id: run.pipeline_id,
                build_number: &run.build_number,
                status: run.status.as_str(),
                result: run.result.map(RunResult::as_str),
                branch: short_branch(&run.source_branch),
                requested_for: run.requested_for.as_deref(),
                reason: &run.reason,
                queued: run.queue_time.map(|at| at.to_rfc3339()),
                started: run.start_time.map(|at| at.to_rfc3339()),
                finished: run.finish_time.map(|at| at.to_rfc3339()),
                url: &run.url,
            },
            timeline: timeline
                .iter()
                .map(|record| TimelineJson {
                    id: &record.id,
                    parent_id: record.parent_id.as_deref(),
                    kind: match record.kind {
                        TimelineKind::Stage => "stage",
                        TimelineKind::Job => "job",
                        TimelineKind::Task => "task",
                        TimelineKind::Checkpoint => "checkpoint",
                    },
                    name: &record.name,
                    state: record.state.as_str(),
                    result: record.result.map(RunResult::as_str),
                    log_id: record.log_id,
                    issues: record.issues.len(),
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct ApprovalJson<'a> {
    id: &'a str,
    pipeline: &'a str,
    run_id: Option<i64>,
    build_number: &'a str,
    stage: &'a str,
    instructions: &'a str,
    requested_at: Option<String>,
}

impl<'a> From<&'a Approval> for ApprovalJson<'a> {
    fn from(approval: &'a Approval) -> Self {
        Self {
            id: &approval.id,
            pipeline: &approval.pipeline,
            run_id: approval.run_id,
            build_number: &approval.build_number,
            stage: &approval.stage,
            instructions: &approval.instructions,
            requested_at: approval.requested_at.map(|at| at.to_rfc3339()),
        }
    }
}

/// Pads every column but the last, which is the title.
fn columns(cells: &[Vec<String>]) -> String {
    let width = cells.first().map_or(0, Vec::len);
    let mut widths = vec![0usize; width];
    for row in cells {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    cells
        .iter()
        .map(|row| {
            let mut line = String::new();
            for (index, cell) in row.iter().enumerate() {
                if index + 1 == row.len() {
                    line.push_str(cell);
                } else {
                    line.push_str(cell);
                    line.push_str(&" ".repeat(widths[index] - cell.chars().count() + 2));
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn emit(text: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::azure::{SyncBatch, create_document};
    use crate::edit;
    use crate::model::{StateOption, StoredWorkItem};
    use crate::timestamp::ts;
    use serde_json::json;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};
    use tempfile::{TempDir, tempdir};

    fn ticket(id: i64, title: &str, state: &str, assignee: Option<&str>) -> Ticket {
        Ticket {
            revision: 4,
            state: state.into(),
            assigned_to: assignee.map(ToOwned::to_owned),
            priority: Some(2),
            tags: vec!["cli".into()],
            description: "Ship the subcommands.".into(),
            description_html: "<p>Ship the subcommands.</p>".into(),
            changed_at: ts("2026-02-01T00:00:00Z"),
            ..Ticket::fixture(id, title)
        }
    }

    fn config() -> AzureConfig {
        AzureConfig {
            organization: "demo".into(),
            project: "atlas".into(),
            code_project: "atlas".into(),
            scope: None,
            team: None,
        }
    }

    /// A database with a schema and nothing in it, kept alive by its directory.
    fn repository() -> (TempDir, SqliteTicketRepository) {
        let directory = tempdir().unwrap();
        let repository =
            SqliteTicketRepository::open(directory.path().join("tickets.sqlite3")).unwrap();
        (directory, repository)
    }

    fn edit_args(id: i64) -> EditArgs {
        EditArgs {
            id,
            state: None,
            assignee: None,
            priority: None,
            iteration: None,
            area: None,
            title: None,
            tags: None,
            description_file: None,
        }
    }

    fn create_args(title: &str) -> CreateArgs {
        CreateArgs {
            work_item_type: "Issue".into(),
            title: title.into(),
            parent: None,
            iteration: None,
            assignee: None,
            priority: None,
            tags: None,
        }
    }

    /// One document this source was sent, and the work item id it was for.
    type SentDocument = (i64, Vec<Value>);
    /// One creation, as its work item type and the parent it named.
    type Creation = (String, Option<i64>);

    /// A stand-in for Azure DevOps that answers a write with a stored copy or
    /// with the refusal it was given, and remembers what it was sent.
    #[derive(Default)]
    struct FakeSource {
        stored: Option<Ticket>,
        refusal: Option<(u16, String)>,
        /// Every document sent, with the work item id it was sent for; a
        /// creation is recorded under the id the stored copy carries.
        sent: Arc<Mutex<Vec<SentDocument>>>,
        /// The type and parent each creation named.
        created: Arc<Mutex<Vec<Creation>>>,
        posted: Arc<Mutex<Vec<(i64, String)>>>,
    }

    impl FakeSource {
        fn storing(ticket: Ticket) -> Self {
            Self {
                stored: Some(ticket),
                ..Self::default()
            }
        }

        fn refusing(status: u16, message: &str) -> Self {
            Self {
                refusal: Some((status, message.to_owned())),
                ..Self::default()
            }
        }

        fn answer(&self, id: i64, document: &[Value]) -> Result<StoredWorkItem> {
            self.sent.lock().unwrap().push((id, document.to_vec()));
            if let Some((status, message)) = &self.refusal {
                return Err(anyhow::Error::new(azure::RequestRejected::new(
                    *status,
                    format!("https://dev.azure.com/demo/_apis/wit/workitems/{id}"),
                    message.clone(),
                )));
            }
            self.stored
                .clone()
                .map(|ticket| (ticket, Vec::new(), Vec::new()))
                .context("the fake source was not given a stored copy")
        }

        /// The one document this source was sent.
        fn document(&self) -> Vec<Value> {
            self.sent.lock().unwrap()[0].1.clone()
        }
    }

    impl WorkItemSource for FakeSource {
        fn pull(&self) -> Result<SyncBatch> {
            Ok(SyncBatch::default())
        }

        fn pull_changed_since(&self, _watermark: crate::timestamp::Timestamp) -> Result<SyncBatch> {
            Ok(SyncBatch::default())
        }

        fn list_ids(&self) -> Result<Vec<i64>> {
            Ok(Vec::new())
        }

        fn display_name(&self) -> Result<Option<String>> {
            Ok(None)
        }

        fn patch_work_item(&self, id: i64, patch: &[Value]) -> Result<StoredWorkItem> {
            self.answer(id, patch)
        }

        fn create_work_item(
            &self,
            work_item_type: &str,
            fields: &[Value],
            parent: Option<i64>,
        ) -> Result<StoredWorkItem> {
            self.created
                .lock()
                .unwrap()
                .push((work_item_type.to_owned(), parent));
            let id = self.stored.as_ref().map_or(0, |ticket| ticket.key.id);
            self.answer(id, fields)
        }

        fn work_item_type_states(&self, _work_item_type: &str) -> Result<Vec<StateOption>> {
            Ok(Vec::new())
        }

        fn post_comment(&self, id: i64, html: &str) -> Result<CommentRecord> {
            self.posted.lock().unwrap().push((id, html.to_owned()));
            Ok(CommentRecord {
                // Deliberately about another work item, so a row landing on the
                // one that was asked for is the code's doing rather than luck.
                ticket: TicketKey {
                    organization: "elsewhere".into(),
                    id: 1,
                },
                comment_id: 77,
                created_at: ts("2026-02-02T00:00:00Z"),
                author: Some("Avery Chen".into()),
                text: html.to_owned(),
            })
        }
    }

    #[test]
    fn the_environment_overrides_the_display_name_recorded_by_the_last_sync() {
        assert_eq!(
            resolve_me(Some("Jacob Ragsdale".into()), None).as_deref(),
            Some("Jacob Ragsdale")
        );
        assert_eq!(
            resolve_me(Some("Jacob Ragsdale".into()), Some("  Avery Chen ".into())).as_deref(),
            Some("Avery Chen"),
            "TICKET_TUI_ME wins over the cached profile name"
        );
        assert_eq!(
            resolve_me(Some("Jacob Ragsdale".into()), Some("   ".into())).as_deref(),
            Some("Jacob Ragsdale"),
            "a blank override is not an override"
        );
        assert_eq!(
            resolve_me(None, Some("Avery Chen".into())).as_deref(),
            Some("Avery Chen")
        );
        assert_eq!(resolve_me(None, None), None);
        assert_eq!(resolve_me(Some(String::new()), None), None);
    }

    #[test]
    fn a_bare_invocation_opens_the_tui_and_the_flags_around_it_reach_every_subcommand() {
        let bare = Cli::parse_from(["ticket-tui"]);
        assert!(bare.command.is_none(), "a bare run still opens the TUI");

        let flagged = Cli::parse_from(["ticket-tui", "--refresh", "300"]);
        assert!(flagged.command.is_none());
        assert_eq!(flagged.refresh, Some(300));

        let before = Cli::parse_from([
            "ticket-tui",
            "--database",
            "tickets.sqlite3",
            "--org",
            "demo",
            "--project",
            "atlas",
            "sync",
            "--full",
        ]);
        assert_eq!(
            before.database.as_deref(),
            Some(Path::new("tickets.sqlite3"))
        );
        assert_eq!(before.org.as_deref(), Some("demo"));
        assert_eq!(before.project.as_deref(), Some("atlas"));
        assert!(matches!(before.command, Some(Command::Sync { full: true })));

        let after = Cli::parse_from([
            "ticket-tui",
            "sync",
            "--database",
            "tickets.sqlite3",
            "--org",
            "demo",
        ]);
        assert_eq!(
            after.database.as_deref(),
            Some(Path::new("tickets.sqlite3")),
            "the global flags may be written after the subcommand too"
        );
        assert_eq!(after.org.as_deref(), Some("demo"));
        assert!(matches!(after.command, Some(Command::Sync { full: false })));
    }

    #[test]
    fn the_file_answers_for_what_no_flag_and_no_variable_did() {
        let file = crate::config::parse(
            "[devops]\norg = \"file-org\"\nproject = \"ISTO\"\ncode_project = \"Fiquants\"\n\
             query = \"[System.Id] > 1\"\nworkspace = \"/srv/code\"\n",
        )
        .unwrap();
        let env = |key: &str| match key {
            "TICKET_TUI_PROJECT" => Some("env-project".to_owned()),
            "TICKET_TUI_CODE_PROJECT" => Some("env-code".to_owned()),
            "TICKET_TUI_QUERY" => Some("env query".to_owned()),
            "TICKET_TUI_WORKSPACE" => Some("/env/code".to_owned()),
            _ => None,
        };

        // The flag wins over the variable, which wins over the file.
        let flagged = Cli::parse_from([
            "ticket-tui",
            "--org",
            "flag-org",
            "--query",
            "flag query",
            "--workspace",
            "/flag/code",
        ])
        .with_file_defaults(&file, env);
        assert_eq!(flagged.org.as_deref(), Some("flag-org"));
        assert_eq!(flagged.query.as_deref(), Some("flag query"));
        assert_eq!(flagged.workspace.as_deref(), Some(Path::new("/flag/code")));
        assert_eq!(flagged.project.as_deref(), Some("env-project"));
        assert_eq!(flagged.code_project.as_deref(), Some("env-code"));

        // With neither flag nor variable, every one of them comes from the file.
        let from_file = Cli::parse_from(["ticket-tui"]).with_file_defaults(&file, |_| None);
        assert_eq!(from_file.org.as_deref(), Some("file-org"));
        assert_eq!(from_file.project.as_deref(), Some("ISTO"));
        assert_eq!(from_file.code_project.as_deref(), Some("Fiquants"));
        assert_eq!(from_file.query.as_deref(), Some("[System.Id] > 1"));
        assert_eq!(from_file.workspace.as_deref(), Some(Path::new("/srv/code")));

        // And with no file either, everything is left for `az` to answer.
        let bare = Cli::parse_from(["ticket-tui"]).with_file_defaults(&Config::default(), |_| None);
        assert_eq!(bare.org, None);
        assert_eq!(bare.code_project, None);
    }

    #[test]
    fn each_subcommand_takes_the_arguments_the_readme_documents() {
        let Some(Command::List { query, json }) =
            Cli::parse_from(["ticket-tui", "list", "--query", "state:doing", "--json"]).command
        else {
            panic!("list did not parse");
        };
        assert_eq!(query.as_deref(), Some("state:doing"));
        assert!(json);

        let scoped = Cli::parse_from([
            "ticket-tui",
            "--query",
            "[System.ChangedDate] > @today-30",
            "list",
            "--query",
            "state:doing",
        ]);
        assert_eq!(
            scoped.query.as_deref(),
            Some("[System.ChangedDate] > @today-30"),
            "the WIQL scope is the --query written before the subcommand; the \
             filter grammar is the one written after it"
        );
        assert!(matches!(
            scoped.command,
            Some(Command::List { ref query, .. }) if query.as_deref() == Some("state:doing")
        ));

        let Some(Command::Show { id, json }) =
            Cli::parse_from(["ticket-tui", "show", "613"]).command
        else {
            panic!("show did not parse");
        };
        assert_eq!((id, json), (613, false));

        let Some(Command::Comment { id, text }) =
            Cli::parse_from(["ticket-tui", "comment", "613", "on its way"]).command
        else {
            panic!("comment did not parse");
        };
        assert_eq!((id, text.as_deref()), (613, Some("on its way")));

        let Some(Command::Edit(args)) = Cli::parse_from([
            "ticket-tui",
            "edit",
            "613",
            "--state",
            "Doing",
            "--tags",
            "cli,agents",
            "--description-file",
            "notes.md",
        ])
        .command
        else {
            panic!("edit did not parse");
        };
        assert_eq!(args.id, 613);
        assert_eq!(args.state.as_deref(), Some("Doing"));
        assert_eq!(args.tags.as_deref(), Some("cli,agents"));
        assert_eq!(
            args.description_file.as_deref(),
            Some(Path::new("notes.md"))
        );

        let Some(Command::Create(args)) = Cli::parse_from([
            "ticket-tui",
            "create",
            "--type",
            "Issue",
            "--title",
            "CLI subcommands",
            "--parent",
            "613",
        ])
        .command
        else {
            panic!("create did not parse");
        };
        assert_eq!(args.work_item_type, "Issue");
        assert_eq!(args.title, "CLI subcommands");
        assert_eq!(args.parent, Some(613));
    }

    #[test]
    fn listing_narrows_by_the_filter_grammar_and_answers_newest_change_first() {
        let mut older = ticket(1, "Edit dispatcher", "Doing", Some("Avery Chen"));
        older.changed_at = ts("2026-01-05T00:00:00Z");
        let newer = ticket(2, "Sync watermark", "Doing", Some("Jacob Ragsdale"));
        let done = ticket(3, "Details pane", "Done", Some("Avery Chen"));
        let tickets = vec![older, newer, done];
        let nobody = MatchContext::now();
        let signed_in = MatchContext::now().with_me(Some("Jacob Ragsdale".into()));

        let all = select(tickets.clone(), None, &nobody, IterationTree::Cached).unwrap();
        assert_eq!(
            all.iter().map(|ticket| ticket.key.id).collect::<Vec<_>>(),
            vec![3, 2, 1],
            "with no query the rows come back newest change first, and the \
             newer id first when two changed in the same second"
        );

        let doing = select(
            tickets.clone(),
            Some("state:doing"),
            &nobody,
            IterationTree::Cached,
        )
        .unwrap();
        assert_eq!(
            doing.iter().map(|ticket| ticket.key.id).collect::<Vec<_>>(),
            vec![2, 1],
            "the grammar matches a state whatever its case"
        );

        let mine = select(
            tickets.clone(),
            Some("state:doing assignee:@me"),
            &signed_in,
            IterationTree::Cached,
        )
        .unwrap();
        assert_eq!(
            mine.iter().map(|ticket| ticket.key.id).collect::<Vec<_>>(),
            vec![2],
            "@me is the name the last sync recorded"
        );

        let fuzzy = select(
            tickets,
            Some("state:doing dispatcher"),
            &nobody,
            IterationTree::Cached,
        )
        .unwrap();
        assert_eq!(
            fuzzy.iter().map(|ticket| ticket.key.id).collect::<Vec<_>>(),
            vec![1],
            "whatever is left over after the field filters is matched fuzzily"
        );
    }

    #[test]
    fn a_sentinel_the_run_cannot_resolve_is_said_rather_than_answered_with_nothing() {
        let tickets = vec![ticket(1, "Edit dispatcher", "Doing", Some("Avery Chen"))];

        let unsigned = select(
            tickets.clone(),
            Some("assignee:@me"),
            &MatchContext::now(),
            IterationTree::Cached,
        )
        .expect_err("@me with nobody signed in has no answer to give");
        assert!(
            format!("{unsigned:#}").contains("TICKET_TUI_ME"),
            "a run with nobody signed in says so rather than matching everything: {unsigned:#}"
        );

        let undated = select(
            tickets.clone(),
            Some("iteration:@current"),
            &MatchContext::now(),
            IterationTree::Cached,
        )
        .expect_err("@current with no sprint around today has no answer to give");
        assert!(
            format!("{undated:#}").contains("dates contain today"),
            "a cached tree with no sprint around today names the dates as the cause \
             rather than sending them back to sync: {undated:#}"
        );

        let unread = select(
            tickets.clone(),
            Some("iteration:@current"),
            &MatchContext::now(),
            IterationTree::Unread,
        )
        .expect_err("@current with no tree to read has no answer to give");
        assert!(
            format!("{unread:#}").contains("ticket-tui sync"),
            "a tree nobody has pulled yet sends them to sync instead: {unread:#}"
        );

        let scheduled = select(
            tickets,
            Some("iteration:@current"),
            &MatchContext::now().with_current_iteration(Some("Atlas\\Sprint 1".into())),
            IterationTree::Cached,
        )
        .unwrap();
        assert_eq!(
            scheduled.len(),
            1,
            "and it resolves to the sprint the cached tree puts today in"
        );
    }

    #[test]
    fn the_json_shape_names_the_fields_the_filter_grammar_names_and_only_show_carries_the_body() {
        let ticket = ticket(613, "Edit dispatcher", "Doing", Some("Avery Chen"));

        let rows: Value =
            serde_json::from_str(&to_json(&vec![TicketJson::row(&ticket)]).unwrap()).unwrap();
        assert_eq!(rows[0]["id"], 613);
        assert_eq!(rows[0]["organization"], "demo");
        assert_eq!(rows[0]["project"], "atlas");
        assert_eq!(rows[0]["rev"], 4);
        assert_eq!(rows[0]["type"], "Task");
        assert_eq!(rows[0]["title"], "Edit dispatcher");
        assert_eq!(rows[0]["state"], "Doing");
        assert_eq!(rows[0]["assignee"], "Avery Chen");
        assert_eq!(rows[0]["priority"], 2);
        assert_eq!(rows[0]["area"], "Atlas");
        assert_eq!(rows[0]["iteration"], "Atlas\\Sprint 1");
        assert_eq!(rows[0]["tags"], json!(["cli"]));
        assert_eq!(rows[0]["created"], "2026-01-01T00:00:00Z");
        assert_eq!(rows[0]["changed"], "2026-02-01T00:00:00Z");
        assert_eq!(
            rows[0]["url"],
            "https://dev.azure.com/demo/atlas/_workitems/edit/613"
        );
        assert!(
            rows[0].get("description").is_none(),
            "a list of five hundred rows is no place for five hundred descriptions"
        );

        let shown: Value =
            serde_json::from_str(&to_json(&TicketJson::detailed(&ticket)).unwrap()).unwrap();
        assert_eq!(shown["id"], 613);
        assert_eq!(shown["description"], "Ship the subcommands.");

        let unheld = Ticket {
            priority: None,
            assigned_to: None,
            ..ticket
        };
        let row: Value =
            serde_json::from_str(&to_json(&TicketJson::row(&unheld)).unwrap()).unwrap();
        assert_eq!(
            (&row["assignee"], &row["priority"]),
            (&Value::Null, &Value::Null),
            "an unset field reads as null rather than as an empty string"
        );
    }

    #[test]
    fn an_edit_leads_with_the_revision_the_database_holds_and_stores_what_came_back() {
        let (_directory, mut repository) = repository();
        let stored = ticket(613, "Edit dispatcher", "To Do", Some("Avery Chen"));
        repository.upsert(&stored, &[], &[]).unwrap();
        let key = stored.key.clone();
        let source = FakeSource::storing(Ticket {
            revision: 5,
            state: "Doing".into(),
            ..stored
        });

        let edits = field_edits(
            &EditArgs {
                state: Some("Doing".into()),
                tags: Some("cli, agents,cli".into()),
                ..edit_args(613)
            },
            &[],
            None,
        )
        .unwrap();
        let ticket = apply_edits(&source, &mut repository, &key, &edits).unwrap();

        assert_eq!(
            source.document(),
            vec![
                json!({"op": "test", "path": "/rev", "value": 4}),
                json!({"op": "add", "path": "/fields/System.State", "value": "Doing"}),
                json!({"op": "add", "path": "/fields/System.Tags", "value": "cli; agents"}),
            ],
            "one document, led by the revision the database holds, with a tag \
             list typed with commas and stored the way the tags prompt stores it"
        );
        assert_eq!((ticket.revision, ticket.state.as_str()), (5, "Doing"));
        assert_eq!(
            repository.revision_of(&key).unwrap(),
            Some(5),
            "the copy Azure DevOps answered with is what the database ends up holding"
        );
    }

    #[test]
    fn a_work_item_the_database_has_never_seen_is_written_without_a_revision_test() {
        let (_directory, mut repository) = repository();
        let stored = ticket(700, "Brand new", "Doing", None);
        let key = stored.key.clone();
        let source = FakeSource::storing(stored);
        let edits = field_edits(
            &EditArgs {
                state: Some("Doing".into()),
                ..edit_args(700)
            },
            &[],
            None,
        )
        .unwrap();

        apply_edits(&source, &mut repository, &key, &edits).unwrap();

        assert_eq!(
            source.document(),
            vec![json!({"op": "add", "path": "/fields/System.State", "value": "Doing"})],
            "there is no revision to claim the work item was read at"
        );
    }

    #[test]
    fn a_refused_edit_reports_what_azure_devops_said_and_a_conflict_says_what_to_do_about_it() {
        let (_directory, mut repository) = repository();
        let stored = ticket(613, "Edit dispatcher", "To Do", Some("Avery Chen"));
        repository.upsert(&stored, &[], &[]).unwrap();
        let key = stored.key.clone();
        let edits = vec![FieldEdit::state("Doing")];

        let refused = apply_edits(
            &FakeSource::refusing(403, "the work item is read only"),
            &mut repository,
            &key,
            &edits,
        )
        .unwrap_err();
        assert_eq!(
            format!("{refused:#}"),
            "Azure DevOps returned HTTP 403 for \
             https://dev.azure.com/demo/_apis/wit/workitems/613: the work item is read only",
            "a refusal is reported in Azure DevOps's own words, and run() exits 1 on it"
        );

        let conflict = apply_edits(
            &FakeSource::refusing(409, "the work item has been changed"),
            &mut repository,
            &key,
            &edits,
        )
        .unwrap_err();
        let message = format!("{conflict:#}");
        assert!(
            message.starts_with("#613 changed in Azure DevOps since the last sync"),
            "a work item that moved on is told how to catch up: {message}"
        );
        assert!(
            message.contains("the work item has been changed"),
            "and still carries what Azure DevOps said: {message}"
        );
        assert_eq!(
            repository.revision_of(&key).unwrap(),
            Some(4),
            "a refused write leaves the stored row exactly as it was"
        );
    }

    #[test]
    fn creating_a_work_item_sends_its_fields_under_its_type_and_stores_the_answer() {
        let (_directory, mut repository) = repository();
        let source = FakeSource::storing(Ticket {
            revision: 1,
            work_item_type: "Issue".into(),
            ..ticket(700, "CLI subcommands", "To Do", Some("Avery Chen"))
        });
        let args = CreateArgs {
            parent: Some(613),
            priority: Some(1),
            assignee: Some("avery@example.com".into()),
            tags: Some("cli,agents".into()),
            ..create_args("CLI subcommands")
        };
        let identities = [Identity::new(
            "Avery Chen",
            Some("avery@example.com".to_owned()),
        )];
        let edits = create_edits(&args, &identities, None).unwrap();

        let ticket = create_work_item(&source, &mut repository, &args, &edits).unwrap();

        assert_eq!(
            source.created.lock().unwrap().clone(),
            vec![("Issue".to_owned(), Some(613))],
            "the type and the parent travel beside the fields, not among them"
        );
        assert_eq!(
            source.document(),
            vec![
                json!({"op": "add", "path": "/fields/System.Title", "value": "CLI subcommands"}),
                json!({
                    "op": "add",
                    "path": "/fields/System.AssignedTo",
                    "value": "avery@example.com",
                }),
                json!({"op": "add", "path": "/fields/Microsoft.VSTS.Common.Priority", "value": 1}),
                json!({"op": "add", "path": "/fields/System.Tags", "value": "cli; agents"}),
            ],
            "a creation carries no revision test, and a name the database knows \
             is written by the address it knows for it"
        );
        assert_eq!(
            create_document(&source.document(), args.parent, &config())
                .last()
                .unwrap(),
            &json!({
                "op": "add",
                "path": "/relations/-",
                "value": {
                    "rel": "System.LinkTypes.Hierarchy-Reverse",
                    "url": "https://dev.azure.com/demo/_apis/wit/workItems/613",
                },
            }),
            "the parent is a link appended after the fields"
        );
        assert_eq!(
            repository.revision_of(&ticket.key).unwrap(),
            Some(1),
            "the new work item is in the database the TUI is watching"
        );
    }

    #[test]
    fn a_created_work_item_needs_a_title_and_an_edit_needs_something_to_change() {
        assert!(
            create_edits(&create_args("   "), &[], None)
                .unwrap_err()
                .to_string()
                .contains("without a title")
        );
        assert!(
            field_edits(&edit_args(613), &[], None).unwrap().is_empty(),
            "an edit naming no field builds no document, which the subcommand refuses"
        );
        assert!(
            field_edits(
                &EditArgs {
                    title: Some("  ".into()),
                    ..edit_args(613)
                },
                &[],
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("empty title")
        );
    }

    #[test]
    fn an_assignee_is_resolved_the_way_the_picker_resolves_one() {
        let identities = [Identity::new(
            "Avery Chen",
            Some("avery@example.com".to_owned()),
        )];

        assert_eq!(
            assignee_edit("avery chen", &identities, None)
                .unwrap()
                .patch(),
            vec![json!({
                "op": "add",
                "path": "/fields/System.AssignedTo",
                "value": "avery@example.com",
            })],
            "a display name the database knows is written by its address"
        );
        assert_eq!(
            assignee_edit("Someone Else", &identities, None)
                .unwrap()
                .patch(),
            vec![json!({
                "op": "add",
                "path": "/fields/System.AssignedTo",
                "value": "Someone Else",
            })],
            "anybody else goes out as typed, for Azure DevOps to resolve"
        );
        assert_eq!(
            assignee_edit("@me", &identities, Some("Avery Chen"))
                .unwrap()
                .value_text(),
            "Avery Chen"
        );
        assert!(
            assignee_edit("@me", &identities, None)
                .unwrap_err()
                .to_string()
                .contains("TICKET_TUI_ME")
        );
        assert_eq!(
            assignee_edit("  ", &identities, None).unwrap().patch(),
            vec![json!({"op": "remove", "path": "/fields/System.AssignedTo"})],
            "an empty name takes the work item off whoever holds it"
        );
    }

    #[test]
    fn a_description_file_is_read_as_markdown_and_written_as_the_html_azure_devops_stores() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("notes.md");
        fs::write(&path, "Ship it.\n\n- one\n- two\n").unwrap();

        let edits = field_edits(
            &EditArgs {
                description_file: Some(path),
                ..edit_args(613)
            },
            &[],
            None,
        )
        .unwrap();

        let [description] = edits.as_slice() else {
            panic!("one description edit was expected");
        };
        assert_eq!(description.field(), edit::DESCRIPTION_FIELD);
        let html = description.patch()[0]["value"].as_str().unwrap().to_owned();
        assert!(html.contains("<p>Ship it.</p>"), "{html}");
        assert!(html.contains("<li>one</li>"), "{html}");

        let missing = field_edits(
            &EditArgs {
                description_file: Some(directory.path().join("nothing.md")),
                ..edit_args(613)
            },
            &[],
            None,
        )
        .unwrap_err();
        assert!(
            format!("{missing:#}").contains("failed to read the description"),
            "{missing:#}"
        );
    }

    #[test]
    fn a_posted_comment_lands_on_the_work_item_the_request_named() {
        let (_directory, mut repository) = repository();
        let stored = ticket(613, "Edit dispatcher", "Doing", None);
        repository.upsert(&stored, &[], &[]).unwrap();
        let source = FakeSource::default();

        let typed = comment_body(Some("on its way <now>"), &mut empty(), true).unwrap();
        let comment = post_comment(&source, &mut repository, &stored.key, &typed).unwrap();

        assert_eq!(
            source.posted.lock().unwrap().clone(),
            vec![(613, "<p>on its way &lt;now&gt;</p>".to_owned())],
            "what was typed is posted as the rich text Azure DevOps stores"
        );
        assert_eq!(
            comment.ticket, stored.key,
            "the row lands on the work item the request named, whatever the answer is about"
        );
        assert_eq!(comment.comment_id, 77);
    }

    fn empty() -> Cursor<Vec<u8>> {
        Cursor::new(Vec::new())
    }

    #[test]
    fn a_piped_comment_body_is_posted_as_a_code_block_and_a_typed_one_as_a_line() {
        let piped = comment_body(
            Some("-"),
            &mut Cursor::new(b"test result: FAILED\n  left  <1>\n".to_vec()),
            true,
        )
        .unwrap();

        assert_eq!(
            piped,
            CommentBody {
                text: "test result: FAILED\n  left  <1>".into(),
                fenced: true,
            },
            "the trailing newline a pipe carries comes off; the indent does not"
        );
        assert_eq!(
            piped.html(),
            "<pre>test result: FAILED\n  left  &lt;1&gt;</pre>",
            "program output is posted as a block, so its columns stay lined up"
        );
        assert_eq!(
            piped.markdown(),
            "```\ntest result: FAILED\n  left  <1>\n```",
            "a pull-request thread stores Markdown, so the fence goes out as a fence"
        );

        let typed = comment_body(Some("on its way"), &mut empty(), true).unwrap();
        assert_eq!(
            typed,
            CommentBody {
                text: "on its way".into(),
                fenced: false,
            }
        );
        assert_eq!(typed.html(), "<p>on its way</p>");
        assert_eq!(typed.markdown(), "on its way");
    }

    #[test]
    fn an_absent_body_reads_the_pipe_and_is_a_usage_error_at_a_terminal() {
        let piped = comment_body(
            None,
            &mut Cursor::new(b"tail -30 said this\n".to_vec()),
            false,
        )
        .expect("a non-tty standard input is the same as `-`");
        assert!(piped.fenced);
        assert_eq!(piped.text, "tail -30 said this");

        let usage = comment_body(None, &mut empty(), true).unwrap_err();
        let usage = format!("{usage:#}");
        assert!(usage.contains("comment ID \"text\""), "{usage}");
        assert!(usage.contains("comment ID -"), "{usage}");
    }

    #[test]
    fn a_comment_with_nothing_in_it_is_refused_however_it_arrived() {
        for empty_body in [
            comment_body(Some("-"), &mut empty(), false),
            comment_body(Some("-"), &mut Cursor::new(b"\n \n".to_vec()), false),
            comment_body(Some("   "), &mut empty(), true),
        ] {
            let refusal = format!("{:#}", empty_body.unwrap_err());
            assert!(refusal.contains("a comment cannot be empty"), "{refusal}");
        }
    }

    #[test]
    fn a_piped_body_quoting_a_code_block_is_fenced_longer_than_the_block() {
        let body = CommentBody {
            text: "before\n```\ninner\n```\nafter".to_owned(),
            fenced: true,
        };
        let markdown = body.markdown();
        assert!(markdown.starts_with("````\n"), "{markdown}");
        assert!(markdown.ends_with("\n````"), "{markdown}");
        let html = body.html();
        assert_eq!(html.matches("<pre>").count(), 1, "one block: {html}");
        assert!(html.contains("inner"), "{html}");
    }

    #[test]
    fn a_body_over_the_limit_is_refused_with_its_size_rather_than_truncated() {
        let over = vec![b'x'; usize::try_from(COMMENT_LIMIT).unwrap() + 1];
        let refusal = format!(
            "{:#}",
            comment_body(Some("-"), &mut Cursor::new(over), false).unwrap_err()
        );

        assert!(
            refusal.contains("that is a log, not a comment: more than 64 kB"),
            "{refusal}"
        );
        assert!(refusal.contains("Pipe it through tail"), "{refusal}");
        assert!(
            comment_body(
                Some("-"),
                &mut Cursor::new(vec![b'x'; usize::try_from(COMMENT_LIMIT).unwrap()]),
                false,
            )
            .is_ok(),
            "the limit itself still posts"
        );
    }

    #[test]
    fn one_pull_reports_what_moved_and_anything_that_stopped_it_is_an_error() {
        assert_eq!(
            sync_report(
                &SyncOutcome::Pulled {
                    snapshot: Box::new(crate::app::Snapshot::with_graph(
                        Vec::new(),
                        crate::model::TicketGraph::default(),
                    )),
                    mode: SyncMode::Full,
                    count: 59,
                },
                &config(),
            )
            .unwrap(),
            "Synced 59 work items from demo/atlas"
        );
        assert_eq!(
            sync_report(&SyncOutcome::Unchanged, &config()).unwrap(),
            "Synced 0 changes from demo/atlas",
            "a pull that reached Azure DevOps and found nothing is not a failure"
        );
        assert_eq!(
            format!(
                "{:#}",
                sync_report(&SyncOutcome::Failed("no network".into()), &config()).unwrap_err()
            ),
            "no network"
        );
        let throttled = sync_report(
            &SyncOutcome::Throttled {
                retry_after: std::time::Duration::from_secs(45),
            },
            &config(),
        )
        .unwrap_err();
        assert!(
            format!("{throttled:#}").contains("try again in 45s"),
            "{throttled:#}"
        );
    }

    #[test]
    fn a_pull_refuses_a_database_another_project_filled_unless_it_replaces_every_row() {
        let stored = || Some(("other-org".to_owned(), "borealis".to_owned()));

        let refused = guard_stored_project(stored(), &config(), false).unwrap_err();
        assert_eq!(
            format!("{refused:#}"),
            "database holds other-org/borealis; pass --database for another \
             project or --full to replace it"
        );
        assert!(
            guard_stored_project(stored(), &config(), true).is_ok(),
            "--full is how the replacement is asked for"
        );
        assert!(
            guard_stored_project(
                Some(("demo".to_owned(), "atlas".to_owned())),
                &config(),
                false
            )
            .is_ok()
        );
        assert!(
            guard_stored_project(None, &config(), false).is_ok(),
            "a database from a build that recorded nothing adopts the project that pulls it"
        );
    }

    #[test]
    fn a_database_that_is_not_there_is_said_so_rather_than_created_empty() {
        let directory = tempdir().unwrap();
        let missing = directory.path().join("tickets.sqlite3");
        let error = open_database(&missing).unwrap_err();
        assert!(
            format!("{error:#}").contains("run `ticket-tui sync` to pull one"),
            "{error:#}"
        );
        assert!(!missing.exists(), "and no empty file is left behind");
    }
    /// A repository, a pull request and a database holding both.
    fn pr_repository() -> (TempDir, PathBuf) {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let mut repository = SqliteTicketRepository::open(&path).unwrap();
        repository
            .replace_repos(&[crate::model::Repo {
                id: "aaa-111".into(),
                name: "ticket-tui".into(),
                project: "atlas".into(),
                default_branch: Some("refs/heads/main".into()),
                remote_url: "https://dev.azure.com/demo/atlas/_git/ticket-tui".into(),
                ssh_url: "git@ssh.dev.azure.com:v3/demo/atlas/ticket-tui".into(),
                web_url: "https://dev.azure.com/demo/atlas/_git/ticket-tui".into(),
                is_disabled: false,
                size: Some(2_097_152),
            }])
            .unwrap();
        repository
            .replace_pull_requests(&[stored_pull_request()])
            .unwrap();
        (directory, path)
    }

    fn stored_pull_request() -> PullRequest {
        PullRequest {
            repo_id: "aaa-111".into(),
            id: 11,
            title: "Split the files".into(),
            description: "What it does.".into(),
            status: crate::model::PrStatus::Active,
            is_draft: false,
            created_by: Identity::new("Avery Chen".to_owned(), None),
            created_at: Some(crate::timestamp::ts("2026-08-29T07:00:00Z")),
            closed_at: None,
            source_ref: "refs/heads/feature/tabs".into(),
            target_ref: "refs/heads/main".into(),
            merge_status: "succeeded".into(),
            last_merge_source_commit: "abc1234".into(),
            auto_complete_set_by: None,
            url: "https://dev.azure.com/demo/atlas/_git/ticket-tui/pullrequest/11".into(),
            reviewers: vec![crate::model::PrReviewer {
                id: "me-id".into(),
                display_name: "Jacob Ragsdale".into(),
                unique_name: None,
                vote: 0,
                is_required: true,
            }],
            work_items: vec![690],
            build: None,
            threads: Vec::new(),
        }
    }

    #[test]
    fn the_repos_and_prs_subcommands_take_the_arguments_the_readme_documents() {
        let Some(Command::Repos(ReposCommand::List { query, json })) = Cli::parse_from([
            "ticket-tui",
            "repos",
            "list",
            "--query",
            "local:dirty",
            "--json",
        ])
        .command
        else {
            panic!("repos list did not parse");
        };
        assert_eq!(query.as_deref(), Some("local:dirty"));
        assert!(json);

        let Some(Command::Repos(ReposCommand::Show { name, .. })) =
            Cli::parse_from(["ticket-tui", "repos", "show", "ticket-tui"]).command
        else {
            panic!("repos show did not parse");
        };
        assert_eq!(name, "ticket-tui");

        let Some(Command::Prs(PrsCommand::List { query, .. })) = Cli::parse_from([
            "ticket-tui",
            "prs",
            "list",
            "--query",
            "reviewer:@me vote:none",
        ])
        .command
        else {
            panic!("prs list did not parse");
        };
        assert_eq!(query.as_deref(), Some("reviewer:@me vote:none"));

        let Some(Command::Prs(PrsCommand::Vote { id, vote })) =
            Cli::parse_from(["ticket-tui", "prs", "vote", "11", "approve"]).command
        else {
            panic!("prs vote did not parse");
        };
        assert_eq!((id, vote.as_str()), (11, "approve"));

        let Some(Command::Prs(PrsCommand::Complete {
            id,
            strategy,
            keep_source,
            no_transition,
        })) = Cli::parse_from([
            "ticket-tui",
            "prs",
            "complete",
            "11",
            "--strategy",
            "rebase",
            "--keep-source",
        ])
        .command
        else {
            panic!("prs complete did not parse");
        };
        assert_eq!((id, strategy.as_deref()), (11, Some("rebase")));
        assert!(keep_source && !no_transition);

        assert!(matches!(
            Cli::parse_from(["ticket-tui", "prs", "abandon", "11"]).command,
            Some(Command::Prs(PrsCommand::Abandon { id: 11 }))
        ));
        assert!(matches!(
            Cli::parse_from(["ticket-tui", "prs", "autocomplete", "11", "on"]).command,
            Some(Command::Prs(PrsCommand::Autocomplete { id: 11, ref state })) if state == "on"
        ));
        assert!(matches!(
            Cli::parse_from(["ticket-tui", "prs", "comment", "11", "looks good"]).command,
            Some(Command::Prs(PrsCommand::Comment { id: 11, ref text })) if text.as_deref() == Some("looks good")
        ));
    }

    #[test]
    fn a_vote_and_a_strategy_are_read_by_name_and_a_typo_says_what_the_names_are() {
        assert_eq!(parse_vote("approve").unwrap(), 10);
        assert_eq!(parse_vote("Suggest").unwrap(), 5);
        assert_eq!(parse_vote("wait").unwrap(), -5);
        assert_eq!(parse_vote("reject").unwrap(), -10);
        assert_eq!(parse_vote("none").unwrap(), 0);
        let refused = parse_vote("lgtm").unwrap_err().to_string();
        assert!(
            refused.contains("approve, suggest, wait, reject or none"),
            "{refused}"
        );

        assert_eq!(merge_strategy(None).unwrap(), MergeStrategy::Squash);
        assert_eq!(merge_strategy(Some("merge")).unwrap(), MergeStrategy::Merge);
        assert_eq!(
            merge_strategy(Some("rebase")).unwrap(),
            MergeStrategy::Rebase
        );
        assert!(
            merge_strategy(Some("octopus"))
                .unwrap_err()
                .to_string()
                .contains("squash, merge or rebase")
        );
    }

    #[test]
    fn the_pull_request_reads_answer_from_the_database_and_narrow_by_the_tab_grammar() {
        let (_directory, path) = pr_repository();
        let repository = open_database(&path).unwrap();
        let rows = pr_rows(&repository).unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].repo, "ticket-tui", "the GUID is resolved to a name");

        let mine = filter_pull_requests(
            rows.clone(),
            Some("reviewer:@me vote:none"),
            Some("Jacob Ragsdale".to_owned()),
        )
        .unwrap();
        assert_eq!(mine.len(), 1, "which is what To review is");

        let others = filter_pull_requests(
            rows.clone(),
            Some("author:@me"),
            Some("Jacob Ragsdale".to_owned()),
        )
        .unwrap();
        assert!(others.is_empty(), "it is somebody else's");

        let refused = filter_pull_requests(rows.clone(), Some("reviewer:@me"), None).unwrap_err();
        assert!(
            refused.to_string().contains("TICKET_TUI_ME"),
            "a sentinel nothing can resolve is said rather than answered with nothing: {refused}"
        );

        let table = tabulate_pull_requests(&rows);
        assert!(
            table.starts_with("!11  ticket-tui  Avery Chen  0/1"),
            "{table}"
        );
        assert!(table.ends_with("Split the files"), "{table}");
        assert_eq!(
            tabulate_pull_requests(&[]),
            "no matching pull requests",
            "and an empty answer says so"
        );

        // `show` carries what `list` leaves out.
        let listed = serde_json::to_value(PrJson::row(&rows[0])).unwrap();
        assert!(listed.get("reviewers").is_none());
        assert!(listed.get("description").is_none());
        let shown = serde_json::to_value(PrJson::full(&rows[0])).unwrap();
        assert_eq!(shown["reviewers"][0]["name"], "Jacob Ragsdale");
        assert_eq!(shown["work_items"][0], 690);
        assert_eq!(shown["target"], "main");
        assert_eq!(shown["description"], "What it does.");

        let text = describe_pull_request(&rows[0]);
        assert!(text.contains("!11 active"), "{text}");
        assert!(text.contains("feature/tabs \u{2192} main"), "{text}");
        assert!(text.contains("#690"), "{text}");
    }

    #[test]
    fn the_repository_read_names_what_is_open_against_each_one() {
        let (directory, path) = pr_repository();
        let cli = Cli::parse_from([
            "ticket-tui",
            "--workspace",
            directory.path().join("nowhere").to_str().unwrap(),
            "repos",
            "list",
        ]);
        let Some(Command::Repos(command)) = &cli.command else {
            panic!("repos list did not parse");
        };
        // A workspace that is not there is not an error: it simply finds
        // nothing local.
        run_repos(&cli, &path, command).unwrap();

        let repository = open_database(&path).unwrap();
        let row = RepoRow {
            repo: repository.load_repos().unwrap().remove(0),
            local: None,
            pull_requests: 1,
            pipelines: 0,
        };
        let table = tabulate_repos(std::slice::from_ref(&row));
        assert!(
            table.starts_with("ticket-tui  main  1  0  \u{2014}"),
            "{table}"
        );
        let text = describe_repo(&row);
        assert!(text.contains("Pull requests   1"), "{text}");
        assert!(text.contains("git@ssh.dev.azure.com"), "{text}");
        assert!(text.contains("not on this machine"), "{text}");
        let json = serde_json::to_value(RepoJson::from(&row)).unwrap();
        assert_eq!(json["name"], "ticket-tui");
        assert_eq!(json["default_branch"], "main");
        assert_eq!(json["pull_requests"], 1);
        assert!(json["local"].is_null());
    }

    #[test]
    fn every_pull_request_write_stores_the_copy_azure_devops_answered_with() {
        let action =
            |action: &PrAction, me: Option<&str>| crate::sync::pull_request_patch(action, me);

        let completion = action(
            &PrAction::Complete(CompletionOptions {
                strategy: MergeStrategy::Rebase,
                delete_source: false,
                transition_work_items: true,
                last_merge_source_commit: "abc1234".into(),
            }),
            None,
        );
        assert_eq!(completion["status"], "completed");
        assert_eq!(completion["completionOptions"]["mergeStrategy"], "rebase");
        assert_eq!(completion["completionOptions"]["deleteSourceBranch"], false);
        assert_eq!(
            completion["lastMergeSourceCommit"]["commitId"], "abc1234",
            "a merge that raced somebody else's push is refused rather than landing over it"
        );
        assert_eq!(action(&PrAction::Abandon, None)["status"], "abandoned");
        assert_eq!(
            action(&PrAction::AutoComplete(true), Some("me-id"))["autoCompleteSetBy"]["id"],
            "me-id"
        );
        assert!(action(&PrAction::AutoComplete(false), None)["autoCompleteSetBy"].is_null());

        // The store puts one back among the rest and keeps the threads a
        // write did not bring down.
        let (_directory, path) = pr_repository();
        let mut repository = open_database(&path).unwrap();
        let mut updated = stored_pull_request();
        updated.status = crate::model::PrStatus::Completed;
        store_pull_request(&mut repository, updated).unwrap();
        let stored = repository.load_pull_requests().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].status, crate::model::PrStatus::Completed);
    }
    /// A pipeline source that answers from a script: each call takes the next
    /// answer, and the last one repeats, so a test can say "running twice,
    /// then finished" without a clock.
    struct ScriptedRuns {
        runs: Mutex<Vec<Run>>,
        timelines: Mutex<Vec<Vec<TimelineRecord>>>,
        lines: Mutex<Vec<Vec<String>>>,
        polls: Arc<Mutex<usize>>,
    }

    impl ScriptedRuns {
        fn new(
            runs: Vec<Run>,
            timelines: Vec<Vec<TimelineRecord>>,
            lines: Vec<Vec<String>>,
        ) -> Self {
            Self {
                runs: Mutex::new(runs),
                timelines: Mutex::new(timelines),
                lines: Mutex::new(lines),
                polls: Arc::new(Mutex::new(0)),
            }
        }
    }

    /// Takes the next scripted answer, repeating the last one for ever.
    fn next_of<T: Clone>(script: &Mutex<Vec<T>>) -> Option<T> {
        let mut script = script.lock().unwrap();
        if script.len() > 1 {
            Some(script.remove(0))
        } else {
            script.first().cloned()
        }
    }

    impl PipelineSource for ScriptedRuns {
        fn live_runs(&self) -> Result<Vec<Run>> {
            Ok(Vec::new())
        }

        fn run(&self, _run_id: i64) -> Result<Option<Run>> {
            *self.polls.lock().unwrap() += 1;
            Ok(next_of(&self.runs))
        }

        fn timeline(&self, _run_id: i64) -> Result<Vec<TimelineRecord>> {
            Ok(next_of(&self.timelines).unwrap_or_default())
        }

        fn log_lines(&self, _run_id: i64, _log_id: i64, start_line: usize) -> Result<Vec<String>> {
            let all = next_of(&self.lines).unwrap_or_default();
            Ok(all.into_iter().skip(start_line).collect())
        }
    }

    fn scripted_run(status: crate::model::RunStatus, result: Option<RunResult>) -> Run {
        Run {
            id: 14,
            pipeline_id: 1,
            build_number: "20260829.4".into(),
            status,
            result,
            source_branch: "refs/heads/main".into(),
            source_version: "abc1234".into(),
            requested_for: Some("Jacob Ragsdale".into()),
            reason: "manual".into(),
            pr_id: None,
            queue_time: Some(crate::timestamp::ts("2026-08-29T10:00:00Z")),
            start_time: Some(crate::timestamp::ts("2026-08-29T10:00:05Z")),
            finish_time: (!status.is_live()).then(|| crate::timestamp::ts("2026-08-29T10:04:17Z")),
            url: "https://dev.azure.com/demo/atlas/_build/results?buildId=14".into(),
        }
    }

    fn scripted_node(name: &str, status: crate::model::RunStatus) -> TimelineRecord {
        TimelineRecord {
            id: name.to_owned(),
            parent_id: None,
            kind: TimelineKind::Task,
            name: name.to_owned(),
            state: status,
            result: (!status.is_live()).then_some(RunResult::Succeeded),
            start: Some(crate::timestamp::ts("2026-08-29T10:00:05Z")),
            finish: (!status.is_live()).then(|| crate::timestamp::ts("2026-08-29T10:04:17Z")),
            percent_complete: None,
            log_id: Some(7),
            order: 1,
            issues: Vec::new(),
        }
    }

    #[test]
    fn the_pipeline_subcommands_take_the_arguments_the_readme_documents() {
        assert!(matches!(
            Cli::parse_from(["ticket-tui", "pipelines", "--json"]).command,
            Some(Command::Pipelines { json: true })
        ));

        let Some(Command::Runs(RunsCommand::List {
            pipeline,
            query,
            json,
        })) = Cli::parse_from([
            "ticket-tui",
            "runs",
            "list",
            "--pipeline",
            "ticket-tui CI",
            "--query",
            "result:failed",
            "--json",
        ])
        .command
        else {
            panic!("runs list did not parse");
        };
        assert_eq!(pipeline.as_deref(), Some("ticket-tui CI"));
        assert_eq!(query.as_deref(), Some("result:failed"));
        assert!(json);

        let Some(Command::Runs(RunsCommand::Logs {
            id, job, follow, ..
        })) = Cli::parse_from([
            "ticket-tui",
            "runs",
            "logs",
            "14",
            "--job",
            "Build",
            "--follow",
        ])
        .command
        else {
            panic!("runs logs did not parse");
        };
        assert_eq!((id, job.as_deref(), follow), (14, Some("Build"), true));
        assert!(
            Cli::try_parse_from([
                "ticket-tui",
                "runs",
                "logs",
                "14",
                "--job",
                "Build",
                "--task",
                "Test",
            ])
            .is_err(),
            "a log comes from one node, so the two ways of naming it are exclusive"
        );

        let Some(Command::Runs(RunsCommand::Trigger {
            pipeline,
            branch,
            follow,
        })) = Cli::parse_from([
            "ticket-tui",
            "runs",
            "trigger",
            "ticket-tui CI",
            "--branch",
            "main",
            "--follow",
        ])
        .command
        else {
            panic!("runs trigger did not parse");
        };
        assert_eq!(
            (pipeline.as_str(), branch.as_str(), follow),
            ("ticket-tui CI", "main", true)
        );

        assert!(matches!(
            Cli::parse_from(["ticket-tui", "runs", "cancel", "14"]).command,
            Some(Command::Runs(RunsCommand::Cancel { id: 14 }))
        ));
        assert!(matches!(
            Cli::parse_from(["ticket-tui", "runs", "retry", "14"]).command,
            Some(Command::Runs(RunsCommand::Retry { id: 14 }))
        ));
        assert!(matches!(
            Cli::parse_from(["ticket-tui", "runs", "wait", "14"]).command,
            Some(Command::Runs(RunsCommand::Wait { id: 14 }))
        ));
        assert!(matches!(
            Cli::parse_from(["ticket-tui", "approvals", "list"]).command,
            Some(Command::Approvals(ApprovalsCommand::List { json: false }))
        ));
        let Some(Command::Approvals(ApprovalsCommand::Approve { id, comment })) =
            Cli::parse_from([
                "ticket-tui",
                "approvals",
                "approve",
                "abc-123",
                "--comment",
                "ship it",
            ])
            .command
        else {
            panic!("approvals approve did not parse");
        };
        assert_eq!(
            (id.as_str(), comment.as_deref()),
            ("abc-123", Some("ship it"))
        );
    }

    #[test]
    fn waiting_polls_until_the_run_stops() {
        use crate::model::RunStatus;

        let source = ScriptedRuns::new(
            vec![
                scripted_run(RunStatus::InProgress, None),
                scripted_run(RunStatus::InProgress, None),
                scripted_run(RunStatus::Completed, Some(RunResult::Failed)),
            ],
            Vec::new(),
            Vec::new(),
        );
        let polls = Arc::clone(&source.polls);
        let rested = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&rested);
        let rest = move |wait: Duration| rested.lock().unwrap().push(wait);

        let run = wait_for_run(&source, 14, &rest).unwrap();

        assert_eq!(run.result, Some(RunResult::Failed));
        assert_eq!(
            *polls.lock().unwrap(),
            3,
            "it asked until the answer changed"
        );
        assert_eq!(
            *recorder.lock().unwrap(),
            vec![LIVE_RUNS_CADENCE, LIVE_RUNS_CADENCE],
            "and waited the watcher's own cadence between asks"
        );
    }

    #[test]
    fn a_followed_log_prints_what_is_new_until_the_node_finishes() {
        use crate::model::RunStatus;

        let source = ScriptedRuns::new(
            vec![scripted_run(
                RunStatus::Completed,
                Some(RunResult::Succeeded),
            )],
            vec![
                vec![scripted_node("Build", RunStatus::InProgress)],
                vec![scripted_node("Build", RunStatus::InProgress)],
                vec![scripted_node("Build", RunStatus::Completed)],
            ],
            vec![
                vec!["one".to_owned()],
                vec!["one".to_owned(), "two".to_owned()],
                vec!["one".to_owned(), "two".to_owned(), "three".to_owned()],
            ],
        );
        let rested = Arc::new(Mutex::new(0usize));
        let counter = Arc::clone(&rested);
        let rest = move |_: Duration| *rested.lock().unwrap() += 1;

        print_log(&source, 14, Some("Build"), true, &rest).unwrap();

        assert_eq!(
            *counter.lock().unwrap(),
            2,
            "it rested between reads and stopped when the node did"
        );

        // Without --follow it prints once and returns, whatever the node is
        // doing.
        let source = ScriptedRuns::new(
            Vec::new(),
            vec![vec![scripted_node("Build", RunStatus::InProgress)]],
            vec![vec!["one".to_owned()]],
        );
        let rested = Arc::new(Mutex::new(0usize));
        let counter = Arc::clone(&rested);
        let rest = move |_: Duration| *rested.lock().unwrap() += 1;
        print_log(&source, 14, None, false, &rest).unwrap();
        assert_eq!(*counter.lock().unwrap(), 0);

        // A node nobody has is said rather than waited on for ever.
        let source = ScriptedRuns::new(
            Vec::new(),
            vec![vec![scripted_node("Build", RunStatus::Completed)]],
            Vec::new(),
        );
        let refused = print_log(&source, 14, Some("Deploy"), false, &|_| ()).unwrap_err();
        assert!(
            refused.to_string().contains("no node called Deploy"),
            "{refused}"
        );
    }

    #[test]
    fn following_with_no_node_named_waits_for_the_first_log_and_moves_on_to_the_next() {
        use crate::model::RunStatus;

        let mut deploy_running = scripted_node("Deploy", RunStatus::InProgress);
        deploy_running.log_id = Some(8);
        let mut deploy_done = scripted_node("Deploy", RunStatus::Completed);
        deploy_done.log_id = Some(8);
        let source = ScriptedRuns::new(
            Vec::new(),
            vec![
                // Just queued: nothing has written anything yet.
                Vec::new(),
                vec![scripted_node("Build", RunStatus::InProgress)],
                vec![scripted_node("Build", RunStatus::Completed), deploy_running],
                vec![scripted_node("Build", RunStatus::Completed), deploy_done],
            ],
            vec![
                vec!["building".to_owned()],
                vec!["building".to_owned()],
                vec!["deploying".to_owned()],
                vec!["deploying".to_owned()],
            ],
        );
        let rested = Arc::new(Mutex::new(0usize));
        let counter = Arc::clone(&rested);
        let rest = move |_: Duration| *rested.lock().unwrap() += 1;

        print_log(&source, 14, None, true, &rest).expect("a run with no log yet is waited for");

        assert_eq!(
            *counter.lock().unwrap(),
            3,
            "one wait for the first log, one while Build ran, one while Deploy ran"
        );

        // Without --follow, a run that has written nothing is said, not waited on.
        let source = ScriptedRuns::new(Vec::new(), vec![Vec::new()], Vec::new());
        let refused = print_log(&source, 14, None, false, &|_| ()).unwrap_err();
        assert!(refused.to_string().contains("no log yet"), "{refused}");
    }

    #[test]
    fn a_run_and_its_timeline_read_as_the_tab_draws_them() {
        use crate::model::RunStatus;

        let run = scripted_run(RunStatus::Completed, Some(RunResult::Succeeded));
        let timeline = vec![
            TimelineRecord {
                kind: TimelineKind::Stage,
                ..scripted_node("Build stage", RunStatus::Completed)
            },
            TimelineRecord {
                kind: TimelineKind::Job,
                ..scripted_node("Build job", RunStatus::Completed)
            },
            scripted_node("Compile", RunStatus::Completed),
        ];

        let text = describe_run(&run, "ticket-tui CI", &timeline);
        assert!(
            text.contains("\u{2713} run 14 · succeeded · 20260829.4"),
            "{text}"
        );
        assert!(text.contains("ticket-tui CI on main"), "{text}");
        assert!(text.contains("  \u{2713} Build stage  4m 12s"), "{text}");
        assert!(
            text.contains("    \u{2713} Build job"),
            "the job is indented under it: {text}"
        );
        assert!(
            text.contains("      \u{2713} Compile"),
            "and the task under that: {text}"
        );

        let json =
            serde_json::to_value(RunShowJson::new(&run, "ticket-tui CI", &timeline)).unwrap();
        assert_eq!(json["id"], 14);
        assert_eq!(json["pipeline"], "ticket-tui CI");
        assert_eq!(json["branch"], "main");
        assert_eq!(json["timeline"][0]["kind"], "stage");
        assert_eq!(json["timeline"][2]["name"], "Compile");
        assert_eq!(json["timeline"][2]["log_id"], 7);
    }

    #[test]
    fn runs_are_narrowed_by_pipeline_and_by_the_tab_grammar() {
        use crate::model::RunStatus;

        let rows = vec![
            RunRow {
                run: scripted_run(RunStatus::Completed, Some(RunResult::Succeeded)),
                pipeline: "ticket-tui CI".to_owned(),
            },
            RunRow {
                run: Run {
                    id: 15,
                    ..scripted_run(RunStatus::Completed, Some(RunResult::Failed))
                },
                pipeline: "nightly".to_owned(),
            },
        ];

        assert_eq!(
            filter_runs(rows.clone(), Some("nightly"), None, None)
                .iter()
                .map(|row| row.run.id)
                .collect::<Vec<_>>(),
            [15]
        );
        assert_eq!(
            filter_runs(rows.clone(), None, Some("result:failed"), None)
                .iter()
                .map(|row| row.run.id)
                .collect::<Vec<_>>(),
            [15]
        );
        assert!(
            filter_runs(
                rows.clone(),
                Some("nightly"),
                Some("result:succeeded"),
                None
            )
            .is_empty()
        );

        let table = tabulate_runs(&rows);
        assert!(
            table.starts_with("14  20260829.4  succeeded  main  ticket-tui CI"),
            "{table}"
        );
        assert_eq!(tabulate_runs(&[]), "no matching runs");

        let json = serde_json::to_value(RunJson::from(&rows[0])).unwrap();
        assert_eq!(json["pipeline"], "ticket-tui CI");
        assert_eq!(json["result"], "succeeded");
        assert_eq!(json["branch"], "main");
    }
}
