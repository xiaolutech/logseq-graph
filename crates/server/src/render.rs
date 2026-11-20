//! Text rendering: the shapes every printed answer needs.
//!
//! The CLI prints plain text, one record per line, so this module holds the conversions that keep a
//! record on one line: a time becomes UTC RFC3339, block text becomes a single line, and a block
//! becomes the four-line record that `block` and `update` both print. They are pure functions, so
//! they are testable without a graph.

use std::time::{SystemTime, UNIX_EPOCH};

use logseq_graph::domain::{BlockSelector, BlockView, JournalDate};

/// Seconds in one day, the unit the civil-date conversion works in.
const SECONDS_PER_DAY: i64 = 86_400;

/// `time` as a UTC RFC3339 timestamp with second precision, e.g. `2026-08-13T07:05:09Z`.
///
/// No time zone conversion happens, because the value is an instant, not a wall clock: the engine
/// reports when a scan observed the graph, and printing it in the local zone would make two
/// machines disagree about the same instant. Fractional seconds are dropped: a scan's start time is
/// reported to the second, which is the precision the field is for.
pub(crate) fn rfc3339(time: SystemTime) -> String {
    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_secs() as i64,
        // Before the epoch the duration runs the other way; the conversion below handles a negative
        // second count with `div_euclid`, so a clock set back still prints a real date.
        Err(before) => -(before.duration().as_secs() as i64),
    };
    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let of_day = seconds.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (of_day / 3600, (of_day % 3600) / 60, of_day % 60);
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z",
        year = year,
        month = month,
        day = day,
        hour = hour,
        minute = minute,
        second = second
    )
}

/// The calendar day `time` falls on, in UTC.
///
/// UTC rather than the local zone, for the reason [`rfc3339`] prints an instant: a query's `today`
/// decides which `(between -7d today)` window is answered, and a machine whose zone differs from the
/// one that wrote the graph would answer a different window for the same query. A caller that means
/// a different day says `--today`.
pub(crate) fn day(time: SystemTime) -> (i64, u32, u32) {
    let days = match time.duration_since(UNIX_EPOCH) {
        Ok(after) => (after.as_secs() / SECONDS_PER_DAY as u64) as i64,
        Err(before) => -(before.duration().as_secs() as i64).div_euclid(SECONDS_PER_DAY),
    };
    let (year, month, day) = civil_from_days(days);
    (year, month as u32, day as u32)
}

/// The day `time` falls on, as a [`JournalDate`].
///
/// `None` only when the clock states a day the proleptic Gregorian calendar does not have, which a
/// working clock cannot: the caller reports that rather than inventing a day, because every relative
/// operand of a query would then be measured from a date the caller never named.
pub(crate) fn today(time: SystemTime) -> Option<JournalDate> {
    let (year, month, day) = day(time);
    JournalDate::new(year, month, day)
}

/// The day `word` states as `YYYY-MM-DD`, or `None` when it states no day the calendar has.
///
/// The one place this shape is read, so every caller agrees on what a day is: `--today` on the
/// command line and `?today=` on the wire differ in whose mistake a bad value is, not in the rule.
pub(crate) fn parse_day(word: &str) -> Option<JournalDate> {
    let bytes = word.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        let slice = word.get(range)?;
        if !slice.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        slice.parse().ok()
    };
    let year = number(0..4)?;
    let month = number(5..7)?;
    let day = number(8..10)?;
    JournalDate::new(i64::from(year), month, day)
}

/// The one spelling of a block selector the command line prints and reads.
///
/// Printed, because a query answer must hand back the identity its caller needs to name the block
/// again, and the kind has to be stated: the two ids share a shape and never convert into one
/// another (ADR-0012), so a bare UUID would leave the caller guessing which namespace it is in.
pub(crate) fn selector(selector: &BlockSelector) -> String {
    match selector {
        BlockSelector::Durable(id) => format!("{}{id}", crate::args::DURABLE_PREFIX),
        BlockSelector::Ephemeral(id) => format!("{}{id}", crate::args::EPHEMERAL_PREFIX),
    }
}

/// The civil date `days` after 1970-01-01, by Howard Hinnant's `civil_from_days`.
///
/// This is the whole reason no date dependency is needed: the algorithm is short, exact for every
/// day the proleptic Gregorian calendar defines, and has no table to get wrong.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_shifted + 2) / 5 + 1;
    let month = if month_shifted < 10 {
        month_shifted + 3
    } else {
        month_shifted - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// `text` with every line break turned into a space.
///
/// Block text is one line by construction, except that a block's property lines are stored on
/// their own lines. Printing them raw would break the one-record-per-line shape the CLI promises, so
/// a block's text is folded onto one line here; nothing else about the text changes.
pub(crate) fn one_line(text: &str) -> String {
    text.lines().collect::<Vec<_>>().join(" ")
}

/// The record one block prints as: page, position, depth, its text on one line, and the selector
/// the block answers to.
///
/// `block` and `update` share it so a caller can compare what it read with what it wrote: the write
/// answers the block it replaced in the same shape the read answers it. The selector is the last
/// line so the four fields above it stay byte for byte what they printed before selectors were
/// added, and a caller piping this into `update` names the block it just read.
pub(crate) fn block(view: &BlockView) -> String {
    format!(
        "page: {}\npreorder: {}\ndepth: {}\ntext: {}\nselector: {}\n",
        view.page(),
        view.preorder(),
        view.depth(),
        one_line(view.text()),
        selector(view.selector())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn a_known_instant_prints_as_rfc3339() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(at(1_700_000_000)), "2023-11-14T22:13:20Z");
        assert_eq!(rfc3339(at(951_782_400)), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn an_instant_before_the_epoch_prints_a_real_date() {
        let before = UNIX_EPOCH - Duration::from_secs(86_400);

        assert_eq!(rfc3339(before), "1969-12-31T00:00:00Z");
    }

    #[test]
    fn a_day_reads_back_as_the_date_it_states() {
        let (year, month, day) = super::day(at(1_700_000_000));
        assert_eq!((year, month, day), (2023, 11, 14));
        assert_eq!(
            super::today(at(1_700_000_000)),
            Some(JournalDate::new(2023, 11, 14).expect("a real day"))
        );
    }

    #[test]
    fn a_selector_prints_the_prefix_that_says_which_kind_it_is() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        assert_eq!(
            super::selector(&BlockSelector::Durable(
                logseq_graph::domain::BlockId::parse(id).expect("valid id")
            )),
            format!("durable:{id}")
        );
        assert_eq!(
            super::selector(&BlockSelector::Ephemeral(
                logseq_graph::domain::EphemeralId::parse(id).expect("valid id")
            )),
            format!("ephemeral:{id}")
        );
    }

    #[test]
    fn block_text_is_folded_onto_one_line() {
        assert_eq!(one_line("plain text"), "plain text");
        assert_eq!(
            one_line("child two\nid:: 01234567-89ab-cdef-0123-456789abcdef"),
            "child two id:: 01234567-89ab-cdef-0123-456789abcdef"
        );
        assert_eq!(one_line("crlf\r\nline"), "crlf line");
        assert_eq!(one_line(""), "");
    }
}
