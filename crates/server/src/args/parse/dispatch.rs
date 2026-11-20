use std::time::Duration;

use logseq_graph::domain::JournalDate;

use super::super::{Command, UsageError};

pub(super) struct Options {
    pub(super) freshness_flag: Option<&'static str>,
    pub(super) limit: Option<u32>,
    pub(super) bind: Option<String>,
    pub(super) api_tokens: bool,
    pub(super) poll_interval: Option<Duration>,
    pub(super) no_watch: bool,
    pub(super) date: bool,
    pub(super) tree: bool,
    pub(super) today: Option<JournalDate>,
}

pub(super) fn build(words: &[String], options: Options) -> Result<Command, UsageError> {
    let Options {
        freshness_flag,
        limit,
        bind,
        api_tokens,
        poll_interval,
        no_watch,
        date,
        tree,
        today,
    } = options;
    let Some(name) = words.first().map(String::as_str) else {
        return Err(UsageError::MissingCommand);
    };
    let arguments = &words[1..];

    // `--bind` names the address only `serve` listens on, so a command that does not listen
    // cannot honour it; refusing beats answering while ignoring what the caller asked for. The
    // check runs before the command match, because the guard cannot outrank a literal arm, and
    // the command name is cloned because the option is parsed before the command and the command
    // word is not a literal.
    for (option, present) in [
        ("--bind", bind.is_some()),
        ("--api-token", api_tokens),
        ("--poll-interval", poll_interval.is_some()),
        ("--no-watch", no_watch),
    ] {
        if present && name != "serve" {
            return Err(UsageError::CommandNotForOption {
                option,
                command: name.to_owned(),
            });
        }
    }

    // `--date` adds the third column of the `pages` listing and no other command prints one, so a
    // command that lists nothing cannot honour it. Refused for the reason `--bind` is: answering
    // while ignoring the flag would let a caller believe it saw a column that was never printed.
    if date && name != "pages" {
        return Err(UsageError::CommandNotForOption {
            option: "--date",
            command: name.to_owned(),
        });
    }

    // `--tree` widens the `namespace` listing to every depth. No other command has a namespace
    // subtree to widen, so accepting it there would answer while ignoring what the caller asked.
    if tree && name != "namespace" {
        return Err(UsageError::CommandNotForOption {
            option: "--tree",
            command: name.to_owned(),
        });
    }

    // `--today` moves the day a query's relative operands count from, and no other command has a
    // day to move: refusing beats accepting a date that would change nothing.
    if today.is_some() && name != "query" {
        return Err(UsageError::CommandNotForOption {
            option: "--today",
            command: name.to_owned(),
        });
    }

    match name {
        "info" | "pages" | "page" | "tree" | "backlinks" | "tags" | "tag" | "search" | "find"
        | "namespace" | "query" | "block" => {
            super::reads::build(name, arguments, limit, date, tree, today)
        }
        "serve" => super::serve::build(
            arguments,
            freshness_flag,
            limit,
            bind,
            poll_interval,
            no_watch,
        ),
        // `--bind` names the address only `serve` listens on, so a command that does not listen
        // cannot honour it; refusing beats answering while ignoring what the caller asked for.
        // The command name is cloned because the option is parsed before the command and the
        // command word is not a literal.
        _ if bind.is_some() => Err(UsageError::CommandNotForOption {
            option: "--bind",
            command: name.to_owned(),
        }),
        "create" | "journal" | "commit" | "append" | "update" | "remove" | "move"
        | "ensure-block-id" | "remove-range" => {
            super::writes::build(name, arguments, freshness_flag, limit)
        }
        // `--bind` is checked before this match; any command here but `serve` was already refused.
        other => Err(UsageError::UnknownCommand(other.to_owned())),
    }
}
