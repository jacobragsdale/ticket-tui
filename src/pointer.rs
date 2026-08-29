use ratatui::layout::Rect;

use crate::filter::{FacetTarget, FilterToken};
use crate::model::{SortDirection, SortField, TicketKey};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PointerLayer {
    Base,
    Modal,
    Popup,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SelectableSurface {
    Search,
    Table,
    Details,
    Help,
    Overlay,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ScrollSurface {
    Table,
    Details,
    Help,
    Filter,
    Columns,
    Palette,
    Views,
    /// The grid and tallies of the sprint summary overlay.
    Sprint,
    FacetMenu,
    Sort,
    EditMenu,
    StatePicker,
    PriorityPicker,
    AssigneePicker,
    NodePicker,
    TypePicker,
    ParentPicker,
    /// The field list of an open form.
    Form,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextEditor {
    Search,
    Palette,
    ViewName,
    /// The single-line field editor the Edit menu opens for a title or a tag
    /// list.
    Prompt,
    /// The assignee picker's filter field.
    Assignee,
    /// The parent picker's filter field.
    Parent,
    /// The iteration or area picker's filter field.
    Node,
    /// The focused text field of an open form.
    Form,
}

/// One value on the details pane that can be edited by clicking it. Each names
/// the editor it opens, which is the same one the Edit menu and the command
/// palette reach.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EditableField {
    Title,
    State,
    Assignee,
    Priority,
    Tags,
    Iteration,
    Area,
}

/// Where an overlay is placed. Every keyboard-opened picker is `Centered`, the
/// way it always was; a picker opened by clicking a field hangs off that
/// field's value instead, and the rect it carries is where that value was
/// drawn.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OverlayAnchor {
    #[default]
    Centered,
    /// A dropdown directly under the field.
    Below(Rect),
    /// A dropdown directly over the field, for one with no room underneath.
    Above(Rect),
}

impl OverlayAnchor {
    /// Whether this overlay is a dropdown hung off a field rather than a
    /// centred modal, which is also what puts a dismiss layer behind it.
    #[must_use]
    pub const fn is_anchored(self) -> bool {
        !matches!(self, Self::Centered)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PointerTarget {
    SearchField,
    ClearQuery,
    OpenPalette,
    OpenHelp,
    CopyActions,
    CloseOverlay,
    NarrowTickets,
    NarrowDetails,
    FocusTickets,
    FocusDetails,
    TableRow {
        index: usize,
    },
    OpenInBrowser {
        index: usize,
    },
    ToggleBookmark {
        index: usize,
    },
    ToggleRowSelect {
        index: usize,
    },
    /// A column header, carrying the column's key rather than a work-item
    /// sort field: every screen's table sorts through the same target and
    /// resolves the key against its own columns.
    SortHeader(&'static str),
    OpenSelectedUrl,
    JumpToTicket(TicketKey),
    FacetPill(FacetTarget),
    FacetValue {
        index: usize,
    },
    DismissFacet,
    RemoveChip(FilterToken),
    /// The `×` on the chip saying finished work is being left out.
    ShowFinished,
    SortChoose(SortField),
    SortSetDirection(SortDirection),
    FilterRow {
        index: usize,
    },
    ColumnToggle {
        index: usize,
    },
    ColumnMove {
        index: usize,
        delta: isize,
    },
    ColumnResize {
        index: usize,
        delta: i16,
    },
    PaletteCommand {
        index: usize,
    },
    PaletteQuery,
    ViewRow {
        index: usize,
    },
    /// One row of the sprint summary grid, which clicking filters the table
    /// down to.
    SummaryRow {
        index: usize,
    },
    /// One field editor in the Edit menu.
    EditMenuRow {
        index: usize,
    },
    /// One state in the state picker.
    StateOption {
        index: usize,
    },
    /// One priority in the priority picker, `Clear` included.
    PriorityOption {
        index: usize,
    },
    /// One person in the assignee picker, `Unassigned` included.
    AssigneeOption {
        index: usize,
    },
    /// The filter field of the assignee picker.
    AssigneeQuery,
    /// One node of the iteration or area picker.
    NodeOption {
        index: usize,
    },
    /// The filter field of the iteration or area picker.
    NodeQuery,
    /// One work item in the parent picker.
    ParentOption {
        index: usize,
    },
    /// The filter field of the parent picker.
    ParentQuery,
    /// One field of an open form, which focusing is what clicking it does.
    FormField {
        index: usize,
    },
    /// One work item type in the type picker.
    TypeOption {
        index: usize,
    },
    /// A form's `[Create]` and `[Cancel]` buttons.
    SubmitForm,
    CancelForm,
    /// The delete confirmation's `[Delete]` and `[Cancel]` buttons.
    ConfirmDelete,
    CancelDelete,
    /// One editable value on the details pane, which opens its editor as a
    /// dropdown anchored under it.
    EditField {
        field: EditableField,
    },
    /// Everything outside an anchored dropdown, which closes it without a
    /// change rather than activating whatever sits underneath.
    DismissOverlay,
    /// The text field of the title or tags prompt.
    PromptInput,
    SubmitPrompt,
    CancelPrompt,
    SaveView,
    DeleteView,
    ViewName,
    CancelNaming,
    OverlayBody,
    ScrollbarTrack {
        surface: ScrollSurface,
        page_down: bool,
    },
    ScrollbarThumb {
        surface: ScrollSurface,
    },
    PaneDivider,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PointerRegion {
    pub rect: Rect,
    pub target: PointerTarget,
    pub layer: PointerLayer,
    pub selectable: Option<SelectableSurface>,
    pub scroll: Option<ScrollSurface>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextPos {
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextSelection {
    pub surface: SelectableSurface,
    pub start: TextPos,
    pub end: TextPos,
}

impl TextSelection {
    #[must_use]
    pub fn ordered(self) -> (TextPos, TextPos) {
        if (self.start.line, self.start.col) <= (self.end.line, self.end.col) {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectableSnapshot {
    pub surface: SelectableSurface,
    pub rect: Rect,
    pub cells: Vec<Vec<String>>,
}

impl SelectableSnapshot {
    #[must_use]
    pub fn pos_at(&self, column: u16, row: u16) -> Option<TextPos> {
        if !contains(self.rect, column, row) || self.cells.is_empty() {
            return None;
        }
        let line = usize::from(row.saturating_sub(self.rect.y)).min(self.cells.len() - 1);
        let width = self.cells[line].len();
        let col = usize::from(column.saturating_sub(self.rect.x)).min(width);
        Some(TextPos { line, col })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScrollMetrics {
    pub offset: usize,
    pub content: usize,
    pub viewport: usize,
    pub track: Rect,
}

impl ScrollMetrics {
    #[must_use]
    pub fn max_offset(self) -> usize {
        self.content.saturating_sub(self.viewport)
    }

    #[must_use]
    pub fn thumb(self) -> Option<ThumbGeometry> {
        thumb_geometry(self.content, self.viewport, self.offset, self.track.height)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThumbGeometry {
    pub y: u16,
    pub height: u16,
    pub travel: usize,
    pub max_offset: usize,
}

/// Everything the last frame painted that the pointer can land on: the click
/// targets in paint order, the text each selectable surface showed, and the
/// scrollbar geometry of each surface that overflowed. Rebuilt every frame.
#[derive(Clone, Debug, Default)]
pub struct HitRegions {
    regions: Vec<PointerRegion>,
    selectable: Vec<SelectableSnapshot>,
    scroll: Vec<(ScrollSurface, ScrollMetrics)>,
}

impl HitRegions {
    pub fn push(&mut self, region: PointerRegion) {
        self.regions.push(region);
    }

    pub fn set_scroll(&mut self, surface: ScrollSurface, metrics: ScrollMetrics) {
        if let Some(existing) = self.scroll.iter_mut().find(|(entry, _)| *entry == surface) {
            existing.1 = metrics;
        } else {
            self.scroll.push((surface, metrics));
        }
    }

    pub fn scroll(&self, surface: ScrollSurface) -> Option<ScrollMetrics> {
        self.scroll
            .iter()
            .find(|(entry, _)| *entry == surface)
            .map(|(_, metrics)| *metrics)
    }

    pub fn add_selectable(&mut self, snapshot: SelectableSnapshot) {
        self.selectable
            .retain(|existing| existing.surface != snapshot.surface);
        self.selectable.push(snapshot);
    }

    #[must_use]
    pub fn selectable(&self, surface: SelectableSurface) -> Option<&SelectableSnapshot> {
        self.selectable
            .iter()
            .find(|snapshot| snapshot.surface == surface)
    }

    /// Where one editable details-pane value was drawn, if it was drawn at
    /// all, which is what an editor opened by clicking it is anchored to.
    #[must_use]
    pub fn edit_field(&self, field: EditableField) -> Option<Rect> {
        self.find_target(
            |target| matches!(target, PointerTarget::EditField { field: drawn } if *drawn == field),
        )
        .map(|region| region.rect)
    }

    /// Where one facet pill was drawn, if it was drawn at all, which is what
    /// its dropdown hangs under.
    #[must_use]
    pub fn facet_pill(&self, wanted: FacetTarget) -> Option<Rect> {
        self.find_target(
            |target| matches!(target, PointerTarget::FacetPill(pill) if *pill == wanted),
        )
        .map(|region| region.rect)
    }

    #[must_use]
    pub fn resolve(&self, column: u16, row: u16) -> Option<&PointerRegion> {
        self.regions
            .iter()
            .enumerate()
            .filter(|(_, region)| contains(region.rect, column, row))
            .max_by_key(|(index, region)| (region.layer, *index))
            .map(|(_, region)| region)
    }

    #[must_use]
    pub fn resolve_scroll(&self, column: u16, row: u16) -> Option<ScrollSurface> {
        self.regions
            .iter()
            .enumerate()
            .filter(|(_, region)| contains(region.rect, column, row) && region.scroll.is_some())
            .max_by_key(|(index, region)| (region.layer, *index))
            .and_then(|(_, region)| region.scroll)
    }

    #[must_use]
    pub fn resolve_selectable(&self, column: u16, row: u16) -> Option<SelectableSurface> {
        self.regions
            .iter()
            .enumerate()
            .filter(|(_, region)| contains(region.rect, column, row) && region.selectable.is_some())
            .max_by_key(|(index, region)| (region.layer, *index))
            .and_then(|(_, region)| region.selectable)
    }

    /// The last region painted for a target the predicate names, which is the
    /// one on top wherever two overlap.
    #[must_use]
    pub fn find_target(
        &self,
        predicate: impl Fn(&PointerTarget) -> bool,
    ) -> Option<&PointerRegion> {
        self.regions
            .iter()
            .rev()
            .find(|region| predicate(&region.target))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DragKind {
    #[default]
    None,
    Cancelled,
    Text,
    Scrollbar {
        surface: ScrollSurface,
        grab: i16,
    },
    Divider,
}

#[derive(Clone, Debug)]
struct Press {
    target: PointerTarget,
    column: u16,
    row: u16,
    selectable: Option<SelectableSurface>,
    scrollbar: Option<ScrollSurface>,
}

#[derive(Clone, Debug, Default)]
pub struct PointerState {
    pub hover: Option<PointerTarget>,
    position: Option<(u16, u16)>,
    press: Option<Press>,
    drag: DragKind,
    pub selection: Option<TextSelection>,
}

impl PointerState {
    pub fn set_position(&mut self, column: u16, row: u16) {
        self.position = Some((column, row));
    }

    #[must_use]
    pub fn position(&self) -> Option<(u16, u16)> {
        self.position
    }

    pub fn begin_press(
        &mut self,
        target: PointerTarget,
        column: u16,
        row: u16,
        selectable: Option<SelectableSurface>,
        scrollbar: Option<ScrollSurface>,
    ) {
        self.press = Some(Press {
            target,
            column,
            row,
            selectable,
            scrollbar,
        });
        self.drag = DragKind::None;
    }

    pub fn clear_press(&mut self) {
        self.press = None;
        self.drag = DragKind::None;
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    #[must_use]
    pub fn is_pressed(&self) -> bool {
        self.press.is_some()
    }

    #[must_use]
    pub fn drag(&self) -> DragKind {
        self.drag
    }

    pub fn set_drag(&mut self, drag: DragKind) {
        self.drag = drag;
    }

    #[must_use]
    pub fn press_target(&self) -> Option<&PointerTarget> {
        self.press.as_ref().map(|press| &press.target)
    }

    #[must_use]
    pub fn press_origin(&self) -> Option<(u16, u16)> {
        self.press.as_ref().map(|press| (press.column, press.row))
    }

    #[must_use]
    pub fn press_selectable(&self) -> Option<SelectableSurface> {
        self.press.as_ref().and_then(|press| press.selectable)
    }

    #[must_use]
    pub fn press_scrollbar(&self) -> Option<ScrollSurface> {
        self.press.as_ref().and_then(|press| press.scrollbar)
    }

    #[must_use]
    pub fn moved_from_origin(&self, column: u16, row: u16) -> bool {
        self.press
            .as_ref()
            .is_some_and(|press| press.column != column || press.row != row)
    }
}

#[must_use]
pub fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

/// Scroll bookkeeping for one surface: where it is scrolled to, how much content it
/// holds, and how much of that content fits on screen.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScrollState {
    pub offset: usize,
    pub content: usize,
    pub viewport: usize,
}

impl ScrollState {
    #[must_use]
    pub const fn max_offset(self) -> usize {
        self.content.saturating_sub(self.viewport)
    }

    /// Records the rendered geometry and re-clamps the offset to the new maximum.
    pub const fn set_viewport(&mut self, viewport: usize, content: usize) {
        self.viewport = viewport;
        self.content = content;
        self.clamp();
    }

    pub const fn scroll_to(&mut self, offset: usize) {
        self.offset = offset;
        self.clamp();
    }

    /// Scrolls by `delta` rows, clamped to the content, and reports whether the
    /// offset moved.
    pub const fn scroll_by(&mut self, delta: i32) -> bool {
        let before = self.offset;
        self.offset = if delta < 0 {
            self.offset.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            self.offset.saturating_add(delta as usize)
        };
        self.clamp();
        self.offset != before
    }

    /// Scrolls the smallest amount that brings `index` inside the viewport.
    pub const fn ensure_visible(&mut self, index: usize) {
        let viewport = if self.viewport == 0 { 1 } else { self.viewport };
        if index < self.offset {
            self.offset = index;
        } else if index >= self.offset.saturating_add(viewport) {
            self.offset = index.saturating_add(1).saturating_sub(viewport);
        }
    }

    /// One screenful minus a row of overlap, as used by PageUp/PageDown and by
    /// clicks on the scrollbar track.
    #[must_use]
    pub const fn page_step(self) -> usize {
        let step = self.viewport.saturating_sub(1);
        if step == 0 { 1 } else { step }
    }

    const fn clamp(&mut self) {
        let maximum = self.max_offset();
        if self.offset > maximum {
            self.offset = maximum;
        }
    }
}

#[must_use]
pub fn thumb_geometry(
    content: usize,
    viewport: usize,
    offset: usize,
    track_height: u16,
) -> Option<ThumbGeometry> {
    if content <= viewport || track_height == 0 {
        return None;
    }
    let track = usize::from(track_height);
    let thumb_len = ((track * viewport) / content).max(1).min(track);
    let max_offset = content - viewport;
    let travel = track.saturating_sub(thumb_len);
    let thumb_pos = if travel == 0 || max_offset == 0 {
        0
    } else {
        (offset * travel + max_offset / 2) / max_offset
    };
    Some(ThumbGeometry {
        y: u16::try_from(thumb_pos).unwrap_or(u16::MAX),
        height: u16::try_from(thumb_len).unwrap_or(1).max(1),
        travel,
        max_offset,
    })
}

#[must_use]
pub fn offset_from_thumb(thumb_pos: usize, travel: usize, max_offset: usize) -> usize {
    (thumb_pos * max_offset + travel / 2)
        .checked_div(travel)
        .unwrap_or(0)
}

#[must_use]
pub fn extract_selected_text(snapshot: &SelectableSnapshot, selection: &TextSelection) -> String {
    if snapshot.surface != selection.surface || selection.is_empty() {
        return String::new();
    }
    let (start, end) = selection.ordered();
    let last_line = end.line.min(snapshot.cells.len().saturating_sub(1));
    let mut lines = Vec::new();
    for (row, cells) in snapshot.cells.iter().enumerate() {
        if row < start.line || row > last_line {
            continue;
        }
        let from = if row == start.line { start.col } else { 0 };
        let to = if row == end.line {
            end.col.max(from)
        } else {
            cells.len()
        };
        let mut text = String::new();
        for cell in cells.iter().take(to.min(cells.len())).skip(from) {
            text.push_str(cell);
        }
        lines.push(text.trim_end().to_string());
    }
    lines.join("\n")
}

#[must_use]
pub fn region(
    rect: Rect,
    target: PointerTarget,
    layer: PointerLayer,
    selectable: Option<SelectableSurface>,
    scroll: Option<ScrollSurface>,
) -> PointerRegion {
    PointerRegion {
        rect,
        target,
        layer,
        selectable,
        scroll,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn resolve_prefers_the_topmost_layer_then_reverse_paint_order() {
        let mut hits = HitRegions::default();
        hits.push(region(
            rect(0, 0, 10, 10),
            PointerTarget::FocusTickets,
            PointerLayer::Base,
            None,
            Some(ScrollSurface::Table),
        ));
        hits.push(region(
            rect(2, 2, 4, 4),
            PointerTarget::CloseOverlay,
            PointerLayer::Modal,
            None,
            None,
        ));
        hits.push(region(
            rect(2, 2, 1, 1),
            PointerTarget::OpenHelp,
            PointerLayer::Modal,
            None,
            None,
        ));

        assert!(matches!(
            hits.resolve(2, 2).map(|region| &region.target),
            Some(PointerTarget::OpenHelp)
        ));
        assert!(matches!(
            hits.resolve(3, 3).map(|region| &region.target),
            Some(PointerTarget::CloseOverlay)
        ));
        assert!(matches!(
            hits.resolve(0, 0).map(|region| &region.target),
            Some(PointerTarget::FocusTickets)
        ));
        assert!(
            hits.resolve(9, 9).is_some(),
            "the far corner is still inside the rect"
        );
        assert!(hits.resolve(10, 9).is_none(), "a rect excludes its border");
        assert!(hits.resolve(9, 10).is_none(), "a rect excludes its border");
        assert!(hits.resolve(20, 20).is_none());
    }

    #[test]
    fn clipped_rows_do_not_receive_hits() {
        let mut hits = HitRegions::default();
        hits.push(region(
            rect(0, 2, 10, 1),
            PointerTarget::FilterRow { index: 2 },
            PointerLayer::Modal,
            Some(SelectableSurface::Overlay),
            Some(ScrollSurface::Filter),
        ));
        assert!(matches!(
            hits.resolve(3, 2).map(|region| &region.target),
            Some(PointerTarget::FilterRow { index: 2 })
        ));
        assert!(hits.resolve(3, 1).is_none());
        assert!(hits.resolve(3, 3).is_none());
    }

    #[test]
    fn scroll_state_clamps_when_the_viewport_is_measured() {
        let mut scroll = ScrollState::default();
        scroll.set_viewport(5, 20);
        scroll.scroll_to(usize::MAX);
        assert_eq!(scroll.offset, 15);
        assert_eq!(scroll.max_offset(), 15);

        scroll.set_viewport(12, 20);
        assert_eq!(scroll.offset, 8, "a taller viewport pulls the offset back");

        scroll.set_viewport(12, 4);
        assert_eq!(
            scroll.offset, 0,
            "content shorter than the viewport cannot scroll"
        );
        assert_eq!(scroll.max_offset(), 0);
    }

    #[test]
    fn scroll_state_pages_and_keeps_the_focused_index_visible() {
        let mut scroll = ScrollState {
            offset: 0,
            content: 20,
            viewport: 5,
        };
        assert_eq!(scroll.page_step(), 4);
        assert!(!scroll.scroll_by(-3), "already at the top");
        assert!(scroll.scroll_by(4));
        assert_eq!(scroll.offset, 4);
        assert!(scroll.scroll_by(30));
        assert_eq!(scroll.offset, 15, "scrolling stops at the last screenful");
        assert!(!scroll.scroll_by(1));

        scroll.offset = 5;
        scroll.viewport = 4;
        scroll.ensure_visible(3);
        assert_eq!(
            scroll.offset, 3,
            "an index above the window scrolls up to it"
        );
        scroll.offset = 5;
        scroll.ensure_visible(10);
        assert_eq!(
            scroll.offset, 7,
            "an index below the window scrolls just far enough"
        );
        scroll.offset = 5;
        scroll.ensure_visible(6);
        assert_eq!(
            scroll.offset, 5,
            "an index already on screen does not scroll"
        );

        let mut unmeasured = ScrollState::default();
        unmeasured.ensure_visible(4);
        assert_eq!(unmeasured.offset, 4);
        assert_eq!(unmeasured.page_step(), 1);
    }

    #[test]
    fn thumb_geometry_maps_offset_proportionally() {
        let thumb = thumb_geometry(100, 10, 0, 20).unwrap();
        assert_eq!(thumb.y, 0);
        assert_eq!(thumb.height, 2);
        let thumb = thumb_geometry(100, 10, 90, 20).unwrap();
        assert_eq!(thumb.y, 18);
        assert!(thumb_geometry(10, 10, 0, 20).is_none());
        assert_eq!(offset_from_thumb(9, 18, 90), 45);
    }

    #[test]
    fn selected_text_joins_visible_rows_and_trims_layout_padding() {
        let snapshot = SelectableSnapshot {
            surface: SelectableSurface::Details,
            rect: rect(0, 0, 8, 2),
            cells: vec![
                vec!["A".into(), "B".into(), "C".into(), " ".into()],
                vec!["D".into(), "E".into(), "F".into(), " ".into()],
            ],
        };
        let selection = TextSelection {
            surface: SelectableSurface::Details,
            start: TextPos { line: 0, col: 1 },
            end: TextPos { line: 1, col: 2 },
        };
        assert_eq!(extract_selected_text(&snapshot, &selection), "BC\nDE");
        let empty = TextSelection {
            surface: SelectableSurface::Details,
            start: TextPos { line: 0, col: 1 },
            end: TextPos { line: 0, col: 1 },
        };
        assert!(extract_selected_text(&snapshot, &empty).is_empty());
    }
}
