//! The calendar day a journal is.
//!
//! A journal is identified by a day, not by its file name and not by the title Logseq renders for
//! it: `:journal/file-name-format` writes the day into `journals/`, and `:journal/page-title-format`
//! displays it, so the same day can be spelled two arbitrary ways and neither spelling is the
//! identity. The day itself is a fact about the page, which is why it lives here beside
//! [`BlockMarker`](super::BlockMarker) and [`PageKind`](super::PageKind) rather than in `source`:
//! `source` owns the naming rules that turn a day into a file name and back (ADR-0003), and this
//! type is what those rules render and what a read hands back.
//!
//! # Spelling
//!
//! [`Display`](fmt::Display) is `YYYY-MM-DD`, which is also the spelling the index stores. That
//! spelling sorts lexicographically exactly as it sorts in time, so `BETWEEN` and `ORDER BY` over
//! the stored text answer date-range and chronological questions without a calendar function in
//! SQL, and a caller that wants a different presentation formats the three numbers itself.

use std::fmt;

/// One calendar day: what identifies a journal, in both directions.
///
/// A journal is named by a day, not by its file name. Reading a journal is
/// `JournalFileNameFormat::parse_stem`, which answers this type; writing one is
/// `JournalFileNameFormat::format`, which takes it. A day that is not on the calendar has no
/// instance, so a caller cannot ask for the journal of February 30th; neither has a year outside the
/// four digits a journal file name holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct JournalDate {
    year: i64,
    month: u32,
    day: u32,
}

impl JournalDate {
    /// Build a date, rejecting a day that the month does not have.
    ///
    /// `None` means the three numbers do not name a calendar day — a month outside `1..=12`, a day
    /// outside the month it belongs to, or a leap day in a year that has none — which is exactly
    /// the check reading a journal file name applies.
    ///
    /// `None` also means a year this crate cannot write as a journal file name. `yyyy` is a
    /// four-digit field in both directions, so only `0..=9999` renders to a stem
    /// `:journal/file-name-format` can read back: a negative year renders a sign, a five-digit year
    /// renders five digits, and either one makes a file discovery reports as an invalid page name.
    /// Refusing the day here keeps that failure from being reported by the layer that merely
    /// discovered the file. It is also what makes the stored `YYYY-MM-DD` spelling a fixed ten
    /// characters.
    pub fn new(year: i64, month: u32, day: u32) -> Option<Self> {
        if !(0..=9999).contains(&year)
            || !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
        {
            return None;
        }
        Some(Self { year, month, day })
    }

    /// Year, as written: `2026` is the year 2026.
    pub fn year(self) -> i64 {
        self.year
    }

    /// Month, `1` for January through `12` for December.
    pub fn month(self) -> u32 {
        self.month
    }

    /// Day of the month, `1` through the last day the month has.
    pub fn day(self) -> u32 {
        self.day
    }

    /// Read the fixed `YYYY-MM-DD` spelling [`Display`](fmt::Display) writes.
    ///
    /// This is the inverse of `Display` for the one spelling the index stores, and nothing else:
    /// a shorter or longer string, another separator, or a value that no longer names a calendar
    /// day answers `None`. A stored day that does not read back is corrupt state rather than
    /// another spelling, so the caller reports it instead of normalizing it.
    pub(crate) fn parse_stored(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let digits = |range: std::ops::Range<usize>| -> Option<u32> {
            let slice = bytes.get(range)?;
            if !slice.iter().all(u8::is_ascii_digit) {
                return None;
            }
            slice.iter().try_fold(0_u32, |value, digit| {
                value.checked_mul(10)?.checked_add(u32::from(digit - b'0'))
            })
        };
        Self::new(i64::from(digits(0..4)?), digits(5..7)?, digits(8..10)?)
    }

    /// This day shifted by `days`, which may be negative, or `None` when the result is not a day
    /// this type can name.
    ///
    /// The arithmetic is proleptic Gregorian, the same calendar [`new`](Self::new) validates
    /// against, so a shift over a leap day lands on the 29th and a shift over a year boundary
    /// moves the year. A week is seven days: callers that think in weeks multiply by seven rather
    /// than reach for a separate method.
    ///
    /// `None` means the calendar has no day at that place, which happens only where the four-digit
    /// year runs out: a result before `0000-01-01` or after `9999-12-31` is not a journal, since
    /// [`new`](Self::new) would refuse it. The alternative — clamping into range — would answer a
    /// question nobody asked with a day that is silently wrong. An offset large enough to overflow
    /// `i64` is out of range for the same reason and answers `None` too.
    pub fn checked_add_days(self, days: i64) -> Option<Self> {
        let target = days_from_civil(self.year, self.month, self.day).checked_add(days)?;
        Self::from_days(target)
    }

    /// This day shifted by `months`, which may be negative, or `None` when the result is not a day
    /// this type can name.
    ///
    /// The result is the same day of the month that many months away; where the target month is too
    /// short to hold that day, it is clamped to the last day the month has, so `2026-03-31` less one
    /// month is `2026-02-28` and, across the leap day, `2024-03-31` less one month is `2024-02-29`.
    /// A year is twelve: callers that think in years multiply by twelve.
    ///
    /// `None` covers the same two cases as [`checked_add_days`](Self::checked_add_days): an offset
    /// large enough to overflow `i64`, and a target month in a year that is not four digits.
    pub fn checked_add_months(self, months: i64) -> Option<Self> {
        let index = (self.year * 12 + i64::from(self.month) - 1).checked_add(months)?;
        let year = index.div_euclid(12);
        let month = (index.rem_euclid(12) + 1) as u32;
        let day = self.day.min(days_in_month(year, month));
        Self::new(year, month, day)
    }

    /// The day `days` days after `1970-01-01`, when that day is one this type can name.
    ///
    /// The range check happens here rather than after the conversion so that reading a day back
    /// from its offset never sees a value that can overflow the inverse algorithm.
    ///
    /// It is also what keeps that algorithm's truncating divisions from rounding the wrong way:
    /// `days` inside this range puts `days + 719_468` in `-60..=3_652_364`, the window the era
    /// fixup was written for, and the fixup then leaves `day_of_era` in `0..=146_096`. Every
    /// division after it — the year of the era, the day of the year, the month shift, the day —
    /// therefore truncates a value that is already non-negative and so floors.
    fn from_days(days: i64) -> Option<Self> {
        if !(DAYS_IN_FIRST_YEAR..=DAYS_IN_LAST_YEAR).contains(&days) {
            return None;
        }
        let (year, month, day) = civil_from_days(days);
        Self::new(year, month, day)
    }

    /// Day of the week, `0` for Sunday through `6` for Saturday.
    ///
    /// Visible to the crate because a journal's title format can render a weekday name (`EEEE`);
    /// it is not part of the day's identity and no caller outside this crate needs it.
    pub(crate) fn weekday(self) -> usize {
        // 1970-01-01 was a Thursday, which is index 4.
        ((days_from_civil(self.year, self.month, self.day) + 4).rem_euclid(7)) as usize
    }
}

impl fmt::Display for JournalDate {
    /// `YYYY-MM-DD`, the spelling an error message and a caller both read.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02}",
            self.year, self.month, self.day
        )
    }
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = i64::from((month + 9) % 12);
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Days since `1970-01-01` of the first day this type can name, `0000-01-01`.
const DAYS_IN_FIRST_YEAR: i64 = -719_528;

/// Days since `1970-01-01` of the last day this type can name, `9999-12-31`.
const DAYS_IN_LAST_YEAR: i64 = 2_932_896;

/// The civil date `days` days after `1970-01-01` in the proleptic Gregorian calendar (Howard
/// Hinnant's algorithm, the inverse of [`days_from_civil`]).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(year: i64, month: u32, day: u32) -> JournalDate {
        JournalDate::new(year, month, day).expect("a day of the calendar")
    }

    /// A shift of `offset` days applied to `from`, and the day it must answer.
    fn check_days(cases: &[(JournalDate, i64, Option<JournalDate>)]) {
        for &(from, offset, want) in cases {
            assert_eq!(from.checked_add_days(offset), want, "{from} {offset:+}d");
        }
    }

    /// A shift of `offset` months applied to `from`, and the day it must answer.
    fn check_months(cases: &[(JournalDate, i64, Option<JournalDate>)]) {
        for &(from, offset, want) in cases {
            assert_eq!(from.checked_add_months(offset), want, "{from} {offset:+}mo");
        }
    }

    #[test]
    fn the_named_range_matches_the_years_the_type_accepts() {
        // The constants guard the inverse algorithm; they must agree with the forward one, or a day
        // at the edge would be refused for no reason (or read back as the wrong date).
        assert_eq!(days_from_civil(0, 1, 1), DAYS_IN_FIRST_YEAR);
        assert_eq!(days_from_civil(9999, 12, 31), DAYS_IN_LAST_YEAR);
        assert_eq!(
            JournalDate::from_days(DAYS_IN_FIRST_YEAR),
            Some(day(0, 1, 1))
        );
        assert_eq!(
            JournalDate::from_days(DAYS_IN_LAST_YEAR),
            Some(day(9999, 12, 31))
        );
        for (year, month, day_of_month) in [
            (0, 1, 1),
            (0, 3, 1),
            (1970, 1, 1),
            (2024, 2, 29),
            (2026, 8, 13),
            (9999, 12, 31),
        ] {
            let offset = days_from_civil(year, month, day_of_month);
            assert_eq!(civil_from_days(offset), (year, month, day_of_month));
        }
    }

    #[test]
    fn days_shift_forwards_and_backwards_across_months_and_years() {
        check_days(&[
            (day(2026, 8, 13), 0, Some(day(2026, 8, 13))),
            (day(2026, 8, 13), 1, Some(day(2026, 8, 14))),
            (day(2026, 8, 13), -1, Some(day(2026, 8, 12))),
            // Off the end of a month and of a year, in both directions.
            (day(2026, 8, 31), 1, Some(day(2026, 9, 1))),
            (day(2026, 9, 1), -1, Some(day(2026, 8, 31))),
            (day(2026, 12, 31), 1, Some(day(2027, 1, 1))),
            (day(2026, 1, 1), -1, Some(day(2025, 12, 31))),
            // A week is seven days; a year is whatever the calendar says it is.
            (day(2026, 8, 13), 7, Some(day(2026, 8, 20))),
            (day(2026, 8, 13), -7, Some(day(2026, 8, 6))),
            (day(2026, 8, 13), 7 * 52, Some(day(2027, 8, 12))),
            (day(2026, 1, 1), 365, Some(day(2027, 1, 1))),
            (day(2024, 1, 1), 366, Some(day(2025, 1, 1))),
            // 2023 has no leap day, so a year back from 2024-01-01 is 365 days.
            (day(2024, 1, 1), -365, Some(day(2023, 1, 1))),
        ]);
    }

    #[test]
    fn days_shift_lands_on_the_leap_day() {
        check_days(&[
            (day(2024, 2, 28), 1, Some(day(2024, 2, 29))),
            (day(2024, 2, 29), 1, Some(day(2024, 3, 1))),
            (day(2023, 2, 28), 1, Some(day(2023, 3, 1))),
            (day(2024, 2, 29), -365, Some(day(2023, 3, 1))),
            // A century that is not a leap year, then one that is.
            (day(1900, 2, 28), 1, Some(day(1900, 3, 1))),
            (day(2000, 2, 28), 1, Some(day(2000, 2, 29))),
        ]);
    }

    #[test]
    fn days_shift_stops_at_the_named_years_instead_of_clamping() {
        let first = day(0, 1, 1);
        let last = day(9999, 12, 31);
        check_days(&[
            (first, 0, Some(first)),
            (first, 1, Some(day(0, 1, 2))),
            (first, -1, None),
            (last, 0, Some(last)),
            (last, -1, Some(day(9999, 12, 30))),
            (last, 1, None),
            // Far past the edge answers `None` rather than a year that is not four digits.
            (last, 365 * 10_000, None),
            (first, -365 * 10_000, None),
        ]);
    }

    #[test]
    fn an_extreme_day_offset_answers_none_without_panicking() {
        let date = day(2026, 8, 13);
        check_days(&[
            (date, i64::MAX, None),
            (date, i64::MAX - 1, None),
            (date, i64::MIN, None),
            (date, i64::MIN + 1, None),
            // The boundaries of the arithmetic, not just beyond it.
            (day(0, 1, 1), i64::MIN, None),
            (day(9999, 12, 31), i64::MAX, None),
        ]);
    }

    #[test]
    fn months_shift_forwards_and_backwards() {
        check_months(&[
            (day(2026, 8, 13), 0, Some(day(2026, 8, 13))),
            (day(2026, 8, 13), 1, Some(day(2026, 9, 13))),
            (day(2026, 8, 13), -1, Some(day(2026, 7, 13))),
            (day(2026, 8, 13), 5, Some(day(2027, 1, 13))),
            (day(2026, 8, 13), -8, Some(day(2025, 12, 13))),
            // A year is twelve months.
            (day(2026, 8, 13), 12, Some(day(2027, 8, 13))),
            (day(2026, 8, 13), -12, Some(day(2025, 8, 13))),
            (day(2026, 8, 13), 120, Some(day(2036, 8, 13))),
        ]);
    }

    #[test]
    fn a_short_target_month_clamps_to_its_last_day() {
        check_months(&[
            (day(2026, 3, 31), -1, Some(day(2026, 2, 28))),
            (day(2024, 3, 31), -1, Some(day(2024, 2, 29))),
            (day(2026, 1, 31), 1, Some(day(2026, 2, 28))),
            (day(2026, 5, 31), 1, Some(day(2026, 6, 30))),
            (day(2026, 8, 31), 3, Some(day(2026, 11, 30))),
            (day(2024, 2, 29), 12, Some(day(2025, 2, 28))),
            // Clamping answers the target month, not a loss carried forward: a shift back to a long
            // month keeps the day the caller held.
            (day(2026, 3, 31), -2, Some(day(2026, 1, 31))),
        ]);
    }

    #[test]
    fn months_shift_stops_at_the_named_years_instead_of_clamping() {
        let first = day(0, 1, 1);
        let last = day(9999, 12, 31);
        check_months(&[
            (first, 0, Some(first)),
            (first, 11, Some(day(0, 12, 1))),
            (first, -1, None),
            (last, 0, Some(last)),
            (last, -11, Some(day(9999, 1, 31))),
            (last, 1, None),
            (last, 12 * 10_000, None),
            (first, -12 * 10_000, None),
        ]);
    }

    #[test]
    fn an_extreme_month_offset_answers_none_without_panicking() {
        let date = day(2026, 8, 13);
        check_months(&[
            (date, i64::MAX, None),
            (date, i64::MAX - 1, None),
            (date, i64::MIN, None),
            (date, i64::MIN + 1, None),
            (day(0, 1, 1), i64::MIN, None),
            (day(9999, 12, 31), i64::MAX, None),
        ]);
    }

    #[test]
    fn the_inverse_agrees_with_the_forward_over_every_day_of_some_years() {
        // Leap rules are where a calendar conversion breaks, so the sample is the years they single
        // out: the year zero, a leap year, a century that is not one, and a century that is.
        for year in [0, 1, 4, 100, 400, 1900, 2000, 2023, 2024, 9999] {
            for month in 1..=12 {
                for day_of_month in 1..=days_in_month(year, month) {
                    let offset = days_from_civil(year, month, day_of_month);
                    assert_eq!(civil_from_days(offset), (year, month, day_of_month));
                }
            }
        }
    }

    #[test]
    fn a_day_and_its_inverse_return_the_same_day() {
        let mut date = day(2026, 8, 13);
        for _ in 0..400 {
            let next = date.checked_add_days(1).expect("a day within range");
            assert_eq!(next.checked_add_days(-1), Some(date), "back from {next}");
            date = next;
        }
    }

    #[test]
    fn the_stored_spelling_is_the_displayed_one_and_reads_back() {
        for date in [
            day(2026, 8, 13),
            day(2024, 2, 29),
            day(0, 1, 1),
            day(9999, 12, 31),
        ] {
            let stored = date.to_string();
            assert_eq!(stored.len(), 10, "the stored day is a fixed ten characters");
            assert_eq!(JournalDate::parse_stored(&stored), Some(date));
        }
    }

    #[test]
    fn only_the_fixed_spelling_reads_back() {
        // The column holds one spelling, so every other shape is a value this build cannot answer
        // for rather than another way of writing the same day.
        for text in [
            "2026-8-13",
            "2026-08-13T00:00:00",
            "2026/08/13",
            "20260813",
            " 2026-08-13",
            "2026-02-30",
            "2026-13-01",
            "abcd-ef-gh",
            "",
        ] {
            assert_eq!(JournalDate::parse_stored(text), None, "{text:?}");
        }
    }
}
