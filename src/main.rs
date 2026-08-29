use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueHint};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind,
};
use crossterm::execute;
use ticket_tui::agent_context::{self, AgentContext};
use ticket_tui::app::{App, AppAction, CopiedContent, PointerTarget, PreparedTickets};
use ticket_tui::azure::{AzureClient, AzureConfig};
use ticket_tui::db::{self, SqliteTicketRepository, default_database_path};
use ticket_tui::model::TicketGraph;
use ticket_tui::session;
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
                    let repository = SqliteTicketRepository::open(&path)?;
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
    let database_path = cli.database.unwrap_or_else(default_database_path);
    let mut repository = SqliteTicketRepository::open(&database_path)?;
    let mut sync_status = None;
    if cli.sync {
        let config = AzureConfig::resolve(cli.org.clone(), cli.project.clone())?;
        eprintln!(
            "syncing work items from {}/{}…",
            config.base_url(),
            config.project
        );
        let client = AzureClient::connect(config)?;
        let batch = client.fetch_all_work_items()?;
        let graph = TicketGraph {
            relations: batch.relations,
            ..TicketGraph::default()
        };
        let count = repository.replace_all(&batch.tickets, &graph)?;
        if let Some(display_name) = client.current_user_display_name()? {
            repository.set_meta(db::ME_DISPLAY_NAME_KEY, &display_name)?;
        }
        sync_status = Some(format!(
            "Synced {count} work items from {}/{}",
            client.config().organization,
            client.config().project
        ));
    }
    let tickets = repository.load_all()?;
    let graph = repository.load_graph()?;
    let cache_is_empty = tickets.is_empty();
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
    if let Some(status) = sync_status {
        app.set_status(status);
    } else if cache_is_empty {
        app.set_status("Cache is empty; run with --sync to pull work items from Azure DevOps");
    }
    let mut context_publisher = AgentContextPublisher::new(repository.path());
    let result = run_terminal(&mut app, &repository, &mut context_publisher);
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
    repository: &SqliteTicketRepository,
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
        redraw |= poll_watch(app, repository, &mut reloader);
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
            app.next_wakeup()
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
        handle_action(action, app, repository, &opener, &mut reloader);
    }
    Ok(())
}

fn handle_action(
    action: AppAction,
    app: &mut App,
    repository: &SqliteTicketRepository,
    opener: &dyn UrlOpener,
    reloader: &mut ReloadEngine,
) {
    match action {
        AppAction::None => {}
        AppAction::Reload => start_reload(app, repository, reloader, "Reloading tickets…"),
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

fn poll_watch(
    app: &mut App,
    repository: &SqliteTicketRepository,
    reloader: &mut ReloadEngine,
) -> bool {
    let signature = db::data_signature(repository.path());
    if signature == app.data_signature || app.reload_pending {
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
}

impl MousePointerShape {
    const fn escape_sequence(self) -> &'static [u8] {
        match self {
            Self::Default => b"\x1b]22;\x1b\\",
            Self::Link => b"\x1b]22;pointer\x1b\\",
        }
    }
}

fn sync_mouse_pointer(app: &App, current: &mut MousePointerShape) {
    let desired = mouse_pointer_for_hover(app.hovered());
    if desired == *current {
        return;
    }
    if write_mouse_pointer_shape(&mut io::stdout(), desired).is_ok() {
        *current = desired;
    }
}

fn mouse_pointer_for_hover(target: Option<&PointerTarget>) -> MousePointerShape {
    match target {
        Some(PointerTarget::OpenTicket { .. } | PointerTarget::OpenSelectedUrl) => {
            MousePointerShape::Link
        }
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
    use ticket_tui::model::{Ticket, TicketGraph, TicketKey};
    use ticket_tui::timestamp::Timestamp;

    struct FailingOpener;

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
    fn rejects_non_https_ticket_urls() {
        let error = open_https_url("file:///tmp/not-a-ticket", &FailingOpener).unwrap_err();
        assert!(error.to_string().contains("only HTTPS"));
    }

    #[test]
    fn rejects_malformed_ticket_urls() {
        let error = open_https_url("not a url", &FailingOpener).unwrap_err();
        assert!(error.to_string().contains("invalid"));
    }

    #[test]
    fn reports_launcher_failures_without_opening_a_browser() {
        let error = open_https_url("https://dev.azure.com/demo", &FailingOpener).unwrap_err();
        assert!(error.to_string().contains("system URL launcher failed"));
    }

    #[test]
    fn clipboard_status_names_the_copied_content() {
        assert_eq!(
            copied_status(CopiedContent::Url),
            "Copied url to clipboard!"
        );
        assert_eq!(
            copied_status(CopiedContent::MarkdownLink),
            "Copied markdown link to clipboard!"
        );
    }

    #[test]
    fn mouse_pointer_sequences_set_and_reset_link_hover() {
        assert_eq!(
            mouse_pointer_for_hover(Some(&PointerTarget::OpenSelectedUrl)),
            MousePointerShape::Link
        );
        assert_eq!(
            mouse_pointer_for_hover(Some(&PointerTarget::OpenTicket { index: 0 })),
            MousePointerShape::Link
        );
        assert_eq!(
            mouse_pointer_for_hover(Some(&PointerTarget::TableRow { index: 0 })),
            MousePointerShape::Default
        );

        let mut output = Vec::new();
        write_mouse_pointer_shape(&mut output, MousePointerShape::Link).unwrap();
        write_mouse_pointer_shape(&mut output, MousePointerShape::Default).unwrap();

        assert_eq!(output, b"\x1b]22;pointer\x1b\\\x1b]22;\x1b\\");
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
