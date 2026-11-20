//! How far a scan has got, readable from another thread while it runs.
//!
//! A cold projection is the most expensive thing this crate does — 9.5 s on the reference graph, one
//! to two orders of magnitude more than any query it serves (ADR-0008) — and a caller that has to
//! answer a person before it finishes needs to say how far it is. The counter is two atomics rather
//! than a caller-supplied callback: a callback runs caller code inside the scan, while a counter
//! lets a second thread read the same numbers without caller code running during the scan.

use std::sync::atomic::{AtomicU64, Ordering};

/// Files prepared, out of files discovery found, for the scan that is running now.
///
/// The numbers describe a scan, not the index: a second scan restarts them, so they never accumulate
/// across scans. A caller reads them with [`snapshot`](Self::snapshot) and does nothing else with
/// them; only the engine writes them.
#[derive(Debug, Default)]
pub struct ScanProgress {
    /// Files prepared since the scan in progress started.
    scanned: AtomicU64,
    /// Files the scan in progress will prepare, `0` until discovery returns.
    total: AtomicU64,
}

impl ScanProgress {
    /// A counter that has not seen a scan begin, so it reads `(0, 0)`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Files prepared and files this scan will prepare, as one pair.
    ///
    /// `scanned` is loaded first and `total` second: the total lands once, before the first file is
    /// counted, so a reader that sees a count of at least one is guaranteed to see that scan's
    /// denominator too and cannot report "5 of 0". The reverse order has no such guarantee, which is
    /// why there is no way to read the two halves separately.
    pub fn snapshot(&self) -> (u64, u64) {
        let scanned = self.scanned.load(Ordering::Acquire);
        let total = self.total.load(Ordering::Acquire);
        (scanned, total)
    }

    /// Start reporting a scan of `total` files, from zero.
    ///
    /// A scan that fails leaves the counters where it stopped: they report the work the failed scan
    /// did rather than pretending it completed, and the read that refused the failure says why.
    pub(crate) fn begin(&self, total: usize) {
        self.scanned.store(0, Ordering::Release);
        // The store is `Release` so that the total is visible to any thread that observes a counted
        // file: the counting store below is also a release.
        self.total.store(total as u64, Ordering::Release);
    }

    /// Count one file the scan looked at, whether it was projected, skipped, or re-baselined.
    pub(crate) fn advance(&self) {
        self.scanned.fetch_add(1, Ordering::AcqRel);
    }
}
