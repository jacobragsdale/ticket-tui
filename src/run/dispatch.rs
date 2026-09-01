//! Sending work to the background: every request that leaves the run for the
//! sync worker, and the timers that decide when one is due.

use super::*;

pub(super) fn start_reload(
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
pub(super) fn dispatch_due_pull(app: &mut App, runtime: &mut SyncRuntime) -> bool {
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
pub(super) fn dispatch_due_details(app: &mut App, runtime: &mut SyncRuntime) -> bool {
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
pub(super) fn start_sync(app: &mut App, runtime: &mut SyncRuntime) {
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
pub(super) fn start_edit(app: &mut App, runtime: &mut SyncRuntime, request: EditRequest) {
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
pub(super) fn start_comment(
    app: &mut App,
    runtime: &mut SyncRuntime,
    key: TicketKey,
    text: String,
) {
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
pub(super) fn start_delete(app: &mut App, runtime: &mut SyncRuntime, key: TicketKey) {
    if let Err(message) = runtime.send(SyncRequest::Delete(key.clone())) {
        app.work_items.reject_delete(&mut app.shell, &key, &message);
    }
}

/// Hands one new work item to the sync worker. Nothing appears in the table
/// until Azure DevOps has stored it, so a worker that is gone only has to say
/// the work item was not created — and the form comes back with everything
/// still in it.
pub(super) fn start_create(
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
        app.reject_create(&message);
    }
}

/// Hands one move to the sync worker. The graph already shows it, so a worker
/// that is gone puts both halves of the old link back here rather than leaving
/// a family tree on screen that nothing in Azure DevOps agrees with.
pub(super) fn start_reparent(
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
