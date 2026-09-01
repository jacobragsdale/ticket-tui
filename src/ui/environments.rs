//! The Environments tab: every service across every environment on the left,
//! and the promotion into the environment under the column cursor on the
//! right.
//!
//! The board is the one table in the app whose columns are not a fixed set:
//! there is one per `[[environments]]`, named out of `config.toml`, so it lays
//! itself out here rather than going through [`crate::columns::TableLayout`],
//! which can only name columns a build knows. The two columns to their left —
//! Service and Namespace — are ordinary ones and the Columns overlay edits
//! them.

use super::*;
use ratatui::widgets::TableState;

use crate::app::environments::{
    DiffLine, DiffLineKind, EnvCell, EnvColumn, EnvMode, EnvironmentsScreen,
};
use crate::columns::{COLUMN_SPACING, ColumnId, SCROLLBAR_WIDTH, SELECTION_WIDTH, TableLayout};
use crate::command::CommandId;
use crate::ui::table::table_geometry;

/// What one environment's column is given. Wide enough for a tag and both
/// counts — `1.4.0 ✗2 ◇1` — and no wider: the services are what the eye reads
/// down.
const ENVIRONMENT_WIDTH: u16 = 16;

/// The whole tab: the search box, the `Findings` chip, the board, the details
/// pane and the footer.
pub(crate) fn render(
    frame: &mut Frame<'_>,
    screen: &mut EnvironmentsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, screen, shell, sections[0]);
    render_filter_bar(frame, screen, shell, sections[1]);
    render_content(frame, screen, shell, sections[2]);
    render_screen_status_bar(frame, screen, shell, sections[3]);
}

fn render_search(
    frame: &mut Frame<'_>,
    screen: &EnvironmentsScreen,
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
            placeholder: "Type / to search services, or ns:, findings:yes",
            active: screen.mode == EnvMode::Search,
            pending: false,
            clearable: false,
            trailer: String::new(),
            layer: PointerLayer::Modal,
            selectable: SelectableSurface::Overlay,
        },
    );
}

/// The board's filter bar: one chip, because there is one question — which
/// services something is missing from.
fn render_filter_bar(
    frame: &mut Frame<'_>,
    screen: &EnvironmentsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let label = " Findings ";
    let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            label,
            pill_style(false, screen.findings_only()),
        ))),
        area,
    );
    shell.hit_regions.push(region(
        Rect::new(area.x, area.y, width.min(area.width), 1),
        PointerTarget::RunCommand(CommandId::ToggleFindings),
        PointerLayer::Base,
        None,
        None,
    ));
}

fn render_content(
    frame: &mut Frame<'_>,
    screen: &mut EnvironmentsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    struct Panes<'a>(&'a mut EnvironmentsScreen);
    impl PanePair for Panes<'_> {
        fn first(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            render_board(frame, self.0, shell, area);
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
            list: "Environments",
            details: "Promotion",
        },
        &mut Panes(screen),
    );
}

/// The board: one row per service, one column per environment, and a cell
/// each saying what that environment runs and what it is short of.
fn render_board(
    frame: &mut Frame<'_>,
    screen: &mut EnvironmentsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let rows = screen.visible();
    let environments: Vec<String> = screen
        .environments()
        .iter()
        .map(|environment| environment.name.clone())
        .collect();
    let focused = shell.focus == Focus::Tickets;
    let block = focused_block(" Environments ", focused)
        .padding(Padding::right(SCROLLBAR_WIDTH))
        .title_bottom(Line::from(format!(" {} services ", rows.len())));
    let geometry = table_geometry(area, 1);
    let inner = geometry.inner;
    if inner.width == 0 || inner.height == 0 {
        frame.render_widget(block, area);
        return;
    }
    // Nothing to draw at all is the one line saying why, and where it looked.
    if let Some(reason) = screen.reason() {
        let text = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(reason.to_owned())
                .style(Style::default().fg(theme().muted))
                .wrap(Wrap { trim: false }),
            text,
        );
        return;
    }
    screen
        .cursor
        .scroll
        .set_viewport(geometry.visible_rows, rows.len());
    let offset = screen.cursor.scroll.offset;
    let column = screen.column();
    let layout = screen.layout.clone();
    let fixed = layout.visible_columns(fixed_width(inner.width, environments.len()));
    let constraints = constraints(&fixed, environments.len());

    let mut header: Vec<Cell<'static>> = fixed
        .iter()
        .map(|held| Cell::from(Line::styled(held.id.label(), header_style(false))))
        .collect();
    header.extend(environments.iter().enumerate().map(|(index, name)| {
        Cell::from(Line::styled(name.clone(), header_style(index == column)))
    }));

    let body: Vec<Row<'static>> = rows
        .iter()
        .skip(offset)
        .take(geometry.visible_rows)
        .map(|row| {
            let mut cells: Vec<Cell<'static>> = fixed
                .iter()
                .map(|held| match held.id {
                    EnvColumn::Service => Cell::from(row.workload.clone()),
                    EnvColumn::Namespace => Cell::from(Line::styled(
                        row.namespace.clone(),
                        Style::default().fg(theme().muted),
                    )),
                })
                .collect();
            cells.extend(
                row.cells
                    .iter()
                    .enumerate()
                    .map(|(index, cell)| environment_cell(cell, index == column)),
            );
            Row::new(cells)
        })
        .collect();

    let table = Table::new(body, constraints.clone())
        .header(Row::new(header).height(1).bottom_margin(1))
        .block(block)
        .column_spacing(COLUMN_SPACING)
        .row_highlight_style(
            Style::default()
                .bg(theme().selected_background)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(Line::styled(
            "\u{203a} ",
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        ))
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = TableState::default();
    state.select(
        screen
            .cursor
            .index
            .checked_sub(offset)
            .filter(|selected| *selected < geometry.visible_rows),
    );
    frame.render_stateful_widget(table, area, &mut state);
    if inner.height < 2 {
        return;
    }
    // The same rule the shared table draws under its header, so the column
    // names read as a heading rather than as a first row.
    frame.render_widget(
        Line::styled(
            BorderType::border_symbols(theme().border_type)
                .horizontal_top
                .repeat(usize::from(inner.width)),
            Style::default().fg(theme().border),
        ),
        Rect::new(inner.x, inner.y.saturating_add(1), inner.width, 1),
    );

    // The columns as the table laid them out, so a click lands on the cell the
    // eye is on.
    let areas = Layout::horizontal(constraints)
        .spacing(COLUMN_SPACING)
        .split(columns_area(inner));
    let body = geometry.body;
    shell.hit_regions.push(region(
        body,
        PointerTarget::FocusTickets,
        PointerLayer::Base,
        Some(SelectableSurface::Table),
        Some(ScrollSurface::Table),
    ));
    let rendered = rows.len().saturating_sub(offset).min(geometry.visible_rows);
    for visible in 0..rendered {
        let index = offset + visible;
        let y = body
            .y
            .saturating_add(u16::try_from(visible).unwrap_or(u16::MAX));
        if y >= body.y.saturating_add(body.height) {
            break;
        }
        shell.hit_regions.push(region(
            Rect::new(body.x, y, body.width.saturating_sub(1), 1),
            PointerTarget::TableRow { index },
            PointerLayer::Base,
            Some(SelectableSurface::Table),
            Some(ScrollSurface::Table),
        ));
        for (column, cell) in areas.iter().skip(fixed.len()).enumerate() {
            shell.hit_regions.push(region(
                Rect::new(cell.x, y, cell.width, 1),
                PointerTarget::TableCell { row: index, column },
                PointerLayer::Base,
                None,
                None,
            ));
        }
    }
    let overflow = rows.len() > geometry.visible_rows;
    if overflow {
        render_scrollbar(
            frame,
            PointerLayer::Base,
            shell,
            body,
            ScrollSurface::Table,
            screen.cursor.scroll,
        );
    }
    capture_selectable(frame, shell, SelectableSurface::Table, body, overflow);
}

/// Where the table lays its columns out: inside the border, past the selection
/// marker, and short of the scrollbar's own column.
fn columns_area(inner: Rect) -> Rect {
    Rect::new(
        inner.x.saturating_add(SELECTION_WIDTH),
        inner.y,
        inner
            .width
            .saturating_sub(SELECTION_WIDTH)
            .saturating_sub(SCROLLBAR_WIDTH),
        inner.height,
    )
}

/// The width the two fixed columns share, once the environments have had
/// theirs.
fn fixed_width(inner_width: u16, environments: usize) -> u16 {
    let taken = u16::try_from(environments)
        .unwrap_or(u16::MAX)
        .saturating_mul(ENVIRONMENT_WIDTH.saturating_add(COLUMN_SPACING));
    inner_width
        .saturating_sub(SELECTION_WIDTH)
        .saturating_sub(SCROLLBAR_WIDTH)
        .saturating_sub(taken)
}

fn constraints(
    fixed: &[crate::columns::ColumnConfig<EnvColumn>],
    environments: usize,
) -> Vec<Constraint> {
    let mut constraints: Vec<Constraint> =
        fixed.iter().copied().map(TableLayout::constraint).collect();
    constraints.extend(std::iter::repeat_n(
        Constraint::Length(ENVIRONMENT_WIDTH),
        environments,
    ));
    constraints
}

/// The column the cursor is on wears the accent, so the board says which
/// promotion the details pane is reading.
fn header_style(current: bool) -> Style {
    if current {
        Style::default()
            .fg(theme().accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme().header)
    }
}

/// One cell: the tag, and what that environment would be short of. Coloured on
/// the state palette — clean, findings, or never rendered.
fn environment_cell(cell: &EnvCell, current: bool) -> Cell<'static> {
    let Some(tag) = cell.tag.clone() else {
        let (text, color) = if cell.rendered {
            ("\u{2014}", theme().muted)
        } else {
            ("?", theme().warning)
        };
        return Cell::from(Line::styled(text, Style::default().fg(color)));
    };
    let mut spans = vec![Span::styled(
        tag,
        Style::default().fg(if cell.clean() {
            theme().state_completed
        } else {
            theme().text
        }),
    )];
    if cell.findings > 0 {
        spans.push(Span::styled(
            format!(" \u{2717}{}", cell.findings),
            Style::default()
                .fg(theme().error)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if cell.expiring > 0 {
        spans.push(Span::styled(
            format!(" \u{25c7}{}", cell.expiring),
            Style::default().fg(theme().warning),
        ));
    }
    let line = Line::from(spans);
    Cell::from(if current {
        line.style(Style::default().add_modifier(Modifier::BOLD))
    } else {
        line
    })
}

/// The details pane: what the environment under the column cursor is missing,
/// and the promotion into it from the environment to its left.
fn render_details(
    frame: &mut Frame<'_>,
    screen: &mut EnvironmentsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let focused = shell.focus == Focus::Details;
    let block = focused_block(format!(" {} ", screen.promotion_label()), focused)
        .padding(Padding::horizontal(1));
    let pane = inside_border(area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    shell.hit_regions.push(region(
        pane,
        PointerTarget::FocusDetails,
        PointerLayer::Base,
        Some(SelectableSurface::Details),
        Some(ScrollSurface::Details),
    ));
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let document = screen.detail_lines(shell);
    if document.is_empty() {
        frame.render_widget(
            Paragraph::new(nothing_selected(screen))
                .style(Style::default().fg(theme().muted))
                .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }
    // Where every jumpable line landed, so a click follows the line the eye is
    // on rather than the one its index would be without wrapping.
    let mut jumps: Vec<(usize, Jump)> = Vec::new();
    let mut lines: Vec<Line<'static>> = Vec::new();
    for entry in &document {
        if let Some(jump) = entry.jump.clone() {
            jumps.push((lines.len(), jump));
        }
        lines.push(detail_line(
            entry,
            inner.width,
            focused,
            screen.jump_cursor,
            jumps.len(),
        ));
    }
    let (rows, height) = wrapped_rows(&lines, inner.width);
    screen
        .details
        .set_viewport(usize::from(inner.height), height);
    let offset = screen.details.offset;
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0)),
        inner,
    );
    for (index, jump) in jumps {
        if let Some(y) = row_on_screen(inner, &rows, index, offset) {
            shell.hit_regions.push(region(
                Rect::new(inner.x, y, inner.width, 1),
                PointerTarget::Follow(jump),
                PointerLayer::Base,
                Some(SelectableSurface::Details),
                Some(ScrollSurface::Details),
            ));
        }
    }
    let overflow = height > usize::from(inner.height);
    if overflow {
        render_scrollbar(
            frame,
            PointerLayer::Base,
            shell,
            pane,
            ScrollSurface::Details,
            screen.details,
        );
    }
    capture_selectable(frame, shell, SelectableSurface::Details, inner, overflow);
}

/// One line of the pane, painted by what it is. A line that goes somewhere is
/// underlined the way every other reference in the app is, and the one the
/// pane's own cursor is on wears the cursor's ground.
fn detail_line(
    entry: &DiffLine,
    width: u16,
    focused: bool,
    cursor: usize,
    jumped: usize,
) -> Line<'static> {
    match entry.kind {
        DiffLineKind::Section => super::details::section_line(&entry.text, width),
        DiffLineKind::Note => Line::styled(
            format!("  {}", entry.text),
            Style::default().fg(theme().muted),
        ),
        DiffLineKind::Missing | DiffLineKind::Entry | DiffLineKind::Pod => {
            let glyph = if entry.kind == DiffLineKind::Missing {
                "\u{2717}"
            } else {
                "\u{00b7}"
            };
            let colour = if entry.kind == DiffLineKind::Missing {
                theme().error
            } else {
                theme().muted
            };
            let text = if entry.jump.is_some() {
                Style::default()
                    .fg(theme().link)
                    .add_modifier(Modifier::UNDERLINED)
            } else {
                Style::default()
            };
            let line = Line::from(vec![
                Span::styled(format!("  {glyph} "), Style::default().fg(colour)),
                Span::styled(entry.text.clone(), text),
            ]);
            // `jumped` counts the jumpable lines written so far, this one
            // included, so the cursor's line is the one that took it past.
            if focused && entry.jump.is_some() && jumped == cursor + 1 {
                line.style(
                    Style::default()
                        .bg(theme().selected_background)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                line
            }
        }
    }
}

/// What the pane says with nothing to compare.
fn nothing_selected(screen: &EnvironmentsScreen) -> Vec<Line<'static>> {
    if let Some(reason) = screen.reason() {
        return vec![Line::from(reason.to_owned())];
    }
    if screen.environments().is_empty() {
        return vec![Line::from("No [[environments]] in config.toml".to_owned())];
    }
    if screen.busy() {
        return vec![Line::from("Rendering the overlays\u{2026}".to_owned())];
    }
    let refused: Vec<Line<'static>> = screen
        .environments()
        .iter()
        .filter_map(|environment| {
            let error = screen.error(&environment.name)?;
            Some(Line::styled(
                format!("{}: {error}", environment.name),
                Style::default().fg(theme().error),
            ))
        })
        .collect();
    if refused.is_empty() {
        vec![Line::from("No service is selected".to_owned())]
    } else {
        refused
    }
}
