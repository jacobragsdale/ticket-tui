use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueHint};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind,
};
use crossterm::execute;
use ticket_tui::app::{App, AppAction};
use ticket_tui::db::{SqliteTicketRepository, default_database_path};
use url::Url;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// SQLite database to open instead of the platform data-directory default
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    database: Option<PathBuf>,
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
    if opened.seeded_demo_data {
        app.set_status(format!(
            "Created demo database with 500 tickets at {}",
            opened.repository.path().display()
        ));
    }
    run_terminal(&mut app, &opened.repository)
}

fn run_terminal(app: &mut App, repository: &SqliteTicketRepository) -> Result<()> {
    let mut terminal = ratatui::init();
    let _restore = TerminalRestore;
    let opener = SystemUrlOpener;
    execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)
        .context("failed to enable terminal input features")?;

    let mut redraw = true;
    while !app.should_quit {
        redraw |= app.poll_search();
        redraw |= app.tick();
        if redraw {
            terminal.draw(|frame| ticket_tui::ui::render(frame, app))?;
            redraw = false;
        }

        let timeout = if app.search_pending {
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
        handle_action(action, app, repository, &opener);
    }
    Ok(())
}

fn handle_action(
    action: AppAction,
    app: &mut App,
    repository: &SqliteTicketRepository,
    opener: &dyn UrlOpener,
) {
    match action {
        AppAction::None => {}
        AppAction::Reload => match repository.load_all() {
            Ok(tickets) => {
                let count = tickets.len();
                app.replace_tickets(tickets);
                app.set_status(format!("Reloaded {count} tickets"));
            }
            Err(error) => app.set_error(format!("Reload failed: {error:#}")),
        },
        AppAction::OpenUrl(raw_url) => match open_https_url(&raw_url, opener) {
            Ok(()) => app.set_status(format!("Opened {raw_url}")),
            Err(error) => app.set_error(format!("Could not open ticket: {error:#}")),
        },
    }
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
    use super::*;

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
}
