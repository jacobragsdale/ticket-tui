//! The event loop and the actions it hands back: one pass over the terminal's
//! input, then whatever the app asked the outside world to do.

use super::*;

pub(super) fn run_terminal(
    app: &mut App,
    repository: &mut SqliteTicketRepository,
    runtime: &mut SyncRuntime,
    context_publisher: &mut AgentContextPublisher,
    config_watch: &mut ConfigWatch,
) -> Result<()> {
    let mut terminal = ratatui::init();
    let _restore = TerminalRestore;
    let mut reloader = ReloadEngine::default();
    let mut mouse_pointer = MousePointerShape::Default;
    enable_terminal_input()?;

    // ponytail: the one instrument this build has. TICKET_TUI_TRACE=<file>
    // appends a line for every drawn frame and every loop turn slower than
    // 30 ms — the polls, the draw, the context publish and the input
    // handling, never the wait for input. Unset, no clock is read. Delete
    // once the 35k round is done.
    let trace = std::env::var_os("TICKET_TUI_TRACE").map(std::path::PathBuf::from);
    let mut redraw = true;
    while !app.shell.should_quit {
        let turn = Instant::now();
        redraw |= app.work_items.poll_search(&mut app.shell);
        redraw |= poll_reload(app, repository, &mut reloader);
        redraw |= poll_sync(app, repository, runtime);
        redraw |= poll_watch(app, repository, &mut reloader);
        redraw |= poll_pipelines(app, runtime);
        redraw |= poll_local(app, runtime);
        redraw |= dispatch_due_pull(app, runtime);
        redraw |= dispatch_due_details(app, runtime);
        redraw |= persist_session(app, repository);
        redraw |= config_watch.poll(app);
        redraw |= app.shell.tick();
        // A spinner has to be repainted to turn. Nothing in flight and
        // nothing is repainted: an idle app draws no frames at all.
        redraw |= spinning(app);
        let polled = turn.elapsed();
        let (mut drew, mut published) = (Duration::ZERO, Duration::ZERO);
        if redraw {
            let at = Instant::now();
            terminal.draw(|frame| ticket_tui::ui::render(frame, app))?;
            sync_mouse_pointer(app, &mut mouse_pointer);
            redraw = false;
            drew = at.elapsed();
            let at = Instant::now();
            if let Err(error) = context_publisher.publish(app) {
                app.shell
                    .set_error(format!("Could not publish agent context: {error:#}"));
                redraw = true;
            }
            published = at.elapsed();
        }

        let timeout = if app.work_items.search_pending || app.shell.reload_pending {
            Duration::from_millis(33)
        } else if spinning(app) {
            // Ten times a second, which is how often a spinner turns, and
            // only while there is one on screen to turn.
            Duration::from_millis(100)
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
            trace_turn(trace.as_deref(), polled, drew, published, Duration::ZERO);
            continue;
        }
        let handling = Instant::now();
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
        trace_turn(
            trace.as_deref(),
            polled,
            drew,
            published,
            handling.elapsed(),
        );
    }
    Ok(())
}

/// ponytail: `TICKET_TUI_TRACE`'s one line per drawn frame or slow loop turn,
/// appended to the file it names. A turn that drew nothing and took under
/// 30 ms writes nothing.
fn trace_turn(
    path: Option<&std::path::Path>,
    poll: Duration,
    draw: Duration,
    publish: Duration,
    input: Duration,
) {
    use std::io::Write as _;
    let Some(path) = path else { return };
    let total = poll + draw + publish + input;
    if draw.is_zero() && total < Duration::from_millis(30) {
        return;
    }
    if let Ok(mut file) = fs::OpenOptions::new().append(true).create(true).open(path) {
        let _ = writeln!(
            file,
            "turn {}ms poll {} draw {} publish {} input {}",
            total.as_millis(),
            poll.as_millis(),
            draw.as_millis(),
            publish.as_millis(),
            input.as_millis()
        );
    }
}

/// Carries out one action, and says whether the screen has to be painted from
/// scratch afterwards. Only the editor hand-off, which gives the terminal back
/// for as long as the editor runs, ever asks for that.
pub(super) fn handle_action(
    action: AppAction,
    app: &mut App,
    runtime: &mut SyncRuntime,
    opener: &dyn Fn(&Url) -> Result<()>,
) -> bool {
    match action {
        AppAction::None => {}
        // The shell answers these itself, before the event loop sees them.
        AppAction::Follow(_)
        | AppAction::HistoryBack
        | AppAction::HistoryForward
        | AppAction::RunCommand(_) => {}
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
        // The picker is already open on what is cached, so a worker that is
        // gone leaves it showing that and says nothing.
        AppAction::FetchBranches(repo_id) => drop(runtime.send(SyncRequest::Branches(repo_id))),
        AppAction::TriggerRun {
            pipeline_id,
            branch,
        } => {
            if let Err(refusal) = runtime.send(SyncRequest::TriggerRun {
                pipeline_id,
                branch,
            }) {
                app.shell.set_error(refusal);
            }
        }
        AppAction::PullRequestAction {
            repo_id,
            id,
            action,
        } => {
            if let Err(refusal) = runtime.send(SyncRequest::PullRequestAction {
                repo_id,
                id,
                action,
            }) {
                app.shell.set_error(refusal);
            }
        }
        AppAction::CommentOnPullRequest { repo_id, id, text } => {
            if let Err(refusal) =
                runtime.send(SyncRequest::CommentOnPullRequest { repo_id, id, text })
            {
                app.shell.set_error(refusal);
            }
        }
        AppAction::LinkWorkItem {
            repo_id,
            id,
            work_item,
        } => {
            if let Err(refusal) = runtime.send(SyncRequest::LinkWorkItem {
                repo_id,
                id,
                work_item,
            }) {
                app.shell.set_error(refusal);
            }
        }
        AppAction::VotePullRequest { repo_id, id, vote } => {
            if let Err(refusal) = runtime.send(SyncRequest::VotePullRequest { repo_id, id, vote }) {
                app.pull_requests
                    .vote_rejected(&mut app.shell, id, &refusal);
            }
        }
        // git runs on the local thread; the screen hears back through its
        // events, so nothing waits here.
        AppAction::LocalGit(request) => match runtime.local.worker.as_ref() {
            Some(worker) => {
                let _ = worker.send(request);
            }
            None => app
                .shell
                .set_error("The local repositories thread is not running".to_owned()),
        },
        AppAction::RefreshApprovals => {
            if let Some(watcher) = runtime.pipelines.as_ref() {
                let _ = watcher.send(WatchRequest::RefreshApprovals);
            }
        }
        AppAction::AnswerApproval {
            id,
            approve,
            comment,
        } => {
            if let Err(refusal) = runtime.send(SyncRequest::AnswerApproval {
                id,
                approve,
                comment,
            }) {
                app.shell.set_error(refusal);
            }
        }
        AppAction::RunAction { run_id, retry } => {
            if let Err(refusal) = runtime.send(SyncRequest::RunAction { run_id, retry }) {
                app.shell.set_error(refusal);
            }
        }
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

/// Whether anything on screen has a spinner turning on it: a pull in flight,
/// a git job, the details fetch, or a row waiting on an edit. Nothing running
/// and the loop goes back to waking a second at a time.
fn spinning(app: &App) -> bool {
    app.shell.sync_pending
        || app.repos.busy()
        || app.work_items.details_pending.is_some()
        || app.shell.flashing()
}
