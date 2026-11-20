use crate::domain::{BlockId, QueryError};
use crate::index::IndexError;
use crate::source::{LockError, SourceError};

/// Failure of a graph operation.
#[derive(Debug)]
pub enum EngineError {
    /// A graph file could not be opened, listed, read, or decoded.
    Source(SourceError),
    /// The derived index could not be opened or written.
    Index(IndexError),
    /// Recovery of a pending cross-page move could not take the graph write lock, so the open
    /// cannot tell whether the files are mid-move and refuses rather than answering over them.
    ///
    /// A pending [`MoveIntent`](crate::index::MoveIntent) means the next open must reconcile the
    /// move before anything else reads the files, and reconciliation writes, so it serialises on
    /// the same lock every write takes. The budget is the caller's own
    /// [`crate::engine::OpenOptions::write_lock_wait`]; a lock held longer than that means another writer is
    /// mid-move or stuck, and retrying the open is safe because nothing was changed.
    Lock(LockError),
    /// The engine cannot vouch that the index matches the files, so a read that refuses stale data
    /// answers with this error instead of possibly outdated rows.
    ///
    /// The reason is the failure of the scan that did not complete. That failure may have happened
    /// after the index was left behind the files, or before the scan observed them at all, so the
    /// condition means "unverified", not "known to be outdated". A successful scan, which
    /// [`crate::engine::Freshness::CheckNow`] performs, clears it; nothing else does.
    StaleIndex {
        /// Why the last scan did not complete.
        reason: String,
    },
    /// A per-path refresh was refused because `logseq/config.edn` no longer matches the one the
    /// index was projected under.
    ///
    /// The configuration decides which page a file denotes: it decodes a file name into a page name
    /// and renders a journal's title from its date. A changed configuration therefore renames pages
    /// without touching a file, and a refresh that projected the given paths would write them under
    /// names the rest of the index does not use. The refusal leaves every row as it was; the caller
    /// falls back to a full scan ([`crate::engine::Freshness::CheckNow`]), which voids and re-derives every page
    /// under the new configuration.
    ConfigurationChanged,
    /// More than one block states the durable id, so this read cannot answer which one it names.
    ///
    /// A durable id names one block (ADR-0005), but a file may state the same `id::` twice — a
    /// hand edit, a copy-paste that kept the identity line, two notes merged. The projection records
    /// that as it is, because refusing the page would make one bad line hide a whole file. A read is
    /// a different matter: it answers about the index only, cannot look at the file to tell a real
    /// duplicate from a stale row, and answering one of the candidates would hand back a block the
    /// caller could not tell from the one it asked for.
    ///
    /// So the read refuses. `pages` names every page involved, sorted and deduplicated, which is what
    /// a person needs to go find the duplicate; a single element means both blocks are on that one
    /// page.
    AmbiguousBlockId {
        /// The id that was asked for.
        id: BlockId,
        /// Distinct page names stating it, sorted.
        pages: Vec<String>,
    },
    /// A query's text is not one this build can answer.
    ///
    /// The query grammar is the domain's ([`crate::domain::QueryExpr`]), and its failures
    /// are its own type ([`QueryError`]); this variant is only how they
    /// reach a caller that asked the engine. A query that matched nothing is a successful read with
    /// an empty result, never this error.
    Query(QueryError),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(error) => write!(formatter, "{error}"),
            Self::Index(error) => write!(formatter, "{error}"),
            Self::Query(error) => write!(formatter, "{error}"),
            Self::Lock(error) => write!(
                formatter,
                "a pending cross-page move needs the graph write lock to recover, and it could \
                 not be taken: {error}; nothing was changed, so opening the graph again is safe"
            ),
            Self::StaleIndex { reason } => write!(
                formatter,
                "the index could not be verified against the graph files, so this read is refused: \
                 {reason}"
            ),
            Self::ConfigurationChanged => write!(
                formatter,
                "logseq/config.edn changed since the index was projected, so refreshing these paths \
                 would write them under names the rest of the index does not use; scan the whole \
                 graph instead, which re-derives every page under the current configuration"
            ),
            Self::AmbiguousBlockId { id, pages } => write!(
                formatter,
                "the id {id} is stated by more than one block, so this read cannot say which block \
                 it names; the pages involved are {} — remove the duplicate `id::` line to make the \
                 id name one block again",
                listed(pages)
            ),
        }
    }
}

/// The page names of an ambiguous durable id, quoted and joined into one readable list.
///
/// The index layer sorts and deduplicates the names; this only makes them legible, because the point
/// of the message is that a person can take one name from it and go find the duplicate.
pub(super) fn listed(pages: &[String]) -> String {
    pages
        .iter()
        .map(|page| format!("{page:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source(error) => Some(error),
            Self::Index(error) => Some(error),
            Self::Query(error) => Some(error),
            Self::Lock(error) => Some(error),
            Self::StaleIndex { .. } => None,
            Self::ConfigurationChanged => None,
            Self::AmbiguousBlockId { .. } => None,
        }
    }
}

impl From<SourceError> for EngineError {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}

impl From<LockError> for EngineError {
    fn from(error: LockError) -> Self {
        Self::Lock(error)
    }
}

impl From<IndexError> for EngineError {
    fn from(error: IndexError) -> Self {
        Self::Index(error)
    }
}

impl From<QueryError> for EngineError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}
