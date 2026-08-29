use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use serde_json::Value;
use ticket_tui::agent_context::{self, AgentContext};
use ticket_tui::app::{
    App, AppAction, CopiedContent, DividerOrientation, PointerTarget, PreparedTickets, SyncTarget,
};
use ticket_tui::azure::AzureConfig;
use ticket_tui::cli::{self, Cli, resolve_me};
use ticket_tui::db::{self, SqliteTicketRepository, default_database_path};
use ticket_tui::edit::{EditRejection, EditRequest, FieldEdit};
use ticket_tui::markdown;
use ticket_tui::model::{Ticket, TicketKey};
use ticket_tui::session;
use ticket_tui::sync::{
    self, AzureConnector, DetailsOutcome, PullOrigin, ReparentRejection, SyncEvent, SyncHandle,
    SyncMode, SyncOutcome, SyncRequest, SyncScheduler,
};
use ticket_tui::timestamp::Timestamp;
use url::Url;

/// How often the background pull runs when nothing says otherwise.
const DEFAULT_REFRESH_SECONDS: u64 = 60;

/// Everything the event loop needs to keep the database in step with Azure
/// DevOps: the worker thread, the timer that feeds it, and why there is no
/// worker when there is none.
struct SyncRuntime {
    worker: Option<SyncHandle>,
    scheduler: SyncScheduler,
    config: Option<AzureConfig>,
    /// Why Azure DevOps could not be resolved, reported when the user asks for
    /// a sync anyway.
    offline_reason: Option<String>,
    /// When to read the selected work item's comments and history.
    details: DetailsEngine,
}

/// How long the selection has to stay on one work item before its comments and
/// history are worth two requests. Holding `j` down the table crosses dozens of
/// rows; none of them is being read.
const DETAILS_REST: Duration = Duration::from_millis(300);

/// When to ask for the selected work item's comments and revision history.
///
/// The trigger is the selection coming to rest, not the selection changing, so
/// scrolling costs nothing. One request is in flight at a time, and a work item
/// whose details could not be read is not asked about again for the rest of the
/// run: a failure is a notification, never a loop.
#[derive(Debug, Default)]
struct DetailsEngine {
    /// The work item the selection is sitting on and when it landed there.
    resting: Option<(TicketKey, Instant)>,
    in_flight: Option<TicketKey>,
    failed: HashSet<TicketKey>,
    /// Whether a failure has already been reported this run.
    reported: bool,
}

impl DetailsEngine {
    /// The work item to read now, if the selection has been on one whose
    /// stored details are behind it for [`DETAILS_REST`]. Called every turn of
    /// the event loop, which is what makes the rest period a rest period.
    fn due(&mut self, selected: Option<&Ticket>, now: Instant) -> Option<TicketKey> {
        let Some(key) = self.wanted(selected) else {
            self.resting = None;
            return None;
        };
        match &self.resting {
            Some((resting, since)) if *resting == key => {
                if now.duration_since(*since) < DETAILS_REST {
                    return None;
                }
            }
            // Somewhere new: the rest period starts over from here.
            _ => {
                self.resting = Some((key, now));
                return None;
            }
        }
        if self.in_flight.is_some() {
            return None;
        }
        self.resting = None;
        self.in_flight = Some(key.clone());
        Some(key)
    }

    /// The selected work item, when what is stored for it is behind the
    /// revision on screen and reading it has not already failed.
    fn wanted(&self, selected: Option<&Ticket>) -> Option<TicketKey> {
        let ticket = selected?;
        (ticket.details_rev < ticket.revision && !self.failed.contains(&ticket.key))
            .then(|| ticket.key.clone())
    }

    /// The request answered, whatever it said.
    fn finish(&mut self) {
        self.in_flight = None;
    }

    /// The request failed. Reports whether it is worth saying so: only the
    /// first failure of the run is, because every one after it says the same
    /// thing about a pane the user did not ask to fill.
    fn fail(&mut self, key: TicketKey) -> bool {
        self.finish();
        self.failed.insert(key);
        !std::mem::replace(&mut self.reported, true)
    }

    /// How long the event loop may sleep before the rest period is up.
    fn time_until_due(&self, now: Instant) -> Option<Duration> {
        if self.in_flight.is_some() {
            return None;
        }
        self.resting
            .as_ref()
            .map(|(_, since)| DETAILS_REST.saturating_sub(now.duration_since(*since)))
    }
}

impl SyncRuntime {
    /// What a pull the user asked for reports, and where it pulled from.
    fn status_for(&self, mode: SyncMode, count: usize) -> String {
        let synced = sync::pull_summary(mode, count);
        self.config.as_ref().map_or_else(
            || synced.clone(),
            |config| format!("{synced} from {}/{}", config.organization, config.project),
        )
    }

    /// Why nothing can be sent, for a run with no worker.
    fn offline_message(&self) -> String {
        self.offline_reason
            .clone()
            .unwrap_or_else(|| "Azure DevOps is not configured".to_owned())
    }

    /// Hands one request to the worker, or says why it could not: there is no
    /// worker, or the one there was has stopped.
    fn send(&self, request: SyncRequest) -> Result<(), String> {
        match &self.worker {
            Some(worker) => worker.send(request).map_err(|error| format!("{error:#}")),
            None => Err(self.offline_message()),
        }
    }

    /// Gives up on syncing for the rest of the run, which only happens when the
    /// worker thread is gone.
    fn stop(&mut self, app: &mut App, error: &str) {
        self.worker = None;
        self.scheduler.stop();
        if app.shell.fail_sync(error, true) {
            app.shell.set_error(format!("Sync stopped: {error}"));
        }
    }
}

#[derive(Default)]
struct ReloadEngine {
    receiver: Option<Receiver<std::result::Result<PreparedTickets, String>>>,
}

struct AgentContextPublisher {
    path: PathBuf,
    last: Option<AgentContext>,
}

impl AgentContextPublisher {
    fn new(database: &Path) -> Self {
        Self {
            path: agent_context::path_for(database),
            last: None,
        }
    }

    fn publish(&mut self, app: &App) -> Result<()> {
        let context = app.work_items.agent_context(&app.shell);
        if self.last.as_ref() == Some(&context) {
            return Ok(());
        }
        agent_context::save(&self.path, &context)?;
        self.last = Some(context);
        Ok(())
    }

    fn remove(&self) -> Result<()> {
        agent_context::remove(&self.path)
    }
}

impl ReloadEngine {
    fn start(&mut self, path: &Path) -> Result<bool> {
        if self.receiver.is_some() {
            return Ok(false);
        }

        let path = path.to_path_buf();
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("ticket-reload".into())
            .spawn(move || {
                let result = (|| -> Result<PreparedTickets> {
                    let repository = SqliteTicketRepository::open_existing(&path)?;
                    let tickets = repository.load_all()?;
                    let graph = repository.load_graph()?;
                    let states = repository.load_type_states()?;
                    Ok(PreparedTickets::with_graph(tickets, graph).with_states(states))
                })()
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send(result);
            })
            .context("failed to start database reload worker")?;
        self.receiver = Some(receiver);
        Ok(true)
    }

    fn try_result(&mut self) -> Option<std::result::Result<PreparedTickets, String>> {
        let result = match self.receiver.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("database reload worker stopped".into()),
        };
        self.receiver = None;
        Some(result)
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    // Every subcommand does its one thing and exits; only a bare invocation
    // opens the TUI.
    if let Some(command) = &cli.command {
        return cli::run(&cli, command);
    }
    let refresh = resolve_refresh(cli.refresh, std::env::var("TICKET_TUI_REFRESH").ok())?;
    let stale_days =
        resolve_stale_days(cli.stale_days, std::env::var("TICKET_TUI_STALE_DAYS").ok())?;
    let database_path = cli.database.clone().unwrap_or_else(default_database_path);
    let mut repository = SqliteTicketRepository::open(&database_path)?;
    let schema_was_rebuilt = repository.schema_was_rebuilt();
    // Which project the database already holds, read before this run can write
    // over it.
    let stored_project = repository
        .meta(db::ORGANIZATION_KEY)?
        .zip(repository.meta(db::PROJECT_KEY)?);
    // An unresolved organization is not a reason to refuse to start: the TUI
    // browses the database offline and says why it cannot sync.
    let (config, offline_reason) =
        match AzureConfig::resolve(cli.org.clone(), cli.project.clone(), cli.query.clone()) {
            Ok(config) => (Some(config), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };

    let tickets = repository.load_all()?;
    let graph = repository.load_graph()?;
    let database_is_empty = tickets.is_empty();
    // A database filled from another project is browsed, never synced into: the
    // first pull would replace every row in it, and only `ticket-tui sync
    // --full` asks for that.
    let wrong_project = config.as_ref().and_then(|config| {
        project_mismatch(
            stored_project
                .as_ref()
                .map(|(organization, project)| (organization.as_str(), project.as_str())),
            config,
            database_is_empty,
        )
    });
    let offline_reason = wrong_project.clone().or(offline_reason);
    let mut app = App::new(tickets);
    app.work_items.set_workspace_graph(&mut app.shell, graph);
    app.work_items
        .set_state_catalog(repository.load_type_states()?);
    app.work_items.set_identities(repository.load_identities()?);
    app.work_items
        .set_work_item_types(repository.load_work_item_types()?);
    app.work_items.set_classification_nodes(
        repository.load_classification_nodes()?,
        repository
            .meta(db::CLASSIFICATION_FETCHED_KEY)?
            .and_then(|raw| Timestamp::parse(&raw).ok()),
    );
    app.shell.set_me(resolve_me(
        repository.meta(db::ME_DISPLAY_NAME_KEY)?,
        std::env::var("TICKET_TUI_ME").ok(),
    ));
    stamp_database(&mut app, &repository);
    app.shell.set_offline_reason(offline_reason.clone());
    let session_path = session::path_for(repository.path());
    match session::load(&session_path) {
        Ok(loaded) => app.restore_session(loaded),
        Err(error) => app
            .shell
            .set_error(format!("Could not load session: {error:#}")),
    }
    // After the session, so a threshold asked for on this run beats the one
    // the last run left behind.
    if let Some(days) = stale_days {
        app.work_items.override_stale_days(days);
    }

    let interval = (refresh > 0).then(|| Duration::from_secs(refresh));
    // Where the rows come from, for the database overlay: the project, how
    // often it is pulled, and whatever narrows it.
    app.shell
        .set_sync_source(config.as_ref().map(|config| sync_source(config, refresh)));
    app.shell
        .set_sync_target(config.as_ref().map(|config| SyncTarget {
            organization: config.organization.clone(),
            project: config.project.clone(),
            refresh_seconds: refresh,
        }));
    let mut runtime = SyncRuntime {
        worker: None,
        scheduler: SyncScheduler::new(interval),
        config: config.clone(),
        offline_reason,
        details: DetailsEngine::default(),
    };
    if let Some(config) = config.filter(|_| wrong_project.is_none()) {
        runtime.worker = Some(SyncHandle::spawn(
            database_path.clone(),
            Box::new(AzureConnector::new(config)),
        )?);
        app.shell.enable_sync();
        // The TUI opens from the database and the first pull runs behind it
        // straight away — even with the timer off, when the database was just
        // rebuilt or holds nothing to browse.
        let now = Instant::now();
        if interval.is_some() || schema_was_rebuilt || database_is_empty {
            runtime.scheduler.schedule_now(now);
        } else {
            runtime.scheduler.schedule_next(now);
        }
    } else {
        app.shell.set_status(offline_status(database_is_empty));
    }
    // Said last, because a database held by another project is a more specific
    // reason to be offline than having no organization at all.
    if let Some(message) = wrong_project {
        app.shell.set_error(message);
    }

    let mut context_publisher = AgentContextPublisher::new(repository.path());
    let result = run_terminal(
        &mut app,
        &mut repository,
        &mut runtime,
        &mut context_publisher,
    );
    let remove_context = context_publisher.remove();
    if let Err(error) = session::save(&session_path, &app.snapshot_session()) {
        eprintln!("warning: could not save session: {error:#}");
    }
    if let Err(error) = remove_context {
        if result.is_ok() {
            return Err(error.context("failed to remove agent context on exit"));
        }
        eprintln!("warning: could not remove agent context: {error:#}");
    }
    result
}

/// Records the database as it stands, so the watcher does not reload a file
/// our own worker just wrote.
fn stamp_database(app: &mut App, repository: &SqliteTicketRepository) {
    app.shell.configure_database(
        repository.path().to_path_buf(),
        db::data_signature(repository.path()),
    );
}

/// What a run without a configured organization opens with.
fn offline_status(database_is_empty: bool) -> &'static str {
    if database_is_empty {
        "Database is empty and offline; run `ticket-tui sync --org ORG --project PROJECT` to pull work items"
    } else {
        "Browsing the database offline; no Azure DevOps organization is configured"
    }
}

/// Why the resolved project must not sync into this database, if it must not.
/// A database another project filled would be emptied by the first pull, so
/// the run browses it offline instead; `ticket-tui sync --full` is how a
/// database is deliberately pointed at another project. A database with
/// nothing in it, or one from before this was recorded, adopts whatever the
/// next pull brings.
fn project_mismatch(
    stored: Option<(&str, &str)>,
    config: &AzureConfig,
    database_is_empty: bool,
) -> Option<String> {
    let (organization, project) = stored?;
    if database_is_empty || (organization == config.organization && project == config.project) {
        return None;
    }
    Some(format!(
        "Database holds {organization}/{project}; pass --database for another project or run `ticket-tui sync --full` to replace it"
    ))
}

/// How often the background pull runs: `--refresh`, then `TICKET_TUI_REFRESH`,
/// then a minute. A variable that is not a number of seconds is a startup
/// error rather than a silent fall back to the default, because a typo there
/// would otherwise change how often the TUI reaches Azure DevOps and say
/// nothing about it.
fn resolve_refresh(flag: Option<u64>, env: Option<String>) -> Result<u64> {
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
fn resolve_stale_days(flag: Option<u16>, env: Option<String>) -> Result<Option<u16>> {
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

/// What the database overlay says about where the rows come from: the project,
/// the timer, and the condition narrowing the project when one is configured.
fn sync_source(config: &AzureConfig, refresh: u64) -> String {
    let timer = if refresh > 0 {
        format!("every {refresh}s")
    } else {
        "on request".to_owned()
    };
    let mut source = format!("{}/{} {timer}", config.organization, config.project);
    if let Some(scope) = &config.scope {
        source.push_str(&format!(" · scope ({scope})"));
    }
    source
}

fn run_terminal(
    app: &mut App,
    repository: &mut SqliteTicketRepository,
    runtime: &mut SyncRuntime,
    context_publisher: &mut AgentContextPublisher,
) -> Result<()> {
    let mut terminal = ratatui::init();
    let _restore = TerminalRestore;
    let mut reloader = ReloadEngine::default();
    let mut mouse_pointer = MousePointerShape::Default;
    enable_terminal_input()?;

    let mut redraw = true;
    while !app.shell.should_quit {
        redraw |= app.work_items.poll_search(&mut app.shell);
        redraw |= poll_reload(app, repository, &mut reloader);
        redraw |= poll_sync(app, repository, runtime);
        redraw |= poll_watch(app, repository, &mut reloader);
        redraw |= dispatch_due_pull(app, runtime);
        redraw |= dispatch_due_details(app, runtime);
        redraw |= persist_session(app, repository);
        redraw |= app.shell.tick();
        if redraw {
            terminal.draw(|frame| ticket_tui::ui::render(frame, app))?;
            sync_mouse_pointer(app, &mut mouse_pointer);
            redraw = false;
            if let Err(error) = context_publisher.publish(app) {
                app.shell
                    .set_error(format!("Could not publish agent context: {error:#}"));
                redraw = true;
            }
        }

        let timeout = if app.work_items.search_pending || app.shell.reload_pending {
            Duration::from_millis(33)
        } else {
            // The loop has to wake for the next scheduled pull as well as for
            // an expiring notification.
            [
                app.shell.next_wakeup(),
                runtime.scheduler.time_until_due(Instant::now()),
                runtime.details.time_until_due(Instant::now()),
            ]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(Duration::from_secs(1))
            .min(Duration::from_secs(1))
        };
        if !event::poll(timeout)? {
            continue;
        }
        let (action, event_redraw) = match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => (app.handle_key(key), true),
            Event::Mouse(mouse) => {
                let update = app.handle_mouse(mouse);
                (update.action, update.redraw)
            }
            Event::Paste(text) => {
                app.handle_paste(&text);
                (AppAction::None, true)
            }
            Event::Resize(_, _) => {
                app.shell.handle_resize();
                (AppAction::None, true)
            }
            Event::FocusGained | Event::FocusLost | Event::Key(_) => (AppAction::None, false),
        };
        if event_redraw {
            redraw = true;
        }
        if handle_action(action, app, runtime, &open_in_browser) {
            // Something else owned the screen for a while, so nothing ratatui
            // believes is on it can be trusted, the pointer shape included.
            terminal.clear()?;
            mouse_pointer = MousePointerShape::Default;
            redraw = true;
        }
    }
    Ok(())
}

/// Carries out one action, and says whether the screen has to be painted from
/// scratch afterwards. Only the editor hand-off, which gives the terminal back
/// for as long as the editor runs, ever asks for that.
fn handle_action(
    action: AppAction,
    app: &mut App,
    runtime: &mut SyncRuntime,
    opener: &dyn Fn(&Url) -> Result<()>,
) -> bool {
    match action {
        AppAction::None => {}
        // The shell answers these itself, before the event loop sees them.
        AppAction::Follow(_) | AppAction::HistoryBack | AppAction::HistoryForward => {}
        AppAction::Sync => start_sync(app, runtime),
        // A bulk change over the checked rows hands over several: the worker
        // takes them in this order, each with its own revision test.
        AppAction::Edit(requests) => {
            for request in requests {
                start_edit(app, runtime, request);
            }
        }
        // One request a work item, taken in the order the confirmation listed
        // them, so a checked-set delete runs sequentially like a bulk edit.
        AppAction::Delete(keys) => {
            for key in keys {
                start_delete(app, runtime, key);
            }
        }
        // The pickers and the form are already open over what the database
        // holds, so a worker that is gone changes nothing and says nothing.
        AppAction::FetchIdentities => drop(runtime.send(SyncRequest::Identities)),
        AppAction::FetchClassificationNodes => {
            drop(runtime.send(SyncRequest::ClassificationNodes));
        }
        AppAction::FetchWorkItemTypes => drop(runtime.send(SyncRequest::WorkItemTypes)),
        AppAction::Comment { key, text } => start_comment(app, runtime, key, text),
        AppAction::Reparent { key, new_parent } => start_reparent(app, runtime, key, new_parent),
        AppAction::Create {
            work_item_type,
            patch,
            parent,
        } => start_create(app, runtime, work_item_type, patch, parent),
        AppAction::EditDescription { key, html } => {
            edit_description(app, runtime, &key, &html);
            return true;
        }
        AppAction::OpenUrl(raw_url) => match open_https_url(&raw_url, opener) {
            Ok(()) => app.shell.set_status(format!("Opened {raw_url}")),
            Err(error) => app
                .shell
                .set_error(format!("Could not open ticket: {error:#}")),
        },
        AppAction::Copy { text, content } => match copy_to_clipboard(&text) {
            Ok(()) => app.shell.set_status(copied_status(content)),
            Err(error) => app.shell.set_error(format!("Could not copy: {error:#}")),
        },
        AppAction::WriteFile { path, contents } => match fs::write(&path, contents) {
            Ok(()) => app.shell.set_status(format!("Exported {}", path.display())),
            Err(error) => app
                .shell
                .set_error(format!("Could not export {}: {error:#}", path.display())),
        },
    }
    false
}

/// The Actions menu's Description row: the description goes out to the user's
/// editor as Markdown, and whatever comes back comes back as HTML.
///
/// The TUI steps out of the way entirely while the editor runs — the alternate
/// screen, mouse capture, and bracketed paste all go back the way they were
/// found — and takes the terminal back afterwards whether the editor saved
/// something, changed nothing, or never started at all.
fn edit_description(app: &mut App, runtime: &mut SyncRuntime, key: &TicketKey, html: &str) {
    let command = editor_command(env::var("VISUAL").ok(), env::var("EDITOR").ok());
    let outcome = released_terminal(|| {
        let directory = tempfile::Builder::new()
            .prefix("ticket-tui-")
            .tempdir()
            .context("could not make a directory to edit in")?;
        run_description_editor(directory.path(), key.id, html, &command)
    });
    apply_description_outcome(app, runtime, key, outcome);
}

/// Files whatever the editor left: a rewritten description goes down the same
/// path as every other field edit, a file that came back untouched is not a
/// change at all, and an editor that failed says so and writes nothing.
fn apply_description_outcome(
    app: &mut App,
    runtime: &mut SyncRuntime,
    key: &TicketKey,
    outcome: Result<Option<String>>,
) {
    match outcome {
        Ok(Some(html)) => {
            if let AppAction::Edit(requests) =
                app.work_items
                    .edit_ticket(&mut app.shell, key, FieldEdit::description(&html))
            {
                for request in requests {
                    start_edit(app, runtime, request);
                }
            }
        }
        Ok(None) => app
            .shell
            .set_status(format!("#{} description unchanged", key.id)),
        Err(error) => app
            .shell
            .set_error(format!("#{} description not saved: {error:#}", key.id)),
    }
}

/// Writes the description out as Markdown, runs the editor on it, and reads
/// back what was saved.
///
/// `Ok(None)` means the file came back as it was written, notice line and all,
/// so there is nothing to save. Anything else is the HTML the Markdown builds,
/// which for an emptied file is the empty document that clears the field.
fn run_description_editor(
    directory: &Path,
    id: i64,
    html: &str,
    command: &[String],
) -> Result<Option<String>> {
    let path = directory.join(format!("ticket-{id}.md"));
    let document = markdown::description_document(html);
    fs::write(&path, format!("{document}\n"))
        .with_context(|| format!("could not write {}", path.display()))?;
    run_editor(command, &path)?;
    let edited = fs::read_to_string(&path)
        .with_context(|| format!("could not read {} back", path.display()))?;
    let saved = markdown::saved_markdown(&edited);
    if saved == markdown::saved_markdown(&document) {
        return Ok(None);
    }
    Ok(Some(markdown::markdown_to_html(&saved)))
}

/// Runs the editor on one file and waits for it. The editor owns the terminal
/// while it runs, so its own output goes straight to the screen.
fn run_editor(command: &[String], path: &Path) -> Result<()> {
    let (program, arguments) = command
        .split_first()
        .context("no editor to run; set $EDITOR")?;
    let status = Command::new(program)
        .args(arguments)
        .arg(path)
        .status()
        .with_context(|| format!("could not run {program}"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

/// The editor to hand a description to: `$VISUAL`, then `$EDITOR`, then `vi`,
/// which every system has. The variable is split on whitespace so a command
/// with arguments works — `code --wait` runs `code` with `--wait` and the file
/// after it — and one that is empty or only whitespace counts as unset.
fn editor_command(visual: Option<String>, editor: Option<String>) -> Vec<String> {
    [visual, editor]
        .into_iter()
        .flatten()
        .map(|raw| {
            raw.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<String>>()
        })
        .find(|parts| !parts.is_empty())
        .unwrap_or_else(|| vec!["vi".to_owned()])
}

/// Runs `body` with the terminal handed back to the shell, and takes it back
/// however `body` went. The caller repaints: [`handle_action`] says so on the
/// way out, because ratatui's idea of what is on screen died with the frame
/// the editor drew over.
fn released_terminal<T>(body: impl FnOnce() -> T) -> T {
    release_terminal();
    let outcome = body();
    if let Err(error) = claim_terminal() {
        // Nothing can be reported through a TUI that is not there, so this
        // goes where the editor's own output went.
        eprintln!("ticket-tui could not take the terminal back: {error:#}");
    }
    outcome
}

fn copied_status(content: CopiedContent) -> String {
    format!("Copied {} to clipboard!", content.label())
}

fn start_reload(
    app: &mut App,
    repository: &SqliteTicketRepository,
    reloader: &mut ReloadEngine,
    message: &str,
) {
    match reloader.start(repository.path()) {
        Ok(true) => {
            app.shell.reload_pending = true;
            app.shell.set_status(message);
        }
        Ok(false) => app.shell.set_status("Reload already in progress"),
        Err(error) => app
            .shell
            .set_error(format!("Could not start reload: {error:#}")),
    }
}

/// Asks for the pull the timer has booked, if one is due. Nothing is ever
/// queued behind a pull already in flight.
fn dispatch_due_pull(app: &mut App, runtime: &mut SyncRuntime) -> bool {
    if runtime.worker.is_none() || !runtime.scheduler.due(Instant::now()) {
        return false;
    }
    runtime.scheduler.start();
    app.shell.begin_sync();
    send_pull(app, runtime, PullOrigin::Timer);
    true
}

/// Asks for the selected work item's comments and revision history once the
/// selection has settled on it. Nothing goes out while the selection is still
/// moving, while one request is already out, or for a work item whose stored
/// details already match the revision on screen.
fn dispatch_due_details(app: &mut App, runtime: &mut SyncRuntime) -> bool {
    if runtime.worker.is_none() {
        return false;
    }
    let Some(key) = runtime
        .details
        .due(app.work_items.selected_ticket(), Instant::now())
    else {
        return false;
    };
    match runtime.send(SyncRequest::Details(key.clone())) {
        Ok(()) => app.work_items.details_pending = Some(key),
        Err(error) => runtime.stop(app, &error),
    }
    true
}

/// `r`: pull now, whatever the timer is doing.
fn start_sync(app: &mut App, runtime: &mut SyncRuntime) {
    if runtime.worker.is_none() {
        app.shell.set_error(runtime.offline_message());
        return;
    }
    if !runtime.scheduler.request_user_pull() {
        app.shell.set_status("Sync already in progress");
        return;
    }
    app.shell.begin_sync();
    app.shell.set_status("Syncing from Azure DevOps…");
    send_pull(app, runtime, PullOrigin::User);
}

/// Hands one edit to the sync worker. The row already shows the change, so a
/// worker that is gone puts it back here rather than leaving a lie on screen.
fn start_edit(app: &mut App, runtime: &mut SyncRuntime, request: EditRequest) {
    let key = request.key.clone();
    let label = request.edit.label().to_owned();
    if let Err(message) = runtime.send(SyncRequest::Edit(request)) {
        app.work_items.reject_edit(
            &mut app.shell,
            &EditRejection {
                key,
                label,
                conflict: false,
                message,
            },
        );
    }
}

/// Hands one comment to the sync worker. Nothing is shown on the work item
/// until Azure DevOps has stored it, so a worker that is gone only has to say
/// the comment was not posted.
fn start_comment(app: &mut App, runtime: &mut SyncRuntime, key: TicketKey, text: String) {
    let request = SyncRequest::Comment {
        key: key.clone(),
        text,
    };
    match runtime.send(request) {
        Ok(()) => app
            .shell
            .set_status(format!("Posting comment on #{}\u{2026}", key.id)),
        Err(message) => app
            .work_items
            .reject_comment(&mut app.shell, &key, &message),
    }
}

/// Hands one delete to the sync worker. Nothing has left the table yet — a row
/// is dropped when Azure DevOps says the work item is gone — so a worker that
/// is gone only has to say the work item is still there.
fn start_delete(app: &mut App, runtime: &mut SyncRuntime, key: TicketKey) {
    if let Err(message) = runtime.send(SyncRequest::Delete(key.clone())) {
        app.work_items.reject_delete(&mut app.shell, &key, &message);
    }
}

/// Hands one new work item to the sync worker. Nothing appears in the table
/// until Azure DevOps has stored it, so a worker that is gone only has to say
/// the work item was not created — and the form comes back with everything
/// still in it.
fn start_create(
    app: &mut App,
    runtime: &mut SyncRuntime,
    work_item_type: String,
    patch: Vec<Value>,
    parent: Option<i64>,
) {
    let request = SyncRequest::Create {
        work_item_type,
        patch,
        parent,
    };
    if let Err(message) = runtime.send(request) {
        app.work_items.reject_create(&mut app.shell, &message);
    }
}

/// Hands one move to the sync worker. The graph already shows it, so a worker
/// that is gone puts both halves of the old link back here rather than leaving
/// a family tree on screen that nothing in Azure DevOps agrees with.
fn start_reparent(
    app: &mut App,
    runtime: &mut SyncRuntime,
    key: TicketKey,
    new_parent: Option<i64>,
) {
    let request = SyncRequest::Reparent {
        key: key.clone(),
        new_parent,
    };
    if let Err(message) = runtime.send(request) {
        app.work_items.reject_reparent(
            &mut app.shell,
            &ReparentRejection {
                key,
                conflict: false,
                message,
            },
        );
    }
}

/// Asks the worker for a pull. Only ever called with a worker to ask, so a
/// refusal means the worker is gone.
fn send_pull(app: &mut App, runtime: &mut SyncRuntime, origin: PullOrigin) {
    if let Err(error) = runtime.send(SyncRequest::Pull(origin)) {
        runtime.stop(app, &error);
    }
}

/// Applies whatever the sync worker has finished. A pull it completed wrote the
/// database itself, so its signature is recorded here and the watcher below
/// leaves it alone instead of reloading behind us.
fn poll_sync(
    app: &mut App,
    repository: &mut SqliteTicketRepository,
    runtime: &mut SyncRuntime,
) -> bool {
    let mut redraw = false;
    while let Some(event) = runtime.worker.as_ref().and_then(SyncHandle::try_event) {
        redraw = true;
        match event {
            SyncEvent::DisplayName(name) => {
                if let Err(error) = repository.set_meta(db::ME_DISPLAY_NAME_KEY, &name) {
                    app.shell
                        .set_error(format!("Could not record the signed-in name: {error:#}"));
                }
                app.shell
                    .set_me(resolve_me(Some(name), std::env::var("TICKET_TUI_ME").ok()));
            }
            SyncEvent::Finished {
                origin,
                outcome,
                pause,
            } => {
                let now = Instant::now();
                let throttled = match &outcome {
                    SyncOutcome::Throttled { retry_after } => Some(*retry_after),
                    _ => None,
                };
                // A pull that reached Azure DevOps clears whatever backoff a run
                // of throttles built up, before the pause below pushes the next
                // one out again. Only throttles in a row keep doubling.
                if throttled.is_none() {
                    runtime.scheduler.finish(now);
                }
                match outcome {
                    SyncOutcome::Pulled {
                        prepared,
                        mode,
                        count,
                    } => {
                        app.work_items
                            .replace_prepared_tickets(&mut app.shell, prepared);
                        app.shell.finish_sync();
                        stamp_database(app, repository);
                        if origin == PullOrigin::User {
                            app.shell.set_status(runtime.status_for(mode, count));
                        }
                    }
                    // Nothing moved in Azure DevOps, so nothing was written and
                    // there is nothing to reload. The signature stays as it was:
                    // if another process wrote the file while this pull was out,
                    // the watcher is free to notice it now.
                    SyncOutcome::Unchanged => {
                        app.shell.finish_sync();
                        if origin == PullOrigin::User {
                            app.shell.set_status("Nothing changed");
                        }
                    }
                    // A timer pull that keeps failing the same way says so in
                    // the table title rather than in a toast every minute.
                    SyncOutcome::Failed(error) => {
                        if app.shell.fail_sync(&error, origin == PullOrigin::User) {
                            app.shell.set_error(format!("Sync failed: {error}"));
                        }
                    }
                    // Throttling is the service working as designed. Nothing is
                    // announced: the title says how long the timer is holding
                    // off, and the pause below books when it stops.
                    SyncOutcome::Throttled { .. } => {}
                }
                // The longer of the two waits wins: a pull turned away outright
                // and a budget that ran out on the way through are the same
                // request to be left alone, asked for twice.
                if let Some(retry_after) = throttled.into_iter().chain(pause).max() {
                    let until = runtime.scheduler.pause(now, retry_after);
                    app.shell.pause_sync(until);
                }
            }
            SyncEvent::Edited(result) => match *result {
                Ok(applied) => {
                    app.work_items.apply_edit(&mut app.shell, applied);
                    // The worker wrote that row itself, so the watcher below is
                    // told about it rather than reloading behind us.
                    stamp_database(app, repository);
                }
                Err(rejection) => {
                    // A stale copy is worth a pull: the refused field is about
                    // to arrive with whatever else moved.
                    if rejection.conflict {
                        start_sync(app, runtime);
                    }
                    app.work_items.reject_edit(&mut app.shell, &rejection);
                }
            },
            // The worker wrote these rows itself, so the signature moves with
            // them and the watcher below leaves the file alone. Only this work
            // item's comments and history change: the table, the search, and
            // every other row stay exactly as they were.
            SyncEvent::Details(outcome) => {
                app.work_items.details_pending = None;
                match *outcome {
                    DetailsOutcome::Fetched(update) => {
                        runtime.details.finish();
                        app.work_items.apply_details(update);
                        stamp_database(app, repository);
                    }
                    DetailsOutcome::Failed { key, message } => {
                        if runtime.details.fail(key) {
                            app.shell.set_error(format!(
                                "Could not read comments and history: {message}"
                            ));
                        }
                    }
                }
            }
            SyncEvent::Identities(identities) => {
                app.work_items.merge_identities(&mut app.shell, identities)
            }
            // The worker inserted this comment itself, so the signature moves
            // with it and the watcher below leaves the file alone. Nothing else
            // about the work item changed: its own `details_rev` is untouched,
            // so the next details fetch still settles the discussion.
            SyncEvent::Commented(result) => match *result {
                Ok(comment) => {
                    app.work_items.apply_comment(&mut app.shell, comment);
                    stamp_database(app, repository);
                }
                Err(rejection) => app.work_items.reject_comment(
                    &mut app.shell,
                    &rejection.key,
                    &rejection.message,
                ),
            },
            SyncEvent::ClassificationNodes(nodes) => {
                app.work_items.merge_classification_nodes(nodes)
            }
            SyncEvent::WorkItemTypes(types) => app.work_items.merge_work_item_types(types),
            // The worker stored this work item itself, so the signature moves
            // with it and the watcher below leaves the file alone.
            SyncEvent::Created(result) => match *result {
                Ok(created) => {
                    app.work_items
                        .apply_created(&mut app.shell, created.ticket, created.relations);
                    stamp_database(app, repository);
                }
                Err(rejection) => app
                    .work_items
                    .reject_create(&mut app.shell, &rejection.message),
            },
            // The worker rewrote both halves of this work item's hierarchy
            // link itself, so the signature moves with them and the watcher
            // below leaves the file alone.
            SyncEvent::Reparented(result) => match *result {
                Ok(applied) => {
                    app.work_items.apply_reparent(&mut app.shell, applied);
                    stamp_database(app, repository);
                }
                Err(rejection) => {
                    // A stale copy is worth a pull for the same reason an edit
                    // is: the family the picker was built from has moved on.
                    if rejection.conflict {
                        start_sync(app, runtime);
                    }
                    app.work_items.reject_reparent(&mut app.shell, &rejection);
                }
            },
            // The worker took this work item out of the file itself, so the
            // signature moves with it and the watcher below leaves it alone.
            SyncEvent::Deleted(result) => match *result {
                Ok(key) => {
                    app.work_items.apply_deleted(&mut app.shell, &key);
                    stamp_database(app, repository);
                }
                Err(rejection) => {
                    app.work_items
                        .reject_delete(&mut app.shell, &rejection.key, &rejection.message)
                }
            },
            SyncEvent::Stopped => {
                runtime.stop(app, "the Azure DevOps sync worker stopped");
            }
        }
    }
    redraw
}

/// Another process writing the database — an agent running `ticket-tui edit`,
/// or `ticket-tui sync` in another terminal — still reloads the rows from
/// SQLite.
fn poll_watch(
    app: &mut App,
    repository: &SqliteTicketRepository,
    reloader: &mut ReloadEngine,
) -> bool {
    let signature = db::data_signature(repository.path());
    if signature == app.shell.data_signature
        || app.shell.reload_pending
        || app.shell.sync_pending
        || app.work_items.edits_pending()
        || app.work_items.comments_pending()
        || app.work_items.creates_pending()
        || app.work_items.reparents_pending()
        || app.work_items.deletes_pending()
        || app.work_items.details_pending.is_some()
    {
        return false;
    }
    app.shell.mark_stale();
    start_reload(app, repository, reloader, "Database changed; reloading…");
    true
}

fn persist_session(app: &mut App, repository: &SqliteTicketRepository) -> bool {
    if !app.shell.session_dirty {
        return false;
    }
    let path = session::path_for(repository.path());
    match session::save(&path, &app.snapshot_session()) {
        Ok(()) => app.shell.session_dirty = false,
        Err(error) => app
            .shell
            .set_error(format!("Could not save session: {error:#}")),
    }
    true
}

fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut last_error = None;
    for command in clipboard_commands() {
        match write_to_command(command, text) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    match last_error {
        Some(error) => Err(error).context("clipboard command failed"),
        None => bail!("no clipboard command available"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MousePointerShape {
    Default,
    Link,
    ColResize,
    RowResize,
}

impl MousePointerShape {
    const fn escape_sequence(self) -> &'static [u8] {
        match self {
            Self::Default => b"\x1b]22;\x1b\\",
            Self::Link => b"\x1b]22;pointer\x1b\\",
            Self::ColResize => b"\x1b]22;col-resize\x1b\\",
            Self::RowResize => b"\x1b]22;row-resize\x1b\\",
        }
    }
}

fn sync_mouse_pointer(app: &App, current: &mut MousePointerShape) {
    let desired = mouse_pointer_for_hover(app.shell.hovered(), app.shell.divider_orientation());
    if desired == *current {
        return;
    }
    if write_mouse_pointer_shape(&mut io::stdout(), desired).is_ok() {
        *current = desired;
    }
}

fn mouse_pointer_for_hover(
    target: Option<&PointerTarget>,
    divider: Option<DividerOrientation>,
) -> MousePointerShape {
    match target {
        Some(
            PointerTarget::OpenInBrowser { .. }
            | PointerTarget::OpenSelectedUrl
            | PointerTarget::EditField { .. },
        ) => MousePointerShape::Link,
        Some(PointerTarget::PaneDivider) => match divider {
            Some(DividerOrientation::Vertical) => MousePointerShape::ColResize,
            Some(DividerOrientation::Horizontal) => MousePointerShape::RowResize,
            None => MousePointerShape::Default,
        },
        _ => MousePointerShape::Default,
    }
}

fn write_mouse_pointer_shape(writer: &mut impl Write, shape: MousePointerShape) -> io::Result<()> {
    writer.write_all(shape.escape_sequence())?;
    writer.flush()
}

fn clipboard_commands() -> Vec<Command> {
    if cfg!(target_os = "macos") {
        vec![Command::new("pbcopy")]
    } else {
        vec![
            {
                let mut command = Command::new("wl-copy");
                command.arg("--trim-newline");
                command
            },
            {
                let mut command = Command::new("xclip");
                command.args(["-selection", "clipboard"]);
                command
            },
            {
                let mut command = Command::new("xsel");
                command.args(["--clipboard", "--input"]);
                command
            },
        ]
    }
}

fn write_to_command(mut command: Command, text: &str) -> Result<()> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to start {program}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .context("failed to write clipboard contents")?;
    }
    let status = child.wait().context("clipboard command stopped")?;
    if status.success() {
        Ok(())
    } else {
        bail!("clipboard command exited with {status}");
    }
}

fn poll_reload(
    app: &mut App,
    repository: &SqliteTicketRepository,
    reloader: &mut ReloadEngine,
) -> bool {
    let Some(result) = reloader.try_result() else {
        return false;
    };
    app.shell.reload_pending = false;
    match result {
        Ok(prepared) => {
            let count = prepared.ticket_count();
            app.work_items
                .replace_prepared_tickets(&mut app.shell, prepared);
            stamp_database(app, repository);
            app.shell.set_status(format!("Reloaded {count} tickets"));
        }
        Err(error) => app.shell.set_error(format!("Reload failed: {error}")),
    }
    true
}

/// Hands one work item's URL to `opener`, which is the system launcher outside
/// the tests. Only HTTPS goes out: a stored URL is data, and a `file:` or
/// `javascript:` one is not a ticket.
fn open_https_url(raw_url: &str, opener: &dyn Fn(&Url) -> Result<()>) -> Result<()> {
    let url = Url::parse(raw_url).context("ticket URL is invalid")?;
    if url.scheme() != "https" {
        bail!("only HTTPS ticket URLs can be opened");
    }
    opener(&url).context("system URL launcher failed")
}

fn open_in_browser(url: &Url) -> Result<()> {
    open::that(url.as_str()).map_err(Into::into)
}

struct TerminalRestore;

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        release_terminal();
    }
}

/// Puts the terminal back the way the TUI found it: the pointer shape, then
/// the input features, then raw mode and the alternate screen. The end of a
/// run and the editor hand-off both leave this way, so what the editor gets is
/// exactly what the shell gets.
fn release_terminal() {
    let _ = write_mouse_pointer_shape(&mut io::stdout(), MousePointerShape::Default);
    let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
}

/// Takes the terminal back after [`release_terminal`] gave it away, in the
/// same order `ratatui::init` and the TUI's own startup take it.
fn claim_terminal() -> Result<()> {
    enable_raw_mode().context("failed to take raw mode back")?;
    execute!(io::stdout(), EnterAlternateScreen).context("failed to take the screen back")?;
    enable_terminal_input()
}

/// Turns on the input the TUI reads beyond the keyboard. Raw mode and the
/// alternate screen are already taken by the time this is called.
fn enable_terminal_input() -> Result<()> {
    execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)
        .context("failed to enable terminal input features")
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use serde_json::Value;
    use tempfile::tempdir;
    use ticket_tui::app::{FormFieldId, NotificationLevel, WorkItemMode};
    use ticket_tui::azure::{RequestRejected, SyncBatch, Throttled};
    use ticket_tui::edit::FieldEdit;
    use ticket_tui::model::{
        CommentRecord, HistoryRecord, RelationKind, RelationRecord, StateOption, Ticket,
        TicketGraph, TicketKey, WorkItemDetails,
    };
    use ticket_tui::sync::{SourceConnector, WorkItemSource};
    use ticket_tui::timestamp::Timestamp;

    /// A launcher that never opens anything, for the tests that only need
    /// the refusal.
    fn failing_opener(_: &Url) -> Result<()> {
        bail!("launcher unavailable")
    }

    /// Azure DevOps stood in for: every pull returns the same tickets, or the
    /// same failure, and a write answers with a stored copy or a refusal.
    #[derive(Clone)]
    struct FakeAzure {
        tickets: Vec<Ticket>,
        failure: Option<String>,
        /// The copies a write is answered with, looked up by work item id, so
        /// a bulk change can be answered one work item at a time.
        stored: Vec<Ticket>,
        refusal: Option<(u16, String)>,
        /// Whether a changed-since query comes back empty: the project still
        /// lists every one of `tickets`, but none of them has moved.
        quiet: bool,
        /// The one work item whose comments and history can be read, and what
        /// they say.
        details: Option<(i64, WorkItemDetails)>,
        /// The comment a post answers with, if this fake takes comments at all.
        comment: Option<CommentRecord>,
        /// The work item a create answers with, and the links that come with
        /// it, if this fake creates work items at all.
        creation: Option<(Ticket, Vec<RelationRecord>)>,
        /// The wait every pull is turned away with, for a project Azure DevOps
        /// is shedding load from.
        throttle: Option<Duration>,
    }

    impl FakeAzure {
        fn returning(tickets: Vec<Ticket>) -> Self {
            Self {
                tickets,
                failure: None,
                stored: Vec::new(),
                refusal: None,
                quiet: false,
                details: None,
                comment: None,
                creation: None,
                throttle: None,
            }
        }

        /// Turns every pull away for throttling, naming the same wait each time.
        fn throttling(retry_after: Duration) -> Self {
            Self {
                throttle: Some(retry_after),
                ..Self::returning(Vec::new())
            }
        }

        /// Still holds `tickets`, but nothing in it has changed.
        fn quiet(tickets: Vec<Ticket>) -> Self {
            Self {
                quiet: true,
                ..Self::returning(tickets)
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                failure: Some(message.to_owned()),
                ..Self::returning(Vec::new())
            }
        }

        /// Accepts the next write and answers with `stored`.
        fn storing(stored: Ticket) -> Self {
            Self::storing_each(vec![stored])
        }

        /// Accepts a write of any of these work items and answers with the
        /// copy it holds for that one.
        fn storing_each(stored: Vec<Ticket>) -> Self {
            Self {
                stored,
                ..Self::returning(Vec::new())
            }
        }

        /// Refuses the next write, then answers pulls with `tickets`.
        fn refusing(status: u16, message: &str, tickets: Vec<Ticket>) -> Self {
            Self {
                refusal: Some((status, message.to_owned())),
                ..Self::returning(tickets)
            }
        }

        /// Accepts the next comment and answers with `comment`.
        fn commenting(comment: CommentRecord) -> Self {
            Self {
                comment: Some(comment),
                ..Self::returning(Vec::new())
            }
        }

        /// Accepts the next create and answers with `ticket` and its links.
        fn creating(ticket: Ticket, relations: Vec<RelationRecord>) -> Self {
            Self {
                creation: Some((ticket, relations)),
                ..Self::returning(Vec::new())
            }
        }

        /// Answers with `details` when that one work item is read.
        fn detailing(id: i64, details: WorkItemDetails) -> Self {
            Self {
                details: Some((id, details)),
                ..Self::returning(Vec::new())
            }
        }
    }

    impl WorkItemSource for FakeAzure {
        fn pull(&self) -> Result<SyncBatch> {
            if let Some(retry_after) = self.throttle {
                return Err(anyhow::Error::new(Throttled::new(
                    retry_after,
                    429,
                    "https://dev.azure.com/example-org/_apis/wit/wiql",
                    "too many requests",
                )));
            }
            match &self.failure {
                Some(message) => bail!("{message}"),
                None => Ok(SyncBatch {
                    tickets: self.tickets.clone(),
                    relations: Vec::new(),
                }),
            }
        }

        /// The same work items whichever way they are asked for, unless the
        /// fake was told the project has gone quiet.
        fn pull_changed_since(&self, _watermark: Timestamp) -> Result<SyncBatch> {
            if self.quiet {
                return Ok(SyncBatch::default());
            }
            self.pull()
        }

        fn list_ids(&self) -> Result<Vec<i64>> {
            match &self.failure {
                Some(message) => bail!("{message}"),
                None => Ok(self.tickets.iter().map(|ticket| ticket.key.id).collect()),
            }
        }

        fn display_name(&self) -> Result<Option<String>> {
            Ok(None)
        }

        fn patch_work_item(
            &self,
            id: i64,
            _patch: &[Value],
        ) -> Result<(Ticket, Vec<RelationRecord>)> {
            if let Some((status, message)) = &self.refusal {
                return Err(anyhow::Error::new(RequestRejected::new(
                    *status,
                    format!("https://dev.azure.com/example-org/_apis/wit/workitems/{id}"),
                    message.clone(),
                )));
            }
            match self.stored.iter().find(|ticket| ticket.key.id == id) {
                Some(ticket) => Ok((ticket.clone(), Vec::new())),
                None => bail!("the fake source was not given a stored copy of #{id}"),
            }
        }

        fn create_work_item(
            &self,
            _work_item_type: &str,
            _fields: &[Value],
            _parent: Option<i64>,
        ) -> Result<(Ticket, Vec<RelationRecord>)> {
            if let Some((status, message)) = &self.refusal {
                return Err(anyhow::Error::new(RequestRejected::new(
                    *status,
                    "https://dev.azure.com/example-org/atlas/_apis/wit/workitems/$Issue".to_owned(),
                    message.clone(),
                )));
            }
            self.creation
                .clone()
                .context("the fake source was not given a work item to create")
        }

        /// The state picker is fed from the database, so these tests never need
        /// the endpoint; answering with nothing keeps a pull from asking twice.
        fn work_item_type_states(&self, _work_item_type: &str) -> Result<Vec<StateOption>> {
            Ok(Vec::new())
        }

        fn fetch_details(&self, id: i64) -> Result<WorkItemDetails> {
            match &self.details {
                Some((detailed, details)) if *detailed == id => Ok(details.clone()),
                _ => Ok(WorkItemDetails::default()),
            }
        }

        fn post_comment(&self, _id: i64, _html: &str) -> Result<CommentRecord> {
            match self.comment.clone() {
                Some(comment) => Ok(comment),
                None => bail!("HTTP 403: the work item is read only"),
            }
        }
    }

    impl SourceConnector for FakeAzure {
        fn connect(&mut self) -> Result<Box<dyn WorkItemSource>> {
            Ok(Box::new(self.clone()))
        }
    }

    /// An app and a runtime wired to a fake Azure DevOps over a seeded database.
    fn synced_app(path: &Path, source: FakeAzure) -> (App, SqliteTicketRepository, SyncRuntime) {
        let repository = seeded_repository(path);
        let mut app = App::new(repository.load_all().unwrap());
        app.shell
            .configure_database(path.to_path_buf(), db::data_signature(path));
        app.shell.enable_sync();
        let runtime = SyncRuntime {
            worker: Some(SyncHandle::spawn(path.to_path_buf(), Box::new(source)).unwrap()),
            scheduler: SyncScheduler::new(Some(Duration::from_secs(60))),
            config: Some(AzureConfig {
                organization: "example-org".into(),
                project: "atlas".into(),
                scope: None,
            }),
            offline_reason: None,
            details: DetailsEngine::default(),
        };
        (app, repository, runtime)
    }

    /// Pumps the event loop's sync polling until the pull in flight lands.
    fn await_sync(
        app: &mut App,
        repository: &mut SqliteTicketRepository,
        runtime: &mut SyncRuntime,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.shell.sync_pending {
            poll_sync(app, repository, runtime);
            assert!(Instant::now() < deadline, "the sync worker timed out");
            thread::yield_now();
        }
    }

    /// Pumps the event loop's sync polling until the edit in flight answers.
    fn await_edit(
        app: &mut App,
        repository: &mut SqliteTicketRepository,
        runtime: &mut SyncRuntime,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.work_items.edits_pending() {
            poll_sync(app, repository, runtime);
            assert!(Instant::now() < deadline, "the sync worker timed out");
            thread::yield_now();
        }
    }

    /// Pumps the event loop's sync polling until the comment in flight answers.
    fn await_comment(
        app: &mut App,
        repository: &mut SqliteTicketRepository,
        runtime: &mut SyncRuntime,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.work_items.comments_pending() {
            poll_sync(app, repository, runtime);
            assert!(Instant::now() < deadline, "the sync worker timed out");
            thread::yield_now();
        }
    }

    /// Pumps the event loop's sync polling until the create in flight answers.
    fn await_create(
        app: &mut App,
        repository: &mut SqliteTicketRepository,
        runtime: &mut SyncRuntime,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.work_items.creates_pending() {
            poll_sync(app, repository, runtime);
            assert!(Instant::now() < deadline, "the sync worker timed out");
            thread::yield_now();
        }
    }

    fn ticket(id: i64) -> Ticket {
        Ticket {
            key: TicketKey {
                organization: "example-org".into(),
                id,
            },
            project: "atlas".into(),
            revision: 1,
            work_item_type: "Task".into(),
            title: format!("Ticket {id}"),
            state: "Active".into(),
            reason: None,
            assigned_to: Some("Avery Chen".into()),
            priority: Some(2),
            area_path: "Atlas".into(),
            iteration_path: "Atlas\\Sprint 1".into(),
            tags: Vec::new(),
            description: String::new(),
            description_html: String::new(),
            created_at: Timestamp::parse("2026-01-01T00:00:00Z").unwrap(),
            changed_at: Timestamp::parse(&format!("2026-0{id}-01T00:00:00Z")).unwrap(),
            web_url: format!("https://dev.azure.com/example-org/atlas/_workitems/edit/{id}"),
            details_rev: 0,
        }
    }

    /// The work items on the table, in the order it holds them.
    fn visible_ids(app: &App) -> Vec<i64> {
        app.work_items
            .visible_tickets()
            .map(|ticket| ticket.key.id)
            .collect()
    }

    fn seeded_repository(path: &Path) -> SqliteTicketRepository {
        let mut repository = SqliteTicketRepository::open(path).unwrap();
        let tickets: Vec<Ticket> = (1..=3).map(ticket).collect();
        repository
            .replace_all(&tickets, &TicketGraph::default())
            .unwrap();
        repository
    }

    #[test]
    fn only_well_formed_https_urls_reach_the_launcher() {
        let error = open_https_url("file:///tmp/not-a-ticket", &failing_opener).unwrap_err();
        assert!(error.to_string().contains("only HTTPS"), "{error}");
        let error = open_https_url("not a url", &failing_opener).unwrap_err();
        assert!(error.to_string().contains("invalid"), "{error}");
        let error = open_https_url("https://dev.azure.com/demo", &failing_opener).unwrap_err();
        assert!(
            error.to_string().contains("system URL launcher failed"),
            "{error}"
        );
    }

    #[test]
    fn mouse_pointer_sequences_set_and_reset_link_hover() {
        assert_eq!(
            mouse_pointer_for_hover(Some(&PointerTarget::OpenSelectedUrl), None),
            MousePointerShape::Link
        );
        assert_eq!(
            mouse_pointer_for_hover(Some(&PointerTarget::OpenInBrowser { index: 0 }), None),
            MousePointerShape::Link
        );
        assert_eq!(
            mouse_pointer_for_hover(
                Some(&PointerTarget::EditField {
                    field: ticket_tui::pointer::EditableField::State
                }),
                None
            ),
            MousePointerShape::Link,
            "an editable details field points the same way a link does"
        );
        assert_eq!(
            mouse_pointer_for_hover(Some(&PointerTarget::TableRow { index: 0 }), None),
            MousePointerShape::Default
        );
        assert_eq!(
            mouse_pointer_for_hover(
                Some(&PointerTarget::PaneDivider),
                Some(DividerOrientation::Vertical)
            ),
            MousePointerShape::ColResize
        );
        assert_eq!(
            mouse_pointer_for_hover(
                Some(&PointerTarget::PaneDivider),
                Some(DividerOrientation::Horizontal)
            ),
            MousePointerShape::RowResize
        );
        assert_eq!(
            mouse_pointer_for_hover(Some(&PointerTarget::PaneDivider), None),
            MousePointerShape::Default,
            "the narrow layout has no divider to resize"
        );

        let mut output = Vec::new();
        write_mouse_pointer_shape(&mut output, MousePointerShape::Link).unwrap();
        write_mouse_pointer_shape(&mut output, MousePointerShape::ColResize).unwrap();
        write_mouse_pointer_shape(&mut output, MousePointerShape::RowResize).unwrap();
        write_mouse_pointer_shape(&mut output, MousePointerShape::Default).unwrap();

        assert_eq!(
            output,
            b"\x1b]22;pointer\x1b\\\x1b]22;col-resize\x1b\\\x1b]22;row-resize\x1b\\\x1b]22;\x1b\\"
        );
    }

    #[test]
    fn the_stale_threshold_comes_from_the_flag_before_the_environment() {
        assert_eq!(
            resolve_stale_days(None, None).unwrap(),
            None,
            "with neither given, whatever the session remembers stands"
        );
        assert_eq!(resolve_stale_days(Some(7), None).unwrap(), Some(7));
        assert_eq!(
            resolve_stale_days(Some(7), Some("30".into())).unwrap(),
            Some(7),
            "the flag wins over TICKET_TUI_STALE_DAYS"
        );
        assert_eq!(
            resolve_stale_days(None, Some(" 30 ".into())).unwrap(),
            Some(30)
        );
        assert_eq!(
            resolve_stale_days(None, Some("   ".into())).unwrap(),
            None,
            "an empty variable is not an answer"
        );

        let error = resolve_stale_days(None, Some("a fortnight".into())).unwrap_err();
        assert!(
            format!("{error:#}").contains("TICKET_TUI_STALE_DAYS is not a number of days"),
            "{error:#}"
        );
    }

    #[test]
    fn the_refresh_interval_comes_from_the_flag_before_the_environment() {
        assert_eq!(resolve_refresh(None, None).unwrap(), 60);
        assert_eq!(resolve_refresh(Some(5), None).unwrap(), 5);
        assert_eq!(
            resolve_refresh(Some(5), Some("300".into())).unwrap(),
            5,
            "the flag wins over TICKET_TUI_REFRESH"
        );
        assert_eq!(resolve_refresh(None, Some(" 300 ".into())).unwrap(), 300);
        assert_eq!(
            resolve_refresh(None, Some("   ".into())).unwrap(),
            60,
            "a blank variable is not a setting"
        );
        assert_eq!(
            resolve_refresh(Some(0), None).unwrap(),
            0,
            "the timer can still be turned off"
        );

        let error = resolve_refresh(None, Some("hourly".into())).unwrap_err();
        assert!(
            format!("{error:#}").contains("TICKET_TUI_REFRESH is not a number of seconds"),
            "{error:#}"
        );
    }

    #[test]
    fn a_database_another_project_filled_is_browsed_rather_than_replaced() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let mut repository = seeded_repository(&path);
        repository
            .set_meta(db::ORGANIZATION_KEY, "other-org")
            .unwrap();
        repository.set_meta(db::PROJECT_KEY, "borealis").unwrap();
        let stored = repository
            .meta(db::ORGANIZATION_KEY)
            .unwrap()
            .zip(repository.meta(db::PROJECT_KEY).unwrap())
            .expect("a pull records the project it ran under");
        let stored = (stored.0.as_str(), stored.1.as_str());
        let config = AzureConfig {
            organization: "example-org".into(),
            project: "atlas".into(),
            scope: None,
        };

        let message = project_mismatch(Some(stored), &config, false)
            .expect("another project's rows are not replaced by accident");
        assert_eq!(
            message,
            "Database holds other-org/borealis; pass --database for another project or run `ticket-tui sync --full` to replace it"
        );
        assert_eq!(
            project_mismatch(Some(stored), &config, true),
            None,
            "a database with nothing in it belongs to nobody"
        );
        assert_eq!(
            project_mismatch(Some(("example-org", "atlas")), &config, false),
            None
        );
        assert_eq!(
            project_mismatch(None, &config, false),
            None,
            "a database from a build that recorded nothing adopts the project that pulls it"
        );

        // The run that finds one opens offline: no worker, and the reason both
        // in the overlay and under the sync key.
        let mut app = App::new(repository.load_all().unwrap());
        app.shell.set_offline_reason(Some(message.clone()));
        app.shell.set_sync_source(Some(sync_source(&config, 60)));
        let mut runtime = SyncRuntime {
            worker: None,
            scheduler: SyncScheduler::new(None),
            config: Some(config),
            offline_reason: Some(message.clone()),
            details: DetailsEngine::default(),
        };

        handle_action(AppAction::Sync, &mut app, &mut runtime, &failing_opener);

        assert_eq!(
            app.shell.notification(),
            Some((message.as_str(), NotificationLevel::Error))
        );
        assert!(!app.shell.sync_pending, "there is no worker to pull with");
        assert_eq!(
            app.shell.sync_summary(),
            format!("example-org/atlas every 60s · offline; {message}"),
            "the database overlay says where the rows would come from and why they do not"
        );
    }

    #[test]
    fn the_database_overlay_names_the_project_the_timer_and_the_scope() {
        let mut config = AzureConfig {
            organization: "example-org".into(),
            project: "atlas".into(),
            scope: None,
        };
        assert_eq!(sync_source(&config, 60), "example-org/atlas every 60s");
        assert_eq!(
            sync_source(&config, 0),
            "example-org/atlas on request",
            "--refresh 0 leaves r as the only way to pull"
        );

        config.scope = Some("[System.ChangedDate] > @today-180".into());
        assert_eq!(
            sync_source(&config, 300),
            "example-org/atlas every 300s · scope ([System.ChangedDate] > @today-180)"
        );

        let mut app = App::new(vec![ticket(1)]);
        app.shell.enable_sync();
        app.shell.set_sync_source(Some(sync_source(&config, 300)));
        assert_eq!(
            app.shell.sync_summary(),
            "example-org/atlas every 300s · scope ([System.ChangedDate] > @today-180) · not yet"
        );
    }

    #[test]
    fn reload_engine_loads_and_prepares_tickets_in_the_background() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        drop(seeded_repository(&path));
        let mut reloader = ReloadEngine::default();

        assert!(reloader.start(&path).unwrap());
        assert!(!reloader.start(&path).unwrap());

        let deadline = Instant::now() + Duration::from_secs(2);
        let prepared = loop {
            if let Some(result) = reloader.try_result() {
                break result.unwrap();
            }
            assert!(Instant::now() < deadline, "reload worker timed out");
            thread::yield_now();
        };
        assert_eq!(prepared.ticket_count(), 3);
    }

    #[test]
    fn a_scheduled_pull_replaces_the_tickets_and_the_table_title_follows_it() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let (mut app, mut repository, mut runtime) =
            synced_app(&path, FakeAzure::returning(vec![ticket(9)]));
        runtime.scheduler.schedule_now(Instant::now());

        assert!(dispatch_due_pull(&mut app, &mut runtime));
        assert_eq!(app.shell.activity_label().as_deref(), Some("Syncing…"));
        assert!(
            !dispatch_due_pull(&mut app, &mut runtime),
            "the timer never queues a second pull behind one in flight"
        );

        await_sync(&mut app, &mut repository, &mut runtime);
        assert_eq!(app.work_items.tickets().len(), 1);
        assert_eq!(app.work_items.tickets()[0].key.id, 9);
        assert_eq!(
            app.shell.activity_label().as_deref(),
            Some("Synced just now")
        );
        assert!(
            app.shell.notification().is_none(),
            "a timer pull says so in the title, not in a toast"
        );
        assert!(
            !poll_watch(&mut app, &repository, &mut ReloadEngine::default()),
            "the watcher does not chase the database our own worker just wrote"
        );
    }

    #[test]
    fn a_pull_that_finds_nothing_says_so_and_reloads_nothing() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let (mut app, mut repository, mut runtime) =
            synced_app(&path, FakeAzure::quiet((1..=3).map(ticket).collect()));
        // The watermark an earlier pull left is what makes this one incremental.
        repository
            .set_meta(db::WATERMARK_KEY, "2026-01-01T00:00:00Z")
            .unwrap();
        app.shell
            .configure_database(path.clone(), db::data_signature(&path));

        start_sync(&mut app, &mut runtime);
        await_sync(&mut app, &mut repository, &mut runtime);

        assert_eq!(
            app.shell.notification().map(|(message, _)| message),
            Some("Nothing changed")
        );
        assert_eq!(
            app.shell.activity_label().as_deref(),
            Some("Synced just now"),
            "the pull still happened, so the title moves"
        );
        assert_eq!(
            app.work_items.tickets().len(),
            3,
            "the rows were never replaced"
        );
        assert!(
            !poll_watch(&mut app, &repository, &mut ReloadEngine::default()),
            "an unchanged pull writes nothing, so there is nothing to reload"
        );

        assert_eq!(
            runtime.status_for(SyncMode::Incremental, 1),
            "Synced 1 change from example-org/atlas"
        );
        assert_eq!(
            runtime.status_for(SyncMode::Incremental, 3),
            "Synced 3 changes from example-org/atlas"
        );
        assert_eq!(
            runtime.status_for(SyncMode::Full, 52),
            "Synced 52 work items from example-org/atlas"
        );
    }

    #[test]
    fn a_failed_pull_keeps_the_tickets_and_reports_the_same_error_once() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let (mut app, mut repository, mut runtime) =
            synced_app(&path, FakeAzure::failing("network unreachable"));
        runtime.scheduler.schedule_now(Instant::now());

        dispatch_due_pull(&mut app, &mut runtime);
        await_sync(&mut app, &mut repository, &mut runtime);

        assert_eq!(
            app.work_items.tickets().len(),
            3,
            "a failed pull changes nothing"
        );
        let (message, level) = app
            .shell
            .notification()
            .expect("the first failure is reported");
        assert!(message.contains("network unreachable"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
        assert_eq!(app.shell.activity_label().as_deref(), Some("Sync failed"));

        app.shell.set_status("still browsing");
        runtime.scheduler.schedule_now(Instant::now());
        dispatch_due_pull(&mut app, &mut runtime);
        await_sync(&mut app, &mut repository, &mut runtime);
        assert_eq!(
            app.shell.notification().map(|(message, _)| message),
            Some("still browsing"),
            "the same timer failure is not raised again"
        );
        assert_eq!(app.shell.activity_label().as_deref(), Some("Sync failed"));
    }

    #[test]
    fn a_throttled_pull_pauses_the_timer_and_says_so_instead_of_failing() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let (mut app, mut repository, mut runtime) =
            synced_app(&path, FakeAzure::throttling(Duration::from_secs(120)));
        app.shell.set_status("still browsing");
        let start = Instant::now();
        runtime.scheduler.schedule_now(start);

        dispatch_due_pull(&mut app, &mut runtime);
        await_sync(&mut app, &mut repository, &mut runtime);

        assert_eq!(
            app.work_items.tickets().len(),
            3,
            "a throttled pull changes nothing"
        );
        assert_eq!(
            app.shell.activity_label().as_deref(),
            Some("Sync paused 2m")
        );
        assert_eq!(
            app.shell.notification().map(|(message, _)| message),
            Some("still browsing"),
            "throttling is the service working, not an error to toast"
        );
        let summary = app.shell.sync_summary();
        assert!(summary.contains("paused for throttling"), "{summary}");
        assert!(summary.contains("next in 2m"), "{summary}");

        assert!(
            !runtime.scheduler.due(start + Duration::from_secs(119)),
            "the next pull waits out the header value"
        );
        assert!(runtime.scheduler.due(start + Duration::from_secs(121)));
    }

    #[test]
    fn a_second_sync_keypress_is_reported_rather_than_queued() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let (mut app, _repository, mut runtime) =
            synced_app(&path, FakeAzure::returning(vec![ticket(9)]));

        start_sync(&mut app, &mut runtime);
        assert!(app.shell.sync_pending);
        assert!(runtime.scheduler.in_flight());

        start_sync(&mut app, &mut runtime);
        assert_eq!(
            app.shell.notification().map(|(message, _)| message),
            Some("Sync already in progress")
        );
    }

    #[test]
    fn an_offline_run_explains_why_it_cannot_sync_and_says_nothing_in_the_title() {
        let mut app = App::new(Vec::new());
        let mut runtime = SyncRuntime {
            worker: None,
            scheduler: SyncScheduler::new(None),
            config: None,
            offline_reason: Some("no Azure DevOps organization; pass --org".into()),
            details: DetailsEngine::default(),
        };

        handle_action(AppAction::Sync, &mut app, &mut runtime, &failing_opener);

        let (message, level) = app
            .shell
            .notification()
            .expect("the sync key answers offline");
        assert!(message.contains("--org"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
        assert!(!app.shell.sync_pending);
        assert_eq!(app.shell.activity_label(), None);
        assert!(offline_status(true).contains("ticket-tui sync"));
        assert!(offline_status(false).contains("offline"));
    }

    #[test]
    fn a_filed_work_item_reaches_the_table_and_a_refused_one_reopens_the_form() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let mut filed = ticket(4);
        filed.work_item_type = "Issue".into();
        filed.title = "Honour Retry-After".into();
        let parent = TicketKey {
            organization: "example-org".into(),
            id: 3,
        };
        let (mut app, mut repository, mut runtime) = synced_app(
            &path,
            FakeAzure::creating(
                filed.clone(),
                vec![RelationRecord {
                    from: filed.key.clone(),
                    to: parent.clone(),
                    kind: RelationKind::Parent,
                }],
            ),
        );

        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(app.work_items.mode, WorkItemMode::Form);
        app.work_items
            .form
            .as_mut()
            .expect("the form is open")
            .set_value(FormFieldId::Title, "Honour Retry-After");
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(action, AppAction::Create { .. }));

        handle_action(action, &mut app, &mut runtime, &failing_opener);
        assert_eq!(
            app.shell.notification().map(|(message, _)| message),
            Some("Creating Issue\u{2026}")
        );
        assert_eq!(
            app.work_items.tickets().len(),
            3,
            "nothing shows until it is stored"
        );
        await_create(&mut app, &mut repository, &mut runtime);

        assert_eq!(app.work_items.tickets().len(), 4);
        assert_eq!(
            app.work_items.selected_ticket().map(|ticket| ticket.key.id),
            Some(4),
            "the table selects the work item that was just filed"
        );
        assert_eq!(
            app.work_items.family_of(&filed.key).ancestors,
            vec![parent.clone()]
        );
        assert_eq!(
            app.work_items.family_of(&parent).children,
            vec![filed.key.clone()]
        );
        assert!(
            repository
                .load_all()
                .unwrap()
                .iter()
                .any(|ticket| ticket.key.id == 4),
            "the worker wrote it to SQLite on the way through"
        );
        assert!(
            !poll_watch(&mut app, &repository, &mut ReloadEngine::default()),
            "our own write is not another writer to reload behind"
        );

        let (mut app, mut repository, mut runtime) = synced_app(
            &path,
            FakeAzure::refusing(400, "TF401320: rule error", Vec::new()),
        );
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        app.work_items
            .form
            .as_mut()
            .expect("the form is open")
            .set_value(FormFieldId::Title, "Honour Retry-After");
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        handle_action(action, &mut app, &mut runtime, &failing_opener);
        await_create(&mut app, &mut repository, &mut runtime);

        assert_eq!(
            app.work_items.mode,
            WorkItemMode::Form,
            "the form comes back to be answered"
        );
        assert_eq!(
            app.work_items
                .form
                .as_ref()
                .unwrap()
                .value(FormFieldId::Title),
            "Honour Retry-After",
            "with everything still in it"
        );
        let (message, level) = app.shell.notification().expect("the refusal is reported");
        assert!(message.contains("TF401320: rule error"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
    }

    #[test]
    fn a_posted_comment_reaches_the_discussion_and_a_refused_one_only_the_toast() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let stored = CommentRecord {
            ticket: TicketKey {
                organization: "example-org".into(),
                id: 3,
            },
            comment_id: 11,
            created_at: Timestamp::parse("2026-03-04T09:15:00Z").unwrap(),
            author: Some("Jacob Ragsdale".into()),
            text: "Merged into main".into(),
        };
        let (mut app, mut repository, mut runtime) =
            synced_app(&path, FakeAzure::commenting(stored.clone()));
        let selected = app.work_items.selected_ticket().unwrap().key.clone();
        assert_eq!(selected.id, 3, "the newest work item starts selected");

        let action = app
            .work_items
            .comment_selected(&mut app.shell, "Merged into main".into());
        assert!(matches!(action, AppAction::Comment { .. }));
        handle_action(action, &mut app, &mut runtime, &failing_opener);
        assert_eq!(
            app.shell.notification().map(|(message, _)| message),
            Some("Posting comment on #3\u{2026}")
        );
        assert!(
            app.work_items.comments_for(&selected).is_empty(),
            "nothing shows until Azure DevOps has stored it"
        );
        await_comment(&mut app, &mut repository, &mut runtime);

        assert_eq!(app.work_items.comments_for(&selected), vec![&stored]);
        assert_eq!(
            app.shell.notification().map(|(message, _)| message),
            Some("Commented on #3")
        );
        assert_eq!(
            app.shell.data_signature,
            db::data_signature(&path),
            "the worker wrote that row, so the watcher leaves the file alone"
        );

        let (mut app, mut repository, mut runtime) =
            synced_app(&path, FakeAzure::returning(Vec::new()));
        let action = app
            .work_items
            .comment_selected(&mut app.shell, "Merged into main".into());
        handle_action(action, &mut app, &mut runtime, &failing_opener);
        await_comment(&mut app, &mut repository, &mut runtime);

        let (message, level) = app.shell.notification().expect("a refusal is reported");
        assert!(message.contains("comment not posted"), "{message}");
        assert!(message.contains("read only"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
        assert!(
            app.work_items.comments_for(&selected).is_empty(),
            "a refused comment files nothing"
        );
    }

    #[test]
    fn an_accepted_edit_updates_the_row_and_the_database_without_a_reload() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let mut stored = ticket(3);
        stored.state = "Done".into();
        stored.revision = 9;
        let (mut app, mut repository, mut runtime) =
            synced_app(&path, FakeAzure::storing(stored.clone()));
        // The edit itself is the subject, so the row it finishes stays on the
        // table for the assertions below to find it on.
        app.work_items.set_show_finished(&mut app.shell, true);
        let selected = app.work_items.selected_ticket().unwrap().key.clone();
        assert_eq!(selected.id, 3, "the newest work item starts selected");

        let action = app
            .work_items
            .edit_selected(&mut app.shell, FieldEdit::state("Done"));
        assert!(matches!(action, AppAction::Edit(_)));
        assert_eq!(
            app.work_items.selected_ticket().unwrap().state,
            "Done",
            "the row changes before the worker is even asked"
        );
        handle_action(action, &mut app, &mut runtime, &failing_opener);
        await_edit(&mut app, &mut repository, &mut runtime);

        assert_eq!(app.work_items.ticket_by_key(&selected), Some(&stored));
        assert_eq!(
            app.shell.notification().map(|(message, _)| message),
            Some("Updated #3 · State → Done")
        );
        assert_eq!(
            app.work_items.selected_ticket().map(|ticket| ticket.key.id),
            Some(3),
            "an edit landing leaves the selection where it was"
        );
        assert_eq!(
            SqliteTicketRepository::open_existing(&path)
                .unwrap()
                .load_all()
                .unwrap()
                .iter()
                .find(|ticket| ticket.key.id == 3)
                .map(|ticket| ticket.state.clone()),
            Some("Done".to_owned()),
            "the worker wrote the row it was told to write"
        );
        assert!(
            !poll_watch(&mut app, &repository, &mut ReloadEngine::default()),
            "the watcher does not chase the row our own worker just wrote"
        );
        assert!(!runtime.scheduler.in_flight(), "nothing else was asked for");
    }

    #[test]
    fn finishing_the_selected_work_item_takes_it_off_the_table_once_the_write_lands() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let stored = Ticket {
            state: "Done".into(),
            revision: 9,
            ..ticket(3)
        };
        let (mut app, mut repository, mut runtime) = synced_app(&path, FakeAzure::storing(stored));
        assert_eq!(visible_ids(&app), vec![3, 2, 1]);

        let action = app
            .work_items
            .edit_selected(&mut app.shell, FieldEdit::state("Done"));
        handle_action(action, &mut app, &mut runtime, &failing_opener);
        assert_eq!(
            visible_ids(&app),
            vec![3, 2, 1],
            "the optimistic copy stays on the table, so a refusal has a row to revert to"
        );

        await_edit(&mut app, &mut repository, &mut runtime);

        assert_eq!(
            visible_ids(&app),
            vec![2, 1],
            "the copy Azure DevOps stored is finished, so the row leaves"
        );
        assert_eq!(
            app.work_items.selected_ticket().map(|ticket| ticket.key.id),
            Some(2),
            "the cursor lands on the next piece of work rather than on nothing"
        );
        assert_eq!(
            app.work_items.hidden_finished(&app.shell),
            1,
            "and the row it took off the table is counted for the `i` overlay"
        );
    }

    #[test]
    fn a_bulk_change_writes_every_checked_work_item_and_reports_itself_once() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let stored: Vec<Ticket> = [2, 3]
            .into_iter()
            .map(|id| Ticket {
                state: "Done".into(),
                revision: 9,
                ..ticket(id)
            })
            .collect();
        let (mut app, mut repository, mut runtime) =
            synced_app(&path, FakeAzure::storing_each(stored.clone()));

        // Space checks the row under the cursor: #3, then #2 below it.
        for _ in 0..2 {
            app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        let action = app
            .work_items
            .edit_checked(&mut app.shell, FieldEdit::state("Done"));
        assert!(
            matches!(&action, AppAction::Edit(requests) if requests.len() == 2),
            "one request a checked row, got {action:?}"
        );
        handle_action(action, &mut app, &mut runtime, &failing_opener);
        await_edit(&mut app, &mut repository, &mut runtime);

        for copy in &stored {
            assert_eq!(
                app.work_items.ticket_by_key(&copy.key),
                Some(copy),
                "every checked work item carries the copy Azure DevOps stored"
            );
        }
        assert_eq!(
            app.work_items
                .ticket_by_key(&ticket(1).key)
                .map(|ticket| ticket.state.clone()),
            Some("Active".to_owned()),
            "the row that was never checked is untouched"
        );
        assert_eq!(
            app.shell.notification().map(|(message, _)| message),
            Some("Updated 2 tickets · State → Done"),
            "one summary, not one toast a work item"
        );
        assert_eq!(
            SqliteTicketRepository::open_existing(&path)
                .unwrap()
                .load_all()
                .unwrap()
                .iter()
                .filter(|ticket| ticket.state == "Done")
                .count(),
            2,
            "the worker wrote both rows it was told to write"
        );
    }

    #[test]
    fn a_conflicting_edit_puts_the_row_back_and_pulls_the_latest_copy() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let (mut app, mut repository, mut runtime) = synced_app(
            &path,
            FakeAzure::refusing(409, "the work item has been changed", vec![ticket(3)]),
        );
        let selected = app.work_items.selected_ticket().unwrap().key.clone();

        let action = app
            .work_items
            .edit_selected(&mut app.shell, FieldEdit::state("Done"));
        handle_action(action, &mut app, &mut runtime, &failing_opener);
        await_edit(&mut app, &mut repository, &mut runtime);

        assert_eq!(
            app.work_items
                .ticket_by_key(&selected)
                .map(|ticket| ticket.state.clone()),
            Some("Active".to_owned()),
            "the row goes back to what Azure DevOps still holds"
        );
        let (message, level) = app
            .shell
            .notification()
            .expect("a conflict is always reported");
        assert!(message.contains("#3 changed in Azure DevOps"), "{message}");
        assert!(message.contains("State not saved"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
        assert!(
            app.shell.sync_pending && runtime.scheduler.in_flight(),
            "a conflict asks for the latest copy straight away"
        );

        await_sync(&mut app, &mut repository, &mut runtime);
        assert_eq!(
            app.work_items.tickets().len(),
            1,
            "the pull the conflict asked for ran"
        );
    }

    #[test]
    fn an_edit_with_no_worker_left_reverts_the_row_and_says_why() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let (mut app, _repository, mut runtime) =
            synced_app(&path, FakeAzure::returning(vec![ticket(3)]));
        let action = app
            .work_items
            .edit_selected(&mut app.shell, FieldEdit::state("Done"));
        runtime.worker = None;
        runtime.offline_reason = Some("no Azure DevOps organization; pass --org".into());

        handle_action(action, &mut app, &mut runtime, &failing_opener);

        assert_eq!(
            app.work_items
                .selected_ticket()
                .map(|ticket| ticket.state.clone()),
            Some("Active".to_owned()),
            "an edit that never left is not left showing"
        );
        assert!(!app.work_items.edits_pending());
        let (message, level) = app.shell.notification().unwrap();
        assert!(message.contains("--org"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
    }

    #[test]
    fn the_watcher_reloads_another_writer_but_never_our_own_sync() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let repository = seeded_repository(&path);
        let mut app = App::new(repository.load_all().unwrap());
        app.shell
            .configure_database(path.clone(), db::data_signature(&path));
        let mut reloader = ReloadEngine::default();
        assert!(!poll_watch(&mut app, &repository, &mut reloader));

        let write = |tickets: &[Ticket]| {
            SqliteTicketRepository::open_existing(&path)
                .unwrap()
                .replace_all(tickets, &TicketGraph::default())
                .unwrap();
        };

        app.shell.sync_pending = true;
        write(&[ticket(4)]);
        assert!(
            !poll_watch(&mut app, &repository, &mut reloader),
            "a pull in flight is writing the database itself"
        );

        app.shell.sync_pending = false;
        app.shell
            .configure_database(path.clone(), db::data_signature(&path));
        assert!(
            !poll_watch(&mut app, &repository, &mut reloader),
            "applying the pull records the signature it wrote"
        );

        write(&[ticket(5), ticket(6)]);
        assert!(
            poll_watch(&mut app, &repository, &mut reloader),
            "another process writing the database still reloads"
        );
        assert!(app.shell.reload_pending);
    }

    /// Pumps the event loop's sync polling until the details fetch answers.
    fn await_details(
        app: &mut App,
        repository: &mut SqliteTicketRepository,
        runtime: &mut SyncRuntime,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.work_items.details_pending.is_some() {
            poll_sync(app, repository, runtime);
            assert!(Instant::now() < deadline, "the sync worker timed out");
            thread::yield_now();
        }
    }

    /// A work item whose details nobody has read, so the pane wants them.
    fn unread(id: i64) -> Ticket {
        let mut ticket = ticket(id);
        ticket.revision = 4;
        ticket
    }

    fn comment(id: i64, text: &str) -> CommentRecord {
        CommentRecord {
            ticket: ticket(id).key,
            comment_id: id * 10,
            created_at: Timestamp::parse("2026-03-04T00:00:00Z").unwrap(),
            author: Some("Avery Chen".into()),
            text: text.into(),
        }
    }

    fn transition(id: i64, to: &str) -> HistoryRecord {
        HistoryRecord {
            ticket: ticket(id).key,
            revision: 4,
            changed_at: Timestamp::parse("2026-03-05T10:00:00Z").unwrap(),
            changed_by: Some("Jacob Ragsdale".into()),
            field_name: "State".into(),
            old_value: Some("To Do".into()),
            new_value: Some(to.to_owned()),
        }
    }

    #[test]
    fn details_are_read_once_the_selection_settles_and_never_while_it_is_moving() {
        let start = Instant::now();
        let after = |millis: u64| start + Duration::from_millis(millis);
        let mut engine = DetailsEngine::default();
        let (first, second) = (unread(1), unread(2));

        // Scrolling: a different work item every hundred milliseconds, so the
        // rest period never runs out and nothing is asked for.
        assert_eq!(engine.due(Some(&first), start), None);
        assert_eq!(engine.due(Some(&second), after(100)), None);
        assert_eq!(engine.due(Some(&first), after(200)), None);
        assert_eq!(engine.due(Some(&first), after(400)), None);
        assert_eq!(
            engine.time_until_due(after(400)),
            Some(Duration::from_millis(100)),
            "the event loop wakes for the rest of the rest period"
        );

        assert_eq!(
            engine.due(Some(&first), after(520)),
            Some(first.key.clone()),
            "settled for longer than the rest period, so it is worth reading"
        );
        assert_eq!(
            engine.due(Some(&first), after(900)),
            None,
            "one request at a time, however long the selection sits there"
        );
        assert_eq!(engine.time_until_due(after(900)), None);

        engine.finish();
        let mut read = first.clone();
        read.details_rev = read.revision;
        assert_eq!(
            engine.due(Some(&read), after(2000)),
            None,
            "a work item whose details are already current asks for nothing"
        );
        assert_eq!(engine.due(None, after(2100)), None);
        assert_eq!(engine.time_until_due(after(2100)), None);

        // A work item that cannot be read is reported once and never asked
        // about again, however often the selection returns to it.
        assert_eq!(engine.due(Some(&second), after(3000)), None);
        assert_eq!(
            engine.due(Some(&second), after(3400)),
            Some(second.key.clone())
        );
        assert!(engine.fail(second.key.clone()));
        assert_eq!(engine.due(Some(&second), after(4000)), None);
        assert_eq!(engine.due(Some(&second), after(5000)), None);

        let third = unread(3);
        assert_eq!(engine.due(Some(&third), after(6000)), None);
        assert_eq!(
            engine.due(Some(&third), after(6400)),
            Some(third.key.clone())
        );
        assert!(
            !engine.fail(third.key.clone()),
            "one notification about a pane nobody asked to fill is enough"
        );
    }

    #[test]
    fn settling_on_a_work_item_reads_its_details_and_patches_only_its_own_rows() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let details = WorkItemDetails {
            comments: vec![comment(3, "Looks good")],
            history: vec![transition(3, "Doing")],
        };
        let (mut app, mut repository, mut runtime) =
            synced_app(&path, FakeAzure::detailing(3, details.clone()));
        let selected = app.work_items.selected_ticket().unwrap().key.clone();
        assert_eq!(selected.id, 3, "the newest work item starts selected");
        // Another work item's discussion, which this fetch must not disturb.
        let elsewhere = ticket(1).key;
        app.work_items.set_workspace_graph(
            &mut app.shell,
            TicketGraph {
                comments: vec![comment(1, "Someone else's thread")],
                history: vec![transition(1, "Done")],
                ..TicketGraph::default()
            },
        );

        assert!(
            !dispatch_due_details(&mut app, &mut runtime),
            "the selection has only just landed"
        );
        // Stand where the selection would be after the rest period, rather
        // than waiting out three hundred milliseconds of real time.
        runtime.details.resting = Some((selected.clone(), Instant::now() - DETAILS_REST));

        assert!(dispatch_due_details(&mut app, &mut runtime));
        assert_eq!(
            app.work_items.details_pending.as_ref(),
            Some(&selected),
            "the pane says it is reading while the request is out"
        );
        assert!(
            !dispatch_due_details(&mut app, &mut runtime),
            "nothing is queued behind the request in flight"
        );

        await_details(&mut app, &mut repository, &mut runtime);

        assert_eq!(
            app.work_items.comments_for(&selected),
            vec![&details.comments[0]]
        );
        assert_eq!(
            app.work_items.history_for(&selected),
            vec![&details.history[0]]
        );
        assert_eq!(
            app.work_items.comments_for(&elsewhere),
            vec![&comment(1, "Someone else's thread")],
            "another work item's discussion is left exactly as it was"
        );
        assert_eq!(
            app.work_items.history_for(&elsewhere),
            vec![&transition(1, "Done")]
        );
        assert_eq!(
            app.work_items
                .ticket_by_key(&selected)
                .map(|ticket| ticket.details_rev),
            Some(1),
            "the row records the revision its details came from"
        );
        assert!(
            app.shell.notification().is_none(),
            "a fetch nobody asked for is silent"
        );
        assert!(
            !poll_watch(&mut app, &repository, &mut ReloadEngine::default()),
            "the watcher does not chase the rows our own worker just wrote"
        );
        assert!(
            !dispatch_due_details(&mut app, &mut runtime),
            "the details are current, so nothing is asked for again"
        );
    }

    #[test]
    fn view_changes_are_published_to_the_agent_context_file() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let repository = seeded_repository(&path);
        let mut app = App::new(repository.load_all().unwrap());
        app.shell
            .configure_database(path.clone(), db::data_signature(&path));
        app.work_items.set_table_viewport(3);
        let mut publisher = AgentContextPublisher::new(&path);
        publisher.publish(&app).unwrap();

        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
        ));
        let expected = app.work_items.selected_ticket().unwrap().key.clone();
        publisher.publish(&app).unwrap();

        let context_path = agent_context::path_for(&path);
        let observed: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&context_path).unwrap()).unwrap();
        assert_eq!(
            observed["selected_ticket"]["organization"],
            expected.organization
        );
        assert_eq!(observed["selected_ticket"]["id"], expected.id);
        assert_eq!(
            observed["tickets"]["visible_rows"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(observed["schema_version"], agent_context::SCHEMA_VERSION);
        assert!(
            observed["sync"]["offline"].as_bool().unwrap(),
            "a run with no worker says so"
        );
        assert!(observed["pending_edits"].as_array().unwrap().is_empty());

        // A view that has not moved is not republished, which is what keeps the
        // file quiet enough for a watcher to trust every write it sees.
        fs::remove_file(&context_path).unwrap();
        publisher.publish(&app).unwrap();
        assert!(
            !context_path.exists(),
            "nothing changed, so nothing was written"
        );
    }

    /// A run without a worker: enough for the answers an editor hand-off gives
    /// before anything is sent anywhere.
    fn offline_runtime() -> SyncRuntime {
        SyncRuntime {
            worker: None,
            scheduler: SyncScheduler::new(None),
            config: None,
            offline_reason: Some("no Azure DevOps organization".into()),
            details: DetailsEngine::default(),
        }
    }

    /// A shell command standing in for an editor, with the file it is told to
    /// edit as `$0`. Nothing interactive is ever run in a test.
    fn fake_editor(script: &str) -> Vec<String> {
        vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()]
    }

    #[test]
    fn the_editor_is_visual_then_editor_then_vi_and_keeps_its_arguments() {
        assert_eq!(
            editor_command(Some("code --wait".into()), Some("vim".into())),
            ["code", "--wait"],
            "$VISUAL wins, and its arguments come with it"
        );
        assert_eq!(
            editor_command(None, Some("  emacs  -nw ".into())),
            ["emacs", "-nw"]
        );
        assert_eq!(editor_command(None, None), ["vi"]);
        assert_eq!(
            editor_command(Some("   ".into()), Some(String::new())),
            ["vi"],
            "a variable set to nothing is not set"
        );
        assert_eq!(
            editor_command(Some(String::new()), Some("nano".into())),
            ["nano"],
            "an empty $VISUAL falls through to $EDITOR"
        );
    }

    #[test]
    fn a_description_saved_in_the_editor_comes_back_as_html() {
        let directory = tempdir().unwrap();
        let saved = run_description_editor(
            directory.path(),
            613,
            "<p>Old words.</p>",
            &fake_editor("printf '# New\\n\\n- one\\n- two\\n' > \"$0\""),
        )
        .unwrap();

        assert_eq!(
            saved.as_deref(),
            Some("<h1>New</h1><ul><li>one</li><li>two</li></ul>")
        );

        let named = run_description_editor(
            directory.path(),
            613,
            "<p>Old words.</p>",
            &fake_editor("basename \"$0\" > \"$0\""),
        )
        .unwrap();
        assert_eq!(
            named.as_deref(),
            Some("<p>ticket-613.md</p>"),
            "the file is named after the work item it holds"
        );

        let emptied = run_description_editor(
            directory.path(),
            613,
            "<p>Old</p>",
            &fake_editor(": > \"$0\""),
        )
        .unwrap();
        assert_eq!(
            emptied.as_deref(),
            Some(""),
            "an emptied file clears the description"
        );
    }

    #[test]
    fn an_untouched_file_writes_nothing_and_an_editor_that_fails_says_so() {
        let directory = tempdir().unwrap();
        let mut app = App::new(vec![ticket(3)]);
        app.shell.enable_sync();
        app.work_items.set_table_viewport(3);
        let key = app.work_items.selected_ticket().unwrap().key.clone();
        let mut runtime = offline_runtime();

        let unchanged = run_description_editor(
            directory.path(),
            key.id,
            "<p>Left <b>alone</b>.</p>",
            &["true".to_owned()],
        )
        .unwrap();
        assert_eq!(unchanged, None, "a file nobody typed into is not an edit");
        apply_description_outcome(&mut app, &mut runtime, &key, Ok(unchanged));
        let (message, level) = app.shell.notification().expect("the run says what it did");
        assert!(message.contains("description unchanged"), "{message}");
        assert_eq!(level, NotificationLevel::Info);
        assert!(!app.work_items.edits_pending(), "nothing was sent");

        let failed = run_description_editor(
            directory.path(),
            key.id,
            "<p>Left alone.</p>",
            &["false".to_owned()],
        );
        assert!(
            failed.is_err(),
            "an editor that exits non-zero saves nothing"
        );
        apply_description_outcome(&mut app, &mut runtime, &key, failed);
        let (message, level) = app.shell.notification().expect("a failure is reported");
        assert!(message.contains("description not saved"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
        assert!(!app.work_items.edits_pending());

        let missing = run_description_editor(
            directory.path(),
            key.id,
            "<p>Left alone.</p>",
            &["definitely-not-an-editor-xyz".to_owned()],
        );
        assert!(
            missing.is_err(),
            "an editor that cannot start saves nothing"
        );
        apply_description_outcome(&mut app, &mut runtime, &key, missing);
        assert!(!app.work_items.edits_pending());
        assert_eq!(
            app.work_items.selected_ticket().unwrap().description_html,
            "",
            "the row is exactly as it was"
        );
    }

    #[test]
    fn an_edited_description_reaches_azure_devops_and_the_details_pane() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let mut stored = ticket(3);
        stored.description_html = "<p>Stored copy.</p>".into();
        stored.description = "Stored copy.".into();
        stored.revision = 9;
        let (mut app, mut repository, mut runtime) =
            synced_app(&path, FakeAzure::storing(stored.clone()));
        let key = app.work_items.selected_ticket().unwrap().key.clone();

        apply_description_outcome(
            &mut app,
            &mut runtime,
            &key,
            Ok(Some("<p>Rewritten in the editor.</p>".to_owned())),
        );
        assert_eq!(
            app.work_items.selected_ticket().unwrap().description,
            "Rewritten in the editor.",
            "the details pane reads the new description before the network answers"
        );
        await_edit(&mut app, &mut repository, &mut runtime);

        assert_eq!(app.work_items.ticket_by_key(&key), Some(&stored));
        assert_eq!(
            app.shell.notification().map(|(message, _)| message),
            Some("Updated #3 · Description → updated")
        );
    }
}
