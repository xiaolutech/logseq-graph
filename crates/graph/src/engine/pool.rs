//! Bounded read connections; freshness scans and writes retain one serialized graph handle.

use std::num::NonZeroUsize;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::domain::{
    BlockId, BlockRefResolution, BlockSelector, PageSnapshot, PageVersions, Read, RootedSnapshot,
};
use crate::index::StorageUsage;

use super::{EngineError, Freshness, Graph, Reads};

#[cfg(test)]
mod tests;

/// A graph with four read-only SQLite connections and one serialized write handle.
///
/// Read closures run concurrently, without the writer mutex. Each receives a restricted handle
/// that cannot edit files. In-memory fallback has no independent database connections, so it uses
/// the original handle under the writer mutex instead. Connection leases are returned on unwind.
#[derive(Debug)]
pub struct SharedGraph {
    writer: Arc<Mutex<Graph>>,
    readers: Mutex<Vec<Graph>>,
    available: Condvar,
    pooled: bool,
}

/// The read-only engine surface available during a [`SharedGraph::read`] call.
#[derive(Debug)]
pub struct ReadHandle<'a> {
    graph: &'a mut Graph,
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    #[expect(
        clippy::expect_used,
        reason = "a panic while holding engine state is not recoverable"
    )]
    mutex.lock().expect("engine state lock is not poisoned")
}

impl SharedGraph {
    /// Retain `graph` for writes and open four independent read-only connections.
    pub fn new(graph: Graph) -> Result<Self, EngineError> {
        Self::with_readers(graph, NonZeroUsize::MIN.saturating_add(3))
    }

    /// Choose a nonzero bound on simultaneously executing read closures.
    pub fn with_readers(graph: Graph, count: NonZeroUsize) -> Result<Self, EngineError> {
        let writer = Arc::new(Mutex::new(graph));
        let mut readers = Vec::with_capacity(count.get());
        {
            let graph = lock(&writer);
            for _ in 0..count.get() {
                let Some(index) = graph.index.reader()? else {
                    break;
                };
                readers.push(Graph {
                    source: Arc::clone(&graph.source),
                    journal: crate::source::MoveJournal::in_directory(&graph.write_lock_dir)?,
                    index,
                    read_coordinator: Some(Arc::clone(&writer)),
                    freshness: graph.freshness,
                    scan_state: Arc::clone(&graph.scan_state),
                    write_lock_dir: graph.write_lock_dir.clone(),
                    write_lock_wait: graph.write_lock_wait,
                    progress: Arc::clone(&graph.progress),
                    #[cfg(test)]
                    test_fault: None,
                    #[cfg(test)]
                    test_scan_prepare: None,
                    #[cfg(test)]
                    test_shadow_build: None,
                    #[cfg(test)]
                    test_shadow_catchup: None,
                    #[cfg(test)]
                    test_fault_fired: Arc::clone(&graph.test_fault_fired),
                });
            }
        }
        Ok(Self {
            writer,
            pooled: !readers.is_empty(),
            readers: Mutex::new(readers),
            available: Condvar::new(),
        })
    }

    /// Run reads on a leased connection, waiting only when all read slots are occupied.
    ///
    /// Select an explicit policy with [`ReadHandle::reads`], or use [`ReadHandle::default_reads`].
    /// Each read still settles its policy independently, just like reads on [`Graph`].
    pub fn read<T>(&self, operation: impl FnOnce(&mut ReadHandle<'_>) -> T) -> T {
        if !self.pooled {
            return operation(&mut ReadHandle {
                graph: &mut lock(&self.writer),
            });
        }
        let mut readers = lock(&self.readers);
        let graph = loop {
            if let Some(graph) = readers.pop() {
                break graph;
            }
            #[expect(
                clippy::expect_used,
                reason = "a panic while holding the pool is not recoverable"
            )]
            {
                readers = self
                    .available
                    .wait(readers)
                    .expect("read pool is not poisoned");
            }
        };
        drop(readers);
        let mut lease = Lease {
            pool: self,
            graph: Some(graph),
        };
        #[expect(
            clippy::expect_used,
            reason = "a new lease always holds its connection"
        )]
        operation(&mut ReadHandle {
            graph: lease.graph.as_mut().expect("leased graph"),
        })
    }

    /// Serialize one write operation on the original graph handle and its existing file locks.
    pub fn write<T>(&self, operation: impl FnOnce(&mut Graph) -> T) -> T {
        operation(&mut lock(&self.writer))
    }
}

struct Lease<'a> {
    pool: &'a SharedGraph,
    graph: Option<Graph>,
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(graph) = self.graph.take() {
            lock(&self.pool.readers).push(graph);
            self.pool.available.notify_one();
        }
    }
}

impl ReadHandle<'_> {
    /// Read under the graph's configured default policy.
    pub fn default_reads(&mut self) -> Reads<'_> {
        self.graph.reads(self.graph.freshness)
    }

    /// Read under an explicit policy, including synchronous freshness checks.
    pub fn reads(&mut self, freshness: Freshness) -> Reads<'_> {
        self.graph.reads(freshness)
    }

    /// Read one page's current source snapshot, under the configured default policy.
    pub fn page_snapshot(&mut self, name: &str) -> Result<Read<Option<PageSnapshot>>, EngineError> {
        self.graph.page_snapshot(name)
    }

    /// Compare indexed and current file versions without running a freshness scan.
    pub fn page_versions(&mut self, name: &str) -> Result<Read<Option<PageVersions>>, EngineError> {
        self.graph.page_versions(name)
    }

    /// Read the current source subtree rooted at one block.
    pub fn block_snapshot(
        &mut self,
        selector: &BlockSelector,
    ) -> Result<Read<Option<RootedSnapshot>>, EngineError> {
        self.graph.block_snapshot(selector)
    }

    /// Resolve a durable reference, preserving missing and ambiguous answers as values.
    pub fn resolve_block_ref(
        &mut self,
        id: &BlockId,
    ) -> Result<Read<BlockRefResolution>, EngineError> {
        self.graph.resolve_block_ref(id)
    }

    /// Inspect live, building, and retired index storage.
    pub fn index_storage_usage(&self) -> Result<Option<StorageUsage>, EngineError> {
        self.graph.index_storage_usage()
    }
}
