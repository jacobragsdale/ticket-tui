use std::collections::{BTreeMap, BTreeSet};

use crate::model::{StateCategory, Ticket, path_leaf, same_text};
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

/// A value written with a leading `@`, standing for something only the running
/// app knows: who is signed in, which sprint contains today, whether a state
/// counts as finished.
///
/// A sentinel is stored in the query exactly as it was typed and read at match
/// time, so a view saved as `assignee:@me` still means whoever is signed in
/// tomorrow and `iteration:@current` follows the sprint over its rollover.
/// This is the shape relative date bounds already take.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sentinel {
    /// `assignee:@me`, the display name the session is signed in under.
    Me,
    /// `assignee:@none`, work nobody owns.
    Nobody,
    /// `iteration:@current`, the sprint whose dates contain today.
    CurrentIteration,
    /// `state:@open`, anything the workflow has not finished with. Read by
    /// state category rather than by name, because every process template
    /// spells its finished states differently.
    Open,
}

impl Sentinel {
    /// The sentinel a value asks for on a given field. A sentinel written on a
    /// field that has none — `state:@me` — is nothing special: it stays an
    /// ordinary value, and so matches the states literally named that, of
    /// which there are none.
    #[must_use]
    pub fn parse(field: FilterField, value: &str) -> Option<Self> {
        let name = value.strip_prefix('@')?.to_ascii_lowercase();
        match (field, name.as_str()) {
            (FilterField::Assignee, "me") => Some(Self::Me),
            (FilterField::Assignee, "none") => Some(Self::Nobody),
            (FilterField::Iteration, "current") => Some(Self::CurrentIteration),
            (FilterField::State, "open") => Some(Self::Open),
            _ => None,
        }
    }

    /// How the sentinel is written in a query, which is how a rule the app
    /// applies on its own — hiding finished work — is spelled in the grammar
    /// the search box already reads.
    #[must_use]
    pub const fn as_value(self) -> &'static str {
        match self {
            Self::Me => "@me",
            Self::Nobody => "@none",
            Self::CurrentIteration => "@current",
            Self::Open => "@open",
        }
    }

    /// Whether a ticket satisfies the sentinel. One the context cannot fill in
    /// — nobody signed in, no sprint scheduled — matches nothing rather than
    /// everything, so a query never quietly widens to the whole project.
    fn matches(self, ticket: &Ticket, context: &MatchContext) -> bool {
        match self {
            Self::Me => context.me.as_deref().is_some_and(|me| {
                ticket
                    .assigned_to
                    .as_deref()
                    .is_some_and(|assignee| same_text(assignee, me))
            }),
            Self::Nobody => ticket.assigned_to.is_none(),
            Self::CurrentIteration => context
                .current_iteration
                .as_deref()
                .is_some_and(|iteration| same_text(&ticket.iteration_path, iteration)),
            Self::Open => !StateCategory::of(&ticket.state).is_done(),
        }
    }
}

/// What a query means at the moment it runs: the clock its relative date
/// bounds are measured from, and the values its sentinels stand for.
///
/// Everything here is resolved as the filter runs rather than as the query is
/// parsed, which is what lets a saved view follow the person and the calendar
/// instead of freezing whatever they meant the day it was written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchContext {
    /// The instant `changed:<7d` and its like are measured back from.
    pub now: Timestamp,
    /// The display name `@me` stands for, and `None` when nobody is signed in.
    pub me: Option<String>,
    /// The iteration path `@current` stands for, and `None` when no sprint is
    /// scheduled around today.
    pub current_iteration: Option<String>,
}

impl MatchContext {
    /// A context reading the wall clock, knowing nobody and no sprint.
    #[must_use]
    pub fn now() -> Self {
        Self::at(Timestamp::now())
    }

    /// The same against a fixed instant, which is how a relative bound is
    /// tested without reaching for the clock.
    #[must_use]
    pub const fn at(now: Timestamp) -> Self {
        Self {
            now,
            me: None,
            current_iteration: None,
        }
    }

    #[must_use]
    pub fn with_me(mut self, me: Option<String>) -> Self {
        self.me = me;
        self
    }

    #[must_use]
    pub fn with_current_iteration(mut self, iteration: Option<String>) -> Self {
        self.current_iteration = iteration;
        self
    }
}

/// The `changed:` comparison a stale-item highlight is, written the way the
/// query language spells it: `>14d`, an age of more than fourteen days.
///
/// The highlight and the filter cannot disagree about which items are old,
/// because this is the text they both parse.
#[must_use]
pub fn stale_bound(days: u16) -> String {
    format!(">{days}d")
}

/// The whole query a stale-item highlight stands for, which is what the
/// palette reports back after moving the threshold and what the built-in
/// **Stale** view asks for.
#[must_use]
pub fn stale_query(days: u16) -> String {
    format!(
        "{}:{} {}:@open",
        FilterField::Changed.key(),
        stale_bound(days),
        FilterField::State.key()
    )
}

/// Whether nobody has touched a work item for longer than `days` and the
/// workflow is still expecting somebody to.
///
/// Finished work is never stale however long it has sat: nothing is waiting on
/// a work item that is done or removed, so the state category is asked before
/// the clock is. The age half is [`DatePredicate`] reading [`stale_bound`], so
/// a flagged row is exactly a row `changed:>14d` lists.
///
/// The bound is exclusive, as `>` is everywhere else: a work item touched
/// exactly `days` ago has not yet crossed it.
#[must_use]
pub fn is_stale(ticket: &Ticket, days: u16, now: Timestamp) -> bool {
    !StateCategory::of(&ticket.state).is_done()
        && DatePredicate::parse(&stale_bound(days))
            .is_some_and(|predicate| predicate.matches(ticket.changed_at, now))
}

/// Whole days since a work item was last touched, which is the number the
/// details pane reports beside the instant. An item changed in the future —
/// clocks disagree — reads as zero rather than as a negative age.
#[must_use]
pub fn days_untouched(ticket: &Ticket, now: Timestamp) -> i64 {
    ticket.changed_at.seconds_until(now) / (24 * 60 * 60)
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

    /// Whether a ticket passes every field of the query, read against the
    /// current instant and against nobody in particular.
    #[must_use]
    pub fn matches(&self, ticket: &Ticket, is_bookmarked: bool) -> bool {
        self.matches_in(ticket, is_bookmarked, &MatchContext::now())
    }

    /// `matches` with everything a sentinel needs to stand for something: the
    /// clock, the signed-in name, and the sprint containing today.
    #[must_use]
    pub fn matches_in(&self, ticket: &Ticket, is_bookmarked: bool, context: &MatchContext) -> bool {
        if self.bookmarked && !is_bookmarked {
            return false;
        }
        self.values.iter().all(|(field, values)| {
            values
                .iter()
                .any(|value| field_matches(*field, ticket, value, context))
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
    context: &MatchContext,
) -> Vec<FacetValue> {
    if field.is_date() {
        return date_facets(tickets, filters, field, bookmarked, context);
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for ticket in tickets {
        if !matches_excluding(ticket, filters, field, bookmarked(ticket), context) {
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
    context: &MatchContext,
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
        .filter(|ticket| matches_excluding(ticket, filters, field, bookmarked(ticket), context))
        .collect();
    values
        .into_iter()
        .map(|value| FacetValue {
            selected: filters.contains(field, &value),
            count: remaining
                .iter()
                .filter(|ticket| field_matches(field, ticket, &value, context))
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
    context: &MatchContext,
) -> bool {
    if filters.bookmarked && !is_bookmarked {
        return false;
    }
    filters.values.iter().all(|(field, values)| {
        *field == excluded
            || values
                .iter()
                .any(|value| field_matches(*field, ticket, value, context))
    })
}

fn field_matches(
    field: FilterField,
    ticket: &Ticket,
    needle: &str,
    context: &MatchContext,
) -> bool {
    if let Some(instant) = date_value(field, ticket) {
        return DatePredicate::parse(needle)
            .is_some_and(|predicate| predicate.matches(instant, context.now));
    }
    if let Some(sentinel) = Sentinel::parse(field, needle) {
        return sentinel.matches(ticket, context);
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
    path_leaf(value).eq_ignore_ascii_case(needle)
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
    let (key, after_colon) = take_key(input)?;
    if !key.eq_ignore_ascii_case("is") {
        return None;
    }
    let (value, rest) = take_value(after_colon)?;
    if value.eq_ignore_ascii_case("bookmarked") || value.eq_ignore_ascii_case("bookmark") {
        filters.bookmarked = true;
        Some(rest)
    } else {
        None
    }
}

fn take_field_filter(input: &str) -> Option<(FilterField, String, &str)> {
    let (key, after_colon) = take_key(input)?;
    let field = FilterField::parse(key)?;
    let (value, rest) = take_value(after_colon)?;
    if value.is_empty() {
        return None;
    }
    if field.is_date() && DatePredicate::parse(&value).is_none() {
        return None;
    }
    Some((field, value, rest))
}

/// The `key:` a `field:value` pair opens with, and the input after the colon.
/// `None` when the input does not start with a word and a colon.
fn take_key(input: &str) -> Option<(&str, &str)> {
    let len = input
        .chars()
        .take_while(|character| character.is_ascii_alphabetic())
        .count();
    if len == 0 {
        return None;
    }
    let (key, rest) = input.split_at(len);
    rest.strip_prefix(':').map(|after_colon| (key, after_colon))
}

/// One value: quoted, or bare up to the next space. `None` for an empty one.
fn take_value(input: &str) -> Option<(String, &str)> {
    let (value, rest) = take_term(input);
    (!value.is_empty()).then_some((value, rest))
}

/// One term: quoted, or bare up to the next space, which for an input that
/// opens with a space is the empty term.
fn take_term(input: &str) -> (String, &str) {
    take_quoted(input).unwrap_or_else(|| {
        let len = input
            .chars()
            .take_while(|character| !character.is_whitespace())
            .map(char::len_utf8)
            .sum();
        (input[..len].to_owned(), &input[len..])
    })
}

/// One quoted value, with the quotes taken off and the escapes undone, and the
/// rest of the input after the closing quote. `None` when the input does not
/// open with a quote or never closes it, which is what sends the caller to the
/// bare reader.
///
/// A backslash escapes only another backslash or the quote that opened the
/// value. Before anything else it is a literal backslash, because every area
/// and iteration path Azure DevOps hands back is backslash-separated and
/// `iteration:"development\Sprint 1"` is what somebody types. The escaped
/// spelling keeps its meaning, so what [`quote_if_needed`] writes still reads
/// back unchanged.
fn take_quoted(input: &str) -> Option<(String, &str)> {
    let quote = input.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let mut output = String::new();
    let mut escaped = false;
    for (index, character) in input.char_indices().skip(1) {
        if escaped {
            if character != '\\' && character != quote {
                output.push('\\');
            }
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

/// One filter value, written so [`take_value`] reads it back unchanged.
///
/// A value is quoted when leaving it bare would split it or read as something
/// else, and inside the quotes a backslash and the quote character are both
/// escaped, because that is what [`take_quoted`] undoes. Escaping the
/// backslash first matters: doing it second would escape the ones just written
/// in front of the quotes. Outside quotes nothing is escaped, and nothing needs
/// to be — the bare reader takes every character up to the next space, the
/// backslash in an unspaced area path included.
fn quote_if_needed(value: &str) -> String {
    if value
        .chars()
        .any(|character| character.is_whitespace() || character == ':' || character == '"')
    {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
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
    fn a_value_holding_a_backslash_or_a_quote_round_trips_through_the_query_text() {
        // Every area and iteration path Azure DevOps hands back is
        // backslash-separated, so the writer and the reader have to agree about
        // what a backslash inside quotes means.
        for value in [
            "Atlas\\Sprint 1",
            "Atlas\\Sub\\Sprint 1",
            "say \"when\" now",
            "Atlas\\say \"when\"",
            "Atlas\\Sprint 1\\",
            "Atlas\\Sprint1",
            "Atlas\\Sprint1\\",
        ] {
            let mut filters = FilterSet::default();
            filters.insert(FilterField::Iteration, value.to_owned());

            let written = format_query(&filters, "");
            let read = parse_query(&written);

            assert_eq!(
                read.filters, filters,
                "{value:?} did not survive the round trip through {written:?}"
            );
            assert!(read.fuzzy.is_empty(), "{value:?} leaked into {written:?}");
        }
    }

    #[test]
    fn a_backslash_typed_once_in_a_quoted_value_is_kept() {
        // What somebody types by hand, and what the writer emits for the same
        // value, both have to land on the path itself.
        for typed in [
            "iteration:\"Atlas\\Sprint 1\"",
            "iteration:\"Atlas\\\\Sprint 1\"",
        ] {
            let parsed = parse_query(typed);
            assert!(
                parsed
                    .filters
                    .contains(FilterField::Iteration, "Atlas\\Sprint 1"),
                "{typed} should select the Sprint 1 path, got {:?}",
                parsed.filters
            );
            assert!(parsed.fuzzy.is_empty(), "{typed} left fuzzy text behind");
        }

        // The quote character keeps its escape, so a value can still hold one.
        let quoted = parse_query("state:\"say \\\"when\\\"\"");
        assert!(quoted.filters.contains(FilterField::State, "say \"when\""));

        // A quote that never closes is not a quoted value at all, and the bare
        // reader takes it as written.
        let unterminated = parse_query("state:\"Atlas\\Sprint");
        assert!(
            unterminated
                .filters
                .contains(FilterField::State, "\"Atlas\\Sprint"),
            "{:?}",
            unterminated.filters
        );

        // take_term shares the reader, so a fuzzy term behaves the same way.
        assert_eq!(parse_query("\"Atlas\\Sprint 1\"").fuzzy, "Atlas\\Sprint 1");
    }

    #[test]
    fn toggling_an_iteration_facet_selects_the_rows_it_was_built_from() {
        let here = ticket("Active", "Bug", Some("Avery"), "rust");
        let elsewhere = Ticket {
            iteration_path: "Atlas\\Sprint 2".into(),
            ..here.clone()
        };
        let tickets = [here.clone(), elsewhere.clone()];
        let now = ts("2026-02-01T00:00:00Z");
        let context = MatchContext::at(now);

        // The facet lists the full path, which is what a toggle writes.
        let facets = facet_values(
            &tickets,
            &FilterSet::default(),
            FilterField::Iteration,
            |_| false,
            &context,
        );
        let paths: Vec<&str> = facets.iter().map(|facet| facet.value.as_str()).collect();
        assert_eq!(paths, ["Atlas\\Sprint 1", "Atlas\\Sprint 2"]);

        let mut filters = FilterSet::default();
        filters.toggle(FilterField::Iteration, "Atlas\\Sprint 1");
        let reread = parse_query(&format_query(&filters, "")).filters;

        assert!(
            reread.matches_in(&here, false, &MatchContext::at(now)),
            "the row the facet was built from is still selected"
        );
        assert!(
            !reread.matches_in(&elsewhere, false, &MatchContext::at(now)),
            "a sibling sprint is not"
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
        let facets = facet_values(
            &tickets,
            &filters,
            FilterField::Type,
            |_| false,
            &MatchContext::now(),
        );

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
        assert!(after.matches_in(&august, false, &MatchContext::at(now)));
        assert!(!after.matches_in(&july, false, &MatchContext::at(now)));

        let before = parse_query("created:<2026-08-01").filters;
        assert!(!before.matches_in(&august, false, &MatchContext::at(now)));
        assert!(before.matches_in(&july, false, &MatchContext::at(now)));
    }

    #[test]
    fn relative_date_filters_are_measured_from_the_instant_they_are_matched_against() {
        let today = parse_query("changed:<24h").filters;
        let stale = parse_query("changed:>14d").filters;
        let ticket = dated("2026-01-01T00:00:00Z", "2026-08-29T09:00:00Z");

        let same_day = ts("2026-08-29T12:00:00Z");
        assert!(today.matches_in(&ticket, false, &MatchContext::at(same_day)));
        assert!(!stale.matches_in(&ticket, false, &MatchContext::at(same_day)));

        let three_weeks_on = ts("2026-09-19T12:00:00Z");
        assert!(
            !today.matches_in(&ticket, false, &MatchContext::at(three_weeks_on)),
            "a saved query reads against the clock, not the day it was written"
        );
        assert!(stale.matches_in(&ticket, false, &MatchContext::at(three_weeks_on)));
    }

    #[test]
    fn the_stale_threshold_is_exclusive_so_the_boundary_day_is_not_yet_stale() {
        let now = ts("2026-08-29T12:00:00Z");
        let changed = |changed: &str| dated("2026-01-01T00:00:00Z", changed);

        assert!(
            !is_stale(&changed("2026-08-15T12:00:00Z"), 14, now),
            "exactly fourteen days is the bound, and > does not take its edge"
        );
        assert!(
            is_stale(&changed("2026-08-15T11:59:59Z"), 14, now),
            "a second past the bound crosses it"
        );
        assert!(is_stale(&changed("2026-08-14T12:00:00Z"), 14, now));
        assert!(!is_stale(&changed("2026-08-28T12:00:00Z"), 14, now));
    }

    #[test]
    fn finished_work_is_never_stale_however_long_it_has_sat() {
        let now = ts("2026-08-29T12:00:00Z");
        let ancient = |state: &str| Ticket {
            changed_at: ts("2025-01-01T00:00:00Z"),
            ..ticket(state, "Bug", Some("Avery"), "rust")
        };

        assert!(is_stale(&ancient("To Do"), 14, now));
        assert!(is_stale(&ancient("Active"), 14, now));
        assert!(is_stale(&ancient("Resolved"), 14, now));
        assert!(
            !is_stale(&ancient("Done"), 14, now),
            "nobody is waiting on completed work"
        );
        assert!(!is_stale(&ancient("Closed"), 14, now));
        assert!(!is_stale(&ancient("Removed"), 14, now));
    }

    #[test]
    fn the_stale_highlight_and_the_changed_filter_agree_on_the_same_data() {
        let now = ts("2026-08-29T12:00:00Z");
        let items = [
            dated("2026-01-01T00:00:00Z", "2026-08-29T11:00:00Z"),
            dated("2026-01-01T00:00:00Z", "2026-08-15T12:00:00Z"),
            dated("2026-01-01T00:00:00Z", "2026-08-14T12:00:00Z"),
            dated("2026-01-01T00:00:00Z", "2026-01-02T00:00:00Z"),
        ];
        let age_only = parse_query("changed:>14d").filters;
        let whole = parse_query(&stale_query(14)).filters;

        for item in &items {
            assert_eq!(
                is_stale(item, 14, now),
                age_only.matches_in(item, false, &MatchContext::at(now)),
                "the highlight is the changed:>14d comparison for open work: {item:?}"
            );
            assert_eq!(
                is_stale(item, 14, now),
                whole.matches_in(item, false, &MatchContext::at(now)),
                "and the whole query for it is what the palette reports: {item:?}"
            );
        }

        let finished = Ticket {
            state: "Done".into(),
            ..dated("2026-01-01T00:00:00Z", "2026-01-02T00:00:00Z")
        };
        assert!(
            age_only.matches_in(&finished, false, &MatchContext::at(now))
                && !is_stale(&finished, 14, now),
            "the age halves agree; only state:@open holds the finished item back"
        );
        assert!(!whole.matches_in(&finished, false, &MatchContext::at(now)));
    }

    #[test]
    fn the_stale_query_reads_as_the_built_in_view_writes_it() {
        assert_eq!(stale_bound(21), ">21d");
        assert_eq!(stale_query(21), "changed:>21d state:@open");
        assert!(
            DatePredicate::parse(&stale_bound(21)).is_some(),
            "the bound the highlight builds is one the query language parses"
        );
    }

    #[test]
    fn days_untouched_counts_whole_days_and_never_goes_negative() {
        let now = ts("2026-08-29T12:00:00Z");
        let changed = |changed: &str| dated("2026-01-01T00:00:00Z", changed);

        assert_eq!(days_untouched(&changed("2026-08-08T12:00:00Z"), now), 21);
        assert_eq!(
            days_untouched(&changed("2026-08-08T11:00:00Z"), now),
            21,
            "a part day does not round up to the next one"
        );
        assert_eq!(days_untouched(&changed("2026-08-29T11:00:00Z"), now), 0);
        assert_eq!(
            days_untouched(&changed("2026-09-30T12:00:00Z"), now),
            0,
            "a work item changed in the future has no age to report"
        );
    }

    #[test]
    fn several_date_values_in_one_field_or_together() {
        let now = ts("2026-08-29T12:00:00Z");
        let filters = parse_query("changed:<24h changed:>14d").filters;
        let fresh = dated("2026-01-01T00:00:00Z", "2026-08-29T11:00:00Z");
        let stale = dated("2026-01-01T00:00:00Z", "2026-08-01T00:00:00Z");
        let middling = dated("2026-01-01T00:00:00Z", "2026-08-25T00:00:00Z");

        assert!(filters.matches_in(&fresh, false, &MatchContext::at(now)));
        assert!(filters.matches_in(&stale, false, &MatchContext::at(now)));
        assert!(!filters.matches_in(&middling, false, &MatchContext::at(now)));
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
        let facets = facet_values(
            &tickets,
            &filters,
            FilterField::Changed,
            |_| false,
            &MatchContext::at(now),
        );
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

    #[test]
    fn the_me_sentinel_stands_for_whoever_the_context_is_signed_in_as() {
        let now = ts("2026-08-29T12:00:00Z");
        let filters = parse_query("assignee:@me").filters;
        let mine = ticket("Active", "Bug", Some("  avery CHEN "), "rust");
        let theirs = ticket("Active", "Bug", Some("Jordan Patel"), "rust");
        let nobodys = ticket("Active", "Bug", None, "rust");

        let signed_in = MatchContext::at(now).with_me(Some("Avery Chen".into()));
        assert!(
            filters.matches_in(&mine, false, &signed_in),
            "casing and padding do not make it somebody else"
        );
        assert!(!filters.matches_in(&theirs, false, &signed_in));
        assert!(!filters.matches_in(&nobodys, false, &signed_in));

        let signed_out = MatchContext::at(now);
        assert!(
            !filters.matches_in(&mine, false, &signed_out),
            "with no name known @me is nobody rather than everybody"
        );
        assert!(!filters.matches_in(&nobodys, false, &signed_out));
    }

    #[test]
    fn the_me_sentinel_follows_the_name_it_is_handed_rather_than_the_one_it_was_saved_beside() {
        let now = ts("2026-08-29T12:00:00Z");
        let filters = parse_query("assignee:@me").filters;
        let jordans = ticket("Active", "Bug", Some("Jordan Patel"), "rust");

        assert!(!filters.matches_in(
            &jordans,
            false,
            &MatchContext::at(now).with_me(Some("Avery Chen".into()))
        ));
        assert!(
            filters.matches_in(
                &jordans,
                false,
                &MatchContext::at(now).with_me(Some("Jordan Patel".into()))
            ),
            "the same saved query means somebody else once somebody else is signed in"
        );
    }

    #[test]
    fn the_none_sentinel_keeps_only_the_work_nobody_owns() {
        let now = ts("2026-08-29T12:00:00Z");
        let filters = parse_query("assignee:@none").filters;
        let context = MatchContext::at(now).with_me(Some("Avery Chen".into()));

        assert!(filters.matches_in(&ticket("Active", "Bug", None, "rust"), false, &context));
        assert!(!filters.matches_in(
            &ticket("Active", "Bug", Some("Avery Chen"), "rust"),
            false,
            &context
        ));
    }

    #[test]
    fn the_current_sentinel_follows_the_sprint_the_context_names() {
        let now = ts("2026-08-29T12:00:00Z");
        let filters = parse_query("iteration:@current").filters;
        let sprint_one = ticket("Active", "Bug", Some("Avery"), "rust");
        let sprint_two = Ticket {
            iteration_path: "Atlas\\Sprint 2".into(),
            ..sprint_one.clone()
        };

        let first = MatchContext::at(now).with_current_iteration(Some("Atlas\\Sprint 1".into()));
        assert!(filters.matches_in(&sprint_one, false, &first));
        assert!(!filters.matches_in(&sprint_two, false, &first));

        let rolled_over =
            MatchContext::at(now).with_current_iteration(Some("Atlas\\Sprint 2".into()));
        assert!(!filters.matches_in(&sprint_one, false, &rolled_over));
        assert!(
            filters.matches_in(&sprint_two, false, &rolled_over),
            "the query is unchanged; the sprint under it moved on"
        );

        assert!(
            !filters.matches_in(&sprint_one, false, &MatchContext::at(now)),
            "with no sprint scheduled @current is no sprint at all"
        );
    }

    #[test]
    fn the_open_sentinel_reads_the_state_category_rather_than_the_state_name() {
        let now = ts("2026-08-29T12:00:00Z");
        let filters = parse_query("state:@open").filters;
        let context = MatchContext::at(now);
        let in_state = |state: &str| {
            filters.matches_in(
                &ticket(state, "Bug", Some("Avery"), "rust"),
                false,
                &context,
            )
        };

        assert!(in_state("To Do"));
        assert!(in_state("Doing"));
        assert!(in_state("Active"));
        assert!(in_state("Resolved"));
        assert!(
            in_state("Needs triage"),
            "an unclassified state is not over"
        );
        assert!(!in_state("Done"));
        assert!(!in_state("Closed"));
        assert!(!in_state("Removed"));
    }

    #[test]
    fn a_sentinel_written_on_a_field_that_has_none_stays_an_ordinary_value() {
        let now = ts("2026-08-29T12:00:00Z");
        let context = MatchContext::at(now).with_me(Some("Avery Chen".into()));
        let subject = ticket("Active", "Bug", Some("Avery Chen"), "rust");

        assert_eq!(Sentinel::parse(FilterField::Type, "@me"), None);
        assert!(
            !parse_query("type:@me")
                .filters
                .matches_in(&subject, false, &context),
            "no work item type is named @me"
        );
        assert_eq!(Sentinel::parse(FilterField::Assignee, "@nobody"), None);
        assert_eq!(
            Sentinel::parse(FilterField::Assignee, "@ME"),
            Some(Sentinel::Me),
            "a sentinel is read without regard to case"
        );
    }

    #[test]
    fn sentinel_chips_read_as_typed_and_round_trip_through_the_query_text() {
        let parsed = parse_query("assignee:@me iteration:@current state:@open changed:>14d rust");
        let labels: Vec<String> = parsed
            .filters
            .tokens()
            .iter()
            .map(FilterToken::chip_label)
            .collect();

        assert_eq!(
            labels,
            vec![
                "state:@open",
                "assignee:@me",
                "iteration:@current",
                "changed:>14d"
            ]
        );

        let formatted = format_query(&parsed.filters, &parsed.fuzzy);
        assert_eq!(
            formatted,
            "state:@open assignee:@me iteration:@current changed:>14d rust"
        );
        assert_eq!(parse_query(&formatted), parsed);
    }

    #[test]
    fn facet_counts_are_taken_against_the_sentinels_the_rest_of_the_query_holds() {
        let now = ts("2026-08-29T12:00:00Z");
        let tickets = vec![
            ticket("Active", "Bug", Some("Avery Chen"), "rust"),
            ticket("Active", "Task", Some("Avery Chen"), "rust"),
            ticket("Active", "Bug", Some("Jordan Patel"), "rust"),
        ];
        let filters = parse_query("assignee:@me").filters;
        let facets = facet_values(
            &tickets,
            &filters,
            FilterField::Type,
            |_| false,
            &MatchContext::at(now).with_me(Some("Avery Chen".into())),
        );

        let count = |value: &str| {
            facets
                .iter()
                .find(|facet| facet.value == value)
                .map_or(0, |facet| facet.count)
        };
        assert_eq!(count("Bug"), 1, "Jordan's bug is not mine");
        assert_eq!(count("Task"), 1);
    }
}
