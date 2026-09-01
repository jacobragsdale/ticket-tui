//! Reading the background workers: one non-blocking pass over each channel,
//! applying whatever came back to the app.

use std::time::SystemTime;

use super::*;

/// How often the loop looks at `config.toml`'s clock. It wakes at least this
/// often anyway, so the look costs one `stat` a second and no wake-ups.
const CONFIG_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// `config.toml`, watched: the theme it names is painted when the run starts
/// and again whenever the file changes, so `theme pick` repaints a running
/// ticket-tui the way it repaints an editor. A theme chosen by `--theme` or
/// `TICKET_TUI_THEME` keeps winning over the file for the whole run.
pub(super) struct ConfigWatch {
    path: PathBuf,
    chosen: Option<ThemeChoice>,
    /// The file's clock as last read, `None` while there is no file.
    modified: Option<SystemTime>,
    checked_at: Option<Instant>,
}

impl ConfigWatch {
    pub(super) fn new(path: PathBuf, chosen: Option<ThemeChoice>) -> Self {
        Self {
            path,
            chosen,
            modified: None,
            checked_at: None,
        }
    }

    /// Looks at the file's clock once a second and repaints from it when it
    /// has moved. Reports whether the screen changed.
    pub(super) fn poll(&mut self, app: &mut App) -> bool {
        if self
            .checked_at
            .is_some_and(|at| at.elapsed() < CONFIG_POLL_INTERVAL)
        {
            return false;
        }
        self.checked_at = Some(Instant::now());
        if self.modified_now() == self.modified {
            return false;
        }
        self.reload(app, true)
    }

    /// Reads the file and paints the theme it settles on. A file that cannot
    /// be read or does not parse is reported and leaves the theme as it was;
    /// `announce` says the new theme's name in the footer, which a change
    /// mid-run wants and the first paint does not.
    pub(super) fn reload(&mut self, app: &mut App, announce: bool) -> bool {
        self.modified = self.modified_now();
        let config = match ticket_tui::config::load(&self.path) {
            Ok(config) => config,
            Err(error) => {
                app.shell
                    .set_error(format!("Could not read config.toml: {error:#}"));
                return true;
            }
        };
        let no_color = env::var_os("NO_COLOR").is_some();
        let painted = ThemeChoice::resolve(no_color, self.chosen, &config)
            .and_then(|choice| choice.theme(&config));
        match painted {
            Ok((theme, label)) => {
                set_theme(theme);
                if announce {
                    app.shell.set_status(format!("Theme: {label}"));
                }
            }
            Err(error) => app.shell.set_error(format!("Theme: {error:#}")),
        }
        // The clusters travel with the theme: one file, one read, and a run
        // started before `[[clusters]]` existed picks them up as soon as they
        // are written.
        app.aks.set_clusters(config.clusters.clone());
        true
    }

    fn modified_now(&self) -> Option<SystemTime> {
        fs::metadata(&self.path)
            .and_then(|metadata| metadata.modified())
            .ok()
    }
}

/// What a finished watch says: the glyph, the build number, how it went, and
/// how long it took.
fn run_finished_summary(run: &Run) -> String {
    let glyph = match run.result {
        Some(RunResult::Succeeded) => "\u{2713}",
        Some(RunResult::PartiallySucceeded) => "\u{25d1}",
        Some(RunResult::Failed) => "\u{2717}",
        _ => "\u{2298}",
    };
    let word = match run.result {
        Some(RunResult::Succeeded) => "succeeded",
        Some(RunResult::PartiallySucceeded) => "partly succeeded",
        Some(RunResult::Failed) => "failed",
        _ => "was canceled",
    };
    let duration = match (run.start_time, run.finish_time) {
        (Some(start), Some(finish)) => {
            let seconds = start.seconds_until(finish).max(0);
            format!(" \u{00b7} {}m {:02}s", seconds / 60, seconds % 60)
        }
        _ => String::new(),
    };
    format!("{glyph} Build {} {word}{duration}", run.build_number)
}

/// Reads the workspace while the Repos tab is showing, and folds in whatever
/// the local thread has found or done. Nothing here is written to SQLite: what
/// is on this machine is not the project's business, and a rescan is cheap.
pub(super) fn poll_local(app: &mut App, runtime: &mut SyncRuntime) -> bool {
    let Some(worker) = runtime.local.worker.as_ref() else {
        return false;
    };
    let showing = app.tab == TabId::Repos;
    let opened = showing && !runtime.local.showing;
    runtime.local.showing = showing;
    let due = runtime
        .local
        .scanned
        .is_none_or(|at| at.elapsed() >= LOCAL_SCAN_CADENCE);
    if showing
        && (opened || due)
        && let Some(workspace) = app.shell.workspace().map(std::path::Path::to_path_buf)
    {
        runtime.local.scanned = Some(Instant::now());
        let repos = app
            .shell
            .repos()
            .iter()
            .map(|repo| local::RepoKey {
                id: repo.id.clone(),
                remote: local::normalise_remote(&repo.remote_url),
                name: repo.name.clone(),
            })
            .collect();
        let _ = worker.send(LocalRequest::Scan { workspace, repos });
    }
    // Drained first, so the events can be answered without holding a borrow of
    // the thread that sent them.
    let events: Vec<LocalEvent> = std::iter::from_fn(|| worker.try_event()).collect();
    // A running job redraws on its own, so its glyph turns.
    let redraw = !events.is_empty() || app.repos.busy();
    for event in events {
        match event {
            LocalEvent::Scanned(local) => app.repos.set_local(local),
            LocalEvent::Started { repo_id, job } => app.repos.set_job(&repo_id, Some(job)),
            LocalEvent::Finished {
                repo_id,
                job,
                message,
                error,
            } => {
                app.repos.set_job(&repo_id, None);
                if error {
                    // A pull git will not fast-forward wants a real git
                    // client, or the repository's own page.
                    app.shell.set_error(match job {
                        GitJob::Pulling => format!("{message} \u{2014} o opens the repository"),
                        _ => message,
                    });
                } else {
                    app.shell.set_news(message);
                }
                // Whatever git did, the workspace is not what it was.
                runtime.local.scanned = None;
            }
            // Nothing in the TUI asks for a render yet; the environments board
            // is what reads these.
            LocalEvent::Rendered { .. } => {}
            LocalEvent::Stopped => runtime.local.worker = None,
        }
    }
    redraw
}

/// Tells the cluster worker what is worth reading and folds in what it has
/// read. Pods live in the screen only: nothing here touches SQLite.
pub(super) fn poll_aks(app: &mut App, runtime: &mut SyncRuntime) -> bool {
    // The thread is started by the first cluster the file names, and never
    // started again once it has stopped.
    if runtime.aks.worker.is_none()
        && runtime.aks.clusters.is_empty()
        && !app.aks.clusters().is_empty()
    {
        match AksHandle::spawn(Box::new(Kubectl)) {
            Ok(handle) => runtime.aks.worker = Some(handle),
            Err(error) => {
                // Said once: without a thread there is nothing to try again
                // with, and the clusters recorded here are what stops it.
                runtime.aks.clusters = app.aks.clusters().to_vec();
                app.shell
                    .set_error(format!("Could not start the cluster worker: {error:#}"));
            }
        }
    }
    let Some(worker) = runtime.aks.worker.as_ref() else {
        return false;
    };
    if app.aks.clusters() != runtime.aks.clusters {
        runtime.aks.clusters = app.aks.clusters().to_vec();
        let _ = worker.send(AksRequest::Clusters(runtime.aks.clusters.clone()));
    }
    let showing = app.tab == TabId::Aks;
    if showing != runtime.aks.showing {
        runtime.aks.showing = showing;
        let _ = worker.send(AksRequest::TabShowing(showing));
    }
    // Which pod the text pane is on decides whose log is worth streaming. It
    // is settled here rather than only on the way to drawing, so the screen and
    // the worker are on the same stream whatever was last painted, and it is
    // not gated on the tab showing: re-tailing on the way back would replay the
    // last five hundred lines.
    app.aks.sync_focus(&app.shell);
    let target = app.aks.following().cloned();
    if target != runtime.aks.following {
        runtime.aks.following = target.clone();
        let _ = worker.send(target.map_or(AksRequest::Unfollow, AksRequest::Follow));
    }
    // Drained first, so an event can be answered without holding a borrow of
    // the thread that sent it.
    let events: Vec<AksEvent> = std::iter::from_fn(|| worker.try_event()).collect();
    let redraw = !events.is_empty();
    for event in events {
        match event {
            AksEvent::Pods {
                cluster,
                namespace,
                pods,
            } => {
                if let Some(toast) =
                    app.aks
                        .set_pods(&app.shell, &cluster, namespace.as_deref(), pods)
                {
                    app.shell.set_error(toast);
                }
            }
            AksEvent::LogLines {
                target,
                lines,
                finished,
            } => app.aks.append_log(&target, lines, finished),
            AksEvent::Described { key, text } => {
                if let Err(message) = &text {
                    app.shell.set_error(message.clone());
                }
                app.aks.set_description(&key, text);
            }
            AksEvent::Stopped => {
                runtime.aks.worker = None;
                app.aks.request_refused();
            }
            AksEvent::Deleted { key, error } => {
                app.aks.delete_answered(&mut app.shell, &key, error);
            }
        }
    }
    redraw
}

/// Tells the subscription worker what is worth reading and folds in what it
/// has read. Registries, repositories and tags live in the screen only:
/// nothing here touches SQLite.
pub(super) fn poll_arm(app: &mut App, runtime: &mut SyncRuntime) -> bool {
    let showing = match app.tab {
        TabId::Acr | TabId::KeyVault => Some(app.tab),
        _ => None,
    };
    // Started by the first of the two tabs to be opened, and never started
    // again once it has refused or stopped.
    if runtime.arm.worker.is_none() && showing.is_some() && !runtime.arm.failed_to_start {
        runtime.arm.failed_to_start = true;
        match ArmHandle::spawn(runtime.arm_config.clone()) {
            Ok(handle) => {
                runtime.arm.failed_to_start = false;
                runtime.arm.worker = Some(handle);
            }
            Err(error) => {
                let refusal = format!("Could not start the subscription worker: {error:#}");
                app.acr.set_arm_error(refusal.clone());
                app.key_vault.set_arm_error(refusal.clone());
                app.shell.set_error(refusal);
            }
        }
    }
    let Some(worker) = runtime.arm.worker.as_ref() else {
        return false;
    };
    if showing != runtime.arm.showing {
        runtime.arm.showing = showing;
        let _ = worker.send(ArmRequest::TabShowing(showing));
    }
    // What the tab on screen is looking at, and only while it is the one
    // showing: a hidden tab is nothing to read for.
    let focus = match showing {
        Some(TabId::Acr) => app.acr.focus(),
        Some(TabId::KeyVault) => app.key_vault.focus(),
        _ => None,
    };
    if focus != runtime.arm.focus {
        runtime.arm.focus = focus.clone();
        let _ = worker.send(focus.map_or(ArmRequest::Blur, ArmRequest::Focus));
    }
    // Drained first, so an event can be answered without holding a borrow of
    // the thread that sent it.
    let events: Vec<ArmEvent> = std::iter::from_fn(|| worker.try_event()).collect();
    // A read in flight redraws on its own, so its spinner turns.
    let tab_busy = match showing {
        Some(TabId::Acr) => app.acr.busy(),
        Some(TabId::KeyVault) => app.key_vault.busy(),
        _ => false,
    };
    let redraw = !events.is_empty() || tab_busy;
    for event in events {
        let toast = match event {
            ArmEvent::Subscription(Ok(subscription)) => {
                app.shell.set_arm_subscription(Some(subscription));
                app.shell.set_arm_state(None);
                None
            }
            ArmEvent::Subscription(Err(reason)) => {
                app.shell.set_arm_state(Some(reason.clone()));
                Some(reason)
            }
            // One query answers for both tabs: each keeps the half it draws,
            // and a refusal is said once rather than twice.
            ArmEvent::Inventory(inventory) => {
                let said = app.key_vault.set_inventory(inventory.clone());
                app.acr.set_inventory(inventory).or(said)
            }
            ArmEvent::Repositories {
                registry,
                repositories,
            } => app.acr.set_repositories(&registry, repositories),
            ArmEvent::Repository {
                registry,
                repository,
            } => app.acr.set_repository(&registry, repository),
            ArmEvent::Tags {
                registry,
                repo,
                tags,
            } => app.acr.set_tags(&registry, &repo, tags),
            ArmEvent::Manifest {
                registry,
                repo,
                digest,
                manifest,
            } => app.acr.set_manifest(&registry, &repo, &digest, manifest),
            ArmEvent::Items { vault, items } => app.key_vault.set_items(&vault, items),
            // The one event carrying something worth hiding. It goes to the
            // screen and no further: nothing here logs it, stores it, or puts
            // it in a notification.
            ArmEvent::Revealed { vault, name, value } => {
                app.key_vault.set_revealed(&vault, &name, value)
            }
            // Throttling is Azure working as designed and passes on its own,
            // so it is said once rather than calling the tab offline.
            ArmEvent::Throttled(wait) => {
                app.shell.set_status(format!(
                    "Holding off {}s \u{2014} Azure asked",
                    wait.as_secs()
                ));
                None
            }
            ArmEvent::Failed(reason) => {
                let said = app.key_vault.set_arm_error(reason.clone());
                app.acr.set_arm_error(reason).or(said)
            }
            ArmEvent::Stopped => {
                runtime.arm.worker = None;
                runtime.arm.failed_to_start = true;
                // Said on both tabs, so a read that will never come back is
                // not waited on and `r` is refused for the right reason.
                let gone = "The Azure subscription worker stopped".to_owned();
                app.acr.set_arm_error(gone.clone());
                app.key_vault.set_arm_error(gone.clone());
                app.shell.set_arm_state(Some(gone));
                break;
            }
        };
        if let Some(toast) = toast {
            app.shell.set_error(toast);
        }
    }
    redraw
}

/// Tells the pipeline watcher what is worth polling and folds in what it has
/// seen. None of it is written to SQLite: the next pull is what persists a run,
/// and until then the screen shows what the watcher has and the file has not.
pub(super) fn poll_pipelines(app: &mut App, runtime: &mut SyncRuntime) -> bool {
    let Some(watcher) = runtime.pipelines.as_ref() else {
        return false;
    };
    let showing = app.tab == TabId::Pipelines;
    // Which run the details pane is on decides whose timeline is worth
    // reading, so the watcher is told whenever the cursor settles somewhere
    // else.
    let focus = showing.then(|| app.pipelines.focused_run()).flatten();
    let node = focus.and_then(|_| app.pipelines.log_target());
    if (focus, node) != runtime.watching_run {
        runtime.watching_run = (focus, node);
        let _ = watcher.send(
            focus.map_or(WatchRequest::Blur, |run_id| WatchRequest::Focus {
                run_id,
                node,
            }),
        );
    }
    let watched = app.pipelines.watched_runs();
    if watched != runtime.watched_runs {
        for run in &watched {
            if !runtime.watched_runs.contains(run) {
                let _ = watcher.send(WatchRequest::Watch(*run));
            }
        }
        for run in &runtime.watched_runs {
            if !watched.contains(run) {
                let _ = watcher.send(WatchRequest::Unwatch(*run));
            }
        }
        runtime.watched_runs = watched;
    }
    if showing != runtime.watching_tab {
        runtime.watching_tab = showing;
        let _ = watcher.send(WatchRequest::TabShowing(showing));
        app.shell.set_watch_state(Some(if showing {
            format!("polling live runs every {}s", LIVE_RUNS_CADENCE.as_secs())
        } else {
            format!(
                "idle · every {}s while showing",
                LIVE_RUNS_CADENCE.as_secs()
            )
        }));
    }
    let mut redraw = false;
    while let Some(event) = watcher.try_event() {
        redraw = true;
        match event {
            WatchEvent::LiveRuns(runs) => app.pipelines.merge_live_runs(runs, &app.shell),
            WatchEvent::Approvals(approvals) => app.pipelines.set_approvals(approvals),
            WatchEvent::RunWorkItems { run_id, work_items } => {
                app.pipelines.set_run_work_items(run_id, work_items);
            }
            // A watched run has stopped: say so wherever the user is, which is
            // the whole point of having watched it.
            WatchEvent::RunFinished(run) => {
                let summary = run_finished_summary(&run);
                if matches!(run.result, Some(RunResult::Failed)) {
                    app.shell.set_error(summary);
                } else {
                    // Eight seconds, like an error: it is news whether it went
                    // well or badly, and you may have looked away.
                    app.shell.set_news(summary);
                }
                app.pipelines.unwatch_run(run.id);
                app.pipelines.merge_live_runs(vec![run], &app.shell);
            }
            WatchEvent::Timeline { run_id, records } => {
                app.pipelines.set_timeline(run_id, records);
            }
            WatchEvent::LogLines {
                run_id,
                log_id,
                from_line,
                lines,
                finished,
            } => app
                .pipelines
                .append_log(run_id, log_id, from_line, lines, finished),
            WatchEvent::Throttled(wait) => app.shell.set_watch_state(Some(format!(
                "holding off {}s — Azure DevOps asked",
                wait.as_secs()
            ))),
            WatchEvent::Failed(error) => {
                app.shell.set_watch_state(Some(format!("failing: {error}")));
            }
            WatchEvent::Stopped => {
                runtime.pipelines = None;
                app.shell.set_watch_state(Some("stopped".to_owned()));
                break;
            }
        }
    }
    redraw
}

/// Applies whatever the sync worker has finished. A pull it completed wrote the
/// database itself, so its signature is recorded here and the watcher below
/// leaves it alone instead of reloading behind us.
pub(super) fn poll_sync(
    app: &mut App,
    repository: &mut SqliteTicketRepository,
    runtime: &mut SyncRuntime,
) -> bool {
    let mut redraw = false;
    while let Some(event) = runtime.worker.as_ref().and_then(SyncHandle::try_event) {
        redraw = true;
        match event {
            SyncEvent::PullRequestUpdated(result) => match result {
                Ok(updated) => app
                    .pull_requests
                    .apply_pull_request(&mut app.shell, *updated),
                Err(refusal) => app.shell.set_error(refusal),
            },
            SyncEvent::PullRequestCommented(result) => match result {
                Ok((id, thread)) => app.pull_requests.apply_comment(&mut app.shell, id, thread),
                Err(refusal) => app.shell.set_error(refusal),
            },
            SyncEvent::Voted(result) => match result {
                Ok((id, _)) => app.pull_requests.vote_accepted(id),
                Err((id, refusal)) => {
                    app.pull_requests
                        .vote_rejected(&mut app.shell, id, &refusal);
                }
            },
            SyncEvent::ApprovalAnswered(result) => match result {
                Ok(id) => {
                    app.pipelines.approval_answered(&id);
                    app.shell.set_status("Approval sent");
                }
                Err(refusal) => app.shell.set_error(refusal),
            },
            SyncEvent::Branches { repo_id, branches } => {
                app.pipelines.set_branches(&repo_id, branches);
            }
            // A run this session started, cancelled or retried. It is not
            // optimistic: what comes back is the run Azure DevOps made.
            SyncEvent::RunStarted(result) => match result {
                Ok(run) => app.pipelines.accept_run(&mut app.shell, run),
                Err(refusal) => app.shell.set_error(refusal),
            },
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
                        snapshot,
                        mode,
                        count,
                    } => {
                        let extras = PulledExtras::of(&snapshot);
                        app.apply_snapshot(*snapshot);
                        app.shell.finish_sync();
                        stamp_database(app, repository);
                        if origin == PullOrigin::User {
                            app.shell
                                .set_status(runtime.status_for(mode, count, extras));
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
            SyncEvent::Warning(text) => app.shell.set_error(text),
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
pub(super) fn poll_watch(
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

pub(super) fn persist_session(app: &mut App, repository: &SqliteTicketRepository) -> bool {
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

pub(super) fn poll_reload(
    app: &mut App,
    repository: &SqliteTicketRepository,
    reloader: &mut ReloadEngine,
) -> bool {
    let Some(result) = reloader.try_result() else {
        return false;
    };
    app.shell.reload_pending = false;
    match result {
        Ok(snapshot) => {
            let count = snapshot.ticket_count();
            app.apply_snapshot(snapshot);
            stamp_database(app, repository);
            app.shell.set_status(format!("Reloaded {count} tickets"));
        }
        Err(error) => app.shell.set_error(format!("Reload failed: {error}")),
    }
    true
}
