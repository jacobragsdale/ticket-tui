use super::*;
use crate::app::TabId;
use crate::app::repos::tests::repos_app;

#[test]
fn the_table_draws_every_repository_with_its_counts_and_local_state() {
    let mut app = repos_app();
    let text = render_text(150, 24, &mut app);

    assert!(text.contains("Repos 4"), "{text}");
    assert!(text.contains("ticket-tui"), "{text}");
    assert!(
        text.contains("main \u{2713}"),
        "a clean clone reads as a tick: {text}"
    );
    assert!(
        text.contains("feature/x *"),
        "a dirty one carries the asterisk: {text}"
    );
    assert!(
        text.contains("\u{2191}1") && text.contains("\u{2193}2"),
        "ahead and behind are counted: {text}"
    );
    assert!(
        text.contains("\u{2014}"),
        "one nobody has here reads as an em dash: {text}"
    );
}

#[test]
fn the_details_pane_carries_the_urls_the_local_copy_and_what_is_open() {
    let mut app = repos_app();
    // The rows read in name order, so ticket-tui is the last of the four.
    app.repos.cursor.focus(3);
    let text = render_text(170, 44, &mut app);

    assert!(text.contains("Default branch"), "{text}");
    assert!(text.contains("2.0 MB"), "the size reads in units: {text}");
    assert!(text.contains("URLs"), "{text}");
    assert!(text.contains("git@ssh.dev.azure.com"), "{text}");
    assert!(text.contains("Local"), "{text}");
    assert!(
        text.contains("/Users/jacob/Development/ticket-tui"),
        "the path of the clone: {text}"
    );
    assert!(text.contains("Open against it"), "{text}");
    assert!(text.contains("!11  Split the files"), "{text}");
    assert!(text.contains("ticket-tui CI"), "{text}");
    assert!(
        text.contains("[Fetch]") && text.contains("[Pull]"),
        "{text}"
    );
}

#[test]
fn a_repository_nobody_has_here_offers_to_clone_it() {
    let mut app = repos_app();
    // home-server, which is not on this machine.
    app.repos.cursor.focus(1);
    let text = render_text(170, 44, &mut app);

    assert!(text.contains("[Clone]"), "{text}");
    assert!(
        text.contains("Not in /Users/jacob/Development"),
        "and says where it looked: {text}"
    );
}

#[test]
fn clicking_clone_runs_it_and_a_running_job_says_so_in_the_column() {
    let mut app = repos_app();
    // home-server, which is not on this machine.
    app.repos.cursor.focus(1);
    render_text(170, 44, &mut app);

    let button = app
        .shell
        .hit_regions
        .find_target(|target| {
            matches!(
                target,
                PointerTarget::RunCommand(crate::command::CommandId::CloneRepo)
            )
        })
        .expect("the clone button")
        .rect;
    let action = click(&mut app, button.x + 2, button.y);
    assert!(
        matches!(
            action,
            crate::app::AppAction::LocalGit(crate::local::LocalRequest::Clone { .. })
        ),
        "the button is the key, got {action:?}"
    );

    // And while git is at it, the column says so where the status would be.
    app.repos
        .set_job("ccc-333", Some(crate::model::GitJob::Cloning));
    let text = render_text(170, 44, &mut app);
    assert!(text.contains("cloning\u{2026}"), "{text}");
    assert!(
        ['\u{25d0}', '\u{25d3}', '\u{25d1}', '\u{25d2}']
            .iter()
            .any(|glyph| text.contains(*glyph)),
        "with a glyph that turns while it runs: {text}"
    );
}

#[test]
fn clicking_a_url_copies_it_and_y_copies_the_ssh_one() {
    let mut app = repos_app();
    app.repos.cursor.focus(3);
    render_text(170, 44, &mut app);

    let ssh = app
        .shell
        .hit_regions
        .find_target(
            |target| matches!(target, PointerTarget::CopyText(text) if text.starts_with("git@")),
        )
        .expect("the ssh line copies")
        .rect;
    let action = click(&mut app, ssh.x + 10, ssh.y);
    assert!(
        matches!(action, crate::app::AppAction::Copy { ref text, .. } if text.starts_with("git@")),
        "got {action:?}"
    );

    let action = app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(
        matches!(action, crate::app::AppAction::Copy { ref text, .. } if text.starts_with("git@")),
        "y copies the ssh URL too, got {action:?}"
    );
}

#[test]
fn the_repos_tab_reads_at_every_breakpoint() {
    let mut app = repos_app();
    for width in [150, 90, 60] {
        app.select_tab(TabId::Repos);
        let text = render_text(width, 24, &mut app);
        assert!(text.contains("Repos 4"), "{width} columns: {text}");
        assert!(text.contains("ticket-tui"), "{width} columns: {text}");
    }
}
