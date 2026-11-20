//! Freshness metadata carried by every read result.
//!
//! A read result is not just a value: it is a value plus the claim of when and against which
//! observation it was produced (ADR-0006). [`ReadMeta`] holds that claim, and [`Read`] pairs it with
//! the value so a caller cannot consume one without the other.
//!
//! `stale = false` means the derived model matched the observed files at the stated scan. It is not
//! a promise that no external writer has changed a file since then; that is what
//! [`ReadMeta::last_scan`] is for.

use std::time::SystemTime;

use super::GraphMetadata;

/// Which graph instance produced a read, at which revision, and how fresh it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadMeta {
    graph: GraphMetadata,
    stale: bool,
    last_scan: SystemTime,
}

impl ReadMeta {
    pub(crate) fn new(graph: GraphMetadata, stale: bool, last_scan: SystemTime) -> Self {
        Self {
            graph,
            stale,
            last_scan,
        }
    }

    /// Identity of the derived instance and the revision the read observed.
    pub fn graph(&self) -> &GraphMetadata {
        &self.graph
    }

    /// Opaque identifier of the derived graph instance.
    pub fn instance_id(&self) -> &str {
        self.graph.instance_id()
    }

    /// Committed index revision the read observed.
    pub fn revision(&self) -> u64 {
        self.graph.revision()
    }

    /// Whether the engine cannot vouch that the derived model matches the files.
    ///
    /// `true` means the last scan failed, so the rows may be behind the files or may never have been
    /// checked against them; it does not claim that a known change was missed. `true` is only
    /// reported when the caller explicitly allowed stale data; a policy that refuses stale data fails
    /// the read instead of returning this flag.
    ///
    /// `false` claims only what the scan behind it could observe. A stat-based scan compared size and
    /// modification time; [`Freshness::VerifyContent`](crate::engine::Freshness::VerifyContent) read
    /// and fingerprinted every file, so under that policy `false` also means no file content differed
    /// from its baseline.
    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// When the scan behind this result started observing the graph files.
    ///
    /// The start, not the completion: a long scan must not make the data look newer than it is.
    /// This is the observation time of the process holding the graph handle, not a property stored
    /// with the index: a new process re-observes the files when it opens, so a time from an earlier
    /// process would describe an observation the current one cannot vouch for. Do not read it as the
    /// index's freshness guarantee; it says how old the newest observation behind this result is.
    pub fn last_scan(&self) -> SystemTime {
        self.last_scan
    }
}

/// A read result: the value and the metadata of the revision that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read<T> {
    value: T,
    meta: ReadMeta,
}

impl<T> Read<T> {
    pub(crate) fn new(value: T, meta: ReadMeta) -> Self {
        Self { value, meta }
    }

    /// The value the read produced.
    pub fn value(&self) -> &T {
        &self.value
    }

    /// Take the value, discarding the freshness metadata.
    pub fn into_value(self) -> T {
        self.value
    }

    /// Freshness metadata of this read.
    pub fn meta(&self) -> &ReadMeta {
        &self.meta
    }

    /// Whether this read is marked stale.
    pub fn is_stale(&self) -> bool {
        self.meta.is_stale()
    }
}

impl<T> Read<Option<T>> {
    /// The found value, or `None` when the read found nothing.
    ///
    /// A miss still carries metadata, so it is a normal read result rather than an absent one.
    pub fn present(&self) -> Option<&T> {
        self.value.as_ref()
    }
}
