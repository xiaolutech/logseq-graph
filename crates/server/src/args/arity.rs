use super::UsageError;

/// The value of an option, from `--option=value` or from the next argument.
pub(super) fn take_value(
    args: &mut impl Iterator<Item = String>,
    inline: Option<String>,
    option: &'static str,
) -> Result<String, UsageError> {
    let value = match inline {
        Some(value) => value,
        None => args.next().ok_or(UsageError::MissingValue(option))?,
    };
    if value.is_empty() {
        return Err(UsageError::MissingValue(option));
    }
    Ok(value)
}

/// Reject a `--limit` that the command has no use for.
pub(super) fn reject_limit(command: &'static str, limit: Option<u32>) -> Result<(), UsageError> {
    match limit {
        Some(_) => Err(UsageError::OptionNotForCommand {
            option: "--limit",
            command,
        }),
        None => Ok(()),
    }
}

/// The single argument a command takes.
pub(super) fn one_argument(
    command: &'static str,
    argument: &'static str,
    arguments: &[String],
) -> Result<String, UsageError> {
    match arguments {
        [] => Err(UsageError::MissingArgument { command, argument }),
        [value] => Ok(value.clone()),
        [_, extra, ..] => Err(UsageError::UnexpectedArgument {
            command,
            argument: extra.clone(),
        }),
    }
}

/// The two arguments a command takes, in order.
pub(super) fn two_arguments(
    command: &'static str,
    first: &'static str,
    second: &'static str,
    arguments: &[String],
) -> Result<(String, String), UsageError> {
    match arguments {
        [] => Err(UsageError::MissingArgument {
            command,
            argument: first,
        }),
        [_] => Err(UsageError::MissingArgument {
            command,
            argument: second,
        }),
        [one, two] => Ok((one.clone(), two.clone())),
        [_, _, extra, ..] => Err(UsageError::UnexpectedArgument {
            command,
            argument: extra.clone(),
        }),
    }
}

/// The three arguments a command takes, in order.
pub(super) fn three_arguments(
    command: &'static str,
    first: &'static str,
    second: &'static str,
    third: &'static str,
    arguments: &[String],
) -> Result<(String, String, String), UsageError> {
    match arguments {
        [] => Err(UsageError::MissingArgument {
            command,
            argument: first,
        }),
        [_] => Err(UsageError::MissingArgument {
            command,
            argument: second,
        }),
        [_, _] => Err(UsageError::MissingArgument {
            command,
            argument: third,
        }),
        [one, two, three] => Ok((one.clone(), two.clone(), three.clone())),
        [_, _, _, extra, ..] => Err(UsageError::UnexpectedArgument {
            command,
            argument: extra.clone(),
        }),
    }
}

/// Reject any argument for a command that takes none.
pub(super) fn expect_no_arguments(
    command: &'static str,
    arguments: &[String],
) -> Result<(), UsageError> {
    match arguments.first() {
        Some(extra) => Err(UsageError::UnexpectedArgument {
            command,
            argument: extra.clone(),
        }),
        None => Ok(()),
    }
}
