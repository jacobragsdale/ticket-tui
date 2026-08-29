//! The overlays that pick or type a value: every picker, the prompt, the
//! form and the delete confirmation.

use super::*;

/// The Edit menu: one row per field editor, each labelled with the field it
/// changes and the key that opens it directly.
pub(super) fn render_edit_menu(frame: &mut Frame<'_>, app: &mut App) {
    let entries = app.edit_menu_entries();
    let height = u16::try_from(entries.len().saturating_add(2)).unwrap_or(u16::MAX);
    let area = centered_rect(frame.area(), 40, height.max(3));
    let inner = render_modal_frame(frame, app, area, " Edit ");
    let selected = app.edit_menu.index;
    let rows: Vec<Line> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let marker = if index == selected { "\u{203a}" } else { " " };
            Line::from(format!(
                "{marker} {:<20} {}",
                entry.label,
                key_label_for(entry.command)
            ))
        })
        .collect();
    render_list_overlay(
        frame,
        app,
        ListOverlay {
            area: inner,
            surface: ScrollSurface::EditMenu,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: true,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::EditMenuRow { index },
            decorate: None,
        },
    );
}

/// The state picker: every state this work item's type allows, coloured by the
/// same categories the table's State column uses, with the state it is in
/// already marked and under the cursor.
pub(super) fn render_state_picker(frame: &mut Frame<'_>, app: &mut App) {
    let options = app.state_picker.options.clone();
    let current = app.state_picker.current.clone();
    let height = u16::try_from(options.len().saturating_add(2))
        .unwrap_or(u16::MAX)
        .clamp(3, 16);
    let selected = app.state_picker.index;
    let rows: Vec<Line> = options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let marker = if index == selected { "\u{203a}" } else { " " };
            let here = if option.name == current {
                "\u{2022}"
            } else {
                " "
            };
            Line::from(vec![
                Span::raw(format!("{marker}{here} ")),
                Span::styled(option.name.clone(), state_category_style(option.category)),
            ])
        })
        .collect();
    let width = overlay_width(app.shell.overlay_anchor, &rows, 40, frame.area());
    let area = overlay_area(frame.area(), app.shell.overlay_anchor, width, height);
    let title = format!(" State \u{b7} {} ", app.state_picker.scope.label());
    let inner = render_modal_frame(frame, app, area, &title);
    render_list_overlay(
        frame,
        app,
        ListOverlay {
            area: inner,
            surface: ScrollSurface::StatePicker,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: true,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::StateOption { index },
            decorate: None,
        },
    );
}

/// The priority picker: 1 to 4 in the colours the Pri column uses, then a
/// `Clear` row that takes the field off the work item, with the priority it
/// already has marked and under the cursor.
pub(super) fn render_priority_picker(frame: &mut Frame<'_>, app: &mut App) {
    let current = app.priority_picker.current;
    let height = u16::try_from(PRIORITY_CHOICES.len().saturating_add(2)).unwrap_or(u16::MAX);
    let selected = app.priority_picker.index;
    let rows: Vec<Line> = PRIORITY_CHOICES
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            let marker = if index == selected { "\u{203a}" } else { " " };
            let here = if *choice == current { "\u{2022}" } else { " " };
            let label = choice.map_or_else(|| "Clear".to_owned(), |value| value.to_string());
            Line::from(vec![
                Span::raw(format!("{marker}{here} ")),
                Span::styled(label, priority_style(*choice)),
            ])
        })
        .collect();
    let width = overlay_width(app.shell.overlay_anchor, &rows, 40, frame.area());
    let area = overlay_area(frame.area(), app.shell.overlay_anchor, width, height);
    let title = format!(" Priority \u{b7} #{} ", app.priority_picker.id);
    let inner = render_modal_frame(frame, app, area, &title);
    render_list_overlay(
        frame,
        app,
        ListOverlay {
            area: inner,
            surface: ScrollSurface::PriorityPicker,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: true,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::PriorityOption { index },
            decorate: None,
        },
    );
}

/// The assignee picker: a filter field over everybody worth offering, with
/// `Unassigned` first, the signed-in user named as such, and whoever holds the
/// work item already marked and under the cursor.
pub(super) fn render_assignee_picker(frame: &mut Frame<'_>, app: &mut App) {
    let candidates = app.assignee_matches();
    let current = app.assignee_picker.current.clone();
    let height = u16::try_from(candidates.len().saturating_add(3))
        .unwrap_or(u16::MAX)
        .clamp(5, 18);
    let selected = app.assignee_picker.index;
    let rows: Vec<Line> = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let marker = if index == selected { "\u{203a}" } else { " " };
            let here = if candidate.is_current(current.as_deref()) {
                "\u{2022}"
            } else {
                " "
            };
            let name = Style::default().fg(if candidate.unassigned {
                theme().muted
            } else {
                theme().text
            });
            let mut spans = vec![
                Span::raw(format!("{marker}{here} ")),
                Span::styled(candidate.display.clone(), name),
            ];
            if candidate.me {
                spans.push(Span::styled(
                    " (me)",
                    Style::default()
                        .fg(theme().accent)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            Line::from(spans)
        })
        .collect();
    let width = overlay_width(app.shell.overlay_anchor, &rows, 52, frame.area());
    let area = overlay_area(frame.area(), app.shell.overlay_anchor, width, height);
    let title = format!(
        " Assignee \u{b7} {} ",
        app.scope_label(app.assignee_picker.scope)
    );
    let inner = render_modal_frame(frame, app, area, &title);
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).split(inner);
    let (text, cursor) = (
        app.assignee_picker.query.text().to_owned(),
        app.assignee_picker.query.cursor(),
    );
    render_query_field(
        frame,
        app,
        chunks[0],
        &text,
        cursor,
        "Filter people\u{2026}",
        PointerTarget::AssigneeQuery,
    );
    render_list_overlay(
        frame,
        app,
        ListOverlay {
            area: chunks[1],
            surface: ScrollSurface::AssigneePicker,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: false,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::AssigneeOption { index },
            decorate: None,
        },
    );
}

/// The parent picker: a filter field over every work item the selected one
/// could be filed under, each row naming its id, its type, and its title, with
/// the parent it hangs under already marked and under the cursor. Neither the
/// work item itself nor anything below it is in the list, so no row here can
/// make a cycle.
pub(super) fn render_parent_picker(frame: &mut Frame<'_>, app: &mut App) {
    let candidates = app.parent_matches();
    let current = app.parent_picker.current.clone();
    let height = u16::try_from(candidates.len().saturating_add(3))
        .unwrap_or(u16::MAX)
        .clamp(5, 18);
    let selected = app.parent_picker.index;
    let rows: Vec<Line> = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let marker = if index == selected { "\u{203a}" } else { " " };
            let here = if current.as_ref() == Some(&candidate.key) {
                "\u{2022}"
            } else {
                " "
            };
            Line::from(vec![
                Span::raw(format!("{marker}{here} ")),
                Span::styled(
                    format!("#{}", candidate.key.id),
                    Style::default().fg(theme().accent),
                ),
                Span::styled(
                    format!(" {} ", candidate.work_item_type),
                    Style::default().fg(theme().muted),
                ),
                Span::styled(candidate.title.clone(), Style::default().fg(theme().text)),
            ])
        })
        .collect();
    let width = overlay_width(app.shell.overlay_anchor, &rows, 64, frame.area());
    let area = overlay_area(frame.area(), app.shell.overlay_anchor, width, height);
    let title = format!(" Parent of #{} ", app.parent_picker.child.id);
    let inner = render_modal_frame(frame, app, area, &title);
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).split(inner);
    let (text, cursor) = (
        app.parent_picker.query.text().to_owned(),
        app.parent_picker.query.cursor(),
    );
    render_query_field(
        frame,
        app,
        chunks[0],
        &text,
        cursor,
        "Filter by id or title\u{2026}",
        PointerTarget::ParentQuery,
    );
    render_list_overlay(
        frame,
        app,
        ListOverlay {
            area: chunks[1],
            surface: ScrollSurface::ParentPicker,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: false,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::ParentOption { index },
            decorate: None,
        },
    );
}

/// The iteration or area picker: the project's tree as indented rows, the leaf
/// of each named and the rest of the path implied by the indent, with the node
/// the work item sits in already marked and under the cursor. An iteration row
/// carries the days it runs between, and the one containing today says
/// `current`.
pub(super) fn render_node_picker(frame: &mut Frame<'_>, app: &mut App) {
    let rows_data = app.node_matches();
    let current = app.node_picker.current.clone();
    let kind = app.node_picker.kind;
    let height = u16::try_from(rows_data.len().saturating_add(3))
        .unwrap_or(u16::MAX)
        .clamp(5, 20);
    let selected = app.node_picker.index;
    let rows: Vec<Line> = rows_data
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let marker = if index == selected { "\u{203a}" } else { " " };
            let here = if row.path == current { "\u{2022}" } else { " " };
            let mut spans = vec![
                Span::raw(format!("{marker}{here} {}", row.indent())),
                Span::styled(row.leaf().to_owned(), Style::default().fg(theme().text)),
            ];
            if let Some(dates) = row.dates.as_deref() {
                spans.push(Span::styled(
                    format!("  {dates}"),
                    Style::default().fg(theme().muted),
                ));
            }
            if row.current_period {
                spans.push(Span::styled(
                    " current",
                    Style::default()
                        .fg(theme().accent)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            Line::from(spans)
        })
        .collect();
    let width = overlay_width(app.shell.overlay_anchor, &rows, 56, frame.area());
    let area = overlay_area(frame.area(), app.shell.overlay_anchor, width, height);
    let title = format!(
        " {} \u{b7} {} ",
        kind.label(),
        app.scope_label(app.node_picker.scope)
    );
    let inner = render_modal_frame(frame, app, area, &title);
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).split(inner);
    let (text, cursor) = (
        app.node_picker.query.text().to_owned(),
        app.node_picker.query.cursor(),
    );
    render_query_field(
        frame,
        app,
        chunks[0],
        &text,
        cursor,
        &format!("Filter {}\u{2026}", kind.label().to_lowercase()),
        PointerTarget::NodeQuery,
    );
    render_list_overlay(
        frame,
        app,
        ListOverlay {
            area: chunks[1],
            surface: ScrollSurface::NodePicker,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: false,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::NodeOption { index },
            decorate: None,
        },
    );
}

/// The single-line prompts: the title and tags fields, prefilled with what the
/// work item says now, and the comment box, which starts empty. All are edited
/// with the same keys as the named-view editor.
pub(super) fn render_prompt(frame: &mut Frame<'_>, app: &mut App) {
    let Some((field, text, cursor, id)) = app.prompt.as_ref().map(|prompt| {
        (
            prompt.field,
            prompt.input.text().to_owned(),
            prompt.input.cursor(),
            prompt.id,
        )
    }) else {
        return;
    };
    let measured = [Line::from(format!("{}: {text}", field.label()))];
    let width = overlay_width(app.shell.overlay_anchor, &measured, 64, frame.area());
    let area = overlay_area(frame.area(), app.shell.overlay_anchor, width, 5);
    let title = format!(" {} ", field.title(id));
    let inner = render_modal_frame(frame, app, area, &title);
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .split(inner);
    let prefix = format!("{}: ", field.label());
    let offset = u16::try_from(prefix.chars().count()).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prefix, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(text.clone()),
        ])),
        chunks[0],
    );
    let editable = Rect::new(
        chunks[0].x.saturating_add(offset),
        chunks[0].y,
        chunks[0].width.saturating_sub(offset),
        1,
    );
    app.shell.hit_regions.push(region(
        editable,
        PointerTarget::PromptInput,
        PointerLayer::Modal,
        Some(SelectableSurface::Overlay),
        None,
    ));
    capture_selectable(frame, app, SelectableSurface::Overlay, editable, false);
    let cursor_x = editable
        .x
        .saturating_add(u16::try_from(cursor).unwrap_or(u16::MAX))
        .min(editable.x.saturating_add(editable.width.saturating_sub(1)));
    frame.set_cursor_position((cursor_x, editable.y));
    // A title has to say something and so does a comment; a tag list is allowed
    // to end up empty, which clears the tags.
    let savable = field == PromptField::Tags || !text.trim().is_empty();
    render_control(
        frame,
        app,
        Rect::new(chunks[1].x, chunks[1].y, 6, 1),
        "[Save]",
        PointerTarget::SubmitPrompt,
        PointerLayer::Modal,
        savable,
    );
    render_control(
        frame,
        app,
        Rect::new(chunks[1].x.saturating_add(7), chunks[1].y, 8, 1),
        "[Cancel]",
        PointerTarget::CancelPrompt,
        PointerLayer::Modal,
        true,
    );
}

/// How wide the label column of a form is, so every value lines up whatever
/// the field is called.
pub(super) const FORM_LABEL_WIDTH: u16 = 11;

/// The work item type picker: every type the project's process offers, with the
/// one the form already names marked and under the cursor.
pub(super) fn render_type_picker(frame: &mut Frame<'_>, app: &mut App) {
    let options = app.type_picker.options.clone();
    let current = app.type_picker.current.clone();
    let height = u16::try_from(options.len().saturating_add(2))
        .unwrap_or(u16::MAX)
        .clamp(3, 16);
    let selected = app.type_picker.index;
    let rows: Vec<Line> = options
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let marker = if index == selected { "\u{203a}" } else { " " };
            let here = if *name == current { "\u{2022}" } else { " " };
            Line::from(vec![
                Span::raw(format!("{marker}{here} ")),
                Span::styled(name.clone(), Style::default().fg(theme().text)),
            ])
        })
        .collect();
    let area = centered_rect(frame.area(), 36, height);
    let inner = render_modal_frame(frame, app, area, " Type ");
    render_list_overlay(
        frame,
        app,
        ListOverlay {
            area: inner,
            surface: ScrollSurface::TypePicker,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: true,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::TypeOption { index },
            decorate: None,
        },
    );
}

/// A form: its fields down the left, their values beside them, and the two
/// buttons underneath. Nothing here knows what the fields mean — the labels,
/// the order, and which of them open pickers all come off the form itself — so
/// every form in the app is drawn by this one function.
pub(super) fn render_form(frame: &mut Frame<'_>, app: &mut App) {
    let Some((title, fields, selected)) = app.form.as_ref().map(|form| {
        (
            form.title.clone(),
            form.fields.clone(),
            form.index.min(form.fields.len().saturating_sub(1)),
        )
    }) else {
        return;
    };
    let submittable = app.form.as_ref().is_some_and(FormOverlay::is_submittable);
    let height = u16::try_from(fields.len().saturating_add(4))
        .unwrap_or(u16::MAX)
        .min(frame.area().height);
    let area = centered_rect(frame.area(), 66, height);
    let inner = render_modal_frame(frame, app, area, &format!(" {title} "));
    let chunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);
    let rows = chunks[0];
    let viewport = usize::from(rows.height);
    app.scroll_state_mut(ScrollSurface::Form)
        .set_viewport(viewport, fields.len());
    let scroll = app.scroll_state(ScrollSurface::Form).offset;
    let value_x = rows.x.saturating_add(2).saturating_add(FORM_LABEL_WIDTH);
    let value_width = rows
        .width
        .saturating_sub(value_x.saturating_sub(rows.x))
        .saturating_sub(1);
    let mut caret: Option<(u16, u16)> = None;
    for (index, y) in (scroll..fields.len().min(scroll + viewport)).zip(rows.y..) {
        let field = &fields[index];
        let focused = index == selected;
        let label = if field.required {
            format!("{} *", field.label)
        } else {
            field.label.to_owned()
        };
        let value_style = if field.read_only {
            Style::default().fg(theme().muted)
        } else {
            Style::default().fg(theme().text)
        };
        let value = if field.shown().is_empty() {
            Span::styled(
                field.placeholder.to_owned(),
                Style::default().fg(theme().muted),
            )
        } else {
            Span::styled(field.shown().to_owned(), value_style)
        };
        let mut spans = vec![
            Span::raw(if focused { "\u{203a} " } else { "  " }),
            Span::styled(
                format!("{label:<width$}", width = usize::from(FORM_LABEL_WIDTH)),
                Style::default().fg(theme().muted),
            ),
            value,
        ];
        if field.picker_kind().is_some() {
            spans.push(Span::styled(
                " \u{25be}",
                Style::default().fg(theme().accent),
            ));
        }
        frame.render_widget(
            Paragraph::new(overlay_line(Line::from(spans), focused)),
            Rect::new(rows.x, y, rows.width, 1),
        );
        let label_rect = Rect::new(rows.x, y, FORM_LABEL_WIDTH.saturating_add(2), 1);
        let value_rect = Rect::new(value_x, y, value_width, 1);
        for rect in [label_rect, value_rect] {
            app.shell.hit_regions.push(region(
                rect,
                PointerTarget::FormField { index },
                PointerLayer::Modal,
                Some(SelectableSurface::Overlay),
                Some(ScrollSurface::Form),
            ));
        }
        if focused && field.is_typed() {
            caret = Some((
                value_x
                    .saturating_add(u16::try_from(field.input.cursor()).unwrap_or(u16::MAX))
                    .min(value_x.saturating_add(value_width.saturating_sub(1))),
                y,
            ));
        }
    }
    if fields.len() > viewport {
        render_scrollbar(
            frame,
            app,
            rows,
            ScrollSurface::Form,
            fields.len(),
            scroll,
            viewport,
        );
    }
    if let Some(field) = fields.get(selected)
        && field.is_typed()
    {
        capture_selectable(
            frame,
            app,
            SelectableSurface::Overlay,
            Rect::new(
                value_x,
                rows.y
                    .saturating_add(u16::try_from(selected.saturating_sub(scroll)).unwrap_or(0)),
                value_width,
                1,
            ),
            false,
        );
    }
    if let Some((x, y)) = caret {
        frame.set_cursor_position((x, y));
    }
    let buttons = chunks[2];
    render_control(
        frame,
        app,
        Rect::new(buttons.x, buttons.y, 8, 1),
        "[Create]",
        PointerTarget::SubmitForm,
        PointerLayer::Modal,
        submittable,
    );
    render_control(
        frame,
        app,
        Rect::new(buttons.x.saturating_add(9), buttons.y, 8, 1),
        "[Cancel]",
        PointerTarget::CancelForm,
        PointerLayer::Modal,
        true,
    );
}

/// The delete confirmation: what is going, what it leaves behind, and how to
/// get it back.
///
/// Three things have to be on screen before anybody presses `d`. Which work
/// item it is, by id and title, so the confirmation is about a work item rather
/// than about a row. How many children it has and that they stay — an Epic over
/// eight issues is exactly when somebody needs telling. And that the delete is
/// the recoverable one, because a confirmation more frightening than the action
/// warrants is its own kind of wrong.
pub(super) fn render_delete_confirm(frame: &mut Frame<'_>, app: &mut App) {
    let Some(confirm) = app.delete_confirm.clone() else {
        return;
    };
    let mut text = vec![
        Line::styled(
            confirm.question(),
            Style::default()
                .fg(theme().text)
                .add_modifier(Modifier::BOLD),
        ),
        Line::default(),
    ];
    if let Some(orphans) = confirm.orphans() {
        text.push(Line::styled(orphans, Style::default().fg(theme().warning)));
    }
    text.push(Line::styled(
        "It goes to the Azure DevOps recycle bin and can be restored from there.",
        Style::default().fg(theme().muted),
    ));
    text.push(Line::default());
    let width: u16 = 66;
    let body = Paragraph::new(Text::from(text.clone())).wrap(Wrap { trim: false });
    // The lines wrap, so the height comes off the wrapped paragraph rather than
    // off the count: a long title must not push the buttons out of the frame.
    let height = u16::try_from(body.line_count(width.saturating_sub(2)).saturating_add(3))
        .unwrap_or(u16::MAX)
        .min(frame.area().height);
    let area = centered_rect(frame.area(), width, height);
    let inner = render_modal_frame(frame, app, area, " Delete ");
    let chunks = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).split(inner);
    frame.render_widget(body, chunks[0]);
    app.shell.hit_regions.push(region(
        chunks[0],
        PointerTarget::OverlayBody,
        PointerLayer::Modal,
        Some(SelectableSurface::Overlay),
        None,
    ));
    capture_selectable(frame, app, SelectableSurface::Overlay, chunks[0], false);
    let buttons = chunks[1];
    render_control(
        frame,
        app,
        Rect::new(buttons.x, buttons.y, 8, 1),
        "[Delete]",
        PointerTarget::ConfirmDelete,
        PointerLayer::Modal,
        true,
    );
    render_control(
        frame,
        app,
        Rect::new(buttons.x.saturating_add(9), buttons.y, 8, 1),
        "[Cancel]",
        PointerTarget::CancelDelete,
        PointerLayer::Modal,
        true,
    );
}
