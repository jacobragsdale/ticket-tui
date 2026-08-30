//! The URL launcher and the pointer shape the terminal is told to draw.

use super::*;

#[test]
fn only_well_formed_https_urls_reach_the_launcher() {
    let error = open_https_url("file:///tmp/not-a-ticket", &failing_opener).unwrap_err();
    assert!(error.to_string().contains("only HTTPS"), "{error}");
    let error = open_https_url("not a url", &failing_opener).unwrap_err();
    assert!(error.to_string().contains("invalid"), "{error}");
    let error = open_https_url("https://dev.azure.com/demo", &failing_opener).unwrap_err();
    assert!(
        error.to_string().contains("system URL launcher failed"),
        "{error}"
    );
}

#[test]
fn mouse_pointer_sequences_set_and_reset_link_hover() {
    assert_eq!(
        mouse_pointer_for_hover(Some(&PointerTarget::OpenSelectedUrl), None),
        MousePointerShape::Link
    );
    assert_eq!(
        mouse_pointer_for_hover(Some(&PointerTarget::OpenInBrowser { index: 0 }), None),
        MousePointerShape::Link
    );
    assert_eq!(
        mouse_pointer_for_hover(
            Some(&PointerTarget::EditField {
                field: ticket_tui::pointer::EditableField::State
            }),
            None
        ),
        MousePointerShape::Link,
        "an editable details field points the same way a link does"
    );
    assert_eq!(
        mouse_pointer_for_hover(Some(&PointerTarget::TableRow { index: 0 }), None),
        MousePointerShape::Default
    );
    assert_eq!(
        mouse_pointer_for_hover(
            Some(&PointerTarget::PaneDivider),
            Some(DividerOrientation::Vertical)
        ),
        MousePointerShape::ColResize
    );
    assert_eq!(
        mouse_pointer_for_hover(
            Some(&PointerTarget::PaneDivider),
            Some(DividerOrientation::Horizontal)
        ),
        MousePointerShape::RowResize
    );
    assert_eq!(
        mouse_pointer_for_hover(Some(&PointerTarget::PaneDivider), None),
        MousePointerShape::Default,
        "the narrow layout has no divider to resize"
    );

    let mut output = Vec::new();
    write_mouse_pointer_shape(&mut output, MousePointerShape::Link).unwrap();
    write_mouse_pointer_shape(&mut output, MousePointerShape::ColResize).unwrap();
    write_mouse_pointer_shape(&mut output, MousePointerShape::RowResize).unwrap();
    write_mouse_pointer_shape(&mut output, MousePointerShape::Default).unwrap();

    assert_eq!(
        output,
        b"\x1b]22;pointer\x1b\\\x1b]22;col-resize\x1b\\\x1b]22;row-resize\x1b\\\x1b]22;\x1b\\"
    );
}
