use super::*;
use crate::app::TabId;
use crate::app::repos::tests::{crossed_app, repos_app};

#[test]
fn the_table_draws_every_repository_with_its_counts_and_local_state() {
    let mut app = repos_app();
    let text = render_text(150, 24, &mut app);

    assert!(pane_reads(&text, "Repos", "4"), "{text}");
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
        text.contains("\u{2191} 1") && text.contains("\u{2193} 2"),
        "ahead and behind are counted: {text}"
    );
    assert!(
        text.contains("\u{2014}"),
        "one nobody has here reads as an em dash: {text}"
    );
    assert!(
        text.contains("\u{25cb} 1"),
        "a pipeline that has not run yet is an open circle before the count: {text}"
    );
}

#[test]
fn the_pipelines_column_says_how_the_pipelines_last_went() {
    let mut app = crossed_app();
    let text = render_text(150, 24, &mut app);

    assert!(
        text.contains("\u{2713} 1"),
        "the count carries the glyph of the last run: {text}"
    );
}

#[test]
fn the_details_pane_carries_the_urls_the_local_copy_and_what_is_open() {
    let mut app = repos_app();
    // The rows read in name order, so ticket-tui is the last of the four.
    app.repos.cursor.focus(3);
    let text = render_text(170, 44, &mut app);

    assert!(
        text.contains("Default branch main"),
        "a name wider than the value column still keeps a space: {text}"
    );
    assert!(text.contains("2.0 MB"), "the size reads in units: {text}");
    assert!(text.contains("URLs"), "{text}");
    assert!(
        text.contains("Copy web") && text.contains("Copy HTTPS") && text.contains("Copy SSH"),
        "the URLs are chips that copy them: {text}"
    );
    assert!(
        !text.contains("git@ssh.dev.azure.com") && !text.contains("_git/ticket-tui"),
        "and no URL is printed: {text}"
    );
    assert!(text.contains("Local"), "{text}");
    assert!(
        text.contains("/Users/jacob/Development/ticket-tui"),
        "the path of the clone: {text}"
    );
    assert!(
        text.contains("read just now"),
        "and when the workspace was last read: {text}"
    );
    let open = text
        .find("Open against it")
        .expect("the pull requests' section");
    let request = text
        .find("!11  Split the files")
        .expect("a pull request line");
    let pipelines = text[open..]
        .find("Pipelines")
        .map(|at| open + at)
        .expect("the pipelines' own section");
    let pipeline = text.find("ticket-tui CI").expect("a pipeline line");
    assert!(
        open < request && request < pipelines && pipelines < pipeline,
        "pull requests under Open against it, pipelines under their own heading: {text}"
    );
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
        [
            '\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}',
            '\u{2827}', '\u{2807}', '\u{280f}'
        ]
        .iter()
        .any(|glyph| text.contains(*glyph)),
        "with the same spinner every other wait uses: {text}"
    );
}

#[test]
fn clicking_a_copy_chip_copies_that_url_and_y_copies_the_ssh_one() {
    let mut app = repos_app();
    app.repos.cursor.focus(3);
    render_text(170, 44, &mut app);

    let ssh = app
        .shell
        .hit_regions
        .find_target(
            |target| matches!(target, PointerTarget::CopyText { text, .. } if text.starts_with("git@")),
        )
        .expect("the ssh chip copies")
        .rect;
    let action = click(&mut app, ssh.x + 2, ssh.y);
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
fn clicking_the_path_of_a_clone_copies_the_path() {
    let mut app = repos_app();
    app.repos.cursor.focus(3);
    render_text(170, 44, &mut app);

    let path = app
        .shell
        .hit_regions
        .find_target(|target| {
            matches!(
                target,
                PointerTarget::CopyText {
                    content: crate::app::CopiedContent::Path,
                    ..
                }
            )
        })
        .expect("the path copies")
        .rect;
    assert_eq!(
        u32::from(path.width),
        "/Users/jacob/Development/ticket-tui".chars().count() as u32,
        "the target is the text, not the whole row"
    );
    let action = click(&mut app, path.x + 2, path.y);
    assert!(
        matches!(
            action,
            crate::app::AppAction::Copy {
                ref text,
                content: crate::app::CopiedContent::Path,
            } if text == "/Users/jacob/Development/ticket-tui"
        ),
        "got {action:?}"
    );
}

#[test]
fn the_repos_tab_reads_at_every_breakpoint() {
    let mut app = repos_app();
    for width in [150, 90, 60] {
        app.select_tab(TabId::Repos);
        let text = render_text(width, 24, &mut app);
        assert!(pane_reads(&text, "Repos", "4"), "{width} columns: {text}");
        assert!(text.contains("ticket-tui"), "{width} columns: {text}");
    }
}

#[test]
fn the_buttons_are_where_they_are_painted_even_when_the_path_wraps() {
    let mut app = repos_app();
    app.repos.cursor.focus(3);
    // A clone deep enough that its path wraps in a narrow pane, pushing every
    // target under it down a row.
    let mut local = crate::app::repos::tests::local("main", false, 0, 0);
    local.path = std::path::PathBuf::from(
        "/Users/jacob/Development/a-rather-deeply-nested-workspace/ticket-tui",
    );
    app.repos.set_local(vec![("aaa-111".to_owned(), local)]);
    let mut terminal = Terminal::new(TestBackend::new(110, 44)).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let whole = Rect::new(0, 0, 110, 44);

    for (label, wanted) in [
        (
            "Copy SSH",
            target_rect(
                &app,
                |target| matches!(target, PointerTarget::CopyText { text, .. } if text.starts_with("git@")),
            ),
        ),
        (
            "[Fetch]",
            target_rect(&app, |target| {
                matches!(
                    target,
                    PointerTarget::RunCommand(crate::command::CommandId::FetchRepo)
                )
            }),
        ),
    ] {
        let painted = find_buffer_text_in(&buffer, whole, label)
            .unwrap_or_else(|| panic!("{label} is painted"));
        assert_eq!(
            wanted.y, painted.1,
            "{label}: the click target is on the row it is painted on"
        );
        assert!(
            wanted.x <= painted.0 && painted.0 < wanted.x + wanted.width,
            "{label}: and covers it: {wanted:?} vs {painted:?}"
        );
    }
}
