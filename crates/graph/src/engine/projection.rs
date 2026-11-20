use crate::document::PageDocument;
use crate::engine::{EngineError, Graph};
use crate::index::FileBaseline;
use crate::source::{self, FileMetadata, PageFile};
use std::path::Path;
use std::time::SystemTime;

/// How a scan decides whether a file has to be read.
///
/// This is the only thing the scanning freshness policies disagree on, so it travels as one private
/// value from the read down to [`Graph::project_file`] rather than as a `bool` that no call site could
/// name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScanMode {
    /// A matching `(size, mtime)` baseline is enough to skip the file.
    TrustStat,
    /// Read and fingerprint every file; a matching stat proves nothing.
    ReadEveryFile,
}

/// What one [`Graph::project_file`] call did to the index.
///
/// Only a caller that reports its work needs the difference: [`Graph::refresh_paths`] has to say how
/// many paths it replaced, how many it only re-baselined, and how many it left alone, and the answer
/// already exists inside the projection. Re-deriving it outside would repeat the baseline comparison
/// that decides it, which is the one thing a second copy must not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Projection {
    /// The stored `(size, mtime)` matched the observation; nothing was read or written.
    Unchanged,
    /// The file was read and its text was unchanged, so only its baseline moved.
    BaselineRefreshed,
    /// The page rows were replaced, which advanced the revision once.
    Replaced,
}

impl Graph {
    /// Scan every graph file now, trusting a matching size and modification time.
    ///
    /// Unlike a read under [`crate::engine::Freshness::CheckNow`], this performs no query. A polling loop uses it to
    /// make external edits visible to subsequent default reads. A failure is retained in the shared
    /// scan state until a later scan succeeds.
    pub fn rescan(&mut self) -> Result<(), EngineError> {
        self.scan(ScanMode::TrustStat)
    }

    /// Bring the index up to the files, then record the outcome.
    ///
    /// A completed scan clears the divergence record and moves the observation time; a failed scan
    /// leaves the record behind, which is what makes the next [`crate::engine::Freshness::LastScan`] read refuse and
    /// the next [`crate::engine::Freshness::AllowStale`] read mark its result stale.
    pub(super) fn scan(&mut self, mode: ScanMode) -> Result<(), EngineError> {
        let started = SystemTime::now();
        let result = self.scan_prepared(mode, started);
        if let Err(error) = &result {
            self.scan_state
                .scan_finished(started, Some(error.to_string()), None);
        }
        result
    }

    /// Delete the derived rows one file leaves behind: the page rows first, then the baseline.
    ///
    /// The order is not a convention here but a foreign key: `pages.relative_path` references
    /// `file_baseline.relative_path` without `ON DELETE`, so a baseline a page still points at
    /// cannot be removed. `name` is the page row that backs this file, and `None` when no page was
    /// projected from it — a baseline left behind by an interrupted removal, which this still
    /// clears. Removing a page advances the revision once; removing a baseline does not.
    ///
    /// Bounded path refreshes use this. Prepared full scans batch
    /// the same ordering in the index scan transaction.
    pub(super) fn remove_file_rows(
        &mut self,
        name: Option<&str>,
        relative_path: &Path,
    ) -> Result<(), EngineError> {
        if let Some(name) = name {
            self.index.remove_page(name)?;
        }
        self.index.remove_file_baseline(relative_path)?;
        Ok(())
    }

    /// Project one discovered file, or skip it when the index already holds its current content.
    ///
    /// A page is replaced as a whole, one transaction per page, which is the only granularity the
    /// index offers (ADR-0004). Each replacement advances the revision once, so a cold start
    /// advances it once per projected page. The answer says which of the three outcomes happened, so
    /// a caller that reports its work does not have to repeat the baseline comparison that decided
    /// it.
    pub(super) fn project_file(
        &mut self,
        file: &PageFile,
        mode: ScanMode,
    ) -> Result<Projection, EngineError> {
        // The observation recorded for this projection is the one discovery took, before the file
        // was read. That order is what makes the baseline safe: see [`baseline_for`].
        let observed = file.metadata();
        let baseline = self.index.file_baseline(file.relative_path())?;
        if mode == ScanMode::TrustStat
            && baseline.is_some_and(|stored| stored.metadata() == observed)
        {
            // Same size and modification time as the observation that produced the stored rows.
            // The stat is a hint rather than a guarantee (a restored mtime looks unchanged), and
            // that limit is what `Freshness` expresses; re-reading every file to prove it would
            // remove the reason to keep a baseline at all. [`ScanMode::ReadEveryFile`] is the caller
            // that pays that cost instead.
            return Ok(Projection::Unchanged);
        }

        let text = file.read_text()?;
        let recorded = baseline_for(observed, &text);

        if let Some(baseline) = baseline {
            if baseline.content_hash() == recorded.content_hash() {
                // The file was touched but its text is unchanged, so no derived fact changed.
                // Refreshing the baseline alone keeps the revision meaningful. The page row keeps
                // the kind the file's path already decided, so there is nothing else to compare:
                // one path cannot change directory without becoming a different path.
                self.index
                    .record_file_baseline(file.relative_path(), recorded)?;
                return Ok(Projection::BaselineRefreshed);
            }
        }

        // Exactly one parse per file: the fingerprint comes from the text, never from the document,
        // so the projection is the only consumer of the parsed page.
        let document = PageDocument::parse(&text);
        self.index.replace_page_with_baseline(
            file.relative_path(),
            file.name(),
            file.journal_date(),
            &document,
            recorded,
        )?;
        Ok(Projection::Replaced)
    }
}

/// Baseline to record for `text`, which was read from a file observed at `observed`.
///
/// The recorded observation is the one taken before the read, and that direction is the whole point:
/// a baseline may be too old, which costs one extra projection on the next start, but it must never
/// be newer than the text it describes. A stat taken after the read can pair a newer modification
/// time with the older text that was actually parsed when a writer lands in between, and the next
/// start would then match the baseline, skip the file, and keep the stale page until the file moves
/// again.
///
/// Keeping the decision in one function keeps it testable without a concurrent writer: the seam is
/// the pair of observations, not the clock.
pub(super) fn baseline_for(observed: FileMetadata, text: &str) -> FileBaseline {
    FileBaseline::new(observed, source::content_fingerprint(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::{Duration, UNIX_EPOCH};

    fn observed(seconds: u64) -> FileMetadata {
        FileMetadata::from_stored(7, Some(UNIX_EPOCH + Duration::from_secs(seconds)))
    }

    #[test]
    fn a_recorded_baseline_keeps_the_observation_taken_before_the_read() {
        // The pre-read observation is older than the post-read one, which is what a writer landing
        // between the read and the stat would produce. Recording the newer one would let the next
        // start skip text that never reached the index, so only the older one may be stored.
        let before_read = observed(1_700_000_000);
        let after_read = observed(1_700_000_100);
        assert_ne!(before_read, after_read);

        let baseline = baseline_for(before_read, "- a\n");

        assert_eq!(baseline.metadata(), before_read);
        assert_ne!(baseline.metadata(), after_read);
        assert_eq!(
            baseline.content_hash(),
            source::content_fingerprint("- a\n"),
            "the fingerprint must come from the text that was parsed"
        );
    }
}
