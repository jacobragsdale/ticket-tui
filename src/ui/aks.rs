//! The AKS tab: every cluster's pods in one list on the left, and what the
//! details pane says about the one under the cursor on the right.

use super::*;
use crate::aks::{Pod, PodRow};
use crate::app::aks::{AksMode, AksScreen, PodColumn, where_it_failed};
use crate::model::Jump;
use crate::ui::details::section_line;
use crate::ui::pipelines::relative_age;
use crate::ui::table::{TableSpec, render_list_table, table_geometry};

/// The whole tab: the search box, the table, the details pane and the footer.
pub(crate) fn render(frame: &mut Frame<'_>, screen: &mut AksScreen, shell: &mut Shell, area: Rect) {
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, screen, shell, sections[0]);
    render_content(frame, screen, shell, sections[1]);
    render_status_bar(frame, shell, sections[2], screen.footer_hint(shell));
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
            render_pod_details(frame, self.0, shell, area);
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

/// The details pane for the pod under the cursor: what it is, where it is
/// running, and what its containers are doing. #722 puts the log under this.
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
    ];
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
        ]));
        (index, repo)
    });
    if !row.pod.containers.is_empty() {
        lines.push(Line::from(""));
        lines.push(section_line("Containers", inner.width));
        for container in &row.pod.containers {
            lines.push(Line::from(vec![
                Span::raw(format!("  {}  ", container.name)),
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
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
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
