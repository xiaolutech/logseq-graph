use std::time::Duration;

use super::super::arity::{expect_no_arguments, reject_limit};
use super::super::{Command, UsageError, BIND_DEFAULT, DEFAULT_POLL_INTERVAL};

/// Why `serve` refuses the freshness flags.
///
/// `serve` scans at startup and polls in the background, and every request names its own policy,
/// describes a behaviour the process could take: `--verify-content` would claim a content check
/// this command never runs, and `--stale` would claim a default the protocol sets per request.
const SERVE_CHOOSES_PER_REQUEST: &str =
    "serve polls in the background and every request names its own freshness policy";

pub(super) fn build(
    arguments: &[String],
    freshness_flag: Option<&'static str>,
    limit: Option<u32>,
    bind: Option<String>,
    poll_interval: Option<Duration>,
    no_watch: bool,
) -> Result<Command, UsageError> {
    if let Some(option) = freshness_flag {
        return Err(UsageError::OptionHasNoEffect {
            option,
            command: "serve",
            reason: SERVE_CHOOSES_PER_REQUEST,
        });
    }
    reject_limit("serve", limit)?;
    expect_no_arguments("serve", arguments)?;
    Ok(Command::Serve {
        bind: bind.unwrap_or_else(|| BIND_DEFAULT.to_owned()),
        poll_interval: poll_interval.unwrap_or(DEFAULT_POLL_INTERVAL),
        no_watch,
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::{invocation, parse_words};
    use super::super::super::{Command, UsageError, BIND_DEFAULT, DEFAULT_POLL_INTERVAL};
    use super::SERVE_CHOOSES_PER_REQUEST;
    use std::time::Duration;

    #[test]
    fn serve_takes_only_bind_and_defaults_to_the_loopback_port() {
        assert_eq!(
            invocation(&["serve"]).command,
            Command::Serve {
                bind: BIND_DEFAULT.to_owned(),
                poll_interval: DEFAULT_POLL_INTERVAL,
                no_watch: false
            }
        );
        assert_eq!(
            invocation(&["serve", "--bind", "127.0.0.1:9999"]).command,
            Command::Serve {
                bind: "127.0.0.1:9999".to_owned(),
                poll_interval: DEFAULT_POLL_INTERVAL,
                no_watch: false
            }
        );
        assert_eq!(
            invocation(&["serve", "--bind=127.0.0.1:9999"]).command,
            Command::Serve {
                bind: "127.0.0.1:9999".to_owned(),
                poll_interval: DEFAULT_POLL_INTERVAL,
                no_watch: false
            }
        );
        assert_eq!(
            invocation(&["-g", "/graphs/w", "serve"]).command,
            Command::Serve {
                bind: BIND_DEFAULT.to_owned(),
                poll_interval: DEFAULT_POLL_INTERVAL,
                no_watch: false
            }
        );
        assert_eq!(
            parse_words(&["serve", "extra"]),
            Err(UsageError::UnexpectedArgument {
                command: "serve",
                argument: "extra".to_owned()
            })
        );
        assert_eq!(
            parse_words(&["serve", "--limit", "2"]),
            Err(UsageError::OptionNotForCommand {
                option: "--limit",
                command: "serve"
            })
        );
        assert_eq!(
            parse_words(&["--bind", "127.0.0.1:9999", "info"]),
            Err(UsageError::CommandNotForOption {
                option: "--bind",
                command: "info".to_owned()
            })
        );
        assert_eq!(
            parse_words(&["--bind", "127.0.0.1:9999", "explode"]),
            Err(UsageError::CommandNotForOption {
                option: "--bind",
                command: "explode".to_owned()
            })
        );
        assert_eq!(
            parse_words(&["serve", "--bind"]),
            Err(UsageError::MissingValue("--bind"))
        );
        assert_eq!(
            invocation(&["serve", "--poll-interval", "25"]).command,
            Command::Serve {
                bind: BIND_DEFAULT.to_owned(),
                poll_interval: Duration::from_millis(25),
                no_watch: false
            }
        );
        for value in ["0", "later", "1.5"] {
            assert!(parse_words(&["serve", "--poll-interval", value]).is_err());
        }
        assert!(matches!(
            invocation(&["serve", "--no-watch"]).command,
            Command::Serve { no_watch: true, .. }
        ));
        assert!(parse_words(&["pages", "--no-watch"]).is_err());
        assert!(matches!(
            parse_words(&["pages", "--poll-interval", "25"]),
            Err(UsageError::CommandNotForOption {
                option: "--poll-interval",
                ..
            })
        ));
    }

    #[test]
    fn serve_takes_repeatable_api_tokens_and_refuses_them_elsewhere() {
        assert_eq!(
            invocation(&["serve", "--api-token", "alpha", "--api-token=beta"]).api_tokens,
            vec!["alpha".to_owned(), "beta".to_owned()]
        );
        // A token is one run of non-whitespace characters, so a value that could never be the
        // token a request presents is refused now instead of leaving a server that always says no.
        for value in ["a b", "a\tb"] {
            let error = parse_words(&["serve", "--api-token", value])
                .expect_err("a whitespace token is refused");
            assert_eq!(
                error,
                UsageError::InvalidValue {
                    option: "--api-token",
                    value: "<redacted>".to_owned(),
                    expected: "a token with no whitespace"
                }
            );
            assert!(
                !error.to_string().contains(value),
                "the refusal never echoes the credential (ADR-0018): {error}"
            );
        }
        // An empty value never reaches the option's own rule: `take_value` names it as a missing
        // value for every option that takes one.
        for missing in [
            parse_words(&["serve", "--api-token", ""]),
            parse_words(&["serve", "--api-token="]),
        ] {
            assert_eq!(missing, Err(UsageError::MissingValue("--api-token")));
        }
        for command in ["info", "pages"] {
            assert_eq!(
                parse_words(&[command, "--api-token", "alpha"]),
                Err(UsageError::CommandNotForOption {
                    option: "--api-token",
                    command: command.to_owned()
                })
            );
        }
    }

    #[test]
    fn serve_refuses_the_freshness_flags_instead_of_ignoring_them() {
        // `serve` scans at startup and polls in the background, and every request names its own
        // policy, so neither flag
        // describes work this command could skip. Ignoring them would let a caller believe the
        // startup scan ran a content check it never ran.
        for option in ["--verify-content", "--stale"] {
            let error = parse_words(&[option, "serve"])
                .expect_err("serve has no freshness policy to choose");
            assert_eq!(
                error,
                UsageError::OptionHasNoEffect {
                    option,
                    command: "serve",
                    reason: SERVE_CHOOSES_PER_REQUEST
                }
            );
            let message = error.to_string();
            assert!(message.contains(option), "{message}");
            assert!(message.contains("every request names"), "{message}");
        }
    }
}
