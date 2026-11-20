use crate::engine::{EngineError, Graph, Projection, ScanMode};
use crate::source::{self, GraphSource};
use std::path::PathBuf;
use std::sync::Arc;

/// What one [`Graph::refresh_paths`] call did, path by path.
///
/// The counts partition the paths the call was given: `projected + removed + unchanged + absent`
/// equals [`RefreshReport::paths`]. `baseline_refreshed` is a subset of `projected`, not a fourth
/// outcome: a file whose bytes did not change still counts as projected, because the call did read
/// and re-baseline it.
///
/// This is a caller-facing result, not a log line: a watcher uses it to decide whether anything
/// happened, and a test uses it to pin that a refresh touches only the paths it was given.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefreshReport {
    paths: usize,
    projected: usize,
    baseline_refreshed: usize,
    removed: usize,
    unchanged: usize,
    absent: usize,
}

impl RefreshReport {
    /// How many paths the call processed, which is the length of the slice it was given.
    pub fn paths(&self) -> usize {
        self.paths
    }

    /// Paths whose page rows were replaced, or whose baseline alone was refreshed.
    pub fn projected(&self) -> usize {
        self.projected
    }

    /// Of [`Self::projected`], how many only refreshed the baseline because the text was unchanged.
    pub fn baseline_refreshed(&self) -> usize {
        self.baseline_refreshed
    }

    /// Paths whose page rows and baseline were removed.
    pub fn removed(&self) -> usize {
        self.removed
    }

    /// Paths whose stored baseline already matched the observation; nothing was read or written.
    pub fn unchanged(&self) -> usize {
        self.unchanged
    }

    /// Paths that never backed a page and hold no file now, so nothing was done for them.
    pub fn absent(&self) -> usize {
        self.absent
    }
}

impl Graph {
    /// Bring the index up to date for exactly the paths a caller names.
    ///
    /// `paths` are graph-relative paths in the coordinate system [`crate::index::Index::file_baseline`] and
    /// [`crate::index::Index::page_name_for_path`] store, such as `pages/Some page.md` or
    /// `journals/2026_08_13.md` — the paths [`crate::source::PageFile::relative_path`] reports. Each path is
    /// resolved with [`GraphSource::page_at`], which stats that one file and lists no directory, so
    /// a refresh of one file costs one stat instead of the whole-graph stat pass of a
    /// [`crate::engine::Freshness::CheckNow`] scan (ADR-0008).
    ///
    /// What one path does:
    ///
    /// - **A page lives there.** It is handed to `Graph::project_file` in `ScanMode::TrustStat`,
    ///   which is the same comparison a scan makes: an unchanged `(size, mtime)` skips it, unchanged
    ///   text only moves its baseline, and anything else replaces the page rows. A replaced page
    ///   advances the revision exactly once, like a scan's projection does; a skipped or re-baselined
    ///   one does not.
    /// - **No page lives there.** The page rows that pointed at the file (looked up by path, because
    ///   removal is keyed by name) and then its baseline are deleted, in the order the foreign key
    ///   forces, and the revision advances once when a page row existed. A path that never backed a
    ///   page and holds no file now is reported absent and changes nothing.
    ///
    /// **The configuration must not have changed.** `logseq/config.edn` decides which page a file
    /// denotes, so a refresh under a different configuration would project the given paths under
    /// names the rest of the index does not use. This call therefore re-opens the source and compares
    /// it exactly as a full scan does, and answers
    /// [`EngineError::ConfigurationChanged`] instead of projecting when it differs; the caller falls
    /// back to a full scan, which re-derives every page under the new configuration. This is the
    /// precondition of refreshing by path, not a recoverable condition.
    ///
    /// A refresh is not a scan, so a successful call does not clear a divergence a failed scan
    /// recorded and does not claim a new observation time: it observed only the paths it was given
    /// and cannot vouch for the rest of the graph. A failure does record the divergence, because
    /// the paths already processed were committed and the engine no longer knows that the index
    /// matches the files; the next [`crate::engine::Freshness::LastScan`] read then refuses until a scan succeeds.
    ///
    /// Duplicate paths are processed as given: the second occurrence of a path the call already
    /// refreshed reports `unchanged`, because its baseline now matches.
    pub fn refresh_paths(&mut self, paths: &[PathBuf]) -> Result<RefreshReport, EngineError> {
        match self.refresh_paths_inner(paths) {
            Ok(report) => Ok(report),
            Err(error) => {
                // Same rule as a failed scan: the engine no longer knows that the index matches the
                // files, so a read that refuses stale data must refuse until a scan completes. A
                // configuration refusal counts too: the rows were derived under a configuration that
                // no longer holds.
                self.scan_state.record_failure(error.to_string());
                Err(error)
            }
        }
    }

    /// The body of [`Graph::refresh_paths`], so every failure is recorded in one place.
    fn refresh_paths_inner(&mut self, paths: &[PathBuf]) -> Result<RefreshReport, EngineError> {
        let _lock =
            source::WriteLock::acquire_for_scan(&self.write_lock_dir, Some(self.write_lock_wait))?;
        let scan_state = Arc::clone(&self.scan_state);
        let _operation = scan_state.lock_operation();
        self.refresh_index()?;
        // Re-read the configuration exactly as a scan does. A refresh cannot repair a configuration
        // change (it would have to void every derived row first), and projecting the given paths
        // under the new names would leave the rest of the index describing the old ones.
        let source = Arc::new(GraphSource::open(self.source.root())?);
        if source != self.source || !self.index.configuration_matches(source.signature())? {
            return Err(EngineError::ConfigurationChanged);
        }

        let mut report = RefreshReport {
            paths: paths.len(),
            ..RefreshReport::default()
        };
        for path in paths {
            match self.source.page_at(path)? {
                Some(file) => match self.project_file(&file, ScanMode::TrustStat)? {
                    Projection::Unchanged => report.unchanged += 1,
                    Projection::BaselineRefreshed => {
                        report.projected += 1;
                        report.baseline_refreshed += 1;
                    }
                    Projection::Replaced => report.projected += 1,
                },
                None => {
                    let name = self.index.page_name_for_path(path)?;
                    if name.is_none() && self.index.file_baseline(path)?.is_none() {
                        // Nothing was ever projected from this path and nothing is there now.
                        report.absent += 1;
                    } else {
                        self.remove_file_rows(name.as_deref(), path)?;
                        report.removed += 1;
                    }
                }
            }
        }
        Ok(report)
    }
}
