use std::cmp::Ordering;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Position, Rect, Spacing};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::merge::MergeStrategy;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, HighlightSpacing, Padding, Paragraph, Row, Table, Wrap,
};
use time::OffsetDateTime;

use crate::app::{
    App, ChildProgress, DividerOrientation, Focus, FormOverlay, HitRegions, NotificationLevel,
    PRIORITY_CHOICES, PROGRESS_BAR_CELLS, PaneSeam, PaneSplit, PromptField, RowDensity, Screen,
    SearchOrder, Shell, SyncStatus, TabId, UNASSIGNED_LABEL, WorkItemMode, WorkItemsScreen,
};
use crate::command::{COMMANDS, Command, Scope, key_label_for};
use crate::filter::{FacetTarget, FilterField, WorkItemSchema};
use crate::model::{
    ArtifactKind, ArtifactLink, FamilySnapshot, FamilyTreeEntry, HistoryRecord, Jump,
    SortDirection, SortField, StateCategory, Ticket, TicketKey, path_leaf,
};
use crate::pointer::{
    EditableField, OverlayAnchor, PointerLayer, PointerTarget, ScrollMetrics, ScrollState,
    ScrollSurface, SelectableSnapshot, SelectableSurface, ThumbGeometry, region,
};
use crate::search::QueryHighlighter;
use crate::sprint::{SummaryRow, SummaryRowKind};
use crate::timestamp::Timestamp;

pub(crate) mod acr;
pub(crate) mod aks;
mod details;
pub(crate) mod key_vault;
mod overlays;
mod panes;
mod pickers;
pub(crate) mod pipelines;
pub(crate) mod pull_requests;
pub(crate) mod repos;
mod table;
#[cfg(test)]
mod tests;
pub mod theme;
mod widgets;

use details::{assigned_to_me_style, field_label, field_line, render_details, state_glyph};
use overlays::{
    ListOverlay, bar_fields, column_rows, link_line, overlay_line, overlay_row, overlay_row_width,
    pill_style, render_chips, render_column_overlay, render_facet_bar, render_facet_menu,
    render_filter_overlay, render_help_popup, render_info_overlay, render_list_overlay,
    render_palette, render_sort_popup, render_sprint_overlay, render_views_overlay,
    terminate_underline,
};
use panes::{PaneNames, PanePair, render_inner_split, render_workspace};
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
pub use theme::{Theme, ThemeChoice, chosen_theme, set_theme, theme};
use widgets::{
    CLOSE_LABEL, Control, ControlKind, SearchRow, button_row, capture_selectable, dim_behind,
    follow_chip, paint_hover, paint_selection, register_buttons, register_close_button,
    render_control, render_modal_frame, render_query_field, render_screen_status_bar,
    render_scrollbar, render_search_row, row_on_screen, spinner_frame, wrapped_rows,
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

/// Paints the frame: the tab bar, then the active screen under it. Painted
/// twice when the pointer has moved onto something, because what is under it
/// is only known once the regions are registered.
pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    paint(frame, app);
    if app.shell.refresh_hover() {
        paint(frame, app);
    }
    paint_hover(frame, &app.shell);
    paint_selection(frame, &app.shell);
}

fn paint(frame: &mut Frame<'_>, app: &mut App) {
    app.shell.hit_regions = HitRegions::default();
    app.shell.clear_seams();
    let area = frame.area();
    if area.width < 36 || area.height < 11 {
        frame.render_widget(
            Paragraph::new("Terminal too small\nResize to at least 36 × 11")
                .alignment(Alignment::Center)
                .block(Block::bordered().title("ticket-tui")),
            area,
        );
        return;
    }
    let sections = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).split(area);
    let tabs = app.tabs();
    render_tab_bar(frame, &mut app.shell, &tabs, sections[0]);
    let (shell, screen) = app.screen();
    screen.render(frame, shell, sections[1]);
    if app.shell_overlay_open() {
        render_shell_overlay(frame, app);
    }
}

/// One of the shared overlays, drawn over a tab other than the work items by
/// the work items screen that drives it. The columns editor shows the columns
/// of the tab it was opened for.
fn render_shell_overlay(frame: &mut Frame<'_>, app: &mut App) {
    let columns = match app.tab {
        TabId::WorkItems => Vec::new(),
        TabId::Repos => column_rows(&app.repos.layout),
        TabId::PullRequests => column_rows(&app.pull_requests.layout),
        TabId::Pipelines => column_rows(Screen::columns(&app.pipelines)),
        TabId::Aks => column_rows(Screen::columns(&app.aks)),
        TabId::Acr => column_rows(Screen::columns(&app.acr)),
        TabId::KeyVault => column_rows(Screen::columns(&app.key_vault)),
    };
    let App {
        shell, work_items, ..
    } = app;
    match work_items.mode {
        WorkItemMode::Help => render_help_popup(frame, work_items, shell),
        WorkItemMode::Palette => render_palette(frame, work_items, shell),
        WorkItemMode::Info => render_info_overlay(frame, work_items, shell),
        WorkItemMode::Columns => render_column_overlay(frame, work_items, shell, &columns),
        _ => {}
    }
}

/// The one row above everything: which tabs there are, which one is showing,
/// and what each has waiting.
pub(crate) fn render_tab_bar(
    frame: &mut Frame<'_>,
    shell: &mut Shell,
    tabs: &[(TabId, bool, Option<String>)],
    area: Rect,
) {
    // Every tab stays on the bar and stays clickable however narrow the
    // terminal is: the names shorten first, and then go altogether.
    // The name and the badge are painted apart \u{2014} the badge is what is waiting
    // on that tab, and it reads in the warning colour wherever the tab sits.
    let written = |tab: TabId, badge: Option<&String>, style: NameStyle| {
        let name = match style {
            NameStyle::Full => tab.label(),
            NameStyle::Short => tab.short_label(),
            NameStyle::Number => "",
        };
        let head = if name.is_empty() {
            format!(" {} ", tab.number())
        } else {
            format!(" {} {name} ", tab.number())
        };
        let badge = badge.map_or_else(String::new, |badge| format!("{badge} "));
        (head, badge)
    };
    let style = [NameStyle::Full, NameStyle::Short, NameStyle::Number]
        .into_iter()
        .find(|style| {
            let width: usize = tabs
                .iter()
                .map(|(tab, _, badge)| {
                    let (head, badge) = written(*tab, badge.as_ref(), *style);
                    head.chars().count() + badge.chars().count()
                })
                .sum();
            width <= usize::from(area.width)
        })
        .unwrap_or(NameStyle::Number);

    let mut spans = Vec::new();
    let mut x = area.x;
    for (tab, active, badge) in tabs {
        let (tab, active) = (*tab, *active);
        let (head, badge) = written(tab, badge.as_ref(), style);
        let width = u16::try_from(head.chars().count() + badge.chars().count()).unwrap_or(u16::MAX);
        if x.saturating_add(width) > area.x.saturating_add(area.width) {
            break;
        }
        let style = if active {
            tab_chip_style()
        } else {
            Style::default().fg(theme().muted)
        };
        spans.push(Span::styled(head, style));
        if !badge.is_empty() {
            spans.push(Span::styled(
                badge,
                style.fg(theme().warning).add_modifier(Modifier::BOLD),
            ));
        }
        shell.hit_regions.push(region(
            Rect::new(x, area.y, width, 1),
            PointerTarget::SelectTab { index: tab.index() },
            PointerLayer::Base,
            None,
            None,
        ));
        x = x.saturating_add(width);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    render_bar_controls(frame, shell, area, x);
}

/// The tab that is showing: the accent on the surface ground, and reversed
/// where the palette has no ground to give it \u{2014} `mono`, where every colour is
/// `Reset` and weight alone would not be enough.
fn tab_chip_style() -> Style {
    let style = Style::default()
        .fg(theme().accent)
        .add_modifier(Modifier::BOLD);
    if theme().surface == Color::Reset {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style.bg(theme().surface)
    }
}

/// `Actions` and `?` at the right end of the tab bar, as far from the tabs as
/// the row is wide. They open on every tab, so they belong to the bar rather
/// than to one screen's search row. The narrower the terminal, the fewer of
/// them there is room for, and neither ever paints over a tab.
fn render_bar_controls(frame: &mut Frame<'_>, shell: &mut Shell, area: Rect, used: u16) {
    const ACTIONS: &str = " Actions ";
    const HELP: &str = " ? ";
    let mut right = area.right();
    let mut spans = Vec::new();
    for (label, target) in [
        (HELP, PointerTarget::OpenHelp),
        (ACTIONS, PointerTarget::OpenPalette),
    ] {
        let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
        let Some(x) = right.checked_sub(width).filter(|x| *x > used) else {
            break;
        };
        shell.hit_regions.push(region(
            Rect::new(x, area.y, width, 1),
            target,
            PointerLayer::Base,
            None,
            None,
        ));
        spans.insert(0, Span::styled(label, pill_style(false, false)));
        right = x;
    }
    if spans.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Right),
        Rect::new(right, area.y, area.right().saturating_sub(right), 1),
    );
}

/// How much of a tab's name the bar has room for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NameStyle {
    Full,
    Short,
    Number,
}

/// One screen, painted into the area the shell left it.
pub(crate) fn render_screen(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    render_pass(frame, screen, shell, area);
}

fn render_pass(frame: &mut Frame<'_>, screen: &mut WorkItemsScreen, shell: &mut Shell, area: Rect) {
    // Which fields are pills decides which filters are chips, so the bar is
    // measured before the rows are laid out.
    screen.facet_bar.shown = bar_fields(screen, area.width);
    let chip_height =
        u16::from(screen.finished_hidden() || !screen.overflow_filter_tokens().is_empty());
    let sections = Layout::vertical([
        Constraint::Length(1),
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
        WorkItemMode::Columns => {
            let columns = column_rows(Screen::columns(screen));
            render_column_overlay(frame, screen, shell, &columns);
        }
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
    render_search_row(
        frame,
        shell,
        SearchRow {
            area,
            text: screen.query(),
            cursor: screen.query_cursor(),
            placeholder: "Type / to search, or pick a filter from the bar below",
            active: screen.mode == WorkItemMode::Search,
            pending: screen.search_pending,
            clearable: true,
            trailer: String::new(),
            layer: PointerLayer::Base,
            selectable: SelectableSurface::Search,
        },
    );
}

/// The work items workspace: the tickets table, and the details of the ticket
/// under the cursor. Drawn through the same pane system every other tab uses.
fn render_content(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    struct Panes<'a>(&'a mut WorkItemsScreen);
    impl PanePair for Panes<'_> {
        fn first(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            render_table(frame, self.0, shell, area);
        }

        fn second(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            render_details(frame, self.0, shell, area);
        }
    }
    render_workspace(
        frame,
        shell,
        area,
        &PaneNames {
            list: "Tickets",
            details: "Details",
        },
        &mut Panes(screen),
    );
}

fn render_footer(frame: &mut Frame<'_>, screen: &WorkItemsScreen, shell: &Shell, area: Rect) {
    render_screen_status_bar(frame, screen, shell, area);
}

/// The layer a modal on the work items screen sits on: the facet menu is a
/// popup, everything else is modal.
fn modal_layer(screen: &WorkItemsScreen) -> PointerLayer {
    match screen.mode {
        WorkItemMode::Facets => PointerLayer::Popup,
        _ => PointerLayer::Modal,
    }
}

fn current_layer(screen: &WorkItemsScreen) -> PointerLayer {
    match screen.mode {
        WorkItemMode::Facets => PointerLayer::Popup,
        WorkItemMode::Browse | WorkItemMode::Search => PointerLayer::Base,
        _ => PointerLayer::Modal,
    }
}

/// The cells inside a pane's border, before any padding it carries: what the
/// pane owns, the scrollbar's column and the seam it shares with a neighbour
/// included.
fn inside_border(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

/// The frame every pane wears: the theme's corners, the accent while it has
/// focus and a weight on its name to say so, and borders that merge with a
/// neighbour's rather than sitting beside them.
///
/// The merge is `Fuzzy` rather than `Exact` because the corners are rounded:
/// there is no rounded `┬`, so an exact merge would leave `╮` where two panes meet.
/// Fuzzy falls back to the plain junction, which is the glyph a seam wants.
fn focused_block<'a>(title: impl Into<Line<'a>>, focused: bool) -> Block<'a> {
    let title = title.into();
    Block::default()
        .title(if focused {
            title.style(Style::default().add_modifier(Modifier::BOLD))
        } else {
            title
        })
        .borders(Borders::ALL)
        .border_type(theme().border_type)
        .merge_borders(MergeStrategy::Fuzzy)
        .border_style(Style::default().fg(if focused {
            theme().border_focused
        } else {
            theme().border
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

/// A modal sized as a share of the screen rather than to a fixed number of
/// cells: `percent` of the screen each way, never larger than the `content`
/// it holds needs, and never smaller than `least`, which is what it was
/// before there was room to grow.
fn ratio_rect(area: Rect, percent: (u16, u16), content: (u16, u16), least: (u16, u16)) -> Rect {
    let pick = |span: u16, percent: u16, content: u16, least: u16| {
        let share = u16::try_from(u32::from(span) * u32::from(percent) / 100).unwrap_or(span);
        share.min(content).max(least.min(span))
    };
    centered_rect(
        area,
        pick(area.width, percent.0, content.0, least.0),
        pick(area.height, percent.1, content.1, least.1),
    )
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
