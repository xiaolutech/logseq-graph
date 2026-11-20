//! Filesystem reads: bytes as UTF-8 text, and the stat fields used for change detection.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::error::SourceError;

/// Filesystem metadata recorded for every discovered file.
///
/// The fields are the ones a later startup slice can compare against a stored snapshot to decide
/// whether a file changed without re-reading it. They are a hint only: content decides, because
/// modification times have coarse resolution and can be restored by tools such as `git checkout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileMetadata {
    size: u64,
    modified: Option<SystemTime>,
}

impl FileMetadata {
    /// File size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Last modification time, or `None` when the platform does not report one.
    pub fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    /// Rebuild the pair from values a caller already holds.
    ///
    /// Crate-internal: production code obtains a `FileMetadata` from a real `stat`, so the fields
    /// stay private. The index uses this to restore a stored baseline, where the stat was observed
    /// by an earlier run.
    pub(crate) fn from_stored(size: u64, modified: Option<SystemTime>) -> Self {
        Self { size, modified }
    }
}

/// Resolve `path` to an absolute, symlink-free path.
///
/// The graph root is resolved once at open so later path handling never depends on the process
/// working directory.
pub(crate) fn canonicalize(path: &Path) -> Result<PathBuf, SourceError> {
    fs::canonicalize(path).map_err(|source| SourceError::Io {
        path: path.to_owned(),
        source,
    })
}

/// Read filesystem metadata for `path`, following symlinks.
pub(crate) fn stat(path: &Path) -> Result<fs::Metadata, SourceError> {
    fs::metadata(path).map_err(|source| SourceError::Io {
        path: path.to_owned(),
        source,
    })
}

/// Whether `path` lexically names a location inside `root`.
///
/// Both paths are expected absolute. The comparison is component-wise on the path text, not on
/// resolved symlinks: a symlink inside the graph pointing outside is caught by the caller reading
/// or writing through it, not here, because the root itself was already resolved at open time.
pub(crate) fn is_within(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok()
}

/// Resolve an existing directory or its nearest existing ancestor before creating it.
pub(crate) fn directory_within(root: &Path, path: &Path) -> Result<PathBuf, SourceError> {
    let invalid = || SourceError::Io {
        path: path.to_owned(),
        source: io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path is outside the graph root",
        ),
    };
    let root = canonicalize(root)?;
    let mut ancestor = path;
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ancestor = ancestor.parent().ok_or_else(invalid)?;
            }
            Err(source) => {
                return Err(SourceError::Io {
                    path: ancestor.to_owned(),
                    source,
                })
            }
        }
    }
    let resolved = canonicalize(ancestor)?;
    let tail = path.strip_prefix(ancestor).map_err(|_| invalid())?;
    if !is_within(&root, &resolved)
        || !tail
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return Err(invalid());
    }
    Ok(resolved.join(tail))
}

/// Reduce filesystem metadata to the fields this crate records.
pub(crate) fn record(metadata: &fs::Metadata) -> FileMetadata {
    FileMetadata {
        size: metadata.len(),
        modified: metadata.modified().ok(),
    }
}

/// Read a file as UTF-8 text.
///
/// The text is returned exactly as stored: a byte order mark stays the first character, line
/// endings stay as written, and a missing final newline stays missing. That is what
/// [`PageDocument::parse`](crate::document::PageDocument::parse) and
/// [`PageDocument::to_source`](crate::document::PageDocument::to_source) need to round-trip the
/// file byte for byte.
pub(crate) fn read_text(path: &Path) -> Result<String, SourceError> {
    let bytes = fs::read(path).map_err(|source| SourceError::Io {
        path: path.to_owned(),
        source,
    })?;
    decode_utf8(path, bytes)
}

/// Read a file as UTF-8 text, or `None` when it does not exist.
///
/// A missing file is an answer here, not a failure, because an optional configuration file uses
/// the same read path as a required one.
pub(crate) fn read_text_if_exists(path: &Path) -> Result<Option<String>, SourceError> {
    match fs::read(path) {
        Ok(bytes) => decode_utf8(path, bytes).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SourceError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Content fingerprint of a file's text, used to decide whether the bytes changed.
///
/// This is FNV-1a over the UTF-8 bytes: a short, dependency-free hash whose value is fixed by this
/// function for as long as the algorithm is unchanged. It is a change detector, not a security
/// hash, so a collision only costs one unnecessary re-projection. The value is stable across
/// processes and platforms, which matters because it is stored in the index and compared by a
/// later run of a different build.
pub(crate) fn content_fingerprint(text: &str) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut fingerprint = OFFSET_BASIS;
    for byte in text.as_bytes() {
        fingerprint ^= u64::from(*byte);
        fingerprint = fingerprint.wrapping_mul(PRIME);
    }
    fingerprint
}

fn decode_utf8(path: &Path, bytes: Vec<u8>) -> Result<String, SourceError> {
    String::from_utf8(bytes).map_err(|error| SourceError::NotUtf8 {
        path: path.to_owned(),
        valid_up_to: error.utf8_error().valid_up_to(),
        len: error.as_bytes().len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_fingerprint_matches_the_published_fnv1a_vectors() {
        // Published FNV-1a 64-bit vectors. Pinning them keeps a later refactor from silently
        // changing the meaning of fingerprints already stored in an index.
        assert_eq!(content_fingerprint(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(content_fingerprint("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(content_fingerprint("foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn content_fingerprint_separates_content_that_differs_anywhere() {
        let base = "- alpha\n\t- beta\n";

        assert_eq!(content_fingerprint(base), content_fingerprint(base));
        for changed in [
            "- alpha\n\t- beta",
            "- alpha\n\t- gamma\n",
            "- alpha\n\t- beta\n\n",
            " - alpha\n\t- beta\n",
        ] {
            assert_ne!(
                content_fingerprint(base),
                content_fingerprint(changed),
                "a change to {changed:?} must change the fingerprint"
            );
        }
    }
}
