use super::{parse, Invocation, Parsed, UsageError};

pub(super) const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

pub(super) fn parse_words(words: &[&str]) -> Result<Parsed, UsageError> {
    parse(words.iter().map(|word| (*word).to_owned()))
}

pub(super) fn invocation(words: &[&str]) -> Invocation {
    match parse_words(words).expect("the arguments parse") {
        Parsed::Run(invocation) => invocation,
        Parsed::Help => panic!("the arguments ask for help"),
    }
}
