use std::path::PathBuf;
use std::time::Duration;

use logseq_graph::domain::JournalDate;
use logseq_graph::engine::Freshness;

use super::arity::take_value;
use super::{parse_day, Invocation, Parsed, UsageError};
use dispatch::{build, Options};

mod dispatch;
mod reads;
mod serve;
mod writes;

/// Parse the arguments that follow the program name.
pub(crate) fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, UsageError> {
    let mut graph = None;
    let mut index = None;
    let mut freshness = Freshness::CheckNow;
    // Which flag, if any, set the freshness policy. `Freshness` has no "unset" value, so the
    // commands that refuse the flags need this to tell a named policy from the default one.
    let mut freshness_flag: Option<&'static str> = None;
    let mut limit: Option<u32> = None;
    let mut bind: Option<String> = None;
    let mut api_tokens: Vec<String> = Vec::new();
    let mut poll_interval: Option<Duration> = None;
    let mut no_watch = false;
    let mut date = false;
    let mut today: Option<JournalDate> = None;
    let mut words: Vec<String> = Vec::new();
    let mut args = args.into_iter();

    while let Some(argument) = args.next() {
        // `--option=value` and `--option value` are both accepted; only options split on the first
        // `=`, so a value that holds one is passed through unchanged.
        let (option, inline) = match argument.split_once('=') {
            Some((option, value)) if option.starts_with('-') => {
                (option.to_owned(), Some(value.to_owned()))
            }
            _ => (argument.clone(), None),
        };

        match option.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-g" | "--graph" => {
                graph = Some(PathBuf::from(take_value(&mut args, inline, "--graph")?))
            }
            "--index" => index = Some(PathBuf::from(take_value(&mut args, inline, "--index")?)),
            "--verify-content" => {
                freshness = Freshness::VerifyContent;
                freshness_flag = Some("--verify-content");
            }
            "--stale" => {
                freshness = Freshness::AllowStale;
                freshness_flag = Some("--stale");
            }
            "--limit" => {
                let value = take_value(&mut args, inline, "--limit")?;
                let parsed = value.parse::<u32>().map_err(|_| UsageError::InvalidValue {
                    option: "--limit",
                    value: value.clone(),
                    expected: "a whole number",
                })?;
                limit = Some(parsed);
            }
            "--bind" => {
                bind = Some(take_value(&mut args, inline, "--bind")?);
            }
            "--api-token" => {
                // The official rule matches a token as one run of non-whitespace characters, so a
                // token that holds whitespace could never be the token a request presents. Refusing
                // it here names the mistake once instead of leaving a server that always says no;
                // an empty value is already `MissingValue`, because `take_value` rejects it.
                let value = take_value(&mut args, inline, "--api-token")?;
                if value.chars().any(char::is_whitespace) {
                    // ADR-0018: the token is never echoed in a message, a log line, or an error
                    // body — the refusal names the rule, not the value.
                    return Err(UsageError::InvalidValue {
                        option: "--api-token",
                        value: "<redacted>".to_owned(),
                        expected: "a token with no whitespace",
                    });
                }
                api_tokens.push(value);
            }
            "--no-watch" => no_watch = true,
            "--poll-interval" => {
                let value = take_value(&mut args, inline, "--poll-interval")?;
                let milliseconds = value.parse::<u64>().map_err(|_| UsageError::InvalidValue {
                    option: "--poll-interval",
                    value: value.clone(),
                    expected: "a positive whole number of milliseconds",
                })?;
                if milliseconds == 0 {
                    return Err(UsageError::InvalidValue {
                        option: "--poll-interval",
                        value,
                        expected: "a positive whole number of milliseconds",
                    });
                }
                poll_interval = Some(Duration::from_millis(milliseconds));
            }
            "--date" => date = true,
            "--today" => {
                let value = take_value(&mut args, inline, "--today")?;
                today = Some(parse_day(&value)?);
            }
            other if other.starts_with('-') && other != "-" => {
                return Err(UsageError::UnknownOption(argument))
            }
            _ => words.push(argument),
        }
    }

    let command = build(
        &words,
        Options {
            freshness_flag,
            limit,
            bind,
            api_tokens: !api_tokens.is_empty(),
            poll_interval,
            no_watch,
            date,
            today,
        },
    )?;

    Ok(Parsed::Run(Invocation {
        graph,
        index,
        freshness,
        api_tokens,
        command,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{invocation, parse_words};
    use super::super::{Command, MoveDirection, Parsed, UsageError, DEFAULT_SEARCH_LIMIT};
    use logseq_graph::domain::{BlockId, BlockSelector};
    use logseq_graph::engine::Freshness;
    use std::path::PathBuf;

    #[test]
    fn every_command_parses_with_its_argument() {
        assert_eq!(invocation(&["info"]).command, Command::Info);
        assert_eq!(
            invocation(&["pages"]).command,
            Command::Pages { date: false }
        );
        assert_eq!(
            invocation(&["page", "alpha"]).command,
            Command::Page {
                name: "alpha".to_owned()
            }
        );
        assert_eq!(
            invocation(&["tree", "alpha"]).command,
            Command::Tree {
                name: "alpha".to_owned()
            }
        );
        assert_eq!(
            invocation(&["backlinks", "alpha"]).command,
            Command::Backlinks {
                name: "alpha".to_owned()
            }
        );
        assert_eq!(invocation(&["tags"]).command, Command::Tags);
        assert_eq!(
            invocation(&["tag", "project"]).command,
            Command::Tag {
                name: "project".to_owned()
            }
        );
        assert_eq!(
            invocation(&["search", "needle"]).command,
            Command::Search {
                query: "needle".to_owned(),
                limit: DEFAULT_SEARCH_LIMIT
            }
        );
        assert_eq!(
            invocation(&["find", "zmi"]).command,
            Command::Find {
                query: "zmi".to_owned(),
                limit: DEFAULT_SEARCH_LIMIT
            }
        );
        assert_eq!(
            invocation(&["find", "zmi", "--limit", "3"]).command,
            Command::Find {
                query: "zmi".to_owned(),
                limit: 3
            }
        );
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        assert_eq!(
            invocation(&["block", id]).command,
            Command::Block {
                selector: BlockSelector::Durable(BlockId::parse(id).expect("valid id"))
            }
        );
        assert_eq!(
            invocation(&["append", "alpha", "one new line"]).command,
            Command::Append {
                page: "alpha".to_owned(),
                text: "one new line".to_owned()
            }
        );
        assert_eq!(
            invocation(&["update", id, "new text"]).command,
            Command::Update {
                selector: BlockSelector::Durable(BlockId::parse(id).expect("valid id")),
                text: "new text".to_owned()
            }
        );
        assert_eq!(
            invocation(&["remove", id]).command,
            Command::Remove {
                selector: BlockSelector::Durable(BlockId::parse(id).expect("valid id"))
            }
        );
        assert_eq!(
            invocation(&["move", id, "after", id]).command,
            Command::Move {
                selector: BlockSelector::Durable(BlockId::parse(id).expect("valid id")),
                direction: MoveDirection::After,
                anchor: BlockSelector::Durable(BlockId::parse(id).expect("valid id"))
            }
        );
    }

    #[test]
    fn the_shared_options_come_out_of_the_command_line() {
        let parsed = invocation(&[
            "--graph",
            "/graphs/work",
            "--index",
            "/cache/index.db",
            "--verify-content",
            "pages",
        ]);

        assert_eq!(parsed.graph, Some(PathBuf::from("/graphs/work")));
        assert_eq!(parsed.index, Some(PathBuf::from("/cache/index.db")));
        assert_eq!(parsed.freshness, Freshness::VerifyContent);
        assert_eq!(parsed.command, Command::Pages { date: false });
    }

    #[test]
    fn options_are_read_after_the_command_too_and_in_the_equals_form() {
        let parsed = invocation(&[
            "search",
            "needle",
            "--limit=3",
            "--stale",
            "-g",
            "/graphs/w",
        ]);

        assert_eq!(
            parsed.command,
            Command::Search {
                query: "needle".to_owned(),
                limit: 3
            }
        );
        assert_eq!(parsed.graph, Some(PathBuf::from("/graphs/w")));
        assert_eq!(parsed.freshness, Freshness::AllowStale);
    }

    #[test]
    fn the_last_freshness_option_wins_and_the_default_scans_now() {
        assert_eq!(invocation(&["info"]).freshness, Freshness::CheckNow);
        assert_eq!(
            invocation(&["--stale", "--verify-content", "info"]).freshness,
            Freshness::VerifyContent
        );
        assert_eq!(
            invocation(&["--verify-content", "--stale", "info"]).freshness,
            Freshness::AllowStale
        );
    }

    #[test]
    fn help_is_answered_without_a_command() {
        assert_eq!(parse_words(&["--help"]).expect("help parses"), Parsed::Help);
        assert_eq!(parse_words(&["-h"]).expect("help parses"), Parsed::Help);
    }

    #[test]
    fn a_missing_command_and_an_unknown_command_are_usage_errors() {
        assert_eq!(parse_words(&[]), Err(UsageError::MissingCommand));
        assert_eq!(
            parse_words(&["--graph", "/graphs/w"]),
            Err(UsageError::MissingCommand)
        );
        assert_eq!(
            parse_words(&["explode"]),
            Err(UsageError::UnknownCommand("explode".to_owned()))
        );
    }

    #[test]
    fn unknown_options_and_missing_values_are_usage_errors() {
        assert_eq!(
            parse_words(&["--nope", "info"]),
            Err(UsageError::UnknownOption("--nope".to_owned()))
        );
        assert_eq!(
            parse_words(&["info", "--index"]),
            Err(UsageError::MissingValue("--index"))
        );
        assert_eq!(
            parse_words(&["--graph=", "info"]),
            Err(UsageError::MissingValue("--graph"))
        );
        assert_eq!(
            parse_words(&["search", "needle", "--limit", "many"]),
            Err(UsageError::InvalidValue {
                option: "--limit",
                value: "many".to_owned(),
                expected: "a whole number"
            })
        );
    }

    #[test]
    fn a_command_rejects_the_arguments_it_does_not_take() {
        assert_eq!(
            parse_words(&["info", "extra"]),
            Err(UsageError::UnexpectedArgument {
                command: "info",
                argument: "extra".to_owned()
            })
        );
        assert_eq!(
            parse_words(&["page"]),
            Err(UsageError::MissingArgument {
                command: "page",
                argument: "a page name"
            })
        );
        assert_eq!(
            parse_words(&["page", "alpha", "beta"]),
            Err(UsageError::UnexpectedArgument {
                command: "page",
                argument: "beta".to_owned()
            })
        );
        assert_eq!(
            parse_words(&["tags", "--limit", "3"]),
            Err(UsageError::OptionNotForCommand {
                option: "--limit",
                command: "tags"
            })
        );
    }
}
