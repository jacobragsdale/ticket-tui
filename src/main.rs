use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueHint};
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
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
    execute!(io::stdout(), EnableMouseCapture).context("failed to enable mouse capture")?;

    while !app.should_quit {
        app.poll_search();
        terminal.draw(|frame| ticket_tui::ui::render(frame, app))?;

        if !event::poll(Duration::from_millis(33))? {
            continue;
        }
        let action = match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
            Event::Mouse(mouse) => app.handle_mouse(mouse),
            Event::Resize(_, _)
            | Event::FocusGained
            | Event::FocusLost
            | Event::Paste(_)
            | Event::Key(_) => AppAction::None,
        };
        handle_action(action, app, repository);
    }
    Ok(())
}

fn handle_action(action: AppAction, app: &mut App, repository: &SqliteTicketRepository) {
    match action {
        AppAction::None => {}
        AppAction::Reload => match repository.load_all() {
            Ok(tickets) => {
                let count = tickets.len();
                app.replace_tickets(tickets);
                app.set_status(format!("Reloaded {count} tickets"));
            }
            Err(error) => app.set_status(format!("Reload failed: {error:#}")),
        },
        AppAction::OpenUrl(raw_url) => match open_https_url(&raw_url) {
            Ok(()) => app.set_status(format!("Opened {raw_url}")),
            Err(error) => app.set_status(format!("Could not open ticket: {error:#}")),
        },
    }
}

fn open_https_url(raw_url: &str) -> Result<()> {
    let url = Url::parse(raw_url).context("ticket URL is invalid")?;
    if url.scheme() != "https" {
        bail!("only HTTPS ticket URLs can be opened");
    }
    open::that(url.as_str()).context("system URL launcher failed")
}

struct TerminalRestore;

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_https_ticket_urls() {
        let error = open_https_url("file:///tmp/not-a-ticket").unwrap_err();
        assert!(error.to_string().contains("only HTTPS"));
    }

    #[test]
    fn rejects_malformed_ticket_urls() {
        let error = open_https_url("not a url").unwrap_err();
        assert!(error.to_string().contains("invalid"));
    }
}
