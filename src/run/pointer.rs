//! The mouse pointer shape the terminal draws, and what the hover under it
//! means. Terminals that speak OSC 22 show a hand over anything that opens.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MousePointerShape {
    Default,
    Link,
    ColResize,
    RowResize,
}

impl MousePointerShape {
    const fn escape_sequence(self) -> &'static [u8] {
        match self {
            Self::Default => b"\x1b]22;\x1b\\",
            Self::Link => b"\x1b]22;pointer\x1b\\",
            Self::ColResize => b"\x1b]22;col-resize\x1b\\",
            Self::RowResize => b"\x1b]22;row-resize\x1b\\",
        }
    }
}

pub(super) fn sync_mouse_pointer(app: &App, current: &mut MousePointerShape) {
    // Which way the seam under the pointer runs is the seam's business, not
    // the layout's: a tab can draw more than one, running different ways.
    let hovered = app.shell.hovered();
    let seam = match hovered {
        Some(PointerTarget::PaneDivider { split }) => app.shell.seam_orientation(*split),
        _ => None,
    };
    let desired = mouse_pointer_for_hover(hovered, seam);
    if desired == *current {
        return;
    }
    if write_mouse_pointer_shape(&mut io::stdout(), desired).is_ok() {
        *current = desired;
    }
}

/// The shape for one hover: `divider` is which way the seam under the pointer
/// runs, for a hover that is on one.
pub(super) fn mouse_pointer_for_hover(
    target: Option<&PointerTarget>,
    divider: Option<DividerOrientation>,
) -> MousePointerShape {
    match target {
        Some(
            PointerTarget::OpenInBrowser { .. }
            | PointerTarget::OpenSelectedUrl
            | PointerTarget::EditField { .. }
            | PointerTarget::RunCommand(_),
        ) => MousePointerShape::Link,
        Some(PointerTarget::PaneDivider { .. }) => match divider {
            Some(DividerOrientation::Vertical) => MousePointerShape::ColResize,
            Some(DividerOrientation::Horizontal) => MousePointerShape::RowResize,
            None => MousePointerShape::Default,
        },
        _ => MousePointerShape::Default,
    }
}

pub(super) fn write_mouse_pointer_shape(
    writer: &mut impl Write,
    shape: MousePointerShape,
) -> io::Result<()> {
    writer.write_all(shape.escape_sequence())?;
    writer.flush()
}
