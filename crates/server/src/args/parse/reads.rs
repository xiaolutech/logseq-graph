use logseq_graph::domain::JournalDate;

use super::super::arity::{expect_no_arguments, one_argument, reject_limit};
use super::super::{
    parse_selector, Command, UsageError, DEFAULT_QUERY_LIMIT, DEFAULT_SEARCH_LIMIT,
};

pub(super) fn build(
    name: &str,
    arguments: &[String],
    limit: Option<u32>,
    date: bool,
    tree: bool,
    today: Option<JournalDate>,
) -> Result<Command, UsageError> {
    Ok(match name {
        "info" => {
            expect_no_arguments("info", arguments)?;
            reject_limit("info", limit)?;
            Command::Info
        }
        "pages" => {
            expect_no_arguments("pages", arguments)?;
            reject_limit("pages", limit)?;
            Command::Pages { date }
        }
        "page" => {
            let command = Command::Page {
                name: one_argument("page", "a page name", arguments)?,
            };
            reject_limit("page", limit)?;
            command
        }
        "tree" => {
            let command = Command::Tree {
                name: one_argument("tree", "a page name", arguments)?,
            };
            reject_limit("tree", limit)?;
            command
        }
        "backlinks" => {
            let command = Command::Backlinks {
                name: one_argument("backlinks", "a page name", arguments)?,
            };
            reject_limit("backlinks", limit)?;
            command
        }
        "tags" => {
            expect_no_arguments("tags", arguments)?;
            reject_limit("tags", limit)?;
            Command::Tags
        }
        "tag" => {
            let command = Command::Tag {
                name: one_argument("tag", "a tag name", arguments)?,
            };
            reject_limit("tag", limit)?;
            command
        }
        "namespace" => {
            let command = Command::Namespace {
                name: one_argument("namespace", "a namespace name", arguments)?,
                tree,
            };
            reject_limit("namespace", limit)?;
            command
        }
        "search" => Command::Search {
            query: one_argument("search", "a query", arguments)?,
            limit: limit.unwrap_or(DEFAULT_SEARCH_LIMIT),
        },
        "find" => Command::Find {
            query: one_argument("find", "a name", arguments)?,
            limit: limit.unwrap_or(DEFAULT_SEARCH_LIMIT),
        },
        "query" => {
            let expr = one_argument("query", "a query", arguments)?;
            Command::Query {
                expr,
                limit: limit.unwrap_or(DEFAULT_QUERY_LIMIT),
                today,
            }
        }
        "block" => {
            let raw = one_argument("block", "a block selector", arguments)?;
            let selector = parse_selector(&raw, "block")?;
            reject_limit("block", limit)?;
            Command::Block { selector }
        }
        other => return Err(UsageError::UnknownCommand(other.to_owned())),
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::{invocation, parse_words};
    use super::super::super::{Command, UsageError, DEFAULT_QUERY_LIMIT};
    use logseq_graph::domain::JournalDate;

    #[test]
    fn query_defaults_its_limit_and_leaves_today_to_the_binary() {
        assert_eq!(
            invocation(&["query", "(task TODO)"]).command,
            Command::Query {
                expr: "(task TODO)".to_owned(),
                limit: DEFAULT_QUERY_LIMIT,
                today: None
            }
        );
        assert_eq!(
            invocation(&[
                "query",
                "(task TODO)",
                "--limit",
                "5",
                "--today",
                "2026-08-10"
            ])
            .command,
            Command::Query {
                expr: "(task TODO)".to_owned(),
                limit: 5,
                today: Some(JournalDate::new(2026, 8, 10).expect("a real day"))
            }
        );
    }

    #[test]
    fn today_belongs_to_the_query_command_alone() {
        assert_eq!(
            parse_words(&["pages", "--today", "2026-08-10"]),
            Err(UsageError::CommandNotForOption {
                option: "--today",
                command: "pages".to_owned()
            })
        );
    }

    #[test]
    fn pages_takes_the_date_flag_and_every_other_command_refuses_it() {
        // The flag is read wherever it stands, like every other option, and it is the only thing
        // that changes what `pages` prints.
        assert_eq!(
            invocation(&["pages", "--date"]).command,
            Command::Pages { date: true }
        );
        assert_eq!(
            invocation(&["--date", "pages"]).command,
            Command::Pages { date: true }
        );
        assert_eq!(
            invocation(&["pages"]).command,
            Command::Pages { date: false }
        );
        // No other command prints a column to put a day in, so a caller that asked for one is told
        // rather than answered without it. The command name is owned because the flag is parsed
        // before the command, exactly like `--bind`.
        for words in [
            vec!["--date", "info"],
            vec!["tree", "alpha", "--date"],
            vec!["serve", "--date"],
            vec!["--date", "explode"],
        ] {
            let command = words
                .iter()
                .find(|word| !word.starts_with('-'))
                .expect("each case names a command");
            assert_eq!(
                parse_words(&words),
                Err(UsageError::CommandNotForOption {
                    option: "--date",
                    command: (*command).to_owned()
                }),
                "{words:?} must be refused rather than answered"
            );
        }
    }

    #[test]
    fn a_block_argument_that_is_not_a_uuid_is_a_usage_error() {
        let error = parse_words(&["block", "not-a-uuid"]).expect_err("the id is malformed");
        match error {
            UsageError::InvalidValue { option, value, .. } => {
                assert_eq!(option, "block");
                assert_eq!(value, "not-a-uuid");
            }
            other => panic!("a malformed id is a usage error, not {other:?}"),
        }
    }

    #[test]
    fn namespace_accepts_a_multi_segment_name_and_the_tree_flag() {
        assert_eq!(
            invocation(&["namespace", "github.com/zhaochunqi/home-repo"]).command,
            Command::Namespace {
                name: "github.com/zhaochunqi/home-repo".to_owned(),
                tree: false
            }
        );
        assert_eq!(
            invocation(&["namespace", "github.com/zhaochunqi/home-repo", "--tree"]).command,
            Command::Namespace {
                name: "github.com/zhaochunqi/home-repo".to_owned(),
                tree: true
            }
        );
    }

    #[test]
    fn the_tree_flag_only_applies_to_namespace() {
        for command in ["pages", "tree", "serve"] {
            let words = vec![command, "--tree"];
            assert_eq!(
                parse_words(&words),
                Err(UsageError::CommandNotForOption {
                    option: "--tree",
                    command: command.to_owned()
                }),
                "{words:?} must be refused rather than answered"
            );
        }
    }
}
