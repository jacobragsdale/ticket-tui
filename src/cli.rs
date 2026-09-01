//! The command line: the flags every run takes, and the subcommands that let
//! an agent read and change work items without opening the TUI.
//!
//! A bare invocation still opens the TUI, which is what `ticket-tui` has
//! always been. Every subcommand does one thing and exits. The reads answer
//! from SQLite and never touch the network — except `pods`, `acr` and the
//! vault commands (`vaults`, `secrets`, `keys`, `certs`), which
//! have no database to answer from and read the clusters through `kubectl`
//! and the subscription through ARM every time; the writes go out over the
//! same
//! trait-backed source the TUI's sync worker uses and store the copy Azure
//! DevOps answers with, so a running TUI picks the change up from the database
//! it is already watching.

use std::collections::BTreeMap;
use std::fs;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueHint};
use serde::Serialize;
use serde_json::Value;

use crate::aks::{KubeSource, Kubectl, PodRow, PodSchema};
use crate::app::acr::rows::short_digest;
use crate::app::pipelines::RunSchema;
use crate::app::pipelines::rows::{RunRow, duration_label, run_glyph, short_branch};
use crate::app::pull_requests::{PrRow, PrSchema};
use crate::app::repos::{RepoRow, RepoSchema};
use crate::arm::{
    ArmClient, ArmConfig, ArmSource, Inventory, ItemKind, Manifest, Registry, Repository, Secret,
    Tag, Vault, VaultItem, portal_url,
};
use crate::azure::{self, AzureClient, AzureConfig};
use crate::classification::{self, NodeKind};
use crate::config::{self, Config};
use crate::db::{self, SqliteTicketRepository, default_database_path};
use crate::edit::{FieldEdit, normalize_tags, revision_test};
use crate::filter::{FilterField, MatchContext, ParsedQuery, WorkItemSchema, parse_query};
use crate::kustomize;
use crate::kustomize::diff::{Entry, PromotionDiff, Side};
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
use crate::ui::acr::{count_label, platform_label};
use crate::ui::pipelines::{instant_label, relative_age};
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
    /// Azure subscription id for the ACR and Key Vault tabs, repeatable;
    /// defaults to TICKET_TUI_SUBSCRIPTION, then config.toml, then
    /// `az account show`
    #[arg(long, global = true, value_name = "SUBSCRIPTION")]
    pub subscription: Vec<String>,
    /// Seconds between background pulls from Azure DevOps, 0 to turn the timer
    /// off; defaults to TICKET_TUI_REFRESH or 60
    #[arg(long, value_name = "SECONDS")]
    pub refresh: Option<u64>,
    /// Extra WIQL condition ANDed into every pull, narrowing a large project;
    /// defaults to TICKET_TUI_QUERY, then config.toml
    #[arg(long, value_name = "WIQL")]
    pub query: Option<String>,
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
    /// The registries worth listing, in this order, as `config.toml` names
    /// them. There is no flag for it: it is a standing choice about a
    /// workplace rather than something one invocation says.
    #[arg(skip)]
    pub registries: Vec<String>,
    /// The vaults worth listing, in this order, as `config.toml` names them.
    #[arg(skip)]
    pub vaults: Vec<String>,
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
        // The variable names one subscription; a list of them is the file's to
        // hold.
        if self.subscription.is_empty() {
            self.subscription = variable("TICKET_TUI_SUBSCRIPTION")
                .map(|one| vec![one])
                .unwrap_or_else(|| file.azure.subscriptions.clone());
        }
        self.workspace = self
            .workspace
            .or_else(|| variable("TICKET_TUI_WORKSPACE").map(PathBuf::from))
            .or_else(|| file.devops.workspace.clone());
        self.registries = file.azure.registries.clone();
        self.vaults = file.azure.vaults.clone();
        self
    }

    /// What the ACR and Key Vault tabs read: the subscriptions settled above,
    /// and which of the registries and vaults in them are worth listing. An
    /// empty subscription list is the Azure CLI's to answer, later and
    /// elsewhere.
    #[must_use]
    pub fn arm_config(&self) -> ArmConfig {
        ArmConfig {
            subscriptions: self.subscription.clone(),
            registries: self.registries.clone(),
            vaults: self.vaults.clone(),
        }
    }
}

/// One thing to do and then exit. The flags above still apply: `--database`,
/// `--org`, `--project`, `--code-project` and `--subscription` may be written
/// either side of the subcommand, and what none of them says `config.toml`
/// answers for.
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
    /// Print the pods of every cluster config.toml names, read live through
    /// kubectl
    Pods(PodsCommand),
    /// Read the subscription's container registries, and what is in them
    #[command(subcommand)]
    Acr(AcrCommand),
    /// Read the subscription's key vaults
    #[command(subcommand)]
    Vaults(VaultsCommand),
    /// Read one vault's secrets, and one secret's value on request
    #[command(subcommand)]
    Secrets(SecretsCommand),
    /// Read one vault's keys
    #[command(subcommand)]
    Keys(KeysCommand),
    /// Read one vault's certificates
    #[command(subcommand)]
    Certs(CertsCommand),
    /// Print the tab badges as one line, for a status bar or a shell prompt
    Status {
        /// Print every figure as a JSON object, zeros and all, rather than as
        /// the line
        #[arg(long)]
        json: bool,
    },
    /// Read what the deployment repository's kustomize overlays declare, and
    /// check an environment against itself
    #[command(subcommand)]
    Env(EnvCommand),
}

/// The environments board, without the board. Nothing here opens the database
/// or touches the network: the deployment clone the Repos tab already knows
/// about is rendered with `kubectl kustomize` and read for names, so a check
/// answers offline and before the merge.
#[derive(Clone, Debug, Subcommand)]
pub enum EnvCommand {
    /// Print the environments config.toml names, and whether each rendered
    List {
        #[arg(long)]
        json: bool,
    },
    /// Print what one environment declares: its workloads with their images and
    /// references, its ConfigMaps and Secrets by key count, and the vault
    /// objects its providers pull
    Show {
        /// The environment, as config.toml names it
        environment: String,
        #[arg(long)]
        json: bool,
    },
    /// Report every ConfigMap and Secret reference an environment's own
    /// overlays do not define. Exits 1 when there is any finding, 0 when every
    /// environment is clean, and 2 when an overlay would not render
    Check {
        /// The environments to check; none named is all of them
        environments: Vec<String>,
        #[arg(long)]
        json: bool,
        /// Skip the vault half: report what the repository alone answers for,
        /// without reading any key vault
        #[arg(long)]
        offline: bool,
    },
    /// Report what one environment has that another has not, read left to
    /// right: the image tag and the pull requests and work items behind it,
    /// the ConfigMap and Secret keys, the vault objects and the variables
    Diff {
        /// The environment promoted from, as config.toml names it
        from: String,
        /// The environment promoted into
        to: String,
        /// One service, by the workload's name; none named is every one of them
        service: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

/// The ACR tab, without the tab. There is no database behind this one either:
/// the subscription is asked for its registries on every invocation, and a
/// registry's own data plane answers for the repositories and tags inside it.
#[derive(Clone, Debug, Subcommand)]
pub enum AcrCommand {
    /// Print the container registries the subscription holds
    List {
        #[arg(long)]
        json: bool,
    },
    /// Print one registry's fields, the way into it in the portal, and how
    /// many repositories its catalog holds
    Show {
        /// The registry's name, as the subscription spells it
        registry: String,
        #[arg(long)]
        json: bool,
    },
    /// Read the repositories one registry holds
    #[command(subcommand)]
    Repos(AcrReposCommand),
    /// Read the tags of one repository
    #[command(subcommand)]
    Tags(AcrTagsCommand),
}

#[derive(Clone, Debug, Subcommand)]
pub enum AcrReposCommand {
    /// Print every repository in one registry, with the counts and the stamp
    /// its attributes carry
    List {
        /// The registry to read, by name
        #[arg(long, value_name = "NAME")]
        registry: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub enum AcrTagsCommand {
    /// Print one repository's tags, newest first
    List {
        /// The registry the repository is in, by name
        #[arg(long, value_name = "NAME")]
        registry: String,
        /// The repository, as the catalog spells it
        #[arg(long, value_name = "NAME")]
        repo: String,
        #[arg(long)]
        json: bool,
    },
    /// Print one tag, what it points at, and the reference that pulls it
    Show {
        /// The registry the repository is in, by name
        #[arg(long, value_name = "NAME")]
        registry: String,
        /// The repository, as the catalog spells it
        #[arg(long, value_name = "NAME")]
        repo: String,
        /// The tag, as the repository spells it
        tag: String,
        #[arg(long)]
        json: bool,
    },
}

/// The Key Vault tab, without the tab. No database backs this one either: the
/// subscription is asked for its vaults on every invocation, and a vault's own
/// data plane answers for the secrets, keys and certificates inside it. A
/// listing never carries a value, and nothing here reads one unless it is
/// asked for in as many words.
#[derive(Clone, Debug, Subcommand)]
pub enum VaultsCommand {
    /// Print the key vaults the subscription holds
    List {
        #[arg(long)]
        json: bool,
    },
    /// Print one vault's fields, the way into it in the portal, and how many
    /// of each thing it holds
    Show {
        /// The vault's name, as the subscription spells it
        vault: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub enum SecretsCommand {
    /// Print every secret in one vault, without any of their values
    List {
        /// The vault to read, by name
        #[arg(long, value_name = "NAME")]
        vault: String,
        #[arg(long)]
        json: bool,
    },
    /// Print what one secret's listing says about it, and its value only when
    /// asked
    Show {
        /// The vault the secret is in, by name
        #[arg(long, value_name = "NAME")]
        vault: String,
        /// The secret, as the vault spells it
        name: String,
        #[arg(long)]
        json: bool,
        /// Print the secret's value itself, raw, to stdout; nothing else is
        /// printed, and it cannot be combined with --json
        #[arg(long, conflicts_with = "json")]
        value: bool,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub enum KeysCommand {
    /// Print every key in one vault
    List {
        /// The vault to read, by name
        #[arg(long, value_name = "NAME")]
        vault: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub enum CertsCommand {
    /// Print every certificate in one vault, with how far off each expiry is
    List {
        /// The vault to read, by name
        #[arg(long, value_name = "NAME")]
        vault: String,
        #[arg(long)]
        json: bool,
    },
}

/// The four vault groups as one value. They are four top-level commands
/// because that is how an agent reaches for them, but they all read the same
/// inventory and the same listing, so one function runs the lot.
#[derive(Clone, Debug)]
pub enum VaultCliCommand {
    Vaults(VaultsCommand),
    Secrets(SecretsCommand),
    Keys(KeysCommand),
    Certs(CertsCommand),
}

/// The AKS tab, without the tab. There is no database behind this one: the
/// clusters in `config.toml` are read through `kubectl` on every invocation,
/// which is also why no repository is matched to a pod here — that lookup
/// wants the project's repositories, and this command does not open the
/// database.
#[derive(Args, Clone, Debug)]
pub struct PodsCommand {
    /// Only this cluster, by the name config.toml gives it
    #[arg(long, value_name = "NAME")]
    pub cluster: Option<String>,
    /// Only this namespace, whatever config.toml lists for the cluster
    #[arg(long, value_name = "NAME")]
    pub namespace: Option<String>,
    /// The AKS tab's grammar — cluster:, ns:, status:, owner:, node:, app:,
    /// repo: — anything else matches the name
    pub query: Option<String>,
    /// Print the pods as a JSON array rather than as a table
    #[arg(long)]
    pub json: bool,
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
        Command::Pods(command) => run_pods(command),
        Command::Acr(command) => run_acr(cli, command),
        Command::Vaults(command) => run_vaults(cli, &VaultCliCommand::Vaults(command.clone())),
        Command::Secrets(command) => run_vaults(cli, &VaultCliCommand::Secrets(command.clone())),
        Command::Keys(command) => run_vaults(cli, &VaultCliCommand::Keys(command.clone())),
        Command::Certs(command) => run_vaults(cli, &VaultCliCommand::Certs(command.clone())),
        Command::Status { json } => run_status(cli, &database, *json),
        Command::Env(command) => run_env(cli, &database, command),
    }
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
            format!("{FENCE}\n{}\n{FENCE}", self.text)
        } else {
            self.text.clone()
        }
    }
}

const FENCE: &str = "```";

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
    stdin
        .read_to_string(&mut raw)
        .context("failed to read the comment from standard input")?;
    let size = i64::try_from(raw.len()).unwrap_or(i64::MAX);
    if size > COMMENT_LIMIT {
        bail!(
            "that is a log, not a comment: {}. Pipe it through tail",
            size_label(size)
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

/// Every pod of every cluster `config.toml` names. A file that will not parse
/// is the error; a cluster that will not answer is one line on stderr and a
/// non-zero exit after the pods that did answer have been printed, because a
/// partial answer is still worth having.
fn run_pods(command: &PodsCommand) -> Result<()> {
    let config = config::load(&config::default_path())?;
    run_pods_with(&config, command, &Kubectl)
}

fn run_pods_with(config: &Config, command: &PodsCommand, source: &dyn KubeSource) -> Result<()> {
    if config.clusters.is_empty() {
        bail!("no clusters in config.toml; add a [[clusters]] table");
    }
    let clusters = match &command.cluster {
        Some(name) => vec![
            config
                .clusters
                .iter()
                .find(|cluster| same_text(&cluster.name, name))
                .with_context(|| format!("no cluster called {name} in config.toml"))?,
        ],
        None => config.clusters.iter().collect(),
    };
    let mut rows = Vec::new();
    let mut unreadable = 0usize;
    for cluster in clusters {
        let targets = command
            .namespace
            .as_deref()
            .map_or_else(|| cluster.targets(), |namespace| vec![Some(namespace)]);
        let mut failed = false;
        for namespace in targets {
            match source.pods(cluster, namespace) {
                // No repositories: this command does not open the database,
                // so no pod can be matched to one.
                Ok(pods) => rows.extend(pods.into_iter().map(|pod| PodRow::new(pod, &[]))),
                Err(error) => {
                    let message = format!("{error:#}");
                    eprintln!("{}: {message}", cluster.name);
                    failed = true;
                    // A server that refused one namespace will answer for the
                    // next; one that could not be reached will not, and is not
                    // asked again — the worker's own rule.
                    if !message.starts_with("Error from server") {
                        break;
                    }
                }
            }
        }
        if failed {
            unreadable += 1;
        }
    }
    let rows = filter_pods(rows, command.query.as_deref());
    let now = Timestamp::now();
    emit(&if command.json {
        to_json(
            &rows
                .iter()
                .map(|row| PodJson::new(row, now))
                .collect::<Vec<_>>(),
        )?
    } else {
        tabulate_pods(&rows, now)
    });
    if unreadable > 0 {
        bail!("{unreadable} cluster(s) could not be read");
    }
    Ok(())
}

/// The rows one query names, in the order the clusters were read.
fn filter_pods(rows: Vec<PodRow>, query: Option<&str>) -> Vec<PodRow> {
    let Some(query) = query else {
        return rows;
    };
    let parsed = parse_query::<PodSchema>(query);
    let context = MatchContext::now();
    rows.into_iter()
        .filter(|row| {
            parsed.filters.matches_in(row, false, &context) && row.matches_fuzzy(&parsed.fuzzy)
        })
        .collect()
}

/// `cluster · namespace · name · ready · status · restarts · age`, the table
/// the tab draws, in the order `kubectl get pods` would print it.
fn tabulate_pods(rows: &[PodRow], now: Timestamp) -> String {
    if rows.is_empty() {
        return "no matching pods".to_owned();
    }
    columns(
        &rows
            .iter()
            .map(|row| {
                vec![
                    row.pod.key.cluster.clone(),
                    row.pod.key.namespace.clone(),
                    row.pod.key.name.clone(),
                    row.pod.ready_label(),
                    row.pod.status.clone(),
                    row.pod.restarts.to_string(),
                    pod_age(row, now).unwrap_or_else(|| "\u{2014}".to_owned()),
                ]
            })
            .collect::<Vec<_>>(),
    )
}

/// How long the pod has been up, in the tab's own words. A pod whose creation
/// stamp `kubectl` did not report has no age rather than an age of zero.
fn pod_age(row: &PodRow, now: Timestamp) -> Option<String> {
    row.pod.created.map(|created| relative_age(created, now))
}

/// One pod as `--json` prints it: everything the details pane shows, so an
/// agent reading this need not open the TUI.
#[derive(Serialize)]
struct PodJson<'a> {
    cluster: &'a str,
    namespace: &'a str,
    name: &'a str,
    status: &'a str,
    ready: String,
    restarts: u32,
    created: Option<String>,
    age: Option<String>,
    node: &'a str,
    ip: &'a str,
    owner: Option<String>,
    containers: Vec<ContainerJson<'a>>,
    labels: BTreeMap<&'a str, &'a str>,
}

#[derive(Serialize)]
struct ContainerJson<'a> {
    name: &'a str,
    image: &'a str,
    ready: bool,
    restarts: u32,
    state: &'a str,
}

impl<'a> PodJson<'a> {
    fn new(row: &'a PodRow, now: Timestamp) -> Self {
        let pod = &row.pod;
        Self {
            cluster: &pod.key.cluster,
            namespace: &pod.key.namespace,
            name: &pod.key.name,
            status: &pod.status,
            ready: pod.ready_label(),
            restarts: pod.restarts,
            created: pod.created.map(Timestamp::to_rfc3339),
            age: pod_age(row, now),
            node: &pod.node,
            ip: &pod.ip,
            owner: pod
                .owner
                .as_ref()
                .map(|(kind, name)| format!("{kind}/{name}")),
            containers: pod
                .containers
                .iter()
                .map(|container| ContainerJson {
                    name: &container.name,
                    image: &container.image,
                    ready: container.ready,
                    restarts: container.restarts,
                    state: &container.state,
                })
                .collect(),
            labels: pod
                .labels
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect(),
        }
    }
}

/// The registries the subscription holds, and what is inside one of them. The
/// subscription is resolved the way the tabs resolve it, and an unresolved one
/// or a refused token is the error rather than an empty listing: there is
/// nothing stored here to fall back on.
fn run_acr(cli: &Cli, command: &AcrCommand) -> Result<()> {
    run_acr_with(command, &ArmClient::new(cli.arm_config().resolve()?))
}

fn run_acr_with(command: &AcrCommand, source: &dyn ArmSource) -> Result<()> {
    let inventory = source.inventory()?;
    let now = Timestamp::now();
    match command {
        AcrCommand::List { json } => {
            emit(&if *json {
                to_json(
                    &inventory
                        .registries
                        .iter()
                        .map(RegistryJson::new)
                        .collect::<Vec<_>>(),
                )?
            } else {
                tabulate_registries(&inventory.registries)
            });
            Ok(())
        }
        AcrCommand::Show { registry, json } => {
            let registry = find_registry(&inventory, registry)?;
            let repositories = source.repositories(registry)?.len();
            emit(&if *json {
                to_json(&RegistryJson::with_catalog(registry, repositories))?
            } else {
                describe_registry(registry, repositories)
            });
            Ok(())
        }
        AcrCommand::Repos(AcrReposCommand::List { registry, json }) => {
            let registry = find_registry(&inventory, registry)?;
            let (rows, unreadable) = attributed_repositories(source, registry)?;
            emit(&if *json {
                to_json(&rows.iter().map(RepositoryJson::new).collect::<Vec<_>>())?
            } else {
                tabulate_repositories(&rows, now)
            });
            if unreadable > 0 {
                bail!("{unreadable} repository(s) could not be read");
            }
            Ok(())
        }
        AcrCommand::Tags(AcrTagsCommand::List {
            registry,
            repo,
            json,
        }) => {
            let registry = find_registry(&inventory, registry)?;
            let tags = source.tags(registry, repo)?;
            emit(&if *json {
                to_json(&tags.iter().map(TagJson::new).collect::<Vec<_>>())?
            } else {
                tabulate_tags(&tags, now)
            });
            Ok(())
        }
        AcrCommand::Tags(AcrTagsCommand::Show {
            registry,
            repo,
            tag,
            json,
        }) => {
            let registry = find_registry(&inventory, registry)?;
            let found = source
                .tags(registry, repo)?
                .into_iter()
                .find(|held| same_text(&held.name, tag))
                .with_context(|| format!("no tag called {tag} on {repo} in {}", registry.name))?;
            let manifest = source.manifest(registry, repo, &found.digest)?;
            let pull = format!("{}/{repo}:{}", registry.login_server, found.name);
            emit(&if *json {
                to_json(&TagShowJson {
                    tag: TagJson::new(&found),
                    pull,
                    manifest: ManifestJson::new(&manifest),
                })?
            } else {
                describe_tag(&found, &manifest, &pull, now)
            });
            Ok(())
        }
    }
}

/// The registry one name means, matched the way every other name on this
/// command line is: ignoring case.
fn find_registry<'a>(inventory: &'a Inventory, name: &str) -> Result<&'a Registry> {
    let Some(first) = inventory.registries.first() else {
        bail!("no container registries in this subscription");
    };
    inventory
        .registries
        .iter()
        .find(|registry| same_text(&registry.name, name))
        .with_context(|| {
            // Nothing on the source says which subscription answered, but
            // every resource it named carries it: `/subscriptions/<id>/…`.
            let subscription = first.id.split('/').nth(2).unwrap_or("unknown");
            format!("no registry called {name} in subscription {subscription}")
        })
}

/// The catalog, then one attributes read per name. A repository that refuses
/// is one line on stderr and a row with its counts still empty, so a listing
/// one repository would not answer for is still worth printing; the count of
/// refusals is the caller's to exit on.
fn attributed_repositories(
    source: &dyn ArmSource,
    registry: &Registry,
) -> Result<(Vec<Repository>, usize)> {
    let mut rows = Vec::new();
    let mut unreadable = 0usize;
    for listed in source.repositories(registry)? {
        match source.repository(registry, &listed.name) {
            Ok(filled) => rows.push(filled),
            Err(error) => {
                eprintln!("{}: {error:#}", listed.name);
                unreadable += 1;
                rows.push(listed);
            }
        }
    }
    Ok((rows, unreadable))
}

/// `name · resource group · sku · location · login server`, the registry table
/// the tab draws with its hidden column shown.
fn tabulate_registries(registries: &[Registry]) -> String {
    if registries.is_empty() {
        return "no container registries in this subscription".to_owned();
    }
    columns(
        &registries
            .iter()
            .map(|registry| {
                vec![
                    registry.name.clone(),
                    registry.resource_group.clone(),
                    registry.sku.clone(),
                    registry.location.clone(),
                    registry.login_server.clone(),
                ]
            })
            .collect::<Vec<_>>(),
    )
}

/// `repository · tags · manifests · updated`. A count nobody could read is a
/// dash rather than a nought, the way the tab draws one that has not landed.
fn tabulate_repositories(rows: &[Repository], now: Timestamp) -> String {
    if rows.is_empty() {
        return "no repositories in this registry".to_owned();
    }
    columns(
        &rows
            .iter()
            .map(|row| {
                vec![
                    row.name.clone(),
                    count_label(row.tags),
                    count_label(row.manifests),
                    instant_label(row.updated, now),
                ]
            })
            .collect::<Vec<_>>(),
    )
}

/// `tag · digest · created`, newest first because that is the order the
/// registry lists them in.
fn tabulate_tags(tags: &[Tag], now: Timestamp) -> String {
    if tags.is_empty() {
        return "no tags on this repository".to_owned();
    }
    columns(
        &tags
            .iter()
            .map(|tag| {
                vec![
                    tag.name.clone(),
                    short_digest(&tag.digest),
                    instant_label(tag.created, now),
                ]
            })
            .collect::<Vec<_>>(),
    )
}

/// One registry as a block of text, in the order the details pane lists it.
fn describe_registry(registry: &Registry, repositories: usize) -> String {
    [
        registry.name.clone(),
        registry.login_server.clone(),
        String::new(),
        format!("Group         {}", registry.resource_group),
        format!("Location      {}", registry.location),
        format!("SKU           {}", registry.sku),
        format!("Repositories  {repositories}"),
        String::new(),
        format!("Portal        {}", portal_url(&registry.id)),
    ]
    .join("\n")
}

/// One tag as a block of text: what it points at, when each end of that was
/// made, what the manifest weighs and runs on, and the reference that pulls
/// it.
fn describe_tag(tag: &Tag, manifest: &Manifest, pull: &str, now: Timestamp) -> String {
    [
        format!("{}  {}", tag.name, short_digest(&tag.digest)),
        String::new(),
        format!("Digest        {}", tag.digest),
        format!("Tagged        {}", instant_label(tag.created, now)),
        format!("Created       {}", instant_label(manifest.created, now)),
        format!("Platform      {}", platform_label(manifest)),
        format!(
            "Size          {}",
            manifest_size(manifest).unwrap_or_else(|| "\u{2014}".to_owned())
        ),
        String::new(),
        format!("Pull          {pull}"),
    ]
    .join("\n")
}

/// What a manifest weighs, in the units the tab writes it in.
fn manifest_size(manifest: &Manifest) -> Option<String> {
    manifest
        .size
        .map(|bytes| size_label(i64::try_from(bytes).unwrap_or(i64::MAX)))
}

/// One registry as `--json` prints it, portal link included so an agent need
/// not build one. The repository count is only there for `show`, which is the
/// one form that reads the catalog.
#[derive(Serialize)]
struct RegistryJson<'a> {
    name: &'a str,
    resource_group: &'a str,
    sku: &'a str,
    location: &'a str,
    login_server: &'a str,
    id: &'a str,
    portal_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    repositories: Option<usize>,
}

impl<'a> RegistryJson<'a> {
    fn new(registry: &'a Registry) -> Self {
        Self {
            name: &registry.name,
            resource_group: &registry.resource_group,
            sku: &registry.sku,
            location: &registry.location,
            login_server: &registry.login_server,
            id: &registry.id,
            portal_url: portal_url(&registry.id),
            repositories: None,
        }
    }

    fn with_catalog(registry: &'a Registry, repositories: usize) -> Self {
        Self {
            repositories: Some(repositories),
            ..Self::new(registry)
        }
    }
}

#[derive(Serialize)]
struct RepositoryJson<'a> {
    name: &'a str,
    tags: Option<u64>,
    manifests: Option<u64>,
    updated: Option<String>,
}

impl<'a> RepositoryJson<'a> {
    fn new(repository: &'a Repository) -> Self {
        Self {
            name: &repository.name,
            tags: repository.tags,
            manifests: repository.manifests,
            updated: repository.updated.map(Timestamp::to_rfc3339),
        }
    }
}

#[derive(Serialize)]
struct TagJson<'a> {
    name: &'a str,
    digest: &'a str,
    short_digest: String,
    created: Option<String>,
    updated: Option<String>,
}

impl<'a> TagJson<'a> {
    fn new(tag: &'a Tag) -> Self {
        Self {
            name: &tag.name,
            digest: &tag.digest,
            short_digest: short_digest(&tag.digest),
            created: tag.created.map(Timestamp::to_rfc3339),
            updated: tag.updated.map(Timestamp::to_rfc3339),
        }
    }
}

#[derive(Serialize)]
struct ManifestJson<'a> {
    digest: &'a str,
    platform: String,
    architecture: &'a str,
    os: &'a str,
    size: Option<u64>,
    size_label: Option<String>,
    created: Option<String>,
}

impl<'a> ManifestJson<'a> {
    fn new(manifest: &'a Manifest) -> Self {
        Self {
            digest: &manifest.digest,
            platform: platform_label(manifest),
            architecture: &manifest.architecture,
            os: &manifest.os,
            size: manifest.size,
            size_label: manifest_size(manifest),
            created: manifest.created.map(Timestamp::to_rfc3339),
        }
    }
}

/// One tag and everything `tags show` prints about it.
#[derive(Serialize)]
struct TagShowJson<'a> {
    tag: TagJson<'a>,
    pull: String,
    manifest: ManifestJson<'a>,
}

/// The vaults the subscription holds, and what is inside one of them. The
/// subscription is resolved the way the tabs resolve it, and an unresolved one
/// or a refused token is the error rather than an empty listing: there is
/// nothing stored here to fall back on.
fn run_vaults(cli: &Cli, command: &VaultCliCommand) -> Result<()> {
    run_vaults_with(command, &ArmClient::new(cli.arm_config().resolve()?))
}

fn run_vaults_with(command: &VaultCliCommand, source: &dyn ArmSource) -> Result<()> {
    let inventory = source.inventory()?;
    let now = Timestamp::now();
    match command {
        VaultCliCommand::Vaults(VaultsCommand::List { json }) => {
            emit(&if *json {
                to_json(
                    &inventory
                        .vaults
                        .iter()
                        .map(VaultJson::new)
                        .collect::<Vec<_>>(),
                )?
            } else {
                tabulate_vaults(&inventory.vaults)
            });
            Ok(())
        }
        VaultCliCommand::Vaults(VaultsCommand::Show { vault, json }) => {
            let vault = find_vault(&inventory, vault)?;
            let items = source.items(vault)?;
            emit(&if *json {
                to_json(&VaultJson::with_items(vault, &items))?
            } else {
                describe_vault(vault, &items)
            });
            Ok(())
        }
        VaultCliCommand::Secrets(SecretsCommand::List { vault, json }) => {
            list_items(source, &inventory, vault, ItemKind::Secret, *json, now)
        }
        VaultCliCommand::Keys(KeysCommand::List { vault, json }) => {
            list_items(source, &inventory, vault, ItemKind::Key, *json, now)
        }
        VaultCliCommand::Certs(CertsCommand::List { vault, json }) => {
            list_items(source, &inventory, vault, ItemKind::Certificate, *json, now)
        }
        VaultCliCommand::Secrets(SecretsCommand::Show {
            vault,
            name,
            json,
            value,
        }) => {
            let vault = find_vault(&inventory, vault)?;
            if *value {
                // The one read in this file that comes back holding something
                // worth hiding, and the one place it is printed. Nothing else
                // goes to stdout, so `$(…)` around this command is the value
                // and only the value.
                println!("{}", secret_output(&source.secret_value(vault, name)?));
                return Ok(());
            }
            let found = source
                .items(vault)?
                .into_iter()
                .find(|item| item.kind == ItemKind::Secret && same_text(&item.name, name))
                .with_context(|| format!("no secret called {name} in {}", vault.name))?;
            emit(&if *json {
                to_json(&VaultItemJson::new(&found))?
            } else {
                describe_item(&found, now)
            });
            Ok(())
        }
    }
}

/// One vault's items of one kind, which is all three listing commands.
fn list_items(
    source: &dyn ArmSource,
    inventory: &Inventory,
    vault: &str,
    kind: ItemKind,
    json: bool,
    now: Timestamp,
) -> Result<()> {
    let vault = find_vault(inventory, vault)?;
    let items = of_kind(&source.items(vault)?, kind);
    emit(&if json {
        to_json(&items.iter().map(VaultItemJson::new).collect::<Vec<_>>())?
    } else {
        tabulate_items(&items, kind, now)
    });
    Ok(())
}

/// The vault one name means, matched the way every other name on this command
/// line is: ignoring case.
fn find_vault<'a>(inventory: &'a Inventory, name: &str) -> Result<&'a Vault> {
    inventory
        .vaults
        .iter()
        .find(|vault| same_text(&vault.name, name))
        .with_context(|| format!("no vault called {name} in this subscription"))
}

/// The items of one kind, in the order the vault listed them.
fn of_kind(items: &[VaultItem], kind: ItemKind) -> Vec<VaultItem> {
    items
        .iter()
        .filter(|item| item.kind == kind)
        .cloned()
        .collect()
}

/// `name · resource group · location · sku · uri`.
fn tabulate_vaults(vaults: &[Vault]) -> String {
    if vaults.is_empty() {
        return "no key vaults in this subscription".to_owned();
    }
    columns(
        &vaults
            .iter()
            .map(|vault| {
                vec![
                    vault.name.clone(),
                    vault.resource_group.clone(),
                    vault.location.clone(),
                    vault.sku.clone(),
                    vault.uri.clone(),
                ]
            })
            .collect::<Vec<_>>(),
    )
}

/// `name · enabled · updated · expires`, and the content type after that for a
/// secret, which is the one kind that carries one. A certificate's expiry is
/// the thing a reader is usually after, so it gets the plain words as well.
fn tabulate_items(items: &[VaultItem], kind: ItemKind, now: Timestamp) -> String {
    if items.is_empty() {
        return format!("no {}s in this vault", kind.as_str());
    }
    columns(
        &items
            .iter()
            .map(|item| {
                let mut expires = instant_label(item.expires, now);
                if kind == ItemKind::Certificate
                    && let Some(words) = expiry_words(item.expires, now)
                {
                    expires.push(' ');
                    expires.push_str(&words);
                }
                let mut row = vec![
                    item.name.clone(),
                    enabled_label(item.enabled),
                    instant_label(item.updated, now),
                    expires,
                ];
                if kind == ItemKind::Secret {
                    row.push(or_dash(item.content_type.as_deref()));
                }
                row
            })
            .collect::<Vec<_>>(),
    )
}

/// One vault as a block of text, with how many of each thing it holds — which
/// is the count `show` reads the listing for.
fn describe_vault(vault: &Vault, items: &[VaultItem]) -> String {
    [
        vault.name.clone(),
        vault.uri.clone(),
        String::new(),
        format!("Group         {}", vault.resource_group),
        format!("Location      {}", vault.location),
        format!("SKU           {}", vault.sku),
        format!("Secrets       {}", of_kind(items, ItemKind::Secret).len()),
        format!("Keys          {}", of_kind(items, ItemKind::Key).len()),
        format!(
            "Certificates  {}",
            of_kind(items, ItemKind::Certificate).len()
        ),
        String::new(),
        format!("Portal        {}", portal_url(&vault.id)),
    ]
    .join("\n")
}

/// One item as everything its listing says about it, and a last line saying
/// what is deliberately missing.
fn describe_item(item: &VaultItem, now: Timestamp) -> String {
    [
        format!("{}  {}", item.name, item.kind.as_str()),
        String::new(),
        format!("Enabled       {}", enabled_label(item.enabled)),
        format!("Created       {}", instant_label(item.created, now)),
        format!("Updated       {}", instant_label(item.updated, now)),
        format!("Expires       {}", instant_label(item.expires, now)),
        format!("Content type  {}", or_dash(item.content_type.as_deref())),
        format!("Recovery      {}", or_dash(item.recovery_level.as_deref())),
        String::new(),
        "value: not shown; pass --value to print it".to_owned(),
    ]
    .join("\n")
}

/// How far off an expiry is, in words rather than a stamp. Nothing to say
/// about an item that never expires.
fn expiry_words(expires: Option<Timestamp>, now: Timestamp) -> Option<String> {
    let expires = expires?;
    let ahead = now.seconds_until(expires);
    Some(if ahead > 0 {
        format!("expires in {}", whole_days(ahead))
    } else {
        format!("expired {} ago", whole_days(expires.seconds_until(now)))
    })
}

/// Whole days, singular where it needs to be.
fn whole_days(seconds: i64) -> String {
    match seconds / 86_400 {
        1 => "1 day".to_owned(),
        days => format!("{days} days"),
    }
}

fn enabled_label(enabled: bool) -> String {
    if enabled { "yes" } else { "no" }.to_owned()
}

fn or_dash(text: Option<&str>) -> String {
    text.map_or_else(|| "\u{2014}".to_owned(), ToOwned::to_owned)
}

/// The value path, alone, so the one `expose` in this file has a name and a
/// test of its own. A [`Secret`] never reaches a format string anywhere else:
/// it would print `[redacted]`, which is not what the caller asked for either.
fn secret_output(secret: &Secret) -> String {
    secret.expose().to_owned()
}

/// One vault as `--json` prints it, portal link included so an agent need not
/// build one. The counts are only there for `show`, which is the one form that
/// reads the listing.
#[derive(Serialize)]
struct VaultJson<'a> {
    name: &'a str,
    resource_group: &'a str,
    location: &'a str,
    sku: &'a str,
    uri: &'a str,
    id: &'a str,
    portal_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    items: Option<ItemCountsJson>,
}

impl<'a> VaultJson<'a> {
    fn new(vault: &'a Vault) -> Self {
        Self {
            name: &vault.name,
            resource_group: &vault.resource_group,
            location: &vault.location,
            sku: &vault.sku,
            uri: &vault.uri,
            id: &vault.id,
            portal_url: portal_url(&vault.id),
            items: None,
        }
    }

    fn with_items(vault: &'a Vault, items: &[VaultItem]) -> Self {
        Self {
            items: Some(ItemCountsJson {
                secrets: of_kind(items, ItemKind::Secret).len(),
                keys: of_kind(items, ItemKind::Key).len(),
                certs: of_kind(items, ItemKind::Certificate).len(),
            }),
            ..Self::new(vault)
        }
    }
}

#[derive(Serialize)]
struct ItemCountsJson {
    secrets: usize,
    keys: usize,
    certs: usize,
}

/// One secret, key or certificate as `--json` prints it: everything the
/// listing carries, and nothing it does not. There is no field here for a
/// value, on purpose.
#[derive(Serialize)]
struct VaultItemJson<'a> {
    kind: &'static str,
    name: &'a str,
    enabled: bool,
    created: Option<String>,
    updated: Option<String>,
    expires: Option<String>,
    content_type: Option<&'a str>,
    recovery_level: Option<&'a str>,
}

impl<'a> VaultItemJson<'a> {
    fn new(item: &'a VaultItem) -> Self {
        Self {
            kind: item.kind.as_str(),
            name: &item.name,
            enabled: item.enabled,
            created: item.created.map(Timestamp::to_rfc3339),
            updated: item.updated.map(Timestamp::to_rfc3339),
            expires: item.expires.map(Timestamp::to_rfc3339),
            content_type: item.content_type.as_deref(),
            recovery_level: item.recovery_level.as_deref(),
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

/// The environments board's reads, without the board. `config.toml` says which
/// repository holds the overlays and what each environment is made of; the
/// clone is the one the Repos tab found in the workspace.
///
/// The render runs here in this process rather than on the local thread: this
/// is a command that does one thing and exits, and there is no edit for it to
/// hold up.
fn run_env(cli: &Cli, database: &Path, command: &EnvCommand) -> Result<()> {
    let config = config::load(&config::default_path())?;
    let workspace = local::workspace_root(cli.workspace.clone());
    run_env_with(cli, &config, workspace.as_deref(), database, command)
}

fn run_env_with(
    cli: &Cli,
    config: &Config,
    workspace: Option<&Path>,
    database: &Path,
    command: &EnvCommand,
) -> Result<()> {
    match command {
        EnvCommand::List { json } => run_env_list(config, workspace, *json),
        EnvCommand::Show { environment, json } => {
            let (clone, render) = deployment_render(config, workspace)?;
            let environment = named_environment(config, environment)?;
            let manifest = kustomize::manifest(&clone, environment, &render)?;
            emit(&if *json {
                to_json(&manifest)?
            } else {
                describe_environment(&manifest)
            });
            Ok(())
        }
        // The exit code is the point of this one — the deployment repository's
        // own pipeline runs it as a gate — so it ends the process itself
        // rather than leaving 1 to stand for everything.
        EnvCommand::Check {
            environments,
            json,
            offline,
        } => match run_env_check(cli, config, workspace, environments, *json, *offline) {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("error: {error:#}");
                std::process::exit(2)
            }
        },
        EnvCommand::Diff {
            from,
            to,
            service,
            json,
        } => run_env_diff(
            config,
            workspace,
            database,
            from,
            to,
            service.as_deref(),
            *json,
        ),
    }
}

/// Where the overlays are and what renders them, or why the feature is off.
/// The message says where it looked, the way the Repos tab does for a
/// workspace that is not there.
fn deployment_render(config: &Config, workspace: Option<&Path>) -> Result<(PathBuf, String)> {
    if config.environments.is_empty() {
        bail!("no environments in config.toml; add an [[environments]] table");
    }
    let clone = kustomize::deployment_clone(config, workspace)?;
    let render = config
        .deployment
        .as_ref()
        .and_then(|deployment| deployment.render.clone())
        .unwrap_or_else(|| kustomize::DEFAULT_RENDER.to_owned());
    Ok((clone, render))
}

/// One environment by name, matched ignoring case like every other name on this
/// command line.
fn named_environment<'a>(config: &'a Config, name: &str) -> Result<&'a config::Environment> {
    config
        .environments
        .iter()
        .find(|held| same_text(&held.name, name))
        .with_context(|| format!("no environment called {name} in config.toml"))
}

/// One environment and what its overlays declare, or why they would not
/// render.
type Rendered<'a> = (&'a config::Environment, Result<kustomize::EnvManifest>);

/// The environments named, rendered, each with what it declares or why it would
/// not render.
fn render_all<'a>(
    config: &'a Config,
    workspace: Option<&Path>,
    names: &[String],
) -> Result<Vec<Rendered<'a>>> {
    let (clone, render) = deployment_render(config, workspace)?;
    let chosen: Vec<&config::Environment> = if names.is_empty() {
        config.environments.iter().collect()
    } else {
        names
            .iter()
            .map(|name| named_environment(config, name))
            .collect::<Result<_>>()?
    };
    Ok(chosen
        .into_iter()
        .map(|environment| {
            let manifest = kustomize::manifest(&clone, environment, &render);
            (environment, manifest)
        })
        .collect())
}

/// `name overlays vault registry cluster rendered`, with the last column
/// saying what each environment holds or why it would not render.
fn run_env_list(config: &Config, workspace: Option<&Path>, json: bool) -> Result<()> {
    emit(&list_table(&render_all(config, workspace, &[])?, json)?);
    Ok(())
}

/// The table, pure over what the renders answered with.
fn list_table(rendered: &[Rendered<'_>], json: bool) -> Result<String> {
    if json {
        return to_json(
            &rendered
                .iter()
                .map(|(environment, manifest)| EnvListJson::new(environment, manifest.as_ref()))
                .collect::<Vec<_>>(),
        );
    }
    Ok(columns(
        &rendered
            .iter()
            .map(|(environment, manifest)| {
                vec![
                    environment.name.clone(),
                    environment.overlays.join(","),
                    or_dash(environment.vault.as_deref()),
                    or_dash(environment.registry.as_deref()),
                    or_dash(environment.cluster.as_deref()),
                    match manifest {
                        Ok(manifest) => counts(manifest),
                        Err(error) => format!("{error:#}"),
                    },
                ]
            })
            .collect::<Vec<_>>(),
    ))
}

/// Every finding of every environment named, one line each, and the exit code
/// the gate reads: 1 for any finding, 0 for clean, 2 for an overlay that would
/// not render.
fn run_env_check(
    cli: &Cli,
    config: &Config,
    workspace: Option<&Path>,
    names: &[String],
    json: bool,
    offline: bool,
) -> Result<i32> {
    let rendered = render_all(config, workspace, names)?;
    let vaults = vault_names(cli, &rendered, offline);
    let (report, code) = check_report(&rendered, json, &vaults)?;
    if !report.is_empty() {
        emit(&report);
    }
    Ok(code)
}

/// What each environment's vault holds, or the one reason there is nothing.
/// The vault half wants a token and the repository half does not, so a run
/// without one still answers for the overlays rather than refusing.
fn vault_names(
    cli: &Cli,
    rendered: &[Rendered<'_>],
    offline: bool,
) -> Result<Vec<kustomize::VaultNames>, String> {
    if offline {
        return Err("--offline".to_owned());
    }
    let mut wanted: Vec<&str> = rendered
        .iter()
        .filter_map(|(environment, _)| environment.vault.as_deref())
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let resolved = cli
        .arm_config()
        .resolve()
        .map_err(|error| format!("{error:#}"))?;
    read_vaults(&ArmClient::new(resolved), &wanted, Timestamp::now())
        .map_err(|error| format!("{error:#}"))
}

/// One listing per vault the environments name, which is the pair of calls
/// `secrets list` makes. Names and dates come back; a value never does.
fn read_vaults(
    source: &dyn ArmSource,
    wanted: &[&str],
    now: Timestamp,
) -> Result<Vec<kustomize::VaultNames>> {
    let inventory = source.inventory()?;
    wanted
        .iter()
        .map(|name| {
            let vault = find_vault(&inventory, name)?;
            Ok(kustomize::VaultNames::from_items(
                &vault.name,
                &source.items(vault)?,
                now,
            ))
        })
        .collect()
}

/// What the check prints and the code it exits with, pure over what the
/// renders answered with. An overlay that would not render is one line on
/// stderr, the way an unreadable cluster is under `pods`, and it takes the
/// exit code with it: a gate that could not look must not read as clean. A
/// vault that could not be read is one line on stderr too, and takes only its
/// own half with it.
fn check_report(
    rendered: &[Rendered<'_>],
    json: bool,
    vaults: &Result<Vec<kustomize::VaultNames>, String>,
) -> Result<(String, i32)> {
    // A vault half that could not be read is one line on stderr rather than a
    // finding per object, and it does not touch the exit code: an unread vault
    // says nothing about the overlays, which still answer for themselves.
    if let Err(reason) = vaults {
        eprintln!("vault check skipped: {reason}");
    }
    let mut lines: Vec<String> = Vec::new();
    let mut reports: Vec<EnvCheckJson> = Vec::new();
    let (mut found, mut unrenderable) = (false, false);
    for (environment, manifest) in rendered {
        let manifest = match manifest {
            Ok(manifest) => manifest,
            Err(error) => {
                eprintln!("{}: {error:#}", environment.name);
                unrenderable = true;
                continue;
            }
        };
        let vault = vaults.as_ref().ok().and_then(|held| {
            let wanted = environment.vault.as_deref()?;
            held.iter().find(|names| same_text(&names.vault, wanted))
        });
        let findings = kustomize::check_with(manifest, vault);
        found |= !findings.is_empty();
        if json {
            reports.push(EnvCheckJson::new(manifest, findings));
        } else if findings.is_empty() {
            lines.push(format!(
                "{}: clean ({}, {})",
                environment.name,
                plural(manifest.workloads.len(), "workload"),
                plural(manifest.config_maps.len(), "configmap")
            ));
        } else {
            lines.extend(findings.iter().map(ToString::to_string));
        }
    }
    let text = if json {
        to_json(&reports)?
    } else {
        lines.join("\n")
    };
    Ok((text, if unrenderable { 2 } else { i32::from(found) }))
}

/// One environment as a block of text: what it holds, then its workloads with
/// every reference each makes, its ConfigMaps and Secrets by key count, and the
/// vault objects its providers pull. Names throughout; no value is in the model
/// to print.
fn describe_environment(manifest: &kustomize::EnvManifest) -> String {
    let mut lines = vec![
        format!("{} \u{b7} {}", manifest.environment, counts(manifest)),
        manifest.overlays.join(" "),
    ];
    if !manifest.workloads.is_empty() {
        lines.push(String::new());
        lines.push("Workloads".to_owned());
    }
    for workload in &manifest.workloads {
        lines.push(format!(
            "  {} {}",
            workload.kind,
            place(&workload.namespace, &workload.name)
        ));
        for container in &workload.containers {
            lines.push(format!(
                "    {}{}  {}",
                container.name,
                if container.init { " (init)" } else { "" },
                container.image
            ));
            lines.extend(
                container
                    .references
                    .iter()
                    .map(|reference| format!("      {reference}")),
            );
        }
        lines.extend(
            workload
                .volumes
                .iter()
                .map(|reference| format!("    {reference}")),
        );
    }
    for (heading, objects) in [
        ("ConfigMaps", &manifest.config_maps),
        ("Secrets", &manifest.secrets),
    ] {
        if objects.is_empty() {
            continue;
        }
        lines.push(String::new());
        lines.push(heading.to_owned());
        lines.extend(objects.iter().map(|object| {
            format!(
                "  {}  {}",
                place(&object.namespace, &object.name),
                plural(object.keys.len(), "key")
            )
        }));
    }
    if !manifest.providers.is_empty() {
        lines.push(String::new());
        lines.push("Providers".to_owned());
    }
    for provider in &manifest.providers {
        lines.push(format!(
            "  {} {} \u{b7} {}",
            provider.kind,
            place(&provider.namespace, &provider.name),
            provider.vault.as_deref().unwrap_or("\u{2014}")
        ));
        lines.push(format!(
            "    objects   {}",
            provider
                .objects
                .iter()
                .map(|object| object.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        for produced in &provider.produces {
            lines.push(format!(
                "    produces  secret {}  {}",
                produced.name,
                if provider.whole {
                    "whole (dataFrom)".to_owned()
                } else {
                    produced.keys.join(", ")
                }
            ));
        }
    }
    lines.join("\n")
}

/// `12 workloads, 9 configmaps, 2 secrets, 1 provider`.
fn counts(manifest: &kustomize::EnvManifest) -> String {
    [
        plural(manifest.workloads.len(), "workload"),
        plural(manifest.config_maps.len(), "configmap"),
        plural(manifest.secrets.len(), "secret"),
        plural(manifest.providers.len(), "provider"),
    ]
    .join(", ")
}

/// `1 workload`, `12 workloads`.
fn plural(count: usize, thing: &str) -> String {
    format!("{count} {thing}{}", if count == 1 { "" } else { "s" })
}

/// `namespace/name`, or just the name where the render gave it no namespace.
fn place(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_owned()
    } else {
        format!("{namespace}/{name}")
    }
}

/// One environment as `env list --json` prints it.
#[derive(Serialize)]
struct EnvListJson<'a> {
    name: &'a str,
    overlays: &'a [String],
    vault: Option<&'a str>,
    registry: Option<&'a str>,
    cluster: Option<&'a str>,
    rendered: bool,
    workloads: usize,
    config_maps: usize,
    secrets: usize,
    providers: usize,
    /// Why it would not render, when it would not.
    error: Option<String>,
}

impl<'a> EnvListJson<'a> {
    fn new(
        environment: &'a config::Environment,
        manifest: Result<&kustomize::EnvManifest, &anyhow::Error>,
    ) -> Self {
        Self {
            name: &environment.name,
            overlays: &environment.overlays,
            vault: environment.vault.as_deref(),
            registry: environment.registry.as_deref(),
            cluster: environment.cluster.as_deref(),
            rendered: manifest.is_ok(),
            workloads: manifest.map_or(0, |held| held.workloads.len()),
            config_maps: manifest.map_or(0, |held| held.config_maps.len()),
            secrets: manifest.map_or(0, |held| held.secrets.len()),
            providers: manifest.map_or(0, |held| held.providers.len()),
            error: manifest.err().map(|error| format!("{error:#}")),
        }
    }
}

/// One environment as `env check --json` prints it: the counts a clean run
/// reports, and every finding.
#[derive(Serialize)]
struct EnvCheckJson<'a> {
    environment: &'a str,
    workloads: usize,
    config_maps: usize,
    clean: bool,
    findings: Vec<kustomize::Finding>,
}

impl<'a> EnvCheckJson<'a> {
    fn new(manifest: &'a kustomize::EnvManifest, findings: Vec<kustomize::Finding>) -> Self {
        Self {
            environment: &manifest.environment,
            workloads: manifest.workloads.len(),
            config_maps: manifest.config_maps.len(),
            clean: findings.is_empty(),
            findings,
        }
    }
}

/// The promotion diff: what one environment has that another has not, read
/// left to right. The two rendered manifests answer for the keys, the vault
/// objects and the variables; the image gap is read back through the runs and
/// pull requests SQLite holds and the service's own clone, and each of those
/// three that is not there is a line rather than a failure — a diff of two
/// overlays is worth having offline.
fn run_env_diff(
    config: &Config,
    workspace: Option<&Path>,
    database: &Path,
    from: &str,
    to: &str,
    service: Option<&str>,
    json: bool,
) -> Result<()> {
    let (clone, render) = deployment_render(config, workspace)?;
    let from = named_environment(config, from)?;
    let to = named_environment(config, to)?;
    let mut diff = kustomize::diff::diff(
        &kustomize::manifest(&clone, from, &render)?,
        &kustomize::manifest(&clone, to, &render)?,
        service,
    );
    read_history(&mut diff, database, workspace);
    emit(&if json {
        to_json(&diff)?
    } else {
        describe_diff(&diff)
    });
    Ok(())
}

/// Every image gap read back to the two runs that built it, the pull requests
/// merged between their commits and the work items those close. A database
/// that is not there leaves every gap unread, which is the tags on their own.
fn read_history(diff: &mut PromotionDiff, database: &Path, workspace: Option<&Path>) {
    let Ok(stored) = open_database(database) else {
        return;
    };
    let (Ok(runs), Ok(requests), Ok(repos), Ok(pipelines)) = (
        stored.load_runs(),
        stored.load_pull_requests(),
        stored.load_repos(),
        stored.load_pipelines(),
    ) else {
        return;
    };
    for service in &mut diff.services {
        let workload = service.workload.clone();
        for change in &mut service.images {
            if change.from.is_none() || change.to.is_none() {
                continue;
            }
            // The repository is wanted before the history is, to find the
            // clone with, so the `from` tag is read back here as well.
            let built = change
                .from
                .as_ref()
                .and_then(|tag| kustomize::diff::read_back(tag, &runs, None));
            let repo = service_repo(&repos, &pipelines, &workload, built.as_ref());
            let clone = service_clone(workspace, repo, &workload);
            let log = |older: &str, newer: &str| -> Result<Vec<String>, String> {
                git_log(clone.as_ref().map_err(String::clone)?, older, newer)
            };
            let history = kustomize::diff::history(
                change,
                &runs,
                &requests,
                repo.map(|repo| repo.id.as_str()),
                // Rule three wants the registry's own
                // `org.opencontainers.image.revision`, which is an ACR read
                // this offline command does not make.
                &|_| None,
                &log,
            );
            change.history = Some(history);
        }
    }
}

/// The repository one service's history is in: the one the workload is named
/// after, else the one the run's pipeline builds.
fn service_repo<'a>(
    repos: &'a [crate::model::Repo],
    pipelines: &[Pipeline],
    workload: &str,
    built: Option<&kustomize::diff::ImageRun>,
) -> Option<&'a crate::model::Repo> {
    repos
        .iter()
        .find(|repo| same_text(&repo.name, workload))
        .or_else(|| {
            let built = built?;
            let pipeline = pipelines.iter().find(|held| held.id == built.pipeline_id)?;
            let id = pipeline.repo_id.as_deref()?;
            repos.iter().find(|repo| repo.id == id)
        })
}

/// Where that repository is cloned, or the line the block prints instead of a
/// list of pull requests.
fn service_clone(
    workspace: Option<&Path>,
    repo: Option<&crate::model::Repo>,
    workload: &str,
) -> Result<PathBuf, String> {
    let missing = |name: &str| format!("no clone of {name} to read the history from");
    let repo = repo.ok_or_else(|| missing(workload))?;
    let workspace = workspace.ok_or_else(|| missing(&repo.name))?;
    local::scan(
        workspace,
        &[local::RepoKey {
            id: repo.id.clone(),
            remote: local::normalise_remote(&repo.remote_url),
            name: repo.name.clone(),
        }],
    )
    .into_iter()
    .next()
    .map(|(_, local)| local.path)
    .ok_or_else(|| missing(&repo.name))
}

/// `git log <older>..<newer>` in one clone, a line each of commit and subject.
/// Whatever git would not do comes back as its own first word on the matter,
/// the way every other shelled-out command here does.
fn git_log(clone: &Path, older: &str, newer: &str) -> Result<Vec<String>, String> {
    local::git(
        clone,
        &["log", "--format=%H %s", &format!("{older}..{newer}")],
    )
    .map(|output| output.lines().map(str::to_owned).collect())
    .map_err(|error| {
        let error = format!("{error:#}");
        error
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("git would not read the history")
            .trim()
            .to_owned()
    })
}

/// One block per service, and `identical` when the two environments say the
/// same thing about every one of them. Direction is left to right: what the
/// target has and the source has not is marked `only in <environment>`.
fn describe_diff(diff: &PromotionDiff) -> String {
    if diff.services.is_empty() {
        return format!("{} \u{2192} {}: identical", diff.from, diff.to);
    }
    let mark = |entry: &Entry| match entry.side {
        Side::From => String::new(),
        side => format!("  only in {}", diff.environment(side)),
    };
    let mut lines: Vec<String> = Vec::new();
    for service in &diff.services {
        lines.push(match service.only_in {
            Some(side) => format!(
                "{} {}  only in {}",
                service.workload,
                service.kind,
                diff.environment(side)
            ),
            None => format!("{} {}", service.workload, service.kind),
        });
        for image in &service.images {
            lines.push(format!(
                "  image   {}  {} \u{2192} {}{}",
                image.container,
                image.from.as_deref().unwrap_or("\u{2014}"),
                image.to.as_deref().unwrap_or("\u{2014}"),
                image
                    .history
                    .as_ref()
                    .map(|history| format!("  {history}"))
                    .unwrap_or_default()
            ));
        }
        for (label, entries) in [
            ("key", &service.keys),
            ("secret", &service.vault_objects),
            ("var", &service.variables),
        ] {
            lines.extend(entries.iter().map(|entry| {
                format!(
                    "  {label:<6}  {} {}{}",
                    entry.object,
                    entry.name,
                    mark(entry)
                )
            }));
        }
    }
    lines.join("\n")
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
    use crate::aks::tests as aks_tests;
    use crate::arm::tests as arm_tests;
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
        let file = config::parse(
            "[devops]\norg = \"file-org\"\nproject = \"ISTO\"\ncode_project = \"Fiquants\"\n\
             query = \"[System.Id] > 1\"\nworkspace = \"/srv/code\"\n\n[azure]\n\
             subscriptions = [\"file-sub\"]\nregistries = [\"acrdev\"]\nvaults = [\"kv-dev\"]\n",
        )
        .unwrap();
        let env = |key: &str| match key {
            "TICKET_TUI_PROJECT" => Some("env-project".to_owned()),
            "TICKET_TUI_CODE_PROJECT" => Some("env-code".to_owned()),
            "TICKET_TUI_QUERY" => Some("env query".to_owned()),
            "TICKET_TUI_SUBSCRIPTION" => Some("env-sub".to_owned()),
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
        assert_eq!(
            flagged.subscription,
            ["env-sub"],
            "the variable names one subscription"
        );

        // --subscription is repeatable, and a subscription named on the
        // command line beats both.
        let repeated = Cli::parse_from([
            "ticket-tui",
            "--subscription",
            "one",
            "--subscription",
            "two",
        ])
        .with_file_defaults(&file, env);
        assert_eq!(repeated.subscription, ["one", "two"]);

        // With neither flag nor variable, every one of them comes from the file.
        let from_file = Cli::parse_from(["ticket-tui"]).with_file_defaults(&file, |_| None);
        assert_eq!(from_file.org.as_deref(), Some("file-org"));
        assert_eq!(from_file.project.as_deref(), Some("ISTO"));
        assert_eq!(from_file.code_project.as_deref(), Some("Fiquants"));
        assert_eq!(from_file.query.as_deref(), Some("[System.Id] > 1"));
        assert_eq!(from_file.workspace.as_deref(), Some(Path::new("/srv/code")));
        assert_eq!(
            from_file.arm_config(),
            ArmConfig {
                subscriptions: vec!["file-sub".to_owned()],
                registries: vec!["acrdev".to_owned()],
                vaults: vec!["kv-dev".to_owned()],
            },
            "the ACR and Key Vault tabs read exactly what the file named"
        );

        // And with no file either, everything is left for `az` to answer.
        let bare = Cli::parse_from(["ticket-tui"]).with_file_defaults(&Config::default(), |_| None);
        assert_eq!(bare.org, None);
        assert_eq!(bare.code_project, None);
        assert_eq!(bare.arm_config(), ArmConfig::default());
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
    fn a_body_over_the_limit_is_refused_with_its_size_rather_than_truncated() {
        let over = vec![b'x'; usize::try_from(COMMENT_LIMIT).unwrap() + 1];
        let refusal = format!(
            "{:#}",
            comment_body(Some("-"), &mut Cursor::new(over), false).unwrap_err()
        );

        assert!(
            refusal.contains("that is a log, not a comment: 64 kB"),
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
    fn pods_command(arguments: &[&str]) -> PodsCommand {
        let Some(Command::Pods(command)) = Cli::parse_from(arguments).command else {
            panic!("pods did not parse");
        };
        command
    }

    fn pod_rows() -> Vec<PodRow> {
        [
            aks_tests::pod("qa", "orders", "orders-api-1", "Running"),
            aks_tests::pod("qa", "orders", "orders-api-2", "CrashLoopBackOff"),
            aks_tests::pod("prod", "billing", "billing-worker-1", "Running"),
        ]
        .into_iter()
        .map(|pod| PodRow::new(pod, &[]))
        .collect()
    }

    #[test]
    fn pods_parses_its_cluster_namespace_query_and_json() {
        let command = pods_command(&[
            "ticket-tui",
            "pods",
            "--cluster",
            "qa",
            "--namespace",
            "orders",
            "status:running",
            "--json",
        ]);
        assert_eq!(command.cluster.as_deref(), Some("qa"));
        assert_eq!(command.namespace.as_deref(), Some("orders"));
        assert_eq!(command.query.as_deref(), Some("status:running"));
        assert!(command.json);

        let bare = pods_command(&["ticket-tui", "pods"]);
        assert!(bare.cluster.is_none() && bare.namespace.is_none() && bare.query.is_none());
        assert!(!bare.json);
    }

    #[test]
    fn a_pod_query_narrows_by_cluster_status_and_app_and_the_rest_matches_the_name() {
        let names = |rows: Vec<PodRow>| {
            rows.into_iter()
                .map(|row| row.pod.key.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(filter_pods(pod_rows(), None)).len(), 3);
        assert_eq!(
            names(filter_pods(pod_rows(), Some("cluster:qa"))),
            ["orders-api-1", "orders-api-2"]
        );
        assert_eq!(
            names(filter_pods(pod_rows(), Some("status:crashloopbackoff"))),
            ["orders-api-2"]
        );
        // Two fields are ANDed: the label every fixture pod carries, and the
        // cluster only one of them is in.
        assert_eq!(
            names(filter_pods(pod_rows(), Some("app:orders-api cluster:prod"))),
            ["billing-worker-1"]
        );
        assert!(filter_pods(pod_rows(), Some("app:nothing")).is_empty());
        assert_eq!(
            names(filter_pods(pod_rows(), Some("billing-worker"))),
            ["billing-worker-1"]
        );
        assert!(filter_pods(pod_rows(), Some("ns:nowhere")).is_empty());
    }

    #[test]
    fn run_pods_prints_a_table_and_json_from_a_fake_and_exits_non_zero_when_a_cluster_fails() {
        let config = Config {
            clusters: vec![
                aks_tests::cluster("qa", &["orders"]),
                aks_tests::cluster("prod", &["billing"]),
            ],
            ..Config::default()
        };
        let fake = aks_tests::FakeKube::default();
        fake.answer(
            "qa",
            Some("orders"),
            Ok(vec![aks_tests::pod(
                "qa",
                "orders",
                "orders-api-1",
                "Running",
            )]),
        );
        fake.answer(
            "prod",
            Some("billing"),
            Err("Unable to connect to the server"),
        );

        let command = pods_command(&["ticket-tui", "pods"]);
        let error = run_pods_with(&config, &command, &fake).unwrap_err();
        assert_eq!(error.to_string(), "1 cluster(s) could not be read");
        assert_eq!(
            *fake.reads.lock().unwrap(),
            [
                ("qa".to_owned(), Some("orders".to_owned())),
                ("prod".to_owned(), Some("billing".to_owned())),
            ]
        );

        // --cluster reads only that cluster, and every cluster answering is a
        // clean exit.
        let only_qa = pods_command(&["ticket-tui", "pods", "--cluster", "qa"]);
        fake.reads.lock().unwrap().clear();
        run_pods_with(&config, &only_qa, &fake).unwrap();
        assert_eq!(fake.reads.lock().unwrap().len(), 1);

        let bad = pods_command(&["ticket-tui", "pods", "--cluster", "staging"]);
        assert_eq!(
            run_pods_with(&config, &bad, &fake).unwrap_err().to_string(),
            "no cluster called staging in config.toml"
        );
        assert_eq!(
            run_pods_with(&Config::default(), &command, &fake)
                .unwrap_err()
                .to_string(),
            "no clusters in config.toml; add a [[clusters]] table"
        );

        let now = ts("2026-08-30T10:30:00Z");
        let rows = pod_rows();
        let table = tabulate_pods(&rows, now);
        assert!(
            table.starts_with("qa    orders   orders-api-1      1/1  Running"),
            "{table}"
        );
        assert!(table.ends_with("30m"), "{table}");
        assert_eq!(tabulate_pods(&[], now), "no matching pods");

        let json = serde_json::to_value(PodJson::new(&rows[0], now)).unwrap();
        assert_eq!(json["cluster"], "qa");
        assert_eq!(json["owner"], "Deployment/orders-api");
        assert_eq!(json["ready"], "1/1");
        assert_eq!(json["age"], "30m");
        assert_eq!(json["created"], "2026-08-30T10:00:00Z");
        assert_eq!(json["labels"]["app"], "orders-api");
        assert_eq!(
            json["containers"][0]["image"],
            "myacr.azurecr.io/team/orders-api:1.2.3"
        );
    }

    fn acr_command(arguments: &[&str]) -> AcrCommand {
        let Some(Command::Acr(command)) = Cli::parse_from(arguments).command else {
            panic!("acr did not parse");
        };
        command
    }

    fn registry_fixture() -> Registry {
        Registry {
            id: "/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.ContainerRegistry/registries/acr".to_owned(),
            name: "acr".to_owned(),
            resource_group: "rg".to_owned(),
            location: "westeurope".to_owned(),
            sku: "Premium".to_owned(),
            login_server: "acr.azurecr.io".to_owned(),
        }
    }

    /// One registry, two repositories whose attributes have landed, two tags
    /// and the manifest the newer one points at.
    fn acr_fake() -> arm_tests::FakeArm {
        let fake = arm_tests::FakeArm::default();
        *fake.registries.lock().unwrap() = vec![registry_fixture()];
        *fake.repositories.lock().unwrap() = vec![
            Repository {
                name: "team/orders-api".to_owned(),
                tags: Some(7),
                manifests: Some(9),
                updated: Some(ts("2026-08-29T09:00:00Z")),
            },
            Repository {
                name: "team/billing".to_owned(),
                tags: Some(2),
                manifests: Some(2),
                updated: None,
            },
        ];
        *fake.tags.lock().unwrap() = vec![
            Tag {
                name: "1.2.3".to_owned(),
                digest: "sha256:0123456789abcdef".to_owned(),
                created: Some(ts("2026-08-29T09:00:00Z")),
                updated: None,
            },
            Tag {
                name: "1.2.2".to_owned(),
                digest: "sha256:fedcba9876543210".to_owned(),
                created: Some(ts("2026-08-28T09:00:00Z")),
                updated: None,
            },
        ];
        *fake.manifest.lock().unwrap() = Some(Manifest {
            digest: "sha256:0123456789abcdef".to_owned(),
            size: Some(13_107_200),
            created: Some(ts("2026-08-29T08:55:00Z")),
            architecture: "amd64".to_owned(),
            os: "linux".to_owned(),
        });
        fake
    }

    #[test]
    fn acr_parses_list_show_repos_and_tags_with_their_flags() {
        assert!(matches!(
            acr_command(&["ticket-tui", "acr", "list"]),
            AcrCommand::List { json: false }
        ));
        // The subscription is a global flag, so it may be written in front of
        // the subcommand.
        assert!(matches!(
            acr_command(&[
                "ticket-tui",
                "--subscription",
                "sub-1",
                "acr",
                "list",
                "--json"
            ]),
            AcrCommand::List { json: true }
        ));

        let AcrCommand::Show { registry, json } =
            acr_command(&["ticket-tui", "acr", "show", "acr", "--json"])
        else {
            panic!("acr show did not parse");
        };
        assert_eq!(registry, "acr");
        assert!(json);

        let AcrCommand::Repos(AcrReposCommand::List { registry, json }) =
            acr_command(&["ticket-tui", "acr", "repos", "list", "--registry", "acr"])
        else {
            panic!("acr repos list did not parse");
        };
        assert_eq!(registry, "acr");
        assert!(!json);

        let AcrCommand::Tags(AcrTagsCommand::List {
            registry,
            repo,
            json,
        }) = acr_command(&[
            "ticket-tui",
            "acr",
            "tags",
            "list",
            "--registry",
            "acr",
            "--repo",
            "team/orders-api",
            "--json",
        ])
        else {
            panic!("acr tags list did not parse");
        };
        assert_eq!(registry, "acr");
        assert_eq!(repo, "team/orders-api");
        assert!(json);

        let AcrCommand::Tags(AcrTagsCommand::Show {
            registry,
            repo,
            tag,
            json,
        }) = acr_command(&[
            "ticket-tui",
            "acr",
            "tags",
            "show",
            "--registry",
            "acr",
            "--repo",
            "team/orders-api",
            "1.2.3",
        ])
        else {
            panic!("acr tags show did not parse");
        };
        assert_eq!(registry, "acr");
        assert_eq!(repo, "team/orders-api");
        assert_eq!(tag, "1.2.3");
        assert!(!json);
    }

    #[test]
    fn acr_prints_registries_repositories_and_tags_from_a_fake() {
        let fake = acr_fake();
        for arguments in [
            vec!["ticket-tui", "acr", "list"],
            vec!["ticket-tui", "acr", "show", "acr"],
            vec!["ticket-tui", "acr", "repos", "list", "--registry", "acr"],
            vec![
                "ticket-tui",
                "acr",
                "tags",
                "list",
                "--registry",
                "acr",
                "--repo",
                "team/orders-api",
            ],
            vec![
                "ticket-tui",
                "acr",
                "tags",
                "show",
                "--registry",
                "acr",
                "--repo",
                "team/orders-api",
                "1.2.3",
                "--json",
            ],
        ] {
            run_acr_with(&acr_command(&arguments), &fake).unwrap();
        }
        // `list` asks for nothing but the inventory; `repos list` reads the
        // catalog and then one attributes call per name; `tags show` reads the
        // manifest the tag points at.
        assert_eq!(
            *fake.reads.lock().unwrap(),
            [
                "inventory",
                "inventory",
                "repositories",
                "inventory",
                "repositories",
                "repository team/orders-api",
                "repository team/billing",
                "inventory",
                "tags team/orders-api",
                "inventory",
                "tags team/orders-api",
                "manifest sha256:0123456789abcdef",
            ]
        );

        let now = ts("2026-08-30T09:00:00Z");
        assert_eq!(
            tabulate_registries(&[registry_fixture()]),
            "acr  rg  Premium  westeurope  acr.azurecr.io"
        );
        assert_eq!(
            tabulate_registries(&[]),
            "no container registries in this subscription"
        );

        let json =
            serde_json::to_value(RegistryJson::with_catalog(&registry_fixture(), 2)).unwrap();
        assert_eq!(json["login_server"], "acr.azurecr.io");
        assert_eq!(json["repositories"], 2);
        assert_eq!(
            json["portal_url"],
            "https://portal.azure.com/#resource/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.ContainerRegistry/registries/acr"
        );
        // A listing has not read any catalog, so it says nothing about counts.
        assert!(
            serde_json::to_value(RegistryJson::new(&registry_fixture()))
                .unwrap()
                .get("repositories")
                .is_none()
        );

        let described = describe_registry(&registry_fixture(), 2);
        assert!(
            described.starts_with("acr\nacr.azurecr.io\n"),
            "{described}"
        );
        assert!(described.contains("Repositories  2"), "{described}");

        let repositories = fake.repositories.lock().unwrap().clone();
        let table = tabulate_repositories(&repositories, now);
        assert!(
            table.starts_with("team/orders-api  7  9  2026-08-29 09:00:00 UTC (1d)"),
            "{table}"
        );
        // A repository whose attributes nobody could read keeps its dashes.
        assert_eq!(
            tabulate_repositories(
                &[Repository {
                    name: "team/billing".to_owned(),
                    tags: None,
                    manifests: None,
                    updated: None,
                }],
                now
            ),
            "team/billing  \u{2014}  \u{2014}  \u{2014}"
        );
        assert_eq!(
            tabulate_repositories(&[], now),
            "no repositories in this registry"
        );

        let json = serde_json::to_value(RepositoryJson::new(&repositories[0])).unwrap();
        assert_eq!(json["tags"], 7);
        assert_eq!(json["manifests"], 9);
        assert_eq!(json["updated"], "2026-08-29T09:00:00Z");

        let tags = fake.tags.lock().unwrap().clone();
        let table = tabulate_tags(&tags, now);
        assert!(
            table.starts_with("1.2.3  0123456789ab  2026-08-29 09:00:00 UTC (1d)"),
            "{table}"
        );
        assert_eq!(tabulate_tags(&[], now), "no tags on this repository");

        let manifest = fake.manifest.lock().unwrap().clone().unwrap();
        let pull = "acr.azurecr.io/team/orders-api:1.2.3";
        let shown = describe_tag(&tags[0], &manifest, pull, now);
        assert!(shown.starts_with("1.2.3  0123456789ab\n"), "{shown}");
        assert!(shown.contains("Platform      linux/amd64"), "{shown}");
        assert!(shown.contains("Size          12.5 MB"), "{shown}");
        assert!(shown.ends_with(&format!("Pull          {pull}")), "{shown}");

        let json = serde_json::to_value(TagShowJson {
            tag: TagJson::new(&tags[0]),
            pull: pull.to_owned(),
            manifest: ManifestJson::new(&manifest),
        })
        .unwrap();
        assert_eq!(json["tag"]["short_digest"], "0123456789ab");
        assert_eq!(json["tag"]["created"], "2026-08-29T09:00:00Z");
        assert_eq!(json["manifest"]["platform"], "linux/amd64");
        assert_eq!(json["manifest"]["size_label"], "12.5 MB");
        assert_eq!(json["pull"], pull);

        // An attributes call that refuses is a line on stderr and a non-zero
        // exit once the rows that did answer have been printed.
        *fake.repository_failure.lock().unwrap() = Some("forbidden".to_owned());
        assert_eq!(
            run_acr_with(
                &acr_command(&["ticket-tui", "acr", "repos", "list", "--registry", "acr"]),
                &fake
            )
            .unwrap_err()
            .to_string(),
            "2 repository(s) could not be read"
        );

        // A tag the repository does not have is refused rather than guessed at.
        *fake.repository_failure.lock().unwrap() = None;
        assert_eq!(
            run_acr_with(
                &acr_command(&[
                    "ticket-tui",
                    "acr",
                    "tags",
                    "show",
                    "--registry",
                    "acr",
                    "--repo",
                    "team/orders-api",
                    "9.9.9",
                ]),
                &fake
            )
            .unwrap_err()
            .to_string(),
            "no tag called 9.9.9 on team/orders-api in acr"
        );
    }

    #[test]
    fn a_registry_the_subscription_does_not_hold_is_refused_by_name() {
        let fake = acr_fake();
        assert_eq!(
            run_acr_with(&acr_command(&["ticket-tui", "acr", "show", "ghcr"]), &fake)
                .unwrap_err()
                .to_string(),
            "no registry called ghcr in subscription sub-1"
        );
        // The name is matched the way every other name here is: ignoring case.
        run_acr_with(&acr_command(&["ticket-tui", "acr", "show", "ACR"]), &fake).unwrap();

        let empty = arm_tests::FakeArm::default();
        assert_eq!(
            run_acr_with(&acr_command(&["ticket-tui", "acr", "show", "acr"]), &empty)
                .unwrap_err()
                .to_string(),
            "no container registries in this subscription"
        );
    }

    /// The four top-level groups are one enum once they are parsed, which is
    /// what lets one function and one harness run the lot.
    fn vault_command(arguments: &[&str]) -> VaultCliCommand {
        match Cli::parse_from(arguments).command {
            Some(Command::Vaults(command)) => VaultCliCommand::Vaults(command),
            Some(Command::Secrets(command)) => VaultCliCommand::Secrets(command),
            Some(Command::Keys(command)) => VaultCliCommand::Keys(command),
            Some(Command::Certs(command)) => VaultCliCommand::Certs(command),
            _ => panic!("the vault command did not parse"),
        }
    }

    fn vault_fixture() -> Vault {
        Vault {
            id: "/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.KeyVault/vaults/atlas-kv".to_owned(),
            name: "atlas-kv".to_owned(),
            resource_group: "rg".to_owned(),
            location: "westeurope".to_owned(),
            sku: "standard".to_owned(),
            uri: "https://atlas-kv.vault.azure.net/".to_owned(),
        }
    }

    /// One vault holding two secrets, a key and two certificates — one of
    /// which has expired — and a value nothing but `--value` may print.
    fn vault_fake() -> arm_tests::FakeArm {
        let fake = arm_tests::FakeArm::default();
        *fake.vaults.lock().unwrap() = vec![vault_fixture()];
        *fake.items.lock().unwrap() = vec![
            VaultItem {
                kind: ItemKind::Secret,
                name: "orders-db".to_owned(),
                enabled: true,
                created: Some(ts("2026-07-01T09:00:00Z")),
                updated: Some(ts("2026-08-29T09:00:00Z")),
                expires: None,
                content_type: Some("text/plain".to_owned()),
                recovery_level: Some("Recoverable+Purgeable".to_owned()),
            },
            VaultItem {
                kind: ItemKind::Secret,
                name: "retired-token".to_owned(),
                enabled: false,
                created: Some(ts("2026-01-05T09:00:00Z")),
                updated: Some(ts("2026-01-05T09:00:00Z")),
                expires: Some(ts("2026-08-20T09:00:00Z")),
                content_type: None,
                recovery_level: None,
            },
            VaultItem {
                kind: ItemKind::Key,
                name: "signing".to_owned(),
                enabled: true,
                created: Some(ts("2026-03-01T09:00:00Z")),
                updated: Some(ts("2026-08-28T09:00:00Z")),
                expires: None,
                content_type: None,
                recovery_level: Some("Purgeable".to_owned()),
            },
            VaultItem {
                kind: ItemKind::Certificate,
                name: "atlas-tls".to_owned(),
                enabled: true,
                created: Some(ts("2026-06-29T09:00:00Z")),
                updated: Some(ts("2026-08-27T09:00:00Z")),
                expires: Some(ts("2026-09-29T09:00:00Z")),
                content_type: None,
                recovery_level: None,
            },
            VaultItem {
                kind: ItemKind::Certificate,
                name: "old-tls".to_owned(),
                enabled: true,
                created: Some(ts("2025-08-20T09:00:00Z")),
                updated: Some(ts("2026-02-01T09:00:00Z")),
                expires: Some(ts("2026-08-20T09:00:00Z")),
                content_type: None,
                recovery_level: None,
            },
        ];
        *fake.secret.lock().unwrap() = "p@ssw0rd-that-belongs-in-no-table".to_owned();
        fake
    }

    #[test]
    fn vaults_secrets_keys_and_certs_parse_their_forms_and_value_rejects_json() {
        assert!(matches!(
            vault_command(&["ticket-tui", "vaults", "list"]),
            VaultCliCommand::Vaults(VaultsCommand::List { json: false })
        ));

        // The subscription is a global flag, so it may be written in front of
        // the subcommand.
        let VaultCliCommand::Vaults(VaultsCommand::Show { vault, json }) = vault_command(&[
            "ticket-tui",
            "--subscription",
            "sub-1",
            "vaults",
            "show",
            "atlas-kv",
            "--json",
        ]) else {
            panic!("vaults show did not parse");
        };
        assert_eq!(vault, "atlas-kv");
        assert!(json);

        let VaultCliCommand::Secrets(SecretsCommand::List { vault, json }) =
            vault_command(&["ticket-tui", "secrets", "list", "--vault", "atlas-kv"])
        else {
            panic!("secrets list did not parse");
        };
        assert_eq!(vault, "atlas-kv");
        assert!(!json);

        let VaultCliCommand::Secrets(SecretsCommand::Show {
            vault,
            name,
            json,
            value,
        }) = vault_command(&[
            "ticket-tui",
            "secrets",
            "show",
            "--vault",
            "atlas-kv",
            "orders-db",
        ])
        else {
            panic!("secrets show did not parse");
        };
        assert_eq!((vault.as_str(), name.as_str()), ("atlas-kv", "orders-db"));
        assert!(!json);
        assert!(!value);

        let VaultCliCommand::Secrets(SecretsCommand::Show { value, .. }) = vault_command(&[
            "ticket-tui",
            "secrets",
            "show",
            "--vault",
            "atlas-kv",
            "orders-db",
            "--value",
        ]) else {
            panic!("secrets show --value did not parse");
        };
        assert!(value);

        let VaultCliCommand::Keys(KeysCommand::List { vault, json }) =
            vault_command(&["ticket-tui", "keys", "list", "--vault", "atlas-kv"])
        else {
            panic!("keys list did not parse");
        };
        assert_eq!(vault, "atlas-kv");
        assert!(!json);

        let VaultCliCommand::Certs(CertsCommand::List { vault, json }) = vault_command(&[
            "ticket-tui",
            "certs",
            "list",
            "--vault",
            "atlas-kv",
            "--json",
        ]) else {
            panic!("certs list did not parse");
        };
        assert_eq!(vault, "atlas-kv");
        assert!(json);

        // Printing a value and printing a document are different asks, and
        // asking for both is refused at the command line rather than resolved
        // into a quiet preference for one of them.
        assert!(
            Cli::try_parse_from([
                "ticket-tui",
                "secrets",
                "show",
                "--vault",
                "atlas-kv",
                "orders-db",
                "--value",
                "--json",
            ])
            .is_err()
        );
    }

    #[test]
    fn vault_items_print_as_tables_and_json_from_a_fake() {
        let fake = vault_fake();
        for arguments in [
            vec!["ticket-tui", "vaults", "list"],
            vec!["ticket-tui", "vaults", "show", "atlas-kv"],
            vec!["ticket-tui", "secrets", "list", "--vault", "atlas-kv"],
            vec!["ticket-tui", "keys", "list", "--vault", "atlas-kv"],
            vec![
                "ticket-tui",
                "certs",
                "list",
                "--vault",
                "atlas-kv",
                "--json",
            ],
            vec![
                "ticket-tui",
                "secrets",
                "show",
                "--vault",
                "atlas-kv",
                "orders-db",
            ],
        ] {
            run_vaults_with(&vault_command(&arguments), &fake).unwrap();
        }
        // `vaults list` asks for nothing but the inventory; every other form
        // reads the one listing that answers for all three kinds, and none of
        // them reads a value.
        assert_eq!(
            *fake.reads.lock().unwrap(),
            [
                "inventory",
                "inventory",
                "items",
                "inventory",
                "items",
                "inventory",
                "items",
                "inventory",
                "items",
                "inventory",
                "items",
            ]
        );

        let now = ts("2026-08-30T09:00:00Z");
        assert_eq!(
            tabulate_vaults(&[vault_fixture()]),
            "atlas-kv  rg  westeurope  standard  https://atlas-kv.vault.azure.net/"
        );
        assert_eq!(tabulate_vaults(&[]), "no key vaults in this subscription");

        let json = serde_json::to_value(VaultJson::new(&vault_fixture())).unwrap();
        assert_eq!(json["uri"], "https://atlas-kv.vault.azure.net/");
        assert_eq!(
            json["portal_url"],
            "https://portal.azure.com/#resource/subscriptions/sub-1/resourceGroups/rg/providers/Microsoft.KeyVault/vaults/atlas-kv"
        );
        // A listing has read no vault's contents, so it says nothing about
        // what is in one.
        assert!(json.get("items").is_none());

        let items = fake.items.lock().unwrap().clone();
        let json = serde_json::to_value(VaultJson::with_items(&vault_fixture(), &items)).unwrap();
        assert_eq!(json["items"]["secrets"], 2);
        assert_eq!(json["items"]["keys"], 1);
        assert_eq!(json["items"]["certs"], 2);

        let described = describe_vault(&vault_fixture(), &items);
        assert!(
            described.starts_with("atlas-kv\nhttps://atlas-kv.vault.azure.net/\n"),
            "{described}"
        );
        assert!(described.contains("Secrets       2"), "{described}");
        assert!(described.contains("Keys          1"), "{described}");
        assert!(described.contains("Certificates  2"), "{described}");
        assert!(
            described.ends_with(&format!(
                "Portal        {}",
                portal_url(&vault_fixture().id)
            )),
            "{described}"
        );

        // Each listing command is the one listing, filtered.
        let secrets = of_kind(&items, ItemKind::Secret);
        let keys = of_kind(&items, ItemKind::Key);
        let certs = of_kind(&items, ItemKind::Certificate);
        assert_eq!(
            secrets
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["orders-db", "retired-token"]
        );
        assert_eq!(keys.len(), 1);
        assert_eq!(certs.len(), 2);

        // A secret carries a content type, so it gets the extra column; a
        // secret that never expires keeps its dash.
        assert_eq!(
            tabulate_items(&secrets[..1], ItemKind::Secret, now),
            "orders-db  yes  2026-08-29 09:00:00 UTC (1d)  \u{2014}  text/plain"
        );
        let table = tabulate_items(&secrets, ItemKind::Secret, now);
        assert!(
            table.contains("orders-db      yes  2026-08-29 09:00:00 UTC (1d)"),
            "{table}"
        );
        // A disabled secret says so, and one with no content type keeps a dash
        // where the column would be.
        assert!(
            table.ends_with("retired-token  no   2026-01-05 09:00:00 UTC (237d)  2026-08-20 09:00:00 UTC (10d)  \u{2014}"),
            "{table}"
        );

        assert_eq!(
            tabulate_items(&keys, ItemKind::Key, now),
            "signing  yes  2026-08-28 09:00:00 UTC (2d)  \u{2014}"
        );
        assert_eq!(
            tabulate_items(&[], ItemKind::Key, now),
            "no keys in this vault"
        );

        // A certificate's expiry is what a reader is after, so it is said in
        // words as well as in a stamp, in whichever direction applies.
        let table = tabulate_items(&certs, ItemKind::Certificate, now);
        assert!(
            table.contains("2026-09-29 09:00:00 UTC (0s) expires in 30 days"),
            "{table}"
        );
        assert!(
            table.contains("2026-08-20 09:00:00 UTC (10d) expired 10 days ago"),
            "{table}"
        );
        assert_eq!(expiry_words(None, now), None);
        assert_eq!(
            expiry_words(Some(ts("2026-08-31T09:00:00Z")), now).as_deref(),
            Some("expires in 1 day")
        );

        let json = serde_json::to_value(VaultItemJson::new(&secrets[0])).unwrap();
        assert_eq!(json["kind"], "secret");
        assert_eq!(json["name"], "orders-db");
        assert_eq!(json["enabled"], true);
        assert_eq!(json["created"], "2026-07-01T09:00:00Z");
        assert_eq!(json["updated"], "2026-08-29T09:00:00Z");
        assert_eq!(json["expires"], Value::Null);
        assert_eq!(json["content_type"], "text/plain");
        assert_eq!(json["recovery_level"], "Recoverable+Purgeable");

        // `secrets show` prints what the listing says and then says, in as
        // many words, what it is deliberately not printing.
        let described = describe_item(&secrets[0], now);
        assert!(described.starts_with("orders-db  secret\n"), "{described}");
        assert!(described.contains("Enabled       yes"), "{described}");
        assert!(
            described.contains("Content type  text/plain"),
            "{described}"
        );
        assert!(
            described.contains("Recovery      Recoverable+Purgeable"),
            "{described}"
        );
        assert!(
            described.ends_with("\nvalue: not shown; pass --value to print it"),
            "{described}"
        );

        // Nothing a metadata form prints, in either shape, carries the value.
        let value = fake.secret.lock().unwrap().clone();
        for printed in [
            described,
            table,
            tabulate_items(&secrets, ItemKind::Secret, now),
            describe_vault(&vault_fixture(), &items),
            to_json(&secrets.iter().map(VaultItemJson::new).collect::<Vec<_>>()).unwrap(),
            to_json(&VaultItemJson::new(&secrets[0])).unwrap(),
            to_json(&VaultJson::with_items(&vault_fixture(), &items)).unwrap(),
        ] {
            assert!(!printed.contains(&value), "{printed}");
        }
    }

    #[test]
    fn secrets_show_value_prints_exactly_the_value() {
        let fake = vault_fake();
        let value = fake.secret.lock().unwrap().clone();
        let secret = fake.secret_value(&vault_fixture(), "orders-db").unwrap();
        assert_eq!(secret_output(&secret), value);
        // Which is why the value path has to go through `expose`: a format
        // string prints the wrong thing rather than nothing.
        assert_eq!(format!("{secret}"), "[redacted]");
        assert_eq!(format!("{secret:?}"), "[redacted]");

        fake.reads.lock().unwrap().clear();
        run_vaults_with(
            &vault_command(&[
                "ticket-tui",
                "secrets",
                "show",
                "--vault",
                "atlas-kv",
                "orders-db",
                "--value",
            ]),
            &fake,
        )
        .unwrap();
        assert_eq!(
            *fake.reads.lock().unwrap(),
            ["inventory", "secret_value orders-db"]
        );

        // The metadata form reads the listing every other form reads, and
        // never the value.
        fake.reads.lock().unwrap().clear();
        run_vaults_with(
            &vault_command(&[
                "ticket-tui",
                "secrets",
                "show",
                "--vault",
                "atlas-kv",
                "orders-db",
            ]),
            &fake,
        )
        .unwrap();
        assert_eq!(*fake.reads.lock().unwrap(), ["inventory", "items"]);
    }

    #[test]
    fn a_vault_or_item_the_subscription_does_not_hold_is_refused_by_name() {
        let fake = vault_fake();
        assert_eq!(
            run_vaults_with(
                &vault_command(&["ticket-tui", "vaults", "show", "billing-kv"]),
                &fake
            )
            .unwrap_err()
            .to_string(),
            "no vault called billing-kv in this subscription"
        );
        // The name is matched the way every other name here is: ignoring case.
        run_vaults_with(
            &vault_command(&["ticket-tui", "vaults", "show", "ATLAS-KV"]),
            &fake,
        )
        .unwrap();

        // A key is not a secret, however the vault lists them.
        assert_eq!(
            run_vaults_with(
                &vault_command(&[
                    "ticket-tui",
                    "secrets",
                    "show",
                    "--vault",
                    "atlas-kv",
                    "signing",
                ]),
                &fake
            )
            .unwrap_err()
            .to_string(),
            "no secret called signing in atlas-kv"
        );

        let empty = arm_tests::FakeArm::default();
        assert_eq!(
            run_vaults_with(
                &vault_command(&["ticket-tui", "secrets", "list", "--vault", "atlas-kv"]),
                &empty
            )
            .unwrap_err()
            .to_string(),
            "no vault called atlas-kv in this subscription"
        );
    }

    /// One environment of the fixture under `fixtures/kustomize`, from the
    /// rendered YAML checked in beside it, so no `kubectl` is wanted here.
    fn env_fixture(name: &str) -> kustomize::EnvManifest {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("fixtures/kustomize/rendered/{name}.yaml"));
        kustomize::parse(name, &fs::read_to_string(path).unwrap())
    }

    fn env_table(name: &str, overlays: &[&str]) -> config::Environment {
        config::Environment {
            name: name.to_owned(),
            overlays: overlays.iter().map(|held| (*held).to_owned()).collect(),
            vault: Some(format!("kv-{name}")),
            registry: None,
            cluster: None,
        }
    }

    #[test]
    fn the_env_subcommands_parse() {
        let Some(Command::Env(EnvCommand::List { json })) =
            Cli::parse_from(["ticket-tui", "env", "list", "--json"]).command
        else {
            panic!("env list --json")
        };
        assert!(json);

        let Some(Command::Env(EnvCommand::Show { environment, json })) =
            Cli::parse_from(["ticket-tui", "env", "show", "prod"]).command
        else {
            panic!("env show")
        };
        assert_eq!(environment, "prod");
        assert!(!json);

        let Some(Command::Env(EnvCommand::Check { environments, .. })) =
            Cli::parse_from(["ticket-tui", "env", "check", "qa", "prod"]).command
        else {
            panic!("env check")
        };
        assert_eq!(environments, ["qa", "prod"]);

        let Some(Command::Env(EnvCommand::Check {
            environments,
            json,
            offline,
        })) = Cli::parse_from(["ticket-tui", "env", "check", "--json"]).command
        else {
            panic!("env check --json")
        };
        assert!(environments.is_empty(), "none named is every environment");
        assert!(json);
        assert!(!offline, "the vault half is on unless it is turned off");

        let Some(Command::Env(EnvCommand::Check { offline, .. })) =
            Cli::parse_from(["ticket-tui", "env", "check", "prod", "--offline"]).command
        else {
            panic!("env check --offline")
        };
        assert!(offline);
    }

    #[test]
    fn env_check_prints_a_line_per_finding_and_exits_one_clean_zero_unreadable_two() {
        let (qa, prod) = (
            env_table("qa", &["overlays/qa"]),
            env_table("prod", &["overlays/prod"]),
        );
        let rendered = vec![
            (&qa, Ok(env_fixture("qa"))),
            (&prod, Ok(env_fixture("prod"))),
        ];
        let (report, code) = check_report(&rendered, false, &Ok(Vec::new())).unwrap();
        assert_eq!(
            report,
            concat!(
                "qa: clean (3 workloads, 1 configmap)\n",
                "prod shop-prod/billing-api Deployment env SIGNING_KEY \u{2190} secret billing-kv key SIGNING_KEY missing\n",
                "prod shop-prod/orders-api Deployment env RATE_LIMIT_PER_MIN \u{2190} configmap orders-config key RATE_LIMIT_PER_MIN missing",
            )
        );
        assert_eq!(code, 1, "any finding at all is a failed gate");

        let clean = vec![(&qa, Ok(env_fixture("qa")))];
        let (report, code) = check_report(&clean, false, &Ok(Vec::new())).unwrap();
        assert_eq!(report, "qa: clean (3 workloads, 1 configmap)");
        assert_eq!(code, 0);

        // An overlay that would not render is 2: a gate that could not look
        // must not read as clean.
        let broken: Vec<Rendered<'_>> = vec![(&prod, Err(anyhow::anyhow!("no such overlay")))];
        let (report, code) = check_report(&broken, false, &Ok(Vec::new())).unwrap();
        assert!(report.is_empty(), "{report}");
        assert_eq!(code, 2);
        let mixed = vec![
            (&qa, Ok(env_fixture("qa"))),
            (&prod, Err(anyhow::anyhow!("boom"))),
        ];
        assert_eq!(check_report(&mixed, false, &Ok(Vec::new())).unwrap().1, 2);

        // `--json` carries the counts a clean run reports as well as the
        // findings, so a pipeline reads one shape either way.
        let (report, code) = check_report(&clean, true, &Ok(Vec::new())).unwrap();
        let parsed: Value = serde_json::from_str(&report).unwrap();
        assert_eq!(parsed[0]["environment"], "qa");
        assert_eq!(parsed[0]["clean"], true);
        assert_eq!(parsed[0]["workloads"], 3);
        assert_eq!(code, 0);
    }

    #[test]
    fn env_check_joins_the_overlay_to_the_vault_and_offline_skips_that_half_alone() {
        let prod = env_table("prod", &["overlays/prod"]);
        let rendered = vec![(&prod, Ok(env_fixture("prod")))];
        let now = ts("2026-09-01T00:00:00Z");
        let fake = arm_tests::FakeArm::default();
        *fake.vaults.lock().unwrap() = vec![Vault {
            id: "/subscriptions/s/resourceGroups/rg/providers/Microsoft.KeyVault/vaults/kv-prod"
                .to_owned(),
            name: "kv-prod".to_owned(),
            resource_group: "rg".to_owned(),
            location: "westeurope".to_owned(),
            sku: "standard".to_owned(),
            uri: "https://kv-prod.vault.azure.net/".to_owned(),
        }];
        *fake.items.lock().unwrap() =
            ["billing-signing-key", "billing-db-password", "reaper-token"]
                .into_iter()
                .map(|name| VaultItem {
                    kind: ItemKind::Secret,
                    name: name.to_owned(),
                    enabled: true,
                    created: None,
                    updated: None,
                    expires: None,
                    content_type: None,
                    recovery_level: None,
                })
                .collect();

        // One inventory and one listing per vault named, which is what
        // `secrets list` costs.
        let vaults = read_vaults(&fake, &["kv-prod"], now).unwrap();
        assert_eq!(*fake.reads.lock().unwrap(), ["inventory", "items"]);
        assert_eq!(vaults[0].vault, "kv-prod");

        let (report, code) = check_report(&rendered, false, &Ok(vaults)).unwrap();
        assert!(
            report.contains(
                "prod shop-prod/billing-vault SecretProviderClass kv-prod secret billing-legacy-token missing"
            ),
            "{report}"
        );
        assert_eq!(code, 1);

        // The skipped half takes its findings with it and leaves the exit code
        // to the repository's own.
        let (offline, code) = check_report(&rendered, false, &Err("--offline".to_owned())).unwrap();
        assert!(!offline.contains("billing-legacy-token"), "{offline}");
        assert_eq!(offline.lines().count(), 2, "{offline}");
        assert_eq!(code, 1, "the two findings the overlays answer for");

        // And the reason is the one the line names.
        let cli = Cli::parse_from(["ticket-tui"]);
        assert_eq!(vault_names(&cli, &rendered, true).unwrap_err(), "--offline");
        // An environment naming no vault has no vault half to skip, and no
        // token is asked for on its account.
        let mut bare = env_table("dev", &["overlays/dev"]);
        bare.vault = None;
        assert!(
            vault_names(&cli, &[(&bare, Ok(env_fixture("qa")))], false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn env_list_says_what_each_environment_holds_or_why_it_would_not_render() {
        let (qa, prod) = (
            env_table("qa", &["overlays/qa"]),
            env_table("prod", &["overlays/prod"]),
        );
        let rendered = vec![
            (&qa, Ok(env_fixture("qa"))),
            (
                &prod,
                Err(anyhow::anyhow!("no overlay matches overlays/prod")),
            ),
        ];
        let table = list_table(&rendered, false).unwrap();
        let lines: Vec<&str> = table.lines().collect();
        assert!(
            lines[0].starts_with("qa ")
                && lines[0].contains("overlays/qa")
                && lines[0].contains("kv-qa"),
            "{}",
            lines[0]
        );
        assert!(lines[0].ends_with("3 workloads, 1 configmap, 1 secret, 2 providers"));
        assert!(
            lines[1].ends_with("no overlay matches overlays/prod"),
            "{}",
            lines[1]
        );
        assert!(
            lines[1].contains('\u{2014}'),
            "what it does not name is a dash"
        );

        let parsed: Value = serde_json::from_str(&list_table(&rendered, true).unwrap()).unwrap();
        assert_eq!(parsed[0]["rendered"], true);
        assert_eq!(parsed[0]["config_maps"], 1);
        assert_eq!(parsed[1]["rendered"], false);
        assert_eq!(parsed[1]["error"], "no overlay matches overlays/prod");
    }

    #[test]
    fn env_show_names_every_reference_and_carries_no_value_at_all() {
        let text = describe_environment(&env_fixture("prod"));
        assert!(text.starts_with("prod \u{b7} 3 workloads, 1 configmap, 1 secret, 2 providers"));
        for line in [
            "  Deployment shop-prod/orders-api",
            "    migrate (init)  acrprod.azurecr.io/team/orders-migrate:1.3.9",
            "      env RATE_LIMIT_PER_MIN \u{2190} configmap orders-config key RATE_LIMIT_PER_MIN",
            "      env FEATURE_FLAGS \u{2190} configmap orders-flags key FLAGS (optional)",
            "    volume banner \u{2190} configmap orders-config key BANNER",
            "  shop-prod/orders-config  2 keys",
            "  SecretProviderClass shop-prod/billing-vault \u{b7} kv-prod",
            "    produces  secret billing-kv  DB_PASSWORD",
            "    produces  secret reaper-external  whole (dataFrom)",
        ] {
            assert!(text.contains(line), "{line:?} is not in\n{text}");
        }
        // Names and presence, never values: nothing the render holds a value
        // for is printed, here or as JSON.
        let json = to_json(&env_fixture("prod")).unwrap();
        for value in ["placeholder", "cGxhY2Vob2xkZXI=", "two lines of it"] {
            assert!(!text.contains(value) && !json.contains(value), "{value}");
        }
    }

    #[test]
    fn env_diff_takes_two_environments_and_one_optional_service() {
        let Some(Command::Env(EnvCommand::Diff {
            from,
            to,
            service,
            json,
        })) = Cli::parse_from([
            "ticket-tui",
            "env",
            "diff",
            "qa",
            "prod",
            "orders",
            "--json",
        ])
        .command
        else {
            panic!("env diff qa prod orders --json")
        };
        assert_eq!((from.as_str(), to.as_str()), ("qa", "prod"));
        assert_eq!(service.as_deref(), Some("orders"));
        assert!(json);

        let Some(Command::Env(EnvCommand::Diff { service, json, .. })) =
            Cli::parse_from(["ticket-tui", "env", "diff", "qa", "prod"]).command
        else {
            panic!("env diff qa prod")
        };
        assert_eq!(service, None, "none named is every service");
        assert!(!json);
    }

    #[test]
    fn env_diff_prints_a_block_a_service_with_the_image_gap_read_as_pull_requests() {
        let mut diff =
            kustomize::diff::diff(&env_fixture("qa"), &env_fixture("prod"), Some("orders"));
        // What the runs, the pull requests and the clone answer with, which
        // `read_history` fills in from SQLite and git.
        diff.services[0].images[0].history = Some(kustomize::diff::ImageHistory {
            pull_requests: vec![812, 815, 820],
            work_items: vec![642, 650, 655],
            ..kustomize::diff::ImageHistory::default()
        });
        diff.services[0].images[1].history = Some(kustomize::diff::ImageHistory {
            note: Some("no clone of orders-api to read the history from".to_owned()),
            ..kustomize::diff::ImageHistory::default()
        });
        assert_eq!(
            describe_diff(&diff),
            concat!(
                "orders-api Deployment\n",
                "  image   api  1.4.0 \u{2192} 1.3.9  3 PRs behind: !812 !815 !820 \u{2014} #642 #650 #655\n",
                "  image   migrate  1.4.0 \u{2192} 1.3.9  no clone of orders-api to read the history from\n",
                "  key     orders-config RATE_LIMIT_PER_MIN\n",
                "  var     api TRACE_SAMPLE_RATE",
            )
        );

        // The reverse entry says which environment has it, and an environment
        // against itself has nothing to say at all.
        let billing = describe_diff(&kustomize::diff::diff(
            &env_fixture("qa"),
            &env_fixture("prod"),
            Some("billing"),
        ));
        assert!(
            billing.contains("  secret  kv-prod billing-legacy-token  only in prod"),
            "{billing}"
        );
        assert_eq!(
            describe_diff(&kustomize::diff::diff(
                &env_fixture("qa"),
                &env_fixture("qa"),
                None
            )),
            "qa \u{2192} qa: identical"
        );

        // `--json` is the model itself, and carries no value from either
        // render any more than the text does.
        let printed = to_json(&diff).unwrap();
        let parsed: Value = serde_json::from_str(&printed).unwrap();
        assert_eq!(parsed["services"][0]["workload"], "orders-api");
        assert_eq!(parsed["services"][0]["images"][0]["from"], "1.4.0");
        assert_eq!(
            parsed["services"][0]["images"][0]["history"]["pull_requests"][0],
            812
        );
        assert_eq!(parsed["services"][0]["variables"][0]["side"], "from");
        assert!(!printed.contains("0.25"), "{printed}");
    }

    #[test]
    fn the_pull_requests_between_two_commits_are_read_out_of_the_clone_and_named_when_there_is_none()
     {
        let directory = tempfile::tempdir().unwrap();
        let clone = directory.path().join("orders-api");
        fs::create_dir_all(&clone).unwrap();
        for arguments in [
            &["init", "--initial-branch=main"][..],
            &["config", "user.email", "test@example.com"],
            &["config", "user.name", "Test"],
        ] {
            local::git(&clone, arguments).unwrap();
        }
        let commit = |subject: &str| {
            fs::write(clone.join("file"), subject).unwrap();
            local::git(&clone, &["add", "."]).unwrap();
            local::git(&clone, &["commit", "-m", subject]).unwrap();
            local::git(&clone, &["rev-parse", "HEAD"])
                .unwrap()
                .trim()
                .to_owned()
        };
        let first = commit("what prod is running");
        commit("Merged PR 815: Squash the second");
        let last = commit("Merge the third one");

        let lines = git_log(&clone, &first, &last).unwrap();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with(&last) && lines[0].ends_with(" Merge the third one"));

        // The squash left its own commit behind in the branch and nowhere in
        // the history, so the subject Azure DevOps wrote is what names it; the
        // other is found by the commit it was read at.
        let mut squashed = stored_pull_request();
        squashed.id = 815;
        let mut merged = stored_pull_request();
        merged.id = 820;
        merged.last_merge_source_commit = last.clone();
        let requests = vec![squashed, merged];
        assert_eq!(
            kustomize::diff::between(&lines, &requests, Some("aaa-111"))
                .iter()
                .map(|request| request.id)
                .collect::<Vec<_>>(),
            [815, 820]
        );
        assert!(
            kustomize::diff::between(&lines, &requests, Some("another-repo")).is_empty(),
            "another repository's pull requests are not this service's"
        );

        // A range git will not walk is git's own first word on it, and no
        // clone at all is the line the block prints in place of a list.
        assert!(
            git_log(&clone, "nowhere", &last)
                .unwrap_err()
                .contains("nowhere")
        );
        assert_eq!(
            service_clone(None, None, "orders-api").unwrap_err(),
            "no clone of orders-api to read the history from"
        );
    }

    #[test]
    fn the_image_gap_is_read_back_through_the_stored_runs_and_the_services_own_clone() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let clone = workspace.join("orders-api");
        fs::create_dir_all(&clone).unwrap();
        for arguments in [
            &["init", "--initial-branch=main"][..],
            &["config", "user.email", "test@example.com"],
            &["config", "user.name", "Test"],
            &[
                "remote",
                "add",
                "origin",
                "https://dev.azure.com/demo/atlas/_git/orders-api",
            ],
        ] {
            local::git(&clone, arguments).unwrap();
        }
        let commit = |subject: &str| {
            fs::write(clone.join("file"), subject).unwrap();
            local::git(&clone, &["add", "."]).unwrap();
            local::git(&clone, &["commit", "-m", subject]).unwrap();
            local::git(&clone, &["rev-parse", "HEAD"])
                .unwrap()
                .trim()
                .to_owned()
        };
        let (behind, ahead) = (
            commit("what prod runs"),
            commit("Merged PR 11: Split the files"),
        );

        // The two tags the fixture's overlays carry, as the runs that built
        // them, and the one pull request the range between them holds.
        let database = directory.path().join("tickets.sqlite3");
        let mut stored = SqliteTicketRepository::open(&database).unwrap();
        let run = |id: i64, build_number: &str, commit: &str| Run {
            id,
            build_number: build_number.into(),
            source_version: commit.into(),
            ..scripted_run(
                crate::model::RunStatus::Completed,
                Some(RunResult::Succeeded),
            )
        };
        stored
            .replace_runs(&[run(40, "1.4.0", &ahead), run(39, "1.3.9", &behind)])
            .unwrap();
        stored
            .replace_pull_requests(&[stored_pull_request()])
            .unwrap();
        let repo = crate::model::Repo {
            id: "aaa-111".into(),
            name: "orders-api".into(),
            project: "atlas".into(),
            default_branch: Some("refs/heads/main".into()),
            remote_url: "https://dev.azure.com/demo/atlas/_git/orders-api".into(),
            ssh_url: String::new(),
            web_url: String::new(),
            is_disabled: false,
            size: None,
        };
        stored.replace_repos(std::slice::from_ref(&repo)).unwrap();

        let mut diff =
            kustomize::diff::diff(&env_fixture("qa"), &env_fixture("prod"), Some("orders"));
        read_history(&mut diff, &database, Some(&workspace));
        let history = diff.services[0].images[0]
            .history
            .as_ref()
            .expect("the gap between two tags on file");
        assert_eq!(history.from.as_ref().map(|run| run.id), Some(40));
        assert_eq!(
            history.from.as_ref().map(|run| run.matched),
            Some(kustomize::diff::Match::BuildNumber)
        );
        assert_eq!(history.pull_requests, [11], "the one the range holds");
        assert_eq!(history.work_items, [690]);
        assert_eq!(history.to_string(), "1 PR behind: !11 \u{2014} #690");

        // Without the repository there is no clone to read it in, and the line
        // says so rather than the diff failing.
        stored.replace_repos(&[]).unwrap();
        let mut diff =
            kustomize::diff::diff(&env_fixture("qa"), &env_fixture("prod"), Some("orders"));
        read_history(&mut diff, &database, Some(&workspace));
        assert_eq!(
            diff.services[0].images[0]
                .history
                .as_ref()
                .unwrap()
                .to_string(),
            "no clone of orders-api to read the history from"
        );

        // And with no database at all the two tags stand on their own.
        let mut diff =
            kustomize::diff::diff(&env_fixture("qa"), &env_fixture("prod"), Some("orders"));
        read_history(&mut diff, &directory.path().join("nothing.sqlite3"), None);
        assert!(diff.services[0].images[0].history.is_none());
    }
}
