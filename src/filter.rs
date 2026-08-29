use std::collections::{BTreeMap, BTreeSet};

use crate::model::Ticket;
use crate::timestamp::Timestamp;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FilterField {
    State,
    Type,
    Assignee,
    Priority,
    Project,
    Area,
    Iteration,
    Tags,
    Changed,
    Created,
}

impl FilterField {
    pub const ALL: [Self; 10] = [
        Self::State,
        Self::Type,
        Self::Assignee,
        Self::Priority,
        Self::Project,
        Self::Area,
        Self::Iteration,
        Self::Tags,
        Self::Changed,
        Self::Created,
    ];

    pub const BAR: [Self; 4] = [Self::State, Self::Type, Self::Tags, Self::Assignee];

    #[must_use]
    pub const fn on_bar(self) -> bool {
        matches!(self, Self::State | Self::Type | Self::Tags | Self::Assignee)
    }

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Type => "type",
            Self::Assignee => "assignee",
            Self::Priority => "priority",
            Self::Project => "project",
            Self::Area => "area",
            Self::Iteration => "iteration",
            Self::Tags => "tag",
            Self::Changed => "changed",
            Self::Created => "created",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::State => "State",
            Self::Type => "Type",
            Self::Assignee => "Assignee",
            Self::Priority => "Priority",
            Self::Project => "Project",
            Self::Area => "Area",
            Self::Iteration => "Iteration",
            Self::Tags => "Tags",
            Self::Changed => "Changed",
            Self::Created => "Created",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "state" => Some(Self::State),
            "type" => Some(Self::Type),
            "assignee" | "assigned" => Some(Self::Assignee),
            "priority" | "pri" => Some(Self::Priority),
            "project" => Some(Self::Project),
            "area" => Some(Self::Area),
            "iteration" | "sprint" => Some(Self::Iteration),
            "tag" | "tags" => Some(Self::Tags),
            "changed" | "updated" => Some(Self::Changed),
            "created" => Some(Self::Created),
            _ => None,
        }
    }

    /// Whether this field holds a date comparison rather than a value drawn
    /// from the tickets themselves. A date has no enumerable list of values,
    /// so the overlay offers presets where the other fields offer facets.
    #[must_use]
    pub const fn is_date(self) -> bool {
        matches!(self, Self::Changed | Self::Created)
    }
}

/// The windows the filter overlay offers for `changed:` and `created:`, in
/// place of the facet list a comparison cannot have. Anything else is typed
/// into the query.
pub const DATE_PRESETS: [&str; 4] = ["<24h", "<7d", "<14d", "<30d"];

/// A comparison a `changed:` or `created:` value asks for, parsed out of the
/// text as typed so the filter set carries on holding plain strings.
///
/// The operator reads against the value written after it, which turns its
/// meaning around between the two forms: `<7d` is an age below seven days and
/// so keeps the *recently* touched items, while `<2026-08-01` keeps the
/// instants falling before that date. Relative bounds are measured from the
/// instant handed to `matches` rather than from when the query was written, so
/// a view saved as `changed:<7d` still means the last seven days tomorrow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DatePredicate {
    operator: Comparison,
    bound: DateBound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Comparison {
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DateBound {
    /// A span reaching back from now, in whole seconds.
    Age(i64),
    /// A fixed instant, which a bare `YYYY-MM-DD` reads as UTC midnight.
    Instant(Timestamp),
}

impl DatePredicate {
    /// Reads a value such as `<7d`, `>=2h`, or `>2026-08-01`, and `None` for
    /// anything that is not a comparison: a bare duration carries no direction
    /// and so is not one.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let (operator, rest) = Comparison::take(value.trim())?;
        let rest = rest.trim();
        parse_age(rest)
            .map(DateBound::Age)
            .or_else(|| Timestamp::parse(rest).ok().map(DateBound::Instant))
            .map(|bound| Self { operator, bound })
    }

    /// Whether an instant satisfies the comparison, with `now` standing in for
    /// the clock so a relative bound can be tested against a fixed moment.
    #[must_use]
    pub fn matches(self, instant: Timestamp, now: Timestamp) -> bool {
        match self.bound {
            DateBound::Age(seconds) => self.operator.holds(instant.seconds_until(now), seconds),
            DateBound::Instant(bound) => self.operator.holds(instant, bound),
        }
    }
}

impl Comparison {
    /// Splits the operator off the front of a value, longer forms first so
    /// `<=` is not read as a `<` with a stray character behind it.
    fn take(value: &str) -> Option<(Self, &str)> {
        [
            ("<=", Self::LessOrEqual),
            (">=", Self::GreaterOrEqual),
            ("<", Self::Less),
            (">", Self::Greater),
        ]
        .into_iter()
        .find_map(|(prefix, operator)| value.strip_prefix(prefix).map(|rest| (operator, rest)))
    }

    fn holds<T: Ord>(self, left: T, right: T) -> bool {
        match self {
            Self::Less => left < right,
            Self::LessOrEqual => left <= right,
            Self::Greater => left > right,
            Self::GreaterOrEqual => left >= right,
        }
    }
}

/// A span such as `7d` in seconds, over the units a work item's age is talked
/// about in: minutes, hours, days, and weeks.
fn parse_age(value: &str) -> Option<i64> {
    let split = value.find(|character: char| !character.is_ascii_digit())?;
    let (count, unit) = value.split_at(split);
    let count: i64 = count.parse().ok()?;
    let seconds = match unit.to_ascii_lowercase().as_str() {
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        "w" => 7 * 24 * 60 * 60,
        _ => return None,
    };
    count.checked_mul(seconds)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FacetTarget {
    Field(FilterField),
    More,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FilterToken {
    Field { field: FilterField, value: String },
    Bookmarked,
}

impl FilterToken {
    #[must_use]
    pub fn chip_label(&self) -> String {
        match self {
            Self::Field { field, value } => format!("{}:{value}", field.key()),
            Self::Bookmarked => "is:bookmarked".into(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FilterSet {
    values: BTreeMap<FilterField, BTreeSet<String>>,
    pub bookmarked: bool,
}

impl FilterSet {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.bookmarked && self.values.values().all(BTreeSet::is_empty)
    }

    #[must_use]
    pub fn selected_count(&self, field: FilterField) -> usize {
        self.values.get(&field).map_or(0, BTreeSet::len)
    }

    #[must_use]
    pub fn selected_values(&self, field: FilterField) -> Vec<String> {
        self.values
            .get(&field)
            .map(|values| values.iter().cloned().collect())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn contains(&self, field: FilterField, value: &str) -> bool {
        self.values
            .get(&field)
            .is_some_and(|values| values.iter().any(|entry| entry.eq_ignore_ascii_case(value)))
    }

    pub fn insert(&mut self, field: FilterField, value: impl Into<String>) {
        let value = value.into();
        if value.is_empty() {
            return;
        }
        if self.contains(field, &value) {
            return;
        }
        self.values.entry(field).or_default().insert(value);
    }

    pub fn remove(&mut self, field: FilterField, value: &str) {
        if let Some(values) = self.values.get_mut(&field) {
            values.retain(|entry| !entry.eq_ignore_ascii_case(value));
            if values.is_empty() {
                self.values.remove(&field);
            }
        }
    }

    pub fn toggle(&mut self, field: FilterField, value: &str) {
        if self.contains(field, value) {
            self.remove(field, value);
        } else {
            self.insert(field, value);
        }
    }

    #[must_use]
    pub fn tokens(&self) -> Vec<FilterToken> {
        let mut tokens = Vec::new();
        if self.bookmarked {
            tokens.push(FilterToken::Bookmarked);
        }
        for field in FilterField::ALL {
            if let Some(values) = self.values.get(&field) {
                for value in values {
                    tokens.push(FilterToken::Field {
                        field,
                        value: value.clone(),
                    });
                }
            }
        }
        tokens
    }

    /// Whether a ticket passes every field of the query, with relative date
    /// bounds measured from the current instant.
    #[must_use]
    pub fn matches(&self, ticket: &Ticket, is_bookmarked: bool) -> bool {
        self.matches_at(ticket, is_bookmarked, Timestamp::now())
    }

    /// `matches` against a given instant, which is how `changed:<7d` is tested
    /// without reaching for the wall clock.
    #[must_use]
    pub fn matches_at(&self, ticket: &Ticket, is_bookmarked: bool, now: Timestamp) -> bool {
        if self.bookmarked && !is_bookmarked {
            return false;
        }
        self.values.iter().all(|(field, values)| {
            values
                .iter()
                .any(|value| field_matches(*field, ticket, value, now))
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ParsedQuery {
    pub fuzzy: String,
    pub filters: FilterSet,
}

impl ParsedQuery {
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.fuzzy.is_empty() || !self.filters.is_empty()
    }
}

#[must_use]
pub fn parse_query(input: &str) -> ParsedQuery {
    let mut filters = FilterSet::default();
    let mut fuzzy = Vec::new();
    let mut rest = input.trim_start();
    while !rest.is_empty() {
        if let Some(remaining) = take_special_filter(rest, &mut filters) {
            rest = remaining.trim_start();
            continue;
        }
        if let Some((field, value, remaining)) = take_field_filter(rest) {
            filters.insert(field, value);
            rest = remaining.trim_start();
            continue;
        }
        let (term, remaining) = take_term(rest);
        if !term.is_empty() {
            fuzzy.push(term);
        }
        rest = remaining.trim_start();
    }
    ParsedQuery {
        fuzzy: fuzzy.join(" "),
        filters,
    }
}

#[must_use]
pub fn format_query(filters: &FilterSet, fuzzy: &str) -> String {
    let mut parts: Vec<String> = filters
        .tokens()
        .into_iter()
        .map(|token| match token {
            FilterToken::Bookmarked => "is:bookmarked".into(),
            FilterToken::Field { field, value } => {
                format!("{}:{}", field.key(), quote_if_needed(&value))
            }
        })
        .collect();
    let fuzzy = fuzzy.trim();
    if !fuzzy.is_empty() {
        parts.push(fuzzy.to_owned());
    }
    parts.join(" ")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FacetValue {
    pub value: String,
    pub count: usize,
    pub selected: bool,
}

#[must_use]
pub fn facet_values(
    tickets: &[Ticket],
    filters: &FilterSet,
    field: FilterField,
    bookmarked: impl Fn(&Ticket) -> bool,
) -> Vec<FacetValue> {
    facet_values_at(tickets, filters, field, bookmarked, Timestamp::now())
}

fn facet_values_at(
    tickets: &[Ticket],
    filters: &FilterSet,
    field: FilterField,
    bookmarked: impl Fn(&Ticket) -> bool,
    now: Timestamp,
) -> Vec<FacetValue> {
    if field.is_date() {
        return date_facets(tickets, filters, field, bookmarked, now);
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for ticket in tickets {
        if !matches_excluding(ticket, filters, field, bookmarked(ticket), now) {
            continue;
        }
        for value in field_values(field, ticket) {
            *counts.entry(value).or_default() += 1;
        }
    }
    let mut facets: Vec<_> = counts
        .into_iter()
        .map(|(value, count)| FacetValue {
            selected: filters.contains(field, &value),
            value,
            count,
        })
        .collect();
    facets.sort_by(|left, right| {
        right.count.cmp(&left.count).then_with(|| {
            left.value
                .to_ascii_lowercase()
                .cmp(&right.value.to_ascii_lowercase())
        })
    });
    facets
}

/// The overlay rows for a date field: the fixed presets, followed by whatever
/// else the query already asks for, since a comparison typed into the search
/// bar has to be un-checkable from the same list it shows up in. The presets
/// keep their written order rather than sorting by count, so the windows read
/// shortest first. Counts are of the tickets the rest of the query leaves,
/// which is how the enumerated facets count too.
fn date_facets(
    tickets: &[Ticket],
    filters: &FilterSet,
    field: FilterField,
    bookmarked: impl Fn(&Ticket) -> bool,
    now: Timestamp,
) -> Vec<FacetValue> {
    let mut values: Vec<String> = DATE_PRESETS
        .iter()
        .map(|preset| (*preset).to_owned())
        .collect();
    for selected in filters.selected_values(field) {
        if !values
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&selected))
        {
            values.push(selected);
        }
    }
    let remaining: Vec<&Ticket> = tickets
        .iter()
        .filter(|ticket| matches_excluding(ticket, filters, field, bookmarked(ticket), now))
        .collect();
    values
        .into_iter()
        .map(|value| FacetValue {
            selected: filters.contains(field, &value),
            count: remaining
                .iter()
                .filter(|ticket| field_matches(field, ticket, &value, now))
                .count(),
            value,
        })
        .collect()
}

fn matches_excluding(
    ticket: &Ticket,
    filters: &FilterSet,
    excluded: FilterField,
    is_bookmarked: bool,
    now: Timestamp,
) -> bool {
    if filters.bookmarked && !is_bookmarked {
        return false;
    }
    filters.values.iter().all(|(field, values)| {
        *field == excluded
            || values
                .iter()
                .any(|value| field_matches(*field, ticket, value, now))
    })
}

fn field_matches(field: FilterField, ticket: &Ticket, needle: &str, now: Timestamp) -> bool {
    if let Some(instant) = date_value(field, ticket) {
        return DatePredicate::parse(needle)
            .is_some_and(|predicate| predicate.matches(instant, now));
    }
    field_values(field, ticket)
        .iter()
        .any(|value| value.eq_ignore_ascii_case(needle) || path_segment_matches(value, needle))
}

/// The instant a date field compares against, and `None` for every field whose
/// values are text.
fn date_value(field: FilterField, ticket: &Ticket) -> Option<Timestamp> {
    match field {
        FilterField::Changed => Some(ticket.changed_at),
        FilterField::Created => Some(ticket.created_at),
        _ => None,
    }
}

fn path_segment_matches(value: &str, needle: &str) -> bool {
    crate::model::path_leaf(value).eq_ignore_ascii_case(needle)
}

fn field_values(field: FilterField, ticket: &Ticket) -> Vec<String> {
    match field {
        FilterField::State => vec![ticket.state.clone()],
        FilterField::Type => vec![ticket.work_item_type.clone()],
        FilterField::Assignee => vec![
            ticket
                .assigned_to
                .clone()
                .unwrap_or_else(|| "Unassigned".into()),
        ],
        FilterField::Priority => vec![
            ticket
                .priority
                .map_or_else(|| "—".into(), |priority| priority.to_string()),
        ],
        FilterField::Project => vec![ticket.project.clone()],
        FilterField::Area => vec![ticket.area_path.clone()],
        FilterField::Iteration => vec![ticket.iteration_path.clone()],
        FilterField::Tags => ticket.tags.clone(),
        // Dates are compared rather than enumerated: `field_matches` answers
        // for them before reaching here, and the overlay offers presets.
        FilterField::Changed | FilterField::Created => Vec::new(),
    }
}

fn take_special_filter<'a>(input: &'a str, filters: &mut FilterSet) -> Option<&'a str> {
    let ident_len = ident_len(input);
    if ident_len == 0 || input.get(ident_len..ident_len + 1) != Some(":") {
        return None;
    }
    if !input[..ident_len].eq_ignore_ascii_case("is") {
        return None;
    }
    let (value, rest) = take_value(&input[ident_len + 1..])?;
    if value.eq_ignore_ascii_case("bookmarked") || value.eq_ignore_ascii_case("bookmark") {
        filters.bookmarked = true;
        Some(rest)
    } else {
        None
    }
}

fn take_field_filter(input: &str) -> Option<(FilterField, String, &str)> {
    let ident_len = ident_len(input);
    if ident_len == 0 || input.get(ident_len..ident_len + 1) != Some(":") {
        return None;
    }
    let field = FilterField::parse(&input[..ident_len])?;
    let (value, rest) = take_value(&input[ident_len + 1..])?;
    if value.is_empty() {
        return None;
    }
    if field.is_date() && DatePredicate::parse(&value).is_none() {
        return None;
    }
    Some((field, value, rest))
}

fn ident_len(input: &str) -> usize {
    input
        .chars()
        .take_while(|character| character.is_ascii_alphabetic())
        .count()
}

fn take_value(input: &str) -> Option<(String, &str)> {
    take_quoted(input).or_else(|| {
        let len = input
            .chars()
            .take_while(|character| !character.is_whitespace())
            .map(char::len_utf8)
            .sum();
        if len == 0 {
            None
        } else {
            Some((input[..len].to_owned(), &input[len..]))
        }
    })
}

fn take_term(input: &str) -> (String, &str) {
    take_quoted(input).map_or_else(
        || {
            let len = input
                .chars()
                .take_while(|character| !character.is_whitespace())
                .map(char::len_utf8)
                .sum();
            (input[..len].to_owned(), &input[len..])
        },
        |(term, rest)| (term, rest),
    )
}

fn take_quoted(input: &str) -> Option<(String, &str)> {
    let quote = input.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let mut output = String::new();
    let mut escaped = false;
    for (index, character) in input.char_indices().skip(1) {
        if escaped {
            output.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == quote {
            let rest = &input[index + character.len_utf8()..];
            return Some((output, rest));
        }
        output.push(character);
    }
    None
}

fn quote_if_needed(value: &str) -> String {
    if value
        .chars()
        .any(|character| character.is_whitespace() || character == ':' || character == '"')
    {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TicketKey;
    use crate::timestamp::ts;

    fn ticket(state: &str, work_item_type: &str, assignee: Option<&str>, tag: &str) -> Ticket {
        Ticket {
            key: TicketKey {
                organization: "demo".into(),
                id: 1,
            },
            project: "atlas".into(),
            revision: 1,
            work_item_type: work_item_type.into(),
            title: "Fix search".into(),
            state: state.into(),
            reason: None,
            assigned_to: assignee.map(str::to_owned),
            priority: Some(1),
            area_path: "Atlas\\Platform".into(),
            iteration_path: "Atlas\\Sprint 1".into(),
            tags: vec![tag.into()],
            description: String::new(),
            description_html: String::new(),
            created_at: ts("2026-01-01T00:00:00Z"),
            changed_at: ts("2026-01-02T00:00:00Z"),
            web_url: "https://dev.azure.com/demo/atlas/_workitems/edit/1".into(),
            details_rev: 0,
        }
    }

    fn dated(created: &str, changed: &str) -> Ticket {
        Ticket {
            created_at: ts(created),
            changed_at: ts(changed),
            ..ticket("Active", "Bug", Some("Avery"), "rust")
        }
    }

    #[test]
    fn the_query_grammar_parses_filters_and_formats_them_back() {
        let parsed = parse_query("state:active type:bug assignee:\"Avery Chen\" search");

        assert_eq!(parsed.fuzzy, "search");
        assert!(parsed.filters.contains(FilterField::State, "active"));
        assert!(parsed.filters.contains(FilterField::Type, "bug"));
        assert!(parsed.filters.contains(FilterField::Assignee, "Avery Chen"));

        let unknown = parse_query("foo:bar state:new");
        assert_eq!(unknown.fuzzy, "foo:bar");
        assert!(unknown.filters.contains(FilterField::State, "new"));

        let bookmarked = parse_query("is:bookmarked priority:1");
        assert!(bookmarked.filters.bookmarked);
        assert_eq!(
            format_query(&bookmarked.filters, "alpha"),
            "is:bookmarked priority:1 alpha"
        );
    }

    #[test]
    fn same_field_values_are_or_and_different_fields_are_and() {
        let parsed = parse_query("state:active state:new type:bug");
        let active = ticket("Active", "Bug", Some("Avery"), "rust");
        let news = ticket("New", "Bug", Some("Avery"), "rust");
        let task = ticket("Active", "Task", Some("Avery"), "rust");

        assert!(parsed.filters.matches(&active, false));
        assert!(parsed.filters.matches(&news, false));
        assert!(!parsed.filters.matches(&task, false));
    }

    #[test]
    fn unassigned_and_path_segments_and_tags_match() {
        let parsed = parse_query("assignee:unassigned area:Platform tag:rust");
        let matching = ticket("Active", "Bug", None, "rust");
        let other = ticket("Active", "Bug", Some("Avery"), "docs");

        assert!(parsed.filters.matches(&matching, false));
        assert!(!parsed.filters.matches(&other, false));
    }

    #[test]
    fn facet_counts_ignore_the_field_being_faceted() {
        let tickets = vec![
            ticket("Active", "Bug", Some("Avery"), "rust"),
            ticket("Active", "Task", Some("Avery"), "rust"),
            ticket("New", "Bug", Some("Avery"), "rust"),
        ];
        let filters = parse_query("type:bug").filters;
        let facets = facet_values(&tickets, &filters, FilterField::Type, |_| false);

        let bug = facets.iter().find(|facet| facet.value == "Bug").unwrap();
        let task = facets.iter().find(|facet| facet.value == "Task").unwrap();
        assert_eq!(bug.count, 2);
        assert_eq!(task.count, 1);
        assert!(bug.selected);
        assert!(!task.selected);
    }

    #[test]
    fn date_values_parse_every_unit_and_both_comparison_directions() {
        let now = ts("2026-08-29T12:00:00Z");
        let holds = |value: &str, changed: &str| {
            DatePredicate::parse(value)
                .unwrap_or_else(|| panic!("{value} should parse"))
                .matches(ts(changed), now)
        };

        assert!(holds("<30m", "2026-08-29T11:45:00Z"));
        assert!(!holds("<30m", "2026-08-29T11:15:00Z"));
        assert!(holds("<2h", "2026-08-29T10:30:00Z"));
        assert!(holds(">2h", "2026-08-29T09:00:00Z"));
        assert!(holds("<7d", "2026-08-25T12:00:00Z"));
        assert!(holds(">14d", "2026-08-01T12:00:00Z"));
        assert!(holds("<2w", "2026-08-20T12:00:00Z"));
        assert!(!holds(">2w", "2026-08-20T12:00:00Z"));
        assert!(holds("<24H", "2026-08-29T00:00:00Z"), "units ignore case");

        assert_eq!(
            DatePredicate::parse("7d"),
            None,
            "a bare duration carries no direction"
        );
        assert_eq!(DatePredicate::parse("<7y"), None, "years are not a unit");
        assert_eq!(DatePredicate::parse("<"), None);
        assert_eq!(DatePredicate::parse("<d"), None);
    }

    #[test]
    fn inclusive_bounds_take_the_edge_and_strict_ones_leave_it() {
        let now = ts("2026-08-29T12:00:00Z");
        let a_day_old = ts("2026-08-28T12:00:00Z");
        let holds = |value: &str| DatePredicate::parse(value).unwrap().matches(a_day_old, now);

        assert!(holds("<=24h"));
        assert!(holds(">=24h"));
        assert!(!holds("<24h"));
        assert!(!holds(">24h"));
    }

    #[test]
    fn absolute_date_values_compare_against_utc_midnight() {
        let now = ts("2026-08-29T12:00:00Z");
        let august = dated("2026-08-01T00:00:01Z", "2026-08-29T00:00:00Z");
        let july = dated("2026-07-30T00:00:00Z", "2026-08-29T00:00:00Z");

        let after = parse_query("created:>2026-08-01").filters;
        assert!(after.matches_at(&august, false, now));
        assert!(!after.matches_at(&july, false, now));

        let before = parse_query("created:<2026-08-01").filters;
        assert!(!before.matches_at(&august, false, now));
        assert!(before.matches_at(&july, false, now));
    }

    #[test]
    fn relative_date_filters_are_measured_from_the_instant_they_are_matched_against() {
        let today = parse_query("changed:<24h").filters;
        let stale = parse_query("changed:>14d").filters;
        let ticket = dated("2026-01-01T00:00:00Z", "2026-08-29T09:00:00Z");

        let same_day = ts("2026-08-29T12:00:00Z");
        assert!(today.matches_at(&ticket, false, same_day));
        assert!(!stale.matches_at(&ticket, false, same_day));

        let three_weeks_on = ts("2026-09-19T12:00:00Z");
        assert!(
            !today.matches_at(&ticket, false, three_weeks_on),
            "a saved query reads against the clock, not the day it was written"
        );
        assert!(stale.matches_at(&ticket, false, three_weeks_on));
    }

    #[test]
    fn several_date_values_in_one_field_or_together() {
        let now = ts("2026-08-29T12:00:00Z");
        let filters = parse_query("changed:<24h changed:>14d").filters;
        let fresh = dated("2026-01-01T00:00:00Z", "2026-08-29T11:00:00Z");
        let stale = dated("2026-01-01T00:00:00Z", "2026-08-01T00:00:00Z");
        let middling = dated("2026-01-01T00:00:00Z", "2026-08-25T00:00:00Z");

        assert!(filters.matches_at(&fresh, false, now));
        assert!(filters.matches_at(&stale, false, now));
        assert!(!filters.matches_at(&middling, false, now));
    }

    #[test]
    fn date_chips_read_as_typed_and_round_trip_through_the_query_text() {
        let parsed = parse_query("changed:<7d created:>2026-08-01 rust");
        let labels: Vec<String> = parsed
            .filters
            .tokens()
            .iter()
            .map(FilterToken::chip_label)
            .collect();

        assert_eq!(labels, vec!["changed:<7d", "created:>2026-08-01"]);

        let formatted = format_query(&parsed.filters, &parsed.fuzzy);
        assert_eq!(formatted, "changed:<7d created:>2026-08-01 rust");
        assert_eq!(parse_query(&formatted), parsed);
    }

    #[test]
    fn a_date_value_that_is_not_a_comparison_stays_fuzzy_text() {
        let parsed = parse_query("changed:soon state:active");

        assert_eq!(parsed.fuzzy, "changed:soon");
        assert!(
            parsed
                .filters
                .selected_values(FilterField::Changed)
                .is_empty()
        );
        assert!(parsed.filters.contains(FilterField::State, "active"));
    }

    #[test]
    fn the_filter_overlay_offers_date_presets_instead_of_an_enumerated_list() {
        let now = ts("2026-08-29T12:00:00Z");
        let tickets = vec![
            dated("2026-08-28T00:00:00Z", "2026-08-29T09:00:00Z"),
            dated("2026-01-01T00:00:00Z", "2026-08-01T00:00:00Z"),
        ];
        let filters = parse_query("changed:>14d").filters;
        let facets = facet_values_at(&tickets, &filters, FilterField::Changed, |_| false, now);
        let values: Vec<&str> = facets.iter().map(|facet| facet.value.as_str()).collect();

        assert_eq!(
            values,
            vec!["<24h", "<7d", "<14d", "<30d", ">14d"],
            "presets keep their order and a typed value stays un-checkable"
        );
        assert_eq!(facets[0].count, 1);
        assert_eq!(facets[3].count, 2);
        assert!(facets[4].selected);
        assert!(!facets[0].selected);
    }
}
