//! The Pull requests tab: the queue on the left, and everything worth knowing
//! about the one under the cursor on the right.

use super::*;
use crate::app::pull_requests::{PrColumn, PrRow, PullRequestsScreen};
use crate::model::{Jump, PrStatus};
use crate::ui::details::section_line;
use crate::ui::table::{TableSpec, render_list_table, table_geometry};

pub(crate) fn render(
    frame: &mut Frame<'_>,
    screen: &mut PullRequestsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let chip_height = u16::from(screen.closed_hidden() && screen.hidden_closed(shell) > 0);
    let sections = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(chip_height),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, screen, shell, sections[0]);
    if chip_height > 0 {
        render_closed_chip(frame, screen, shell, sections[1]);
    }
    render_content(frame, screen, shell, sections[2]);
    render_footer(frame, screen, shell, sections[3]);
}

fn render_search(
    frame: &mut Frame<'_>,
    screen: &PullRequestsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let title = screen.active_view.as_ref().map_or_else(
        || " Search / ".to_owned(),
        |view| format!(" Search / \u{2022} {view} "),
    );
    let block = focused_block(title, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    render_query_field(
        frame,
        shell,
        inner,
        screen.query(),
        screen.query_cursor(),
        "Type / to search, or repo:, author:@me, reviewer:@me, vote:none",
        PointerTarget::SearchField,
    );
}

/// The same chip finished work items get, saying what is being left out.
fn render_closed_chip(
    frame: &mut Frame<'_>,
    screen: &PullRequestsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let label = format!(" Closed hidden ({}) \u{00d7} ", screen.hidden_closed(shell));
    let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(Line::styled(
            label,
            Style::default()
                .fg(theme().text)
                .bg(theme().selected_background),
        )),
        area,
    );
    shell.hit_regions.push(region(
        Rect::new(area.x, area.y, width.min(area.width), 1),
        PointerTarget::ShowFinished,
        PointerLayer::Base,
        None,
        None,
    ));
}

fn render_content(
    frame: &mut Frame<'_>,
    screen: &mut PullRequestsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let split = if area.width >= WIDE_BREAKPOINT {
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).split(area)
    } else {
        Layout::horizontal([Constraint::Fill(1)]).split(area)
    };
    render_table(frame, screen, shell, split[0]);
    if split.len() > 1 {
        render_details(frame, screen, shell, split[1]);
    }
}

fn render_table(
    frame: &mut Frame<'_>,
    screen: &mut PullRequestsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let now = Timestamp::now();
    let rows = screen.visible(shell);
    let geometry = table_geometry(area, 1);
    screen
        .cursor
        .scroll
        .set_viewport(geometry.visible_rows, rows.len());
    let offset = screen.cursor.scroll.offset;
    let (sorted, descending) = screen.sort;
    let layout = screen.layout.clone();
    let mut cell = |index: usize, column: PrColumn| {
        rows.get(index)
            .map_or_else(|| Cell::from(""), |row| pr_cell(row, column, now))
    };
    let mut spec = TableSpec {
        title: format!(" Pull requests {} ", rows.len()),
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

fn pr_cell(row: &PrRow, column: PrColumn, now: Timestamp) -> Cell<'static> {
    let faded = row.is_closed();
    let plain = if faded {
        Style::default().fg(theme().muted)
    } else {
        Style::default()
    };
    match column {
        PrColumn::Id => Cell::from(Line::styled(format!("!{}", row.request.id), plain)),
        PrColumn::Title => {
            let mut spans = vec![Span::styled(row.request.title.clone(), plain)];
            if row.request.is_draft {
                spans.push(Span::styled(" [draft]", Style::default().fg(theme().muted)));
            }
            Cell::from(Line::from(spans))
        }
        PrColumn::Repo => Cell::from(Line::styled(row.repo.clone(), plain)),
        PrColumn::Branches => Cell::from(Line::styled(row.branches(), plain)),
        PrColumn::Author => Cell::from(Line::styled(
            row.request.created_by.display_name.clone(),
            plain,
        )),
        PrColumn::Votes => Cell::from(Line::from(
            row.vote_glyphs()
                .into_iter()
                .map(|(glyph, vote)| Span::styled(glyph, vote_style(vote)))
                .collect::<Vec<_>>(),
        )),
        PrColumn::Build => Cell::from(build_line(row)),
        PrColumn::Age => Cell::from(
            Line::styled(
                row.changed_at()
                    .map_or_else(String::new, |at| relative_age(at, now)),
                plain,
            )
            .right_aligned(),
        ),
    }
}

/// `⚠ conflicts` in red beats whatever the build says: a merge that cannot
/// happen is the thing to know.
fn build_line(row: &PrRow) -> Line<'static> {
    if row.request.has_conflicts() {
        return Line::styled("\u{26a0} conflicts", Style::default().fg(theme().error));
    }
    let Some(build) = row.request.build.as_ref() else {
        return Line::from("");
    };
    let (glyph, color) = match build.status.to_ascii_lowercase().as_str() {
        "approved" => ("\u{2713}", theme().state_completed),
        "running" | "queued" => ("\u{25d0}", theme().state_in_progress),
        "rejected" => ("\u{2717}", theme().error),
        _ => ("\u{25cb}", theme().muted),
    };
    Line::styled(
        format!("{glyph} {}", build.status),
        Style::default().fg(color),
    )
}

fn vote_style(vote: i8) -> Style {
    let color = match vote {
        10 => theme().state_completed,
        5 => theme().state_in_progress,
        -5 => theme().warning,
        -10 => theme().error,
        _ => theme().muted,
    };
    Style::default().fg(color)
}

/// Everything worth knowing about the pull request under the cursor, short of
/// the diff: who raised it, what it says, who is reviewing it, what it closes,
/// what the build thinks, and how it means to land.
fn render_details(
    frame: &mut Frame<'_>,
    screen: &mut PullRequestsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let block = focused_block(" Pull request ", shell.focus == Focus::Details);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(row) = screen.selected(shell) else {
        frame.render_widget(
            Paragraph::new("Select a pull request to see it here")
                .style(Style::default().fg(theme().muted)),
            inner,
        );
        return;
    };
    let jumps = screen.jumps(shell);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("!{}  ", row.request.id),
                Style::default().fg(theme().muted),
            ),
            Span::styled(
                row.request.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            if row.request.is_draft {
                Span::styled(" [draft]", Style::default().fg(theme().muted))
            } else {
                Span::raw("")
            },
        ]),
        Line::from(vec![
            Span::styled(
                status_word(row.request.status).to_owned(),
                status_style(row.request.status),
            ),
            Span::raw(format!(
                "  \u{00b7}  {}  \u{00b7}  {}",
                row.request.created_by.display_name,
                row.branches()
            )),
        ]),
        Line::from(""),
    ];
    if !row.request.description.is_empty() {
        for line in crate::html::html_to_text(&row.request.description)
            .lines()
            .take(6)
        {
            lines.push(Line::from(line.to_owned()));
        }
        lines.push(Line::from(""));
    }
    lines.push(section_line("Reviewers"));
    if row.request.reviewers.is_empty() {
        lines.push(Line::styled(
            "  Nobody yet",
            Style::default().fg(theme().muted),
        ));
    }
    for reviewer in &row.request.reviewers {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {} ", reviewer.glyph()),
                vote_style(reviewer.vote),
            ),
            Span::raw(reviewer.display_name.clone()),
            Span::styled(
                format!("  {}", reviewer.word()),
                Style::default().fg(theme().muted),
            ),
            if reviewer.is_required {
                Span::styled("  required", Style::default().fg(theme().warning))
            } else {
                Span::raw("")
            },
        ]));
    }
    lines.push(Line::from(""));
    lines.push(section_line("Related"));
    let jump_start = lines.len();
    for (label, jump) in &jumps {
        let what = match jump {
            Jump::Repo(_) => "Repository",
            Jump::WorkItems(_) => "Work items",
            _ => "Build",
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {what}: "), Style::default().fg(theme().muted)),
            Span::styled(
                label.clone(),
                Style::default()
                    .fg(theme().link)
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(section_line("Completion"));
    lines.push(Line::from(format!(
        "  Auto-complete: {}",
        row.request
            .auto_complete_set_by
            .as_deref()
            .map_or_else(|| "off".to_owned(), |who| format!("on, set by {who}"))
    )));
    lines.push(Line::from(format!("  Merge: {}", row.request.merge_status)));
    lines.push(Line::from(""));
    lines.push(Line::from(
        [
            "[Approve]",
            "[Suggest]",
            "[Wait]",
            "[Reject]",
            "[Complete]",
            "[Abandon]",
        ]
        .into_iter()
        .map(|button| Span::styled(format!(" {button} "), Style::default().fg(theme().muted)))
        .collect::<Vec<_>>(),
    ));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
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
}

fn render_footer(frame: &mut Frame<'_>, screen: &PullRequestsScreen, shell: &Shell, area: Rect) {
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

const fn status_word(status: PrStatus) -> &'static str {
    match status {
        PrStatus::Active => "Active",
        PrStatus::Completed => "Completed",
        PrStatus::Abandoned => "Abandoned",
    }
}

fn status_style(status: PrStatus) -> Style {
    let color = match status {
        PrStatus::Active => theme().state_in_progress,
        PrStatus::Completed => theme().state_completed,
        PrStatus::Abandoned => theme().muted,
    };
    Style::default().fg(color)
}

fn relative_age(instant: Timestamp, now: Timestamp) -> String {
    let seconds = instant.seconds_until(now).max(0);
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}
