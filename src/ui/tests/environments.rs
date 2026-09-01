//! The Environments tab: the board, its cells, and the promotion pane beside
//! it.

use super::*;
use crate::app::environments::tests::environments_app;
use crate::app::{Focus, TabId};

/// The tab, drawn, with the board showing.
fn board(width: u16, height: u16, app: &mut App) -> String {
    app.select_tab(TabId::Environments);
    render_text(width, height, app)
}

#[test]
fn the_board_draws_a_column_per_environment_with_the_tag_and_what_is_missing() {
    let mut app = environments_app();
    let text = board(150, 30, &mut app);

    assert!(text.contains("Service"), "the two fixed columns: {text}");
    assert!(text.contains("Namespace"), "{text}");
    assert!(
        text.contains("qa") && text.contains("prod"),
        "one column per [[environments]]: {text}"
    );
    assert!(text.contains("orders-api"), "{text}");
    assert!(text.contains("1.4.0") && text.contains("1.3.9"), "{text}");
    assert!(
        text.contains("\u{2717}1"),
        "the cell counts what prod would be missing: {text}"
    );
    assert!(pane_reads(&text, "Environments", "3 services"), "{text}");
    assert!(
        text.contains("Findings"),
        "the filter bar's one chip: {text}"
    );
}

#[test]
fn the_details_pane_names_the_promotion_and_the_sections_under_it() {
    let mut app = environments_app();
    app.select_tab(TabId::Environments);
    app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    let text = render_text(150, 40, &mut app);

    assert!(
        text.contains("qa \u{2192} prod"),
        "the frame says which promotion it reads: {text}"
    );
    assert!(text.contains("Missing in prod"), "{text}");
    assert!(text.contains("Image"), "{text}");
    assert!(
        text.contains("adds RATE_LIMIT_PER_MIN"),
        "and the diff in the words the pre-flight uses: {text}"
    );
}

#[test]
fn clicking_a_cell_settles_both_the_row_and_the_environment_it_is_under() {
    let mut app = environments_app();
    board(150, 30, &mut app);
    // The cell of the second row under the first environment's column.
    let cell = app
        .shell
        .hit_regions
        .find_target(|target| matches!(target, PointerTarget::TableCell { row: 1, column: 0 }))
        .expect("every cell is a target")
        .rect;

    click(&mut app, cell.x, cell.y);

    assert_eq!(
        app.environments
            .selected()
            .map(|row| row.workload)
            .as_deref(),
        Some("orders-api"),
        "the row it was on"
    );
    assert_eq!(app.environments.column(), 0, "and the column it was under");
    assert_eq!(app.shell.focus, Focus::Tickets);
    assert!(
        render_text(150, 30, &mut app).contains("Promotion \u{00b7} orders-api \u{00b7} qa"),
        "the pane follows the click"
    );
}

#[test]
fn with_no_deployment_repository_the_board_is_the_one_line_saying_where_it_looked() {
    let mut app = App::new(vec![ticket()]);
    app.environments
        .set_deployment(None, Some("no clone of deployment in /srv/code".to_owned()));
    let text = board(150, 30, &mut app);

    assert!(
        text.contains("no clone of deployment in /srv/code"),
        "{text}"
    );
}
