//! The overlays that list things: sort, help, filters, columns, the
//! palette, views, the sprint summary and the chips and facets above the table.

use super::*;

pub(super) fn render_sort_popup(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
) {
    let area = centered_rect(frame.area(), 48, 16);
    let inner = render_modal_frame(frame, modal_layer(screen), shell, area, " Sort tickets ");
    let selected = screen.sort_draft.field_index;
    let rows: Vec<Line> = SortField::ALL
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let marker = if index == selected { "›" } else { " " };
            let direction = if index == selected {
                screen.sort_draft.direction.symbol()
            } else if *field == screen.sort_field {
                screen.sort_direction.symbol()
            } else {
                " "
            };
            Line::from(format!("{marker} {:<14} {direction}", field.label()))
        })
        .collect();
    render_list_overlay(
        frame,
        screen,
        shell,
        ListOverlay {
            area: inner,
            surface: ScrollSurface::Sort,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: false,
            selected,
            rows,
            row_hit_width: Some(inner.width.saturating_sub(8)),
            target: &|index| PointerTarget::SortChoose(SortField::ALL[index]),
            decorate: Some(&|frame: &mut Frame<'_>,
                             _screen: &mut WorkItemsScreen,
                             shell: &mut Shell,
                             logical,
                             y| {
                if logical == selected {
                    render_sort_controls(frame, shell, inner, y);
                }
            }),
        },
    );
    capture_selectable(frame, shell, SelectableSurface::Overlay, inner, false);
}

pub(super) fn render_sort_controls(frame: &mut Frame<'_>, shell: &mut Shell, inner: Rect, y: u16) {
    for (offset, label, direction) in [
        (7, "[↑]", SortDirection::Ascending),
        (3, "[↓]", SortDirection::Descending),
    ] {
        render_control(
            frame,
            shell,
            Rect::new(
                inner.x.saturating_add(inner.width.saturating_sub(offset)),
                y,
                3,
                1,
            ),
            label,
            PointerTarget::SortSetDirection(direction),
            PointerLayer::Modal,
            true,
        );
    }
}

pub(super) fn render_help_popup(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
) {
    let height = frame.area().height.saturating_sub(2).min(18);
    let area = centered_rect(frame.area(), 62, height);
    frame.render_widget(Clear, area);
    let mut lines = vec![
        Line::styled("Navigation", Style::default().add_modifier(Modifier::BOLD)),
        Line::from("  ↑/↓, j/k        Move ticket, family row, or details"),
        Line::from("  PgUp/PgDn       Move ten rows or a family page"),
        Line::from("  Home/End        First/last ticket, family row, or line"),
        Line::from("  Tab             Toggle tickets / details focus"),
        Line::from("  Enter           Select family cursor, or open from details"),
        Line::from("  Space           Toggle ticket multi-select"),
        Line::from("  Esc             Clear active search or selection"),
        Line::from(""),
        Line::styled(
            "Search and filters",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::from("  ←/→, Home/End   Move the query cursor"),
        Line::from("  Backspace/Del   Delete around the cursor"),
        Line::from("  Ctrl-W/Ctrl-U   Delete word / clear query"),
        Line::from("  Ctrl-P/Ctrl-N   Previous / next completed query"),
        Line::from("  state:active    Structured filters in the query"),
        Line::from("  Paste           Insert sanitized text"),
        Line::from(""),
        Line::styled("Tabs", Style::default().add_modifier(Modifier::BOLD)),
        Line::from("  1/2/3/4         Work items, Repos, Pull requests, Pipelines"),
        Line::from(""),
        Line::styled("Everywhere", Style::default().add_modifier(Modifier::BOLD)),
    ];
    // The bound commands describe themselves; the palette lists the rest. The
    // global ones come first, then the ones this tab owns, under its name.
    let described = |command: &Command| {
        let detail = if command.help.is_empty() {
            command.title.to_owned()
        } else {
            format!("{} — {}", command.title, command.help)
        };
        Line::from(format!("  {:<15} {detail}", command.key_label()))
    };
    lines.extend(
        COMMANDS
            .iter()
            .filter(|command| !command.keys.is_empty() && command.scope == Scope::Global)
            .map(described),
    );
    lines.push(Line::from(""));
    lines.push(Line::styled(
        TabId::WorkItems.label(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    lines.extend(
        COMMANDS
            .iter()
            .filter(|command| {
                !command.keys.is_empty() && command.scope == Scope::Tab(TabId::WorkItems)
            })
            .map(described),
    );
    lines.extend([
        Line::from(""),
        Line::styled("Mouse", Style::default().add_modifier(Modifier::BOLD)),
        Line::from("  Wheel           Scroll the hovered table, details, help, or overlay"),
        Line::from("  Click           Activate buttons, rows, links, headers, and checkboxes"),
        Line::from("  Drag            Select visible text and copy it on release"),
        Line::from("  Scrollbar       Click the track or drag the thumb"),
        Line::from("  Divider         Drag between panes to resize"),
        Line::from("  Paste           Insert into search, palette, and view-name fields"),
        Line::from(""),
        Line::styled(
            "Press ? or Esc to close",
            Style::default().fg(theme().muted),
        ),
    ]);
    let help = Text::from(lines);
    let block = Block::default()
        .title(" Help ")
        .title(Line::from("[×]").right_aligned())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme().accent));
    let inner = block.inner(area);
    let paragraph = Paragraph::new(help).block(block).wrap(Wrap { trim: false });
    let line_count = paragraph.line_count(area.width);
    screen
        .help
        .set_viewport(usize::from(inner.height), line_count);
    let scroll = screen.help.offset;
    frame.render_widget(
        paragraph.scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        area,
    );
    shell.hit_regions.push(region(
        inner,
        PointerTarget::OverlayBody,
        PointerLayer::Modal,
        Some(SelectableSurface::Help),
        Some(ScrollSurface::Help),
    ));
    register_close_button(shell, area, PointerLayer::Modal);
    let overflow = line_count > usize::from(inner.height);
    if overflow {
        render_scrollbar(
            frame,
            current_layer(screen),
            shell,
            inner,
            ScrollSurface::Help,
            ScrollState {
                offset: scroll,
                content: line_count,
                viewport: usize::from(inner.height),
            },
        );
    }
    capture_selectable(frame, shell, SelectableSurface::Help, inner, overflow);
}

pub(super) fn render_chips(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let mut spans = Vec::new();
    let mut x = area.x;
    // The rule the screen applies on its own leads the row, so it keeps its place
    // however many filters are typed beside it, and reads like the rest: its
    // `×` puts finished work back on the table.
    let mut chips: Vec<(String, PointerTarget)> = Vec::new();
    if screen.finished_hidden() {
        chips.push(("Finished hidden".to_owned(), PointerTarget::ShowFinished));
    }
    chips.extend(
        screen
            .overflow_filter_tokens()
            .into_iter()
            .map(|(index, token)| (token.chip_label(), PointerTarget::RemoveChip { index })),
    );
    for (text, target) in chips {
        let label = format!(" {text} × ");
        let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
        if x.saturating_add(width) > area.x.saturating_add(area.width) {
            break;
        }
        shell.hit_regions.push(region(
            Rect::new(x, area.y, width, 1),
            target,
            PointerLayer::Base,
            None,
            None,
        ));
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(theme().text)
                .bg(theme().selected_background),
        ));
        spans.push(Span::raw(" "));
        x = x.saturating_add(width.saturating_add(1));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(super) fn render_facet_bar(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let filters = screen.parsed_query().filters;
    let focused = screen.mode == WorkItemMode::Facets;
    let mut spans = Vec::new();
    let mut x = area.x;
    let mut remaining = area.width;
    for (index, field) in FilterField::BAR.iter().enumerate() {
        let label = facet_pill_label(*field, &filters);
        let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
        if remaining < width.saturating_add(1) {
            break;
        }
        let rect = Rect::new(x, area.y, width, 1);
        shell.hit_regions.push(region(
            rect,
            PointerTarget::FacetPill(FacetTarget::Field(field.key())),
            PointerLayer::Base,
            None,
            None,
        ));
        let selected = focused && screen.facet_bar.field_index == index;
        let active = filters.selected_count(*field) > 0;
        spans.push(Span::styled(label, pill_style(selected, active)));
        spans.push(Span::raw(" "));
        x = x.saturating_add(width.saturating_add(1));
        remaining = remaining.saturating_sub(width.saturating_add(1));
    }
    if remaining >= 5 {
        let more_count = screen.overflow_filter_tokens().len();
        let more = if more_count == 0 {
            " + ".to_owned()
        } else {
            format!(" +{more_count} ")
        };
        let width = u16::try_from(more.chars().count()).unwrap_or(u16::MAX);
        shell.hit_regions.push(region(
            Rect::new(x, area.y, width.min(remaining), 1),
            PointerTarget::FacetPill(FacetTarget::More),
            PointerLayer::Base,
            None,
            None,
        ));
        let selected = focused && screen.facet_bar.field_index >= FilterField::BAR.len();
        spans.push(Span::styled(more, pill_style(selected, more_count > 0)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(super) fn facet_pill_label(
    field: FilterField,
    filters: &crate::filter::FilterSet<WorkItemSchema>,
) -> String {
    let selected = filters.selected_values(field);
    match selected.as_slice() {
        [] => format!(" {} ▾ ", field.label()),
        [value] => format!(" {}:{} ", field.label(), truncate_pill(value, 12)),
        [value, rest @ ..] => {
            format!(
                " {}:{} +{} ",
                field.label(),
                truncate_pill(value, 8),
                rest.len()
            )
        }
    }
}

pub(super) fn truncate_pill(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_owned()
    } else {
        format!(
            "{}…",
            value
                .chars()
                .take(max.saturating_sub(1))
                .collect::<String>()
        )
    }
}

pub(super) fn pill_style(selected: bool, active: bool) -> Style {
    if selected {
        Style::default()
            .fg(theme().text)
            .bg(theme().selected_background)
            .add_modifier(Modifier::BOLD)
    } else if active {
        Style::default()
            .fg(theme().text)
            .bg(theme().selected_background)
    } else {
        Style::default().fg(theme().muted)
    }
}

/// Paints extras for one visible overlay row, given its logical index and `y`.
pub(super) type RowDecorator<'a> =
    &'a dyn Fn(&mut Frame<'_>, &mut WorkItemsScreen, &mut Shell, usize, u16);

pub(super) struct ListOverlay<'a> {
    pub(super) area: Rect,
    pub(super) surface: ScrollSurface,
    pub(super) layer: PointerLayer,
    /// Selectable surface recorded on each row hit region.
    pub(super) selectable: Option<SelectableSurface>,
    /// Snapshot `area` for text selection once the rows are painted.
    pub(super) capture: bool,
    pub(super) selected: usize,
    /// One unstyled line per logical row; the selected row is styled here.
    pub(super) rows: Vec<Line<'a>>,
    /// Hit region width, defaulting to the area minus its scrollbar column.
    pub(super) row_hit_width: Option<u16>,
    pub(super) target: &'a dyn Fn(usize) -> PointerTarget,
    /// Extra painting for each visible row. It runs before the scrollbar, so row
    /// controls reaching into the last column stay underneath the scrollbar.
    pub(super) decorate: Option<RowDecorator<'a>>,
}

/// Renders one scrollable list inside an overlay: viewport bookkeeping, the visible
/// window, selection styling, per-row hit regions, the scrollbar on overflow, and the
/// text selection snapshot.
pub(super) fn render_list_overlay(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
    overlay: ListOverlay<'_>,
) {
    let ListOverlay {
        area,
        surface,
        layer,
        selectable,
        capture,
        selected,
        rows,
        row_hit_width,
        target,
        decorate,
    } = overlay;
    let content = rows.len();
    let viewport = usize::from(area.height);
    screen
        .scroll_state_mut(surface)
        .set_viewport(viewport, content);
    let scroll = screen.scroll_state(surface).offset;
    let lines: Vec<Line<'_>> = rows
        .into_iter()
        .enumerate()
        .skip(scroll)
        .take(viewport)
        .map(|(index, line)| overlay_line(line, index == selected))
        .collect();
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
    let hit_width = row_hit_width.unwrap_or_else(|| area.width.saturating_sub(1));
    let visible = scroll..content.min(scroll.saturating_add(viewport));
    for (logical, y) in visible.clone().zip(area.y..) {
        shell.hit_regions.push(region(
            Rect::new(area.x, y, hit_width, 1),
            target(logical),
            layer,
            selectable,
            Some(surface),
        ));
    }
    if let Some(decorate) = decorate {
        for (logical, y) in visible.zip(area.y..) {
            decorate(frame, screen, shell, logical, y);
        }
    }
    let overflow = content > viewport;
    if overflow {
        render_scrollbar(
            frame,
            current_layer(screen),
            shell,
            area,
            surface,
            ScrollState {
                offset: scroll,
                content,
                viewport,
            },
        );
    }
    if capture && let Some(surface) = selectable {
        capture_selectable(frame, shell, surface, area, overflow);
    }
}

pub(super) fn render_facet_menu(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
) {
    let Some(field) = FilterField::BAR.get(screen.facet_bar.field_index).copied() else {
        return;
    };
    let facets = screen.facets_for(shell, field);
    let pill = shell
        .hit_regions
        .facet_pill(FacetTarget::Field(field.key()));
    let width = 36.min(frame.area().width.saturating_sub(2)).max(20);
    let height = u16::try_from(facets.len().saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(14)
        .min(frame.area().height.saturating_sub(2));
    let mut area = Rect {
        x: pill.map_or(frame.area().x + 1, |pill| pill.x),
        y: pill.map_or(4, |pill| pill.y.saturating_add(1)),
        width,
        height,
    };
    if area.x.saturating_add(area.width) > frame.area().width {
        area.x = frame.area().width.saturating_sub(area.width);
    }
    if area.y.saturating_add(area.height) > frame.area().height {
        area.y = area.y.saturating_sub(area.height.saturating_add(1));
    }
    shell.hit_regions.push(region(
        frame.area(),
        PointerTarget::DismissFacet,
        PointerLayer::Popup,
        None,
        None,
    ));
    let inner = render_modal_frame(
        frame,
        modal_layer(screen),
        shell,
        area,
        &format!(" {} ", field.label()),
    );
    let selected = screen.facet_bar.value_index;
    let rows: Vec<Line> = facets
        .iter()
        .enumerate()
        .map(|(index, facet)| {
            let marker = if index == selected { "›" } else { " " };
            let check = if facet.selected { "[x]" } else { "[ ]" };
            Line::from(format!(
                "{marker} {check} {:<18} {:>4}",
                facet.value, facet.count
            ))
        })
        .collect();
    render_list_overlay(
        frame,
        screen,
        shell,
        ListOverlay {
            area: inner,
            surface: ScrollSurface::FacetMenu,
            layer: PointerLayer::Popup,
            selectable: None,
            capture: false,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::FacetValue { index },
            decorate: None,
        },
    );
}

pub(super) fn render_filter_overlay(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
) {
    let area = centered_rect(frame.area(), 52, 16);
    let title = if screen.filter_overlay.showing_values {
        format!(" {} ", screen.facet_field().label())
    } else {
        " Filters ".into()
    };
    let inner = render_modal_frame(frame, modal_layer(screen), shell, area, &title);
    let showing_values = screen.filter_overlay.showing_values;
    let selected = if showing_values {
        screen.filter_overlay.value_index
    } else {
        screen.filter_overlay.field_index
    };
    let rows: Vec<Line> = if showing_values {
        screen
            .current_facets(shell)
            .into_iter()
            .enumerate()
            .map(|(index, facet)| {
                let marker = if index == selected { "›" } else { " " };
                let check = if facet.selected { "[x]" } else { "[ ]" };
                Line::from(format!(
                    "{marker} {check} {:<18} {:>4}",
                    facet.value, facet.count
                ))
            })
            .collect()
    } else {
        FilterField::OVERLAY
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let marker = if index == selected { "›" } else { " " };
                let count = screen.parsed_query().filters.selected_count(*field);
                let suffix = if count == 0 {
                    String::new()
                } else {
                    format!("{count} selected")
                };
                Line::from(format!("{marker} {:<12} {suffix}", field.label()))
            })
            .collect()
    };
    render_list_overlay(
        frame,
        screen,
        shell,
        ListOverlay {
            area: inner,
            surface: ScrollSurface::Filter,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: true,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::FilterRow { index },
            decorate: None,
        },
    );
}

pub(super) fn render_column_overlay(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
) {
    let area = centered_rect(frame.area(), 56, 18);
    let inner = render_modal_frame(frame, modal_layer(screen), shell, area, " Columns ");
    let layout = Screen::columns(screen);
    let content = layout.count();
    let selected = screen.column_overlay.cursor.index;
    let rows: Vec<Line> = (0..content)
        .map(|index| {
            let marker = if index == selected { "›" } else { " " };
            let check = if layout.is_visible(index) {
                "[x]"
            } else {
                "[ ]"
            };
            let width = if layout.flexible(index) {
                "fill".into()
            } else {
                layout.width(index).to_string()
            };
            Line::from(format!(
                "{marker} {check} {:<12} {width}",
                layout.label(index)
            ))
        })
        .collect();
    render_list_overlay(
        frame,
        screen,
        shell,
        ListOverlay {
            area: inner,
            surface: ScrollSurface::Columns,
            layer: PointerLayer::Modal,
            selectable: None,
            capture: false,
            selected,
            rows,
            row_hit_width: Some(5),
            target: &|index| PointerTarget::ColumnToggle { index },
            decorate: Some(&|frame: &mut Frame<'_>,
                             screen: &mut WorkItemsScreen,
                             shell: &mut Shell,
                             logical,
                             y| {
                render_column_controls(frame, screen, shell, inner, content, logical, y);
            }),
        },
    );
}

pub(super) fn render_column_controls(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
    inner: Rect,
    content: usize,
    logical: usize,
    y: u16,
) {
    let resizable = !Screen::columns(screen).flexible(logical);
    let controls = [
        (
            15,
            "[↑]",
            PointerTarget::ColumnMove {
                index: logical,
                delta: -1,
            },
            logical > 0,
        ),
        (
            11,
            "[↓]",
            PointerTarget::ColumnMove {
                index: logical,
                delta: 1,
            },
            logical + 1 < content,
        ),
        (
            7,
            "[−]",
            PointerTarget::ColumnResize {
                index: logical,
                delta: -1,
            },
            resizable,
        ),
        (
            3,
            "[+]",
            PointerTarget::ColumnResize {
                index: logical,
                delta: 1,
            },
            resizable,
        ),
    ];
    for (offset, label, target, enabled) in controls {
        render_control(
            frame,
            shell,
            Rect::new(
                inner.x.saturating_add(inner.width.saturating_sub(offset)),
                y,
                3,
                1,
            ),
            label,
            target,
            PointerLayer::Modal,
            enabled,
        );
    }
}

pub(super) fn render_palette(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
) {
    let commands = screen.palette_commands();
    let height = u16::try_from(commands.len().saturating_add(4))
        .unwrap_or(u16::MAX)
        .min(16);
    let area = centered_rect(frame.area(), 56, height.max(6));
    let inner = render_modal_frame(frame, modal_layer(screen), shell, area, " Commands ");
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).split(inner);
    let (text, cursor) = (
        screen.palette.query.text().to_owned(),
        screen.palette.query.cursor(),
    );
    render_query_field(
        frame,
        shell,
        chunks[0],
        &text,
        cursor,
        "Filter commands…",
        PointerTarget::PaletteQuery,
    );
    let list_area = chunks[1];
    let selected = screen.palette.selected;
    let rows: Vec<Line> = commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let marker = if index == selected { "›" } else { " " };
            Line::from(format!(
                "{marker} {:<28} {}",
                command.title,
                command.key_label()
            ))
        })
        .collect();
    render_list_overlay(
        frame,
        screen,
        shell,
        ListOverlay {
            area: list_area,
            surface: ScrollSurface::Palette,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: false,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::PaletteCommand { index },
            decorate: None,
        },
    );
}

pub(super) fn render_views_overlay(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
) {
    let area = centered_rect(frame.area(), 56, 18);
    let title = if screen.views_overlay.naming.is_some() {
        " Save view "
    } else {
        " Views "
    };
    let inner = render_modal_frame(frame, modal_layer(screen), shell, area, title);
    if let Some((name, name_cursor)) = screen
        .views_overlay
        .naming
        .as_ref()
        .map(|input| (input.text().to_owned(), input.cursor()))
    {
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .split(inner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Name: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(name.clone()),
            ])),
            chunks[0],
        );
        let field = Rect::new(
            chunks[0].x.saturating_add(6),
            chunks[0].y,
            chunks[0].width.saturating_sub(6),
            1,
        );
        shell.hit_regions.push(region(
            field,
            PointerTarget::ViewName,
            PointerLayer::Modal,
            Some(SelectableSurface::Overlay),
            None,
        ));
        capture_selectable(frame, shell, SelectableSurface::Overlay, field, false);
        let cursor_x = field
            .x
            .saturating_add(u16::try_from(name_cursor).unwrap_or(u16::MAX))
            .min(field.x.saturating_add(field.width.saturating_sub(1)));
        frame.set_cursor_position((cursor_x, field.y));
        render_control(
            frame,
            shell,
            Rect::new(chunks[1].x, chunks[1].y, 6, 1),
            "[Save]",
            PointerTarget::SaveView,
            PointerLayer::Modal,
            !name.trim().is_empty(),
        );
        render_control(
            frame,
            shell,
            Rect::new(chunks[1].x.saturating_add(7), chunks[1].y, 8, 1),
            "[Cancel]",
            PointerTarget::CancelNaming,
            PointerLayer::Modal,
            true,
        );
        return;
    }
    if inner.width >= 28 {
        render_control(
            frame,
            shell,
            Rect::new(inner.x, inner.y, 14, 1),
            "[Save current]",
            PointerTarget::SaveView,
            PointerLayer::Modal,
            true,
        );
        render_control(
            frame,
            shell,
            Rect::new(inner.x.saturating_add(15), inner.y, 8, 1),
            "[Delete]",
            PointerTarget::DeleteView,
            PointerLayer::Modal,
            screen.can_delete_focused_view(),
        );
    }
    let list = Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        inner.height.saturating_sub(1),
    );
    let selected = screen.views_overlay.index;
    let rows: Vec<Line> = screen
        .view_rows()
        .iter()
        .enumerate()
        .map(|(index, row)| {
            if row.is_heading() {
                return Line::from(Span::styled(
                    row.label.clone(),
                    Style::default()
                        .fg(theme().muted)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            let marker = if index == selected { "›" } else { " " };
            let current = if row.active { "*" } else { " " };
            Line::from(format!("{marker}{current} {:<18} {}", row.label, row.query))
        })
        .collect();
    render_list_overlay(
        frame,
        screen,
        shell,
        ListOverlay {
            area: list,
            surface: ScrollSurface::Views,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: false,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::ViewRow { index },
            decorate: None,
        },
    );
}

/// The sprint summary: the per-assignee grid, the by-type tally under it, and
/// the headline, all painted through the same list helper the other overlays
/// use so the scrollbar and the hit regions come for free.
pub(super) fn render_sprint_overlay(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
) {
    let summary = screen.summary_rows();
    // Cut to the grid rather than fixed: a one-person sprint should not open a
    // half-empty box, and a whole team still gets its scrollbar.
    let widest = summary
        .iter()
        .map(|row| row.text.chars().count())
        .max()
        .unwrap_or_default()
        .saturating_add(SPRINT_OVERLAY_CHROME);
    let area = centered_rect(
        frame.area(),
        u16::try_from(widest)
            .unwrap_or(u16::MAX)
            .clamp(SPRINT_OVERLAY_MIN_WIDTH, SPRINT_OVERLAY_MAX_WIDTH),
        u16::try_from(summary.len().saturating_add(2))
            .unwrap_or(u16::MAX)
            .min(SPRINT_OVERLAY_MAX_HEIGHT),
    );
    let title = screen.summary_title();
    let inner = render_modal_frame(frame, modal_layer(screen), shell, area, &title);
    // An overlay with no grid — an empty sprint, or none to count — has nothing
    // for the cursor to sit on, so nothing is highlighted rather than the first
    // line of an explanation being lit up as though it were a row.
    let selected = summary
        .get(screen.sprint_overlay.index)
        .filter(|row| row.is_selectable())
        .map_or(usize::MAX, |_| screen.sprint_overlay.index);
    let rows: Vec<Line> = summary
        .iter()
        .enumerate()
        .map(|(index, row)| summary_line(row, index == selected))
        .collect();
    render_list_overlay(
        frame,
        screen,
        shell,
        ListOverlay {
            area: inner,
            surface: ScrollSurface::Sprint,
            layer: PointerLayer::Modal,
            selectable: Some(SelectableSurface::Overlay),
            capture: false,
            selected,
            rows,
            row_hit_width: None,
            target: &|index| PointerTarget::SummaryRow { index },
            decorate: None,
        },
    );
}

/// One line of the sprint summary. The grid rows carry the cursor marker and
/// the headings and tallies are indented past where it would sit, so the
/// columns line up down the whole overlay.
pub(super) fn summary_line(row: &SummaryRow, selected: bool) -> Line<'static> {
    let marker = if selected { "\u{203a}" } else { " " };
    match row.kind {
        SummaryRowKind::Blank => Line::default(),
        SummaryRowKind::Heading => Line::from(Span::styled(
            format!("  {}", row.text),
            Style::default()
                .fg(theme().muted)
                .add_modifier(Modifier::BOLD),
        )),
        SummaryRowKind::Note => Line::from(Span::styled(
            format!("  {}", row.text),
            Style::default().fg(theme().muted),
        )),
        SummaryRowKind::Total => Line::from(Span::styled(
            format!("{marker} {}", row.text),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        SummaryRowKind::Assignee(_) => Line::from(format!("{marker} {}", row.text)),
    }
}

pub(super) fn link_line(text: String) -> Line<'static> {
    terminate_underline(Line::from(Span::styled(
        text,
        Style::default()
            .fg(theme().link)
            .add_modifier(Modifier::UNDERLINED),
    )))
}

pub(super) fn terminate_underline(mut line: Line<'static>) -> Line<'static> {
    line.spans.push(Span::styled(
        " ",
        Style::default().remove_modifier(Modifier::UNDERLINED),
    ));
    line
}

pub(super) fn render_info_overlay(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
) {
    let area = centered_rect(frame.area(), 62, 13);
    let stale = if shell.stale { "stale" } else { "current" };
    // What the difference between the count and the total is made of, so the
    // rows the table is leaving out are a number rather than a suspicion.
    let finished = if screen.finished_hidden() {
        format!("{} hidden", screen.hidden_finished(shell))
    } else {
        "shown".to_owned()
    };
    let path = if shell.database_path.as_os_str().is_empty() {
        "(not set)".into()
    } else {
        shell.database_path.display().to_string()
    };
    let text = Text::from(vec![
        field_line("Path", path),
        field_line("Tickets", screen.tickets().len().to_string()),
        field_line("Visible", screen.visible_count().to_string()),
        field_line("Finished", finished),
        field_line("Loaded", shell.freshness_label()),
        field_line("Freshness", stale),
        field_line("Sync", shell.sync_summary()),
        field_line(
            "Watcher",
            shell
                .watch_state()
                .map_or_else(|| "not running".to_owned(), str::to_owned),
        ),
        Line::default(),
        Line::styled(
            "Press Esc or i to close",
            Style::default().fg(theme().muted),
        ),
    ]);
    let inner = render_modal_frame(frame, modal_layer(screen), shell, area, " Database ");
    frame.render_widget(Paragraph::new(text), inner);
    shell.hit_regions.push(region(
        inner,
        PointerTarget::OverlayBody,
        PointerLayer::Modal,
        Some(SelectableSurface::Overlay),
        None,
    ));
    capture_selectable(frame, shell, SelectableSurface::Overlay, inner, false);
}

pub(super) fn overlay_line(line: Line<'_>, selected: bool) -> Line<'_> {
    if selected {
        line.style(
            Style::default()
                .bg(theme().selected_background)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        line
    }
}
