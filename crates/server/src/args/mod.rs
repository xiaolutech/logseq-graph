//! Argument parsing for the command line.
//!
//! Parsing is a pure function over the arguments: it reads no environment variable and touches no
//! filesystem, so every rule here is testable without a process. The graph root may be absent from
//! the arguments, because it also comes from the environment; resolving that is [`crate::run`]'s
//! job.
//!
//! Anything the grammar does not name is a [`UsageError`], never a panic: a wrong command line is a
//! normal answer about the caller's input, not a defect of the program.

mod arity;
mod command;
mod defaults;
mod error;
mod help;
mod parse;
mod values;

#[cfg(test)]
mod test_support;

pub(crate) use command::{Command, Invocation, MoveDirection, Parsed};
pub(crate) use defaults::{
    BIND_DEFAULT, DEFAULT_POLL_INTERVAL, DEFAULT_QUERY_LIMIT, DEFAULT_SEARCH_LIMIT,
};
pub(crate) use error::UsageError;
pub(crate) use help::{HELP, USAGE};
pub(crate) use parse::parse;
pub(crate) use values::{parse_day, parse_selector, DURABLE_PREFIX, EPHEMERAL_PREFIX};
