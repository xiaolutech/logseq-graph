use super::super::arity::{one_argument, reject_limit, three_arguments, two_arguments};
use super::super::{parse_selector, Command, MoveDirection, UsageError};

/// Why `append` refuses the freshness flags.
const WRITE_SCANS_EVERY_FILE: &str =
    "a write reads every file before it edits one, which is stricter than either flag asks for";

/// Why `create` and `journal` refuse the freshness flags.
///
/// `append`'s wording does not fit: a create reads no file at all, so neither flag describes work it
/// skipped.
const CREATE_OBSERVES_ONE_FILE: &str =
    "a create writes one new file and reads nothing, so there is nothing for either flag to refresh";

pub(super) fn build(
    name: &str,
    arguments: &[String],
    freshness_flag: Option<&'static str>,
    limit: Option<u32>,
) -> Result<Command, UsageError> {
    Ok(match name {
        "create" => {
            if let Some(option) = freshness_flag {
                return Err(UsageError::OptionHasNoEffect {
                    option,
                    command: "create",
                    reason: CREATE_OBSERVES_ONE_FILE,
                });
            }
            reject_limit("create", limit)?;
            Command::Create {
                name: one_argument("create", "a page name", arguments)?,
            }
        }
        "journal" => {
            if let Some(option) = freshness_flag {
                return Err(UsageError::OptionHasNoEffect {
                    option,
                    command: "journal",
                    reason: CREATE_OBSERVES_ONE_FILE,
                });
            }
            reject_limit("journal", limit)?;
            let raw = one_argument("journal", "a date as YYYY-MM-DD", arguments)?;
            let date = crate::render::parse_day(&raw).ok_or(UsageError::InvalidValue {
                option: "journal",
                value: raw,
                expected: "a date as YYYY-MM-DD",
            })?;
            Command::Journal { date }
        }
        "commit" => {
            if let Some(option) = freshness_flag {
                return Err(UsageError::OptionHasNoEffect {
                    option,
                    command: "commit",
                    reason: WRITE_SCANS_EVERY_FILE,
                });
            }
            reject_limit("commit", limit)?;
            let (page, base_version, json) = three_arguments(
                "commit",
                "a page name",
                "a base version",
                "a JSON operations array",
                arguments,
            )?;
            let operations =
                crate::batch::parse_operations(&json).map_err(UsageError::InvalidOperations)?;
            Command::Commit {
                page,
                base_version,
                operations,
            }
        }
        "append" => {
            if let Some(option) = freshness_flag {
                return Err(UsageError::OptionHasNoEffect {
                    option,
                    command: "append",
                    reason: WRITE_SCANS_EVERY_FILE,
                });
            }
            reject_limit("append", limit)?;
            let (page, text) = two_arguments("append", "a page name", "block text", arguments)?;
            Command::Append { page, text }
        }
        "update" => {
            if let Some(option) = freshness_flag {
                return Err(UsageError::OptionHasNoEffect {
                    option,
                    command: "update",
                    reason: WRITE_SCANS_EVERY_FILE,
                });
            }
            reject_limit("update", limit)?;
            let (raw, text) = two_arguments("update", "a block selector", "block text", arguments)?;
            let selector = parse_selector(&raw, "update")?;
            Command::Update { selector, text }
        }
        "ensure-block-id" => {
            if let Some(option) = freshness_flag {
                return Err(UsageError::OptionHasNoEffect {
                    option,
                    command: "ensure-block-id",
                    reason: WRITE_SCANS_EVERY_FILE,
                });
            }
            reject_limit("ensure-block-id", limit)?;
            let raw = one_argument("ensure-block-id", "a block selector", arguments)?;
            Command::EnsureBlockId {
                selector: parse_selector(&raw, "ensure-block-id")?,
            }
        }
        "remove" => {
            if let Some(option) = freshness_flag {
                return Err(UsageError::OptionHasNoEffect {
                    option,
                    command: "remove",
                    reason: WRITE_SCANS_EVERY_FILE,
                });
            }
            reject_limit("remove", limit)?;
            let raw = one_argument("remove", "a block selector", arguments)?;
            let selector = parse_selector(&raw, "remove")?;
            Command::Remove { selector }
        }
        "remove-range" => {
            if let Some(option) = freshness_flag {
                return Err(UsageError::OptionHasNoEffect {
                    option,
                    command: "remove-range",
                    reason: WRITE_SCANS_EVERY_FILE,
                });
            }
            reject_limit("remove-range", limit)?;
            let (first, last) = two_arguments(
                "remove-range",
                "a first block selector",
                "a last block selector",
                arguments,
            )?;
            Command::RemoveRange {
                first: parse_selector(&first, "remove-range")?,
                last: parse_selector(&last, "remove-range")?,
            }
        }
        "move" => {
            if let Some(option) = freshness_flag {
                return Err(UsageError::OptionHasNoEffect {
                    option,
                    command: "move",
                    reason: WRITE_SCANS_EVERY_FILE,
                });
            }
            reject_limit("move", limit)?;
            let (raw, word, raw_anchor) = three_arguments(
                "move",
                "a block selector",
                MoveDirection::WORDS,
                "an anchor block selector",
                arguments,
            )?;
            let selector = parse_selector(&raw, "move")?;
            let direction = MoveDirection::parse(&word).ok_or(UsageError::InvalidValue {
                option: "move",
                value: word,
                expected: MoveDirection::WORDS,
            })?;
            let anchor = parse_selector(&raw_anchor, "move")?;
            Command::Move {
                selector,
                direction,
                anchor,
            }
        }
        other => return Err(UsageError::UnknownCommand(other.to_owned())),
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::{invocation, parse_words};
    use super::super::super::{Command, MoveDirection, UsageError};
    use super::WRITE_SCANS_EVERY_FILE;
    use logseq_graph::domain::{BlockId, BlockSelector};
    use std::path::PathBuf;

    #[test]
    fn remove_range_takes_two_selectors_without_freshness_or_limit() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        assert_eq!(
            invocation(&["remove-range", id, id]).command,
            Command::RemoveRange {
                first: BlockSelector::Durable(BlockId::parse(id).expect("UUID")),
                last: BlockSelector::Durable(BlockId::parse(id).expect("UUID")),
            }
        );
        for words in [
            vec!["remove-range"],
            vec!["remove-range", id],
            vec!["remove-range", "", id],
            vec!["remove-range", id, id, "extra"],
            vec!["--stale", "remove-range", id, id],
            vec!["remove-range", id, id, "--limit", "2"],
        ] {
            assert!(parse_words(&words).is_err(), "{words:?}");
        }
    }

    #[test]
    fn ensure_block_id_takes_exactly_one_selector_and_no_freshness_options() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        assert_eq!(
            invocation(&["ensure-block-id", id]).command,
            Command::EnsureBlockId {
                selector: BlockSelector::Durable(BlockId::parse(id).expect("UUID"))
            }
        );
        for words in [
            vec!["ensure-block-id"],
            vec!["ensure-block-id", ""],
            vec!["ensure-block-id", id, "extra"],
            vec!["--stale", "ensure-block-id", id],
            vec!["--verify-content", "ensure-block-id", id],
            vec!["ensure-block-id", id, "--limit", "2"],
        ] {
            assert!(parse_words(&words).is_err(), "{words:?}");
        }
    }

    #[test]
    fn remove_needs_one_selector_and_nothing_else() {
        assert_eq!(
            parse_words(&["remove"]),
            Err(UsageError::MissingArgument {
                command: "remove",
                argument: "a block selector"
            })
        );
        assert_eq!(
            parse_words(&["remove", "01234567-89ab-cdef-0123-456789abcdef", "extra"]),
            Err(UsageError::UnexpectedArgument {
                command: "remove",
                argument: "extra".to_owned()
            })
        );
        assert_eq!(
            parse_words(&["remove", "not-an-id"]),
            Err(UsageError::InvalidValue {
                option: "remove",
                value: "not-an-id".to_owned(),
                expected: "a durable or ephemeral block selector"
            })
        );
    }

    #[test]
    fn remove_refuses_the_freshness_flags_the_way_the_other_writes_do() {
        for option in ["--verify-content", "--stale"] {
            let error = parse_words(&[option, "remove", "01234567-89ab-cdef-0123-456789abcdef"])
                .expect_err("a write has no freshness policy to choose");
            assert_eq!(
                error,
                UsageError::OptionHasNoEffect {
                    option,
                    command: "remove",
                    reason: WRITE_SCANS_EVERY_FILE
                }
            );
        }
    }

    #[test]
    fn move_needs_two_selectors_and_a_direction() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";

        assert_eq!(
            parse_words(&["move"]),
            Err(UsageError::MissingArgument {
                command: "move",
                argument: "a block selector"
            })
        );
        assert_eq!(
            parse_words(&["move", id]),
            Err(UsageError::MissingArgument {
                command: "move",
                argument: MoveDirection::WORDS
            })
        );
        assert_eq!(
            parse_words(&["move", id, "after"]),
            Err(UsageError::MissingArgument {
                command: "move",
                argument: "an anchor block selector"
            })
        );
        assert_eq!(
            parse_words(&["move", id, "after", id, "extra"]),
            Err(UsageError::UnexpectedArgument {
                command: "move",
                argument: "extra".to_owned()
            })
        );
        assert_eq!(
            parse_words(&["move", "not-an-id", "after", id]),
            Err(UsageError::InvalidValue {
                option: "move",
                value: "not-an-id".to_owned(),
                expected: "a durable or ephemeral block selector"
            })
        );
        assert_eq!(
            parse_words(&["move", id, "sideways", id]),
            Err(UsageError::InvalidValue {
                option: "move",
                value: "sideways".to_owned(),
                expected: MoveDirection::WORDS
            })
        );
        assert_eq!(
            parse_words(&["move", id, "before", "not-an-id"]),
            Err(UsageError::InvalidValue {
                option: "move",
                value: "not-an-id".to_owned(),
                expected: "a durable or ephemeral block selector"
            })
        );
    }

    #[test]
    fn every_move_direction_parses_and_lands_on_its_own_variant() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        for (word, direction) in [
            ("before", MoveDirection::Before),
            ("after", MoveDirection::After),
            ("first-child", MoveDirection::FirstChild),
            ("last-child", MoveDirection::LastChild),
        ] {
            assert_eq!(
                invocation(&["move", id, word, id]).command,
                Command::Move {
                    selector: BlockSelector::Durable(BlockId::parse(id).expect("valid id")),
                    direction,
                    anchor: BlockSelector::Durable(BlockId::parse(id).expect("valid id"))
                },
                "{word} names {direction:?}"
            );
        }
    }

    #[test]
    fn move_refuses_the_freshness_flags_the_way_the_other_writes_do() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        for option in ["--verify-content", "--stale"] {
            let error = parse_words(&[option, "move", id, "after", id])
                .expect_err("a write has no freshness policy to choose");
            assert_eq!(
                error,
                UsageError::OptionHasNoEffect {
                    option,
                    command: "move",
                    reason: WRITE_SCANS_EVERY_FILE
                }
            );
        }
        assert_eq!(
            parse_words(&["move", id, "after", id, "--limit", "2"]),
            Err(UsageError::OptionNotForCommand {
                option: "--limit",
                command: "move"
            })
        );
    }

    #[test]
    fn update_needs_a_selector_and_one_line_of_text() {
        assert_eq!(
            parse_words(&["update"]),
            Err(UsageError::MissingArgument {
                command: "update",
                argument: "a block selector"
            })
        );
        assert_eq!(
            parse_words(&["update", "01234567-89ab-cdef-0123-456789abcdef"]),
            Err(UsageError::MissingArgument {
                command: "update",
                argument: "block text"
            })
        );
        assert_eq!(
            parse_words(&["update", "not-an-id", "text"]),
            Err(UsageError::InvalidValue {
                option: "update",
                value: "not-an-id".to_owned(),
                expected: "a durable or ephemeral block selector"
            })
        );
    }

    #[test]
    fn update_refuses_the_freshness_flags_the_way_append_does() {
        for option in ["--verify-content", "--stale"] {
            let error = parse_words(&[
                option,
                "update",
                "01234567-89ab-cdef-0123-456789abcdef",
                "t",
            ])
            .expect_err("a write has no freshness policy to choose");
            assert_eq!(
                error,
                UsageError::OptionHasNoEffect {
                    option,
                    command: "update",
                    reason: WRITE_SCANS_EVERY_FILE
                }
            );
        }
    }

    #[test]
    fn append_needs_both_of_its_arguments() {
        assert_eq!(
            parse_words(&["append"]),
            Err(UsageError::MissingArgument {
                command: "append",
                argument: "a page name"
            })
        );
        assert_eq!(
            parse_words(&["append", "alpha"]),
            Err(UsageError::MissingArgument {
                command: "append",
                argument: "block text"
            })
        );
        assert_eq!(
            parse_words(&["append", "alpha", "one", "two"]),
            Err(UsageError::UnexpectedArgument {
                command: "append",
                argument: "two".to_owned()
            })
        );
    }

    #[test]
    fn append_refuses_the_freshness_flags_and_says_why() {
        for option in ["--verify-content", "--stale"] {
            let error = parse_words(&[option, "append", "alpha", "text"])
                .expect_err("a write has no freshness policy to choose");
            assert_eq!(
                error,
                UsageError::OptionHasNoEffect {
                    option,
                    command: "append",
                    reason: WRITE_SCANS_EVERY_FILE
                }
            );
            // The message names the flag and the reason, so a caller learns why it was refused
            // instead of guessing that the flag was honoured.
            let message = error.to_string();
            assert!(message.contains(option), "{message}");
            assert!(message.contains("reads every file"), "{message}");
        }
    }

    #[test]
    fn append_still_takes_the_options_that_apply_to_every_command() {
        let parsed = invocation(&["-g", "/graphs/work", "append", "alpha", "text"]);

        assert_eq!(parsed.graph, Some(PathBuf::from("/graphs/work")));
        assert_eq!(
            parsed.command,
            Command::Append {
                page: "alpha".to_owned(),
                text: "text".to_owned()
            }
        );
        assert_eq!(
            parse_words(&["append", "alpha", "text", "--limit", "2"]),
            Err(UsageError::OptionNotForCommand {
                option: "--limit",
                command: "append"
            })
        );
    }
}
