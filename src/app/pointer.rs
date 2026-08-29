//! Hover, press, drag and release: what the mouse does to the app.

use super::*;

/// Which way the draggable pane divider runs in the current layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DividerOrientation {
    /// A column between the tickets and details panes (wide layout).
    Vertical,
    /// A row between the stacked tickets and details panes.
    Horizontal,
}

#[derive(Debug)]
pub struct PointerUpdate {
    pub action: AppAction,
    pub redraw: bool,
}

impl PointerUpdate {
    fn none(redraw: bool) -> Self {
        Self {
            action: AppAction::None,
            redraw,
        }
    }

    fn action(action: AppAction) -> Self {
        Self {
            action,
            redraw: true,
        }
    }
}

impl App {
    pub fn set_table_viewport(&mut self, rows: usize) {
        self.table.set_viewport(rows, self.visible.len());
    }

    /// The scroll bookkeeping for one surface. The table measures its content from
    /// the visible rows, so that length is refreshed on the way out.
    #[must_use]
    pub fn scroll_state(&self, surface: ScrollSurface) -> ScrollState {
        match surface {
            ScrollSurface::Table => ScrollState {
                content: self.visible.len(),
                ..self.table
            },
            ScrollSurface::Details => self.details,
            ScrollSurface::Help => self.help,
            ScrollSurface::Sort => self.sort,
            ScrollSurface::Filter => self.filter_overlay.scroll,
            ScrollSurface::Columns => self.column_overlay.scroll,
            ScrollSurface::Palette => self.palette.scroll,
            ScrollSurface::Views => self.views_overlay.scroll,
            ScrollSurface::Sprint => self.sprint_overlay.scroll,
            ScrollSurface::FacetMenu => self.facet_bar.scroll,
            ScrollSurface::EditMenu => self.edit_menu.scroll,
            ScrollSurface::StatePicker => self.state_picker.scroll,
            ScrollSurface::PriorityPicker => self.priority_picker.scroll,
            ScrollSurface::AssigneePicker => self.assignee_picker.scroll,
            ScrollSurface::ParentPicker => self.parent_picker.scroll,
            ScrollSurface::NodePicker => self.node_picker.scroll,
            ScrollSurface::TypePicker => self.type_picker.scroll,
            ScrollSurface::Form => self.form_scroll,
        }
    }

    pub fn scroll_state_mut(&mut self, surface: ScrollSurface) -> &mut ScrollState {
        if matches!(surface, ScrollSurface::Table) {
            self.table.content = self.visible.len();
        }
        match surface {
            ScrollSurface::Table => &mut self.table,
            ScrollSurface::Details => &mut self.details,
            ScrollSurface::Help => &mut self.help,
            ScrollSurface::Sort => &mut self.sort,
            ScrollSurface::Filter => &mut self.filter_overlay.scroll,
            ScrollSurface::Columns => &mut self.column_overlay.scroll,
            ScrollSurface::Palette => &mut self.palette.scroll,
            ScrollSurface::Views => &mut self.views_overlay.scroll,
            ScrollSurface::Sprint => &mut self.sprint_overlay.scroll,
            ScrollSurface::FacetMenu => &mut self.facet_bar.scroll,
            ScrollSurface::EditMenu => &mut self.edit_menu.scroll,
            ScrollSurface::StatePicker => &mut self.state_picker.scroll,
            ScrollSurface::PriorityPicker => &mut self.priority_picker.scroll,
            ScrollSurface::AssigneePicker => &mut self.assignee_picker.scroll,
            ScrollSurface::ParentPicker => &mut self.parent_picker.scroll,
            ScrollSurface::NodePicker => &mut self.node_picker.scroll,
            ScrollSurface::TypePicker => &mut self.type_picker.scroll,
            ScrollSurface::Form => &mut self.form_scroll,
        }
    }

    #[must_use]
    pub fn hovered(&self) -> Option<&PointerTarget> {
        self.pointer.hover.as_ref()
    }

    pub(crate) fn hovered_region(&self) -> Option<&crate::pointer::PointerRegion> {
        let (column, row) = self.pointer.position()?;
        self.hit_regions.resolve(column, row)
    }

    #[must_use]
    pub fn selection(&self) -> Option<TextSelection> {
        self.pointer.selection
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> PointerUpdate {
        self.pointer.set_position(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp => self.handle_wheel(mouse.column, mouse.row, -3),
            MouseEventKind::ScrollDown => self.handle_wheel(mouse.column, mouse.row, 3),
            MouseEventKind::Down(MouseButton::Left) => self.handle_press(mouse.column, mouse.row),
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved
                if self.pointer.is_pressed() =>
            {
                self.handle_drag(mouse.column, mouse.row)
            }
            MouseEventKind::Moved => self.handle_hover(mouse.column, mouse.row),
            MouseEventKind::Up(MouseButton::Left) => self.handle_release(mouse.column, mouse.row),
            _ => PointerUpdate::none(false),
        }
    }

    fn handle_hover(&mut self, column: u16, row: u16) -> PointerUpdate {
        self.pointer.set_position(column, row);
        PointerUpdate::none(self.refresh_hover())
    }

    pub fn refresh_hover(&mut self) -> bool {
        let hover = self
            .pointer
            .position()
            .and_then(|(column, row)| self.hit_regions.resolve(column, row))
            .map(|region| region.target.clone());
        let changed = hover != self.pointer.hover;
        self.pointer.hover = hover;
        changed
    }

    fn handle_press(&mut self, column: u16, row: u16) -> PointerUpdate {
        let region = self.hit_regions.resolve(column, row).cloned();
        let selectable = self.hit_regions.resolve_selectable(column, row);
        self.pointer.clear_selection();
        if let Some(region) = region {
            let scrollbar = match region.target {
                PointerTarget::ScrollbarThumb { surface } => Some(surface),
                _ => None,
            };
            let selectable = match region.target {
                // Neither drags text: one resizes the panes, and the other is
                // the empty space around a dropdown.
                PointerTarget::PaneDivider | PointerTarget::DismissOverlay => None,
                _ => selectable,
            };
            self.pointer.hover = Some(region.target.clone());
            self.pointer
                .begin_press(region.target, column, row, selectable, scrollbar);
        } else {
            self.pointer.hover = None;
            self.pointer.clear_press();
        }
        PointerUpdate::none(true)
    }

    fn handle_drag(&mut self, column: u16, row: u16) -> PointerUpdate {
        let hover = self
            .hit_regions
            .resolve(column, row)
            .map(|region| region.target.clone());
        let hover_changed = hover != self.pointer.hover;
        self.pointer.hover = hover;
        if !self.pointer.moved_from_origin(column, row)
            && matches!(self.pointer.drag(), DragKind::None)
        {
            return PointerUpdate::none(hover_changed);
        }
        match self.pointer.drag() {
            DragKind::Scrollbar { surface, grab } => {
                self.drag_scrollbar(surface, row, grab);
                PointerUpdate::none(true)
            }
            DragKind::Text => {
                self.update_text_drag(column, row);
                PointerUpdate::none(true)
            }
            DragKind::Divider => {
                self.drag_divider(column, row);
                PointerUpdate::none(true)
            }
            DragKind::Cancelled => PointerUpdate::none(hover_changed),
            DragKind::None => {
                if matches!(
                    self.pointer.press_target(),
                    Some(PointerTarget::PaneDivider)
                ) {
                    self.pointer.set_drag(DragKind::Divider);
                    self.drag_divider(column, row);
                    PointerUpdate::none(true)
                } else if let Some(surface) = self.pointer.press_scrollbar() {
                    let grab = self.scrollbar_grab(surface, self.pointer.press_origin());
                    self.pointer.set_drag(DragKind::Scrollbar { surface, grab });
                    self.drag_scrollbar(surface, row, grab);
                    PointerUpdate::none(true)
                } else if let Some(surface) = self.pointer.press_selectable() {
                    self.pointer.set_drag(DragKind::Text);
                    if let Some(origin) = self.pointer.press_origin()
                        && let Some(snapshot) = self.hit_regions.selectable(surface)
                        && let Some(start) = snapshot.pos_at(origin.0, origin.1)
                    {
                        self.pointer.selection = Some(TextSelection {
                            surface,
                            start,
                            end: start,
                        });
                    }
                    self.update_text_drag(column, row);
                    PointerUpdate::none(true)
                } else {
                    self.pointer.set_drag(DragKind::Cancelled);
                    PointerUpdate::none(hover_changed)
                }
            }
        }
    }

    fn handle_release(&mut self, column: u16, row: u16) -> PointerUpdate {
        let drag = self.pointer.drag();
        let target = self.pointer.press_target().cloned();
        let selection = self.pointer.selection;
        self.pointer.clear_press();
        self.handle_hover(column, row);
        match drag {
            DragKind::Text => {
                if let Some(selection) = selection.filter(|selection| !selection.is_empty())
                    && let Some(snapshot) = self.hit_regions.selectable(selection.surface)
                {
                    let text = crate::pointer::extract_selected_text(snapshot, &selection);
                    if !text.is_empty() {
                        return PointerUpdate::action(AppAction::Copy {
                            text,
                            content: CopiedContent::Text,
                        });
                    }
                }
                PointerUpdate::none(true)
            }
            DragKind::Divider => {
                self.session_dirty = true;
                PointerUpdate::none(true)
            }
            DragKind::Scrollbar { .. } | DragKind::Cancelled => PointerUpdate::none(true),
            DragKind::None => {
                if let Some(target) = target {
                    PointerUpdate::action(self.activate_target(target, column, row))
                } else {
                    PointerUpdate::none(true)
                }
            }
        }
    }

    fn handle_wheel(&mut self, column: u16, row: u16, delta: i32) -> PointerUpdate {
        let hover_changed = self.refresh_hover();
        let Some(surface) = self.hit_regions.resolve_scroll(column, row) else {
            return PointerUpdate::none(hover_changed);
        };
        let changed = self.scroll_surface(surface, delta);
        PointerUpdate::none(changed || hover_changed)
    }

    pub(super) fn activate_target(
        &mut self,
        target: PointerTarget,
        column: u16,
        row: u16,
    ) -> AppAction {
        match target {
            PointerTarget::SearchField => {
                self.begin_search();
                self.place_caret(TextEditor::Search, column, row);
            }
            PointerTarget::ClearQuery => self.set_query(String::new()),
            PointerTarget::OpenPalette => return self.run_command(CommandId::Palette),
            PointerTarget::OpenHelp => return self.run_command(CommandId::Help),
            PointerTarget::CopyActions => self.open_copy_actions(),
            PointerTarget::CloseOverlay => self.close_overlay(),
            PointerTarget::NarrowTickets => {
                self.narrow_details = false;
                self.focus = Focus::Tickets;
            }
            PointerTarget::NarrowDetails => {
                self.narrow_details = true;
                if !self.focus.is_details_pane() {
                    self.focus = Focus::Details;
                }
            }
            PointerTarget::FocusTickets => {
                self.focus = Focus::Tickets;
                self.narrow_details = false;
            }
            PointerTarget::FocusDetails => {
                self.focus = Focus::Details;
            }
            PointerTarget::TableRow { index } => {
                self.focus = Focus::Tickets;
                self.narrow_details = false;
                if index < self.visible.len() {
                    self.select_row(index);
                    self.record_history();
                }
            }
            PointerTarget::OpenTicket { index } => {
                self.focus = Focus::Tickets;
                self.narrow_details = false;
                if index < self.visible.len() {
                    self.select_row(index);
                    self.record_history();
                    return self.open_selected();
                }
            }
            PointerTarget::ToggleBookmark { index } => {
                if index < self.visible.len() {
                    self.select_row(index);
                    self.toggle_bookmark();
                }
            }
            PointerTarget::ToggleRowSelect { index } => {
                if index < self.visible.len() {
                    self.select_row(index);
                    self.toggle_row_selection();
                }
            }
            PointerTarget::SortHeader(field) => self.toggle_sort(field),
            PointerTarget::OpenSelectedUrl => {
                self.focus = Focus::Details;
                self.narrow_details = true;
                return self.open_selected();
            }
            PointerTarget::JumpToTicket(key) => {
                if self
                    .visible_family_tree()
                    .iter()
                    .any(|entry| entry.key == key)
                {
                    self.focus = Focus::Family;
                    self.family_cursor = Some(key.clone());
                    self.ensure_family_cursor_visible();
                } else if self
                    .selected_family()
                    .is_some_and(|family| family.extra_parents.iter().any(|parent| parent == &key))
                {
                    self.focus = Focus::Family;
                } else {
                    self.focus = Focus::Details;
                }
                self.jump_to_ticket(&key);
            }
            PointerTarget::FacetPill(target) => match target {
                FacetTarget::More => self.open_filters(),
                FacetTarget::Field(field) => {
                    let index = FilterField::BAR
                        .iter()
                        .position(|entry| *entry == field)
                        .unwrap_or_default();
                    self.open_facets(index);
                }
            },
            PointerTarget::FacetValue { index } => {
                self.facet_bar.value_index = index;
                self.toggle_current_bar_facet();
            }
            PointerTarget::DismissFacet => {
                if self.mode == AppMode::Facets {
                    self.mode = AppMode::Browse;
                }
            }
            PointerTarget::RemoveChip(token) => self.remove_filter_token(token),
            PointerTarget::ShowFinished => self.set_show_finished(true),
            PointerTarget::SortChoose(field) => {
                self.toggle_sort(field);
                self.mode = AppMode::Browse;
            }
            PointerTarget::SortSetDirection(direction) => {
                self.sort_draft.direction = direction;
            }
            PointerTarget::FilterRow { index } => {
                if self.filter_overlay.showing_values {
                    self.filter_overlay.value_index = index;
                    self.toggle_current_facet();
                } else {
                    self.filter_overlay.field_index = index;
                    self.filter_overlay.showing_values = true;
                    self.filter_overlay.value_index = 0;
                    self.filter_overlay.scroll.scroll_to(0);
                }
            }
            PointerTarget::ColumnToggle { index } => {
                self.column_overlay.index = index;
                self.layout.toggle_visible(index);
                self.session_dirty = true;
            }
            PointerTarget::ColumnMove { index, delta } => {
                self.column_overlay.index = self.layout.move_column(index, delta);
                self.session_dirty = true;
            }
            PointerTarget::ColumnResize { index, delta } => {
                self.column_overlay.index = index;
                self.layout.resize(index, delta);
                self.session_dirty = true;
            }
            PointerTarget::PaletteCommand { index } => {
                self.palette.selected = index;
                return self.run_selected_command();
            }
            PointerTarget::PaletteQuery => {
                self.place_caret(TextEditor::Palette, column, row);
            }
            PointerTarget::EditMenuRow { index } => {
                self.edit_menu.index = index;
                return self.run_edit_menu_entry(index);
            }
            PointerTarget::StateOption { index } => {
                self.state_picker.index = index;
                return self.choose_state(index);
            }
            PointerTarget::PriorityOption { index } => {
                self.priority_picker.index = index;
                return self.choose_priority(index);
            }
            PointerTarget::AssigneeOption { index } => {
                self.assignee_picker.index = index;
                return self.choose_assignee(index);
            }
            PointerTarget::AssigneeQuery => {
                self.place_caret(TextEditor::Assignee, column, row);
            }
            PointerTarget::ParentOption { index } => {
                self.parent_picker.index = index;
                return self.choose_parent(index);
            }
            PointerTarget::ParentQuery => {
                self.place_caret(TextEditor::Parent, column, row);
            }
            PointerTarget::NodeOption { index } => {
                self.node_picker.index = index;
                return self.choose_node(index);
            }
            PointerTarget::NodeQuery => {
                self.place_caret(TextEditor::Node, column, row);
            }
            PointerTarget::FormField { index } => {
                self.focus_form_field(index);
                self.place_caret(TextEditor::Form, column, row);
            }
            PointerTarget::SubmitForm => return self.submit_form(),
            PointerTarget::CancelForm => self.cancel_form(),
            PointerTarget::ConfirmDelete => return self.confirm_delete(),
            PointerTarget::CancelDelete => self.cancel_delete(),
            PointerTarget::TypeOption { index } => {
                self.type_picker.index = index;
                self.choose_work_item_type(index);
            }
            PointerTarget::EditField { field } => return self.open_field_editor(field),
            PointerTarget::DismissOverlay => self.close_overlay(),
            PointerTarget::PromptInput => {
                self.place_caret(TextEditor::Prompt, column, row);
            }
            PointerTarget::SubmitPrompt => return self.submit_prompt(),
            PointerTarget::CancelPrompt => self.close_prompt(),
            PointerTarget::ViewRow { index } => {
                if self
                    .view_rows()
                    .get(index)
                    .is_some_and(|row| !row.is_heading())
                {
                    self.views_overlay.index = index;
                    self.apply_view_at(index);
                }
            }
            PointerTarget::SummaryRow { index } => {
                if self
                    .summary_rows()
                    .get(index)
                    .is_some_and(SummaryRow::is_selectable)
                {
                    self.sprint_overlay.index = index;
                    self.apply_summary_row(index);
                }
            }
            PointerTarget::SaveView => {
                if self.views_overlay.naming.is_some() {
                    if let Some(name) = self
                        .views_overlay
                        .naming
                        .take()
                        .map(|name| name.text().trim().to_owned())
                        .filter(|name| !name.is_empty())
                    {
                        self.save_view(name);
                    }
                } else {
                    self.views_overlay.naming =
                        Some(TextInput::new(self.active_view.clone().unwrap_or_default()));
                }
            }
            PointerTarget::DeleteView => self.delete_view_at(self.views_overlay.index),
            PointerTarget::ViewName => {
                self.place_caret(TextEditor::ViewName, column, row);
            }
            PointerTarget::CancelNaming => self.views_overlay.naming = None,
            PointerTarget::OverlayBody => {}
            PointerTarget::ScrollbarTrack { surface, page_down } => {
                let step =
                    i32::try_from(self.scroll_state(surface).page_step()).unwrap_or(i32::MAX);
                self.scroll_surface(surface, if page_down { step } else { -step });
            }
            PointerTarget::ScrollbarThumb { .. } => {}
            PointerTarget::PaneDivider => {}
        }
        AppAction::None
    }

    /// The details-pane field the pointer is resting on, which is what `Enter`
    /// opens an editor for while that pane is focused.
    #[must_use]
    pub(super) fn pointed_edit_field(&self) -> Option<EditableField> {
        match self.hovered_region().map(|region| &region.target) {
            Some(PointerTarget::EditField { field }) => Some(*field),
            _ => None,
        }
    }

    /// Opens the editor one details-pane field owns, as a dropdown hung under
    /// the value on screen. It runs the same command the Edit menu and the
    /// palette run, so both paths open the same picker and write the same
    /// edit; only where the overlay lands differs.
    pub(super) fn open_field_editor(&mut self, field: EditableField) -> AppAction {
        let anchor = self
            .hit_regions
            .edit_field(field)
            .map_or(OverlayAnchor::Centered, OverlayAnchor::Below);
        let action = self.run_command(command_for_field(field));
        self.overlay_anchor = anchor;
        action
    }

    fn place_caret(&mut self, editor: TextEditor, column: u16, row: u16) {
        let Some(snapshot) = self
            .hit_regions
            .selectable(match editor {
                TextEditor::Search => SelectableSurface::Search,
                TextEditor::Palette
                | TextEditor::ViewName
                | TextEditor::Prompt
                | TextEditor::Assignee
                | TextEditor::Node
                | TextEditor::Parent
                | TextEditor::Form => SelectableSurface::Overlay,
            })
            .and_then(|snapshot| snapshot.pos_at(column, row))
            .or_else(|| {
                self.hit_regions.resolve(column, row).map(|region| TextPos {
                    line: 0,
                    col: usize::from(column.saturating_sub(region.rect.x)),
                })
            })
        else {
            return;
        };
        let index = snapshot.col;
        match editor {
            TextEditor::Search => self.query.set_cursor(index),
            TextEditor::Palette => self.palette.query.set_cursor(index),
            TextEditor::ViewName => {
                if let Some(name) = self.views_overlay.naming.as_mut() {
                    name.set_cursor(index);
                }
            }
            TextEditor::Prompt => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.input.set_cursor(index);
                }
            }
            TextEditor::Assignee => self.assignee_picker.query.set_cursor(index),
            TextEditor::Parent => self.parent_picker.query.set_cursor(index),
            TextEditor::Node => self.node_picker.query.set_cursor(index),
            TextEditor::Form => {
                if let Some(field) = self.focused_form_field_mut() {
                    field.input.set_cursor(index);
                }
            }
        }
    }

    fn update_text_drag(&mut self, column: u16, row: u16) {
        let Some(surface) = self
            .pointer
            .selection
            .map(|selection| selection.surface)
            .or_else(|| self.pointer.press_selectable())
        else {
            return;
        };
        let Some(snapshot) = self.hit_regions.selectable(surface) else {
            return;
        };
        let Some(end) = snapshot
            .pos_at(column, row)
            .or_else(|| clamp_pos_to_snapshot(snapshot, column, row))
        else {
            return;
        };
        if let Some(selection) = self.pointer.selection.as_mut() {
            selection.end = end;
        } else if let Some(origin) = self.pointer.press_origin()
            && let Some(start) = snapshot.pos_at(origin.0, origin.1)
        {
            self.pointer.selection = Some(TextSelection {
                surface,
                start,
                end,
            });
        }
    }

    fn scrollbar_grab(&self, surface: ScrollSurface, origin: Option<(u16, u16)>) -> i16 {
        let Some((_, row)) = origin else {
            return 0;
        };
        let Some(metrics) = self.hit_regions.scroll(surface) else {
            return 0;
        };
        let Some(thumb) = metrics.thumb() else {
            return 0;
        };
        i16::try_from(row).unwrap_or(0)
            - i16::try_from(metrics.track.y.saturating_add(thumb.y)).unwrap_or(0)
    }

    fn drag_scrollbar(&mut self, surface: ScrollSurface, row: u16, grab: i16) {
        let Some(metrics) = self.hit_regions.scroll(surface) else {
            return;
        };
        let Some(thumb) = metrics.thumb() else {
            return;
        };
        let pointer = i32::from(row) - i32::from(grab);
        let track_y = i32::from(metrics.track.y);
        let rel = pointer.saturating_sub(track_y).max(0) as usize;
        let offset = crate::pointer::offset_from_thumb(
            rel.min(thumb.travel),
            thumb.travel,
            thumb.max_offset,
        );
        self.scroll_state_mut(surface).scroll_to(offset);
    }

    fn scroll_surface(&mut self, surface: ScrollSurface, delta: i32) -> bool {
        self.scroll_state_mut(surface).scroll_by(delta)
    }

    /// Records the workspace the panes were last split inside, and which way the
    /// divider runs there. The narrow layout passes `None`: it has no divider.
    pub const fn set_content_layout(&mut self, area: Rect, divider: Option<DividerOrientation>) {
        self.content_area = area;
        self.divider = divider;
    }

    #[must_use]
    pub const fn content_area(&self) -> Rect {
        self.content_area
    }

    #[must_use]
    pub const fn divider_orientation(&self) -> Option<DividerOrientation> {
        self.divider
    }

    /// Moves the divider under the pointer: the tickets pane keeps everything up
    /// to the pointer, the details pane the rest.
    fn drag_divider(&mut self, column: u16, row: u16) {
        match self.divider {
            Some(DividerOrientation::Vertical) => {
                let span = self.content_area.width;
                let cells = column.saturating_sub(self.content_area.x);
                self.pane_split_wide =
                    split_percent(cells, span, MIN_TICKETS_COLUMNS, MIN_DETAILS_COLUMNS);
            }
            Some(DividerOrientation::Horizontal) => {
                let span = self.content_area.height;
                let cells = row.saturating_sub(self.content_area.y);
                self.pane_split_stacked = split_percent(cells, span, MIN_PANE_ROWS, MIN_PANE_ROWS);
            }
            None => {}
        }
    }

    /// Restores the built-in split for both layouts.
    pub(super) fn reset_pane_split(&mut self) {
        self.pane_split_wide = DEFAULT_PANE_SPLIT_WIDE;
        self.pane_split_stacked = DEFAULT_PANE_SPLIT_STACKED;
        self.session_dirty = true;
        self.set_status("Reset pane split");
    }
}
