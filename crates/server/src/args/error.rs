/// Why a command line names no invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UsageError {
    /// An option this grammar does not have.
    UnknownOption(String),
    /// An option that needs a value was the last argument.
    MissingValue(&'static str),
    /// A value the option cannot take.
    InvalidValue {
        option: &'static str,
        value: String,
        expected: &'static str,
    },
    /// A commit's operations are not valid JSON input.
    InvalidOperations(String),
    /// No command at all.
    MissingCommand,
    /// A command this grammar does not have.
    UnknownCommand(String),
    /// A command was given without the argument it needs.
    MissingArgument {
        command: &'static str,
        argument: &'static str,
    },
    /// A command was given more arguments than it takes.
    UnexpectedArgument {
        command: &'static str,
        argument: String,
    },
    /// An option that belongs to one command was given to another.
    OptionNotForCommand {
        option: &'static str,
        command: &'static str,
    },
    /// `--bind` was given to a command that does not listen.
    ///
    /// This mirrors [`UsageError::OptionNotForCommand`] with an owned command name, because the
    /// option is parsed before the command and the command word is not a literal.
    CommandNotForOption {
        option: &'static str,
        command: String,
    },
    /// An option that the command cannot honour, with the reason it cannot.
    ///
    /// This is separate from [`UsageError::OptionNotForCommand`] because silently ignoring the
    /// option would be worse than refusing it: a caller that passed `--verify-content` would believe
    /// it bought a stronger guarantee that the command never offered.
    OptionHasNoEffect {
        option: &'static str,
        command: &'static str,
        reason: &'static str,
    },
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownOption(option) => write!(formatter, "unknown option: {option}"),
            Self::MissingValue(option) => write!(formatter, "{option} needs a value"),
            Self::InvalidValue {
                option,
                value,
                expected,
            } => write!(formatter, "{option} needs {expected}, not {value:?}"),
            Self::InvalidOperations(reason) => {
                write!(formatter, "commit needs a JSON operations array: {reason}")
            }
            Self::MissingCommand => write!(formatter, "no command given"),
            Self::UnknownCommand(command) => write!(formatter, "unknown command: {command}"),
            Self::MissingArgument { command, argument } => {
                write!(formatter, "{command} needs {argument}")
            }
            Self::UnexpectedArgument { command, argument } => {
                write!(formatter, "{command} takes no argument {argument:?}")
            }
            Self::OptionNotForCommand { option, command } => {
                write!(formatter, "{option} does not apply to {command}")
            }
            Self::CommandNotForOption { option, command } => {
                write!(formatter, "{option} does not apply to {command}")
            }
            Self::OptionHasNoEffect {
                option,
                command,
                reason,
            } => write!(formatter, "{option} has no effect on {command}: {reason}"),
        }
    }
}

impl std::error::Error for UsageError {}

#[cfg(test)]
mod tests {
    use super::super::UsageError;

    #[test]
    fn a_usage_error_reads_as_one_line() {
        for error in [
            UsageError::UnknownOption("--nope".to_owned()),
            UsageError::MissingCommand,
            UsageError::MissingValue("--index"),
        ] {
            let message = error.to_string();
            assert!(!message.contains('\n'), "one line, not {message:?}");
        }
    }
}
