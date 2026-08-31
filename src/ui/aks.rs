//! The AKS tab: every cluster's pods in one list on the left, and what the
//! details pane says about the one under the cursor on the right.

use super::*;
use crate::aks::{Pod, PodRow};
use crate::app::aks::{AksMode, AksScreen, PaneText, PodColumn, where_it_failed};
use crate::command::CommandId;
use crate::model::Jump;
use crate::ui::details::section_line;
use crate::ui::pipelines::{relative_age, split_timestamp};
use crate::ui::table::{TableSpec, render_list_table, table_geometry};

/// The whole tab: the search box, the table, the details pane and the footer.
pub(crate) fn render(frame: &mut Frame<'_>, screen: &mut AksScreen, shell: &mut Shell, area: Rect) {
    // Which pod the text pane is on is settled here, on the way to drawing it,
    // so the worker is asked for the log of whatever is on screen.
    screen.sync_focus(shell);
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, screen, shell, sections[0]);
    render_content(frame, screen, shell, sections[1]);
    render_status_bar(frame, shell, sections[2], screen.footer_hint(shell));
    if screen.mode == AksMode::ConfirmRestart {
        render_restart_confirm(frame, screen, shell);
    }
}

/// `Restart orders-api-7d9f5b-abc12?`, with what puts a new one up said out
/// loud: a delete that nothing replaces is a different act altogether, and the
/// modal is where the difference has to be visible.
fn render_restart_confirm(frame: &mut Frame<'_>, screen: &AksScreen, shell: &mut Shell) {
    let Some(pod) = screen.restarting_pod() else {
        return;
    };
    let name = pod.key.name.clone();
    let replacement = pod.owner.as_ref().map_or_else(
        || "Deletes the pod.".to_owned(),
        |(kind, owner)| format!("Deletes the pod; {kind} {owner} replaces it."),
    );
    let area = centered_rect(frame.area(), 56, 8);
    let inner = render_modal_frame(frame, PointerLayer::Modal, shell, area, " Restart pod ");
    let rows = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!("Restart {name}?")),
            Line::from(""),
            Line::styled(replacement, Style::default().fg(theme().muted)),
        ])
        .wrap(Wrap { trim: false }),
        rows[0],
    );
    render_control(
        frame,
        shell,
        Control {
            area: Rect::new(rows[1].x, rows[1].y, 9, 1),
            label: " Restart ",
            target: PointerTarget::RunCommand(CommandId::RestartPod),
            layer: PointerLayer::Modal,
            kind: ControlKind::Primary,
            enabled: true,
        },
    );
    render_control(
        frame,
        shell,
        Control {
            area: Rect::new(rows[1].x.saturating_add(10), rows[1].y, 10, 1),
            label: " Leave it ",
            target: PointerTarget::CloseOverlay,
            layer: PointerLayer::Modal,
            kind: ControlKind::Chip,
            enabled: true,
        },
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            "x again to restart it  \u{00b7}  Esc to leave it",
            Style::default().fg(theme().muted),
        )),
        rows[2],
    );
}

fn render_search(frame: &mut Frame<'_>, screen: &AksScreen, shell: &mut Shell, area: Rect) {
    render_search_row(
        frame,
        shell,
        SearchRow {
            area,
            text: screen.query(),
            cursor: screen.query_cursor(),
            placeholder: "Type / to search pods, or cluster:, ns:, status:, owner:, app:, repo:",
            active: screen.mode == AksMode::Search,
            pending: false,
            clearable: false,
            trailer: String::new(),
            layer: PointerLayer::Modal,
            selectable: SelectableSurface::Overlay,
        },
    );
}

fn render_content(frame: &mut Frame<'_>, screen: &mut AksScreen, shell: &mut Shell, area: Rect) {
    struct Panes<'a>(&'a mut AksScreen);
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
            list: "Pods",
            details: "Pod",
        },
        &mut Panes(screen),
    );
}

fn render_table(frame: &mut Frame<'_>, screen: &mut AksScreen, shell: &mut Shell, area: Rect) {
    let now = Timestamp::now();
    let rows = screen.visible_pods(shell);
    let geometry = table_geometry(area, 1);
    screen
        .cursor
        .scroll
        .set_viewport(geometry.visible_rows, rows.len());
    let offset = screen.cursor.scroll.offset;
    let (sorted, descending) = screen.sort;
    let layout = screen.layout.clone();
    let status = table_status(screen, rows.len());
    let mut cell = |index: usize, column: PodColumn| {
        rows.get(index)
            .map_or_else(|| Cell::from(""), |row| pod_cell(row, column, now))
    };
    let mut spec = TableSpec {
        title: " Pods ".to_owned(),
        status,
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

/// What the bottom border says: how many pods, and how old the reading is —
/// or, when a cluster could not be read, what it said instead. A refusal is
/// worth the whole line: the count is already on the table.
fn table_status(screen: &AksScreen, matching: usize) -> String {
    if let Some((cluster, namespace, message)) = screen.errors().first() {
        return format!(
            "{matching} pods \u{00b7} {}: {}",
            where_it_failed(cluster, namespace.as_deref()),
            first_line(message)
        );
    }
    match screen.read_at() {
        Some(read_at) => format!(
            "{matching} pods \u{00b7} read {}",
            crate::app::relative_age(read_at.elapsed())
        ),
        None => format!("{matching} pods"),
    }
}

/// The first line of a complaint, which is the one that says what to fix.
fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or(message)
}

fn pod_cell(row: &PodRow, column: PodColumn, now: Timestamp) -> Cell<'static> {
    match column {
        PodColumn::Name => Cell::from(row.pod.key.name.clone()),
        PodColumn::Cluster => Cell::from(row.pod.key.cluster.clone()),
        PodColumn::Namespace => Cell::from(row.pod.key.namespace.clone()),
        PodColumn::Ready => Cell::from(Line::from(row.pod.ready_label()).right_aligned()),
        PodColumn::Status => Cell::from(Line::from(vec![
            Span::styled(format!("{} ", row.pod.glyph()), pod_style(&row.pod)),
            Span::styled(row.pod.status.clone(), pod_style(&row.pod)),
        ])),
        PodColumn::Restarts => Cell::from(Line::from(row.pod.restarts.to_string()).right_aligned()),
        PodColumn::Age => Cell::from(Line::from(pod_age(&row.pod, now)).right_aligned()),
        PodColumn::Node => Cell::from(row.pod.node.clone()),
        PodColumn::Repo => Cell::from(row.repo.clone().unwrap_or_default()),
    }
}

/// How long the pod has been there, or nothing when it never said.
fn pod_age(pod: &Pod, now: Timestamp) -> String {
    pod.created
        .map_or_else(String::new, |created| relative_age(created, now))
}

/// The colour of a pod's glyph and status word, through the theme's own
/// tokens: what a run's result reads as, said about a pod.
fn pod_style(pod: &Pod) -> Style {
    let colour = match pod.glyph() {
        "\u{25cf}" => theme().state_completed,
        "\u{25d0}" => theme().state_in_progress,
        "\u{2717}" => theme().error,
        _ => theme().muted,
    };
    Style::default().fg(colour)
}

/// The right-hand pane: the pod above, the text pane under it, either side of
/// a seam that drags like every other. `l` gives the text pane the whole area.
fn render_details(frame: &mut Frame<'_>, screen: &mut AksScreen, shell: &mut Shell, area: Rect) {
    if screen.log_full_pane() {
        render_text_pane(frame, screen, shell, area);
        return;
    }
    struct Halves<'a>(&'a mut AksScreen);
    impl PanePair for Halves<'_> {
        fn first(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            render_pod_details(frame, self.0, shell, area);
        }

        fn second(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            render_text_pane(frame, self.0, shell, area);
        }
    }
    render_inner_split(frame, shell, area, &mut Halves(screen));
}

/// The pod's log, tailed, or what describe said in its place. Following keeps
/// the tail in view; scrolling up by any means leaves it, and `End` goes back.
fn render_text_pane(frame: &mut Frame<'_>, screen: &mut AksScreen, shell: &mut Shell, area: Rect) {
    let row = screen.selected_pod(shell);
    let describing = screen.pane() == PaneText::Describe;
    let (title, lines, empty, refused) = match screen.pane() {
        PaneText::Log => (
            log_title(screen, row.as_ref()),
            screen.log_lines().to_vec(),
            "No log yet",
            false,
        ),
        PaneText::Describe => describe_pane(screen, row.as_ref()),
    };
    let block = focused_block(title, shell.focus == Focus::Details).padding(Padding::horizontal(1));
    let pane = inside_border(area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // The pane itself: the wheel scrolls it, a click gives it the focus.
    shell.hit_regions.push(region(
        pane,
        PointerTarget::FocusDetails,
        PointerLayer::Base,
        Some(SelectableSurface::Details),
        Some(ScrollSurface::Details),
    ));
    if lines.is_empty() {
        frame.render_widget(
            Paragraph::new(empty).style(Style::default().fg(theme().muted)),
            inner,
        );
        return;
    }
    let viewport = usize::from(inner.height).max(1);
    screen.pane_scroll.set_viewport(viewport, lines.len());
    if !describing && screen.log_following() {
        screen
            .pane_scroll
            .scroll_to(lines.len().saturating_sub(viewport));
    }
    let offset = screen.pane_scroll.offset;
    // Only the lines on screen are painted: the buffer runs to twenty thousand.
    let painted: Vec<Line<'static>> = lines
        .iter()
        .skip(offset)
        .take(viewport)
        .map(|line| match (describing, refused) {
            (false, _) => log_line(line),
            (true, false) => Line::raw(line.clone()),
            (true, true) => Line::styled(line.clone(), Style::default().fg(theme().error)),
        })
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

/// What the log pane's border says: whose log, which container, how much of
/// it, and whether it is still arriving.
fn log_title(screen: &AksScreen, row: Option<&PodRow>) -> String {
    let Some(target) = screen.following() else {
        return " Log ".to_owned();
    };
    let container = target
        .container
        .clone()
        .or_else(|| {
            row.and_then(|row| row.pod.first_container())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "\u{2014}".to_owned());
    // A stream that has ended has nothing left to wait for and says so
    // plainly; one still arriving spins.
    let state = if screen.log_ended() {
        "ended".to_owned()
    } else if screen.log_following() {
        format!("{} following", spinner_frame())
    } else {
        "scrolled".to_owned()
    };
    // The state first and the count last: a narrow pane cuts the title
    // short, and whether the log is still arriving is the one thing it has to
    // say, while how long it is can be seen by scrolling.
    format!(
        " Log \u{00b7} {state} \u{00b7} {} \u{00b7} {container}{} \u{00b7} {} lines ",
        target.key.name,
        if target.previous { " (previous)" } else { "" },
        screen.log_lines().len(),
    )
}

/// The describe pane: what `kubectl describe pod` said, what it refused with,
/// or that it is still being asked. The flag says the lines are a refusal.
fn describe_pane(
    screen: &AksScreen,
    row: Option<&PodRow>,
) -> (String, Vec<String>, &'static str, bool) {
    let title = format!(
        " Describe \u{00b7} {} ",
        row.map_or("nothing chosen", |row| row.pod.key.name.as_str())
    );
    match screen.describe_lines() {
        Some(Ok(text)) => (title, text.clone(), "Nothing came back", false),
        Some(Err(message)) => (title, vec![message.clone()], "", true),
        None => (
            title,
            Vec::new(),
            if screen.busy() {
                "Describing\u{2026}"
            } else {
                "D describes this pod"
            },
            false,
        ),
    }
}

/// One log line, with the timestamp `--timestamps` puts in front dimmed and
/// the line painted by what its own words say about it.
// ponytail: token heuristic, no log-format parsing.
fn log_line(raw: &str) -> Line<'static> {
    let (stamp, rest) = split_timestamp(raw);
    let mut spans = Vec::new();
    if let Some(stamp) = stamp {
        spans.push(Span::styled(stamp, Style::default().fg(theme().muted)));
    }
    spans.push(Span::styled(rest.to_owned(), severity_style(rest)));
    Line::from(spans)
}

fn severity_style(line: &str) -> Style {
    if line.contains("ERROR") || line.contains("FATAL") || line.contains("level=error") {
        Style::default().fg(theme().error)
    } else if line.contains("WARN") {
        Style::default().fg(theme().warning)
    } else {
        Style::default()
    }
}

/// The details pane for the pod under the cursor: what it is, where it is
/// running, and what its containers are doing.
fn render_pod_details(
    frame: &mut Frame<'_>,
    screen: &mut AksScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let now = Timestamp::now();
    let block =
        focused_block(" Pod ", shell.focus == Focus::Details).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(row) = screen.selected_pod(shell) else {
        frame.render_widget(
            Paragraph::new(nothing_selected(screen))
                .style(Style::default().fg(theme().muted))
                .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{} ", row.pod.glyph()), pod_style(&row.pod)),
            Span::styled(
                row.pod.key.name.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(row.pod.status.clone(), pod_style(&row.pod)),
        ]),
        Line::styled(
            format!("{} \u{00b7} {}", row.pod.key.cluster, row.pod.key.namespace),
            Style::default().fg(theme().accent),
        ),
        Line::from(""),
        field_line("Owner", owner_label(&row)),
        field_line("Node", dash_if_empty(&row.pod.node)),
        field_line("IP", dash_if_empty(&row.pod.ip)),
        field_line("Created", created_label(&row.pod, now)),
        field_line("Ready", row.pod.ready_label()),
        field_line("Restarts", row.pod.restarts.to_string()),
        Line::from(""),
    ];
    // The buttons stand for the keys they name, so clicking one is the key.
    let buttons: [(&str, PointerTarget); 4] = [
        (" Logs ", PointerTarget::RunCommand(CommandId::ShowLogs)),
        (
            " Describe ",
            PointerTarget::RunCommand(CommandId::DescribePod),
        ),
        (" Shell ", PointerTarget::RunCommand(CommandId::ExecShell)),
        (
            " Restart ",
            PointerTarget::RunCommand(CommandId::RestartPod),
        ),
    ];
    let buttons_index = lines.len();
    lines.push(button_row(&buttons));
    // The repository that built it, when one on file matches: the one line of
    // this pane that goes somewhere.
    let repo_line = row.repo.clone().map(|repo| {
        let index = lines.len();
        lines.push(Line::from(vec![
            Span::styled("Repository: ", Style::default().fg(theme().muted)),
            Span::styled(
                repo.clone(),
                Style::default()
                    .fg(theme().link)
                    .add_modifier(Modifier::UNDERLINED),
            ),
            Span::styled("  g", Style::default().fg(theme().muted)),
        ]));
        (index, repo)
    });
    // Which container the log is on, so the line saying so can be marked and
    // the others clicked to move it.
    let followed = screen
        .following()
        .and_then(|target| target.container.clone())
        .or_else(|| row.pod.first_container().map(str::to_owned));
    let mut containers_start = 0;
    if !row.pod.containers.is_empty() {
        lines.push(Line::from(""));
        lines.push(section_line("Containers", inner.width));
        containers_start = lines.len();
        for container in &row.pod.containers {
            let marker = if followed.as_deref() == Some(container.name.as_str()) {
                "\u{203a} "
            } else {
                "  "
            };
            lines.push(Line::from(vec![
                Span::styled(marker.to_owned(), Style::default().fg(theme().accent)),
                Span::raw(format!("{}  ", container.name)),
                Span::styled(container.image.clone(), Style::default().fg(theme().muted)),
                Span::styled(
                    if container.ready {
                        "  \u{2713}"
                    } else {
                        "  \u{2717}"
                    },
                    Style::default().fg(if container.ready {
                        theme().state_completed
                    } else {
                        theme().error
                    }),
                ),
                Span::styled(
                    format!("  {}  {}", container.restarts, container.state),
                    Style::default().fg(theme().muted),
                ),
            ]));
        }
    }
    // What could not be read on this pod's cluster, where the pod it is about
    // is being looked at.
    let problems: Vec<&(String, Option<String>, String)> = screen
        .errors()
        .iter()
        .filter(|(cluster, _, _)| *cluster == row.pod.key.cluster)
        .collect();
    if !problems.is_empty() {
        lines.push(Line::from(""));
        lines.push(section_line("Problems", inner.width));
        for (cluster, namespace, message) in problems {
            lines.push(Line::styled(
                format!(
                    "  {}: {message}",
                    where_it_failed(cluster, namespace.as_deref())
                ),
                Style::default().fg(theme().error),
            ));
        }
    }
    // An image name or a message wraps, so the one target this pane has is
    // placed by the row its line landed on rather than by the line's index.
    let (rows, _) = wrapped_rows(&lines, inner.width);
    let containers = row.pod.containers.len();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    if let Some(y) = row_on_screen(inner, &rows, buttons_index, 0) {
        register_buttons(shell, inner, y, PointerLayer::Base, &buttons);
    }
    if let Some((index, repo)) = repo_line
        && let Some(y) = row_on_screen(inner, &rows, index, 0)
    {
        shell.hit_regions.push(region(
            Rect::new(inner.x, y, inner.width, 1),
            PointerTarget::Follow(Jump::Repo(repo)),
            PointerLayer::Base,
            None,
            None,
        ));
    }
    // One hit region per container, so a click picks the one the log follows.
    for index in 0..containers {
        if let Some(y) = row_on_screen(inner, &rows, containers_start + index, 0) {
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

/// What the pane says with no pod under the cursor: nothing is configured,
/// nothing has come back yet, what went wrong, or nothing matches.
fn nothing_selected(screen: &AksScreen) -> Vec<Line<'static>> {
    if screen.clusters().is_empty() {
        return vec![Line::from(
            "No clusters configured \u{2014} add [[clusters]] to ~/.config/ticket-tui/config.toml",
        )];
    }
    if !screen.errors().is_empty() {
        return screen
            .errors()
            .iter()
            .map(|(cluster, namespace, message)| {
                Line::from(format!(
                    "{}: {message}",
                    where_it_failed(cluster, namespace.as_deref())
                ))
            })
            .collect();
    }
    if !screen.has_read() {
        let names: Vec<&str> = screen
            .clusters()
            .iter()
            .map(|cluster| cluster.name.as_str())
            .collect();
        return vec![Line::from(format!("Reading {}\u{2026}", names.join(", ")))];
    }
    vec![Line::from("No pods match")]
}

/// `Deployment/orders-api`, or a dash for a pod nothing put there.
fn owner_label(row: &PodRow) -> String {
    row.pod.owner.as_ref().map_or_else(
        || "\u{2014}".to_owned(),
        |(kind, name)| format!("{kind}/{name}"),
    )
}

fn created_label(pod: &Pod, now: Timestamp) -> String {
    pod.created.map_or_else(
        || "\u{2014}".to_owned(),
        |created| format!("{} ({})", created.exact_utc(), relative_age(created, now)),
    )
}

fn dash_if_empty(value: &str) -> String {
    if value.is_empty() {
        "\u{2014}".to_owned()
    } else {
        value.to_owned()
    }
}
