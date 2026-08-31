//! The sprint summary: who is carrying how much of one iteration, how far it
//! has got, and how much of it has gone quiet.
//!
//! Everything here is read off the work items already in memory, so the
//! overlay opens without a round trip and says the same thing offline as it
//! does after a pull. The set it counts is the *whole* one rather than the
//! rows the table is showing: the table hides finished work by default, and a
//! sprint summary that left the finished work out would report a sprint that
//! had barely started right up until the day it ended.
//!
//! Work is grouped by [`StateCategory`] rather than by state name, because the
//! three stations a board has are called different things in every process
//! template — `To Do`/`Doing`/`Done` in Basic, `New`/`Active`/`Closed` in
//! Agile — while the categories behind them are the same everywhere.

use std::collections::BTreeMap;

use crate::filter::is_stale;
use crate::model::{StateCategory, Ticket, same_text};
use crate::timestamp::Timestamp;

/// What the summary calls work nobody owns. It is also the value the
/// `assignee:` filter matches an unassigned work item on, so the row and the
/// query it applies agree without translating between them.
pub const UNASSIGNED: &str = "Unassigned";

/// One column of the summary grid: the three stations a board has, whatever
/// the process template calls the states behind them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SummaryColumn {
    ToDo,
    Doing,
    Done,
}

impl SummaryColumn {
    pub const ALL: [Self; 3] = [Self::ToDo, Self::Doing, Self::Done];

    /// Which column a state category sits under. Resolved counts as in
    /// flight rather than finished: work waiting to be verified is still
    /// somebody's, which is the same reading [`StateCategory::is_done`]
    /// takes. A category nothing maps onto — a state this build has never
    /// heard of — starts at the left, where unstarted work is.
    #[must_use]
    pub const fn of(category: StateCategory) -> Self {
        match category {
            StateCategory::Proposed | StateCategory::Unknown => Self::ToDo,
            StateCategory::InProgress | StateCategory::Resolved => Self::Doing,
            StateCategory::Completed | StateCategory::Removed => Self::Done,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ToDo => "To Do",
            Self::Doing => "Doing",
            Self::Done => "Done",
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::ToDo => 0,
            Self::Doing => 1,
            Self::Done => 2,
        }
    }
}

/// One row of the grid: a person, the Unassigned pile, or the Total line, and
/// how much of the sprint sits in each column for them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssigneeCounts {
    /// The display name Azure DevOps holds, or [`UNASSIGNED`].
    pub name: String,
    pub counts: [usize; SummaryColumn::ALL.len()],
}

impl AssigneeCounts {
    #[must_use]
    fn new(name: impl Into<String>, counts: [usize; SummaryColumn::ALL.len()]) -> Self {
        Self {
            name: name.into(),
            counts,
        }
    }

    #[must_use]
    pub fn total(&self) -> usize {
        self.counts.iter().sum()
    }

    #[must_use]
    pub fn count(&self, column: SummaryColumn) -> usize {
        self.counts[column.index()]
    }

    /// Whether this row stands for the work nobody owns, which is sorted after
    /// the people rather than in among them.
    #[must_use]
    pub fn is_unassigned(&self) -> bool {
        self.name == UNASSIGNED
    }
}

/// One iteration, counted: the grid, the by-type tally under it, and the three
/// figures the headline reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SprintSummary {
    /// The iteration path the counts were taken over, as the work items spell
    /// it: `development\Sprint 1`.
    pub iteration: String,
    /// One row per person, then the Unassigned pile if there is one.
    pub assignees: Vec<AssigneeCounts>,
    /// The same counts over everybody, which is the grid's last row.
    pub total: AssigneeCounts,
    /// Work item types and how many of each, commonest first.
    pub types: Vec<(String, usize)>,
    /// How many work items have sat untouched past the stale threshold, by the
    /// same rule the Changed column paints: [`is_stale`].
    pub stale: usize,
}

impl SprintSummary {
    #[must_use]
    pub fn items(&self) -> usize {
        self.total.total()
    }

    #[must_use]
    pub fn done(&self) -> usize {
        self.total.count(SummaryColumn::Done)
    }

    /// How much of the sprint is finished, rounded to the nearest whole
    /// percent. An iteration holding nothing reads as nought per cent done
    /// rather than as a division by zero.
    #[must_use]
    pub fn done_percent(&self) -> usize {
        let items = self.items();
        (self.done() * 100 + items / 2)
            .checked_div(items)
            .unwrap_or_default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items() == 0
    }

    /// The one-line reading of the sprint, such as
    /// `23 items \u{b7} 9 done (39%) \u{b7} 4 stale`.
    #[must_use]
    pub fn headline(&self) -> String {
        let items = self.items();
        let unit = if items == 1 { "item" } else { "items" };
        format!(
            "{items} {unit} \u{b7} {} done ({}%) \u{b7} {} stale",
            self.done(),
            self.done_percent(),
            self.stale
        )
    }

    /// How wide the first column runs: enough for the longest name on it,
    /// within bounds that keep a short sprint from looking sparse and a long
    /// display name from pushing the counts off the overlay.
    fn name_width(&self) -> usize {
        self.assignees
            .iter()
            .map(|row| row.name.chars().count())
            .chain(self.types.iter().map(|(name, _)| name.chars().count()))
            .max()
            .unwrap_or(0)
            .clamp(MIN_NAME_WIDTH, MAX_NAME_WIDTH)
    }

    /// Every line of the overlay, in the order it is painted: the grid under
    /// its column headings, the by-type tally, and the headline.
    ///
    /// An iteration holding nothing says so rather than painting an empty
    /// grid, which reads as a bug rather than as an empty sprint.
    #[must_use]
    pub fn rows(&self) -> Vec<SummaryRow> {
        if self.is_empty() {
            return vec![SummaryRow::note("No work items in this iteration.")];
        }
        let width = self.name_width();
        let mut rows = vec![SummaryRow {
            kind: SummaryRowKind::Heading,
            text: grid_line(
                "Assignee",
                width,
                [
                    SummaryColumn::ToDo.label(),
                    SummaryColumn::Doing.label(),
                    SummaryColumn::Done.label(),
                    "Total",
                ],
            ),
        }];
        rows.extend(
            self.assignees
                .iter()
                .enumerate()
                .map(|(index, row)| SummaryRow {
                    kind: SummaryRowKind::Assignee(index),
                    text: counts_line(row, width),
                }),
        );
        rows.push(SummaryRow {
            kind: SummaryRowKind::Total,
            text: counts_line(&self.total, width),
        });
        rows.push(SummaryRow::blank());
        rows.push(SummaryRow {
            kind: SummaryRowKind::Heading,
            text: "By type".to_owned(),
        });
        rows.extend(
            self.types
                .iter()
                .map(|(name, count)| SummaryRow::note(type_line(name, *count, width))),
        );
        rows.push(SummaryRow::blank());
        rows.push(SummaryRow::note(self.headline()));
        rows
    }
}

/// Widths the first column is held between, so the grid keeps its shape
/// whether the sprint belongs to one person or to a dozen.
const MIN_NAME_WIDTH: usize = 12;
const MAX_NAME_WIDTH: usize = 24;

/// How wide one count column runs, which is enough for a five-figure sprint
/// and for the `Total` heading over the last of them.
const COLUMN_WIDTH: usize = 6;

/// What one line of the sprint summary overlay is. Only the grid rows can be
/// applied as a filter, so the cursor steps over everything else.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SummaryRowKind {
    /// A column heading or a section heading: shown, never landed on.
    Heading,
    /// One person's row, or the Unassigned pile: an index into
    /// [`SprintSummary::assignees`].
    Assignee(usize),
    /// The grid's last row, which stands for the whole iteration.
    Total,
    /// A by-type count, the headline, or a line explaining why there is no
    /// grid.
    Note,
    /// A spacer between the sections.
    Blank,
}

/// One line of the overlay, formatted but unstyled: the renderer adds the
/// cursor marker and paints it, and nothing else needs to know the layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SummaryRow {
    pub kind: SummaryRowKind,
    pub text: String,
}

impl SummaryRow {
    /// A line that is shown and never landed on, which is what the overlay is
    /// made of when there is no sprint to count.
    pub fn note(text: impl Into<String>) -> Self {
        Self {
            kind: SummaryRowKind::Note,
            text: text.into(),
        }
    }

    fn blank() -> Self {
        Self {
            kind: SummaryRowKind::Blank,
            text: String::new(),
        }
    }

    /// Whether the cursor can land on this row, which is the same thing as
    /// whether `Enter` on it has a filter to apply.
    #[must_use]
    pub const fn is_selectable(&self) -> bool {
        matches!(
            self.kind,
            SummaryRowKind::Assignee(_) | SummaryRowKind::Total
        )
    }
}

/// One grid line: a name, one cell per column, and the row total after them.
fn grid_line(name: &str, width: usize, cells: [&str; SummaryColumn::ALL.len() + 1]) -> String {
    let cell = COLUMN_WIDTH;
    let mut line = format!("{:<width$}", fit(name, width));
    for text in cells {
        line.push_str(&format!(" {text:>cell$}"));
    }
    line
}

fn counts_line(row: &AssigneeCounts, width: usize) -> String {
    let cells = [
        row.count(SummaryColumn::ToDo).to_string(),
        row.count(SummaryColumn::Doing).to_string(),
        row.count(SummaryColumn::Done).to_string(),
        row.total().to_string(),
    ];
    grid_line(
        &row.name,
        width,
        [&cells[0], &cells[1], &cells[2], &cells[3]],
    )
}

/// One line of the by-type tally. The count is set under the grid's Total
/// column, so the two tallies read down the same edge and either can be
/// checked against the other.
fn type_line(name: &str, count: usize, width: usize) -> String {
    let span = (SummaryColumn::ALL.len() + 1) * (COLUMN_WIDTH + 1) - 1;
    format!("{:<width$} {count:>span$}", fit(name, width))
}

/// A name cut to the column, with an ellipsis standing for what was dropped.
fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}\u{2026}")
}

/// Counts one iteration out of `tickets`.
///
/// `tickets` is deliberately the whole set rather than the rows the table is
/// showing: since finished work is hidden from the table by default, counting
/// the visible rows would report a sprint whose Done column never filled up.
/// The caller passes [`crate::app::App::tickets`], not `visible_tickets`.
///
/// Rows are ordered by how much work they carry, heaviest first, with the
/// people sorted by name where they carry the same amount and the Unassigned
/// pile always after them: it is a queue rather than a person, so it reads as
/// the last line of the grid before the total.
#[must_use]
pub fn summarize(
    tickets: &[Ticket],
    iteration: &str,
    stale_days: u16,
    now: Timestamp,
) -> SprintSummary {
    let mut people: BTreeMap<String, [usize; SummaryColumn::ALL.len()]> = BTreeMap::new();
    let mut types: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = [0; SummaryColumn::ALL.len()];
    let mut stale = 0;
    for ticket in tickets
        .iter()
        .filter(|ticket| same_text(&ticket.iteration_path, iteration))
    {
        let column = SummaryColumn::of(StateCategory::of(&ticket.state)).index();
        let name = ticket
            .assigned_to
            .clone()
            .unwrap_or_else(|| UNASSIGNED.to_owned());
        people.entry(name).or_default()[column] += 1;
        *types.entry(ticket.work_item_type.clone()).or_default() += 1;
        total[column] += 1;
        if is_stale(ticket, stale_days, now) {
            stale += 1;
        }
    }
    let mut assignees: Vec<AssigneeCounts> = people
        .into_iter()
        .map(|(name, counts)| AssigneeCounts::new(name, counts))
        .collect();
    assignees.sort_by(|left, right| {
        left.is_unassigned()
            .cmp(&right.is_unassigned())
            .then_with(|| right.total().cmp(&left.total()))
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
    });
    let mut types: Vec<(String, usize)> = types.into_iter().collect();
    types.sort_by(|left, right| {
        right.1.cmp(&left.1).then_with(|| {
            left.0
                .to_ascii_lowercase()
                .cmp(&right.0.to_ascii_lowercase())
        })
    });
    SprintSummary {
        iteration: iteration.trim().to_owned(),
        assignees,
        total: AssigneeCounts::new("Total", total),
        types,
        stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timestamp::ts;

    fn ticket(id: i64, state: &str, assignee: Option<&str>, changed_at: &str) -> Ticket {
        Ticket {
            project: "development".into(),
            title: format!("Work item {id}"),
            state: state.into(),
            assigned_to: assignee.map(Into::into),
            priority: Some(2),
            area_path: "development".into(),
            iteration_path: "development\\Sprint 1".into(),
            created_at: ts("2026-08-01T00:00:00Z"),
            changed_at: ts(changed_at),
            web_url: String::new(),
            ..Ticket::fixture(id, format!("Work item {id}"))
        }
    }

    fn now() -> Timestamp {
        ts("2026-08-29T00:00:00Z")
    }

    /// Two people and one unowned work item across the three columns, with one
    /// item parked in a different sprint to prove the filter bites.
    fn sprint() -> Vec<Ticket> {
        let mut elsewhere = ticket(9, "To Do", Some("Avery Chen"), "2026-08-28T00:00:00Z");
        elsewhere.iteration_path = "development\\Sprint 2".into();
        vec![
            ticket(1, "To Do", Some("Avery Chen"), "2026-08-28T00:00:00Z"),
            ticket(2, "Doing", Some("Avery Chen"), "2026-08-27T00:00:00Z"),
            ticket(3, "Done", Some("Avery Chen"), "2026-08-26T00:00:00Z"),
            ticket(4, "Done", Some("Avery Chen"), "2026-08-25T00:00:00Z"),
            ticket(5, "To Do", Some("Blake Ford"), "2026-08-24T00:00:00Z"),
            ticket(6, "Done", Some("Blake Ford"), "2026-08-23T00:00:00Z"),
            ticket(7, "To Do", None, "2026-01-01T00:00:00Z"),
            elsewhere,
        ]
    }

    #[test]
    fn the_grid_counts_each_person_by_category_and_pins_unassigned_under_them() {
        let summary = summarize(&sprint(), "development\\Sprint 1", 14, now());

        let rows: Vec<(&str, [usize; 3], usize)> = summary
            .assignees
            .iter()
            .map(|row| (row.name.as_str(), row.counts, row.total()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("Avery Chen", [1, 1, 2], 4),
                ("Blake Ford", [1, 0, 1], 2),
                ("Unassigned", [1, 0, 0], 1),
            ],
            "heaviest first, and the pile nobody owns after the people however big it is"
        );
        assert_eq!(summary.total.counts, [3, 1, 3]);
        assert_eq!(summary.total.total(), 7, "the sprint next door is left out");
    }

    #[test]
    fn a_state_is_counted_by_its_category_rather_than_by_its_name() {
        let tickets = vec![
            ticket(1, "New", Some("Avery Chen"), "2026-08-28T00:00:00Z"),
            ticket(2, "Active", Some("Avery Chen"), "2026-08-28T00:00:00Z"),
            ticket(3, "Resolved", Some("Avery Chen"), "2026-08-28T00:00:00Z"),
            ticket(4, "Closed", Some("Avery Chen"), "2026-08-28T00:00:00Z"),
            ticket(5, "Removed", Some("Avery Chen"), "2026-08-28T00:00:00Z"),
        ];

        let summary = summarize(&tickets, "development\\Sprint 1", 14, now());

        assert_eq!(
            summary.total.counts,
            [1, 2, 2],
            "Agile names map onto the same three columns Basic names do, \
             with Resolved still in flight and Removed off the board"
        );
    }

    #[test]
    fn the_by_type_counts_and_the_headline_read_off_the_same_set() {
        let mut tickets = sprint();
        tickets[0].work_item_type = "Bug".into();
        tickets[1].work_item_type = "Bug".into();
        tickets[4].work_item_type = "Epic".into();

        let summary = summarize(&tickets, "development\\Sprint 1", 14, now());

        assert_eq!(
            summary.types,
            vec![
                ("Task".to_owned(), 4),
                ("Bug".to_owned(), 2),
                ("Epic".to_owned(), 1),
            ],
            "commonest type first, ties broken by name"
        );
        assert_eq!(
            summary.types.iter().map(|(_, count)| count).sum::<usize>(),
            summary.items(),
            "every work item is one type, so the tally adds up to the total"
        );
        assert_eq!(summary.done(), 3);
        assert_eq!(summary.done_percent(), 43, "3 of 7, rounded");
        assert_eq!(
            summary.headline(),
            "7 items \u{b7} 3 done (43%) \u{b7} 1 stale"
        );
    }

    #[test]
    fn the_stale_figure_is_the_one_is_stale_would_give() {
        let tickets = sprint();

        let summary = summarize(&tickets, "development\\Sprint 1", 14, now());

        let expected = tickets
            .iter()
            .filter(|ticket| same_text(&ticket.iteration_path, "development\\Sprint 1"))
            .filter(|ticket| is_stale(ticket, 14, now()))
            .count();
        assert_eq!(summary.stale, expected);
        assert_eq!(summary.stale, 1, "only the untouched unassigned one");

        let tighter = summarize(&tickets, "development\\Sprint 1", 3, now());
        assert_eq!(
            tighter.stale, 2,
            "a shorter threshold flags more, and finished work still never counts"
        );
    }

    #[test]
    fn an_empty_percentage_never_divides_by_zero() {
        let summary = summarize(&[], "development\\Sprint 1", 14, now());

        assert!(summary.is_empty());
        assert_eq!(summary.done_percent(), 0);
        let rows = summary.rows();
        assert_eq!(
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
            vec!["No work items in this iteration."],
            "an empty sprint says so rather than painting an empty grid"
        );
        assert!(
            rows.iter().all(|row| !row.is_selectable()),
            "and there is nothing for Enter to filter to"
        );
    }

    #[test]
    fn the_rows_lay_the_grid_out_over_the_type_tally_and_the_headline() {
        let summary = summarize(&sprint(), "development\\Sprint 1", 14, now());

        let rows = summary.rows();
        let text: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        assert_eq!(
            text[..7],
            [
                "Assignee      To Do  Doing   Done  Total",
                "Avery Chen        1      1      2      4",
                "Blake Ford        1      0      1      2",
                "Unassigned        1      0      0      1",
                "Total             3      1      3      7",
                "",
                "By type",
            ]
        );
        assert!(text[7].starts_with("Task") && text[7].ends_with('7'));
        assert_eq!(
            text[7].chars().count(),
            text[4].chars().count(),
            "the type tally is set under the grid's Total column"
        );
        assert_eq!(text[8], "");
        assert_eq!(text[9], "7 items \u{b7} 3 done (43%) \u{b7} 1 stale");
        assert_eq!(
            rows.iter().filter(|row| row.is_selectable()).count(),
            4,
            "three people plus the total, and nothing else is landed on"
        );
        assert_eq!(rows[1].kind, SummaryRowKind::Assignee(0));
        assert_eq!(rows[4].kind, SummaryRowKind::Total);
    }

    #[test]
    fn a_long_display_name_is_cut_to_the_column_rather_than_pushing_the_counts_off() {
        let tickets = vec![ticket(
            1,
            "To Do",
            Some("Bartholomew Fitzgerald-Wellington"),
            "2026-08-28T00:00:00Z",
        )];

        let summary = summarize(&tickets, "development\\Sprint 1", 14, now());

        let row = &summary.rows()[1];
        assert_eq!(
            row.text,
            format!(
                "{:<24} {:>6} {:>6} {:>6} {:>6}",
                "Bartholomew Fitzgerald-\u{2026}", 1, 0, 0, 1
            ),
            "the name gives way, so the counts stay where the headings put them"
        );
    }

    #[test]
    fn an_iteration_path_matches_however_azure_devops_cased_it() {
        let summary = summarize(&sprint(), "  DEVELOPMENT\\sprint 1 ", 14, now());

        assert_eq!(summary.items(), 7);
        assert_eq!(
            summary.iteration, "DEVELOPMENT\\sprint 1",
            "the path is kept as asked for, trimmed"
        );
    }
}
