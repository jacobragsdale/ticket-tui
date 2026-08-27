use std::collections::{BTreeMap, BTreeSet};

use crate::model::Ticket;

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
}

impl FilterField {
    pub const ALL: [Self; 8] = [
        Self::State,
        Self::Type,
        Self::Assignee,
        Self::Priority,
        Self::Project,
        Self::Area,
        Self::Iteration,
        Self::Tags,
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
            _ => None,
        }
    }
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

    #[must_use]
    pub fn matches(&self, ticket: &Ticket, is_bookmarked: bool) -> bool {
        if self.bookmarked && !is_bookmarked {
            return false;
        }
        self.values.iter().all(|(field, values)| {
            values
                .iter()
                .any(|value| field_matches(*field, ticket, value))
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
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for ticket in tickets {
        if !matches_excluding(ticket, filters, field, bookmarked(ticket)) {
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

fn matches_excluding(
    ticket: &Ticket,
    filters: &FilterSet,
    excluded: FilterField,
    is_bookmarked: bool,
) -> bool {
    if filters.bookmarked && !is_bookmarked {
        return false;
    }
    filters.values.iter().all(|(field, values)| {
        *field == excluded
            || values
                .iter()
                .any(|value| field_matches(*field, ticket, value))
    })
}

fn field_matches(field: FilterField, ticket: &Ticket, needle: &str) -> bool {
    field_values(field, ticket)
        .iter()
        .any(|value| value.eq_ignore_ascii_case(needle) || path_segment_matches(value, needle))
}

fn path_segment_matches(value: &str, needle: &str) -> bool {
    value
        .rsplit(['\\', '/'])
        .next()
        .is_some_and(|segment| segment.eq_ignore_ascii_case(needle))
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
            created_at: crate::timestamp::ts("2026-01-01T00:00:00Z"),
            changed_at: crate::timestamp::ts("2026-01-02T00:00:00Z"),
            web_url: "https://dev.azure.com/demo/atlas/_workitems/edit/1".into(),
        }
    }

    #[test]
    fn parses_structured_filters_and_leaves_fuzzy_text() {
        let parsed = parse_query("state:active type:bug assignee:\"Avery Chen\" search");

        assert_eq!(parsed.fuzzy, "search");
        assert!(parsed.filters.contains(FilterField::State, "active"));
        assert!(parsed.filters.contains(FilterField::Type, "bug"));
        assert!(parsed.filters.contains(FilterField::Assignee, "Avery Chen"));
    }

    #[test]
    fn unknown_prefixes_remain_fuzzy_terms() {
        let parsed = parse_query("foo:bar state:new");

        assert_eq!(parsed.fuzzy, "foo:bar");
        assert!(parsed.filters.contains(FilterField::State, "new"));
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
    fn bookmarked_filter_and_round_trip_formatting() {
        let parsed = parse_query("is:bookmarked priority:1");
        assert!(parsed.filters.bookmarked);
        assert_eq!(
            format_query(&parsed.filters, "alpha"),
            "is:bookmarked priority:1 alpha"
        );
    }

    #[test]
    fn toggling_a_filter_removes_a_matching_value() {
        let mut filters = parse_query("state:Active").filters;
        filters.toggle(FilterField::State, "active");
        assert!(filters.is_empty());
        filters.toggle(FilterField::State, "Active");
        assert!(filters.contains(FilterField::State, "Active"));
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
}
