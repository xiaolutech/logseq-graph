//! The task marker and priority a block's text can lead with.
//!
//! These two values are what makes a block a task: Logseq writes `TODO [#A] buy milk` as the text of
//! one block, and both the marker and the priority are facts read out of that text. They are not
//! [`BlockMarker`](super::BlockMarker), which is Markdown *structure* — a bullet or a heading, and
//! part of the line grammar that never reaches a block's text. A task marker is content: it is the
//! first word of the text, and the text after it is the description.
//!
//! # Spelling has two sides
//!
//! The file side is exact where Logseq is exact. Logseq writes its task words in uppercase, so
//! [`TaskMarker::leading`] accepts only that spelling: a block whose text starts with `todo` is not a
//! task, and treating it as one would report state the graph does not have. The letter inside a
//! priority group is a different matter — nothing about `A` is case-sensitive to Logseq, so
//! [`Priority::leading_after`] takes `[#a]` as the same priority as `[#A]`. The DSL side is forgiving
//! throughout — [`TaskMarker::parse_dsl`] and [`Priority::parse_dsl`] accept any casing — because a
//! query is written by a person, not by Logseq.
//!
//! # What is not a marker
//!
//! Three shapes look like markers and are not:
//!
//! * a marker that is the whole text (`TODO`) is a block whose first word is a task word, not a task;
//!   the rule is "the first word, and something after it", so a bare word never counts.
//! * a word that merely starts with a marker (`TODOS`, `TODO:`): the first *word* has to be the
//!   marker, so the character after it must be whitespace.
//! * a marker anywhere but first (`x TODO`): only position zero is the leading position.
//!
//! `CANCELLED` — two L's — is not a Logseq task word and is refused; the marker is `CANCELED`. A
//! block that spells it with two L's is left with no marker rather than being normalized into one.
//!
//! All of this is a pure read of a `&str`: no IO, no SQL, no clock, no regular expression. The parser
//! axis and the facts projection both need the same answer to "is this block a task, and at what
//! priority", so the rule lives here once rather than once per caller (ADR-0003).

/// The task word a block's text leads with.
///
/// The variants are named as Rust types are, and [`as_str`](Self::as_str) is the spelling the file
/// holds. The seven words are Logseq's whole set; a block with any other leading word has no task
/// marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TaskMarker {
    /// `TODO`: not started.
    Todo,
    /// `DOING`: being worked on.
    Doing,
    /// `DONE`: finished.
    Done,
    /// `NOW`: worked on now, in the "now" workflow.
    Now,
    /// `LATER`: deferred, in the "later" workflow.
    Later,
    /// `WAITING`: blocked on something else.
    Waiting,
    /// `CANCELED`: dropped without being done.
    Canceled,
}

impl TaskMarker {
    /// Every marker, in no particular order: the order a listing reports them is a presentation that
    /// nothing here fixes.
    pub const ALL: [TaskMarker; 7] = [
        TaskMarker::Todo,
        TaskMarker::Doing,
        TaskMarker::Done,
        TaskMarker::Now,
        TaskMarker::Later,
        TaskMarker::Waiting,
        TaskMarker::Canceled,
    ];

    /// The spelling Logseq writes into the file, and the one [`leading`](Self::leading) accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskMarker::Todo => "TODO",
            TaskMarker::Doing => "DOING",
            TaskMarker::Done => "DONE",
            TaskMarker::Now => "NOW",
            TaskMarker::Later => "LATER",
            TaskMarker::Waiting => "WAITING",
            TaskMarker::Canceled => "CANCELED",
        }
    }

    /// The marker a query's word denotes, accepting any casing, or `None` when it denotes none of
    /// them.
    ///
    /// This is the DSL side: a query says `TODO` or `todo` and means the same marker. It is
    /// deliberately more permissive than [`leading`](Self::leading), which reads the file and has to
    /// agree with what Logseq actually wrote.
    pub fn parse_dsl(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|marker| marker.as_str().eq_ignore_ascii_case(word))
    }

    /// The marker this text leads with, when the text is a task: the first word is a marker and
    /// something other than whitespace follows it.
    ///
    /// The text is the block's content as the document layer holds it — indentation already removed,
    /// so a marker is expected at byte zero and one space in front of it means the first word is not
    /// the marker. Structure inside the text is left alone: this decides only whether the leading
    /// word is a task word.
    ///
    /// `None` covers every text that is not a task: a different first word, a marker in any casing
    /// but uppercase, [`CANCELED`](Self::Canceled) spelled with two L's, the marker alone, and a
    /// marker followed only by whitespace.
    pub fn leading(text: &str) -> Option<Self> {
        let (marker, after) = Self::leading_with_end(text)?;
        text[after..]
            .chars()
            .any(|character| !character.is_whitespace())
            .then_some(marker)
    }

    /// The leading marker and the byte offset just past it, without requiring anything after it.
    ///
    /// [`leading`](Self::leading) adds "and content follows"; [`Priority::leading_after`] needs the
    /// offset to continue reading where the marker ended. Both go through this one match so the two
    /// cannot disagree about what the first word is.
    fn leading_with_end(text: &str) -> Option<(Self, usize)> {
        for marker in Self::ALL {
            let word = marker.as_str();
            if !text.starts_with(word) {
                continue;
            }
            // `word` is ASCII, so this offset is a char boundary. The word is the whole first word
            // only when whitespace follows: `TODOS` and `TODO:` are other words.
            let end = word.len();
            return text[end..]
                .chars()
                .next()
                .filter(|character| character.is_whitespace())
                .map(|_| (marker, end));
        }
        None
    }
}

/// The priority a task carries: the `[#A]`, `[#B]`, or `[#C]` group that follows its marker.
///
/// Logseq has exactly three, and the file writes them uppercase even though nothing about the letter
/// is case-sensitive when a query names one. A group with any other letter, or a group that is not
/// where the priority belongs, is not a priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// `[#A]`: highest.
    A,
    /// `[#B]`: middle.
    B,
    /// `[#C]`: lowest.
    C,
}

impl Priority {
    /// Every priority, in no particular order.
    pub const ALL: [Priority; 3] = [Priority::A, Priority::B, Priority::C];

    /// The single letter a priority is spelled with, in the uppercase form the file usually holds.
    ///
    /// [`leading_after`](Self::leading_after) accepts the lowercase spelling of the same letter too,
    /// because the case of the letter carries no meaning; this method answers the one spelling
    /// callers store and print.
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::A => "A",
            Priority::B => "B",
            Priority::C => "C",
        }
    }

    /// The priority a query's word denotes, accepting any casing, or `None` when it denotes none of
    /// the three.
    ///
    /// The word is the letter alone — `A`, `a` — not the bracketed group the file holds; a query
    /// that says `[#A]` is asking about the group, which is [`leading_after`](Self::leading_after)'s
    /// question, not this one.
    pub fn parse_dsl(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|priority| priority.as_str().eq_ignore_ascii_case(word))
    }

    /// The priority that sits where a priority belongs, given the marker the caller found.
    ///
    /// With `task` set, the text must lead with exactly that marker, whitespace must follow the
    /// word, and `[#A]`-shaped text must begin immediately after that whitespace: `TODO [#A] buy
    /// milk` carries `A`, and `TODO [#D] buy milk` carries none because `D` is not a priority. A
    /// task the caller names that the text does not lead with is a contradiction, and answers `None`
    /// rather than guessing which of the two is right.
    ///
    /// With `task` as `None` the group must begin at byte zero, which is the shape of a block that
    /// carries a priority and no marker. Whitespace in front of it is not allowed for the same
    /// reason [`TaskMarker::leading`] rejects it: the block's indentation is already gone, so a
    /// leading space means the group is not the first thing in the text.
    ///
    /// A group later in the text (`TODO buy [#A] milk`) is not the priority: only the position right
    /// after the marker holds it, which is what keeps a `[#A]` in the description from being read as
    /// state. Nothing has to follow the group, unlike a task marker, because the group is the whole
    /// priority fact — a block ending in `TODO [#A]` still has one.
    pub fn leading_after(text: &str, task: Option<TaskMarker>) -> Option<Self> {
        let rest = match task {
            Some(expected) => {
                let (found, end) = TaskMarker::leading_with_end(text)?;
                if found != expected {
                    return None;
                }
                text[end..].trim_start()
            }
            None => text,
        };
        let group = rest.strip_prefix("[#")?.as_bytes();
        let letter = *group.first()?;
        if group.get(1) != Some(&b']') {
            return None;
        }
        Self::ALL
            .into_iter()
            .find(|priority| priority.as_str().as_bytes()[0].eq_ignore_ascii_case(&letter))
    }
}

impl std::fmt::Display for TaskMarker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::fmt::Display for Priority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_marker_round_trips_through_the_spelling_logseq_writes() {
        for marker in TaskMarker::ALL {
            assert_eq!(TaskMarker::parse_dsl(marker.as_str()), Some(marker));
            assert_eq!(marker.to_string(), marker.as_str());
            let text = format!("{} buy milk", marker.as_str());
            assert_eq!(TaskMarker::leading(&text), Some(marker), "{text}");
        }
    }

    #[test]
    fn every_priority_round_trips_through_its_letter() {
        for priority in Priority::ALL {
            assert_eq!(Priority::parse_dsl(priority.as_str()), Some(priority));
            assert_eq!(priority.to_string(), priority.as_str());
            let text = format!("TODO [#{}] buy milk", priority.as_str());
            assert_eq!(
                Priority::leading_after(&text, Some(TaskMarker::Todo)),
                Some(priority),
                "{text}"
            );
        }
    }

    #[test]
    fn a_bare_marker_or_a_marker_then_only_whitespace_is_not_a_task() {
        assert_eq!(TaskMarker::leading("TODO"), None);
        assert_eq!(TaskMarker::leading("TODO "), None);
        assert_eq!(TaskMarker::leading("TODO   \t\n "), None);
        assert_eq!(TaskMarker::leading("CANCELED"), None);
    }

    #[test]
    fn the_marker_must_be_the_whole_first_word() {
        assert_eq!(TaskMarker::leading("TODOS buy milk"), None);
        assert_eq!(TaskMarker::leading("TODO:buy milk"), None);
        assert_eq!(TaskMarker::leading("TODO-x"), None);
        assert_eq!(TaskMarker::leading("x TODO buy milk"), None);
    }

    #[test]
    fn the_file_spelling_is_uppercase_and_other_casings_are_not_markers() {
        for text in ["todo buy milk", "Todo buy milk", "tOdO buy milk", "doing x"] {
            assert_eq!(TaskMarker::leading(text), None, "{text}");
        }
    }

    #[test]
    fn the_dsl_spelling_accepts_any_casing() {
        assert_eq!(TaskMarker::parse_dsl("todo"), Some(TaskMarker::Todo));
        assert_eq!(TaskMarker::parse_dsl("ToDo"), Some(TaskMarker::Todo));
        assert_eq!(
            TaskMarker::parse_dsl("canceled"),
            Some(TaskMarker::Canceled)
        );
        assert_eq!(Priority::parse_dsl("a"), Some(Priority::A));
        assert_eq!(Priority::parse_dsl("c"), Some(Priority::C));
    }

    #[test]
    fn cancelled_with_two_ls_and_unknown_words_denote_nothing() {
        assert_eq!(TaskMarker::parse_dsl("CANCELLED"), None);
        assert_eq!(TaskMarker::parse_dsl("cancelled"), None);
        assert_eq!(TaskMarker::parse_dsl("LATER2"), None);
        assert_eq!(TaskMarker::parse_dsl(""), None);
        assert_eq!(TaskMarker::leading("CANCELLED buy milk"), None);
        assert_eq!(Priority::parse_dsl("D"), None);
        assert_eq!(Priority::parse_dsl("AA"), None);
        assert_eq!(Priority::parse_dsl(""), None);
    }

    #[test]
    fn a_priority_sits_right_after_the_marker() {
        assert_eq!(
            Priority::leading_after("TODO [#A] buy milk", Some(TaskMarker::Todo)),
            Some(Priority::A)
        );
        assert_eq!(
            Priority::leading_after("DOING [#B] buy milk", Some(TaskMarker::Doing)),
            Some(Priority::B)
        );
        assert_eq!(
            Priority::leading_after("DONE [#C] buy milk", Some(TaskMarker::Done)),
            Some(Priority::C)
        );
        assert_eq!(
            Priority::leading_after("TODO   [#A] buy milk", Some(TaskMarker::Todo)),
            Some(Priority::A)
        );
        assert_eq!(
            Priority::leading_after("TODO\t[#B] buy milk", Some(TaskMarker::Todo)),
            Some(Priority::B)
        );
    }

    #[test]
    fn a_letter_outside_a_to_c_is_not_a_priority() {
        assert_eq!(
            Priority::leading_after("TODO [#D] buy milk", Some(TaskMarker::Todo)),
            None
        );
        assert_eq!(
            Priority::leading_after("TODO [#a] buy milk", Some(TaskMarker::Todo)),
            Some(Priority::A)
        );
        assert_eq!(
            Priority::leading_after("TODO [#AA] buy milk", Some(TaskMarker::Todo)),
            None
        );
        assert_eq!(
            Priority::leading_after("TODO #A buy milk", Some(TaskMarker::Todo)),
            None
        );
    }

    #[test]
    fn without_a_marker_a_priority_must_begin_the_text() {
        assert_eq!(
            Priority::leading_after("[#A] buy milk", None),
            Some(Priority::A)
        );
        assert_eq!(
            Priority::leading_after("[#C] buy milk", None),
            Some(Priority::C)
        );
        assert_eq!(Priority::leading_after(" [#A] buy milk", None), None);
        assert_eq!(Priority::leading_after("buy milk [#A]", None), None);
        assert_eq!(
            Priority::leading_after("buy milk [#A]", Some(TaskMarker::Todo)),
            None
        );
    }

    #[test]
    fn a_priority_later_in_the_description_is_not_the_priority() {
        assert_eq!(
            Priority::leading_after("TODO buy [#A] milk", Some(TaskMarker::Todo)),
            None
        );
        assert_eq!(
            Priority::leading_after("buy TODO [#A] milk", Some(TaskMarker::Todo)),
            None
        );
        // The group is the whole fact, so nothing has to follow it.
        assert_eq!(
            Priority::leading_after("TODO [#A]", Some(TaskMarker::Todo)),
            Some(Priority::A)
        );
    }

    #[test]
    fn a_task_the_text_does_not_lead_with_contradicts_the_text() {
        assert_eq!(
            Priority::leading_after("TODO [#A] buy milk", Some(TaskMarker::Doing)),
            None
        );
        assert_eq!(
            Priority::leading_after("TODO [#A] buy milk", Some(TaskMarker::Todo)),
            Some(Priority::A)
        );
        assert_eq!(
            Priority::leading_after("TODO", Some(TaskMarker::Todo)),
            None
        );
    }

    #[test]
    fn leading_whitespace_means_the_marker_is_not_the_first_word() {
        assert_eq!(TaskMarker::leading(" TODO buy milk"), None);
        assert_eq!(TaskMarker::leading("\tTODO buy milk"), None);
    }

    #[test]
    fn unicode_around_the_marker_is_read_by_char_not_by_byte() {
        assert_eq!(TaskMarker::leading("TODO 买菜"), Some(TaskMarker::Todo));
        assert_eq!(TaskMarker::leading("TODO 🎉 party"), Some(TaskMarker::Todo));
        assert_eq!(TaskMarker::leading("待办 TODO"), None);
        assert_eq!(TaskMarker::leading("TODO待办"), None);
        assert_eq!(
            Priority::leading_after("TODO [#A] 买菜", Some(TaskMarker::Todo)),
            Some(Priority::A)
        );
        assert_eq!(Priority::leading_after("[#🎉] 买菜", None), None);
        for text in ["", "␣", "日本語のブロック", "TOD", "T", "[#"] {
            assert_eq!(TaskMarker::leading(text), None, "{text}");
            assert_eq!(Priority::leading_after(text, None), None, "{text}");
            assert_eq!(
                Priority::leading_after(text, Some(TaskMarker::Todo)),
                None,
                "{text}"
            );
        }
    }

    #[test]
    fn an_unclosed_group_is_not_a_priority() {
        assert_eq!(
            Priority::leading_after("TODO [#A buy milk", Some(TaskMarker::Todo)),
            None
        );
        assert_eq!(Priority::leading_after("[#B buy milk", None), None);
        assert_eq!(
            Priority::leading_after("TODO [#", Some(TaskMarker::Todo)),
            None
        );
        assert_eq!(Priority::leading_after("[#]", None), None);
    }

    #[test]
    fn the_letter_of_a_priority_reads_in_either_case() {
        assert_eq!(
            Priority::leading_after("[#b] buy milk", None),
            Some(Priority::B)
        );
        assert_eq!(
            Priority::leading_after("[#c] buy milk", None),
            Some(Priority::C)
        );
        assert_eq!(
            Priority::leading_after("TODO [#b] buy milk", Some(TaskMarker::Todo)),
            Some(Priority::B)
        );
        assert_eq!(
            Priority::leading_after("TODO [#c] buy milk", Some(TaskMarker::Todo)),
            Some(Priority::C)
        );
    }

    #[test]
    fn a_newline_after_the_marker_separates_the_next_word() {
        assert_eq!(
            TaskMarker::leading("TODO\nbuy milk"),
            Some(TaskMarker::Todo)
        );
        assert_eq!(
            TaskMarker::leading("TODO\r\nbuy milk"),
            Some(TaskMarker::Todo)
        );
        assert_eq!(
            Priority::leading_after("TODO\n[#B] buy milk", Some(TaskMarker::Todo)),
            Some(Priority::B)
        );
    }

    #[test]
    fn every_marker_word_is_the_prefix_of_no_other() {
        // `leading_with_end` returns on the first marker the text starts with, so a word that is a
        // prefix of another would make the answer depend on `ALL`'s order.
        for outer in TaskMarker::ALL {
            for inner in TaskMarker::ALL {
                assert!(
                    outer == inner || !inner.as_str().starts_with(outer.as_str()),
                    "{} prefixes {}",
                    outer.as_str(),
                    inner.as_str()
                );
            }
        }
    }
}
