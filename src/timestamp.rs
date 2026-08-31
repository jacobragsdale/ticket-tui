use std::fmt;

use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{Date, OffsetDateTime, UtcOffset};

const DATE_ONLY: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]");
const CALENDAR_DAY: &[time::format_description::FormatItem<'static>] =
    format_description!("[month repr:short] [day padding:none]");
const EXACT_UTC: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second] UTC");
const ISO_UTC: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");

/// A UTC instant parsed from ticket data.
///
/// Stored values are normalized to UTC so sorting and display do not depend on
/// lexical RFC 3339 order or a leading `YYYY-MM-DD` slice.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Timestamp {
    instant: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimestampError {
    raw: String,
}

impl Timestamp {
    /// The current instant, in UTC.
    #[must_use]
    pub fn now() -> Self {
        Self::from_offset_date_time(OffsetDateTime::now_utc())
    }

    #[must_use]
    pub fn from_offset_date_time(instant: OffsetDateTime) -> Self {
        Self {
            instant: instant.to_offset(UtcOffset::UTC),
        }
    }

    pub fn parse(raw: &str) -> Result<Self, TimestampError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(TimestampError {
                raw: raw.to_owned(),
            });
        }
        if let Ok(parsed) = OffsetDateTime::parse(trimmed, &Rfc3339) {
            return Ok(Self::from_offset_date_time(parsed));
        }

        let mut candidate = trimmed.replace(' ', "T");
        if !has_zone_suffix(&candidate) {
            candidate.push('Z');
        }
        if let Ok(parsed) = OffsetDateTime::parse(&candidate, &Rfc3339) {
            return Ok(Self::from_offset_date_time(parsed));
        }

        if let Ok(date) = Date::parse(trimmed, DATE_ONLY) {
            return Ok(Self::from_offset_date_time(date.midnight().assume_utc()));
        }

        Err(TimestampError {
            raw: trimmed.to_owned(),
        })
    }

    #[must_use]
    pub fn to_rfc3339(self) -> String {
        self.instant
            .format(&Rfc3339)
            .unwrap_or_else(|_| self.calendar_date())
    }

    /// The UTC calendar day this instant falls on, which is what an iteration's
    /// start and finish dates are compared against: a sprint finishing on
    /// `2026-09-05T00:00:00Z` still runs for the whole of September 5th.
    #[must_use]
    pub const fn date(self) -> Date {
        self.instant.date()
    }

    /// The compact day an iteration's date range reads in, such as `Aug 25`.
    #[must_use]
    pub fn calendar_day(self) -> String {
        self.instant
            .format(CALENDAR_DAY)
            .unwrap_or_else(|_| self.calendar_date())
    }

    /// Whole seconds from this instant to `later`, and zero when `later` is not
    /// after it. This is how a cache's age is measured against the clock
    /// without either side needing to reach for `time` itself.
    #[must_use]
    pub fn seconds_until(self, later: Self) -> i64 {
        (later.instant - self.instant).whole_seconds().max(0)
    }

    /// This instant `seconds` later, which is how a query names a date that has
    /// not arrived yet. A span so long it leaves the calendar keeps the instant
    /// it started from rather than wrapping.
    #[must_use]
    pub fn plus_seconds(self, seconds: i64) -> Self {
        self.instant
            .checked_add(time::Duration::seconds(seconds))
            .map_or(self, Self::from_offset_date_time)
    }

    #[must_use]
    pub fn calendar_date(self) -> String {
        self.instant
            .format(DATE_ONLY)
            .unwrap_or_else(|_| self.instant.to_string())
    }

    #[must_use]
    pub fn exact_utc(self) -> String {
        self.instant
            .format(EXACT_UTC)
            .unwrap_or_else(|_| self.to_rfc3339())
    }

    /// The instant as an ISO 8601 UTC literal down to the second, which is the
    /// form a WIQL date comparison takes. Sub-second precision is dropped
    /// rather than rounded, so a `>=` watermark can only ever look further
    /// back than the instant it came from, never past an edit.
    #[must_use]
    pub fn to_iso8601_utc(self) -> String {
        self.instant
            .format(ISO_UTC)
            .unwrap_or_else(|_| self.to_rfc3339())
    }

    #[must_use]
    pub fn relative_to(self, now: OffsetDateTime) -> String {
        let changed = self.instant;
        let age = now - changed;
        if age.is_negative() {
            return self.calendar_date();
        }
        if age.whole_minutes() < 1 {
            return "now".into();
        }
        if age.whole_hours() < 1 {
            return format!("{}m", age.whole_minutes());
        }
        if age.whole_days() < 1 {
            return format!("{}h", age.whole_hours());
        }
        if age.whole_days() < 7 {
            return format!("{}d", age.whole_days());
        }
        if changed.year() == now.year() {
            return changed
                .format(CALENDAR_DAY)
                .unwrap_or_else(|_| self.calendar_date());
        }
        self.calendar_date()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_rfc3339())
    }
}

impl fmt::Display for TimestampError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid timestamp {:?}; expected RFC 3339, 'YYYY-MM-DD HH:MM:SS', or 'YYYY-MM-DD'",
            self.raw
        )
    }
}

impl std::error::Error for TimestampError {}

fn has_zone_suffix(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes
        .last()
        .is_some_and(|byte| *byte == b'Z' || *byte == b'z')
    {
        return true;
    }
    if bytes.len() >= 6 {
        let suffix = &bytes[bytes.len() - 6..];
        if (suffix[0] == b'+' || suffix[0] == b'-') && suffix[3] == b':' {
            return true;
        }
    }
    false
}

#[cfg(test)]
pub(crate) fn ts(raw: &str) -> Timestamp {
    Timestamp::parse(raw).unwrap_or_else(|error| panic!("{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn parse_normalizes_offsets_and_accepts_space_and_date_only_values() {
        let timestamp = ts("2026-08-26T13:00:00-05:00");

        assert_eq!(timestamp.exact_utc(), "2026-08-26 18:00:00 UTC");
        assert_eq!(timestamp, ts("2026-08-26T18:00:00Z"));

        assert_eq!(ts("2026-08-26 18:00:00"), ts("2026-08-26T18:00:00Z"));
        assert_eq!(ts("2026-08-26"), ts("2026-08-26T00:00:00Z"));
    }

    #[test]
    fn iso8601_literals_are_utc_and_truncated_to_the_second() {
        assert_eq!(
            ts("2026-08-26T13:00:00-05:00").to_iso8601_utc(),
            "2026-08-26T18:00:00Z"
        );
        assert_eq!(
            ts("2026-08-26T18:00:00.987654Z").to_iso8601_utc(),
            "2026-08-26T18:00:00Z",
            "truncating never steps past an edit made in the same second"
        );
    }

    #[test]
    fn parse_rejects_empty_and_unrecognized_values() {
        assert!(Timestamp::parse("").is_err());
        assert!(Timestamp::parse("yesterday").is_err());
        assert!(Timestamp::parse("26/08/2026").is_err());
    }

    #[test]
    fn relative_labels_use_the_normalized_calendar_date() {
        let now = datetime!(2026-08-26 18:00 UTC);

        assert_eq!(ts("2026-08-26T17:30:00Z").relative_to(now), "30m");
        assert_eq!(ts("2026-08-26T12:00:00Z").relative_to(now), "6h");
        assert_eq!(ts("2026-08-23T18:00:00Z").relative_to(now), "3d");
        assert_eq!(ts("2026-07-01T00:00:00Z").relative_to(now), "Jul 1");
        assert_eq!(
            ts("2025-07-01T22:00:00-05:00").relative_to(now),
            "2025-07-02"
        );
    }
}
