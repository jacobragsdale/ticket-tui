use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueHint};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind,
};
use crossterm::execute;
use ticket_tui::agent_context::{self, AgentContext};
use ticket_tui::app::{
    App, AppAction, CopiedContent, DividerOrientation, PointerTarget, PreparedTickets,
};
use ticket_tui::azure::{AzureClient, AzureConfig};
use ticket_tui::db::{self, SqliteTicketRepository, default_database_path};
use ticket_tui::model::TicketGraph;
use ticket_tui::session;
use ticket_tui::sync::{
    AzureConnector, PullOrigin, SyncEvent, SyncHandle, SyncOutcome, SyncRequest, SyncScheduler,
};
use url::Url;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// SQLite database to open instead of the platform data-directory default
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    database: Option<PathBuf>,
    /// Pull every work item from Azure DevOps into the database before opening the TUI
    #[arg(long)]
    sync: bool,
    /// Azure DevOps organization (slug or URL); defaults to TICKET_TUI_ORG or `az devops configure`
    #[arg(long, value_name = "ORG")]
    org: Option<String>,
    /// Azure DevOps project; defaults to TICKET_TUI_PROJECT or `az devops configure`
    #[arg(long, value_name = "PROJECT")]
    project: Option<String>,
    /// Seconds between background pulls from Azure DevOps; 0 turns the timer off
    #[arg(long, value_name = "SECONDS", default_value_t = 60)]
    refresh: u64,
}

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
}

impl SyncRuntime {
    fn status_for(&self, count: usize) -> String {
        self.config.as_ref().map_or_else(
            || format!("Synced {count} work items"),
            |config| {
                format!(
                    "Synced {count} work items from {}/{}",
                    config.organization, config.project
                )
            },
        )
    }

    /// Gives up on syncing for the rest of the run, which only happens when the
    /// worker thread is gone.
    fn stop(&mut self, app: &mut App, error: &str) {
        self.worker = None;
        self.scheduler.stop();
        if app.fail_sync(error, true) {
            app.set_error(format!("Sync stopped: {error}"));
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
        let context = app.agent_context();
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
                    Ok(PreparedTickets::with_graph(tickets, graph))
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
    let database_path = cli.database.clone().unwrap_or_else(default_database_path);
    let mut repository = SqliteTicketRepository::open(&database_path)?;
    let schema_was_rebuilt = repository.schema_was_rebuilt();
    let (config, offline_reason) = match AzureConfig::resolve(cli.org.clone(), cli.project.clone())
    {
        Ok(config) => (Some(config), None),
        // `--sync` is an explicit request to reach Azure DevOps, so there an
        // unresolved organization stays a hard error.
        Err(error) if cli.sync => return Err(error),
        Err(error) => (None, Some(format!("{error:#}"))),
    };

    // `--sync` still blocks before the TUI opens, but no longer aborts: a
    // failure becomes a notification over whatever the database already holds.
    let startup_sync = match (cli.sync, config.as_ref()) {
        (true, Some(config)) => {
            eprintln!(
                "syncing work items from {}/{}…",
                config.base_url(),
                config.project
            );
            Some(blocking_sync(&mut repository, config))
        }
        _ => None,
    };

    let tickets = repository.load_all()?;
    let graph = repository.load_graph()?;
    let database_is_empty = tickets.is_empty();
    let mut app = App::new(tickets);
    app.set_workspace_graph(graph);
    app.set_me(resolve_me(
        repository.meta(db::ME_DISPLAY_NAME_KEY)?,
        std::env::var("TICKET_TUI_ME").ok(),
    ));
    app.configure_database(
        repository.path().to_path_buf(),
        db::data_signature(repository.path()),
    );
    let session_path = session::path_for(repository.path());
    match session::load(&session_path) {
        Ok(loaded) => app.restore_session(loaded),
        Err(error) => app.set_error(format!("Could not load session: {error:#}")),
    }

    let interval = (cli.refresh > 0).then(|| Duration::from_secs(cli.refresh));
    let mut runtime = SyncRuntime {
        worker: None,
        scheduler: SyncScheduler::new(interval),
        config: config.clone(),
        offline_reason,
    };
    if let Some(config) = config {
        runtime.worker = Some(SyncHandle::spawn(
            database_path.clone(),
            Box::new(AzureConnector::new(config)),
        )?);
        app.enable_sync();
        let now = Instant::now();
        if pull_at_startup(
            startup_sync.is_some(),
            interval.is_some(),
            schema_was_rebuilt,
            database_is_empty,
        ) {
            runtime.scheduler.schedule_now(now);
        } else {
            runtime.scheduler.schedule_next(now);
        }
    }

    match startup_sync {
        Some(Ok(status)) => {
            app.finish_sync();
            app.set_status(status);
        }
        Some(Err(error)) => {
            let error = format!("{error:#}");
            app.fail_sync(&error, true);
            app.set_error(format!("Sync failed: {error}"));
        }
        None if runtime.worker.is_none() => app.set_status(offline_status(database_is_empty)),
        None => {}
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

/// The `--sync` pull, run before the TUI opens. Reports the status line to
/// show, leaving the error to the caller: an unreachable Azure DevOps is a
/// notification over the existing database, not a reason to refuse to start.
fn blocking_sync(repository: &mut SqliteTicketRepository, config: &AzureConfig) -> Result<String> {
    let client = AzureClient::connect(config.clone())?;
    let batch = client.fetch_all_work_items()?;
    let graph = TicketGraph {
        relations: batch.relations,
        ..TicketGraph::default()
    };
    let count = repository.replace_all(&batch.tickets, &graph)?;
    if let Some(display_name) = client.current_user_display_name()? {
        repository.set_meta(db::ME_DISPLAY_NAME_KEY, &display_name)?;
    }
    Ok(format!(
        "Synced {count} work items from {}/{}",
        config.organization, config.project
    ))
}

/// Whether the first background pull goes out as the TUI opens. `--sync`
/// already pulled, so the timer takes over one interval later; otherwise the
/// TUI opens from the database and pulls straight away — even with the timer
/// off, when the database was just rebuilt or holds nothing to browse.
const fn pull_at_startup(
    synced_at_startup: bool,
    timer_enabled: bool,
    schema_was_rebuilt: bool,
    database_is_empty: bool,
) -> bool {
    !synced_at_startup && (timer_enabled || schema_was_rebuilt || database_is_empty)
}

/// What a run without a configured organization opens with.
fn offline_status(database_is_empty: bool) -> &'static str {
    if database_is_empty {
        "Database is empty and offline; run with --sync --org ORG --project PROJECT to pull work items"
    } else {
        "Browsing the database offline; no Azure DevOps organization is configured"
    }
}

/// Who "mine" means: the display name the last sync recorded, overridden by
/// `TICKET_TUI_ME` for anyone whose profile name differs from the name their
/// work items are assigned to. Blank values count as unset.
fn resolve_me(stored: Option<String>, env: Option<String>) -> Option<String> {
    [env, stored]
        .into_iter()
        .flatten()
        .map(|name| name.trim().to_owned())
        .find(|name| !name.is_empty())
}

fn run_terminal(
    app: &mut App,
    repository: &mut SqliteTicketRepository,
    runtime: &mut SyncRuntime,
    context_publisher: &mut AgentContextPublisher,
) -> Result<()> {
    let mut terminal = ratatui::init();
    let _restore = TerminalRestore;
    let opener = SystemUrlOpener;
    let mut reloader = ReloadEngine::default();
    let mut mouse_pointer = MousePointerShape::Default;
    execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)
        .context("failed to enable terminal input features")?;

    let mut redraw = true;
    while !app.should_quit {
        redraw |= app.poll_search();
        redraw |= poll_reload(app, repository, &mut reloader);
        redraw |= poll_sync(app, repository, runtime);
        redraw |= poll_watch(app, repository, &mut reloader);
        redraw |= dispatch_due_pull(app, runtime);
        redraw |= persist_session(app, repository);
        redraw |= app.tick();
        if redraw {
            terminal.draw(|frame| ticket_tui::ui::render(frame, app))?;
            sync_mouse_pointer(app, &mut mouse_pointer);
            redraw = false;
            if let Err(error) = context_publisher.publish(app) {
                app.set_error(format!("Could not publish agent context: {error:#}"));
                redraw = true;
            }
        }

        let timeout = if app.search_pending || app.reload_pending {
            Duration::from_millis(33)
        } else {
            // The loop has to wake for the next scheduled pull as well as for
            // an expiring notification.
            [
                app.next_wakeup(),
                runtime.scheduler.time_until_due(Instant::now()),
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
                app.handle_resize();
                (AppAction::None, true)
            }
            Event::FocusGained | Event::FocusLost | Event::Key(_) => (AppAction::None, false),
        };
        if event_redraw {
            redraw = true;
        }
        handle_action(action, app, runtime, &opener);
    }
    Ok(())
}

fn handle_action(
    action: AppAction,
    app: &mut App,
    runtime: &mut SyncRuntime,
    opener: &dyn UrlOpener,
) {
    match action {
        AppAction::None => {}
        AppAction::Sync => start_sync(app, runtime),
        AppAction::OpenUrl(raw_url) => match open_https_url(&raw_url, opener) {
            Ok(()) => app.set_status(format!("Opened {raw_url}")),
            Err(error) => app.set_error(format!("Could not open ticket: {error:#}")),
        },
        AppAction::Copy { text, content } => match copy_to_clipboard(&text) {
            Ok(()) => app.set_status(copied_status(content)),
            Err(error) => app.set_error(format!("Could not copy: {error:#}")),
        },
        AppAction::WriteFile { path, contents } => match fs::write(&path, contents) {
            Ok(()) => app.set_status(format!("Exported {}", path.display())),
            Err(error) => app.set_error(format!("Could not export {}: {error:#}", path.display())),
        },
    }
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
            app.reload_pending = true;
            app.set_status(message);
        }
        Ok(false) => app.set_status("Reload already in progress"),
        Err(error) => app.set_error(format!("Could not start reload: {error:#}")),
    }
}

/// Asks for the pull the timer has booked, if one is due. Nothing is ever
/// queued behind a pull already in flight.
fn dispatch_due_pull(app: &mut App, runtime: &mut SyncRuntime) -> bool {
    if runtime.worker.is_none() || !runtime.scheduler.due(Instant::now()) {
        return false;
    }
    runtime.scheduler.start();
    app.begin_sync();
    send_pull(app, runtime, PullOrigin::Timer);
    true
}

/// `r`: pull now, whatever the timer is doing.
fn start_sync(app: &mut App, runtime: &mut SyncRuntime) {
    if runtime.worker.is_none() {
        let reason = runtime
            .offline_reason
            .clone()
            .unwrap_or_else(|| "Azure DevOps is not configured".to_owned());
        app.set_error(reason);
        return;
    }
    if !runtime.scheduler.request_user_pull() {
        app.set_status("Sync already in progress");
        return;
    }
    app.begin_sync();
    app.set_status("Syncing from Azure DevOps…");
    send_pull(app, runtime, PullOrigin::User);
}

fn send_pull(app: &mut App, runtime: &mut SyncRuntime, origin: PullOrigin) {
    let sent = runtime
        .worker
        .as_ref()
        .map(|worker| worker.send(SyncRequest::Pull(origin)));
    if let Some(Err(error)) = sent {
        runtime.stop(app, &format!("{error:#}"));
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
                    app.set_error(format!("Could not record the signed-in name: {error:#}"));
                }
                app.set_me(resolve_me(Some(name), std::env::var("TICKET_TUI_ME").ok()));
            }
            SyncEvent::Finished { origin, outcome } => {
                runtime.scheduler.finish(Instant::now());
                match outcome {
                    SyncOutcome::Pulled { prepared, count } => {
                        app.replace_prepared_tickets(prepared);
                        app.finish_sync();
                        app.configure_database(
                            repository.path().to_path_buf(),
                            db::data_signature(repository.path()),
                        );
                        if origin == PullOrigin::User {
                            app.set_status(runtime.status_for(count));
                        }
                    }
                    // A timer pull that keeps failing the same way says so in
                    // the table title rather than in a toast every minute.
                    SyncOutcome::Failed(error) => {
                        if app.fail_sync(&error, origin == PullOrigin::User) {
                            app.set_error(format!("Sync failed: {error}"));
                        }
                    }
                }
            }
            SyncEvent::Stopped => {
                runtime.stop(app, "the Azure DevOps sync worker stopped");
            }
        }
    }
    redraw
}

/// Another process writing the database — an agent, or `ticket-tui --sync` in
/// another terminal — still reloads the rows from SQLite.
fn poll_watch(
    app: &mut App,
    repository: &SqliteTicketRepository,
    reloader: &mut ReloadEngine,
) -> bool {
    let signature = db::data_signature(repository.path());
    if signature == app.data_signature || app.reload_pending || app.sync_pending {
        return false;
    }
    app.mark_stale();
    start_reload(app, repository, reloader, "Database changed; reloading…");
    true
}

fn persist_session(app: &mut App, repository: &SqliteTicketRepository) -> bool {
    if !app.session_dirty {
        return false;
    }
    let path = session::path_for(repository.path());
    match session::save(&path, &app.snapshot_session()) {
        Ok(()) => app.session_dirty = false,
        Err(error) => app.set_error(format!("Could not save session: {error:#}")),
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
    let desired = mouse_pointer_for_hover(app.hovered(), app.divider_orientation());
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
        Some(PointerTarget::OpenTicket { .. } | PointerTarget::OpenSelectedUrl) => {
            MousePointerShape::Link
        }
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
    app.reload_pending = false;
    match result {
        Ok(prepared) => {
            let count = prepared.ticket_count();
            app.replace_prepared_tickets(prepared);
            app.configure_database(
                repository.path().to_path_buf(),
                db::data_signature(repository.path()),
            );
            app.set_status(format!("Reloaded {count} tickets"));
        }
        Err(error) => app.set_error(format!("Reload failed: {error}")),
    }
    true
}

fn open_https_url(raw_url: &str, opener: &dyn UrlOpener) -> Result<()> {
    let url = Url::parse(raw_url).context("ticket URL is invalid")?;
    if url.scheme() != "https" {
        bail!("only HTTPS ticket URLs can be opened");
    }
    opener.open(&url).context("system URL launcher failed")
}

trait UrlOpener {
    fn open(&self, url: &Url) -> Result<()>;
}

struct SystemUrlOpener;

impl UrlOpener for SystemUrlOpener {
    fn open(&self, url: &Url) -> Result<()> {
        open::that(url.as_str()).map_err(Into::into)
    }
}

struct TerminalRestore;

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = write_mouse_pointer_shape(&mut io::stdout(), MousePointerShape::Default);
        let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
        ratatui::restore();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use tempfile::tempdir;
    use ticket_tui::app::NotificationLevel;
    use ticket_tui::azure::SyncBatch;
    use ticket_tui::model::{Ticket, TicketGraph, TicketKey};
    use ticket_tui::sync::{SourceConnector, WorkItemSource};
    use ticket_tui::timestamp::Timestamp;

    struct FailingOpener;

    /// Azure DevOps stood in for: every pull returns the same tickets, or the
    /// same failure.
    #[derive(Clone)]
    struct FakeAzure {
        tickets: Vec<Ticket>,
        failure: Option<String>,
    }

    impl FakeAzure {
        fn returning(tickets: Vec<Ticket>) -> Self {
            Self {
                tickets,
                failure: None,
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                tickets: Vec::new(),
                failure: Some(message.to_owned()),
            }
        }
    }

    impl WorkItemSource for FakeAzure {
        fn pull(&self) -> Result<SyncBatch> {
            match &self.failure {
                Some(message) => bail!("{message}"),
                None => Ok(SyncBatch {
                    tickets: self.tickets.clone(),
                    relations: Vec::new(),
                }),
            }
        }

        fn display_name(&self) -> Result<Option<String>> {
            Ok(None)
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
        app.configure_database(path.to_path_buf(), db::data_signature(path));
        app.enable_sync();
        let runtime = SyncRuntime {
            worker: Some(SyncHandle::spawn(path.to_path_buf(), Box::new(source)).unwrap()),
            scheduler: SyncScheduler::new(Some(Duration::from_secs(60))),
            config: Some(AzureConfig {
                organization: "example-org".into(),
                project: "atlas".into(),
            }),
            offline_reason: None,
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
        while app.sync_pending {
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
            created_at: Timestamp::parse("2026-01-01T00:00:00Z").unwrap(),
            changed_at: Timestamp::parse(&format!("2026-0{id}-01T00:00:00Z")).unwrap(),
            web_url: format!("https://dev.azure.com/example-org/atlas/_workitems/edit/{id}"),
        }
    }

    fn seeded_repository(path: &Path) -> SqliteTicketRepository {
        let mut repository = SqliteTicketRepository::open(path).unwrap();
        let tickets: Vec<Ticket> = (1..=3).map(ticket).collect();
        repository
            .replace_all(&tickets, &TicketGraph::default())
            .unwrap();
        repository
    }

    impl UrlOpener for FailingOpener {
        fn open(&self, _url: &Url) -> Result<()> {
            bail!("launcher unavailable")
        }
    }

    #[test]
    fn only_well_formed_https_urls_reach_the_launcher() {
        let error = open_https_url("file:///tmp/not-a-ticket", &FailingOpener).unwrap_err();
        assert!(error.to_string().contains("only HTTPS"), "{error}");
        let error = open_https_url("not a url", &FailingOpener).unwrap_err();
        assert!(error.to_string().contains("invalid"), "{error}");
        let error = open_https_url("https://dev.azure.com/demo", &FailingOpener).unwrap_err();
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
            mouse_pointer_for_hover(Some(&PointerTarget::OpenTicket { index: 0 }), None),
            MousePointerShape::Link
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
        assert_eq!(app.activity_label().as_deref(), Some("Syncing…"));
        assert!(
            !dispatch_due_pull(&mut app, &mut runtime),
            "the timer never queues a second pull behind one in flight"
        );

        await_sync(&mut app, &mut repository, &mut runtime);
        assert_eq!(app.tickets().len(), 1);
        assert_eq!(app.tickets()[0].key.id, 9);
        assert_eq!(app.activity_label().as_deref(), Some("Synced just now"));
        assert!(
            app.notification().is_none(),
            "a timer pull says so in the title, not in a toast"
        );
        assert!(
            !poll_watch(&mut app, &repository, &mut ReloadEngine::default()),
            "the watcher does not chase the database our own worker just wrote"
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

        assert_eq!(app.tickets().len(), 3, "a failed pull changes nothing");
        let (message, level) = app.notification().expect("the first failure is reported");
        assert!(message.contains("network unreachable"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
        assert_eq!(app.activity_label().as_deref(), Some("Sync failed"));

        app.set_status("still browsing");
        runtime.scheduler.schedule_now(Instant::now());
        dispatch_due_pull(&mut app, &mut runtime);
        await_sync(&mut app, &mut repository, &mut runtime);
        assert_eq!(
            app.notification().map(|(message, _)| message),
            Some("still browsing"),
            "the same timer failure is not raised again"
        );
        assert_eq!(app.activity_label().as_deref(), Some("Sync failed"));
    }

    #[test]
    fn a_second_sync_keypress_is_reported_rather_than_queued() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let (mut app, _repository, mut runtime) =
            synced_app(&path, FakeAzure::returning(vec![ticket(9)]));

        start_sync(&mut app, &mut runtime);
        assert!(app.sync_pending);
        assert!(runtime.scheduler.in_flight());

        start_sync(&mut app, &mut runtime);
        assert_eq!(
            app.notification().map(|(message, _)| message),
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
        };

        handle_action(AppAction::Sync, &mut app, &mut runtime, &FailingOpener);

        let (message, level) = app.notification().expect("the sync key answers offline");
        assert!(message.contains("--org"), "{message}");
        assert_eq!(level, NotificationLevel::Error);
        assert!(!app.sync_pending);
        assert_eq!(app.activity_label(), None);
        assert!(offline_status(true).contains("--sync"));
        assert!(offline_status(false).contains("offline"));
    }

    #[test]
    fn the_watcher_reloads_another_writer_but_never_our_own_sync() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let repository = seeded_repository(&path);
        let mut app = App::new(repository.load_all().unwrap());
        app.configure_database(path.clone(), db::data_signature(&path));
        let mut reloader = ReloadEngine::default();
        assert!(!poll_watch(&mut app, &repository, &mut reloader));

        let write = |tickets: &[Ticket]| {
            SqliteTicketRepository::open_existing(&path)
                .unwrap()
                .replace_all(tickets, &TicketGraph::default())
                .unwrap();
        };

        app.sync_pending = true;
        write(&[ticket(4)]);
        assert!(
            !poll_watch(&mut app, &repository, &mut reloader),
            "a pull in flight is writing the database itself"
        );

        app.sync_pending = false;
        app.configure_database(path.clone(), db::data_signature(&path));
        assert!(
            !poll_watch(&mut app, &repository, &mut reloader),
            "applying the pull records the signature it wrote"
        );

        write(&[ticket(5), ticket(6)]);
        assert!(
            poll_watch(&mut app, &repository, &mut reloader),
            "another process writing the database still reloads"
        );
        assert!(app.reload_pending);
    }

    #[test]
    fn the_first_pull_goes_out_at_startup_unless_sync_already_pulled() {
        assert!(
            pull_at_startup(false, true, false, false),
            "the timer pulls as soon as the TUI opens"
        );
        assert!(
            !pull_at_startup(true, true, false, false),
            "--sync already pulled, so the timer waits an interval"
        );
        assert!(
            pull_at_startup(false, false, true, false),
            "a rebuilt schema is filled even with --refresh 0"
        );
        assert!(
            pull_at_startup(false, false, false, true),
            "an empty database is filled even with --refresh 0"
        );
        assert!(
            !pull_at_startup(false, false, false, false),
            "--refresh 0 over a populated database waits for the sync key"
        );
        assert!(!pull_at_startup(true, false, true, true));
    }

    #[test]
    fn view_changes_are_published_to_the_agent_context_file() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let repository = seeded_repository(&path);
        let mut app = App::new(repository.load_all().unwrap());
        app.configure_database(path.clone(), db::data_signature(&path));
        app.set_table_viewport(3);
        let mut publisher = AgentContextPublisher::new(&path);
        publisher.publish(&app).unwrap();

        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
        ));
        let expected = app.selected_ticket().unwrap().key.clone();
        publisher.publish(&app).unwrap();

        let context_path = agent_context::path_for(&path);
        let observed: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(context_path).unwrap()).unwrap();
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
    }
}
