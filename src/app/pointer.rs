//! Hover, press, drag and release: what the mouse does to the app.

use super::*;

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

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> PointerUpdate {
        self.shell.pointer.set_position(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp => self.handle_wheel(mouse.column, mouse.row, -3),
            MouseEventKind::ScrollDown => self.handle_wheel(mouse.column, mouse.row, 3),
            MouseEventKind::Down(MouseButton::Left) => {
                self.shell.handle_press(mouse.column, mouse.row)
            }
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved
                if self.shell.pointer.is_pressed() =>
            {
                self.handle_drag(mouse.column, mouse.row)
            }
            MouseEventKind::Moved => self.shell.handle_hover(mouse.column, mouse.row),
            MouseEventKind::Up(MouseButton::Left) => self.handle_release(mouse.column, mouse.row),
            _ => PointerUpdate::none(false),
        }
    }

    fn handle_drag(&mut self, column: u16, row: u16) -> PointerUpdate {
        let hover = self
            .shell
            .hit_regions
            .resolve(column, row)
            .map(|region| region.target.clone());
        let hover_changed = hover != self.shell.pointer.hover;
        self.shell.pointer.hover = hover;
        if !self.shell.pointer.moved_from_origin(column, row)
            && matches!(self.shell.pointer.drag(), DragKind::None)
        {
            return PointerUpdate::none(hover_changed);
        }
        match self.shell.pointer.drag() {
            DragKind::Scrollbar { surface, grab } => {
                self.drag_scrollbar(surface, row, grab);
                PointerUpdate::none(true)
            }
            DragKind::Text => {
                self.shell.update_text_drag(column, row);
                PointerUpdate::none(true)
            }
            DragKind::Divider => {
                self.shell.drag_divider(column, row);
                PointerUpdate::none(true)
            }
            DragKind::Cancelled => PointerUpdate::none(hover_changed),
            DragKind::None => {
                if matches!(
                    self.shell.pointer.press_target(),
                    Some(PointerTarget::PaneDivider)
                ) {
                    self.shell.pointer.set_drag(DragKind::Divider);
                    self.shell.drag_divider(column, row);
                    PointerUpdate::none(true)
                } else if let Some(surface) = self.shell.pointer.press_scrollbar() {
                    let grab = self
                        .shell
                        .scrollbar_grab(surface, self.shell.pointer.press_origin());
                    self.shell
                        .pointer
                        .set_drag(DragKind::Scrollbar { surface, grab });
                    self.drag_scrollbar(surface, row, grab);
                    PointerUpdate::none(true)
                } else if let Some(surface) = self.shell.pointer.press_selectable() {
                    self.shell.pointer.set_drag(DragKind::Text);
                    if let Some(origin) = self.shell.pointer.press_origin()
                        && let Some(snapshot) = self.shell.hit_regions.selectable(surface)
                        && let Some(start) = snapshot.pos_at(origin.0, origin.1)
                    {
                        self.shell.pointer.selection = Some(TextSelection {
                            surface,
                            start,
                            end: start,
                        });
                    }
                    self.shell.update_text_drag(column, row);
                    PointerUpdate::none(true)
                } else {
                    self.shell.pointer.set_drag(DragKind::Cancelled);
                    PointerUpdate::none(hover_changed)
                }
            }
        }
    }

    fn handle_release(&mut self, column: u16, row: u16) -> PointerUpdate {
        let drag = self.shell.pointer.drag();
        let target = self.shell.pointer.press_target().cloned();
        let selection = self.shell.pointer.selection;
        self.shell.pointer.clear_press();
        self.shell.handle_hover(column, row);
        match drag {
            DragKind::Text => {
                if let Some(selection) = selection.filter(|selection| !selection.is_empty())
                    && let Some(snapshot) = self.shell.hit_regions.selectable(selection.surface)
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
                self.shell.session_dirty = true;
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
        let hover_changed = self.shell.refresh_hover();
        let Some(surface) = self.shell.hit_regions.resolve_scroll(column, row) else {
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
                self.shell.narrow_details = false;
                self.shell.focus = Focus::Tickets;
            }
            PointerTarget::NarrowDetails => {
                self.shell.narrow_details = true;
                if !self.shell.focus.is_details_pane() {
                    self.shell.focus = Focus::Details;
                }
            }
            PointerTarget::FocusTickets => {
                self.shell.focus = Focus::Tickets;
                self.shell.narrow_details = false;
            }
            PointerTarget::FocusDetails => {
                self.shell.focus = Focus::Details;
            }
            PointerTarget::TableRow { index } => {
                self.shell.focus = Focus::Tickets;
                self.shell.narrow_details = false;
                if index < self.visible.len() {
                    self.select_row(index);
                    self.record_history();
                }
            }
            PointerTarget::OpenTicket { index } => {
                self.shell.focus = Focus::Tickets;
                self.shell.narrow_details = false;
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
                self.shell.focus = Focus::Details;
                self.shell.narrow_details = true;
                return self.open_selected();
            }
            PointerTarget::JumpToTicket(key) => {
                if self
                    .visible_family_tree()
                    .iter()
                    .any(|entry| entry.key == key)
                {
                    self.shell.focus = Focus::Family;
                    self.family_cursor = Some(key.clone());
                    self.ensure_family_cursor_visible();
                } else if self
                    .selected_family()
                    .is_some_and(|family| family.extra_parents.iter().any(|parent| parent == &key))
                {
                    self.shell.focus = Focus::Family;
                } else {
                    self.shell.focus = Focus::Details;
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
                self.shell.session_dirty = true;
            }
            PointerTarget::ColumnMove { index, delta } => {
                self.column_overlay.index = self.layout.move_column(index, delta);
                self.shell.session_dirty = true;
            }
            PointerTarget::ColumnResize { index, delta } => {
                self.column_overlay.index = index;
                self.layout.resize(index, delta);
                self.shell.session_dirty = true;
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
        match self.shell.hovered_region().map(|region| &region.target) {
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
            .shell
            .hit_regions
            .edit_field(field)
            .map_or(OverlayAnchor::Centered, OverlayAnchor::Below);
        let action = self.run_command(command_for_field(field));
        self.shell.overlay_anchor = anchor;
        action
    }

    fn place_caret(&mut self, editor: TextEditor, column: u16, row: u16) {
        let Some(snapshot) = self
            .shell
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
                self.shell
                    .hit_regions
                    .resolve(column, row)
                    .map(|region| TextPos {
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

    fn drag_scrollbar(&mut self, surface: ScrollSurface, row: u16, grab: i16) {
        let Some(metrics) = self.shell.hit_regions.scroll(surface) else {
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
}
