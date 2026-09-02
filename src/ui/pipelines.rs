//! The Pipelines tab: the list of definitions or of one pipeline's runs on the
//! left, and what the details pane says about a run on the right.

use super::*;
use crate::app::pipelines::rows::{duration_label, run_glyph};
use crate::app::pipelines::{
    Level, PipelineColumn, PipelineMode, PipelineRow, PipelinesScreen, RunColumn, RunRow,
};
use crate::command::CommandId;
use crate::model::{Jump, RunResult, RunStatus, TimelineKind, TimelineRecord};
use crate::ui::details::section_line;
use crate::ui::table::{TableSpec, render_list_table, table_geometry};

/// The whole tab: the search box, the table, the details pane and the footer.
pub(crate) fn render(
    frame: &mut Frame<'_>,
    screen: &mut PipelinesScreen,
    shell: &mut Shell,
    area: Rect,
) {
    // Which run the details pane is on is settled here, on the way to drawing
    // it, so the watcher is asked for the timeline of whatever is on screen.
    screen.sync_focus(shell);
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, screen, shell, sections[0]);
    render_content(frame, screen, shell, sections[1]);
    render_footer(frame, screen, shell, sections[2]);
    match screen.mode {
        PipelineMode::BranchPicker => render_branch_picker(frame, screen, shell),
        PipelineMode::ConfirmCancel => render_cancel_confirm(frame, screen, shell),
        PipelineMode::Approvals => render_approvals(frame, screen, shell),
        PipelineMode::ApprovalComment => render_approval_comment(frame, screen, shell),
        _ => {}
    }
}

/// Every approval the project is waiting on: what it gates, what it asks, and
/// how long it has been waiting.
fn render_approvals(frame: &mut Frame<'_>, screen: &mut PipelinesScreen, shell: &mut Shell) {
    let now = Timestamp::now();
    let approvals = screen.approvals().to_vec();
    let area = centered_rect(frame.area(), 72, 16);
    let inner = render_modal_frame(frame, PointerLayer::Modal, shell, area, " Approvals ");
    if approvals.is_empty() {
        frame.render_widget(
            Paragraph::new("Nothing is waiting on an approval")
                .style(Style::default().fg(theme().muted)),
            inner,
        );
        return;
    }
    let selected = screen.approval_cursor.index;
    let rows: Vec<Line> = approvals
        .iter()
        .enumerate()
        .map(|(index, approval)| {
            let marker = if index == selected { "\u{203a}" } else { " " };
            let age = approval
                .requested_at
                .map_or_else(String::new, |at| format!("  {}", relative_age(at, now)));
            Line::from(vec![
                Span::styled(
                    format!("{marker} \u{25c7} "),
                    Style::default().fg(theme().state_in_progress),
                ),
                Span::raw(format!(
                    "{} \u{00b7} {} \u{00b7} {}",
                    approval.pipeline, approval.build_number, approval.stage
                )),
                Span::styled(age, Style::default().fg(theme().muted)),
            ])
        })
        .collect();
    let mut lines = rows;
    if let Some(approval) = approvals.get(selected)
        && !approval.instructions.is_empty()
    {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            approval.instructions.clone(),
            Style::default().fg(theme().muted),
        ));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    for index in 0..approvals.len() {
        let y = inner
            .y
            .saturating_add(u16::try_from(index).unwrap_or(u16::MAX));
        if y >= inner.y.saturating_add(inner.height) {
            break;
        }
        shell.hit_regions.push(region(
            Rect::new(inner.x, y, inner.width, 1),
            PointerTarget::ApprovalRow { index },
            PointerLayer::Modal,
            None,
            None,
        ));
    }
}

/// The word that goes with an answer, which may be nothing at all.
fn render_approval_comment(frame: &mut Frame<'_>, screen: &mut PipelinesScreen, shell: &mut Shell) {
    let approve = screen.answering_approval().unwrap_or(true);
    let area = centered_rect(frame.area(), 60, 6);
    let title = if approve { " Approve " } else { " Reject " };
    let inner = render_modal_frame(frame, PointerLayer::Modal, shell, area, title);
    render_query_field(
        frame,
        shell,
        Rect::new(inner.x, inner.y, inner.width, 1),
        screen.approval_comment.text(),
        screen.approval_comment.cursor(),
        "A word about why, or nothing",
        PointerTarget::PromptInput,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            "Enter sends it  \u{00b7}  Esc goes back",
            Style::default().fg(theme().muted),
        )),
        Rect::new(inner.x, inner.y.saturating_add(2), inner.width, 1),
    );
}

/// The branch picker a run is started from: a filter field over the
/// repository's branches, opening on whatever is cached.
fn render_branch_picker(frame: &mut Frame<'_>, screen: &mut PipelinesScreen, shell: &mut Shell) {
    let branches = screen.branch_matches();
    let height = u16::try_from(branches.len().clamp(1, 12) + 5).unwrap_or(12);
    let area = centered_rect(frame.area(), 52, height);
    let inner = render_modal_frame(frame, PointerLayer::Modal, shell, area, " Run on branch ");
    let field = Rect::new(inner.x, inner.y, inner.width, 1);
    render_query_field(
        frame,
        shell,
        field,
        screen.branch_picker.query.text(),
        screen.branch_picker.query.cursor(),
        "Type to filter branches",
        PointerTarget::NodeQuery,
    );
    let selected = screen.branch_picker.cursor.index;
    let rows: Vec<Line> = branches
        .iter()
        .enumerate()
        .map(|(index, branch)| {
            let marker = if index == selected { "\u{203a}" } else { " " };
            Line::from(format!("{marker} {branch}"))
        })
        .collect();
    let list = Rect::new(
        inner.x,
        inner.y.saturating_add(2),
        inner.width,
        inner.height.saturating_sub(2),
    );
    frame.render_widget(Paragraph::new(rows), list);
    for (index, _) in branches.iter().enumerate() {
        let y = list
            .y
            .saturating_add(u16::try_from(index).unwrap_or(u16::MAX));
        if y >= list.y.saturating_add(list.height) {
            break;
        }
        shell.hit_regions.push(region(
            Rect::new(list.x, y, list.width, 1),
            PointerTarget::NodeOption { index },
            PointerLayer::Modal,
            None,
            None,
        ));
    }
}

/// `Cancel 20260829.4? · x again`, in the same shape the delete confirmation
/// takes on the work items tab.
fn render_cancel_confirm(frame: &mut Frame<'_>, screen: &mut PipelinesScreen, shell: &mut Shell) {
    let Some(row) = screen.cancelling_run() else {
        return;
    };
    let area = centered_rect(frame.area(), 52, 7);
    let inner = render_modal_frame(frame, PointerLayer::Modal, shell, area, " Cancel run ");
    let lines = vec![
        Line::from(format!("Cancel {}?", row.run.build_number)),
        Line::from(""),
        Line::styled(
            "The jobs still going are stopped where they are.",
            Style::default().fg(theme().muted),
        ),
        Line::from(""),
        Line::styled(
            "x again to cancel it  \u{00b7}  Esc to leave it",
            Style::default().fg(theme().muted),
        ),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_search(frame: &mut Frame<'_>, screen: &PipelinesScreen, shell: &mut Shell, area: Rect) {
    let (placeholder, trailer) = match screen.level() {
        Level::Pipelines => (
            "Type / to search pipelines, or folder:, repo:, result:",
            String::new(),
        ),
        Level::Runs(_) => (
            "Type / to search runs, or branch:, result:, reason:, by:@me",
            format!(
                "\u{2022} {}",
                screen
                    .open_pipeline()
                    .map_or_else(|| "runs".to_owned(), |pipeline| pipeline.name.clone())
            ),
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
            active: screen.mode == PipelineMode::Search,
            pending: false,
            clearable: false,
            trailer,
            layer: PointerLayer::Modal,
            selectable: SelectableSurface::Overlay,
        },
    );
}

fn render_content(
    frame: &mut Frame<'_>,
    screen: &mut PipelinesScreen,
    shell: &mut Shell,
    area: Rect,
) {
    struct Panes<'a>(&'a mut PipelinesScreen);
    impl PanePair for Panes<'_> {
        fn first(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            match self.0.level() {
                Level::Pipelines => render_pipeline_table(frame, self.0, shell, area),
                Level::Runs(_) => render_run_table(frame, self.0, shell, area),
            }
        }

        fn second(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            render_details(frame, self.0, shell, area);
        }
    }
    // The list is whichever level is open: the pipelines, or one pipeline's
    // runs. The chips the narrow layout wears say which.
    let list = match screen.level() {
        Level::Pipelines => "Pipelines".to_owned(),
        Level::Runs(_) => screen
            .open_pipeline()
            .map_or_else(|| "Runs".to_owned(), |pipeline| pipeline.name.clone()),
    };
    render_workspace(
        frame,
        shell,
        area,
        &PaneNames {
            list: &list,
            details: "Run",
        },
        &mut Panes(screen),
    );
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
    let watched: Vec<bool> = rows
        .iter()
        .map(|row| {
            row.last_run
                .as_ref()
                .is_some_and(|run| screen.is_watched(run.id))
        })
        .collect();
    let marker = |index: usize| watch_marker(watched.get(index).copied().unwrap_or_default());
    let mut spec = TableSpec {
        title: " Pipelines ".to_owned(),
        status: rows.len().to_string(),
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
        marker: Some(&marker),
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
        |pipeline| format!(" {} ", pipeline.name),
    );
    let status = format!("{} runs", rows.len());
    let watched: Vec<bool> = rows
        .iter()
        .map(|row| screen.is_watched(row.run.id))
        .collect();
    let marker = |index: usize| watch_marker(watched.get(index).copied().unwrap_or_default());
    let mut cell = |index: usize, column: RunColumn| {
        rows.get(index)
            .map_or_else(|| Cell::from(""), |row| run_cell(row, column, now))
    };
    let mut spec = TableSpec {
        title,
        status,
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
        marker: Some(&marker),
        cell: &mut cell,
    };
    render_list_table(frame, shell, area, &mut spec);
}

/// The gutter of a watched row: a filled circle, the way a bookmarked work
/// item wears one.
fn watch_marker(watched: bool) -> Line<'static> {
    if watched {
        Line::styled(" \u{25c9}", Style::default().fg(theme().accent))
    } else {
        Line::from("  ")
    }
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
    // `l` gives the log the whole pane; otherwise the run and its timeline
    // take the first half and the log the second, either side of a seam that
    // drags like every other.
    if screen.log_full_pane() {
        render_log(frame, screen, shell, area);
        return;
    }
    struct Halves<'a>(&'a mut PipelinesScreen);
    impl PanePair for Halves<'_> {
        fn first(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            render_run_details(frame, self.0, shell, area);
        }

        fn second(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            render_log(frame, self.0, shell, area);
        }
    }
    render_inner_split(frame, shell, area, &mut Halves(screen));
}

fn render_run_details(
    frame: &mut Frame<'_>,
    screen: &mut PipelinesScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let now = Timestamp::now();
    let block =
        focused_block(" Run ", shell.focus == Focus::Details).padding(Padding::horizontal(1));
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
    let timeline = screen.timeline(row.run.id).to_vec();
    let cursor = screen.timeline_cursor();
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
    let buttons: [(&str, PointerTarget); 2] = [
        (" Cancel ", PointerTarget::RunCommand(CommandId::CancelRun)),
        (" Retry ", PointerTarget::RunCommand(CommandId::RetryRun)),
    ];
    let buttons_index = lines.len();
    lines.push(button_row(&buttons));
    // Where this run came from and what it carried, each one a jump.
    let jumps = screen.run_jumps(shell);
    if !jumps.is_empty() {
        lines.push(Line::from(""));
        lines.push(section_line("Related", inner.width));
    }
    let jump_start = lines.len();
    for (label, jump) in &jumps {
        let what = match jump {
            Jump::Repo(_) => "Repository",
            Jump::PullRequest { .. } => "Pull request",
            _ => "Work items",
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{what}: "), Style::default().fg(theme().muted)),
            Span::styled(
                label.clone(),
                Style::default()
                    .fg(theme().link)
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ]));
    }
    if !timeline.is_empty() {
        lines.push(Line::from(""));
        lines.push(section_line("Timeline", inner.width));
    }
    let tree_start = lines.len();
    lines.extend(timeline_lines(
        &timeline,
        cursor,
        now,
        shell.focus == Focus::Details,
    ));
    // A pipeline name or a work-item line wraps, so every target is placed
    // by the row its line landed on rather than by the line's index.
    let (rows, _) = wrapped_rows(&lines, inner.width);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    // The buttons stand for the keys they name, so clicking one is the key.
    if let Some(y) = row_on_screen(inner, &rows, buttons_index, 0) {
        register_buttons(shell, inner, y, PointerLayer::Base, &buttons);
    }
    // One hit region per reference, so a click follows it.
    for (index, (_, jump)) in jumps.iter().enumerate() {
        if let Some(y) = row_on_screen(inner, &rows, jump_start + index, 0) {
            shell.hit_regions.push(region(
                Rect::new(inner.x, y, inner.width, 1),
                PointerTarget::Follow(jump.clone()),
                PointerLayer::Base,
                None,
                None,
            ));
        }
    }
    // One hit region per node, so a click picks the node the log follows.
    for index in 0..timeline.len() {
        if let Some(y) = row_on_screen(inner, &rows, tree_start + index, 0) {
            shell.hit_regions.push(region(
                Rect::new(inner.x, y, inner.width, 1),
                PointerTarget::TreeRow { index },
                PointerLayer::Base,
                None,
                None,
            ));
        }
    }
}

fn render_footer(frame: &mut Frame<'_>, screen: &PipelinesScreen, shell: &Shell, area: Rect) {
    render_screen_status_bar(frame, screen, shell, area);
}

/// The timeline as a tree: stages, the jobs under them, the tasks under those,
/// with the same connectors the work-item family tree uses. A running node
/// carries the time it has been going, recomputed each frame; a pending one
/// reads `—`; a node with errors says how many.
fn timeline_lines(
    records: &[TimelineRecord],
    cursor: usize,
    now: Timestamp,
    focused: bool,
) -> Vec<Line<'static>> {
    let depth_of = |record: &TimelineRecord| match record.kind {
        TimelineKind::Stage => 0_usize,
        TimelineKind::Job | TimelineKind::Checkpoint => 1,
        TimelineKind::Task => 2,
    };
    records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let indent = "  ".repeat(depth_of(record));
            let glyph = node_glyph(record);
            let style = node_style(record);
            let mut spans = vec![
                Span::raw(format!(" {indent}")),
                Span::styled(format!("{glyph} "), style),
                Span::raw(record.name.clone()),
            ];
            let errors = record.error_count();
            if errors > 0 {
                spans.push(Span::styled(
                    format!("  \u{2717} {errors}"),
                    Style::default().fg(theme().error),
                ));
            }
            if let Some(percent) = record
                .percent_complete
                .filter(|_| record.state == RunStatus::InProgress)
            {
                spans.push(Span::styled(
                    format!("  {percent}%"),
                    Style::default().fg(theme().muted),
                ));
            }
            spans.push(Span::styled(
                format!(
                    "  {}",
                    record
                        .duration_seconds(now)
                        .map_or_else(|| "\u{2014}".to_owned(), duration_label)
                ),
                Style::default().fg(theme().muted),
            ));
            let line = Line::from(spans);
            if index == cursor && focused {
                line.style(Style::default().bg(theme().selected_background))
            } else {
                line
            }
        })
        .collect()
}

fn node_glyph(record: &TimelineRecord) -> &'static str {
    match (record.kind, record.state, record.result) {
        (TimelineKind::Checkpoint, RunStatus::InProgress, _) => "\u{25c7}",
        _ => run_glyph(record.state, record.result),
    }
}

fn node_style(record: &TimelineRecord) -> Style {
    if record.kind == TimelineKind::Checkpoint && record.state == RunStatus::InProgress {
        return Style::default().fg(theme().state_in_progress);
    }
    run_style(record.state, record.result)
}

/// The log of the node the tree cursor is on, or of the deepest task still
/// running when nobody has chosen one. Following keeps the tail in view;
/// scrolling up by any means leaves it, and `End` goes back.
fn render_log(frame: &mut Frame<'_>, screen: &mut PipelinesScreen, shell: &mut Shell, area: Rect) {
    let node = screen.log_node_name();
    let lines: Vec<String> = screen
        .log_target()
        .map(|target| {
            screen
                .focused_run()
                .map(|run| screen.log(run, target.log_id).to_vec())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    // A log following a run that is still going spins; one following a run
    // that has finished has nothing left to wait for and says so plainly.
    let running = screen
        .selected_run(shell)
        .is_some_and(|row| row.run.status == RunStatus::InProgress);
    let following = match (screen.log_following(), running) {
        (true, true) => format!("{} following", spinner_frame()),
        (true, false) => "following".to_owned(),
        (false, _) => "scrolled".to_owned(),
    };
    let title = format!(
        " Log \u{00b7} {} \u{00b7} {} lines \u{00b7} {following} ",
        node.unwrap_or_else(|| "nothing chosen".to_owned()),
        lines.len(),
    );
    let block = focused_block(title, shell.focus == Focus::Details).padding(Padding::horizontal(1));
    let pane = inside_border(area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if lines.is_empty() {
        frame.render_widget(
            Paragraph::new("No log yet").style(Style::default().fg(theme().muted)),
            inner,
        );
        return;
    }
    let viewport = usize::from(inner.height).max(1);
    screen.log_scroll.set_viewport(viewport, lines.len());
    if screen.log_following() {
        let tail = lines.len().saturating_sub(viewport);
        screen.log_scroll.scroll_to(tail);
    }
    let offset = screen.log_scroll.offset;
    let painted: Vec<Line<'static>> = lines
        .iter()
        .skip(offset)
        .take(viewport)
        .map(|line| log_line(line))
        .collect();
    frame.render_widget(Paragraph::new(painted), inner);
    if lines.len() > viewport {
        render_scrollbar(
            frame,
            PointerLayer::Base,
            shell,
            pane,
            ScrollSurface::Details,
            ScrollState {
                offset,
                content: lines.len(),
                viewport,
            },
        );
    }
    capture_selectable(frame, shell, SelectableSurface::Details, inner, true);
}

/// One log line, with the timestamp dimmed and the `##[marker]` prefixes
/// painted the way the conventions ask.
fn log_line(raw: &str) -> Line<'static> {
    let (stamp, rest) = split_timestamp(raw);
    let mut spans = Vec::new();
    if let Some(stamp) = stamp {
        spans.push(Span::styled(stamp, Style::default().fg(theme().muted)));
    }
    let (marker, body) = split_marker(rest);
    match marker {
        Some("section") => spans.push(Span::styled(
            body.to_owned(),
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        )),
        Some("group") => spans.push(Span::styled(
            format!("\u{25b8} {body}"),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Some("endgroup") => spans.push(Span::styled(
            format!("\u{25b8} {body}"),
            Style::default()
                .fg(theme().muted)
                .add_modifier(Modifier::BOLD),
        )),
        Some("warning") => spans.push(Span::styled(
            body.to_owned(),
            Style::default().fg(theme().warning),
        )),
        Some("error") => spans.push(Span::styled(
            body.to_owned(),
            Style::default().fg(theme().error),
        )),
        Some("debug") => spans.push(Span::styled(
            body.to_owned(),
            Style::default().fg(theme().muted),
        )),
        Some("command") => spans.push(Span::styled(
            body.to_owned(),
            Style::default().fg(theme().accent),
        )),
        _ => spans.push(Span::raw(body.to_owned())),
    }
    Line::from(spans)
}

/// Azure DevOps prefixes every line with an ISO instant. It is dimmed rather
/// than dropped: a slow step is easiest to spot by its clock.
pub(super) fn split_timestamp(raw: &str) -> (Option<String>, &str) {
    let Some((stamp, rest)) = raw.split_once(' ') else {
        return (None, raw);
    };
    let looks_like_a_stamp = stamp.len() >= 20
        && stamp.starts_with(|character: char| character.is_ascii_digit())
        && stamp.contains('T');
    if looks_like_a_stamp {
        (Some(format!("{} ", &stamp[..19])), rest)
    } else {
        (None, raw)
    }
}

/// `##[error]something went wrong` is `("error", "something went wrong")`.
fn split_marker(line: &str) -> (Option<&str>, &str) {
    let Some(rest) = line.strip_prefix("##[") else {
        return (None, line);
    };
    let Some((marker, body)) = rest.split_once(']') else {
        return (None, line);
    };
    (Some(marker), body)
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

/// The colour a run is painted in, wherever it is drawn: the Pipelines table,
/// its details pane, and the Repos tab's Pipelines column.
pub(crate) fn run_style(status: RunStatus, result: Option<RunResult>) -> Style {
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

pub(crate) fn instant_label(instant: Option<Timestamp>, now: Timestamp) -> String {
    instant.map_or_else(
        || "—".to_owned(),
        |instant| format!("{} ({})", instant.exact_utc(), relative_age(instant, now)),
    )
}

pub(crate) fn relative_age(instant: Timestamp, now: Timestamp) -> String {
    let seconds = instant.seconds_until(now).max(0);
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}
