//! Editing a description in `$VISUAL`/`$EDITOR`/`vi`: the round trip out to
//! the editor and back, and what the file that comes back means.

use super::*;

/// The Actions menu's Description row: the description goes out to the user's
/// editor as Markdown, and whatever comes back comes back as HTML.
///
/// The TUI steps out of the way entirely while the editor runs — the alternate
/// screen, mouse capture, and bracketed paste all go back the way they were
/// found — and takes the terminal back afterwards whether the editor saved
/// something, changed nothing, or never started at all.
pub(super) fn edit_description(
    app: &mut App,
    runtime: &mut SyncRuntime,
    key: &TicketKey,
    html: &str,
) {
    let command = editor_command(env::var("VISUAL").ok(), env::var("EDITOR").ok());
    let outcome = released_terminal(|| {
        let directory = tempfile::Builder::new()
            .prefix("ticket-tui-")
            .tempdir()
            .context("could not make a directory to edit in")?;
        run_description_editor(directory.path(), key.id, html, &command)
    });
    apply_description_outcome(app, runtime, key, outcome);
}

/// Files whatever the editor left: a rewritten description goes down the same
/// path as every other field edit, a file that came back untouched is not a
/// change at all, and an editor that failed says so and writes nothing.
pub(super) fn apply_description_outcome(
    app: &mut App,
    runtime: &mut SyncRuntime,
    key: &TicketKey,
    outcome: Result<Option<String>>,
) {
    match outcome {
        Ok(Some(html)) => {
            if let AppAction::Edit(requests) =
                app.work_items
                    .edit_ticket(&mut app.shell, key, FieldEdit::description(&html))
            {
                for request in requests {
                    start_edit(app, runtime, request);
                }
            }
        }
        Ok(None) => app
            .shell
            .set_status(format!("#{} description unchanged", key.id)),
        Err(error) => app
            .shell
            .set_error(format!("#{} description not saved: {error:#}", key.id)),
    }
}

/// Writes the description out as Markdown, runs the editor on it, and reads
/// back what was saved.
///
/// `Ok(None)` means the file came back as it was written, notice line and all,
/// so there is nothing to save. Anything else is the HTML the Markdown builds,
/// which for an emptied file is the empty document that clears the field.
pub(super) fn run_description_editor(
    directory: &Path,
    id: i64,
    html: &str,
    command: &[String],
) -> Result<Option<String>> {
    let path = directory.join(format!("ticket-{id}.md"));
    let document = markdown::description_document(html);
    fs::write(&path, format!("{document}\n"))
        .with_context(|| format!("could not write {}", path.display()))?;
    run_editor(command, &path)?;
    let edited = fs::read_to_string(&path)
        .with_context(|| format!("could not read {} back", path.display()))?;
    let saved = markdown::saved_markdown(&edited);
    if saved == markdown::saved_markdown(&document) {
        return Ok(None);
    }
    Ok(Some(markdown::markdown_to_html(&saved)))
}

/// Runs the editor on one file and waits for it. The editor owns the terminal
/// while it runs, so its own output goes straight to the screen.
fn run_editor(command: &[String], path: &Path) -> Result<()> {
    let (program, arguments) = command
        .split_first()
        .context("no editor to run; set $EDITOR")?;
    let status = Command::new(program)
        .args(arguments)
        .arg(path)
        .status()
        .with_context(|| format!("could not run {program}"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

/// The editor to hand a description to: `$VISUAL`, then `$EDITOR`, then `vi`,
/// which every system has. The variable is split on whitespace so a command
/// with arguments works — `code --wait` runs `code` with `--wait` and the file
/// after it — and one that is empty or only whitespace counts as unset.
pub(super) fn editor_command(visual: Option<String>, editor: Option<String>) -> Vec<String> {
    [visual, editor]
        .into_iter()
        .flatten()
        .map(|raw| {
            raw.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<String>>()
        })
        .find(|parts| !parts.is_empty())
        .unwrap_or_else(|| vec!["vi".to_owned()])
}
