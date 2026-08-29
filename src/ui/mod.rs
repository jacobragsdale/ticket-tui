use std::cmp::Ordering;
use std::sync::OnceLock;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row, Table, Wrap,
};
use time::OffsetDateTime;

use crate::app::{
    App, ChildProgress, DividerOrientation, Focus, FormOverlay, HitRegions, NotificationLevel,
    PRIORITY_CHOICES, PROGRESS_BAR_CELLS, PromptField, RowDensity, Screen, SearchOrder, Shell,
    UNASSIGNED_LABEL, WorkItemMode, WorkItemsScreen,
};
use crate::command::{COMMANDS, key_label_for};
use crate::filter::{FacetTarget, FilterField, WorkItemSchema};
use crate::model::{
    FamilySnapshot, FamilyTreeEntry, HistoryRecord, SortDirection, SortField, StateCategory,
    Ticket, TicketKey, path_leaf,
};
use crate::pointer::{
    EditableField, OverlayAnchor, PointerLayer, PointerTarget, ScrollMetrics, ScrollState,
    ScrollSurface, SelectableSnapshot, SelectableSurface, ThumbGeometry, region,
};
use crate::search::QueryHighlighter;
use crate::sprint::{SummaryRow, SummaryRowKind};
use crate::timestamp::Timestamp;

mod details;
mod overlays;
mod pickers;
mod table;
#[cfg(test)]
mod tests;
mod widgets;

use details::{assigned_to_me_style, field_line, render_details};
use overlays::{
    ListOverlay, link_line, overlay_line, render_chips, render_column_overlay, render_facet_bar,
    render_facet_menu, render_filter_overlay, render_help_popup, render_info_overlay,
    render_list_overlay, render_palette, render_sort_popup, render_sprint_overlay,
    render_views_overlay, terminate_underline,
};
use pickers::{
    render_assignee_picker, render_delete_confirm, render_edit_menu, render_form,
    render_node_picker, render_parent_picker, render_priority_picker, render_prompt,
    render_state_picker, render_type_picker,
};
use table::{
    RowTone, child_progress_line, highlight_line, highlight_searchable, priority_style,
    render_table, search_match_style, state_category_style, state_color, state_style,
    tag_badge_spans, type_badge_spans,
};
use widgets::{
    capture_selectable, paint_hover, paint_selection, register_close_button, render_control,
    render_modal_frame, render_query_field, render_scrollbar,
};

const WIDE_BREAKPOINT: u16 = 110;

const NARROW_BREAKPOINT: u16 = 70;

/// The narrowest a dropdown gets, however short its entries are, and the
/// fewest rows it is worth opening in: two for the frame and one for a row.
const ANCHORED_MIN_WIDTH: u16 = 24;

const ANCHORED_MIN_HEIGHT: u16 = 3;

/// What the sprint summary needs around its widest line: the cursor marker,
/// the two borders, the scrollbar column, and a space to breathe.
const SPRINT_OVERLAY_CHROME: usize = 6;

/// Wide enough for the title bar however small the grid is, narrow enough to
/// leave the table either side of it, and never taller than a short terminal.
const SPRINT_OVERLAY_MIN_WIDTH: u16 = 42;

const SPRINT_OVERLAY_MAX_WIDTH: u16 = 72;

const SPRINT_OVERLAY_MAX_HEIGHT: u16 = 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Theme {
    accent: Color,
    muted: Color,
    text: Color,
    body: Color,
    link: Color,
    selected_background: Color,
    /// A dimmer wash than `selected_background`, laid under a hovered row so
    /// its colour-coded cells keep their own foregrounds.
    hover_background: Color,
    info: Color,
    /// What the Changed column paints work nobody has touched in weeks. It is
    /// deliberately not one of the state colours: staleness is a fact about
    /// the clock, not about where the work item sits in the workflow.
    warning: Color,
    error: Color,
    scrollbar: Color,
    search_match: Color,
    state_proposed: Color,
    state_in_progress: Color,
    state_resolved: Color,
    state_completed: Color,
    state_removed: Color,
    type_epic: Color,
    type_feature: Color,
    type_story: Color,
    type_task: Color,
    type_bug: Color,
    type_test: Color,
    priority_critical: Color,
    priority_high: Color,
    priority_normal: Color,
    /// Restrained badge colours a tag is hashed into, so one tag always reads
    /// the same wherever it appears.
    tag_palette: [Color; 6],
}

impl Theme {
    const fn new(monochrome: bool) -> Self {
        if monochrome {
            Self {
                accent: Color::Reset,
                muted: Color::Reset,
                text: Color::Reset,
                body: Color::Reset,
                link: Color::Reset,
                selected_background: Color::Reset,
                hover_background: Color::Reset,
                info: Color::Reset,
                warning: Color::Reset,
                error: Color::Reset,
                scrollbar: Color::Reset,
                search_match: Color::Reset,
                state_proposed: Color::Reset,
                state_in_progress: Color::Reset,
                state_resolved: Color::Reset,
                state_completed: Color::Reset,
                state_removed: Color::Reset,
                type_epic: Color::Reset,
                type_feature: Color::Reset,
                type_story: Color::Reset,
                type_task: Color::Reset,
                type_bug: Color::Reset,
                type_test: Color::Reset,
                priority_critical: Color::Reset,
                priority_high: Color::Reset,
                priority_normal: Color::Reset,
                tag_palette: [Color::Reset; 6],
            }
        } else {
            Self {
                accent: Color::Cyan,
                muted: Color::DarkGray,
                text: Color::White,
                body: Color::Gray,
                link: Color::Blue,
                selected_background: Color::DarkGray,
                hover_background: Color::Indexed(237),
                info: Color::Yellow,
                warning: Color::Yellow,
                error: Color::Red,
                scrollbar: Color::DarkGray,
                search_match: Color::Yellow,
                state_proposed: Color::Blue,
                state_in_progress: Color::Yellow,
                state_resolved: Color::Magenta,
                state_completed: Color::Green,
                state_removed: Color::DarkGray,
                type_epic: Color::Yellow,
                type_feature: Color::Magenta,
                type_story: Color::Blue,
                type_task: Color::Cyan,
                type_bug: Color::Red,
                type_test: Color::Green,
                priority_critical: Color::Red,
                priority_high: Color::Yellow,
                priority_normal: Color::Blue,
                tag_palette: [
                    Color::Cyan,
                    Color::Blue,
                    Color::Magenta,
                    Color::Green,
                    Color::Yellow,
                    Color::White,
                ],
            }
        }
    }
}

fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| Theme::new(std::env::var_os("NO_COLOR").is_some()))
}

/// Paints the frame: the shell hands the active screen the whole area.
pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    let (shell, screen) = app.screen();
    screen.render(frame, shell, area);
}

/// One screen, painted into the area the shell left it, then the hover and
/// selection paint that go over everything.
pub(crate) fn render_screen(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    render_pass(frame, screen, shell, area);
    if shell.refresh_hover() {
        render_pass(frame, screen, shell, area);
    }
    paint_hover(frame, shell);
    paint_selection(frame, shell);
}

fn render_pass(frame: &mut Frame<'_>, screen: &mut WorkItemsScreen, shell: &mut Shell, area: Rect) {
    shell.hit_regions = HitRegions::default();
    if area.width < 36 || area.height < 10 {
        frame.render_widget(
            Paragraph::new("Terminal too small\nResize to at least 36 × 10")
                .alignment(Alignment::Center)
                .block(Block::bordered().title("ticket-tui")),
            area,
        );
        return;
    }

    let chip_height =
        u16::from(screen.finished_hidden() || !screen.overflow_filter_tokens().is_empty());
    let sections = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(chip_height),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, screen, shell, sections[0]);
    render_facet_bar(frame, screen, shell, sections[1]);
    if chip_height > 0 {
        render_chips(frame, screen, shell, sections[2]);
    }
    render_content(frame, screen, shell, sections[3]);
    render_footer(frame, screen, shell, sections[4]);

    // A dropdown is dismissed by clicking away from it, so everything outside
    // it becomes one target that closes it. The overlay's own regions are
    // pushed after this one and on the same layer, so they still win.
    if anchored_overlay(screen, shell) {
        shell.hit_regions.push(region(
            area,
            PointerTarget::DismissOverlay,
            PointerLayer::Modal,
            None,
            None,
        ));
    }
    match screen.mode {
        WorkItemMode::Sort => render_sort_popup(frame, screen, shell),
        WorkItemMode::Help => render_help_popup(frame, screen, shell),
        WorkItemMode::Filter => render_filter_overlay(frame, screen, shell),
        WorkItemMode::Columns => render_column_overlay(frame, screen, shell),
        WorkItemMode::Palette => render_palette(frame, screen, shell),
        WorkItemMode::Views => render_views_overlay(frame, screen, shell),
        WorkItemMode::Info => render_info_overlay(frame, screen, shell),
        WorkItemMode::Sprint => render_sprint_overlay(frame, screen, shell),
        WorkItemMode::Facets => render_facet_menu(frame, screen, shell),
        WorkItemMode::Edit => render_edit_menu(frame, screen, shell),
        WorkItemMode::StatePicker => render_state_picker(frame, screen, shell),
        WorkItemMode::PriorityPicker => render_priority_picker(frame, screen, shell),
        WorkItemMode::Prompt => render_prompt(frame, screen, shell),
        WorkItemMode::AssigneePicker => render_assignee_picker(frame, screen, shell),
        WorkItemMode::ParentPicker => render_parent_picker(frame, screen, shell),
        WorkItemMode::NodePicker => render_node_picker(frame, screen, shell),
        WorkItemMode::Form => render_form(frame, screen, shell),
        WorkItemMode::TypePicker => render_type_picker(frame, screen, shell),
        WorkItemMode::ConfirmDelete => render_delete_confirm(frame, screen, shell),
        WorkItemMode::Browse | WorkItemMode::Search => {}
    }
}

fn render_search(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let active = screen.mode == WorkItemMode::Search;
    let title = if screen.search_pending {
        " Search (matching…) "
    } else {
        " Search / "
    };
    let mut block = focused_block(title, active);
    let actions_width = 11;
    let help_width = 4;
    let mut right_title = String::new();
    if area.width >= 48 {
        right_title.push_str("[Actions] ");
    }
    if area.width >= 36 {
        right_title.push_str("[?]");
    }
    if !right_title.is_empty() {
        block = block.title(Line::from(right_title.clone()).right_aligned());
    }
    let inner = block.inner(area);
    let clear = if !screen.query().is_empty() && inner.width > 4 {
        3
    } else {
        0
    };
    let field = Rect::new(
        inner.x,
        inner.y,
        inner.width.saturating_sub(clear),
        inner.height.max(1),
    );
    let text = if screen.query().is_empty() && !active {
        Line::styled(
            "Type / to search, or pick State, Type, Tags, or Assignee below",
            Style::default().fg(theme().muted),
        )
    } else {
        Line::from(screen.query())
    };
    let cursor_offset = u16::try_from(screen.query_cursor()).unwrap_or(u16::MAX);
    let horizontal_scroll = cursor_offset.saturating_sub(field.width.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(text)
            .block(block)
            .scroll((0, horizontal_scroll)),
        area,
    );
    if clear > 0 {
        let clear_area = Rect::new(
            inner.x.saturating_add(inner.width.saturating_sub(3)),
            inner.y,
            3,
            1,
        );
        render_control(
            frame,
            shell,
            clear_area,
            "[×]",
            PointerTarget::ClearQuery,
            PointerLayer::Base,
            true,
        );
    }
    shell.hit_regions.push(region(
        field,
        PointerTarget::SearchField,
        PointerLayer::Base,
        Some(SelectableSurface::Search),
        None,
    ));
    if area.width >= 48 {
        let actions = Rect::new(
            area.x
                .saturating_add(area.width.saturating_sub(actions_width + help_width)),
            area.y,
            actions_width.saturating_sub(1),
            1,
        );
        shell.hit_regions.push(region(
            actions,
            PointerTarget::OpenPalette,
            PointerLayer::Base,
            None,
            None,
        ));
    }
    if area.width >= 36 {
        let help = Rect::new(
            area.x.saturating_add(area.width.saturating_sub(5)),
            area.y,
            3,
            1,
        );
        shell.hit_regions.push(region(
            help,
            PointerTarget::OpenHelp,
            PointerLayer::Base,
            None,
            None,
        ));
    }
    capture_selectable(frame, shell, SelectableSurface::Search, field, false);

    if active {
        let cursor_x = field
            .x
            .saturating_add(cursor_offset.saturating_sub(horizontal_scroll));
        frame.set_cursor_position((
            cursor_x.min(field.x.saturating_add(field.width.saturating_sub(1))),
            field.y,
        ));
    }
}

fn render_content(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    if area.width >= WIDE_BREAKPOINT {
        shell.set_content_layout(area, Some(DividerOrientation::Vertical));
        let panes = Layout::horizontal([
            Constraint::Percentage(shell.pane_split_wide),
            Constraint::Fill(1),
        ])
        .spacing(1)
        .split(area);
        render_table(frame, screen, shell, panes[0]);
        render_details(frame, screen, shell, panes[1]);
        render_divider(
            frame,
            shell,
            panes[0],
            panes[1],
            DividerOrientation::Vertical,
        );
    } else if area.width >= NARROW_BREAKPOINT {
        shell.set_content_layout(area, Some(DividerOrientation::Horizontal));
        let panes = Layout::vertical([
            Constraint::Percentage(shell.pane_split_stacked),
            Constraint::Fill(1),
        ])
        .spacing(1)
        .split(area);
        render_table(frame, screen, shell, panes[0]);
        render_details(frame, screen, shell, panes[1]);
        render_divider(
            frame,
            shell,
            panes[0],
            panes[1],
            DividerOrientation::Horizontal,
        );
    } else {
        shell.set_content_layout(area, None);
        if shell.narrow_details {
            render_details(frame, screen, shell, area);
        } else {
            render_table(frame, screen, shell, area);
        }
    }
}

/// Paints the gap the layout leaves between the panes and registers it as the
/// draggable divider. Hovering reverses it through the usual hover pass.
fn render_divider(
    frame: &mut Frame<'_>,
    shell: &mut Shell,
    first: Rect,
    second: Rect,
    orientation: DividerOrientation,
) {
    let Some(rect) = divider_rect(first, second, orientation) else {
        return;
    };
    let glyph = match orientation {
        DividerOrientation::Vertical => "\u{2502}",
        DividerOrientation::Horizontal => "\u{2500}",
    };
    let style = Style::default().fg(theme().muted);
    let row = glyph.repeat(usize::from(rect.width));
    let lines: Vec<Line<'_>> = (0..rect.height)
        .map(|_| Line::styled(row.clone(), style))
        .collect();
    frame.render_widget(Paragraph::new(Text::from(lines)), rect);
    shell.hit_regions.push(region(
        rect,
        PointerTarget::PaneDivider,
        PointerLayer::Base,
        None,
        None,
    ));
}

/// The cells the layout left between two panes, if any.
fn divider_rect(first: Rect, second: Rect, orientation: DividerOrientation) -> Option<Rect> {
    let rect = match orientation {
        DividerOrientation::Vertical => Rect {
            x: first.right(),
            y: first.y,
            width: second.x.checked_sub(first.right())?,
            height: first.height,
        },
        DividerOrientation::Horizontal => Rect {
            x: first.x,
            y: first.bottom(),
            width: first.width,
            height: second.y.checked_sub(first.bottom())?,
        },
    };
    (rect.width > 0 && rect.height > 0).then_some(rect)
}

fn render_footer(frame: &mut Frame<'_>, screen: &WorkItemsScreen, shell: &Shell, area: Rect) {
    let (text, style) = if let Some((message, level)) = shell.notification() {
        let color = match level {
            NotificationLevel::Info => theme().info,
            NotificationLevel::Error => theme().error,
        };
        (message, Style::default().fg(color))
    } else {
        let text = screen.footer_hint(shell);
        (text, Style::default().fg(theme().muted))
    };
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(style),
        area,
    );
}

fn register_narrow_tabs(shell: &mut Shell, area: Rect) {
    let tickets = Rect::new(area.x.saturating_add(1), area.y, 9, 1);
    let details = Rect::new(area.x.saturating_add(11), area.y, 9, 1);
    shell.hit_regions.push(region(
        tickets,
        PointerTarget::NarrowTickets,
        PointerLayer::Base,
        None,
        None,
    ));
    shell.hit_regions.push(region(
        details,
        PointerTarget::NarrowDetails,
        PointerLayer::Base,
        None,
        None,
    ));
}

fn current_layer(screen: &WorkItemsScreen) -> PointerLayer {
    match screen.mode {
        WorkItemMode::Facets => PointerLayer::Popup,
        WorkItemMode::Browse | WorkItemMode::Search => PointerLayer::Base,
        _ => PointerLayer::Modal,
    }
}

fn focused_block<'a>(title: impl Into<Line<'a>>, focused: bool) -> Block<'a> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            theme().accent
        } else {
            theme().muted
        }))
}

/// Whether the overlay on screen is a dropdown hung off a details-pane field
/// rather than a centred modal.
fn anchored_overlay(screen: &WorkItemsScreen, shell: &Shell) -> bool {
    shell.overlay_anchor.is_anchored()
        && matches!(
            screen.mode,
            WorkItemMode::StatePicker
                | WorkItemMode::PriorityPicker
                | WorkItemMode::AssigneePicker
                | WorkItemMode::NodePicker
                | WorkItemMode::Prompt
        )
}

/// Where an overlay of this size lands.
///
/// A centred one is placed the way it always was. A dropdown opens directly
/// under the field that was clicked, left edge on the value, taking as many of
/// the rows below as it needs; with too few rows under the field it opens above
/// it instead, and with too few either way it falls back to the middle of the
/// screen.
fn overlay_area(area: Rect, anchor: OverlayAnchor, width: u16, height: u16) -> Rect {
    let (field, prefer_above) = match anchor {
        OverlayAnchor::Centered => return centered_rect(area, width, height),
        OverlayAnchor::Below(field) => (field, false),
        OverlayAnchor::Above(field) => (field, true),
    };
    let width = width.min(area.width).max(1);
    let x = field
        .x
        .min(area.x.saturating_add(area.width).saturating_sub(width))
        .max(area.x);
    let top = field.y.saturating_add(field.height);
    let below = area
        .y
        .saturating_add(area.height)
        .saturating_sub(top.max(area.y));
    let above = field.y.saturating_sub(area.y);
    let drop_below = || Rect::new(x, top, width, height.min(below));
    let drop_above = || {
        let height = height.min(above);
        Rect::new(x, field.y.saturating_sub(height), width, height)
    };
    let (first, second) = if prefer_above {
        (above, below)
    } else {
        (below, above)
    };
    if first >= ANCHORED_MIN_HEIGHT {
        if prefer_above {
            drop_above()
        } else {
            drop_below()
        }
    } else if second >= ANCHORED_MIN_HEIGHT {
        if prefer_above {
            drop_below()
        } else {
            drop_above()
        }
    } else {
        centered_rect(area, width, height)
    }
}

/// How wide an overlay is drawn: the width it uses when centred, or, as a
/// dropdown, whatever its longest row needs, never under
/// [`ANCHORED_MIN_WIDTH`] and never wider than the screen.
fn overlay_width(anchor: OverlayAnchor, rows: &[Line<'_>], centered: u16, area: Rect) -> u16 {
    if !anchor.is_anchored() {
        return centered;
    }
    let longest = rows.iter().map(Line::width).max().unwrap_or_default();
    // Two columns of frame, one for the scrollbar, and one to breathe.
    let fitted = u16::try_from(longest.saturating_add(4)).unwrap_or(u16::MAX);
    fitted.clamp(ANCHORED_MIN_WIDTH.min(area.width), area.width)
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height - height) / 2),
            Constraint::Length(height),
            Constraint::Fill(1),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width - width) / 2),
            Constraint::Length(width),
            Constraint::Fill(1),
        ])
        .split(vertical[1])[1]
}
