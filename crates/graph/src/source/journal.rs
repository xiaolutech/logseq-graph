//! Journal file names and the page titles Logseq shows for them.
//!
//! A journal is identified by a calendar day, not by its file name. `:journal/file-name-format`
//! says how that day is written into `journals/`, and `:journal/page-title-format` says how the day
//! is displayed. Both are format strings over a date, and both are separate from `:file/name-format`
//! in [`super::config`]: a regular page's name comes from its file name alone, a journal's name is a
//! date rendered by a pattern.
//!
//! The day itself is [`JournalDate`](crate::domain::JournalDate), a fact about the page and so a
//! `domain` value; what stays here is the pair of formats that render it, which depend on the
//! configuration and are therefore naming rules rather than page facts.
//!
//! This module is pure date and string arithmetic. It never stats a file; [`super::discovery`] hands
//! it a file stem. It also goes the other way — a day renders into a stem and a title — which is the
//! direction creating a journal needs, and the two directions are inverses of each other.
//!
//! # Supported patterns
//!
//! A pattern is a sequence of pattern letters and literals, in the Java `SimpleDateFormat` spelling
//! Logseq uses. Pattern letters are ASCII letters; every other character is a literal and is copied
//! through unchanged, which is what makes `yyyy_MM_dd` and `yyyy年M月d日` work.
//!
//! | Letters | Meaning | File name | Page title |
//! | --- | --- | --- | --- |
//! | `yyyy` | four-digit year | yes | yes |
//! | `yy` | two-digit year | no | yes |
//! | `MMMM`, `MMM` | month name, full and abbreviated | no | yes |
//! | `MM`, `M` | month number, padded and unpadded | yes | yes |
//! | `EEEE`, `EEE` | weekday name, full and abbreviated | no | yes |
//! | `dd`, `d` | day of month, padded and unpadded | yes | yes |
//! | `do` | day of month with its English ordinal suffix (`1st`, `2nd`, `3rd`, `29th`) | no | yes |
//!
//! Anything else is an error rather than an approximation, because a pattern this module renders
//! differently from Logseq would rename pages. Two-digit years are refused in a file name: `26` does
//! not name a century, and Logseq would resolve it against the current date. Weekday and month names
//! in a file name are refused too: they add nothing the date does not already carry, and parsing
//! them back would have to validate a redundant claim. ASCII letters that are not pattern letters
//! are refused as well, so a literal word needs a non-ASCII or non-letter spelling.

use std::fmt;

use crate::domain::JournalDate;

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Day of month with the English ordinal suffix, as `do` renders it.
fn ordinal(day: u32) -> String {
    let suffix = match day % 100 {
        11..=13 => "th",
        _ => match day % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        },
    };
    format!("{day}{suffix}")
}

/// One piece of a pattern: either text to copy through or a date field to render.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Literal(String),
    Field(Field),
}

/// A date field a pattern can render or read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    YearFour,
    YearTwo,
    MonthFull,
    MonthShort,
    MonthNumberPadded,
    MonthNumber,
    WeekdayFull,
    WeekdayShort,
    DayPadded,
    Day,
    DayOrdinal,
}

impl Field {
    /// How many digits this field reads, shortest first. Only date-number fields are readable.
    fn widths(self) -> &'static [usize] {
        match self {
            Self::YearFour => &[4],
            Self::MonthNumberPadded | Self::DayPadded => &[2],
            Self::MonthNumber | Self::Day => &[1, 2],
            _ => &[],
        }
    }

    /// Index of the value this field reads: year, month, day.
    fn slot(self) -> usize {
        match self {
            Self::YearFour | Self::YearTwo => 0,
            Self::MonthFull | Self::MonthShort | Self::MonthNumberPadded | Self::MonthNumber => 1,
            Self::WeekdayFull
            | Self::WeekdayShort
            | Self::DayPadded
            | Self::Day
            | Self::DayOrdinal => 2,
        }
    }

    /// Render this field of `date`.
    fn render(self, date: JournalDate) -> String {
        match self {
            Self::YearFour => format!("{:04}", date.year()),
            Self::YearTwo => format!("{:02}", date.year() % 100),
            Self::MonthFull => MONTHS[(date.month() - 1) as usize].to_owned(),
            Self::MonthShort => MONTHS[(date.month() - 1) as usize][..3].to_owned(),
            Self::MonthNumberPadded => format!("{:02}", date.month()),
            Self::MonthNumber => date.month().to_string(),
            Self::WeekdayFull => WEEKDAYS[date.weekday()].to_owned(),
            Self::WeekdayShort => WEEKDAYS[date.weekday()][..3].to_owned(),
            Self::DayPadded => format!("{:02}", date.day()),
            Self::Day => date.day().to_string(),
            Self::DayOrdinal => ordinal(date.day()),
        }
    }
}

/// Which pattern letters a format string may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Usage {
    /// `:journal/file-name-format`: date numbers and literals only.
    FileName,
    /// `:journal/page-title-format`: every supported letter.
    PageTitle,
}

/// A pattern this module does not implement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatternError {
    detail: String,
}

impl PatternError {
    fn new(detail: String) -> Self {
        Self { detail }
    }
}

impl fmt::Display for PatternError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

/// Parse a pattern, rejecting letters this build does not implement.
fn parse_pattern(pattern: &str, usage: Usage) -> Result<Vec<Piece>, PatternError> {
    let characters: Vec<char> = pattern.chars().collect();
    let mut pieces: Vec<Piece> = Vec::new();
    let mut literal = String::new();
    let mut index = 0;

    while index < characters.len() {
        let character = characters[index];
        if !character.is_ascii_alphabetic() {
            literal.push(character);
            index += 1;
            continue;
        }

        let run = characters[index..]
            .iter()
            .take_while(|current| **current == character)
            .count();
        // `do` is the ordinal day: the day number followed by its English suffix.
        let (field, consumed) = match (usage, character, run) {
            (Usage::PageTitle, 'd', 1) if characters.get(index + 1) == Some(&'o') => {
                (Field::DayOrdinal, 2)
            }
            _ => (
                field_for(usage, character, run).ok_or_else(|| {
                    PatternError::new(format!(
                        "`{}` is not a supported pattern letter here",
                        character.to_string().repeat(run)
                    ))
                })?,
                run,
            ),
        };

        if !literal.is_empty() {
            pieces.push(Piece::Literal(std::mem::take(&mut literal)));
        }
        pieces.push(Piece::Field(field));
        index += consumed;
    }
    if !literal.is_empty() {
        pieces.push(Piece::Literal(literal));
    }

    Ok(pieces)
}

/// The field one run of a pattern letter renders, or `None` if this build does not implement it.
fn field_for(usage: Usage, letter: char, run: usize) -> Option<Field> {
    let page_title = usage == Usage::PageTitle;
    match (letter, run) {
        ('y', 4) => Some(Field::YearFour),
        ('y', 2) if page_title => Some(Field::YearTwo),
        ('M', 4) if page_title => Some(Field::MonthFull),
        ('M', 3) if page_title => Some(Field::MonthShort),
        ('M', 2) => Some(Field::MonthNumberPadded),
        ('M', 1) => Some(Field::MonthNumber),
        ('E', 4) if page_title => Some(Field::WeekdayFull),
        ('E', 3) if page_title => Some(Field::WeekdayShort),
        ('d', 2) => Some(Field::DayPadded),
        ('d', 1) => Some(Field::Day),
        _ => None,
    }
}

/// How a journal file name is written under `journals/`, from `:journal/file-name-format`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JournalFileNameFormat {
    pattern: String,
    pieces: Vec<Piece>,
}

impl JournalFileNameFormat {
    pub(crate) fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Parse `:journal/file-name-format`.
    pub(crate) fn parse(pattern: &str) -> Result<Self, PatternError> {
        let pieces = parse_pattern(pattern, Usage::FileName)?;

        let mut years = 0;
        let mut months = 0;
        let mut days = 0;
        for piece in &pieces {
            match piece {
                Piece::Field(Field::YearFour) => years += 1,
                Piece::Field(Field::MonthNumber | Field::MonthNumberPadded) => months += 1,
                Piece::Field(Field::Day | Field::DayPadded) => days += 1,
                _ => {}
            }
        }
        if (years, months, days) != (1, 1, 1) {
            return Err(PatternError::new(format!(
                "`{pattern}` must name exactly one year, one month, and one day"
            )));
        }

        // Two neighbouring number fields whose boundary is not fixed by both widths cannot be read
        // back unambiguously: `yyyyMd` could be January 11th or November 1st.
        for pair in pieces.windows(2) {
            if let [Piece::Field(left), Piece::Field(right)] = pair {
                if left.widths().len() > 1 || right.widths().len() > 1 {
                    return Err(PatternError::new(format!(
                        "`{pattern}` puts two date fields side by side with a variable width"
                    )));
                }
            }
        }

        Ok(Self {
            pattern: pattern.to_owned(),
            pieces,
        })
    }

    /// Render `date` as the file stem under `journals/` that names it.
    ///
    /// This is [`Self::parse_stem`]'s inverse. The pattern was accepted only when it names exactly
    /// one year, one month, and one day with no two variable-width fields adjacent, so a rendered
    /// stem is unambiguous and reads back as the same day — that round trip is what makes it
    /// correct to write a journal from a date instead of from a title.
    pub(crate) fn format(&self, date: JournalDate) -> String {
        render(&self.pieces, date)
    }

    /// Read the calendar day a journal file stem names.
    pub(crate) fn parse_stem(&self, stem: &str) -> Result<JournalDate, JournalNameError> {
        let mut values: [Option<u32>; 3] = [None; 3];
        let matched = self.match_at(0, stem.as_bytes(), 0, &mut values);
        if matched != Some(stem.len()) {
            return Err(JournalNameError::NoMatch {
                pattern: self.pattern.clone(),
            });
        }
        let (Some(year), Some(month), Some(day)) = (values[0], values[1], values[2]) else {
            // `parse` accepts a pattern only when it names all three parts.
            return Err(JournalNameError::NoMatch {
                pattern: self.pattern.clone(),
            });
        };
        let year = i64::from(year);
        JournalDate::new(year, month, day).ok_or(JournalNameError::NotADate { year, month, day })
    }

    /// Match the pattern from `piece` onward at `position`, backtracking over field widths.
    ///
    /// A match succeeds only when every piece is consumed and the whole stem is used, so a short
    /// field width cannot accept a prefix and leave the rest unread.
    fn match_at(
        &self,
        piece: usize,
        text: &[u8],
        position: usize,
        values: &mut [Option<u32>; 3],
    ) -> Option<usize> {
        let Some(current) = self.pieces.get(piece) else {
            return (position == text.len()).then_some(position);
        };
        match current {
            Piece::Literal(literal) => {
                let literal = literal.as_bytes();
                let end = position + literal.len();
                if text.get(position..end) != Some(literal) {
                    return None;
                }
                self.match_at(piece + 1, text, end, values)
            }
            Piece::Field(field) => {
                for width in field.widths() {
                    let end = position + width;
                    let Some(digits) = text.get(position..end) else {
                        continue;
                    };
                    if !digits.iter().all(u8::is_ascii_digit) {
                        continue;
                    }
                    let value = digits
                        .iter()
                        .fold(0_u32, |value, digit| value * 10 + u32::from(digit - b'0'));
                    let slot = field.slot();
                    let previous = values[slot].replace(value);
                    if let Some(matched) = self.match_at(piece + 1, text, end, values) {
                        return Some(matched);
                    }
                    values[slot] = previous;
                }
                None
            }
        }
    }
}

/// Failure of reading a journal file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JournalNameError {
    /// The file stem does not match the configured file name format.
    NoMatch {
        /// The format the stem was matched against.
        pattern: String,
    },
    /// The stem matched the format but does not name a day of the calendar.
    NotADate {
        /// Year the stem named.
        year: i64,
        /// Month the stem named.
        month: u32,
        /// Day the stem named.
        day: u32,
    },
}

impl fmt::Display for JournalNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMatch { pattern } => write!(
                formatter,
                "journal file name does not match the file name format `{pattern}`"
            ),
            Self::NotADate { year, month, day } => {
                write!(
                    formatter,
                    "{year:04}-{month:02}-{day:02} is not a calendar day"
                )
            }
        }
    }
}

/// How a journal page title is written, from `:journal/page-title-format`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JournalPageTitleFormat {
    pattern: String,
    pieces: Vec<Piece>,
}

impl JournalPageTitleFormat {
    pub(crate) fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Parse `:journal/page-title-format`.
    pub(crate) fn parse(pattern: &str) -> Result<Self, PatternError> {
        let pieces = parse_pattern(pattern, Usage::PageTitle)?;
        if !pieces.iter().any(|piece| matches!(piece, Piece::Field(_))) {
            return Err(PatternError::new(format!(
                "`{pattern}` names no date, so every journal would get the same title"
            )));
        }
        Ok(Self {
            pattern: pattern.to_owned(),
            pieces,
        })
    }

    /// Render the page title Logseq shows for `date`.
    pub(crate) fn format(&self, date: JournalDate) -> String {
        render(&self.pieces, date)
    }
}

/// Render one compiled pattern for `date`, literals copied through and fields expanded.
///
/// Both formats are the same walk over the same pieces; only the usage that compiled them differs.
fn render(pieces: &[Piece], date: JournalDate) -> String {
    let mut rendered = String::new();
    for piece in pieces {
        match piece {
            Piece::Literal(literal) => rendered.push_str(literal),
            Piece::Field(field) => rendered.push_str(&field.render(date)),
        }
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i64, month: u32, day: u32) -> JournalDate {
        JournalDate::new(year, month, day).expect("valid date")
    }

    fn title(pattern: &str) -> JournalPageTitleFormat {
        JournalPageTitleFormat::parse(pattern).expect("supported title pattern")
    }

    fn file_name(pattern: &str) -> JournalFileNameFormat {
        JournalFileNameFormat::parse(pattern).expect("supported file name pattern")
    }

    #[test]
    fn default_patterns_round_trip() {
        let format = file_name("yyyy_MM_dd");
        let day = format.parse_stem("2026_08_13").expect("read stem");
        assert_eq!(day, date(2026, 8, 13));
        assert_eq!(title("MMM do, yyyy").format(day), "Aug 13th, 2026");
    }

    #[test]
    fn file_name_format_is_the_inverse_of_parse_stem() {
        // Every pattern `parse` accepts names one year, one month, and one day with fixed
        // boundaries, so rendering and reading have to be each other's inverse. The days cover a
        // leap day, a last day of the year, and single-digit month and day values.
        let days = [
            date(2024, 2, 29),
            date(2026, 12, 31),
            date(2026, 1, 1),
            date(2026, 8, 3),
            date(1900, 2, 28),
        ];
        for pattern in [
            "yyyy_MM_dd",
            "yyyy-MM-dd",
            "yyyy年M月d日",
            "yyyyMMdd",
            "yyyy_M_d",
        ] {
            let format = file_name(pattern);
            for day in days {
                let stem = format.format(day);
                assert_eq!(
                    format
                        .parse_stem(&stem)
                        .expect("read back the rendered stem"),
                    day,
                    "`{pattern}` rendered {day} as {stem:?}"
                );
            }
        }
    }

    #[test]
    fn a_year_outside_four_digits_has_no_date() {
        // `yyyy` is a fixed four-digit field, so a year outside `0..=9999` renders a stem that
        // `parse_stem` cannot read back. The constructor is where that has to stop.
        assert!(JournalDate::new(10_000, 1, 1).is_none());
        assert!(JournalDate::new(-1, 1, 1).is_none());
        assert!(JournalDate::new(i64::MIN, 1, 1).is_none());
        let last = date(9999, 12, 31);
        let format = file_name("yyyy_MM_dd");
        let stem = format.format(last);
        assert_eq!(stem, "9999_12_31");
        assert_eq!(format.parse_stem(&stem).expect("read back"), last);
        assert_eq!(date(0, 1, 1).to_string(), "0000-01-01");
    }

    #[test]
    fn file_name_reads_unpadded_fields() {
        let format = file_name("yyyy_M_d");
        assert_eq!(
            format.parse_stem("2026_8_13").expect("read"),
            date(2026, 8, 13)
        );
        assert_eq!(
            format.parse_stem("2026_08_13").expect("read"),
            date(2026, 8, 13)
        );
    }

    #[test]
    fn file_name_reads_adjacent_fixed_width_fields() {
        let format = file_name("yyyyMMdd");
        assert_eq!(
            format.parse_stem("20260813").expect("read"),
            date(2026, 8, 13)
        );
    }

    #[test]
    fn file_name_rejects_a_stem_of_another_shape() {
        let format = file_name("yyyy_MM_dd");
        for stem in [
            "13_08_2026",
            "2026-08-13",
            "2026_08_13_journal",
            "2026_08",
            "",
        ] {
            let error = format.parse_stem(stem).expect_err("shape mismatch");
            assert!(
                matches!(error, JournalNameError::NoMatch { .. }),
                "unexpected error for {stem}: {error}"
            );
        }
    }

    #[test]
    fn file_name_rejects_a_day_the_month_does_not_have() {
        let format = file_name("yyyy_MM_dd");
        for stem in ["2026_13_01", "2026_00_10", "2026_02_30", "2026_04_31"] {
            let error = format.parse_stem(stem).expect_err("impossible day");
            assert!(
                matches!(error, JournalNameError::NotADate { .. }),
                "unexpected error for {stem}: {error}"
            );
        }
        assert!(format.parse_stem("2026_02_28").is_ok());
    }

    #[test]
    fn leap_day_is_accepted_only_in_a_leap_year() {
        let format = file_name("yyyy_MM_dd");
        assert_eq!(
            format.parse_stem("2024_02_29").expect("leap"),
            date(2024, 2, 29)
        );
        assert!(format.parse_stem("2026_02_29").is_err());
        assert!(format.parse_stem("2000_02_29").is_ok());
        assert!(format.parse_stem("1900_02_29").is_err());
    }

    #[test]
    fn unsupported_file_name_letters_are_rejected() {
        for pattern in [
            "yy_MM_dd",
            "yyyy_MM_dd_EEEE",
            "yyyy_MM_dd_HH",
            "yyyy_MMM_dd",
            "yyyy_MM_do",
            "yyyy_MM",
            "yyyy_MM_dd_MM",
            "yyyyMd",
        ] {
            assert!(
                JournalFileNameFormat::parse(pattern).is_err(),
                "{pattern} should be rejected"
            );
        }
    }

    #[test]
    fn title_renders_like_logseq() {
        let day = date(2026, 8, 13);
        assert_eq!(title("MMM do, yyyy").format(day), "Aug 13th, 2026");
        assert_eq!(title("yyyy-MM-dd EEEE").format(day), "2026-08-13 Thursday");
        assert_eq!(title("EEE MMM dd").format(day), "Thu Aug 13");
        assert_eq!(title("MMMM d, yy").format(day), "August 13, 26");
        assert_eq!(title("yyyy年M月d日").format(day), "2026年8月13日");
    }

    #[test]
    fn title_capitalizes_month_and_weekday_names() {
        let day = date(2026, 1, 4);
        assert_eq!(title("EEEE MMMM").format(day), "Sunday January");
        assert_eq!(title("EEE MMM").format(day), "Sun Jan");
    }

    #[test]
    fn ordinal_suffixes_follow_english_rules() {
        let suffixes: Vec<String> = (1..=31)
            .map(|day| title("do").format(date(2026, 1, day)))
            .collect();
        assert_eq!(suffixes[0], "1st");
        assert_eq!(suffixes[1], "2nd");
        assert_eq!(suffixes[2], "3rd");
        assert_eq!(suffixes[3], "4th");
        assert_eq!(suffixes[10], "11th");
        assert_eq!(suffixes[11], "12th");
        assert_eq!(suffixes[12], "13th");
        assert_eq!(suffixes[20], "21st");
        assert_eq!(suffixes[21], "22nd");
        assert_eq!(suffixes[22], "23rd");
        assert_eq!(suffixes[30], "31st");
    }

    #[test]
    fn weekday_matches_known_dates() {
        // 1970-01-01 was a Thursday, 2025-12-07 a Sunday.
        assert_eq!(date(1970, 1, 1).weekday(), 4);
        assert_eq!(date(2025, 12, 7).weekday(), 0);
        assert_eq!(date(2026, 8, 13).weekday(), 4);
    }

    #[test]
    fn unsupported_title_letters_are_rejected() {
        for pattern in [
            "yyyy-MM-dd HH:mm",
            "yyy-MM-dd",
            "EE MMM dd",
            "dddd",
            "yyyy-MM-dd 'at' HH",
            "",
            "-",
        ] {
            assert!(
                JournalPageTitleFormat::parse(pattern).is_err(),
                "{pattern} should be rejected"
            );
        }
    }
}
