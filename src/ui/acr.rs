//! The ACR tab: the subscription's registries, or the repositories of the one
//! chosen, on the left; and what the details pane says about the row under the
//! cursor on the right — a registry's fields, or a repository's tags and the
//! manifest the tag cursor is on.

use super::*;
use crate::app::acr::rows::short_digest;
use crate::app::acr::{
    AcrMode, AcrScreen, Level, RegistryColumn, RegistryRow, RepositoryColumn, RepositoryRow,
};
use crate::arm::Tag;
use crate::command::CommandId;
use crate::ui::details::section_line;
use crate::ui::pipelines::relative_age;
use crate::ui::repos::size_label;
use crate::ui::table::{TableSpec, render_list_table, table_geometry};

/// The whole tab: the search box, the table, the details pane and the footer.
pub(crate) fn render(frame: &mut Frame<'_>, screen: &mut AcrScreen, shell: &mut Shell, area: Rect) {
    // Which repository the details pane is on is settled here, on the way to
    // drawing it, so the worker is asked for the tags of whatever is on screen.
    screen.sync_focus();
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, screen, shell, sections[0]);
    render_content(frame, screen, shell, sections[1]);
    render_screen_status_bar(frame, screen, shell, sections[2]);
}

fn render_search(frame: &mut Frame<'_>, screen: &AcrScreen, shell: &mut Shell, area: Rect) {
    let (placeholder, trailer) = match screen.level() {
        Level::Registries => (
            "Type / to search registries, or rg:, sku:, location:",
            String::new(),
        ),
        Level::Repositories(name) => (
            "Type / to search repositories, or name:",
            format!("\u{2022} {name}"),
        ),
    };
    render_search_row(
        frame,
        shell,
        SearchRow {
            area,
            text: screen.query(),
            cursor: screen.query_cursor(),
            placeholder,
            active: screen.mode == AcrMode::Search,
            pending: false,
            clearable: false,
            trailer,
            layer: PointerLayer::Modal,
            selectable: SelectableSurface::Overlay,
        },
    );
}

fn render_content(frame: &mut Frame<'_>, screen: &mut AcrScreen, shell: &mut Shell, area: Rect) {
    struct Panes<'a>(&'a mut AcrScreen);
    impl PanePair for Panes<'_> {
        fn first(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            match self.0.level() {
                Level::Registries => render_registry_table(frame, self.0, shell, area),
                Level::Repositories(_) => render_repository_table(frame, self.0, shell, area),
            }
        }

        fn second(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            render_details(frame, self.0, shell, area);
        }
    }
    // The list is whichever level is open, and the chips the narrow layout
    // wears say which.
    let list = match screen.level() {
        Level::Registries => "Registries".to_owned(),
        Level::Repositories(name) => name.clone(),
    };
    let details = if screen.selected_repository().is_some() {
        "Repository"
    } else {
        "Registry"
    };
    render_workspace(
        frame,
        shell,
        area,
        &PaneNames {
            list: &list,
            details,
        },
        &mut Panes(screen),
    );
}

fn render_registry_table(
    frame: &mut Frame<'_>,
    screen: &mut AcrScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let rows = screen.visible_registries();
    let geometry = table_geometry(area, 1);
    screen
        .registry_cursor
        .scroll
        .set_viewport(geometry.visible_rows, rows.len());
    let offset = screen.registry_cursor.scroll.offset;
    let (sorted, descending) = screen.registry_sort;
    let layout = screen.registries_layout.clone();
    let status = format!("{} registries", rows.len());
    let mut cell = |index: usize, column: RegistryColumn| {
        rows.get(index)
            .map_or_else(|| Cell::from(""), |row| registry_cell(row, column))
    };
    let mut spec = TableSpec {
        title: " Registries ".to_owned(),
        status,
        focused: shell.focus == Focus::Tickets,
        layout: &layout,
        sorted: Some((sorted, if descending { "\u{2193}" } else { "\u{2191}" })),
        count: rows.len(),
        offset,
        selected: Some(screen.registry_cursor.index),
        row_height: 1,
        layer: PointerLayer::Base,
        scroll: ScrollSurface::Table,
        selectable: SelectableSurface::Table,
        marker: None,
        cell: &mut cell,
    };
    render_list_table(frame, shell, area, &mut spec);
}

fn render_repository_table(
    frame: &mut Frame<'_>,
    screen: &mut AcrScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let now = Timestamp::now();
    let rows = screen.visible_repositories();
    let geometry = table_geometry(area, 1);
    screen
        .repository_cursor
        .scroll
        .set_viewport(geometry.visible_rows, rows.len());
    let offset = screen.repository_cursor.scroll.offset;
    let (sorted, descending) = screen.repository_sort;
    let layout = screen.repositories_layout.clone();
    let title = screen.open_registry().map_or_else(
        || " Repositories ".to_owned(),
        |registry| format!(" {} ", registry.name),
    );
    let mut cell = |index: usize, column: RepositoryColumn| {
        rows.get(index)
            .map_or_else(|| Cell::from(""), |row| repository_cell(row, column, now))
    };
    let mut spec = TableSpec {
        title,
        status: repository_status(screen, rows.len()),
        focused: shell.focus == Focus::Tickets,
        layout: &layout,
        sorted: Some((sorted, if descending { "\u{2193}" } else { "\u{2191}" })),
        count: rows.len(),
        offset,
        selected: Some(screen.repository_cursor.index),
        row_height: 1,
        layer: PointerLayer::Base,
        scroll: ScrollSurface::Table,
        selectable: SelectableSurface::Table,
        marker: None,
        cell: &mut cell,
    };
    render_list_table(frame, shell, area, &mut spec);
}

/// What the bottom border says: how many repositories, and how far through the
/// attributes calls the worker has got while they are still landing.
fn repository_status(screen: &AcrScreen, matching: usize) -> String {
    let (read, total) = screen.attributes_read();
    if read < total {
        format!("{matching} repositories \u{00b7} {read} of {total} read")
    } else {
        format!("{matching} repositories")
    }
}

fn registry_cell(row: &RegistryRow, column: RegistryColumn) -> Cell<'static> {
    match column {
        RegistryColumn::Name => Cell::from(row.registry.name.clone()),
        RegistryColumn::ResourceGroup => Cell::from(row.registry.resource_group.clone()),
        RegistryColumn::Sku => Cell::from(row.registry.sku.clone()),
        RegistryColumn::Location => Cell::from(row.registry.location.clone()),
        RegistryColumn::LoginServer => Cell::from(row.registry.login_server.clone()),
    }
}

fn repository_cell(row: &RepositoryRow, column: RepositoryColumn, now: Timestamp) -> Cell<'static> {
    match column {
        RepositoryColumn::Name => Cell::from(row.repository.name.clone()),
        // A count that has not landed yet is a dash rather than a nought: the
        // repository has tags, nobody has asked how many.
        RepositoryColumn::Tags => Cell::from(
            Line::styled(
                row.repository
                    .tags
                    .map_or_else(|| "\u{2014}".to_owned(), |count| count.to_string()),
                if row.repository.tags.is_some() {
                    Style::default()
                } else {
                    Style::default().fg(theme().muted)
                },
            )
            .right_aligned(),
        ),
        RepositoryColumn::Updated => Cell::from(
            row.repository
                .updated
                .map_or_else(String::new, |at| stamp_label(at, now)),
        ),
    }
}

/// `2026-08-29 09:00:00 UTC (2h)`, the way every other pane writes an instant.
fn stamp_label(at: Timestamp, now: Timestamp) -> String {
    format!("{} ({})", at.exact_utc(), relative_age(at, now))
}

fn render_details(frame: &mut Frame<'_>, screen: &mut AcrScreen, shell: &mut Shell, area: Rect) {
    match screen.selected_repository() {
        Some(row) => render_repository_details(frame, screen, shell, area, &row),
        None => render_registry_details(frame, screen, shell, area),
    }
}

/// The registry under the cursor: what it is, where it is, and the way into it
/// in the portal.
fn render_registry_details(
    frame: &mut Frame<'_>,
    screen: &AcrScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let block =
        focused_block(" Registry ", shell.focus == Focus::Details).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(row) = screen.selected_registry() else {
        frame.render_widget(
            Paragraph::new(nothing_selected(screen, shell))
                .style(Style::default().fg(theme().muted))
                .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    };
    let mut lines = vec![
        Line::styled(
            row.registry.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            row.registry.login_server.clone(),
            Style::default().fg(theme().accent),
        ),
        Line::from(""),
        field_line("Group", row.registry.resource_group.clone()),
        field_line("Location", row.registry.location.clone()),
        field_line("SKU", row.registry.sku.clone()),
        field_line(
            "Repos",
            row.repositories
                .map_or_else(|| "\u{2014}".to_owned(), |count| count.to_string()),
        ),
        Line::from(""),
    ];
    let portal = lines.len();
    lines.push(Line::from(vec![
        Span::styled("Portal: ", Style::default().fg(theme().muted)),
        Span::styled(
            portal_link(&row),
            Style::default()
                .fg(theme().link)
                .add_modifier(Modifier::UNDERLINED),
        ),
    ]));
    if let Some(error) = screen.arm_error() {
        lines.push(Line::from(""));
        lines.push(section_line("Problem", inner.width));
        lines.push(Line::styled(
            error.to_owned(),
            Style::default().fg(theme().error),
        ));
    }
    // A portal link wraps, so its target is placed by the row its line landed
    // on rather than by the line's index.
    let (rows, _) = wrapped_rows(&lines, inner.width);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    if let Some(y) = row_on_screen(inner, &rows, portal, 0) {
        shell.hit_regions.push(region(
            Rect::new(inner.x, y, inner.width, 1),
            PointerTarget::RunCommand(CommandId::Open),
            PointerLayer::Base,
            None,
            None,
        ));
    }
    capture_selectable(frame, shell, SelectableSurface::Details, inner, true);
}

fn portal_link(row: &RegistryRow) -> String {
    crate::arm::portal_url(&row.registry.id)
}

/// The repository under the cursor: its counts, the chips that copy and open
/// it, its tags newest first, and what the tag under the pane's own cursor
/// points at.
fn render_repository_details(
    frame: &mut Frame<'_>,
    screen: &AcrScreen,
    shell: &mut Shell,
    area: Rect,
    row: &RepositoryRow,
) {
    let now = Timestamp::now();
    let focused = shell.focus == Focus::Details;
    let block = focused_block(" Repository ", focused).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let server = screen
        .selected_registry()
        .map_or_else(String::new, |registry| registry.registry.login_server);
    let tags = screen.shown_tags();
    let cursor = screen.tag_cursor();
    let mut lines = vec![
        Line::styled(
            row.repository.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!("{server}/{}", row.repository.name),
            Style::default().fg(theme().accent),
        ),
        Line::from(""),
        field_line("Tags", count_label(row.repository.tags)),
        field_line("Manifests", count_label(row.repository.manifests)),
        field_line(
            "Updated",
            row.repository
                .updated
                .map_or_else(|| "\u{2014}".to_owned(), |at| stamp_label(at, now)),
        ),
        Line::from(""),
    ];
    // The chip that stands for `g`, in the header, where the pane names what
    // this row goes to.
    let follow = follow_chip(screen, shell);
    if let Some((line, _)) = follow.clone() {
        lines.insert(2, line);
    }
    let buttons: [(&str, PointerTarget); 3] = [
        (" Copy pull ", PointerTarget::RunCommand(CommandId::CopyId)),
        (
            " Copy digest ",
            PointerTarget::RunCommand(CommandId::CopyDigest),
        ),
        (" Open ", PointerTarget::RunCommand(CommandId::Open)),
    ];
    let buttons_index = lines.len();
    lines.push(button_row(&buttons));
    lines.push(Line::from(""));
    lines.push(section_line("Tags", inner.width));
    let tags_start = lines.len();
    if tags.is_empty() {
        lines.push(Line::styled(
            "  Reading\u{2026}",
            Style::default().fg(theme().muted),
        ));
    }
    for (index, tag) in tags.iter().enumerate() {
        let line = tag_line(tag, screen, &row.repository.name, now);
        lines.push(if index == cursor && focused {
            line.style(Style::default().bg(theme().selected_background))
        } else {
            line
        });
    }
    if let Some(manifest) = screen.shown_manifest() {
        lines.push(Line::from(""));
        lines.push(section_line("Manifest", inner.width));
        lines.push(field_line(
            "Created",
            manifest
                .created
                .map_or_else(|| "\u{2014}".to_owned(), |at| stamp_label(at, now)),
        ));
        lines.push(field_line("Platform", platform_label(manifest)));
        lines.push(field_line(
            "Size",
            manifest.size.map_or_else(
                || "\u{2014}".to_owned(),
                |bytes| size_label(i64::try_from(bytes).unwrap_or(i64::MAX)),
            ),
        ));
    }
    if let Some(error) = screen.arm_error() {
        lines.push(Line::from(""));
        lines.push(section_line("Problem", inner.width));
        lines.push(Line::styled(
            error.to_owned(),
            Style::default().fg(theme().error),
        ));
    }
    // A long repository name or a refusal wraps, so every target is placed by
    // the row its line landed on rather than by the line's index.
    let (rows, _) = wrapped_rows(&lines, inner.width);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    if let Some((_, jump)) = follow
        && let Some(y) = row_on_screen(inner, &rows, 2, 0)
    {
        shell.hit_regions.push(region(
            Rect::new(inner.x, y, inner.width, 1),
            PointerTarget::Follow(jump),
            PointerLayer::Base,
            None,
            None,
        ));
    }
    if let Some(y) = row_on_screen(inner, &rows, buttons_index, 0) {
        register_buttons(shell, inner, y, PointerLayer::Base, &buttons);
    }
    // One hit region per tag, so a click picks the tag the manifest is read
    // for.
    for index in 0..tags.len() {
        if let Some(y) = row_on_screen(inner, &rows, tags_start + index, 0) {
            shell.hit_regions.push(region(
                Rect::new(inner.x, y, inner.width, 1),
                PointerTarget::TreeRow { index },
                PointerLayer::Base,
                None,
                None,
            ));
        }
    }
    capture_selectable(frame, shell, SelectableSurface::Details, inner, true);
}

/// One tag: what it is called, the head of its digest, when it was made, and
/// what it weighs once the manifest has been read.
fn tag_line(tag: &Tag, screen: &AcrScreen, repo: &str, now: Timestamp) -> Line<'static> {
    let registry = screen
        .open_registry()
        .map_or_else(String::new, |registry| registry.name.clone());
    let size = screen
        .manifest(&registry, repo, &tag.digest)
        .and_then(|manifest| manifest.size)
        .map(|bytes| size_label(i64::try_from(bytes).unwrap_or(i64::MAX)));
    let mut spans = vec![
        Span::raw(format!("  {}  ", tag.name)),
        Span::styled(
            short_digest(&tag.digest),
            Style::default().fg(theme().muted),
        ),
    ];
    if let Some(created) = tag.created {
        spans.push(Span::styled(
            format!("  {}", relative_age(created, now)),
            Style::default().fg(theme().muted),
        ));
    }
    if let Some(size) = size {
        spans.push(Span::styled(
            format!("  {size}"),
            Style::default().fg(theme().muted),
        ));
    }
    Line::from(spans)
}

/// `linux/amd64`, or a dash for a manifest that named neither.
pub(crate) fn platform_label(manifest: &crate::arm::Manifest) -> String {
    match (manifest.os.as_str(), manifest.architecture.as_str()) {
        ("", "") => "\u{2014}".to_owned(),
        (os, "") => os.to_owned(),
        ("", architecture) => architecture.to_owned(),
        (os, architecture) => format!("{os}/{architecture}"),
    }
}

pub(crate) fn count_label(count: Option<u64>) -> String {
    count.map_or_else(|| "\u{2014}".to_owned(), |count| count.to_string())
}

/// What the pane says with no registry under the cursor: why ARM cannot be
/// reached, what refused, or that nothing has come back yet.
fn nothing_selected(screen: &AcrScreen, shell: &Shell) -> Vec<Line<'static>> {
    if let Some(reason) = shell.arm_state() {
        return vec![Line::from(reason.to_owned())];
    }
    if let Some(error) = screen.arm_error() {
        return vec![Line::from(error.to_owned())];
    }
    vec![Line::from("Reading the subscription\u{2026}")]
}
