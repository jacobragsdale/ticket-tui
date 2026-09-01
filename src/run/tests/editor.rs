//! The description round trip through `$VISUAL`/`$EDITOR`/`vi`.

use super::*;

/// A run without a worker: enough for the answers an editor hand-off gives
/// before anything is sent anywhere.
fn offline_runtime() -> SyncRuntime {
    SyncRuntime {
        worker: None,
        scheduler: SyncScheduler::new(None),
        config: None,
        offline_reason: Some("no Azure DevOps organization".into()),
        details: DetailsEngine::default(),
        pipelines: None,
        watching_tab: false,
        watching_run: (None, None),
        watched_runs: Vec::new(),
        approvals_seen: None,
        local: LocalRuntime::default(),
        aks: AksRuntime::default(),
        arm: ArmRuntime::default(),
        arm_config: ArmConfig::default(),
    }
}

/// A shell command standing in for an editor, with the file it is told to
/// edit as `$0`. Nothing interactive is ever run in a test.
fn fake_editor(script: &str) -> Vec<String> {
    vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()]
}

#[test]
fn the_editor_is_visual_then_editor_then_vi_and_keeps_its_arguments() {
    assert_eq!(
        editor_command(Some("code --wait".into()), Some("vim".into())),
        ["code", "--wait"],
        "$VISUAL wins, and its arguments come with it"
    );
    assert_eq!(
        editor_command(None, Some("  emacs  -nw ".into())),
        ["emacs", "-nw"]
    );
    assert_eq!(editor_command(None, None), ["vi"]);
    assert_eq!(
        editor_command(Some("   ".into()), Some(String::new())),
        ["vi"],
        "a variable set to nothing is not set"
    );
    assert_eq!(
        editor_command(Some(String::new()), Some("nano".into())),
        ["nano"],
        "an empty $VISUAL falls through to $EDITOR"
    );
}

#[test]
fn a_description_saved_in_the_editor_comes_back_as_html() {
    let directory = tempdir().unwrap();
    let saved = run_description_editor(
        directory.path(),
        613,
        "<p>Old words.</p>",
        &fake_editor("printf '# New\\n\\n- one\\n- two\\n' > \"$0\""),
    )
    .unwrap();

    assert_eq!(
        saved.as_deref(),
        Some("<h1>New</h1><ul><li>one</li><li>two</li></ul>")
    );

    let named = run_description_editor(
        directory.path(),
        613,
        "<p>Old words.</p>",
        &fake_editor("basename \"$0\" > \"$0\""),
    )
    .unwrap();
    assert_eq!(
        named.as_deref(),
        Some("<p>ticket-613.md</p>"),
        "the file is named after the work item it holds"
    );

    let emptied = run_description_editor(
        directory.path(),
        613,
        "<p>Old</p>",
        &fake_editor(": > \"$0\""),
    )
    .unwrap();
    assert_eq!(
        emptied.as_deref(),
        Some(""),
        "an emptied file clears the description"
    );
}

#[test]
fn an_untouched_file_writes_nothing_and_an_editor_that_fails_says_so() {
    let directory = tempdir().unwrap();
    let mut app = App::new(vec![ticket(3)]);
    app.shell.enable_sync();
    app.work_items.set_table_viewport(3);
    let key = app.work_items.selected_ticket().unwrap().key.clone();
    let mut runtime = offline_runtime();

    let unchanged = run_description_editor(
        directory.path(),
        key.id,
        "<p>Left <b>alone</b>.</p>",
        &["true".to_owned()],
    )
    .unwrap();
    assert_eq!(unchanged, None, "a file nobody typed into is not an edit");
    apply_description_outcome(&mut app, &mut runtime, &key, Ok(unchanged));
    let (message, level) = app.shell.notification().expect("the run says what it did");
    assert!(message.contains("description unchanged"), "{message}");
    assert_eq!(level, NotificationLevel::Info);
    assert!(!app.work_items.edits_pending(), "nothing was sent");

    let failed = run_description_editor(
        directory.path(),
        key.id,
        "<p>Left alone.</p>",
        &["false".to_owned()],
    );
    assert!(
        failed.is_err(),
        "an editor that exits non-zero saves nothing"
    );
    apply_description_outcome(&mut app, &mut runtime, &key, failed);
    let (message, level) = app.shell.notification().expect("a failure is reported");
    assert!(message.contains("description not saved"), "{message}");
    assert_eq!(level, NotificationLevel::Error);
    assert!(!app.work_items.edits_pending());

    let missing = run_description_editor(
        directory.path(),
        key.id,
        "<p>Left alone.</p>",
        &["definitely-not-an-editor-xyz".to_owned()],
    );
    assert!(
        missing.is_err(),
        "an editor that cannot start saves nothing"
    );
    apply_description_outcome(&mut app, &mut runtime, &key, missing);
    assert!(!app.work_items.edits_pending());
    assert_eq!(
        app.work_items.selected_ticket().unwrap().description_html,
        "",
        "the row is exactly as it was"
    );
}

#[test]
fn an_edited_description_reaches_azure_devops_and_the_details_pane() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tickets.sqlite3");
    let mut stored = ticket(3);
    stored.description_html = "<p>Stored copy.</p>".into();
    stored.description = "Stored copy.".into();
    stored.revision = 9;
    let (mut app, mut repository, mut runtime) =
        synced_app(&path, FakeAzure::storing(stored.clone()));
    let key = app.work_items.selected_ticket().unwrap().key.clone();

    apply_description_outcome(
        &mut app,
        &mut runtime,
        &key,
        Ok(Some("<p>Rewritten in the editor.</p>".to_owned())),
    );
    assert_eq!(
        app.work_items.selected_ticket().unwrap().description,
        "Rewritten in the editor.",
        "the details pane reads the new description before the network answers"
    );
    await_edit(&mut app, &mut repository, &mut runtime);

    assert_eq!(app.work_items.ticket_by_key(&key), Some(&stored));
    assert_eq!(
        app.shell.notification().map(|(message, _)| message),
        Some("Updated #3 · Description → updated")
    );
}
