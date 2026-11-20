//! Shadow preparation and catch-up run unlocked; publication requires a zero-delta verification.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::document::PageDocument;
use crate::index::{FileBaseline, PreparedPage, ScanPage, Shadow};
use crate::source::{GraphSource, PageFile, WriteLock};

use super::{baseline_for, EngineError, Graph, ScanMode};

const SHADOW_CATCHUP_ROUNDS: usize = 8;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CatchupStage {
    Applying,
    Verifying,
}

#[cfg(test)]
pub(super) type CatchupHook =
    std::sync::mpsc::Sender<(usize, CatchupStage, std::sync::mpsc::Sender<()>)>;

struct ShadowDelta {
    changed: Vec<PageFile>,
    removed: Vec<std::path::PathBuf>,
}

impl ShadowDelta {
    fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
    }
}

#[derive(Debug)]
struct PreparedFile {
    file: PageFile,
    baseline: FileBaseline,
    projection: Option<PreparedPage>,
}

#[derive(Debug)]
struct PreparedScan {
    source: Arc<GraphSource>,
    files: Vec<PreparedFile>,
    full: bool,
}

impl Graph {
    pub(super) fn scan_prepared(
        &mut self,
        mode: ScanMode,
        started: std::time::SystemTime,
    ) -> Result<(), EngineError> {
        let mut force = false;
        let mut remaining = SHADOW_CATCHUP_ROUNDS;
        loop {
            let mut prepared = self.prepare_scan(mode, force)?;
            let shadow = if prepared.full {
                Some(self.build_shadow(&mut prepared)?)
            } else {
                None
            };
            #[cfg(test)]
            if let Some((prepared, resume)) = self.test_scan_prepare.take() {
                prepared.send(()).expect("scan test is waiting");
                resume.recv().expect("scan test resumes preparation");
            }
            if let Some(shadow) = shadow {
                if self.publish_shadow(prepared, shadow, &mut remaining, started)? {
                    return Ok(());
                }
                force = true;
                continue;
            }
            let lock =
                WriteLock::acquire_for_scan(&self.write_lock_dir, Some(self.write_lock_wait))?;
            let state = Arc::clone(&self.scan_state);
            let operation = state.lock_operation();
            self.refresh_index()?;
            let current = GraphSource::open(self.source.root())?;
            if current.signature() != prepared.source.signature() {
                drop(operation);
                drop(lock);
                continue;
            }
            let committed = self.commit_prepared(prepared, mode)?;
            if committed {
                state.scan_finished(started, None, Some(&self.source));
                return Ok(());
            }
            drop(operation);
            drop(lock);
            force = true;
        }
    }

    fn prepare_scan(&mut self, mode: ScanMode, force: bool) -> Result<PreparedScan, EngineError> {
        self.refresh_index()?;
        let source = Arc::new(GraphSource::open(self.source.root())?);
        let files = source.discover()?;
        let stored = self.index.file_baseline_paths()?;
        let present: HashSet<_> = files.iter().map(PageFile::relative_path).collect();
        let mut changes = stored
            .iter()
            .filter(|path| !present.contains(path.as_path()))
            .count();
        for file in &files {
            if self
                .index
                .file_baseline(file.relative_path())?
                .is_none_or(|b| b.metadata() != file.metadata())
            {
                changes += 1;
            }
        }
        let full = force
            || mode == ScanMode::ReadEveryFile
            || changes > 64
            || (stored.is_empty() && !files.is_empty())
            || !self.index.configuration_matches(source.signature())?;
        self.progress.begin(files.len());
        let mut prepared = Vec::with_capacity(files.len());
        for file in files {
            prepared.push(self.prepare_scan_file(file, mode, full)?);
            self.progress.advance();
        }
        Ok(PreparedScan {
            source,
            files: prepared,
            full,
        })
    }

    fn prepare_scan_file(
        &self,
        file: PageFile,
        mode: ScanMode,
        full: bool,
    ) -> Result<PreparedFile, EngineError> {
        let stored = self.index.file_baseline(file.relative_path())?;
        if let Some(baseline) = stored
            .filter(|b| !full && mode == ScanMode::TrustStat && b.metadata() == file.metadata())
        {
            return Ok(PreparedFile {
                file,
                baseline,
                projection: None,
            });
        }
        let text = file.read_text()?;
        let baseline = baseline_for(file.metadata(), &text);
        let projection =
            if full || stored.is_none_or(|b| b.content_hash() != baseline.content_hash()) {
                Some(PreparedPage::project(&PageDocument::parse(&text))?)
            } else {
                None
            };
        Ok(PreparedFile {
            file,
            baseline,
            projection,
        })
    }

    pub(super) fn refresh_index(&mut self) -> Result<(), EngineError> {
        if self.index.reopen_if_published()? {
            self.source = Arc::new(GraphSource::open(self.source.root())?);
            if self.read_coordinator.is_none()
                && self.index.configuration_matches(self.source.signature())?
            {
                self.scan_state.index_reopened(&self.source);
            }
        }
        Ok(())
    }

    fn build_shadow(&mut self, prepared: &mut PreparedScan) -> Result<Shadow, EngineError> {
        let mut shadow = self.index.shadow()?;
        let changed = !shadow
            .database()?
            .configuration_matches(prepared.source.signature())?;
        if changed {
            shadow.database()?.snapshot_shadow_identities()?;
        }
        let present: HashSet<_> = prepared
            .files
            .iter()
            .map(|p| p.file.relative_path().to_owned())
            .collect();
        let removed: Vec<_> = shadow
            .database()?
            .file_baseline_paths()?
            .into_iter()
            .filter(|p| !present.contains(p))
            .collect();
        let pages: Vec<_> = prepared
            .files
            .iter_mut()
            .map(|p| ScanPage {
                path: p.file.relative_path().to_owned(),
                name: p.file.name().to_owned(),
                date: p.file.journal_date(),
                baseline: p.baseline,
                projection: p.projection.take(),
            })
            .collect();
        #[cfg(test)]
        if let Some((ready, resume)) = self.test_shadow_build.take() {
            ready.send(()).expect("shadow test waiting");
            resume.recv().expect("shadow test resumes");
        }
        shadow
            .database()?
            .commit_scan("", changed, &removed, &pages)?;
        if changed {
            shadow.database()?.restore_shadow_identities()?;
        }
        shadow.sync()?;
        Ok(shadow)
    }

    fn publish_shadow(
        &mut self,
        prepared: PreparedScan,
        mut shadow: Shadow,
        remaining: &mut usize,
        started: std::time::SystemTime,
    ) -> Result<bool, EngineError> {
        let mut observed: HashMap<_, _> = prepared
            .files
            .iter()
            .map(|p| (p.file.relative_path().to_owned(), p.baseline))
            .collect();
        let mut touched = HashSet::new();
        while *remaining > 0 {
            *remaining -= 1;
            self.refresh_index()?;
            let live = self.index.metadata()?;
            if live.instance_id() != shadow.database()?.metadata()?.instance_id() {
                return Ok(false);
            }
            if GraphSource::open(self.source.root())?.signature() != prepared.source.signature() {
                return Ok(false);
            }
            let delta = Self::shadow_delta(&prepared.source, &observed)?;
            #[cfg(test)]
            self.pause_shadow_catchup(*remaining, CatchupStage::Applying);
            let mut updates = Vec::with_capacity(delta.changed.len());
            for file in delta.changed {
                let file = self.prepare_scan_file(file, ScanMode::ReadEveryFile, true)?;
                updates.push(ScanPage {
                    path: file.file.relative_path().to_owned(),
                    name: file.file.name().to_owned(),
                    date: file.file.journal_date(),
                    baseline: file.baseline,
                    projection: file.projection,
                });
            }
            if !delta.removed.is_empty() || !updates.is_empty() {
                shadow
                    .database()?
                    .commit_scan("", false, &delta.removed, &updates)?;
                for path in delta.removed {
                    observed.remove(&path);
                }
                for update in updates {
                    touched.insert(update.path.clone());
                    observed.insert(update.path, update.baseline);
                }
            }
            for path in &touched {
                shadow.database()?.preserve_identities(&self.index, path)?;
            }
            shadow
                .database()?
                .finish_shadow(&self.index, prepared.source.signature())?;
            shadow.sync()?;
            #[cfg(test)]
            self.pause_shadow_catchup(*remaining, CatchupStage::Verifying);
            let lock =
                WriteLock::acquire_for_scan(&self.write_lock_dir, Some(self.write_lock_wait))?;
            let state = Arc::clone(&self.scan_state);
            let operation = state.lock_operation();
            self.refresh_index()?;
            let latest = self.index.metadata()?;
            if latest.instance_id() != live.instance_id() || latest.revision() != live.revision() {
                continue;
            }
            if GraphSource::open(self.source.root())?.signature() != prepared.source.signature() {
                return Ok(false);
            }
            if !Self::shadow_delta(&prepared.source, &observed)?.is_empty() {
                drop(operation);
                drop(lock);
                continue;
            }
            self.index = shadow.publish()?;
            self.source = prepared.source;
            state.scan_finished(started, None, Some(&self.source));
            return Ok(true);
        }
        Err(EngineError::StaleIndex {
            reason: format!(
                "shadow reprojection did not stabilize within {SHADOW_CATCHUP_ROUNDS} catch-up \
                 rounds; the previous generation was retained; retry the scan"
            ),
        })
    }

    #[cfg(test)]
    fn pause_shadow_catchup(&self, remaining: usize, stage: CatchupStage) {
        if let Some(hook) = &self.test_shadow_catchup {
            let (resume, paused) = std::sync::mpsc::channel();
            hook.send((SHADOW_CATCHUP_ROUNDS - remaining, stage, resume))
                .expect("catch-up test waiting");
            paused.recv().expect("catch-up test resumes");
        }
    }

    fn shadow_delta(
        source: &GraphSource,
        observed: &HashMap<std::path::PathBuf, FileBaseline>,
    ) -> Result<ShadowDelta, EngineError> {
        let files = source.discover()?;
        let present: HashSet<_> = files.iter().map(|p| p.relative_path().to_owned()).collect();
        let removed: Vec<_> = observed
            .keys()
            .filter(|p| !present.contains(*p))
            .cloned()
            .collect();
        let changed = files
            .into_iter()
            .filter(|file| {
                observed
                    .get(file.relative_path())
                    .is_none_or(|b| b.metadata() != file.metadata())
            })
            .collect();
        Ok(ShadowDelta { changed, removed })
    }

    fn commit_prepared(
        &mut self,
        mut prepared: PreparedScan,
        mode: ScanMode,
    ) -> Result<bool, EngineError> {
        let changed = !self
            .index
            .configuration_matches(prepared.source.signature())?;
        if changed && !prepared.full {
            return Ok(false);
        }
        let present: HashSet<_> = prepared
            .files
            .iter()
            .map(|p| p.file.relative_path().to_owned())
            .collect();
        let mut removed = Vec::new();
        for path in self.index.file_baseline_paths()? {
            if !present.contains(&path) {
                match prepared.source.page_at(&path)? {
                    Some(file) => prepared
                        .files
                        .push(self.prepare_scan_file(file, mode, changed)?),
                    None => removed.push(path),
                }
            }
        }
        let mut updates = Vec::new();
        for mut file in prepared.files {
            let Some(fresh) = prepared.source.page_at(file.file.relative_path())? else {
                removed.push(file.file.relative_path().to_owned());
                continue;
            };
            if fresh.metadata() != file.file.metadata() {
                file = self.prepare_scan_file(fresh, mode, changed)?;
            }
            let stored = self.index.file_baseline(file.file.relative_path())?;
            if !changed
                && mode == ScanMode::TrustStat
                && stored.is_some_and(|b| b.metadata() == file.file.metadata())
            {
                continue;
            }
            if !changed && stored.is_some_and(|b| b.content_hash() == file.baseline.content_hash())
            {
                file.projection = None;
            } else if file.projection.is_none() {
                return Ok(false);
            }
            updates.push(ScanPage {
                path: file.file.relative_path().to_owned(),
                name: file.file.name().to_owned(),
                date: file.file.journal_date(),
                baseline: file.baseline,
                projection: file.projection,
            });
        }
        self.index
            .commit_scan(prepared.source.signature(), changed, &removed, &updates)?;
        self.source = prepared.source;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use crate::engine::OpenOptions;

    struct Sandbox(PathBuf);

    impl Sandbox {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "graph-prepared-scan-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            fs::create_dir_all(path.join("pages")).expect("pages");
            fs::create_dir_all(path.join("logseq")).expect("config directory");
            fs::write(path.join("pages/alpha.md"), "- original\n").expect("page");
            Self(path)
        }

        fn open(&self) -> Graph {
            Graph::open(OpenOptions::new(&self.0, self.0.join("index.db"))).expect("graph")
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn bulk_and_late_changes_catch_up_unlocked_before_a_zero_delta_publication() {
        let fixture = Sandbox::new();
        for i in 0..256 {
            fs::write(fixture.0.join(format!("pages/bulk-{i}.md")), "- before\n")
                .expect("bulk page");
        }
        fs::write(fixture.0.join("pages/removed.md"), "- removed later\n").expect("removed page");
        let mut scanner = fixture.open();
        let mut writer = fixture.open();
        let mut reader = fixture.open();
        let (built, building) = mpsc::channel();
        let (resume, continued) = mpsc::channel();
        scanner.test_shadow_build = Some((built, continued));
        let (hook, phases) = mpsc::channel();
        scanner.test_shadow_catchup = Some(hook);
        let scan = std::thread::spawn(move || {
            scanner
                .reads(super::super::Freshness::VerifyContent)
                .metadata()
                .expect("full scan");
            scanner
        });
        building
            .recv_timeout(Duration::from_secs(10))
            .expect("shadow building");
        for i in 0..256 {
            fs::write(
                fixture.0.join(format!("pages/bulk-{i}.md")),
                "- before\n- changed during build\n",
            )
            .expect("change during build");
        }
        resume.send(()).expect("resume build");
        let mut selector = None;
        let mut writer_revision = 0;
        for (round, stage) in [
            (1, CatchupStage::Applying),
            (1, CatchupStage::Verifying),
            (2, CatchupStage::Applying),
            (2, CatchupStage::Verifying),
        ] {
            let (actual_round, actual_stage, resume) = phases
                .recv_timeout(Duration::from_secs(10))
                .expect("catch-up phase");
            assert_eq!((actual_round, actual_stage), (round, stage));
            let lock = WriteLock::acquire_for_scan(
                &scanner_lock_directory(&fixture),
                Some(Duration::ZERO),
            )
            .expect("catch-up does not hold the graph lock");
            drop(lock);
            assert_eq!(
                reader
                    .page_metadata("bulk-0")
                    .expect("old generation")
                    .present()
                    .expect("bulk page")
                    .block_count(),
                1
            );
            if (round, stage) == (1, CatchupStage::Applying) {
                writer
                    .append_block("alpha", "writer during catch-up")
                    .expect("concurrent writer completes");
                selector = Some(
                    writer
                        .index
                        .page_blocks("alpha")
                        .expect("blocks")
                        .expect("alpha")[1]
                        .selector()
                        .clone(),
                );
                writer_revision = writer.index.metadata().expect("metadata").revision();
            }
            if (round, stage) == (1, CatchupStage::Verifying) {
                for i in 0..256 {
                    fs::write(
                        fixture.0.join(format!("pages/bulk-{i}.md")),
                        "- before\n- changed during build\n- changed after catch-up\n",
                    )
                    .expect("late changes require another unlocked round");
                }
                fs::remove_file(fixture.0.join("pages/removed.md")).expect("removed");
                fs::write(fixture.0.join("pages/added.md"), "- added\n").expect("added");
            }
            resume.send(()).expect("resume catch-up");
        }
        let mut scanner = scan.join().expect("scan thread");
        assert!(scanner.index.metadata().expect("metadata").revision() > writer_revision);
        for i in 0..256 {
            assert_eq!(
                scanner
                    .page_metadata(&format!("bulk-{i}"))
                    .expect("page")
                    .present()
                    .expect("bulk")
                    .block_count(),
                3
            );
        }
        assert!(scanner
            .page_metadata("removed")
            .expect("removed")
            .present()
            .is_none());
        assert!(scanner
            .page_metadata("added")
            .expect("added")
            .present()
            .is_some());
        writer
            .update_block(
                &selector.expect("writer selector"),
                "selector survived catch-up",
            )
            .expect("concurrent writer identities survive publication");
        assert!(fs::read_to_string(fixture.0.join("pages/alpha.md"))
            .expect("alpha")
            .contains("selector survived catch-up"));
    }

    fn scanner_lock_directory(fixture: &Sandbox) -> PathBuf {
        fixture.0.canonicalize().expect("graph root")
    }

    #[test]
    fn a_live_revision_change_without_file_changes_retries_unlocked_finalization() {
        let fixture = Sandbox::new();
        let mut scanner = fixture.open();
        let mut writer = fixture.open();
        let (hook, phases) = mpsc::channel();
        scanner.test_shadow_catchup = Some(hook);
        let scan = std::thread::spawn(move || {
            scanner
                .reads(super::super::Freshness::VerifyContent)
                .metadata()
                .expect("full scan");
            scanner
        });
        let mut writer_revision = 0;
        for round in 1..=2 {
            for stage in [CatchupStage::Applying, CatchupStage::Verifying] {
                let (actual_round, actual_stage, resume) = phases
                    .recv_timeout(Duration::from_secs(10))
                    .expect("finalization phase");
                assert_eq!((actual_round, actual_stage), (round, stage));
                if (round, stage) == (1, CatchupStage::Verifying) {
                    let lock = WriteLock::acquire_for_scan(
                        &scanner_lock_directory(&fixture),
                        Some(Duration::ZERO),
                    )
                    .expect("unlocked finalization");
                    let path = std::path::Path::new("pages/alpha.md");
                    let baseline = writer
                        .index
                        .file_baseline(path)
                        .expect("baseline")
                        .expect("alpha");
                    for _ in 0..4 {
                        writer_revision = writer
                            .index
                            .replace_page_with_baseline(
                                path,
                                "alpha",
                                None,
                                &PageDocument::parse("- original\n"),
                                baseline,
                            )
                            .expect("same bytes, higher live revision")
                            .revision();
                    }
                    drop(lock);
                }
                resume.send(()).expect("resume finalization");
            }
        }
        let scanner = scan.join().expect("scan thread");
        assert!(scanner.index.metadata().expect("metadata").revision() > writer_revision);
    }

    #[test]
    fn continuous_changes_exhaust_catch_up_without_publishing_or_holding_the_lock() {
        let fixture = Sandbox::new();
        let mut scanner = fixture.open();
        let original_path = scanner.index.storage_path().expect("storage").to_owned();
        let original_revision = scanner.index.metadata().expect("metadata").revision();
        let (hook, phases) = mpsc::channel();
        scanner.test_shadow_catchup = Some(hook);
        let scan = std::thread::spawn(move || {
            let result = scanner
                .reads(super::super::Freshness::VerifyContent)
                .metadata();
            (scanner, result)
        });
        for round in 1..=SHADOW_CATCHUP_ROUNDS {
            for stage in [CatchupStage::Applying, CatchupStage::Verifying] {
                let (actual_round, actual_stage, resume) = phases
                    .recv_timeout(Duration::from_secs(10))
                    .expect("bounded round");
                assert_eq!((actual_round, actual_stage), (round, stage));
                if stage == CatchupStage::Verifying {
                    fs::write(
                        fixture.0.join("pages/alpha.md"),
                        format!("- {}\n", "keeps changing ".repeat(round)),
                    )
                    .expect("never stable");
                }
                resume.send(()).expect("resume");
            }
        }
        let (mut scanner, result) = scan.join().expect("scan thread");
        assert!(
            matches!(result, Err(EngineError::StaleIndex { reason }) if reason.contains("8 catch-up rounds"))
        );
        assert_eq!(scanner.index.storage_path(), Some(original_path.as_path()));
        assert_eq!(
            scanner.index.metadata().expect("metadata").revision(),
            original_revision
        );
        assert_eq!(
            scanner
                .index
                .page_blocks("alpha")
                .expect("blocks")
                .expect("alpha")
                .len(),
            1
        );
        assert!(
            scanner.metadata().is_err(),
            "failed scan refuses default reads"
        );
        let lock =
            WriteLock::acquire_for_scan(&scanner_lock_directory(&fixture), Some(Duration::ZERO))
                .expect("failure releases the graph lock");
        drop(lock);
        scanner.test_shadow_catchup = None;
        scanner.rescan().expect("retry succeeds after edits stop");
    }

    #[test]
    fn writes_complete_during_prepare_and_are_reverified_before_publication() {
        for config in [false, true] {
            let fixture = Sandbox::new();
            fs::write(fixture.0.join("pages/beta.md"), "- removed later\n").expect("beta");
            let mut scanner = fixture.open();
            let mut writer = fixture.open();
            fs::write(fixture.0.join("pages/alpha.md"), "- edited before scan\n")
                .expect("outside edit");
            if config {
                fs::write(
                    fixture.0.join("logseq/config.edn"),
                    "{:journal/page-title-format \"yyyy-MM-dd\"}",
                )
                .expect("configuration change");
            }
            let (ready, prepared) = mpsc::channel();
            let (resume, continued) = mpsc::channel();
            scanner.test_scan_prepare = Some((ready, continued));
            let scan = std::thread::spawn(move || {
                scanner.rescan().expect("scan");
                scanner
            });
            prepared
                .recv_timeout(Duration::from_secs(5))
                .expect("prepare reached without graph lock");
            writer
                .append_block("alpha", "written during prepare")
                .expect("write must finish before scan resumes");
            writer
                .create_page("new page")
                .expect("new indexed path survives sweep");
            let alpha = fixture.0.join("pages/alpha.md");
            let mut text = fs::read_to_string(&alpha).expect("written page");
            text.push_str("- external edit after prepare\n");
            fs::write(alpha, text).expect("changed file needs reparse before publication");
            fs::remove_file(fixture.0.join("pages/beta.md"))
                .expect("file disappears after prepare");
            if config {
                fs::write(
                    fixture.0.join("logseq/config.edn"),
                    "{:journal/page-title-format \"yyyy/MM/dd\"}",
                )
                .expect("configuration changes again");
            }
            resume.send(()).expect("resume");
            let mut scanner = scan.join().expect("scanner thread");
            assert_eq!(
                scanner
                    .page_metadata("alpha")
                    .expect("metadata")
                    .present()
                    .expect("alpha")
                    .block_count(),
                3
            );
            assert!(scanner
                .page_metadata("new page")
                .expect("metadata")
                .present()
                .is_some());
            assert!(scanner
                .page_metadata("beta")
                .expect("metadata")
                .present()
                .is_none());
            let current = GraphSource::open(&fixture.0).expect("current configuration");
            assert!(scanner
                .index
                .configuration_matches(current.signature())
                .expect("signature"));
        }
    }

    #[test]
    fn publication_repairs_another_handles_failed_generation() {
        let fixture = Sandbox::new();
        let mut scanner = fixture.open();
        let mut reader = fixture.open();
        reader.scan_state.scan_finished(
            SystemTime::now(),
            Some("old generation failed".to_owned()),
            None,
        );
        assert!(reader.metadata().is_err());
        scanner
            .reads(crate::engine::Freshness::VerifyContent)
            .metadata()
            .expect("publish");
        assert!(reader.metadata().is_ok());
    }

    #[test]
    fn durable_selector_fallback_builds_without_excluding_another_write() {
        let fixture = Sandbox::new();
        let mut scanner = fixture.open();
        let mut writer = fixture.open();
        fs::write(
            fixture.0.join("pages/late.md"),
            "- target\n  id:: 11111111-2222-4333-8444-555555555555\n",
        )
        .expect("unindexed durable block");
        let selector = crate::domain::BlockSelector::Durable(
            "11111111-2222-4333-8444-555555555555".parse().expect("id"),
        );
        let (ready, building) = mpsc::channel();
        let (resume, continued) = mpsc::channel();
        scanner.test_shadow_build = Some((ready, continued));
        let scan = std::thread::spawn(move || {
            scanner
                .update_block(&selector, "found through shadow fallback")
                .expect("write durable target");
            scanner
        });
        building
            .recv_timeout(Duration::from_secs(5))
            .expect("fallback building outside lock");
        writer
            .append_block("alpha", "concurrent writer")
            .expect("fallback releases lock");
        resume.send(()).expect("resume");
        let mut scanner = scan.join().expect("fallback");
        assert_eq!(
            scanner
                .page_metadata("alpha")
                .expect("page")
                .present()
                .expect("alpha")
                .block_count(),
            2
        );
        assert!(fs::read_to_string(fixture.0.join("pages/late.md"))
            .expect("file")
            .contains("found through shadow fallback"));
    }

    #[test]
    fn configuration_reprojection_preserves_unchanged_byte_selectors() {
        let fixture = Sandbox::new();
        let mut scanner = fixture.open();
        let mut writer = fixture.open();
        let selector = writer
            .index
            .page_blocks("alpha")
            .expect("blocks")
            .expect("alpha")[0]
            .selector()
            .clone();
        fs::write(
            fixture.0.join("logseq/config.edn"),
            "{:journal/page-title-format \"yyyy-MM-dd\"}",
        )
        .expect("config");
        scanner.rescan().expect("configuration shadow publication");
        writer
            .update_block(
                &selector,
                "unchanged-byte identity survives configuration publication",
            )
            .expect("selector survives");
    }

    #[test]
    fn readers_racing_publication_only_see_complete_generations() {
        let fixture = Sandbox::new();
        let mut scanner = fixture.open();
        let mut reader = fixture.open();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finished = Arc::clone(&stop);
        let reading = std::thread::spawn(move || {
            let mut previous = 0;
            while !finished.load(std::sync::atomic::Ordering::Acquire) {
                let page = reader.page_metadata("alpha").expect("concurrent reader");
                assert!(page.meta().revision() >= previous);
                previous = page.meta().revision();
                assert_eq!(page.present().expect("complete page").block_count(), 1);
            }
        });
        for _ in 0..8 {
            scanner.scan(ScanMode::ReadEveryFile).expect("publish");
        }
        stop.store(true, std::sync::atomic::Ordering::Release);
        reading.join().expect("reader");
    }

    #[test]
    fn shadow_build_keeps_readers_consistent_and_reopens_every_handle_after_publication() {
        let fixture = Sandbox::new();
        let mut scanner = fixture.open();
        let mut reader = fixture.open();
        let mut writer = fixture.open();
        let mut idle_writer = fixture.open();
        let before = reader.metadata().expect("metadata").into_value();
        let original_file = reader.index.storage_path().expect("file").to_owned();
        let (ready, building) = mpsc::channel();
        let (resume, continued) = mpsc::channel();
        scanner.test_shadow_build = Some((ready, continued));
        let scan = std::thread::spawn(move || {
            scanner.scan(ScanMode::ReadEveryFile).expect("shadow scan");
            scanner
        });
        building
            .recv_timeout(Duration::from_secs(5))
            .expect("shadow built snapshot outside lock");
        assert_eq!(
            reader
                .page_metadata("alpha")
                .expect("old consistent page")
                .present()
                .expect("alpha")
                .block_count(),
            1
        );
        assert_eq!(reader.index.storage_path(), Some(original_file.as_path()));
        writer
            .append_block("alpha", "concurrent write")
            .expect("write while building");
        writer
            .create_page("added during build")
            .expect("added file");
        fs::write(
            fixture.0.join("pages/external.md"),
            "- added without index\n",
        )
        .expect("outside file");
        let written = writer.metadata().expect("writer revision").into_value();
        let base = writer
            .page_snapshot("alpha")
            .expect("snapshot")
            .present()
            .expect("alpha")
            .version()
            .clone();
        let selector = writer
            .index
            .page_blocks("alpha")
            .expect("blocks")
            .expect("alpha")[1]
            .selector()
            .clone();
        resume.send(()).expect("continue");
        let mut scanner = scan.join().expect("scanner");
        let published = reader.metadata().expect("reader reopens").into_value();
        assert_eq!(published.instance_id(), before.instance_id());
        assert!(published.revision() > written.revision());
        assert_ne!(reader.index.storage_path(), Some(original_file.as_path()));
        assert_eq!(
            reader
                .page_metadata("alpha")
                .expect("new consistent page")
                .present()
                .expect("alpha")
                .block_count(),
            2
        );
        assert!(reader
            .page_metadata("external")
            .expect("new external file")
            .present()
            .is_some());
        assert!(reader
            .page_metadata("added during build")
            .expect("new indexed file")
            .present()
            .is_some());
        let commit = crate::domain::PageCommit::new("alpha", base, vec![]);
        idle_writer
            .commit_page(&commit)
            .expect("unchanged file version survives a higher index revision");
        writer
            .update_block(&selector, "identity survives the concurrent publish")
            .expect("concurrent writer selector survives");
        idle_writer
            .append_block("alpha", "idle handle writes into new generation")
            .expect("idle writer reopens");
        scanner.refresh_index().expect("reopen");
        assert_eq!(
            scanner
                .index
                .page_metadata("alpha")
                .expect("metadata")
                .expect("alpha")
                .block_count(),
            3
        );
    }

    #[test]
    fn a_failed_batch_keeps_the_previous_rows_revision_and_signature() {
        let fixture = Sandbox::new();
        let mut scanner = fixture.open();
        let revision = scanner.index.metadata().expect("metadata").revision();
        fs::write(fixture.0.join("pages/alpha.md"), "- changed\n- second\n").expect("edit");
        fs::write(
            fixture.0.join("logseq/config.edn"),
            "{:journal/page-title-format \"yyyy-MM-dd\"}",
        )
        .expect("configuration");
        scanner
            .index
            .install_test_replacement_trigger()
            .expect("reject SQLite insert");
        assert!(scanner.rescan().is_err());
        assert_eq!(
            scanner.index.metadata().expect("metadata").revision(),
            revision
        );
        assert_eq!(
            scanner
                .index
                .page_metadata("alpha")
                .expect("old page")
                .expect("alpha")
                .block_count(),
            1
        );
        let current = GraphSource::open(&fixture.0).expect("configuration");
        assert!(!scanner
            .index
            .configuration_matches(current.signature())
            .expect("unpublished signature"));
        scanner
            .index
            .drop_test_replacement_trigger()
            .expect("retry");
        scanner.rescan().expect("retry scan");
        assert_eq!(
            scanner
                .page_metadata("alpha")
                .expect("page")
                .present()
                .expect("alpha")
                .block_count(),
            2
        );
    }
}
