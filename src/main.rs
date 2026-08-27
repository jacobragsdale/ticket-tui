use std::ffi::OsStr;
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
use ticket_tui::app::{App, AppAction, PreparedTickets};
use ticket_tui::db::{self, SqliteTicketRepository, default_database_path};
use ticket_tui::import::{self, ImportFormat};
use ticket_tui::session;
use url::Url;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// SQLite database to open instead of the platform data-directory default
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    database: Option<PathBuf>,
    /// Open the database without migrating, seeding, or journal changes
    #[arg(long)]
    read_only: bool,
    /// Import a local JSON or CSV file before opening the TUI
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    import: Option<PathBuf>,
    /// Capture the mouse for in-app clicks and scrolling
    #[arg(long, overrides_with = "no_mouse")]
    mouse: bool,
    /// Leave the mouse to the terminal so drag-select can copy
    #[arg(long, overrides_with = "mouse")]
    no_mouse: bool,
}

impl Cli {
    fn mouse_override(&self) -> Option<bool> {
        if self.mouse {
            Some(true)
        } else if self.no_mouse {
            Some(false)
        } else {
            None
        }
    }
}

#[derive(Default)]
struct ReloadEngine {
    receiver: Option<Receiver<std::result::Result<PreparedTickets, String>>>,
}

impl ReloadEngine {
    fn start(&mut self, path: &Path, read_only: bool) -> Result<bool> {
        if self.receiver.is_some() {
            return Ok(false);
        }

        let path = path.to_path_buf();
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("ticket-reload".into())
            .spawn(move || {
                let result = (|| -> Result<PreparedTickets> {
                    let repository = if read_only {
                        SqliteTicketRepository::open_read_only(&path)?
                    } else {
                        SqliteTicketRepository::open(&path)?.repository
                    };
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

fn running_in_herdr() -> bool {
    std::env::var_os("HERDR_ENV").as_deref() == Some(OsStr::new("1"))
}

fn should_capture_mouse(explicit: Option<bool>, in_herdr: bool) -> bool {
    explicit.unwrap_or(!in_herdr)
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let capture_mouse = should_capture_mouse(cli.mouse_override(), running_in_herdr());
    let database_path = cli.database.unwrap_or_else(default_database_path);
    if cli.read_only && cli.import.is_some() {
        bail!("--import cannot be used with --read-only");
    }
    let (mut repository, seeded_demo_data) = if cli.read_only {
        (
            SqliteTicketRepository::open_read_only(&database_path)?,
            false,
        )
    } else {
        let opened = SqliteTicketRepository::open(&database_path)?;
        (opened.repository, opened.seeded_demo_data)
    };
    if let Some(import_path) = &cli.import {
        let report = import_file(&mut repository, import_path, import_format(import_path))?;
        eprintln!("imported {report}");
    }
    let tickets = repository.load_all()?;
    let graph = repository.load_graph()?;
    let mut app = App::new(tickets);
    app.set_workspace_graph(graph);
    app.configure_database(
        repository.path().to_path_buf(),
        cli.read_only,
        db::data_signature(repository.path()),
    );
    let session_path = session::path_for(repository.path());
    match session::load(&session_path) {
        Ok(loaded) => app.restore_session(loaded),
        Err(error) => app.set_error(format!("Could not load session: {error:#}")),
    }
    if seeded_demo_data {
        app.set_status(format!(
            "Created demo database with 500 tickets at {}",
            repository.path().display()
        ));
    } else if cli.read_only {
        app.set_status(format!("Opened {} read-only", repository.path().display()));
    }
    let result = run_terminal(&mut app, &mut repository, capture_mouse);
    if let Err(error) = session::save(&session_path, &app.snapshot_session()) {
        eprintln!("warning: could not save session: {error:#}");
    }
    result
}

fn run_terminal(
    app: &mut App,
    repository: &mut SqliteTicketRepository,
    capture_mouse: bool,
) -> Result<()> {
    let mut terminal = ratatui::init();
    let _restore = TerminalRestore;
    let opener = SystemUrlOpener;
    let mut reloader = ReloadEngine::default();
    if capture_mouse {
        execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)
            .context("failed to enable terminal input features")?;
    } else {
        execute!(io::stdout(), EnableBracketedPaste)
            .context("failed to enable terminal input features")?;
    }

    let mut redraw = true;
    while !app.should_quit {
        redraw |= app.poll_search();
        redraw |= poll_reload(app, repository, &mut reloader);
        redraw |= poll_watch(app, repository, &mut reloader);
        redraw |= persist_session(app, repository);
        redraw |= app.tick();
        if redraw {
            terminal.draw(|frame| ticket_tui::ui::render(frame, app))?;
            redraw = false;
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
        redraw = true;
        let action = match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
            Event::Mouse(mouse) => app.handle_mouse(mouse),
            Event::Paste(text) => {
                app.handle_paste(&text);
                AppAction::None
            }
            Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Key(_) => {
                AppAction::None
            }
        };
        handle_action(action, app, repository, &opener, &mut reloader);
    }
    Ok(())
}

fn handle_action(
    action: AppAction,
    app: &mut App,
    repository: &mut SqliteTicketRepository,
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
        AppAction::Copy(text) => match copy_to_clipboard(&text) {
            Ok(()) => app.set_status("Copied to clipboard"),
            Err(error) => app.set_error(format!("Could not copy: {error:#}")),
        },
        AppAction::WriteFile { path, contents } => match fs::write(&path, contents) {
            Ok(()) => app.set_status(format!("Exported {}", path.display())),
            Err(error) => app.set_error(format!("Could not export {}: {error:#}", path.display())),
        },
        AppAction::Import { path, format } => match import_file(repository, &path, format) {
            Ok(summary) => {
                app.set_status(format!("Imported {summary}"));
                start_reload(app, repository, reloader, "Reloading imported tickets…");
            }
            Err(error) => app.set_error(format!("Import failed: {error:#}")),
        },
    }
}

fn start_reload(
    app: &mut App,
    repository: &SqliteTicketRepository,
    reloader: &mut ReloadEngine,
    message: &str,
) {
    match reloader.start(repository.path(), app.read_only) {
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

fn import_file(
    repository: &mut SqliteTicketRepository,
    path: &Path,
    format: ImportFormat,
) -> Result<String> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let batch = match format {
        ImportFormat::Json => import::parse_json(&raw),
        ImportFormat::Csv => import::parse_csv(&raw),
    };
    if batch.tickets.is_empty() {
        let details = batch
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        bail!(
            "no valid tickets in {}{}",
            path.display(),
            if details.is_empty() {
                String::new()
            } else {
                format!(" ({details})")
            }
        );
    }
    repository.import_batch(&batch)?;
    let mut summary = batch.summary();
    if !batch.diagnostics.is_empty() {
        let issues: Vec<_> = batch.diagnostics.iter().map(ToString::to_string).collect();
        summary = format!("{summary}; issues: {}", issues.join("; "));
    }
    Ok(summary)
}

fn import_format(path: &Path) -> ImportFormat {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("csv") => ImportFormat::Csv,
        _ => ImportFormat::Json,
    }
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
                app.read_only,
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
        let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
        ratatui::restore();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use tempfile::tempdir;

    struct FailingOpener;

    impl UrlOpener for FailingOpener {
        fn open(&self, _url: &Url) -> Result<()> {
            bail!("launcher unavailable")
        }
    }

    #[test]
    fn mouse_capture_defaults_on_outside_herdr_and_off_inside() {
        assert!(should_capture_mouse(None, false));
        assert!(!should_capture_mouse(None, true));
        assert!(should_capture_mouse(Some(true), true));
        assert!(!should_capture_mouse(Some(false), false));
    }

    #[test]
    fn mouse_flags_override_each_other() {
        let default = Cli::try_parse_from(["ticket-tui"]).unwrap();
        assert_eq!(default.mouse_override(), None);

        let mouse = Cli::try_parse_from(["ticket-tui", "--mouse"]).unwrap();
        assert_eq!(mouse.mouse_override(), Some(true));

        let no_mouse = Cli::try_parse_from(["ticket-tui", "--no-mouse"]).unwrap();
        assert_eq!(no_mouse.mouse_override(), Some(false));

        let last_wins = Cli::try_parse_from(["ticket-tui", "--mouse", "--no-mouse"]).unwrap();
        assert_eq!(last_wins.mouse_override(), Some(false));
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
    fn reload_engine_loads_and_prepares_tickets_in_the_background() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tickets.sqlite3");
        let opened = SqliteTicketRepository::open(&path).unwrap();
        drop(opened);
        let mut reloader = ReloadEngine::default();

        assert!(reloader.start(&path, false).unwrap());
        assert!(!reloader.start(&path, false).unwrap());

        let deadline = Instant::now() + Duration::from_secs(2);
        let prepared = loop {
            if let Some(result) = reloader.try_result() {
                break result.unwrap();
            }
            assert!(Instant::now() < deadline, "reload worker timed out");
            thread::yield_now();
        };
        assert_eq!(prepared.ticket_count(), 500);
    }
}
