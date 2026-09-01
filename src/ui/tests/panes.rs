//! The pane system: the arrangement every tab draws its list and its details
//! in, and the seam between them.

use super::*;
use crate::app::acr::tests::acr_app;
use crate::app::aks::tests::aks_app;
use crate::app::environments::tests::environments_app;
use crate::app::key_vault::tests::key_vault_app;
use crate::app::pipelines::tests::pipelines_app;
use crate::app::pull_requests::tests::pull_requests_app;
use crate::app::repos::tests::repos_app;
use crate::app::{DividerOrientation, TabId};
use crate::pointer::PaneSplit;

/// Every tab, on the tab it belongs to, with the names of its two panes.
fn tabs() -> Vec<(TabId, App, &'static str, &'static str)> {
    let mut work_items = App::new(vec![ticket()]);
    work_items.select_tab(TabId::WorkItems);
    let mut repos = repos_app();
    repos.select_tab(TabId::Repos);
    let mut pull_requests = pull_requests_app();
    pull_requests.select_tab(TabId::PullRequests);
    let mut pipelines = pipelines_app();
    pipelines.select_tab(TabId::Pipelines);
    let mut aks = aks_app();
    aks.select_tab(TabId::Aks);
    let mut acr = acr_app();
    acr.select_tab(TabId::Acr);
    let mut key_vault = key_vault_app();
    key_vault.select_tab(TabId::KeyVault);
    let mut environments = environments_app();
    environments.select_tab(TabId::Environments);
    vec![
        (TabId::WorkItems, work_items, "Tickets", "Details"),
        (TabId::Repos, repos, "Repos", "Repository"),
        (
            TabId::PullRequests,
            pull_requests,
            "Pull requests",
            "Pull request",
        ),
        (TabId::Pipelines, pipelines, "Pipelines", "Run"),
        (TabId::Aks, aks, "Pods", "Pod"),
        (TabId::Acr, acr, "Registries", "Registry"),
        (TabId::KeyVault, key_vault, "Vaults", "Vault"),
        (
            TabId::Environments,
            environments,
            "Environments",
            "Promotion",
        ),
    ]
}

fn seam(app: &App, split: PaneSplit) -> Rect {
    app.shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::PaneDivider { split: found } if *found == split))
        .unwrap_or_else(|| panic!("{split:?} draws a seam"))
        .rect
}

#[test]
fn every_tab_arranges_its_two_panes_the_same_way_at_every_breakpoint() {
    for (tab, mut app, list, details) in tabs() {
        let wide = render_text(130, 30, &mut app);
        assert!(wide.contains(list), "{tab:?} lists at 130 columns: {wide}");
        assert!(
            wide.contains(details),
            "{tab:?} shows details beside the list: {wide}"
        );
        let workspace = seam(&app, PaneSplit::Workspace);
        assert_eq!(workspace.width, 1, "{tab:?} panes share one border column");
        assert_eq!(
            app.shell.divider_orientation(),
            Some(DividerOrientation::Vertical),
            "{tab:?} puts them side by side while there is room"
        );

        // Between the breakpoints they stack, and the seam turns with them.
        let stacked = render_text(90, 30, &mut app);
        assert!(stacked.contains(list), "{tab:?} at 90 columns: {stacked}");
        assert!(stacked.contains(details), "{tab:?}: {stacked}");
        let workspace = seam(&app, PaneSplit::Workspace);
        assert_eq!(workspace.height, 1, "{tab:?} panes share one border row");
        assert_eq!(
            app.shell.divider_orientation(),
            Some(DividerOrientation::Horizontal)
        );

        // Narrower than that there is room for one, and the chips say which.
        let narrow = render_text(60, 20, &mut app);
        assert!(
            narrow.contains(&format!("[{list}]")) && narrow.contains(&format!("[{details}]")),
            "{tab:?} wears both chips: {narrow}"
        );
        assert_eq!(
            app.shell.divider_orientation(),
            None,
            "{tab:?} has no seam with one pane on screen"
        );
    }
}

#[test]
fn the_chips_switch_panes_on_every_tab_and_both_of_them_are_clickable() {
    for (tab, mut app, list, details) in tabs() {
        render_text(60, 20, &mut app);
        let chip = app
            .shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::NarrowDetails))
            .unwrap_or_else(|| panic!("{tab:?} offers the details chip"))
            .rect;
        click(&mut app, chip.x, chip.y);
        assert!(app.shell.narrow_details, "{tab:?} switched to its details");

        // The pane it switched to wears the chips too, so there is a way back.
        let text = render_text(60, 20, &mut app);
        assert!(
            text.contains(&format!("[{list}]")),
            "{tab:?} keeps the way back on screen: {text}"
        );
        assert!(text.contains(&format!("[{details}]")), "{tab:?}: {text}");
        let back = app
            .shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::NarrowTickets))
            .unwrap_or_else(|| panic!("{tab:?} offers the list chip"))
            .rect;
        click(&mut app, back.x, back.y);
        assert!(!app.shell.narrow_details, "{tab:?} switched back");
    }
}

#[test]
fn dragging_the_seam_resizes_the_panes_on_every_tab_and_the_split_is_shared() {
    for (tab, mut app, _, _) in tabs() {
        render_text(130, 30, &mut app);
        let before = seam(&app, PaneSplit::Workspace);
        app.shell.session_dirty = false;
        drag(
            &mut app,
            (before.x, before.y + 2),
            (before.x + 12, before.y + 2),
        );
        assert!(
            app.shell.pane_split_wide > crate::app::DEFAULT_PANE_SPLIT_WIDE,
            "{tab:?} moved the split"
        );
        assert!(
            app.shell.session_dirty,
            "{tab:?} is worth remembering the split of"
        );
        render_text(130, 30, &mut app);
        assert!(
            seam(&app, PaneSplit::Workspace).x > before.x,
            "{tab:?} redraws the seam where it was dragged"
        );
    }

    // One split, kept by the shell rather than by a screen, so the next tab
    // opens arranged the way the last one was left.
    let mut app = repos_app();
    app.select_tab(TabId::Repos);
    render_text(130, 30, &mut app);
    let before = seam(&app, PaneSplit::Workspace);
    drag(
        &mut app,
        (before.x, before.y + 2),
        (before.x + 12, before.y + 2),
    );
    render_text(130, 30, &mut app);
    let dragged = seam(&app, PaneSplit::Workspace);
    assert!(dragged.x > before.x);

    app.select_tab(TabId::PullRequests);
    render_text(130, 30, &mut app);
    assert_eq!(
        seam(&app, PaneSplit::Workspace).x,
        dragged.x,
        "the next tab opens on the split the last one was left at"
    );
}

#[test]
fn the_seam_inside_the_pipelines_details_pane_drags_like_any_other() {
    let mut app = pipelines_app();
    app.select_tab(TabId::Pipelines);
    render_text(130, 30, &mut app);
    let before = seam(&app, PaneSplit::Details);
    assert_eq!(before.height, 1, "the run and its log share one border row");
    assert_eq!(
        app.shell.seam_orientation(PaneSplit::Details),
        Some(DividerOrientation::Horizontal),
        "a tall pane stacks its halves"
    );

    drag(
        &mut app,
        (before.x + 4, before.y),
        (before.x + 4, before.y + 3),
    );
    assert!(app.shell.pane_split_details > crate::app::DEFAULT_PANE_SPLIT_DETAILS);
    render_text(130, 30, &mut app);
    assert!(
        seam(&app, PaneSplit::Details).y > before.y,
        "the log seam moved down"
    );

    // Stacked, the details pane is wide and short, so it divides the other
    // way and the seam turns with it.
    render_text(90, 30, &mut app);
    assert_eq!(
        app.shell.seam_orientation(PaneSplit::Details),
        Some(DividerOrientation::Vertical)
    );
    assert_eq!(seam(&app, PaneSplit::Details).width, 1);
}

#[test]
fn the_pane_commands_answer_on_every_tab() {
    for (tab, mut app, list, details) in tabs() {
        render_text(60, 20, &mut app);
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(app.shell.narrow_details, "{tab:?} answers d");
        let text = render_text(60, 20, &mut app);
        assert!(text.contains(&format!("[{details}]")), "{tab:?}: {text}");
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(!app.shell.narrow_details, "{tab:?} answers it again");
        assert!(
            render_text(60, 20, &mut app).contains(&format!("[{list}]")),
            "{tab:?} is back on its list"
        );

        // `Tab` swaps them too, and brings the pane it moves to on screen
        // rather than leaving the keyboard on one that is not drawn.
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(
            app.shell.narrow_details,
            "{tab:?} shows the pane Tab moved to"
        );
        assert!(app.shell.focus.is_details_pane(), "{tab:?} focused it");
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(!app.shell.narrow_details, "{tab:?} came back to its list");

        // And the palette's own pane command, on the tab that is showing.
        app.shell.pane_split_wide = 71;
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        for character in "reset pane".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.shell.pane_split_wide,
            crate::app::DEFAULT_PANE_SPLIT_WIDE,
            "{tab:?} answers Reset pane split"
        );
    }
}

#[test]
fn dragging_across_a_row_copies_its_text_on_every_tab() {
    for (tab, mut app, _, _) in tabs() {
        render_text(130, 30, &mut app);
        let row = app
            .shell
            .hit_regions
            .find_target(|target| matches!(target, PointerTarget::TableRow { index: 0 }))
            .unwrap_or_else(|| panic!("{tab:?} draws a first row"))
            .rect;
        let action = drag(&mut app, (row.x + 1, row.y), (row.x + 20, row.y));
        match action {
            crate::app::AppAction::Copy { text, .. } => {
                assert!(!text.trim().is_empty(), "{tab:?} copied {text:?}");
            }
            other => panic!("{tab:?} dragged across a row and got {other:?}"),
        }
    }
}
