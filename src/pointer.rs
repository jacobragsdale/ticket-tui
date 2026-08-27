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
    Prompt,
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
    FacetMenu,
    Sort,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextEditor {
    Search,
    Palette,
    Prompt,
    ViewName,
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
    OpenTicket {
        index: usize,
    },
    ToggleBookmark {
        index: usize,
    },
    ToggleRowSelect {
        index: usize,
    },
    SortHeader(SortField),
    OpenSelectedUrl,
    JumpToTicket(TicketKey),
    ToggleFamily(TicketKey),
    FacetPill(FacetTarget),
    FacetValue {
        index: usize,
    },
    DismissFacet,
    RemoveChip(FilterToken),
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
    SaveView,
    DeleteView,
    ViewName,
    CancelNaming,
    ImportSubmit,
    ImportCancel,
    PromptField,
    OverlayBody,
    ScrollbarTrack {
        surface: ScrollSurface,
        page_down: bool,
    },
    ScrollbarThumb {
        surface: ScrollSurface,
    },
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

#[derive(Clone, Debug, Default)]
pub struct HitRegions {
    regions: Vec<PointerRegion>,
    selectable: Vec<SelectableSnapshot>,
    scroll: Vec<(ScrollSurface, ScrollMetrics)>,
    pub search: Option<Rect>,
    pub table_body: Option<Rect>,
    pub id_column: Option<Rect>,
    pub details: Option<Rect>,
    pub detail_url: Option<Rect>,
    pub headers: Vec<(Rect, SortField)>,
    pub detail_links: Vec<(Rect, TicketKey)>,
    pub facet_pills: Vec<(Rect, FacetTarget)>,
}

impl HitRegions {
    pub fn push(&mut self, region: PointerRegion) {
        match &region.target {
            PointerTarget::SearchField => self.search = Some(region.rect),
            PointerTarget::FocusTickets => self.table_body = Some(region.rect),
            PointerTarget::FocusDetails => self.details = Some(region.rect),
            PointerTarget::OpenSelectedUrl => self.detail_url = Some(region.rect),
            PointerTarget::SortHeader(field) => self.headers.push((region.rect, *field)),
            PointerTarget::JumpToTicket(key) => self.detail_links.push((region.rect, key.clone())),
            PointerTarget::FacetPill(target) => self.facet_pills.push((region.rect, *target)),
            _ => {}
        }
        self.regions.push(region);
    }

    pub fn set_id_column(&mut self, rect: Rect) {
        self.id_column = Some(rect);
    }

    pub fn set_details(&mut self, rect: Rect) {
        self.details = Some(rect);
    }

    pub fn set_table_body(&mut self, rect: Rect) {
        self.table_body = Some(rect);
    }

    pub fn set_search(&mut self, rect: Rect) {
        self.search = Some(rect);
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

    #[must_use]
    pub fn regions(&self) -> &[PointerRegion] {
        &self.regions
    }

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

    pub fn reset_interaction(&mut self) {
        self.clear_press();
        self.clear_selection();
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

#[must_use]
pub fn clamp_offset(offset: usize, content: usize, viewport: usize) -> usize {
    offset.min(content.saturating_sub(viewport))
}

#[must_use]
pub fn scroll_by(offset: usize, delta: i32, content: usize, viewport: usize) -> usize {
    let next = if delta < 0 {
        offset.saturating_sub(delta.unsigned_abs() as usize)
    } else {
        offset.saturating_add(delta as usize)
    };
    clamp_offset(next, content, viewport)
}

#[must_use]
pub fn page_step(viewport: usize) -> usize {
    viewport.saturating_sub(1).max(1)
}

#[must_use]
pub fn ensure_index_visible(offset: usize, viewport: usize, index: usize) -> usize {
    if viewport == 0 {
        return offset;
    }
    if index < offset {
        index
    } else if index >= offset.saturating_add(viewport) {
        index.saturating_add(1).saturating_sub(viewport)
    } else {
        offset
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
        assert!(hits.resolve(20, 20).is_none());
    }

    #[test]
    fn borders_are_outside_inner_hit_rects() {
        let mut hits = HitRegions::default();
        hits.push(region(
            rect(1, 1, 8, 5),
            PointerTarget::SearchField,
            PointerLayer::Base,
            Some(SelectableSurface::Search),
            None,
        ));
        assert!(hits.resolve(1, 1).is_some());
        assert!(hits.resolve(8, 5).is_some());
        assert!(hits.resolve(0, 1).is_none());
        assert!(hits.resolve(9, 1).is_none());
        assert!(hits.resolve(1, 0).is_none());
        assert!(hits.resolve(1, 6).is_none());
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
    fn scroll_helpers_clamp_and_keep_the_focused_index_visible() {
        assert_eq!(scroll_by(0, -3, 20, 5), 0);
        assert_eq!(scroll_by(18, 3, 20, 5), 15);
        assert_eq!(scroll_by(4, 3, 20, 5), 7);
        assert_eq!(page_step(10), 9);
        assert_eq!(ensure_index_visible(5, 4, 3), 3);
        assert_eq!(ensure_index_visible(5, 4, 10), 7);
        assert_eq!(ensure_index_visible(5, 4, 6), 5);
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
