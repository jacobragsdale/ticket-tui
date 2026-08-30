//! The Pull requests tab: the queue on the left, and everything worth knowing
//! about the one under the cursor on the right.

use super::*;
use crate::app::pull_requests::{PrColumn, PrMode, PrRow, PullRequestsScreen};
use crate::command::CommandId;
use crate::model::{Jump, PrStatus};
use crate::ui::details::section_line;
use crate::ui::table::{TableSpec, render_list_table, table_geometry};

pub(crate) fn render(
    frame: &mut Frame<'_>,
    screen: &mut PullRequestsScreen,
    shell: &mut Shell,
    work_item_titles: &[(i64, String)],
    area: Rect,
) {
    let chip_height = u16::from(screen.closed_hidden() && screen.hidden_closed(shell) > 0);
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(chip_height),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, screen, shell, sections[0]);
    if chip_height > 0 {
        render_closed_chip(frame, screen, shell, sections[1]);
    }
    render_content(frame, screen, shell, work_item_titles, sections[2]);
    render_footer(frame, screen, shell, sections[3]);
    match screen.mode {
        PrMode::Complete => render_complete_form(frame, screen, shell),
        PrMode::ConfirmAbandon => render_abandon_confirm(frame, screen, shell),
        PrMode::Comment => render_comment_prompt(frame, screen, shell),
        _ => {}
    }
}

/// How the pull request should land: the strategy, and the two things
/// completing it also does.
fn render_complete_form(frame: &mut Frame<'_>, screen: &mut PullRequestsScreen, shell: &mut Shell) {
    let options = screen.completion();
    let focused = screen.completion_field();
    let area = centered_rect(frame.area(), 56, 9);
    let inner = render_modal_frame(frame, PointerLayer::Modal, shell, area, " Complete ");
    let row = |index: usize, label: &str, value: String| {
        let marker = if index == focused { "\u{203a}" } else { " " };
        Line::from(vec![
            Span::raw(format!("{marker} {label:<22}")),
            Span::styled(value, Style::default().fg(theme().accent)),
        ])
    };
    let lines = vec![
        row(0, "Merge strategy", options.strategy.label().to_owned()),
        row(1, "Delete source branch", checkbox(options.delete_source)),
        row(
            2,
            "Complete work items",
            checkbox(options.transition_work_items),
        ),
        Line::from(""),
        Line::styled(
            "\u{2190}\u{2192} or Space change  \u{00b7}  Enter sends it  \u{00b7}  Esc cancels",
            Style::default().fg(theme().muted),
        ),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn checkbox(on: bool) -> String {
    if on {
        "[x] yes".to_owned()
    } else {
        "[ ] no".to_owned()
    }
}

fn render_abandon_confirm(
    frame: &mut Frame<'_>,
    screen: &mut PullRequestsScreen,
    shell: &mut Shell,
) {
    let Some(row) = screen.selected(shell) else {
        return;
    };
    let area = centered_rect(frame.area(), 54, 7);
    let inner = render_modal_frame(frame, PointerLayer::Modal, shell, area, " Abandon ");
    let lines = vec![
        Line::from(format!("Abandon !{}?", row.request.id)),
        Line::from(""),
        Line::styled(
            "Reactivating one is a job for the browser.",
            Style::default().fg(theme().muted),
        ),
        Line::from(""),
        Line::styled(
            "X again to abandon it  \u{00b7}  Esc to leave it",
            Style::default().fg(theme().muted),
        ),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_comment_prompt(
    frame: &mut Frame<'_>,
    screen: &mut PullRequestsScreen,
    shell: &mut Shell,
) {
    let Some(row) = screen.selected(shell) else {
        return;
    };
    let title = format!(" Comment on !{} ", row.request.id);
    let area = centered_rect(frame.area(), 64, 6);
    let inner = render_modal_frame(frame, PointerLayer::Modal, shell, area, &title);
    render_query_field(
        frame,
        shell,
        Rect::new(inner.x, inner.y, inner.width, 1),
        screen.comment_text(),
        screen.comment_cursor(),
        "One line; replies and line comments are o",
        PointerTarget::PromptInput,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            "Enter posts it  \u{00b7}  Esc cancels",
            Style::default().fg(theme().muted),
        )),
        Rect::new(inner.x, inner.y.saturating_add(2), inner.width, 1),
    );
}

fn render_search(
    frame: &mut Frame<'_>,
    screen: &PullRequestsScreen,
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
            placeholder: "Type / to search, or repo:, author:@me, reviewer:@me, vote:none",
            active: screen.mode == PrMode::Search,
            pending: false,
            clearable: false,
            trailer: screen
                .active_view
                .as_ref()
                .map_or_else(String::new, |view| format!("\u{2022} {view}")),
            layer: PointerLayer::Modal,
            selectable: SelectableSurface::Overlay,
        },
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
    work_item_titles: &[(i64, String)],
    area: Rect,
) {
    let split = if area.width >= WIDE_BREAKPOINT {
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).split(area)
    } else {
        Layout::horizontal([Constraint::Fill(1)]).split(area)
    };
    render_table(frame, screen, shell, split[0]);
    if split.len() > 1 {
        render_details(frame, screen, shell, work_item_titles, split[1]);
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
        title: " Pull requests ".to_owned(),
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
    work_item_titles: &[(i64, String)],
    area: Rect,
) {
    let block = focused_block(" Pull request ", shell.focus == Focus::Details)
        .padding(Padding::horizontal(1));
    let pane = inside_border(area);
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
    // Work-item lines read as the work items tab has them when the database
    // holds the row, and as bare ids when it does not.
    let titles = |id: i64| {
        work_item_titles
            .iter()
            .find(|(held, _)| *held == id)
            .map(|(_, title)| title.clone())
    };
    let jumps = screen.jumps(shell, &titles);
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
            Jump::WorkItems(_) => "Work item",
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
    if !row.request.threads.is_empty() {
        lines.push(Line::from(""));
        lines.push(section_line("Discussion"));
        for thread in &row.request.threads {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {}", thread.author),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "  {}{}",
                        thread
                            .published_at
                            .map_or_else(String::new, |at| relative_age(at, Timestamp::now())),
                        if thread.status.is_empty() {
                            String::new()
                        } else {
                            format!(" \u{00b7} {}", thread.status)
                        }
                    ),
                    Style::default().fg(theme().muted),
                ),
            ]));
            lines.push(Line::from(format!("  {}", thread.text)));
        }
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
    let buttons: [(&str, PointerTarget); 6] = [
        ("[Approve]", PointerTarget::RunCommand(CommandId::ApprovePr)),
        ("[Suggest]", PointerTarget::RunCommand(CommandId::SuggestPr)),
        ("[Wait]", PointerTarget::RunCommand(CommandId::WaitPr)),
        ("[Reject]", PointerTarget::RunCommand(CommandId::RejectPr)),
        (
            "[Complete]",
            PointerTarget::RunCommand(CommandId::CompletePr),
        ),
        ("[Abandon]", PointerTarget::RunCommand(CommandId::AbandonPr)),
    ];
    let buttons_index = lines.len();
    lines.push(Line::from(
        buttons
            .iter()
            .map(|(button, _)| {
                Span::styled(format!(" {button} "), Style::default().fg(theme().muted))
            })
            .collect::<Vec<_>>(),
    ));
    // A title or a comment wraps, so every target is placed by the row its
    // line landed on rather than by the line's index; and the pane scrolls,
    // because a discussion can be longer than the terminal.
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
        register_buttons(shell, inner, y, PointerLayer::Base, &buttons);
    }
}

fn render_footer(frame: &mut Frame<'_>, screen: &PullRequestsScreen, shell: &Shell, area: Rect) {
    render_status_bar(frame, shell, area, screen.footer_hint(shell));
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
