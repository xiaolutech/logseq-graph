use super::scan::ScanObservation;
use crate::domain::{GraphMetadata, Read, ReadMeta};
use crate::engine::{EngineError, Freshness, Graph, ScanMode, ScanProgress};
use crate::index::{Index, IndexError};
use std::sync::Arc;

impl Graph {
    /// Freshness policy used by reads that do not name their own.
    pub fn freshness(&self) -> Freshness {
        self.freshness
    }

    /// How far this handle's scans have got, as a counter a caller may keep and read from another
    /// thread.
    ///
    /// It describes the scan running now: a `CheckNow` read restarts it. A caller that opened the
    /// graph with [`Graph::open_with_progress`] already holds a handle to the same counter.
    pub fn scan_progress(&self) -> Arc<ScanProgress> {
        Arc::clone(&self.progress)
    }

    /// Whether the engine cannot vouch that the index matches the files.
    ///
    /// `true` after a scan this handle ran failed; only a scan that completes clears it.
    pub fn is_stale(&self) -> bool {
        self.scan_state.observation().divergence.is_some()
    }

    fn observation(&self) -> ScanObservation {
        self.scan_state.observation()
    }

    pub(super) fn read_meta(&self, graph: GraphMetadata, stale: bool) -> ReadMeta {
        ReadMeta::new(graph, stale, self.observation().last_scan)
    }

    /// Run one read under the default policy.
    pub(super) fn read<T>(
        &mut self,
        query: impl FnOnce(&Index) -> Result<T, IndexError>,
    ) -> Result<Read<T>, EngineError> {
        self.read_with(self.freshness, query)
    }

    /// Run one read under `freshness`.
    ///
    /// The revision and value are read from one SQLite snapshot, pinned before query execution.
    pub(super) fn read_with<T>(
        &mut self,
        freshness: Freshness,
        query: impl FnOnce(&Index) -> Result<T, IndexError>,
    ) -> Result<Read<T>, EngineError> {
        self.read_answer(freshness, |index, _| Ok(query(index)?))
    }

    pub(super) fn read_answer<T>(
        &mut self,
        freshness: Freshness,
        query: impl FnOnce(&Index, &crate::source::GraphSource) -> Result<T, EngineError>,
    ) -> Result<Read<T>, EngineError> {
        self.settle(freshness)?;
        self.read_settled(freshness, query)
    }

    pub(super) fn read_settled<T>(
        &mut self,
        freshness: Freshness,
        query: impl FnOnce(&Index, &crate::source::GraphSource) -> Result<T, EngineError>,
    ) -> Result<Read<T>, EngineError> {
        let state = Arc::clone(&self.scan_state);
        let operation = state.lock_operation();
        self.refresh_index()?;
        self.index.snapshot(|index| {
            let graph = index.metadata()?;
            let observation = self.observation();
            let stale = observation.divergence.is_some();
            if freshness != Freshness::AllowStale {
                if let Some(reason) = observation.divergence {
                    return Err(EngineError::StaleIndex { reason });
                }
            }
            let meta = ReadMeta::new(graph, stale, observation.last_scan);
            drop(operation);
            Ok(Read::new(query(index, &self.source)?, meta))
        })
    }

    /// Apply one freshness policy and say whether its answer must be marked stale.
    ///
    /// Strict policies use the writable coordinator when this handle owns a read-only connection.
    pub(super) fn settle(&mut self, freshness: Freshness) -> Result<bool, EngineError> {
        if matches!(freshness, Freshness::CheckNow | Freshness::VerifyContent) {
            if let Some(coordinator) = self.read_coordinator.clone() {
                let mut writer = super::pool::lock(&coordinator);
                writer.settle(freshness)?;
                self.refresh_index()?;
                self.source = Arc::clone(&writer.source);
                return Ok(false);
            }
        }
        self.refresh_index()?;
        match freshness {
            Freshness::LastScan => {
                if let Some(reason) = self.observation().divergence {
                    return Err(EngineError::StaleIndex { reason });
                }
                Ok(false)
            }
            Freshness::AllowStale => Ok(self.observation().divergence.is_some()),
            Freshness::CheckNow | Freshness::VerifyContent => {
                // A completed scan applied every change it observed, so the model matches the files
                // as they are now; a failed one returned above and never reaches this line. The two
                // policies share this path and differ only in how much they trust the stat.
                let mode = if freshness == Freshness::VerifyContent {
                    ScanMode::ReadEveryFile
                } else {
                    ScanMode::TrustStat
                };
                self.scan(mode)?;
                Ok(false)
            }
        }
    }
}
