//! The Pipelines tab: the list of definitions or of one pipeline's runs on the
//! left, and what the details pane says about a run on the right.

use super::*;
use crate::app::pipelines::rows::{duration_label, run_glyph};
use crate::app::pipelines::{
    Level, PipelineColumn, PipelineRow, PipelinesScreen, RunColumn, RunRow,
};
use crate::model::{RunResult, RunStatus};
use crate::ui::table::{TableSpec, render_list_table, table_geometry};

/// The whole tab: the search box, the table, the details pane and the footer.
pub(crate) fn render(
    frame: &mut Frame<'_>,
    screen: &mut PipelinesScreen,
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

fn render_search(frame: &mut Frame<'_>, screen: &PipelinesScreen, shell: &mut Shell, area: Rect) {
    let title = match screen.level() {
        Level::Pipelines => " Search / ".to_owned(),
        Level::Runs(_) => format!(
            " Search / \u{2022} {} ",
            screen
                .open_pipeline()
                .map_or_else(|| "runs".to_owned(), |pipeline| pipeline.name.clone())
        ),
    };
    let block = focused_block(title, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let placeholder = match screen.level() {
        Level::Pipelines => "Type / to search pipelines, or folder:, repo:, result:",
        Level::Runs(_) => "Type / to search runs, or branch:, result:, reason:, by:@me",
    };
    render_query_field(
        frame,
        shell,
        inner,
        screen.query(),
        screen.query_cursor(),
        placeholder,
        PointerTarget::SearchField,
    );
}

fn render_content(
    frame: &mut Frame<'_>,
    screen: &mut PipelinesScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let split = if area.width >= WIDE_BREAKPOINT {
        Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).split(area)
    } else {
        Layout::horizontal([Constraint::Fill(1)]).split(area)
    };
    match screen.level() {
        Level::Pipelines => render_pipeline_table(frame, screen, shell, split[0]),
        Level::Runs(_) => render_run_table(frame, screen, shell, split[0]),
    }
    if split.len() > 1 {
        render_details(frame, screen, shell, split[1]);
    }
}

fn render_pipeline_table(
    frame: &mut Frame<'_>,
    screen: &mut PipelinesScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let now = Timestamp::now();
    let rows = screen.visible_pipelines(shell);
    let geometry = table_geometry(area, 1);
    screen
        .pipeline_cursor
        .scroll
        .set_viewport(geometry.visible_rows, rows.len());
    let offset = screen.pipeline_cursor.scroll.offset;
    let (sorted, descending) = screen.pipeline_sort;
    let layout = screen.pipelines_layout.clone();
    let mut cell = |index: usize, column: PipelineColumn| {
        rows.get(index)
            .map_or_else(|| Cell::from(""), |row| pipeline_cell(row, column, now))
    };
    let mut spec = TableSpec {
        title: format!(" Pipelines {} ", rows.len()),
        focused: true,
        layout: &layout,
        sorted: Some((sorted, if descending { "\u{2193}" } else { "\u{2191}" })),
        count: rows.len(),
        offset,
        selected: Some(screen.pipeline_cursor.index),
        row_height: 1,
        layer: PointerLayer::Base,
        scroll: ScrollSurface::Table,
        selectable: SelectableSurface::Table,
        marker: None,
        cell: &mut cell,
    };
    render_list_table(frame, shell, area, &mut spec);
}

fn render_run_table(
    frame: &mut Frame<'_>,
    screen: &mut PipelinesScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let now = Timestamp::now();
    let rows = screen.visible_runs(shell);
    let geometry = table_geometry(area, 1);
    screen
        .run_cursor
        .scroll
        .set_viewport(geometry.visible_rows, rows.len());
    let offset = screen.run_cursor.scroll.offset;
    let (sorted, descending) = screen.run_sort;
    let layout = screen.runs_layout.clone();
    let title = screen.open_pipeline().map_or_else(
        || " Runs ".to_owned(),
        |pipeline| format!(" {} \u{00b7} {} runs ", pipeline.name, rows.len()),
    );
    let mut cell = |index: usize, column: RunColumn| {
        rows.get(index)
            .map_or_else(|| Cell::from(""), |row| run_cell(row, column, now))
    };
    let mut spec = TableSpec {
        title,
        focused: true,
        layout: &layout,
        sorted: Some((sorted, if descending { "\u{2193}" } else { "\u{2191}" })),
        count: rows.len(),
        offset,
        selected: Some(screen.run_cursor.index),
        row_height: 1,
        layer: PointerLayer::Base,
        scroll: ScrollSurface::Table,
        selectable: SelectableSurface::Table,
        marker: None,
        cell: &mut cell,
    };
    render_list_table(frame, shell, area, &mut spec);
}

fn pipeline_cell(row: &PipelineRow, column: PipelineColumn, now: Timestamp) -> Cell<'static> {
    match column {
        PipelineColumn::Name => Cell::from(row.pipeline.name.clone()),
        PipelineColumn::Folder => Cell::from(folder_label(&row.pipeline.folder)),
        PipelineColumn::LastRun => row.last_run.as_ref().map_or_else(
            || Cell::from(Line::styled("—", Style::default().fg(theme().muted))),
            |run| {
                let row = RunRow {
                    run: run.clone(),
                    pipeline: row.pipeline.name.clone(),
                };
                Cell::from(last_run_line(&row, now))
            },
        ),
        PipelineColumn::Branch => Cell::from(row.branch()),
        PipelineColumn::Age => Cell::from(
            Line::from(
                row.last_run
                    .as_ref()
                    .and_then(|run| run.queue_time)
                    .map_or_else(String::new, |queued| relative_age(queued, now)),
            )
            .right_aligned(),
        ),
    }
}

/// `◐ 20260829.4 · 3m 12s` for a run that is going, `✓ 20260829.4` for one
/// that has finished. The elapsed time is worked out against `now`, so it
/// ticks as long as the frame is redrawn.
fn last_run_line(row: &RunRow, now: Timestamp) -> Line<'static> {
    let glyph = run_glyph(row.run.status, row.run.result);
    let style = run_style(row.run.status, row.run.result);
    let mut spans = vec![
        Span::styled(format!("{glyph} "), style),
        Span::raw(row.run.build_number.clone()),
    ];
    if row.run.status.is_live()
        && let Some(seconds) = row.duration_seconds(now)
    {
        spans.push(Span::styled(
            format!(" \u{00b7} {}", duration_label(seconds)),
            Style::default().fg(theme().muted),
        ));
    }
    Line::from(spans)
}

fn run_cell(row: &RunRow, column: RunColumn, now: Timestamp) -> Cell<'static> {
    let faded = matches!(row.run.result, Some(RunResult::Canceled));
    let plain = if faded {
        Style::default().fg(theme().muted)
    } else {
        Style::default()
    };
    match column {
        RunColumn::Run => Cell::from(Line::from(vec![
            Span::styled(
                format!("{} ", run_glyph(row.run.status, row.run.result)),
                run_style(row.run.status, row.run.result),
            ),
            Span::styled(row.run.build_number.clone(), plain),
        ])),
        RunColumn::Result => Cell::from(Line::styled(
            result_word(row),
            run_style(row.run.status, row.run.result),
        )),
        RunColumn::Branch => Cell::from(Line::styled(row.branch(), plain)),
        RunColumn::Reason => Cell::from(Line::styled(reason_label(&row.run.reason), plain)),
        RunColumn::By => Cell::from(Line::styled(
            row.run.requested_for.clone().unwrap_or_default(),
            plain,
        )),
        RunColumn::Duration => Cell::from(
            Line::styled(
                row.duration_seconds(now)
                    .map_or_else(String::new, duration_label),
                if row.run.status.is_live() {
                    Style::default().fg(theme().state_in_progress)
                } else {
                    plain
                },
            )
            .right_aligned(),
        ),
        RunColumn::Age => Cell::from(
            Line::styled(
                row.run
                    .queue_time
                    .map_or_else(String::new, |queued| relative_age(queued, now)),
                plain,
            )
            .right_aligned(),
        ),
    }
}

/// The details pane for the run under the cursor: what it was, where it came
/// from, and how long it took. #683 puts the timeline under this.
fn render_details(
    frame: &mut Frame<'_>,
    screen: &mut PipelinesScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let now = Timestamp::now();
    let block = focused_block(" Run ", shell.focus == Focus::Details);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(row) = screen.selected_run(shell) else {
        frame.render_widget(
            Paragraph::new("Select a pipeline or a run to see it here")
                .style(Style::default().fg(theme().muted)),
            inner,
        );
        return;
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", run_glyph(row.run.status, row.run.result)),
                run_style(row.run.status, row.run.result),
            ),
            Span::styled(
                row.run.build_number.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(result_word(&row), run_style(row.run.status, row.run.result)),
        ]),
        Line::styled(row.pipeline.clone(), Style::default().fg(theme().accent)),
        Line::from(""),
        field_line("Branch", row.branch()),
        field_line("Commit", short_commit(&row.run.source_version)),
        field_line(
            "By",
            row.run.requested_for.clone().unwrap_or_else(|| "—".into()),
        ),
        field_line("Reason", reason_label(&row.run.reason)),
        Line::from(""),
        field_line("Queued", instant_label(row.run.queue_time, now)),
        field_line("Started", instant_label(row.run.start_time, now)),
        field_line("Finished", instant_label(row.run.finish_time, now)),
        field_line(
            "Duration",
            row.duration_seconds(now)
                .map_or_else(|| "—".to_owned(), duration_label),
        ),
        Line::from(""),
    ];
    lines.push(Line::from(vec![
        Span::styled(" [Cancel] ", Style::default().fg(theme().muted)),
        Span::styled(" [Retry] ", Style::default().fg(theme().muted)),
    ]));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_footer(frame: &mut Frame<'_>, screen: &PipelinesScreen, shell: &Shell, area: Rect) {
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

fn result_word(row: &RunRow) -> String {
    match (row.run.status, row.run.result) {
        (RunStatus::InProgress, _) => "Running".to_owned(),
        (RunStatus::Cancelling, _) => "Cancelling".to_owned(),
        (RunStatus::NotStarted | RunStatus::Postponed, _) => "Queued".to_owned(),
        (_, Some(RunResult::Succeeded)) => "Succeeded".to_owned(),
        (_, Some(RunResult::PartiallySucceeded)) => "Partly succeeded".to_owned(),
        (_, Some(RunResult::Failed)) => "Failed".to_owned(),
        (_, Some(RunResult::Canceled)) => "Canceled".to_owned(),
        (_, None) => "Finished".to_owned(),
    }
}

fn run_style(status: RunStatus, result: Option<RunResult>) -> Style {
    let color = match (status, result) {
        (RunStatus::InProgress | RunStatus::Cancelling, _) => theme().state_in_progress,
        (RunStatus::NotStarted | RunStatus::Postponed, _) => theme().muted,
        (_, Some(RunResult::Succeeded)) => theme().state_completed,
        (_, Some(RunResult::PartiallySucceeded)) => theme().state_in_progress,
        (_, Some(RunResult::Failed)) => theme().error,
        (_, Some(RunResult::Canceled) | None) => theme().muted,
    };
    Style::default().fg(color)
}

/// `\` is the root folder, which reads as nothing rather than as a backslash.
fn folder_label(folder: &str) -> String {
    folder.trim_matches('\\').replace('\\', " / ")
}

/// `individualCI` reads as `CI`, `pullRequest` as `PR`; anything else is left
/// as Azure DevOps spells it.
fn reason_label(reason: &str) -> String {
    match reason {
        "individualCI" | "batchedCI" => "CI".to_owned(),
        "pullRequest" => "PR".to_owned(),
        "manual" => "Manual".to_owned(),
        "schedule" => "Scheduled".to_owned(),
        other => other.to_owned(),
    }
}

fn short_commit(version: &str) -> String {
    version.chars().take(8).collect()
}

fn instant_label(instant: Option<Timestamp>, now: Timestamp) -> String {
    instant.map_or_else(
        || "—".to_owned(),
        |instant| format!("{} ({})", instant.exact_utc(), relative_age(instant, now)),
    )
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
