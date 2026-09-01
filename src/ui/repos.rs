//! The Repos tab: the project's repositories on the left, and everything about
//! the one under the cursor — including what it looks like on this machine —
//! on the right.

use super::*;
use crate::app::CopiedContent;
use crate::app::repos::{RepoColumn, RepoMode, RepoRow, ReposScreen};
use crate::command::CommandId;
use crate::model::Jump;
use crate::ui::details::section_line;
use crate::ui::table::{TableSpec, render_list_table, table_geometry};

pub(crate) fn render(
    frame: &mut Frame<'_>,
    screen: &mut ReposScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, screen, shell, sections[0]);
    render_content(frame, screen, shell, sections[1]);
    render_footer(frame, screen, shell, sections[2]);
}

fn render_search(frame: &mut Frame<'_>, screen: &ReposScreen, shell: &mut Shell, area: Rect) {
    render_search_row(
        frame,
        shell,
        SearchRow {
            area,
            text: screen.query(),
            cursor: screen.query_cursor(),
            placeholder: "Type / to search, or local:cloned, local:dirty, branch:, disabled:",
            active: screen.mode == RepoMode::Search,
            pending: false,
            clearable: false,
            trailer: String::new(),
            layer: PointerLayer::Modal,
            selectable: SelectableSurface::Overlay,
        },
    );
}

fn render_content(frame: &mut Frame<'_>, screen: &mut ReposScreen, shell: &mut Shell, area: Rect) {
    struct Panes<'a>(&'a mut ReposScreen);
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
            list: "Repos",
            details: "Repository",
        },
        &mut Panes(screen),
    );
}

fn render_table(frame: &mut Frame<'_>, screen: &mut ReposScreen, shell: &mut Shell, area: Rect) {
    let rows = screen.visible(shell);
    let geometry = table_geometry(area, 1);
    screen
        .cursor
        .scroll
        .set_viewport(geometry.visible_rows, rows.len());
    let offset = screen.cursor.scroll.offset;
    let (sorted, descending) = screen.sort;
    let layout = screen.layout.clone();
    let mut cell = |index: usize, column: RepoColumn| {
        rows.get(index)
            .map_or_else(|| Cell::from(""), |row| repo_cell(row, column))
    };
    let mut spec = TableSpec {
        title: " Repos ".to_owned(),
        status: rows.len().to_string(),
        focused: shell.focus == Focus::Tickets,
        layout: &layout,
        sorted: Some((sorted, if descending { "\u{2193}" } else { "\u{2191}" })),
        count: rows.len(),
        offset,
        selected: Some(screen.cursor.index),
        row_height: 1,
        layer: PointerLayer::Base,
        scroll: ScrollSurface::Table,
        selectable: SelectableSurface::Table,
        marker: None,
        cell: &mut cell,
    };
    render_list_table(frame, shell, area, &mut spec);
}

fn repo_cell(row: &RepoRow, column: RepoColumn) -> Cell<'static> {
    // A repository the project has switched off is still on the table, faded:
    // a link naming it should still resolve.
    let plain = if row.repo.is_disabled {
        Style::default().fg(theme().muted)
    } else {
        Style::default()
    };
    match column {
        RepoColumn::Name => Cell::from(Line::styled(row.repo.name.clone(), plain)),
        RepoColumn::DefaultBranch => Cell::from(Line::styled(row.branch(), plain)),
        RepoColumn::PullRequests => {
            Cell::from(Line::styled(count_label(row.pull_requests), plain).right_aligned())
        }
        RepoColumn::Pipelines => {
            Cell::from(Line::styled(count_label(row.pipelines), plain).right_aligned())
        }
        RepoColumn::Local => Cell::from(local_line(row)),
    }
}

fn count_label(count: usize) -> String {
    if count == 0 {
        "\u{2014}".to_owned()
    } else {
        count.to_string()
    }
}

/// `main ✓` clean · `feat/x *` dirty · `main ↑2 ↓1` · `—` not here.
pub(crate) fn local_line(row: &RepoRow) -> Line<'static> {
    let Some(local) = row.local.as_ref() else {
        return Line::styled("\u{2014}", Style::default().fg(theme().muted));
    };
    if let Some(job) = local.busy {
        return Line::styled(
            format!("{} {}", spinner_frame(), job.label()),
            Style::default().fg(theme().state_in_progress),
        );
    }
    let mut spans = vec![Span::raw(local.branch.clone()), Span::raw(" ")];
    if local.dirty {
        spans.push(Span::styled("*", Style::default().fg(theme().warning)));
    } else if local.ahead == 0 && local.behind == 0 {
        spans.push(Span::styled(
            "\u{2713}",
            Style::default().fg(theme().state_completed),
        ));
    }
    if local.ahead > 0 {
        spans.push(Span::styled(
            format!("\u{2191}{}", local.ahead),
            Style::default().fg(theme().state_in_progress),
        ));
    }
    if local.behind > 0 {
        spans.push(Span::styled(
            format!(" \u{2193}{}", local.behind),
            Style::default().fg(theme().state_in_progress),
        ));
    }
    Line::from(spans)
}

fn render_details(frame: &mut Frame<'_>, screen: &mut ReposScreen, shell: &mut Shell, area: Rect) {
    let block = focused_block(" Repository ", shell.focus == Focus::Details)
        .padding(Padding::horizontal(1));
    let pane = inside_border(area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(row) = screen.selected(shell) else {
        frame.render_widget(
            Paragraph::new("Select a repository to see it here")
                .style(Style::default().fg(theme().muted)),
            inner,
        );
        return;
    };
    let jumps = screen.jumps(shell);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                row.repo.name.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            if row.repo.is_disabled {
                Span::styled("  [disabled]", Style::default().fg(theme().muted))
            } else {
                Span::raw("")
            },
        ]),
        Line::styled(row.repo.project.clone(), Style::default().fg(theme().muted)),
        Line::from(""),
        field_line("Default branch", row.branch()),
        field_line(
            "Size",
            row.repo
                .size
                .map_or_else(|| "\u{2014}".to_owned(), size_label),
        ),
        Line::from(""),
        section_line("URLs", inner.width),
    ];
    // A URL is long, wraps badly in a narrow pane, and is never read here:
    // it is wanted on the clipboard. So the section is three chips, not three
    // lines of text.
    // The chip that stands for `g`, in the header, where the pane names what
    // this row goes to.
    let follow = follow_chip(screen, shell);
    if let Some((line, _)) = follow.clone() {
        lines.insert(2, line);
    }
    let copies: [(&str, PointerTarget); 3] = [
        (
            " Copy web ",
            PointerTarget::CopyText {
                text: row.repo.web_url.clone(),
                content: CopiedContent::Url,
            },
        ),
        (
            " Copy HTTPS ",
            PointerTarget::CopyText {
                text: row.repo.remote_url.clone(),
                content: CopiedContent::Url,
            },
        ),
        (
            " Copy SSH ",
            PointerTarget::CopyText {
                text: row.repo.ssh_url.clone(),
                content: CopiedContent::Url,
            },
        ),
    ];
    let copies_index = lines.len();
    lines.push(button_row(&copies));
    lines.push(Line::from(""));
    lines.push(section_line("Local", inner.width));
    // The path stays on the pane — it is short enough to read, and it is what
    // a shell command wants next — and copies itself on a click.
    let mut path_line: Option<(usize, String)> = None;
    match row.local.as_ref() {
        Some(local) => {
            let path = local.path.display().to_string();
            path_line = Some((lines.len(), path.clone()));
            lines.push(Line::from(format!("  {path}")));
            let mut status = vec![Span::raw("  ")];
            status.extend(local_line(&row).spans);
            // Nothing here is watched, so how old the reading is matters.
            if let Some(scanned) = screen.scanned_at() {
                status.push(Span::styled(
                    format!("  read {}", crate::app::relative_age(scanned.elapsed())),
                    Style::default().fg(theme().muted),
                ));
            }
            lines.push(Line::from(status));
            // A clone claimed by its name rather than its remote says where
            // that remote actually points, because a fetch here goes there.
            if crate::local::normalise_remote(&local.origin)
                != crate::local::normalise_remote(&row.repo.remote_url)
            {
                lines.push(Line::styled(
                    format!("  origin {} \u{2014} matched by name", local.origin),
                    Style::default().fg(theme().muted),
                ));
            }
        }
        None => {
            lines.push(Line::styled(
                shell.workspace().map_or_else(
                    || "  No workspace to look in".to_owned(),
                    |workspace| format!("  Not in {}", workspace.display()),
                ),
                Style::default().fg(theme().muted),
            ));
        }
    }
    lines.push(Line::from(""));
    lines.push(section_line("Open against it", inner.width));
    let jump_start = lines.len();
    if jumps.is_empty() {
        lines.push(Line::styled(
            "  Nothing open",
            Style::default().fg(theme().muted),
        ));
    }
    for (index, (label, jump)) in jumps.iter().enumerate() {
        let what = match jump {
            Jump::PullRequest { .. } => "Pull request",
            _ => "Pipeline",
        };
        // The line the pane's cursor is on, which `Enter` follows.
        let chosen = shell.focus == Focus::Details && index == screen.jump_cursor;
        let line = Line::from(vec![
            Span::styled(format!("  {what}: "), Style::default().fg(theme().muted)),
            Span::styled(
                label.clone(),
                Style::default()
                    .fg(theme().link)
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ]);
        lines.push(if chosen {
            line.style(
                Style::default()
                    .bg(theme().selected_background)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            line
        });
    }
    lines.push(Line::from(""));
    let controls: Vec<(&str, PointerTarget)> = if row.local.is_some() {
        vec![
            ("[Fetch]", PointerTarget::RunCommand(CommandId::FetchRepo)),
            ("[Pull]", PointerTarget::RunCommand(CommandId::PullRepo)),
        ]
    } else {
        vec![("[Clone]", PointerTarget::RunCommand(CommandId::CloneRepo))]
    };
    let buttons_index = lines.len();
    lines.push(Line::from(
        controls
            .iter()
            .map(|(button, _)| {
                Span::styled(format!(" {button} "), Style::default().fg(theme().muted))
            })
            .collect::<Vec<_>>(),
    ));
    // A URL wraps in a pane this narrow, so every target below it is placed
    // by the row its line landed on rather than by the line's index.
    let (rows, total) = wrapped_rows(&lines, inner.width);
    screen
        .details
        .set_viewport(usize::from(inner.height), total);
    let offset = screen.details.offset;
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0)),
        inner,
    );
    // The pane itself: the wheel scrolls it, a click gives it the focus.
    shell.hit_regions.push(region(
        pane,
        PointerTarget::FocusDetails,
        PointerLayer::Base,
        Some(SelectableSurface::Details),
        Some(ScrollSurface::Details),
    ));
    if total > usize::from(inner.height) {
        render_scrollbar(
            frame,
            PointerLayer::Base,
            shell,
            pane,
            ScrollSurface::Details,
            ScrollState {
                offset,
                content: total,
                viewport: usize::from(inner.height),
            },
        );
    }

    // The chips that copy the URLs, and the path, which copies itself. The
    // path's target is only as wide as the text, so the pointer reverses the
    // path rather than the row it sits on.
    if let Some((_, jump)) = follow
        && let Some(y) = row_on_screen(inner, &rows, 2, offset)
    {
        shell.hit_regions.push(region(
            Rect::new(inner.x, y, inner.width, 1),
            PointerTarget::Follow(jump),
            PointerLayer::Base,
            None,
            None,
        ));
    }
    if let Some(y) = row_on_screen(inner, &rows, copies_index, offset) {
        register_buttons(shell, inner, y, PointerLayer::Base, &copies);
    }
    if let Some((index, path)) = path_line
        && let Some(y) = row_on_screen(inner, &rows, index, offset)
    {
        let x = inner.x.saturating_add(2);
        let width = u16::try_from(path.chars().count())
            .unwrap_or(u16::MAX)
            .min(inner.width.saturating_sub(2));
        shell.hit_regions.push(region(
            Rect::new(x, y, width, 1),
            PointerTarget::CopyText {
                text: path,
                content: CopiedContent::Path,
            },
            PointerLayer::Base,
            None,
            None,
        ));
    }
    for (index, (_, jump)) in jumps.iter().enumerate() {
        if let Some(y) = row_on_screen(inner, &rows, jump_start + index, offset) {
            shell.hit_regions.push(region(
                Rect::new(inner.x, y, inner.width, 1),
                PointerTarget::Follow(jump.clone()),
                PointerLayer::Base,
                None,
                None,
            ));
        }
    }
    // The buttons stand for the keys they name, so clicking one is the key.
    if let Some(y) = row_on_screen(inner, &rows, buttons_index, offset) {
        register_buttons(shell, inner, y, PointerLayer::Base, &controls);
    }
}

/// Bytes as the API reports them, in the units a person reads. Shared with
/// the ACR tab, whose manifests are counted the same way.
pub(crate) fn size_label(bytes: i64) -> String {
    let bytes = bytes.max(0);
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{} kB", bytes / 1024),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

fn render_footer(frame: &mut Frame<'_>, screen: &ReposScreen, shell: &Shell, area: Rect) {
    render_screen_status_bar(frame, screen, shell, area);
}
