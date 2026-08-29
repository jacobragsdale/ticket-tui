use super::*;

#[test]
fn clicking_the_pane_divider_neither_acts_nor_selects_text() {
    let mut app = App::new(vec![ticket(1, "One", "2026-01-02T00:00:00Z")]);
    let rect = Rect {
        x: 60,
        y: 5,
        width: 1,
        height: 10,
    };
    app.shell.set_content_layout(
        Rect {
            x: 0,
            y: 4,
            width: 130,
            height: 20,
        },
        Some(DividerOrientation::Vertical),
    );
    // A selectable pane sits under the divider; pressing the divider must
    // still not start a selection in it.
    app.shell.hit_regions.push(crate::pointer::region(
        Rect {
            x: 0,
            y: 4,
            width: 130,
            height: 20,
        },
        PointerTarget::FocusDetails,
        crate::pointer::PointerLayer::Base,
        Some(SelectableSurface::Details),
        None,
    ));
    app.shell.hit_regions.push(crate::pointer::region(
        rect,
        PointerTarget::PaneDivider,
        crate::pointer::PointerLayer::Base,
        None,
        None,
    ));
    app.shell.session_dirty = false;

    let point = |kind| MouseEvent {
        kind,
        column: rect.x,
        row: rect.y + 3,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(point(MouseEventKind::Down(MouseButton::Left)));
    let update = app.handle_mouse(point(MouseEventKind::Up(MouseButton::Left)));

    assert!(matches!(update.action, AppAction::None));
    assert!(
        app.shell.selection().is_none(),
        "a divider press selects no text"
    );
    assert_eq!(app.shell.pane_split_wide, DEFAULT_PANE_SPLIT_WIDE);
    assert!(
        !app.shell.session_dirty,
        "a press with no drag changes nothing"
    );

    app.shell.pane_split_wide = 71;
    app.shell.pane_split_stacked = 45;
    let session = app.snapshot_session();
    let mut restored = App::new(vec![ticket(1, "One", "2026-01-02T00:00:00Z")]);
    restored.restore_session(session);
    assert_eq!(
        restored.shell.pane_split_wide, 71,
        "the split is remembered"
    );
    assert_eq!(restored.shell.pane_split_stacked, 45);

    restored.shell.session_dirty = false;
    restored.run_command(CommandId::ResetPaneSplit);
    assert_eq!(restored.shell.pane_split_wide, DEFAULT_PANE_SPLIT_WIDE);
    assert_eq!(
        restored.shell.pane_split_stacked,
        DEFAULT_PANE_SPLIT_STACKED
    );
    assert!(restored.shell.session_dirty);
}
