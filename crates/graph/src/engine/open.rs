use crate::engine::{EngineError, Graph, OpenOptions, ScanMode, ScanProgress};
use crate::index::Index;
use crate::source::{self, GraphSource};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

impl Graph {
    /// Open the graph described by `options` and, unless the options turn it off, scan its files into
    /// the index.
    ///
    /// Opening runs the same scan [`crate::engine::Freshness::CheckNow`] runs: every discovered file is compared
    /// with its baseline, and only the files that changed are read, parsed once, and projected. When
    /// the call returns, queries see the projected state, so a caller can read a page immediately
    /// after opening (read-your-writes at the returned revision).
    ///
    /// [`OpenOptions::with_startup_scan`] turns that off for a caller that only writes. What is lost
    /// then is read-your-writes at open, not consistency: the stored rows may predate the files, and
    /// a read that needs them current names [`crate::engine::Freshness::CheckNow`] and pays for the scan itself.
    /// A missing or changed configuration signature always forces a full reprojection, regardless
    /// of that option, so writes never combine names from different configuration generations.
    pub fn open(options: OpenOptions) -> Result<Self, EngineError> {
        Self::open_with_progress(options, Arc::new(ScanProgress::new()))
    }

    /// Open the graph, reporting the scan's progress to a counter another thread can read.
    ///
    /// [`Graph::open`] is this call with a counter nobody keeps a handle to. This one exists for a
    /// caller that must answer before the call returns: a server binds its port first and serves
    /// "still scanning" until the graph is open (ADR-0008), and the counter is how it says how far
    /// the scan has got without touching the index the scan holds. The counter reads `(0, 0)` until
    /// discovery returns, then counts prepared files; SQL publication can continue after that total.
    pub fn open_with_progress(
        options: OpenOptions,
        progress: Arc<ScanProgress>,
    ) -> Result<Self, EngineError> {
        let OpenOptions {
            root,
            index_path,
            freshness,
            write_lock_wait,
            startup_scan,
            scan_state,
        } = options;
        let source = Arc::new(GraphSource::open(root)?);
        // Resolved before the index opens so the lock location is fixed before anything else can
        // move. Both the Git directory and the fallback canonical graph root already exist.
        let write_lock_dir = source::lock_directory(source.root());
        let migration_lock =
            source::WriteLock::acquire_for_scan(&write_lock_dir, Some(write_lock_wait))?;
        let journal = source::MoveJournal::in_directory(&write_lock_dir)?;
        if let Some(intent) = Index::legacy_intent_at(&index_path)? {
            if let source::ScanLock::Unavailable(error) = migration_lock {
                return Err(error.into());
            }
            journal.write(&intent)?;
            Index::clear_legacy_intent_at(&index_path)?;
        }
        let index = Index::open_at(&index_path)?;
        drop(migration_lock);
        let mut graph = Self {
            source,
            journal,
            index,
            read_coordinator: None,
            freshness,
            scan_state,
            write_lock_dir,
            write_lock_wait,
            progress,
            #[cfg(test)]
            test_fault: None,
            #[cfg(test)]
            test_scan_prepare: None,
            #[cfg(test)]
            test_shadow_build: None,
            #[cfg(test)]
            test_shadow_catchup: None,
            #[cfg(test)]
            test_fault_fired: Arc::new(AtomicBool::new(false)),
        };
        // Reconcile pending moves before serving or the ordinary startup scan. Recovery takes
        // the graph lock and enforces the configuration-generation gate before resolving paths.
        graph.recover_move_intent()?;
        // The startup scan exists to make the handle readable, not to prove the content, so it trusts
        // the stat like any other scan does; a `VerifyContent` read after it scans again and reads
        // every file. Skipping it therefore forgoes an answer for reads, not an invariant: a write
        // still finds its target from the index and the files, and a read that needs the rows
        // current asks for a scan. A configuration mismatch overrides skipping it: those rows
        // cannot be used to resolve names for a write. It is on by default for ordinary reads.
        if startup_scan
            || !graph
                .index
                .configuration_matches(graph.source.signature())?
        {
            graph.scan(ScanMode::TrustStat)?;
        }
        Ok(graph)
    }
}
