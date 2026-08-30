//! One way to draw a pane, wherever the app draws one.
//!
//! Every tab shows the same two things — a list, and the details of whatever
//! the cursor is on — and every tab arranges them the same way: side by side
//! while there is room for both, stacked when the terminal is narrower, and
//! one at a time when it is narrower still. The seam between them is a border
//! the two panes share, and dragging it moves the split.
//!
//! A tab says what its two panes are and how to paint them; everything else —
//! which arrangement the width calls for, where the seam falls, what it is
//! worth in cells, and the chips that switch panes when only one fits — is
//! settled here, so a pane looks and behaves the same on all four tabs.

use super::*;

/// The two panes either side of one seam. A pair rather than two closures
/// because both halves need the screen, and two closures cannot each hold it.
pub(crate) trait PanePair {
    fn first(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect);
    fn second(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect);
}

/// What a tab calls its two panes, for the chips the one-pane layout switches
/// with. The list name changes as the tab does — Pipelines becomes the name of
/// whichever pipeline's runs are open — so these are borrowed, not static.
pub(crate) struct PaneNames<'a> {
    pub(crate) list: &'a str,
    pub(crate) details: &'a str,
}

/// Columns each pane keeps while the seam between them is dragged side to
/// side: enough of a list to read a row by, enough of a details pane to read a
/// wrapped line in.
const MIN_LIST_COLUMNS: u16 = 40;

const MIN_DETAILS_COLUMNS: u16 = 30;

/// Rows each pane keeps while a seam is dragged up and down: a border either
/// side, a header, and something under it.
const MIN_PANE_ROWS: u16 = 6;

/// The same, for a seam inside a pane rather than across the workspace: what
/// is being divided is already one pane's worth of room, so both halves keep
/// less.
const MIN_INNER_COLUMNS: u16 = 24;

const MIN_INNER_ROWS: u16 = 4;

/// The shortest a pane can be and still be worth dividing into two stacked
/// halves, and the narrowest it can be and still be worth dividing side by
/// side. Below both it draws its first pane alone.
const INNER_STACK_ROWS: u16 = 16;

const INNER_SIDE_COLUMNS: u16 = 60;

/// A tab's whole workspace: its list, the details of what the cursor is on,
/// and the seam between them.
///
/// Which arrangement the width calls for is the one rule every tab follows.
/// The narrowest layout has no room for two panes at once, so it shows one and
/// wears the chips that swap them.
pub(crate) fn render_workspace(
    frame: &mut Frame<'_>,
    shell: &mut Shell,
    area: Rect,
    names: &PaneNames<'_>,
    panes: &mut dyn PanePair,
) {
    if area.width >= WIDE_BREAKPOINT {
        render_split(
            frame,
            shell,
            area,
            PaneSplit::Workspace,
            DividerOrientation::Vertical,
            (MIN_LIST_COLUMNS, MIN_DETAILS_COLUMNS),
            panes,
        );
    } else if area.width >= NARROW_BREAKPOINT {
        render_split(
            frame,
            shell,
            area,
            PaneSplit::Workspace,
            DividerOrientation::Horizontal,
            (MIN_PANE_ROWS, MIN_PANE_ROWS),
            panes,
        );
    } else {
        if shell.narrow_details {
            panes.second(frame, shell, area);
        } else {
            panes.first(frame, shell, area);
        }
        render_switcher(frame, shell, area, names);
    }
}

/// A seam inside one pane: the pipelines log under its run today. It follows
/// the pane's shape rather than the terminal's — a tall pane stacks its
/// halves, a wide one puts them side by side — and a pane with room for
/// neither keeps the first half alone.
pub(crate) fn render_inner_split(
    frame: &mut Frame<'_>,
    shell: &mut Shell,
    area: Rect,
    panes: &mut dyn PanePair,
) {
    if area.height >= INNER_STACK_ROWS {
        render_split(
            frame,
            shell,
            area,
            PaneSplit::Details,
            DividerOrientation::Horizontal,
            (MIN_INNER_ROWS, MIN_INNER_ROWS),
            panes,
        );
    } else if area.width >= INNER_SIDE_COLUMNS {
        render_split(
            frame,
            shell,
            area,
            PaneSplit::Details,
            DividerOrientation::Vertical,
            (MIN_INNER_COLUMNS, MIN_INNER_COLUMNS),
            panes,
        );
    } else {
        panes.first(frame, shell, area);
    }
}

/// Two panes and the seam they share. The seam is registered before either
/// pane paints, because a pane reads which way it runs to know whether it
/// shares its bottom border with a neighbour, and painted after both, because
/// the glyphs it recolours are the ones they merged.
fn render_split(
    frame: &mut Frame<'_>,
    shell: &mut Shell,
    area: Rect,
    split: PaneSplit,
    orientation: DividerOrientation,
    (first_min, second_min): (u16, u16),
    panes: &mut dyn PanePair,
) {
    let direction = match orientation {
        DividerOrientation::Vertical => Direction::Horizontal,
        DividerOrientation::Horizontal => Direction::Vertical,
    };
    let percent = shell.split_percent(split, orientation);
    let areas = Layout::new(
        direction,
        [Constraint::Percentage(percent), Constraint::Fill(1)],
    )
    .spacing(Spacing::Overlap(1))
    .split(area);
    shell.set_seam(
        split,
        PaneSeam {
            orientation,
            workspace: area,
            first_min,
            second_min,
        },
    );
    panes.first(frame, shell, areas[0]);
    panes.second(frame, shell, areas[1]);
    render_seam(frame, shell, areas[0], areas[1], split, orientation);
}

/// Registers the border the two panes share as the draggable seam, and gives
/// it the neutral border colour: the seam belongs to neither pane, so a
/// focused pane does not paint half of it in the accent. The glyphs are the
/// merged ones the panes have already drawn. Hovering reverses it through the
/// usual hover pass.
fn render_seam(
    frame: &mut Frame<'_>,
    shell: &mut Shell,
    first: Rect,
    second: Rect,
    split: PaneSplit,
    orientation: DividerOrientation,
) {
    let Some(rect) = seam_rect(first, second, orientation) else {
        return;
    };
    let buffer = frame.buffer_mut();
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            let cell = &mut buffer[(x, y)];
            let style = cell.style().fg(theme().border);
            cell.set_style(style);
        }
    }
    shell.hit_regions.push(region(
        rect,
        PointerTarget::PaneDivider { split },
        PointerLayer::Base,
        None,
        None,
    ));
}

/// The one border column, or row, the two panes share.
fn seam_rect(first: Rect, second: Rect, orientation: DividerOrientation) -> Option<Rect> {
    let rect = match orientation {
        DividerOrientation::Vertical => Rect {
            x: second.x,
            y: first.y,
            width: first.right().checked_sub(second.x)?,
            height: first.height,
        },
        DividerOrientation::Horizontal => Rect {
            x: first.x,
            y: second.y,
            width: first.width,
            height: first.bottom().checked_sub(second.y)?,
        },
    };
    (rect.width > 0 && rect.height > 0).then_some(rect)
}

/// The chips the one-pane layout switches with, worn on the top border of
/// whichever pane is showing: the one you are on in the accent, the other
/// waiting to be clicked. They stand where the pane's own name would go and
/// say the same thing, so that name is painted over rather than crowded.
fn render_switcher(frame: &mut Frame<'_>, shell: &mut Shell, area: Rect, names: &PaneNames<'_>) {
    let Some(width) = area.width.checked_sub(2).filter(|width| *width > 0) else {
        return;
    };
    if area.height == 0 {
        return;
    }
    let border = theme().border_type.to_border_set().horizontal_top;
    let border_style = Style::default().fg(theme().border);
    let buffer = frame.buffer_mut();
    for x in area.x.saturating_add(1)..area.right().saturating_sub(1) {
        let cell = &mut buffer[(x, area.y)];
        cell.set_symbol(border);
        cell.set_style(border_style);
    }

    // Laid out like the pane name it replaces: a space off the corner, and a
    // space between the chips and after them.
    let mut spans = vec![Span::raw(" ")];
    let mut chips = Vec::new();
    let mut x = area.x.saturating_add(2);
    let right = area.right().saturating_sub(1);
    for (name, target, showing) in [
        (
            names.list,
            PointerTarget::NarrowTickets,
            !shell.narrow_details,
        ),
        (
            names.details,
            PointerTarget::NarrowDetails,
            shell.narrow_details,
        ),
    ] {
        let label = format!("[{name}]");
        let chip = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
        if x.saturating_add(chip) > right {
            break;
        }
        spans.push(Span::styled(label, pill_style(false, showing)));
        spans.push(Span::raw(" "));
        chips.push((Rect::new(x, area.y, chip, 1), target));
        x = x.saturating_add(chip).saturating_add(1);
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(area.x.saturating_add(1), area.y, width, 1),
    );
    for (rect, target) in chips {
        shell
            .hit_regions
            .push(region(rect, target, PointerLayer::Base, None, None));
    }
}
