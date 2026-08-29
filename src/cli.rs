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
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueHint};
use serde::Serialize;
use serde_json::Value;

use crate::azure::{self, AzureClient, AzureConfig};
use crate::classification::{self, NodeKind};
use crate::db::{self, SqliteTicketRepository, default_database_path};
use crate::edit::{FieldEdit, normalize_tags, revision_test};
use crate::filter::{FilterField, MatchContext, ParsedQuery, WorkItemSchema, parse_query};
use crate::markdown;
use crate::model::{CommentRecord, Identity, Ticket, TicketKey};
use crate::search;
use crate::sync::{self, AzureConnector, SyncMode, SyncOutcome, WorkItemSource};
use crate::timestamp::Timestamp;

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// SQLite database to open instead of the platform data-directory default
    #[arg(long, global = true, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub database: Option<PathBuf>,
    /// Azure DevOps organization (slug or URL); defaults to TICKET_TUI_ORG or `az devops configure`
    #[arg(long, global = true, value_name = "ORG")]
    pub org: Option<String>,
    /// Azure DevOps project; defaults to TICKET_TUI_PROJECT or `az devops configure`
    #[arg(long, global = true, value_name = "PROJECT")]
    pub project: Option<String>,
    /// Seconds between background pulls from Azure DevOps, 0 to turn the timer
    /// off; defaults to TICKET_TUI_REFRESH or 60
    #[arg(long, value_name = "SECONDS")]
    pub refresh: Option<u64>,
    /// Extra WIQL condition ANDed into every pull, narrowing a large project;
    /// defaults to TICKET_TUI_QUERY
    #[arg(long, value_name = "WIQL")]
    pub query: Option<String>,
    /// Days a work item may sit untouched before the Changed column flags it
    /// as stale; defaults to TICKET_TUI_STALE_DAYS, then whatever the session
    /// remembers, then 14
    #[arg(long, value_name = "DAYS")]
    pub stale_days: Option<u16>,
    /// Directory the Repos tab looks for clones in and makes new ones under;
    /// defaults to TICKET_TUI_WORKSPACE, then ~/Development
    #[arg(long, global = true, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub workspace: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// One thing to do and then exit. The flags above still apply: `--database`,
/// `--org` and `--project` may be written either side of the subcommand.
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
        /// What the comment says, as plain text
        text: String,
    },
    /// Add a work item to the project
    Create(CreateArgs),
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
        Command::Comment { id, text } => run_comment(cli, &database, *id, text),
        Command::Create(args) => run_create(cli, &database, args),
    }
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

/// One pull, run to completion, reporting what moved. The database is opened
/// here first so a file that does not exist yet is created and given its
/// schema; the pull itself takes its own connection.
fn run_sync(cli: &Cli, database: PathBuf, full: bool) -> Result<()> {
    let config = AzureConfig::resolve(cli.org.clone(), cli.project.clone(), cli.query.clone())?;
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
    let (ticket, relations) = source
        .patch_work_item(key.id, &document)
        .map_err(|error| conflict_advice(error, key.id))?;
    repository.upsert(&ticket, &relations)?;
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

fn run_comment(cli: &Cli, database: &Path, id: i64, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        bail!("a comment cannot be empty");
    }
    let mut repository = open_database(database)?;
    let client = connect(cli)?;
    let key = key_for(&client, id);
    let comment = post_comment(&client, &mut repository, &key, text)?;
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
    text: &str,
) -> Result<CommentRecord> {
    let posted = source.post_comment(key.id, &azure::comment_html(text))?;
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
    let (ticket, relations) =
        source.create_work_item(args.work_item_type.trim(), &patch_ops(edits), args.parent)?;
    repository.upsert(&ticket, &relations)?;
    Ok(ticket)
}

/// Opens Azure DevOps for a subcommand that writes. An unresolved organization
/// is a hard error here: a write has nowhere else to go.
fn connect(cli: &Cli) -> Result<AzureClient> {
    AzureClient::connect(AzureConfig::resolve(
        cli.org.clone(),
        cli.project.clone(),
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
    use crate::model::{RelationRecord, StateOption};
    use crate::timestamp::ts;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tempfile::{TempDir, tempdir};

    fn ticket(id: i64, title: &str, state: &str, assignee: Option<&str>) -> Ticket {
        Ticket {
            key: TicketKey {
                organization: "demo".into(),
                id,
            },
            project: "atlas".into(),
            revision: 4,
            work_item_type: "Task".into(),
            title: title.into(),
            state: state.into(),
            reason: None,
            assigned_to: assignee.map(ToOwned::to_owned),
            priority: Some(2),
            area_path: "Atlas".into(),
            iteration_path: "Atlas\\Sprint 1".into(),
            tags: vec!["cli".into()],
            description: "Ship the subcommands.".into(),
            description_html: "<p>Ship the subcommands.</p>".into(),
            created_at: ts("2026-01-01T00:00:00Z"),
            changed_at: ts("2026-02-01T00:00:00Z"),
            web_url: format!("https://dev.azure.com/demo/atlas/_workitems/edit/{id}"),
            details_rev: 0,
        }
    }

    fn config() -> AzureConfig {
        AzureConfig {
            organization: "demo".into(),
            project: "atlas".into(),
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

        fn answer(&self, id: i64, document: &[Value]) -> Result<(Ticket, Vec<RelationRecord>)> {
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
                .map(|ticket| (ticket, Vec::new()))
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

        fn patch_work_item(
            &self,
            id: i64,
            patch: &[Value],
        ) -> Result<(Ticket, Vec<RelationRecord>)> {
            self.answer(id, patch)
        }

        fn create_work_item(
            &self,
            work_item_type: &str,
            fields: &[Value],
            parent: Option<i64>,
        ) -> Result<(Ticket, Vec<RelationRecord>)> {
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
        assert_eq!((id, text.as_str()), (613, "on its way"));

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
        repository.upsert(&stored, &[]).unwrap();
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
        repository.upsert(&stored, &[]).unwrap();
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
        repository.upsert(&stored, &[]).unwrap();
        let source = FakeSource::default();

        let comment =
            post_comment(&source, &mut repository, &stored.key, "on its way <now>").unwrap();

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
}
