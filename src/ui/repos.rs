//! The Repos tab: the project's repositories on the left, and everything about
//! the one under the cursor — including what it looks like on this machine —
//! on the right.

use super::*;
use crate::app::repos::{RepoColumn, RepoRow, ReposScreen};
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
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, screen, shell, sections[0]);
    render_content(frame, screen, shell, sections[1]);
    render_footer(frame, screen, shell, sections[2]);
}

fn render_search(frame: &mut Frame<'_>, screen: &ReposScreen, shell: &mut Shell, area: Rect) {
    let block = focused_block(" Search / ", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    render_query_field(
        frame,
        shell,
        inner,
        screen.query(),
        screen.query_cursor(),
        "Type / to search, or local:cloned, local:dirty, branch:, disabled:",
        PointerTarget::SearchField,
    );
}

fn render_content(frame: &mut Frame<'_>, screen: &mut ReposScreen, shell: &mut Shell, area: Rect) {
    let split = if area.width >= WIDE_BREAKPOINT {
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area)
    } else {
        Layout::horizontal([Constraint::Fill(1)]).split(area)
    };
    render_table(frame, screen, shell, split[0]);
    if split.len() > 1 {
        render_details(frame, screen, shell, split[1]);
    }
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
        title: format!(" Repos {} ", rows.len()),
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

/// The glyph turns four times a second while git works, so a clone that takes
/// a minute is visibly alive rather than merely stuck.
fn spinner() -> char {
    const FRAMES: [char; 4] = ['\u{25d0}', '\u{25d3}', '\u{25d1}', '\u{25d2}'];
    let ticks = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() / 250);
    FRAMES[usize::try_from(ticks % 4).unwrap_or(0)]
}

/// `main ✓` clean · `feat/x *` dirty · `main ↑2 ↓1` · `—` not here.
pub(crate) fn local_line(row: &RepoRow) -> Line<'static> {
    let Some(local) = row.local.as_ref() else {
        return Line::styled("\u{2014}", Style::default().fg(theme().muted));
    };
    if let Some(job) = local.busy {
        return Line::styled(
            format!("{} {}", spinner(), job.label()),
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
    let block = focused_block(" Repository ", shell.focus == Focus::Details);
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
        section_line("URLs"),
    ];
    let url_start = lines.len();
    let urls = [
        ("Web", row.repo.web_url.clone()),
        ("HTTPS", row.repo.remote_url.clone()),
        ("SSH", row.repo.ssh_url.clone()),
    ];
    for (label, url) in &urls {
        lines.push(Line::from(vec![
            Span::styled(format!("  {label:<6}"), Style::default().fg(theme().muted)),
            Span::styled(url.clone(), Style::default().fg(theme().link)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(section_line("Local"));
    match row.local.as_ref() {
        Some(local) => {
            lines.push(Line::from(format!("  {}", local.path.display())));
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
    lines.push(section_line("Open against it"));
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
    let controls: Vec<(&str, CommandId)> = if row.local.is_some() {
        vec![
            ("[Fetch]", CommandId::FetchRepo),
            ("[Pull]", CommandId::PullRepo),
        ]
    } else {
        vec![("[Clone]", CommandId::CloneRepo)]
    };
    let buttons_row = inner.y + u16::try_from(lines.len()).unwrap_or(u16::MAX);
    lines.push(Line::from(
        controls
            .iter()
            .map(|(button, _)| {
                Span::styled(format!(" {button} "), Style::default().fg(theme().muted))
            })
            .collect::<Vec<_>>(),
    ));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);

    // Every URL line copies what it says.
    for (index, (_, url)) in urls.iter().enumerate() {
        let y = inner
            .y
            .saturating_add(u16::try_from(url_start + index).unwrap_or(u16::MAX));
        if y >= inner.y.saturating_add(inner.height) {
            break;
        }
        shell.hit_regions.push(region(
            Rect::new(inner.x, y, inner.width, 1),
            PointerTarget::CopyText(url.clone()),
            PointerLayer::Base,
            None,
            None,
        ));
    }
    for (index, (_, jump)) in jumps.iter().enumerate() {
        let y = inner
            .y
            .saturating_add(u16::try_from(jump_start + index).unwrap_or(u16::MAX));
        if y >= inner.y.saturating_add(inner.height) {
            break;
        }
        shell.hit_regions.push(region(
            Rect::new(inner.x, y, inner.width, 1),
            PointerTarget::Follow(jump.clone()),
            PointerLayer::Base,
            None,
            None,
        ));
    }
    // The buttons stand for the keys they name, so clicking one is the key.
    let mut x = inner.x;
    for (button, command) in &controls {
        let width = u16::try_from(button.chars().count() + 2).unwrap_or(u16::MAX);
        if buttons_row < inner.y.saturating_add(inner.height)
            && x.saturating_add(width) <= inner.x.saturating_add(inner.width)
        {
            shell.hit_regions.push(region(
                Rect::new(x, buttons_row, width, 1),
                PointerTarget::RunCommand(*command),
                PointerLayer::Base,
                None,
                None,
            ));
        }
        x = x.saturating_add(width);
    }
}

/// Bytes as the API reports them, in the units a person reads.
fn size_label(bytes: i64) -> String {
    let bytes = bytes.max(0);
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{} kB", bytes / 1024),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

fn render_footer(frame: &mut Frame<'_>, screen: &ReposScreen, shell: &Shell, area: Rect) {
    let (text, style) = shell.notification().map_or_else(
        || {
            (
                screen.footer_hint(shell),
                Style::default().fg(theme().muted),
            )
        },
        |(message, level)| {
            let color = match level {
                NotificationLevel::Info => theme().info,
                NotificationLevel::Error => theme().error,
            };
            (message, Style::default().fg(color))
        },
    );
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(style),
        area,
    );
}
