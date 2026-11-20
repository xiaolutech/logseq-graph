//! File baselines: what the index last observed about each source file.
//!
//! A baseline answers one question per file: has this file changed since its content was projected?
//! It holds the observation of the file (size and modification time) and a fingerprint of the exact
//! text that was parsed, keyed by the graph-relative path. It deliberately holds nothing else: which
//! kind of page the file holds is a property of the page and lives on the page row, and the path
//! itself already says which directory the file is in. The observation is never later than the text
//! it describes, so a baseline can be too old but never too new.
//!
//! Files remain authoritative: a baseline is derived state, deleted with the database, and rebuilt
//! by reading the files again (ADR-0001, ADR-0002). It exists only to avoid re-reading and
//! re-parsing files that cannot have changed, so a wrong answer here costs a stale page, never lost
//! content.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use crate::source::FileMetadata;

use super::IndexError;

/// What the index last observed about one source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileBaseline {
    metadata: FileMetadata,
    content_hash: u64,
}

impl FileBaseline {
    /// Baseline built from an observation of a file and the fingerprint of its text.
    ///
    /// The observation must not be later than the read that produced the text; in practice it is the
    /// snapshot discovery took before that read. An observation that is too old only costs one
    /// re-projection on the next start, while one that is too new can describe a write the index
    /// never projected and make the next start skip that write, leaving the page stale until the
    /// file moves again.
    pub fn new(metadata: FileMetadata, content_hash: u64) -> Self {
        Self {
            metadata,
            content_hash,
        }
    }

    /// Observation of the file, taken no later than the text whose fingerprint is stored here.
    pub fn metadata(&self) -> FileMetadata {
        self.metadata
    }

    /// Fingerprint of the text that was parsed into the index.
    pub fn content_hash(&self) -> u64 {
        self.content_hash
    }
}

/// Baseline recorded for `relative_path`, or `None` when the file was never projected.
pub(super) fn read(
    conn: &Connection,
    relative_path: &Path,
) -> Result<Option<FileBaseline>, IndexError> {
    let key = path_key(relative_path)?;
    let row = conn
        .query_row(
            "SELECT size, modified_ns, content_hash
               FROM file_baseline WHERE relative_path = ?1",
            [&key],
            |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;

    let Some((size, modified_ns, content_hash)) = row else {
        return Ok(None);
    };
    Ok(Some(FileBaseline {
        metadata: FileMetadata::from_stored(size, modified_from_ns(modified_ns)),
        content_hash: content_hash as u64,
    }))
}

/// Insert or replace the baseline of `relative_path`.
pub(super) fn write(
    conn: &Connection,
    relative_path: &Path,
    baseline: FileBaseline,
) -> Result<(), IndexError> {
    let key = path_key(relative_path)?;
    conn.execute(
        "INSERT INTO file_baseline (relative_path, size, modified_ns, content_hash)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (relative_path) DO UPDATE SET
             size = excluded.size,
             modified_ns = excluded.modified_ns,
             content_hash = excluded.content_hash",
        params![
            key,
            baseline.metadata.size(),
            modified_to_ns(baseline.metadata.modified()),
            // SQLite integers are signed, so the fingerprint is stored as its two's-complement bit
            // pattern. The cast is lossless in both directions.
            baseline.content_hash as i64,
        ],
    )?;
    Ok(())
}

/// Every recorded baseline path, ordered by path.
///
/// A scan compares this list with the files discovery returned, so a file that was deleted or
/// renamed loses its baseline instead of keeping one forever.
pub(super) fn paths(conn: &Connection) -> Result<Vec<PathBuf>, IndexError> {
    let mut stmt =
        conn.prepare("SELECT relative_path FROM file_baseline ORDER BY relative_path")?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        // Stored keys are always the text of a path this crate produced (see `path_key`), so the
        // conversion back is exact on every platform.
        out.push(PathBuf::from(row.get::<_, String>(0)?));
    }
    Ok(out)
}

/// Remove the baseline of `relative_path`.
///
/// A page row references the baseline of the file it was projected from, so this fails while one
/// still points at this path: the caller removes the page first. The order is not a convention here
/// but a `NO ACTION` foreign key, because a page row without its file's observation cannot say where
/// it came from.
pub(super) fn delete(conn: &Connection, relative_path: &Path) -> Result<(), IndexError> {
    conn.execute(
        "DELETE FROM file_baseline WHERE relative_path = ?1",
        [path_key(relative_path)?],
    )?;
    Ok(())
}

/// Key of a baseline row: the graph-relative path as text.
///
/// Discovery only returns UTF-8 file names, so this cannot fail for a file the source layer
/// produced; a caller-supplied path that is not UTF-8 is rejected instead of being stored lossily.
pub(super) fn path_key(relative_path: &Path) -> Result<String, IndexError> {
    relative_path
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| IndexError::NonUtf8Path {
            path: relative_path.to_owned(),
        })
}

/// Store a modification time as signed nanoseconds since the Unix epoch.
///
/// A missing time stays `NULL`, which keeps "the platform reported no modification time" distinct
/// from any real time. A time before the epoch becomes a negative value instead of being dropped.
fn modified_to_ns(modified: Option<SystemTime>) -> Option<i64> {
    modified.map(|time| match time.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_nanos() as i64,
        Err(before) => -(before.duration().as_nanos() as i64),
    })
}

fn modified_from_ns(nanos: Option<i64>) -> Option<SystemTime> {
    let nanos = nanos?;
    // Every value written here came from a `SystemTime`, so the round trip is exact. A value this
    // build cannot represent is treated as "no observation", which only costs one re-projection.
    let magnitude = Duration::from_nanos(nanos.unsigned_abs());
    if nanos < 0 {
        UNIX_EPOCH.checked_sub(magnitude)
    } else {
        UNIX_EPOCH.checked_add(magnitude)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn modification_times_round_trip_including_the_epoch_and_missing_values() {
        let cases = [
            None,
            Some(UNIX_EPOCH),
            Some(UNIX_EPOCH + Duration::from_nanos(1_700_000_000_123_456_789)),
            Some(UNIX_EPOCH - Duration::from_nanos(1)),
        ];
        for case in cases {
            assert_eq!(modified_from_ns(modified_to_ns(case)), case);
        }
    }

    #[test]
    fn a_path_that_is_not_utf8_cannot_be_a_key() {
        #[cfg(unix)]
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;

            let path = PathBuf::from(OsStr::from_bytes(b"pages/\xff.md"));
            assert!(matches!(
                path_key(&path),
                Err(IndexError::NonUtf8Path { .. })
            ));
        }
    }
}
