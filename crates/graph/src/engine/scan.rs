//! Scan coordination shared by handles of one served graph.
//!
//! A polling handle and a request handle have separate SQLite connections. Sharing the observation
//! makes a background failure visible to `LastScan` reads immediately, while the operation lock
//! serializes scan commits and bounded path refreshes. Preparation can overlap across handles.
//! Neither lock is held by a `LastScan` read
//! while it queries the index.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use crate::source::GraphSource;

/// Freshness facts and scan exclusion shared by related [`Graph`](super::Graph) handles.
///
/// Construct this in the long-lived caller and pass it to every handle through
/// [`OpenOptions::with_scan_state`](super::OpenOptions::with_scan_state). Ordinary callers need not
/// do so: each opened graph creates its own state, preserving the single-handle behaviour.
#[derive(Debug)]
pub struct ScanState {
    observation: Mutex<Observation>,
    operation: Mutex<()>,
}

#[derive(Debug)]
struct Observation {
    divergence: Option<String>,
    last_scan: SystemTime,
    source: Option<Arc<GraphSource>>,
}

/// The freshness facts behind one read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScanObservation {
    pub(crate) divergence: Option<String>,
    pub(crate) last_scan: SystemTime,
}

impl ScanState {
    /// State for a graph that has not completed a scan yet.
    pub fn new() -> Self {
        Self {
            observation: Mutex::new(Observation {
                divergence: None,
                last_scan: SystemTime::now(),
                source: None,
            }),
            operation: Mutex::new(()),
        }
    }

    pub(crate) fn observation(&self) -> ScanObservation {
        #[expect(
            clippy::expect_used,
            reason = "a poisoned observation lock means a scan panicked; no freshness claim can be \
                      recovered after that"
        )]
        let observation = self
            .observation
            .lock()
            .expect("the scan observation lock is not poisoned");
        ScanObservation {
            divergence: observation.divergence.clone(),
            last_scan: observation.last_scan,
        }
    }

    pub(crate) fn scan_finished(
        &self,
        started: SystemTime,
        divergence: Option<String>,
        source: Option<&Arc<GraphSource>>,
    ) {
        #[expect(
            clippy::expect_used,
            reason = "a poisoned observation lock means a scan panicked; no freshness claim can be \
                      recovered after that"
        )]
        let mut observation = self
            .observation
            .lock()
            .expect("the scan observation lock is not poisoned");
        if divergence.is_none() {
            observation.last_scan = started;
            observation.source = source.cloned();
        }
        observation.divergence = divergence;
    }

    pub(crate) fn index_reopened(&self, source: &Arc<GraphSource>) {
        #[expect(
            clippy::expect_used,
            reason = "a poisoned observation lock invalidates scan state"
        )]
        let mut observation = self
            .observation
            .lock()
            .expect("scan observation lock is not poisoned");
        observation.source = Some(Arc::clone(source));
        observation.divergence = None;
    }

    pub(crate) fn published_source(&self) -> Option<Arc<GraphSource>> {
        #[expect(
            clippy::expect_used,
            reason = "a poisoned observation lock means a scan panicked"
        )]
        self.observation
            .lock()
            .expect("the scan observation lock is not poisoned")
            .source
            .clone()
    }

    pub(crate) fn record_failure(&self, reason: String) {
        #[expect(
            clippy::expect_used,
            reason = "a poisoned observation lock means a refresh panicked; no freshness claim can \
                      be recovered after that"
        )]
        let mut observation = self
            .observation
            .lock()
            .expect("the scan observation lock is not poisoned");
        observation.divergence = Some(reason);
    }

    pub(crate) fn lock_operation(&self) -> MutexGuard<'_, ()> {
        #[expect(
            clippy::expect_used,
            reason = "a poisoned operation lock means a scan panicked; continuing could run two \
                      scans over one index"
        )]
        self.operation
            .lock()
            .expect("the scan operation lock is not poisoned")
    }
}

impl Default for ScanState {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) type SharedScanState = Arc<ScanState>;
