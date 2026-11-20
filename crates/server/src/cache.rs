//! Where the CLI keeps the derived index.
//!
//! The engine deliberately owns no cache policy: [`OpenOptions`](logseq_graph::engine::OpenOptions)
//! says the caller owns the index location, and this module is that caller's policy.
//!
//! One cache directory per graph, named by a stable hash of the graph root's canonical absolute
//! path, under the platform cache directory:
//!
//! - `$XDG_CACHE_HOME/logseq-graph/<key>/index.db` when the variable is set;
//! - `$HOME/Library/Caches/logseq-graph/<key>/index.db` on macOS otherwise;
//! - `$HOME/.cache/logseq-graph/<key>/index.db` as the last resort.
//!
//! The key is the first eight hex digits of an FNV-1a hash of the canonical path. Eight digits can
//! collide, so the directory also records the canonical root it was derived from in a `graph-root`
//! file: a directory whose record names another root is refused instead of served, because serving
//! it would answer one graph's questions with another graph's rows.
//!
//! Nothing here writes inside the graph. A real graph is a git repository, and one file left in it
//! is one dirty tree.

use std::path::{Path, PathBuf};

/// Directory under the platform cache root that holds one directory per graph.
const CACHE_DIR: &str = "logseq-graph";

/// File inside one cache directory that holds the database.
const INDEX_FILE: &str = "index.db";

/// File inside one cache directory that records which graph root it belongs to.
const ROOT_MARKER: &str = "graph-root";

/// Hex digits of the hash used as the directory name.
const KEY_DIGITS: usize = 8;

/// Failure of the cache policy.
#[derive(Debug)]
pub(crate) enum CacheError {
    /// Neither `XDG_CACHE_HOME` nor `HOME` is set, so there is nowhere to keep the index.
    NoCacheDirectory,
    /// The graph root could not be resolved to an absolute path.
    Canonicalize {
        /// The path that was asked for.
        path: PathBuf,
        /// Why it could not be resolved.
        detail: String,
    },
    /// The cache directory exists for another graph root.
    Collision {
        /// The cache directory.
        directory: PathBuf,
        /// The root the directory records.
        recorded: String,
        /// The root this invocation asked for.
        requested: String,
    },
    /// A cache directory or its marker could not be read or created.
    Io {
        /// The path that failed.
        path: PathBuf,
        /// Why it failed.
        detail: String,
    },
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCacheDirectory => write!(
                formatter,
                "no cache directory: set XDG_CACHE_HOME or HOME, or pass --index"
            ),
            Self::Canonicalize { path, detail } => {
                write!(formatter, "graph not found: {} ({detail})", path.display())
            }
            Self::Collision {
                directory,
                recorded,
                requested,
            } => write!(
                formatter,
                "cache key collision: {} records graph root {recorded:?} but this graph is \
                 {requested:?}; pass --index to choose another location",
                directory.display()
            ),
            Self::Io { path, detail } => {
                write!(formatter, "could not use {}: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for CacheError {}

/// Index path the cache policy picks for `graph_root`, creating the directory when missing.
pub(crate) fn index_path(graph_root: &Path) -> Result<PathBuf, CacheError> {
    let base = cache_root().ok_or(CacheError::NoCacheDirectory)?;
    let canonical =
        std::fs::canonicalize(graph_root).map_err(|error| CacheError::Canonicalize {
            path: graph_root.to_path_buf(),
            detail: error.to_string(),
        })?;
    index_path_in(&base, &canonical)
}

/// Index path inside `cache_root` for an already canonical `graph_root`.
pub(crate) fn index_path_in(cache_root: &Path, graph_root: &Path) -> Result<PathBuf, CacheError> {
    let key = key_for(graph_root);
    let directory = cache_root.join(CACHE_DIR).join(&key);
    std::fs::create_dir_all(&directory).map_err(|error| CacheError::Io {
        path: directory.clone(),
        detail: error.to_string(),
    })?;

    // The recorded root is the canonical path, the same value the key is derived from, so a
    // symlinked invocation of one graph cannot look like a collision.
    let requested = graph_root.as_os_str().as_encoded_bytes();
    let marker = directory.join(ROOT_MARKER);
    match std::fs::read(&marker) {
        Ok(recorded) => {
            let recorded = recorded.strip_suffix(b"\n").unwrap_or(&recorded);
            if recorded != requested {
                return Err(CacheError::Collision {
                    directory,
                    recorded: String::from_utf8_lossy(recorded).into_owned(),
                    requested: String::from_utf8_lossy(requested).into_owned(),
                });
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut contents = requested.to_vec();
            contents.push(b'\n');
            std::fs::write(&marker, contents).map_err(|error| CacheError::Io {
                path: marker,
                detail: error.to_string(),
            })?;
        }
        Err(error) => {
            return Err(CacheError::Io {
                path: marker,
                detail: error.to_string(),
            })
        }
    }

    Ok(directory.join(INDEX_FILE))
}

/// The platform cache directory, before the per-graph directory is added.
fn cache_root() -> Option<PathBuf> {
    cache_root_from(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))
}

/// Cache directory from the two environment values it depends on.
///
/// Split from [`cache_root`] so the precedence is testable without mutating the environment of a
/// running test binary.
fn cache_root_from(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(xdg) = xdg.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(xdg));
    }
    let home = PathBuf::from(home.filter(|value| !value.is_empty())?);
    Some(if cfg!(target_os = "macos") {
        home.join("Library").join("Caches")
    } else {
        home.join(".cache")
    })
}

/// Directory name for `graph_root`: the first hex digits of its FNV-1a hash.
fn key_for(graph_root: &Path) -> String {
    let hash = fnv1a64(graph_root.as_os_str().as_encoded_bytes());
    format!("{hash:016x}")[..KEY_DIGITS].to_owned()
}

/// FNV-1a, 64-bit, over the path's bytes.
///
/// Deliberately not `std`'s `DefaultHasher`: its output is not stable across releases, and a cache
/// key that moves would orphan every index already on disk.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU64, Ordering};

    /// A temporary directory removed when dropped.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "logseq-graph-cli-cache-{label}-{}-{serial}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temporary directory");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn fnv_1a_matches_the_published_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"hello"), 0xa430_d846_80aa_bd0b);
    }

    #[test]
    fn the_key_is_eight_hex_digits_of_the_path_hash() {
        let first = key_for(Path::new("/graphs/work"));
        let second = key_for(Path::new("/graphs/other"));

        assert_eq!(first.len(), KEY_DIGITS);
        assert!(
            first.chars().all(|digit| digit.is_ascii_hexdigit()),
            "a key is hex, not {first:?}"
        );
        assert_eq!(
            first,
            key_for(Path::new("/graphs/work")),
            "the same path always hashes to the same key"
        );
        assert_ne!(first, second, "two graphs do not share a key");
    }

    #[test]
    fn the_index_lives_under_the_cache_root_and_outside_the_graph() {
        let temp = TempDir::new("layout");
        let cache_root = temp.path.join("cache");
        let graph_root = temp.path.join("graph");
        std::fs::create_dir_all(&graph_root).expect("create graph root");

        let index = index_path_in(&cache_root, &graph_root).expect("compute index path");

        assert_eq!(
            index,
            cache_root
                .join(CACHE_DIR)
                .join(key_for(&graph_root))
                .join(INDEX_FILE)
        );
        assert!(
            index.parent().is_some_and(Path::exists),
            "the directory is created on demand"
        );
        assert!(
            !index.starts_with(&graph_root),
            "the index is never placed inside the graph"
        );
    }

    #[test]
    fn the_directory_records_the_root_it_belongs_to() {
        let temp = TempDir::new("marker");
        let cache_root = temp.path.join("cache");
        let graph_root = temp.path.join("graph");
        std::fs::create_dir_all(&graph_root).expect("create graph root");

        let index = index_path_in(&cache_root, &graph_root).expect("compute index path");
        let marker = index
            .parent()
            .expect("index has a directory")
            .join(ROOT_MARKER);

        assert_eq!(
            std::fs::read_to_string(&marker).expect("read the marker"),
            format!("{}\n", graph_root.display())
        );
        // A second invocation of the same graph reuses the directory.
        assert_eq!(
            index_path_in(&cache_root, &graph_root).expect("reuse the directory"),
            index
        );
    }

    #[test]
    fn a_directory_that_records_another_root_is_refused() {
        let temp = TempDir::new("collision");
        let cache_root = temp.path.join("cache");
        let graph_root = temp.path.join("graph");
        std::fs::create_dir_all(&graph_root).expect("create graph root");
        let index = index_path_in(&cache_root, &graph_root).expect("compute index path");

        // Simulate the collision a truncated hash can produce: the directory exists and records a
        // different root.
        let marker = index
            .parent()
            .expect("index has a directory")
            .join(ROOT_MARKER);
        std::fs::write(&marker, "/somewhere/else\n").expect("write the marker");

        match index_path_in(&cache_root, &graph_root) {
            Err(CacheError::Collision { recorded, .. }) => {
                assert_eq!(recorded, "/somewhere/else");
            }
            other => panic!("a foreign marker must be refused, got {other:?}"),
        }
    }

    #[test]
    fn the_cache_root_prefers_xdg_then_the_platform_default() {
        assert_eq!(
            cache_root_from(Some("/xdg/cache".into()), Some("/home/ada".into())),
            Some(PathBuf::from("/xdg/cache"))
        );
        let expected = if cfg!(target_os = "macos") {
            PathBuf::from("/home/ada/Library/Caches")
        } else {
            PathBuf::from("/home/ada/.cache")
        };
        assert_eq!(
            cache_root_from(None, Some("/home/ada".into())),
            Some(expected)
        );
        assert_eq!(
            cache_root_from(Some("".into()), Some("/home/ada".into())),
            Some(if cfg!(target_os = "macos") {
                PathBuf::from("/home/ada/Library/Caches")
            } else {
                PathBuf::from("/home/ada/.cache")
            }),
            "an empty XDG_CACHE_HOME is treated as unset"
        );
        assert_eq!(
            cache_root_from(None, None),
            None,
            "without either variable there is no cache location"
        );
    }

    #[test]
    fn a_root_that_does_not_exist_is_reported_as_not_found() {
        let temp = TempDir::new("missing-root");

        match index_path(&temp.path.join("absent")) {
            Err(CacheError::Canonicalize { detail, .. }) => {
                assert!(!detail.is_empty(), "the failure states why");
            }
            other => panic!("a missing graph root must fail, got {other:?}"),
        }
    }
}
