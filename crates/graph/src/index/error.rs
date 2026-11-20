use std::io;
use std::path::PathBuf;

use super::schema;
use crate::domain::{BlockMarker, PageKind};

/// Failure of an index operation.
#[derive(Debug)]
pub enum IndexError {
    /// SQLite rejected a statement or a connection could not be opened.
    Sqlite(rusqlite::Error),
    /// The stored schema or parser version does not match this build.
    ///
    /// The derived state is disposable, so it is rebuilt from the files instead of migrated.
    /// [`Index::open_at`](super::Index::open_at) does that by deleting the database file, which makes this variant the
    /// signal between schema initialization and the disk open rather than something a disk-backed
    /// caller has to handle.
    Incompatible {
        /// Schema version found in the database.
        stored_schema: i64,
        /// Parser version found in the database.
        stored_parser: i64,
    },
    /// The page nests deeper than [`MAX_BLOCK_DEPTH`](super::MAX_BLOCK_DEPTH), so it was not projected.
    TooDeep {
        /// Depth the projection reached.
        depth: u32,
        /// Deepest supported depth.
        limit: u32,
    },
    /// A stored row holds a value this build cannot interpret.
    CorruptRow {
        /// Table or column that holds the unexpected value.
        column: &'static str,
        /// The value as read from the database.
        value: String,
    },
    /// The database file or its containing directory could not be created or removed.
    Io {
        /// Path the operation addressed.
        path: PathBuf,
        /// Underlying filesystem error.
        source: io::Error,
    },
    /// The database file belongs to another application, so it was neither used nor deleted.
    ///
    /// The file's `application_id` is not zero and not this project's marker, so it holds someone
    /// else's data. Using it would write this project's tables into it, and deleting it would
    /// destroy content this project does not own.
    NotAnIndex {
        /// Path that was opened.
        path: PathBuf,
        /// `application_id` recorded in that file.
        found: i64,
    },
    /// A graph-relative path is not UTF-8, so it cannot key a file baseline.
    NonUtf8Path {
        /// Path that could not be encoded.
        path: PathBuf,
    },
    /// The operating system's random source failed, so no ephemeral block id could be minted.
    ///
    /// Ephemeral identity is random precisely so that nothing can derive or re-derive it, which is
    /// what makes an id that no row holds name nothing at all (ADR-0012). A system that cannot
    /// supply random bytes therefore cannot mint one, and inventing a value from position or
    /// revision instead would reintroduce the aliasing this identity exists to prevent.
    Entropy {
        /// Why the system source refused.
        reason: String,
    },
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "index storage failure: {error}"),
            Self::Incompatible {
                stored_schema,
                stored_parser,
            } => write!(
                formatter,
                "index holds schema version {stored_schema} and parser version {stored_parser}, \
                 but this build needs schema version {} and parser version {}",
                schema::SCHEMA_VERSION,
                schema::PARSER_VERSION
            ),
            Self::TooDeep { depth, limit } => write!(
                formatter,
                "page nests to depth {depth}, above the supported limit of {limit}"
            ),
            Self::CorruptRow { column, value } => {
                write!(
                    formatter,
                    "index row {column} holds an unexpected value: {value}"
                )
            }
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "cannot manage index file {}: {source}",
                    path.display()
                )
            }
            Self::NotAnIndex { path, found } => write!(
                formatter,
                "{} is a SQLite database of another application (application_id {found}), \
                 so it is neither used nor deleted",
                path.display()
            ),
            Self::NonUtf8Path { path } => write!(
                formatter,
                "{} is not valid UTF-8, so it cannot be recorded as a file baseline",
                path.display()
            ),
            Self::Entropy { reason } => write!(
                formatter,
                "no ephemeral block id could be minted, because the system random source failed: \
                 {reason}"
            ),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for IndexError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// The page kind a stored spelling denotes, or a corrupt-row error naming `column`.
///
/// `pages.kind` is `CHECK`ed to the spellings [`PageKind::as_str`] writes, so a value this refuses
/// means the row was written by a build whose vocabulary differed — the row cannot be read back into
/// a meaning, and the caller gets the column rather than a guess.
pub(crate) fn decode_page_kind(stored: &str, column: &'static str) -> Result<PageKind, IndexError> {
    PageKind::parse(stored).ok_or_else(|| IndexError::CorruptRow {
        column,
        value: stored.to_owned(),
    })
}

/// Decode the structural marker pair the block table's CHECK constrains.
pub(crate) fn decode_marker(kind: &str, level: Option<u32>) -> Result<BlockMarker, IndexError> {
    match (kind, level) {
        ("bullet", None) => Ok(BlockMarker::Bullet),
        ("heading", Some(level @ 1..=6)) => Ok(BlockMarker::Heading { level: level as u8 }),
        _ => Err(IndexError::CorruptRow {
            column: "blocks.marker_kind",
            value: format!("{kind} with heading level {level:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_kind_this_build_cannot_read_back_names_the_column() {
        // The CHECK constraints keep such a row from being written, so this is the guard for a
        // database written by a build whose vocabulary differed: the answer names the column instead
        // of guessing a meaning.
        assert_eq!(
            decode_page_kind("journal", "pages.kind").expect("a spelling this build writes"),
            PageKind::Journal
        );
        assert!(matches!(
            decode_page_kind("daily", "pages.kind"),
            Err(IndexError::CorruptRow {
                column: "pages.kind",
                ..
            })
        ));
    }
}
