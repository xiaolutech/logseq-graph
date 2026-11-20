use crate::document::EditError;
use crate::domain::{BlockId, EphemeralId, PageSnapshot, PageVersion};
use crate::engine::EngineError;
#[cfg(doc)]
use crate::engine::Graph;
use crate::index::IndexError;
#[cfg(doc)]
use crate::index::MoveIntent;
use crate::source::{LockError, SourceError};
use std::fmt;
use std::path::{Path, PathBuf};

/// Why a write did not complete, and whether the file changed anyway.
///
/// The variants are ordered by how much the caller has to do about them: an error before the file
/// is touched leaves the graph as it was, while [`WriteError::IndexAfterWrite`] and
/// [`WriteError::ReReadAfterWrite`] mean the file already holds the new text and only the index is
/// behind. Those two are separate from the plain index and source errors on purpose: folding them
/// into [`WriteError::Index`] would tell the caller that nothing was written, and the caller cannot
/// recover from that belief.
#[derive(Debug)]
pub enum WriteError {
    /// A removal range names blocks on different pages; nothing was written.
    RangeAcrossPages,
    /// The clock or system random source could not provide a durable identity; nothing was written.
    IdentityGeneration {
        /// Why identity generation failed.
        reason: String,
    },
    /// Another writer held the graph write lock for the whole wait budget, so nothing was read and
    /// nothing was written.
    Lock(LockError),
    /// The scan [`Graph::update_block`] falls back to on a miss did not complete, so nothing was
    /// written.
    Engine(EngineError),
    /// A graph file could not be listed, read, or replaced. A failure before the commit leaves the
    /// graph untouched; [`SourceError::Committed`] reports a durability step that failed after the
    /// new file state was already visible, and [`WriteError::ReReadAfterWrite`] reports the failure
    /// after a replacement.
    Source(SourceError),
    /// The index could not be read. A failure of the page replacement itself is
    /// [`WriteError::IndexAfterWrite`] instead.
    Index(IndexError),
    /// The edit was refused, so the file was not touched.
    Edit(EditError),
    /// `page` names no file in this graph.
    ///
    /// A write never creates a page: creating one is a different operation with its own decisions
    /// (which directory, which name spelling, whether the name is a journal date), so a miss is an
    /// error rather than a silently created file.
    PageNotFound {
        /// The name this write asked for.
        page: String,
    },
    /// The requested page name has no file name that decodes back to it, so no file was created.
    ///
    /// Refusing is deliberate: a stem the encoder cannot prove would make a file that decodes to a
    /// different page, which no later read could tell apart from a page the caller never named.
    PageNameNotEncodable {
        /// The page name this create asked for.
        name: String,
        /// Why the encoder refused it.
        reason: String,
    },
    /// A page with this name already exists, so nothing was created.
    ///
    /// Names compare by [`page_name_key`](crate::domain::page_name_key), so `Foo` and `foo` are one
    /// page and a create of either refuses while the other exists. `path` is the file the existing
    /// page lives in, which is what tells the caller where the page it asked for already is.
    PageExists {
        /// The existing page's name, as the index or the file names it.
        page: String,
        /// The file that holds the existing page.
        path: PathBuf,
    },
    /// No file in the graph states `id:: <block_id>`, so there was nothing to update and no file
    /// was touched.
    ///
    /// This is a miss in the files, not only in the derived rows: the lookup that produced it was
    /// retried after a full scan, so by the time this variant is reported every file has been
    /// projected and none of them states the id. That is also why an id whose `id::` line was
    /// deleted externally reports [`WriteError::Edit`] instead: the row the index still held sent the
    /// edit to the right file, and the file then refused it.
    BlockNotFound {
        /// The id this write asked for.
        id: BlockId,
    },
    /// No stored row holds this ephemeral id, so the write refused before reading or writing a file.
    ///
    /// A miss here means the page holding the block was reprojected, the index was rebuilt, or the
    /// value was never minted — in every case the block this selector named no longer has that
    /// address, and the caller re-reads to get the one it has now (ADR-0012). The lookup is one
    /// equality on the unique `blocks.ephemeral_id` index, and there is deliberately no fallback: a
    /// durable miss may only mean an unprojected file states the id somewhere, so that path pays for
    /// one scan, while scanning every file here could not produce a row that holds this id.
    EphemeralNotFound {
        /// The ephemeral id this write asked for.
        id: EphemeralId,
    },
    /// More than one page states `id:: <block_id>`, so this write cannot tell which block the
    /// caller meant and nothing was read or written.
    ///
    /// A durable id names one block (ADR-0005), and a file that states it twice is refused by the
    /// document layer as [`EditError::AmbiguousBlockId`]. That answer is only available when both
    /// candidates live on one page, because a write reads exactly one file. When the belonging pages
    /// differ, one file cannot settle the question, and picking either would edit a page the caller
    /// may never have named — so the refusal happens before any file is read, and both files stay
    /// exactly as they were.
    AmbiguousBlockId {
        /// The id this write asked for.
        id: BlockId,
        /// Distinct page names stating it, sorted and deduplicated.
        pages: Vec<String>,
    },
    /// One side of a cross-page move was written and the other was not, or the move succeeded on
    /// disk but could not be projected, so the graph is mid-move.
    ///
    /// A move replaces two files, and it commits a [`MoveIntent`] before the first write, so the
    /// interrupted state is described and recoverable rather than guessed at: the next
    /// [`Graph::open`] finishes the move from the intent, or — when outside interference changed a
    /// file beyond recognition — projects both pages as they actually are. The index may be behind
    /// either file until that recovery runs, which is why this is an error and not a success.
    PartialMove {
        /// The page the block moves out of, as written.
        from: String,
        /// The file that holds the page the block moves out of.
        from_path: PathBuf,
        /// The page the block moves into, as written.
        to: String,
        /// The file that holds the page the block moves into.
        to_path: PathBuf,
        /// Why the move stopped short of its final index commit.
        cause: String,
    },
    /// The page's bytes are not the version the operation's positions were computed from, so nothing
    /// was applied and neither the file nor the index was touched.
    ///
    /// Two writes share this one refusal because it is one fact about the page. A
    /// [`PageCommit`](crate::domain::PageCommit) addresses blocks by child index paths against a
    /// stated `baseVersion` (ADR-0011), and an ephemeral block selector is a row's child-index
    /// path plus the content fingerprint of the bytes that row was projected from (ADR-0012); both
    /// are coordinates in one observation of one page and carry no identity of their own. Applying
    /// them to different bytes could edit a block the caller never named.
    ///
    /// The check is page-scoped and made against the bytes read under the write lock — not against
    /// the index revision, which another process can leave behind while the file changes. For a
    /// batch it is the caller's stated version that is compared; for an ephemeral selector it is the
    /// fingerprint the projection recorded, rendered by the same token a version is.
    ///
    /// `current` is a whole snapshot of the page these bytes describe, parsed from the bytes that
    /// were just read, so the caller can take the block paths it needs from it and retry against
    /// [`current.version()`](crate::domain::PageSnapshot::version) instead of re-reading the page
    /// itself. It is boxed because one snapshot is much larger than every other payload of this
    /// enum, and every error would otherwise pay its size.
    StaleVersion {
        /// Name of the page the operation named, as the graph stores it.
        page: String,
        /// Version the commit's paths were computed from.
        base_version: PageVersion,
        /// The page as the file holds it now, which is what a retry has to be based on.
        current: Box<PageSnapshot>,
    },
    /// One operation of a [`PageCommit`](crate::domain::PageCommit) was refused, so the whole commit
    /// was refused: no prefix of it was applied and the file is byte-identical.
    ///
    /// Operations run in order against a candidate document, which exists only in memory until the
    /// last of them succeeds; a failure at `index` throws that candidate away, so the operations
    /// before it leave nothing behind and the later ones are never even resolved.
    ///
    /// `operation` is the variant name of the refused operation — `"SplitBlock"`, `"MoveBlock"`,
    /// and so on — because `cause` alone cannot say which step of a batch a miss belongs to.
    Operation {
        /// Name of the page the commit named, as the graph stores it.
        page: String,
        /// Zero-based position of the refused operation in the commit's operations.
        index: usize,
        /// Variant name of the refused operation.
        operation: &'static str,
        /// Why the edit refused it.
        cause: EditError,
    },
    /// The file was replaced, but the page could not be re-observed, so the index was not updated.
    ///
    /// The usual cause is another process renaming or removing the file inside the window between
    /// the replacement and the re-observation. The next scan reconciles it: the recorded baseline no
    /// longer matches the file, so the page is re-projected or its rows are removed.
    ReReadAfterWrite {
        /// The page as it was named when the file was replaced.
        page: String,
        /// The file that holds the new text.
        path: PathBuf,
    },
    /// The file was replaced and is on disk, but the index replacement failed.
    ///
    /// This is the one state a write can leave behind, and it is reported rather than repaired. The
    /// file is the source of truth and is never rolled back (ADR-0004), so undoing the write to make
    /// the two sides agree again would discard content the caller asked to keep. Recovery is the
    /// next scan: the file's stat and fingerprint no longer match the stored baseline, so the page is
    /// read and re-projected, and the index catches up on its own.
    ///
    /// The engine's test-only fault seam asserts this state, including that the new bytes survive,
    /// the old projection stays complete, and the next scan converges.
    IndexAfterWrite {
        /// The page whose file was replaced.
        page: String,
        /// The file that holds the new text.
        path: PathBuf,
        /// Why the index replacement failed.
        cause: IndexError,
    },
}

/// Whether a failed write may already have changed source bytes.
///
/// This is the library's answer to the one question a caller must not infer from the variant
/// spelling: a failure can happen before the file commit, when the graph is exactly as it was, or
/// after it, when the bytes are on disk and only observation or projection failed. CLI exit codes
/// and RPC `write_state` both derive from this value, so neither adapter re-decides the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteState {
    /// The failure happened before any source bytes changed.
    Unwritten,
    /// Source bytes may already hold the new text and must be treated as written.
    FileChanged,
}

impl WriteError {
    /// Whether this failure happened before or after the file commit.
    ///
    /// The exhaustive match is deliberate: a new error variant has to choose a state before this
    /// crate compiles, which is what keeps the CLI and RPC from silently classifying it by hand.
    pub fn write_state(&self) -> WriteState {
        match self {
            Self::PartialMove { .. }
            | Self::ReReadAfterWrite { .. }
            | Self::IndexAfterWrite { .. } => WriteState::FileChanged,
            Self::Source(SourceError::Committed { .. }) => WriteState::FileChanged,
            Self::IdentityGeneration { .. }
            | Self::Lock(_)
            | Self::Engine(_)
            | Self::Source(_)
            | Self::Index(_)
            | Self::Edit(_)
            | Self::PageNotFound { .. }
            | Self::PageNameNotEncodable { .. }
            | Self::PageExists { .. }
            | Self::BlockNotFound { .. }
            | Self::EphemeralNotFound { .. }
            | Self::AmbiguousBlockId { .. }
            | Self::RangeAcrossPages
            | Self::StaleVersion { .. }
            | Self::Operation { .. } => WriteState::Unwritten,
        }
    }
}

impl fmt::Display for WriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RangeAcrossPages => formatter.write_str("range endpoints are on different pages"),
            Self::IdentityGeneration { reason } => write!(formatter, "could not mint a durable block id: {reason}"),
            Self::Lock(error) => write!(formatter, "{error}"),
            Self::Engine(error) => write!(formatter, "{error}"),
            Self::Source(error) => write!(formatter, "{error}"),
            Self::Index(error) => write!(formatter, "{error}"),
            Self::Edit(error) => write!(formatter, "{error}"),
            Self::PageNotFound { page } => write!(
                formatter,
                "the page {page:?} has no file in this graph, and a write never creates one"
            ),
            Self::PageNameNotEncodable { name, reason } => write!(
                formatter,
                "the page name {name:?} cannot be written as a file name: {reason}"
            ),
            Self::PageExists { page, path } => write!(
                formatter,
                "the page {page:?} already exists, in {}, so nothing was created",
                path.display()
            ),
            Self::BlockNotFound { id } => write!(
                formatter,
                "no block states the id {id}, so there is nothing to update"
            ),
            Self::EphemeralNotFound { id } => write!(
                formatter,
                "no stored block holds the ephemeral id {id}, so it names nothing any more; the \
                 page it was read from was reprojected or the index was rebuilt, and re-reading \
                 the page answers the id it holds now"
            ),
            Self::AmbiguousBlockId { id, pages } => write!(
                formatter,
                "the id {id} is stated by more than one block, on more than one page ({}), so this \
                 write cannot tell which block was meant; each page has to be fixed by hand, since \
                 a durable id has to name one block",
                crate::engine::listed(pages)
            ),
            Self::PartialMove {
                from,
                from_path,
                to,
                to_path,
                cause,
            } => write!(
                formatter,
                "the cross-page move from the page {from:?} ({}) to the page {to:?} ({}) stopped \
                 after some files were written: {cause}; the move intent is kept, and the next open \
                 of this graph completes or reconciles it",
                from_path.display(),
                to_path.display()
            ),
            Self::StaleVersion {
                page,
                base_version,
                current,
            } => write!(
                formatter,
                "the page {page:?} is at version {}, not at the base version {base_version} the \
                 operation's positions were computed from, so nothing was applied and nothing was \
                 written; re-read the page and retry against its current version",
                current.version()
            ),
            Self::Operation {
                page,
                index,
                operation,
                cause,
            } => write!(
                formatter,
                "operation {index} ({operation}) of the commit against the page {page:?} was \
                 refused, so the whole commit was refused and nothing was written: {cause}"
            ),
            Self::ReReadAfterWrite { page, path } => write!(
                formatter,
                "the file {} of the page {page:?} was replaced but the page can no longer be \
                 observed, so the index was not updated; the next scan reconciles it",
                path.display()
            ),
            Self::IndexAfterWrite { page, path, cause } => write!(
                formatter,
                "the file {} of the page {page:?} was replaced but the index replacement failed, \
                 so the index is behind the file: {cause}; the next scan repairs it from the file \
                 baseline",
                path.display()
            ),
        }
    }
}

impl std::error::Error for WriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Lock(error) => Some(error),
            Self::Engine(error) => Some(error),
            Self::Source(error) => Some(error),
            Self::Index(error) => Some(error),
            Self::Edit(error) => Some(error),
            Self::IdentityGeneration { .. }
            | Self::PageNotFound { .. }
            | Self::PageNameNotEncodable { .. }
            | Self::PageExists { .. }
            | Self::BlockNotFound { .. }
            | Self::EphemeralNotFound { .. }
            | Self::AmbiguousBlockId { .. }
            | Self::RangeAcrossPages
            | Self::StaleVersion { .. }
            | Self::PartialMove { .. }
            | Self::ReReadAfterWrite { .. } => None,
            Self::IndexAfterWrite { cause, .. } => Some(cause),
            Self::Operation { cause, .. } => Some(cause),
        }
    }
}

/// Turn an answer read that failed after the commit into the file-changed error.
///
/// Every caller that reads metadata, a selector, or a view back after `commit_page_edit` has
/// already replaced the file. Keeping that mapping here is what stops a plain index failure from
/// reaching a caller as "nothing was written".
pub(super) fn answer_after_write(page: &str, path: &Path, error: IndexError) -> WriteError {
    WriteError::IndexAfterWrite {
        page: page.to_owned(),
        path: path.to_owned(),
        cause: error,
    }
}

impl From<LockError> for WriteError {
    fn from(error: LockError) -> Self {
        Self::Lock(error)
    }
}

impl From<EngineError> for WriteError {
    fn from(error: EngineError) -> Self {
        match error {
            EngineError::Lock(error) => Self::Lock(error),
            other => Self::Engine(other),
        }
    }
}

impl From<SourceError> for WriteError {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}

impl From<IndexError> for WriteError {
    fn from(error: IndexError) -> Self {
        Self::Index(error)
    }
}

impl From<EditError> for WriteError {
    fn from(error: EditError) -> Self {
        Self::Edit(error)
    }
}

/// Whether an index failure is SQLite refusing a uniqueness constraint.
///
/// A create is the one write that can meet such a refusal on a fresh page: the index may hold a row
/// for the page name or the file path that its own lookup did not find, and the uniqueness
/// constraint is what settles the question. The caller then removes the file it just created and
/// reports the page as existing instead of leaving a file no row describes.
///
/// The extended code is what distinguishes this from every other constraint failure. A CHECK
/// failure is not another writer claiming the name — it means this call passed a pair of values
/// that cannot both hold, such as a journal row without its day — and the recovery above is written
/// for the uniqueness meaning only: it deletes the file, which would turn a caller's own mistake
/// into a lost page with an unrelated `PageExists` report. A CHECK failure therefore falls through
/// to [`WriteError::IndexAfterWrite`], which keeps the file and names the cause.
pub(super) fn is_unique_violation(error: &IndexError) -> bool {
    matches!(
        error,
        IndexError::Sqlite(rusqlite::Error::SqliteFailure(failure, _))
            if failure.code == rusqlite::ErrorCode::ConstraintViolation
                && failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::ffi;

    /// The failure SQLite reports for one constraint, as a write would receive it.
    fn constraint_failure(extended_code: i32) -> IndexError {
        IndexError::Sqlite(rusqlite::Error::SqliteFailure(
            ffi::Error {
                code: rusqlite::ErrorCode::ConstraintViolation,
                extended_code,
            },
            None,
        ))
    }

    #[test]
    fn only_a_uniqueness_refusal_means_another_writer_claimed_the_name() {
        // The recovery this predicate guards deletes the file the create just wrote, which is the
        // right answer for a uniqueness refusal and the wrong one for anything else. A CHECK
        // failure means the caller passed a pair of values that cannot both hold, so the file has
        // to survive and the error has to name the cause.
        assert!(is_unique_violation(&constraint_failure(
            ffi::SQLITE_CONSTRAINT_UNIQUE
        )));
        assert!(!is_unique_violation(&constraint_failure(
            ffi::SQLITE_CONSTRAINT_CHECK
        )));
        assert!(!is_unique_violation(&IndexError::Sqlite(
            rusqlite::Error::SqliteFailure(
                ffi::Error {
                    code: rusqlite::ErrorCode::DatabaseBusy,
                    extended_code: 0,
                },
                None,
            )
        )));
    }
}
