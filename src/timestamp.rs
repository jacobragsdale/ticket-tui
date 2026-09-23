use std::fmt;

use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{Date, OffsetDateTime, UtcOffset};

const DATE_ONLY: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]");
const CALENDAR_DAY: &[time::format_description::FormatItem<'static>] =
    format_description!("[month repr:short] [day padding:none]");
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

    /// The calendar day this instant falls on by the local clock, which is
    /// what "today" is when a sprint is read against it: in Chicago a sprint
    /// finishing on September 5th is still current at 8 pm that evening, when
    /// the UTC day is already the 6th.
    #[must_use]
    pub fn local_date(self) -> Date {
        self.date_in(&local_zone())
    }

    fn date_in(self, zone: &jiff::tz::TimeZone) -> Date {
        let offset = UtcOffset::from_whole_seconds(self.in_zone(zone).offset().seconds())
            .unwrap_or(UtcOffset::UTC);
        self.instant.to_offset(offset).date()
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
        // Past the calendar's end the answer is the calendar's end, so a
        // span that overflows still points the way it was asked to.
        self.instant
            .checked_add(time::Duration::seconds(seconds))
            .map_or_else(
                || {
                    if seconds >= 0 {
                        Self::from_offset_date_time(time::PrimitiveDateTime::MAX.assume_utc())
                    } else {
                        Self::from_offset_date_time(time::PrimitiveDateTime::MIN.assume_utc())
                    }
                },
                Self::from_offset_date_time,
            )
    }

    #[must_use]
    pub fn calendar_date(self) -> String {
        self.instant
            .format(DATE_ONLY)
            .unwrap_or_else(|_| self.instant.to_string())
    }

    /// The instant down to the second on the local clock, with the zone it
    /// reads in: `2026-09-05 23:49:22 CDT`. Everything a person reads is in
    /// local time; what a program reads — JSON, the agent context, WIQL — stays
    /// RFC 3339 in UTC.
    #[must_use]
    pub fn exact_local(self) -> String {
        self.exact_in(&local_zone())
    }

    fn exact_in(self, zone: &jiff::tz::TimeZone) -> String {
        self.in_zone(zone)
            .strftime("%Y-%m-%d %H:%M:%S %Z")
            .to_string()
    }

    /// This instant on the local clock, in the zone the system is set to —
    /// `TZ` first, then `/etc/localtime` — with its daylight saving rules
    /// applied at the instant itself, so a winter timestamp read in summer
    /// keeps its winter offset.
    fn local(self) -> jiff::Zoned {
        self.in_zone(&local_zone())
    }

    fn in_zone(self, zone: &jiff::tz::TimeZone) -> jiff::Zoned {
        jiff::Timestamp::new(
            self.instant.unix_timestamp(),
            i32::try_from(self.instant.nanosecond()).unwrap_or(0),
        )
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .to_zoned(zone.clone())
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

    /// How long ago this was, as a table cell reads it: `now`, `5m`, `3h`,
    /// `4d`, then the local calendar day — `Sep 5` this year, `2025-11-02`
    /// before it.
    #[must_use]
    pub fn relative_to(self, now: OffsetDateTime) -> String {
        let changed = self.instant;
        let age = now - changed;
        let local = self.local();
        let local_date = || local.strftime("%Y-%m-%d").to_string();
        if age.is_negative() {
            return local_date();
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
        if local.year() == Self::from_offset_date_time(now).local().year() {
            return local.strftime("%b %-d").to_string();
        }
        local_date()
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

/// The zone local time reads in: the system's, looked up once. A test run
/// reads UTC whatever machine it is on, so what it asserts does not move with
/// the clock on the wall.
#[cfg(not(test))]
fn local_zone() -> jiff::tz::TimeZone {
    static ZONE: std::sync::OnceLock<jiff::tz::TimeZone> = std::sync::OnceLock::new();
    ZONE.get_or_init(jiff::tz::TimeZone::system).clone()
}

#[cfg(test)]
fn local_zone() -> jiff::tz::TimeZone {
    jiff::tz::TimeZone::UTC
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

        assert_eq!(timestamp.exact_local(), "2026-08-26 18:00:00 UTC");
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

    #[test]
    fn an_exact_time_reads_on_the_local_clock_with_the_offset_of_its_own_season() {
        let chicago = jiff::tz::TimeZone::get("America/Chicago").unwrap();
        assert_eq!(
            ts("2026-01-15T12:00:00Z").exact_in(&chicago),
            "2026-01-15 06:00:00 CST",
            "a winter instant keeps standard time"
        );
        assert_eq!(
            ts("2026-07-15T12:00:00Z").exact_in(&chicago),
            "2026-07-15 07:00:00 CDT",
            "a summer instant keeps daylight time"
        );
        assert_eq!(
            ts("2026-07-15T03:00:00Z").exact_in(&chicago),
            "2026-07-14 22:00:00 CDT",
            "and the local day, not the UTC one"
        );
    }

    #[test]
    fn a_local_date_is_the_day_on_the_local_calendar() {
        let chicago = jiff::tz::TimeZone::get("America/Chicago").unwrap();
        let evening = ts("2026-09-06T01:00:00Z");
        assert_eq!(evening.date(), time::macros::date!(2026 - 09 - 06));
        assert_eq!(
            evening.date_in(&chicago),
            time::macros::date!(2026 - 09 - 05),
            "8 pm in Chicago is still the 5th"
        );
    }

    #[test]
    fn plus_seconds_saturates_at_the_calendars_end() {
        let now = Timestamp::now();
        assert!(now.plus_seconds(i64::MAX) > now);
        assert!(now.plus_seconds(i64::MIN) < now);
        assert_eq!(now.plus_seconds(0), now);
    }
}
