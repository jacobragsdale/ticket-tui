//! Everything the interactive run does, from claiming the terminal to
//! putting it back. `main` opens the database and hands over to `run`.

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
use ticket_tui::aks::{AksEvent, AksHandle, AksRequest, Cluster, Kubectl, LogFollow, PodKey};
use ticket_tui::app::{
    App, AppAction, CopiedContent, DividerOrientation, PointerTarget, Snapshot, SyncTarget, TabId,
};
use ticket_tui::arm::ArmConfig;
use ticket_tui::arm_watch::{ArmEvent, ArmFocus, ArmHandle, ArmRequest};
use ticket_tui::azure::AzureConfig;
use ticket_tui::cli::{self, Cli, resolve_me};
use ticket_tui::db::{self, SqliteTicketRepository, default_database_path};
use ticket_tui::edit::{EditRejection, EditRequest, FieldEdit};
use ticket_tui::local::{self, LocalEvent, LocalHandle, LocalRequest};
use ticket_tui::markdown;
use ticket_tui::model::{GitJob, Run, RunResult, Ticket, TicketKey};
use ticket_tui::session;
use ticket_tui::sync::{
    self, AzureConnector, DetailsOutcome, PullOrigin, PulledExtras, ReparentRejection, SyncEvent,
    SyncHandle, SyncMode, SyncOutcome, SyncRequest, SyncScheduler,
};
use ticket_tui::timestamp::Timestamp;
use ticket_tui::ui::{ThemeChoice, chosen_theme, set_theme};
use ticket_tui::watch::{LIVE_RUNS_CADENCE, LogTarget, WatchEvent, WatchHandle, WatchRequest};
use url::Url;

mod desktop;
mod dispatch;
mod editor;
mod engines;
mod events;
mod pointer;
mod polling;

use desktop::*;
use dispatch::*;
use editor::*;
use engines::*;
use events::*;
use pointer::*;
use polling::*;

#[cfg(test)]
mod tests;

pub(super) fn run() -> Result<()> {
    let cli = Cli::parse();
    // Every subcommand does its one thing and exits; only a bare invocation
    // opens the TUI.
    if let Some(command) = &cli.command {
        return cli::run(&cli, command);
    }
    let refresh = resolve_refresh(cli.refresh, std::env::var("TICKET_TUI_REFRESH").ok())?;
    let stale_days =
        resolve_stale_days(cli.stale_days, std::env::var("TICKET_TUI_STALE_DAYS").ok())?;
    let theme_choice = chosen_theme(
        cli.theme.as_deref(),
        std::env::var("TICKET_TUI_THEME").ok().as_deref(),
    )?;
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
    app.shell.set_repos(repository.load_repos()?);
    app.shell
        .set_workspace(local::workspace_root(cli.workspace.clone()));
    let (pipelines, runs) = (repository.load_pipelines()?, repository.load_runs()?);
    let shell = &app.shell;
    app.pipelines.set_pipelines(pipelines, runs, shell);
    let pull_requests = repository.load_pull_requests()?;
    let shell = &app.shell;
    app.pull_requests
        .set_pull_requests(pull_requests.clone(), shell);
    app.relate_repos(&pull_requests);
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
        pipelines: None,
        watching_tab: false,
        watching_run: (None, None),
        watched_runs: Vec::new(),
        local: LocalRuntime {
            worker: LocalHandle::spawn().ok(),
            ..LocalRuntime::default()
        },
        aks: AksRuntime::default(),
        arm: ArmRuntime::default(),
        // Which subscription the ACR and Key Vault tabs read: the flag, then
        // the variable, and nothing else. Asking the Azure CLI costs a
        // shell-out, so the worker thread does that instead — and only once
        // one of those tabs is opened.
        arm_config: ArmConfig::from_settings(
            cli.subscription.clone(),
            std::env::var("TICKET_TUI_SUBSCRIPTION").ok(),
        ),
    };
    app.shell.set_arm_subscription(
        runtime
            .arm_config
            .as_ref()
            .map(|config| config.subscription.clone()),
    );
    if let Some(config) = config.filter(|_| wrong_project.is_none()) {
        runtime.worker = Some(SyncHandle::spawn(
            database_path.clone(),
            Box::new(AzureConnector::new(config.clone())),
        )?);
        runtime.pipelines = WatchHandle::spawn(config).ok();
        app.shell
            .set_watch_state(runtime.pipelines.as_ref().map(|_| {
                format!(
                    "idle · every {}s while showing",
                    LIVE_RUNS_CADENCE.as_secs()
                )
            }));
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

    // The theme is painted from the file last, so a file that cannot be read
    // is the newest thing in the footer when the screen first appears.
    let mut config_watch = ConfigWatch::new(ticket_tui::config::default_path(), theme_choice);
    config_watch.reload(&mut app, false);

    let mut context_publisher = AgentContextPublisher::new(repository.path());
    let result = run_terminal(
        &mut app,
        &mut repository,
        &mut runtime,
        &mut context_publisher,
        &mut config_watch,
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
