use super::scan::SharedScanState;
use crate::engine::ScanState;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// How a read relates the derived index to the graph files (ADR-0006).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Freshness {
    /// Serve the last committed scan, but fail with [`crate::engine::EngineError::StaleIndex`] when the engine
    /// cannot vouch that the index matches the files, which is the case after any scan it ran failed.
    ///
    /// This is the default: refusing is preferred over silently answering with data the engine cannot
    /// stand behind. The refusal lasts until a scan succeeds, so a single transient failure keeps
    /// every `LastScan` read on this handle failing until a later [`Freshness::CheckNow`] or
    /// background scan succeeds. That is deliberate (fail closed).
    #[default]
    LastScan,
    /// Serve the last committed scan even when the engine cannot vouch for it, and mark the result
    /// stale. The caller accepts possibly outdated data deliberately.
    AllowStale,
    /// Scan the files now, apply every discovered change, and only then answer.
    ///
    /// A failed scan fails the read; there is no fallback to the last committed state under this
    /// policy, because the caller asked for the files as they are now.
    CheckNow,
    /// Scan the files now, reading every file instead of trusting its `(size, mtime)` snapshot,
    /// and only then answer.
    ///
    /// A file whose size and modification time are unchanged can still hold different text: tools
    /// such as `rsync -a`, `cp -p`, and `tar` restore modification times, and a sync client can do the
    /// same. [`Freshness::CheckNow`] trusts that snapshot and therefore cannot see such a change; this
    /// policy reads and fingerprints every file, so it can.
    ///
    /// It costs one read of the whole graph per read. That is the price of not being able to trust the
    /// stat: skip this policy when a stat-based answer is good enough.
    VerifyContent,
}

/// How long a write waits for the graph write lock before it reports [`crate::engine::WriteError::Lock`].
///
/// A writer holds the lock for the milliseconds a read, an edit, and one rename take, so a wait this
/// long outlasts any write that is not stuck. Failing instead of waiting forever keeps a hung writer
/// from blocking every other one, and the caller can retry.
pub const DEFAULT_WRITE_LOCK_WAIT: Duration = Duration::from_secs(5);

/// Everything [`crate::engine::Graph::open`] needs: the graph root, the index location, the default freshness,
/// how long a write waits for the graph write lock, and whether opening scans.
///
/// The index path names the database file; its directory is created when missing. The caller owns
/// that location, which keeps this layer free of a platform cache-directory policy.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    pub(super) root: PathBuf,
    pub(super) index_path: PathBuf,
    pub(super) freshness: Freshness,
    pub(super) write_lock_wait: Duration,
    pub(super) startup_scan: bool,
    pub(super) scan_state: SharedScanState,
}

impl OpenOptions {
    /// Options for the graph rooted at `root`, indexed at `index_path`, with the default freshness
    /// [`Freshness::LastScan`].
    pub fn new(root: impl Into<PathBuf>, index_path: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            index_path: index_path.into(),
            freshness: Freshness::default(),
            write_lock_wait: DEFAULT_WRITE_LOCK_WAIT,
            startup_scan: true,
            scan_state: Arc::new(ScanState::new()),
        }
    }

    /// Share scan exclusion and freshness observations with another handle of the same graph.
    ///
    /// This is for a long-lived process that scans on one handle while serving reads on another.
    /// Every handle must open the same root and index. Each still owns a separate SQLite connection,
    /// so a last-scan read never waits for the background scan's filesystem walk.
    pub fn with_scan_state(mut self, scan_state: Arc<ScanState>) -> Self {
        self.scan_state = scan_state;
        self
    }

    /// Whether [`crate::engine::Graph::open`] scans the files before returning a handle.
    ///
    /// The default is `true`, because the startup scan is what makes a fresh handle answer a read
    /// immediately; it is not what proves the index current.
    ///
    /// Turning it off is for a caller that only writes. The index may then still hold rows from the
    /// previous scan, including a `pages` row whose file has since moved or disappeared, and no read
    /// of this handle may assume otherwise: a read that needs the files reflected asks for
    /// [`Freshness::CheckNow`] and pays for that scan itself. A write does not need it, because it
    /// resolves its target with one `pages.name_key` query and re-lands the stored path through
    /// `source.page_at`, falling back to one `discover()` when that file is gone. An outdated path
    /// row therefore costs a fallback, never a write to the wrong file.
    /// A missing or mismatched persisted configuration signature overrides this option: opening
    /// fully reprojects, including for a fresh index. Only a matching generation can skip the scan.
    pub fn with_startup_scan(mut self, startup_scan: bool) -> Self {
        self.startup_scan = startup_scan;
        self
    }

    /// Whether opening scans the files.
    pub fn startup_scan(&self) -> bool {
        self.startup_scan
    }

    /// Use `freshness` for every read that does not name its own policy.
    pub fn with_freshness(mut self, freshness: Freshness) -> Self {
        self.freshness = freshness;
        self
    }

    /// Freshness a read uses when it does not name its own policy.
    pub fn freshness(&self) -> Freshness {
        self.freshness
    }

    /// Graph root these options open.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Index file these options open.
    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    /// How long a write keeps retrying for the graph write lock before it reports
    /// [`crate::engine::WriteError::Lock`].
    ///
    /// [`Duration::ZERO`] fails immediately when another writer holds the lock, which is what a test
    /// needs to observe the refusal without waiting out the default.
    pub fn with_write_lock_wait(mut self, wait: Duration) -> Self {
        self.write_lock_wait = wait;
        self
    }

    /// How long a write waits for the graph write lock.
    pub fn write_lock_wait(&self) -> Duration {
        self.write_lock_wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source;

    #[test]
    fn a_root_without_a_git_entry_keeps_the_lock_in_the_graph_root() {
        let root = std::env::temp_dir().join("logseq-graph-unit-test-root-without-git");
        assert!(
            !root.join(".git").exists(),
            "the fixture root must not be a working tree"
        );
        assert_eq!(source::lock_directory(&root), root);
    }

    #[test]
    fn a_write_waits_for_the_lock_by_default() {
        assert_eq!(
            OpenOptions::new("/graph", "/cache/index.db").write_lock_wait(),
            DEFAULT_WRITE_LOCK_WAIT
        );
        assert_eq!(
            OpenOptions::new("/graph", "/cache/index.db")
                .with_write_lock_wait(Duration::ZERO)
                .write_lock_wait(),
            Duration::ZERO
        );
    }

    #[test]
    fn the_default_freshness_refuses_stale_data() {
        assert_eq!(Freshness::default(), Freshness::LastScan);
        assert_eq!(
            OpenOptions::new("/graph", "/cache/index.db").freshness(),
            Freshness::LastScan
        );
        assert_eq!(
            OpenOptions::new("/graph", "/cache/index.db")
                .with_freshness(Freshness::AllowStale)
                .freshness(),
            Freshness::AllowStale
        );
    }
}
