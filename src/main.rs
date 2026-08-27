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
use ticket_tui::db::{SqliteTicketRepository, default_database_path};
use ticket_tui::session;
use url::Url;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// SQLite database to open instead of the platform data-directory default
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    database: Option<PathBuf>,
}

#[derive(Default)]
struct ReloadEngine {
    receiver: Option<Receiver<std::result::Result<PreparedTickets, String>>>,
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
                    let opened = SqliteTicketRepository::open(&path)?;
                    let tickets = opened.repository.load_all()?;
                    Ok(PreparedTickets::new(tickets))
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
    let opened = SqliteTicketRepository::open(&database_path)?;
    let tickets = opened.repository.load_all()?;
    let mut app = App::new(tickets);
    let session_path = session::path_for(opened.repository.path());
    match session::load(&session_path) {
        Ok(loaded) => app.restore_session(loaded),
        Err(error) => app.set_error(format!("Could not load session: {error:#}")),
    }
    if opened.seeded_demo_data {
        app.set_status(format!(
            "Created demo database with 500 tickets at {}",
            opened.repository.path().display()
        ));
    }
    let result = run_terminal(&mut app, &opened.repository);
    if let Err(error) = session::save(&session_path, &app.snapshot_session()) {
        eprintln!("warning: could not save session: {error:#}");
    }
    result
}

fn run_terminal(app: &mut App, repository: &SqliteTicketRepository) -> Result<()> {
    let mut terminal = ratatui::init();
    let _restore = TerminalRestore;
    let opener = SystemUrlOpener;
    let mut reloader = ReloadEngine::default();
    execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)
        .context("failed to enable terminal input features")?;

    let mut redraw = true;
    while !app.should_quit {
        redraw |= app.poll_search();
        redraw |= poll_reload(app, &mut reloader);
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
    repository: &SqliteTicketRepository,
    opener: &dyn UrlOpener,
    reloader: &mut ReloadEngine,
) {
    match action {
        AppAction::None => {}
        AppAction::Reload => match reloader.start(repository.path()) {
            Ok(true) => {
                app.reload_pending = true;
                app.set_status("Reloading tickets…");
            }
            Ok(false) => app.set_status("Reload already in progress"),
            Err(error) => app.set_error(format!("Could not start reload: {error:#}")),
        },
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

fn poll_reload(app: &mut App, reloader: &mut ReloadEngine) -> bool {
    let Some(result) = reloader.try_result() else {
        return false;
    };
    app.reload_pending = false;
    match result {
        Ok(prepared) => {
            let count = prepared.ticket_count();
            app.replace_prepared_tickets(prepared);
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
        assert_eq!(prepared.ticket_count(), 500);
    }
}
