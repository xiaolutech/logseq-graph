use std::time::Duration;

/// Number of search hits printed when `--limit` is absent.
pub(crate) const DEFAULT_SEARCH_LIMIT: u32 = 20;

/// Number of query matches printed when `--limit` is absent.
///
/// The same default as `search`, for the same reason: a listing is for reading, and a caller that
/// wants everything says so. A query is the one read whose cost grows with the complement sets
/// `(not …)` builds, so an answer that would be cut states that it was cut.
pub(crate) const DEFAULT_QUERY_LIMIT: u32 = 20;

/// The address `serve` binds when `--bind` is absent: loopback only, so the read surface serves
/// the machine it runs on and nothing outside it.
pub(crate) const BIND_DEFAULT: &str = "127.0.0.1:8765";

/// The external-change poll period when the command line names none.
pub(crate) const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(1_000);
