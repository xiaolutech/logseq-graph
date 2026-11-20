use logseq_graph::domain::{BlockId, BlockSelector, EphemeralId, JournalDate};

use super::UsageError;

/// The spellings a selector may be written in, as every message lists them.
const SELECTOR_SPELLINGS: &str = "a durable or ephemeral block selector";

/// The prefix that names durable identity, the kind a bare UUID already means.
pub(crate) const DURABLE_PREFIX: &str = "durable:";

/// The prefix that names ephemeral identity: the id the index minted for a block that states none.
pub(crate) const EPHEMERAL_PREFIX: &str = "ephemeral:";

/// Parse one block selector, with `command` naming the argument it came from.
///
/// A bare UUID means `durable:`, because that is what every command line written before selectors
/// existed meant, and changing it would silently redirect a script at a different block. The two
/// prefixed spellings are the only way to say which kind is meant otherwise, and the pair is needed
/// rather than one arbitrary prefix: the two ids have the same shape and never convert into one
/// another (ADR-0012), so a string alone cannot say which namespace it is in.
pub(crate) fn parse_selector(
    word: &str,
    command: &'static str,
) -> Result<BlockSelector, UsageError> {
    let invalid = |expected: &'static str| UsageError::InvalidValue {
        option: command,
        value: word.to_owned(),
        expected,
    };
    if let Some(rest) = word.strip_prefix(DURABLE_PREFIX) {
        return BlockId::parse(rest)
            .map(BlockSelector::Durable)
            .map_err(|_| invalid(SELECTOR_SPELLINGS));
    }
    if let Some(rest) = word.strip_prefix(EPHEMERAL_PREFIX) {
        return EphemeralId::parse(rest)
            .map(BlockSelector::Ephemeral)
            .map_err(|_| invalid(SELECTOR_SPELLINGS));
    }
    BlockId::parse(word)
        .map(BlockSelector::Durable)
        .map_err(|_| invalid(SELECTOR_SPELLINGS))
}

/// One calendar day written `YYYY-MM-DD`, as `--today` spells it.
///
/// A day the proleptic Gregorian calendar does not have is refused rather than carried as a string:
/// a query measures its offsets from this value, so `2026-02-30` must not become March by accident.
pub(crate) fn parse_day(word: &str) -> Result<JournalDate, UsageError> {
    crate::render::parse_day(word).ok_or_else(|| UsageError::InvalidValue {
        option: "--today",
        value: word.to_owned(),
        expected: "a date as YYYY-MM-DD",
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_support::ID;
    use super::super::{parse_day, parse_selector};
    use logseq_graph::domain::{BlockId, BlockSelector, EphemeralId, JournalDate};

    #[test]
    fn a_bare_id_still_names_durable_identity() {
        // Every command line written before selectors existed means `durable:`, so this spelling
        // must not change meaning: a script that worked would otherwise edit a different block.
        assert_eq!(
            parse_selector(ID, "block").expect("a bare id parses"),
            BlockSelector::Durable(BlockId::parse(ID).expect("valid id"))
        );
    }

    #[test]
    fn a_prefix_names_which_kind_of_identity_is_meant() {
        assert_eq!(
            parse_selector(&format!("durable:{ID}"), "block").expect("a prefixed id parses"),
            BlockSelector::Durable(BlockId::parse(ID).expect("valid id"))
        );
        assert_eq!(
            parse_selector(&format!("ephemeral:{ID}"), "block").expect("a prefixed id parses"),
            BlockSelector::Ephemeral(EphemeralId::parse(ID).expect("valid id"))
        );
        // The two kinds are told apart by the prefix and by nothing else: the ids have the same
        // shape, so a wrong prefix must be refused rather than reinterpreted.
        assert!(parse_selector("ephemeral:not-an-id", "block").is_err());
        assert!(parse_selector("durable:", "block").is_err());
        assert!(parse_selector("", "block").is_err());
    }

    #[test]
    fn a_day_the_calendar_does_not_have_is_refused() {
        assert_eq!(
            parse_day("2026-08-10").expect("a real day"),
            JournalDate::new(2026, 8, 10).expect("a real day")
        );
        for word in [
            "2026-02-30",
            "2026-13-01",
            "2026-8-10",
            "10-08-2026",
            "2026/08/10",
            "",
            "2026-08-1x",
        ] {
            assert!(parse_day(word).is_err(), "{word:?} is not a day");
        }
    }
}
