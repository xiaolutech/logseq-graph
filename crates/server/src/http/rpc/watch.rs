//! Filesystem notifications are hints; full scans repair uncertainty and failed refreshes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use logseq_graph::engine::{Graph, OpenOptions, ScanState};
use notify::{Event, EventKind, RecursiveMode, Watcher as _};
use serde::Serialize;

// Coalesce editor save bursts without indefinitely postponing a busy graph.
const QUIET_WINDOW: Duration = Duration::from_millis(50);
const MAX_DELAY: Duration = Duration::from_millis(500);

/// Why one full rescan ran. A batch can carry several causes; its one rescan counts under each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cause {
    /// The first scan once the watcher (or the poll loop) starts.
    Startup,
    /// An event carried notify's need-rescan flag: the kernel queue overflowed or the backend
    /// dropped events (inotify `IN_Q_OVERFLOW`, FSEvents `MustScanSubDirs`).
    NeedRescan,
    /// The watcher delivered an error instead of an event.
    WatcherError,
    /// A change discovery cannot apply path by path: configuration, a directory, the graph root.
    Structural,
    /// A path refresh failed, so the batch was repaired by a full scan.
    RefreshFailed,
    /// A previous full scan failed and is being retried.
    Retry,
    /// One period of polling, with notifications disabled or unavailable.
    Poll,
}

const CAUSES: usize = 7;

/// Counters a benchmark reads to prove whether notifications were lost, through `getCurrentGraph`.
///
/// The watcher thread writes them; requests only load them. The event channel to that thread is
/// unbounded, so a lost notification can only surface as a need-rescan event or a watcher error.
#[derive(Debug, Default)]
pub(super) struct WatchStats {
    mode: AtomicU8,
    need_rescan_events: AtomicU64,
    watcher_errors: AtomicU64,
    full_rescans: AtomicU64,
    by_cause: [AtomicU64; CAUSES],
}

const MODE_NOTIFY: u8 = 1;
const MODE_POLL: u8 = 2;
const MODE_STOPPED: u8 = 3;

/// The serialized form of [`WatchStats`]: every counter counts since the process started.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(super) struct WatchStatus {
    /// `starting`, `notify`, `poll`, or `stopped` when the watcher's graph could not open.
    mode: &'static str,
    need_rescan_events: u64,
    watcher_errors: u64,
    /// Full rescans attempted, failed ones included. Causes overlap, so their sum can exceed this.
    full_rescans: u64,
    full_rescans_by_cause: RescansByCause,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct RescansByCause {
    startup: u64,
    need_rescan: u64,
    watcher_error: u64,
    structural: u64,
    refresh_failed: u64,
    retry: u64,
    poll: u64,
}

impl WatchStats {
    fn set_mode(&self, mode: u8) {
        self.mode.store(mode, Ordering::Relaxed);
    }

    fn observe(&self, event: &notify::Result<Event>) {
        match event {
            Ok(event) if event.need_rescan() => {
                self.need_rescan_events.fetch_add(1, Ordering::Relaxed);
            }
            Ok(_) => {}
            Err(_) => {
                self.watcher_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn rescanning(&self, causes: &[Cause]) {
        self.full_rescans.fetch_add(1, Ordering::Relaxed);
        for cause in causes {
            self.by_cause[*cause as usize].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn status(&self) -> WatchStatus {
        let cause = |cause: Cause| self.by_cause[cause as usize].load(Ordering::Relaxed);
        WatchStatus {
            mode: match self.mode.load(Ordering::Relaxed) {
                MODE_NOTIFY => "notify",
                MODE_POLL => "poll",
                MODE_STOPPED => "stopped",
                _ => "starting",
            },
            need_rescan_events: self.need_rescan_events.load(Ordering::Relaxed),
            watcher_errors: self.watcher_errors.load(Ordering::Relaxed),
            full_rescans: self.full_rescans.load(Ordering::Relaxed),
            full_rescans_by_cause: RescansByCause {
                startup: cause(Cause::Startup),
                need_rescan: cause(Cause::NeedRescan),
                watcher_error: cause(Cause::WatcherError),
                structural: cause(Cause::Structural),
                refresh_failed: cause(Cause::RefreshFailed),
                retry: cause(Cause::Retry),
                poll: cause(Cause::Poll),
            },
        }
    }
}

#[derive(Debug, Default)]
struct Batch {
    need_rescan: bool,
    watcher_error: bool,
    structural: bool,
    paths: BTreeSet<PathBuf>,
    directories: BTreeSet<PathBuf>,
}

impl Batch {
    fn add(&mut self, event: notify::Result<Event>, root: &Path, index: &Path) {
        let Ok(event) = event else {
            self.watcher_error = true;
            return;
        };
        if event.need_rescan() {
            self.need_rescan = true;
        }
        // Reading the graph and SQLite must never generate another refresh.
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        for path in event.paths {
            let mut wal = index.as_os_str().to_os_string();
            wal.push("-wal");
            let mut shm = index.as_os_str().to_os_string();
            shm.push("-shm");
            if path == index || path == Path::new(&wal) || path == Path::new(&shm) {
                continue;
            }
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            if relative == Path::new("logseq/config.edn") {
                self.structural = true;
                continue;
            }
            let parts: Vec<_> = relative.components().collect();
            if parts.is_empty() {
                self.structural = true;
                continue;
            }
            if relative == Path::new("logseq") {
                self.structural = true;
                continue;
            }
            if !matches!(parts[0].as_os_str().to_str(), Some("pages" | "journals")) {
                continue;
            }
            if parts.len() > 1 && parts[1].as_os_str().as_encoded_bytes().starts_with(b".") {
                continue;
            }
            // Discovery is flat: only direct, non-dot Markdown children are projected.
            let directory_event = matches!(
                event.kind,
                EventKind::Create(notify::event::CreateKind::Folder)
                    | EventKind::Remove(notify::event::RemoveKind::Folder)
            );
            let was_directory = self.directories.remove(&path);
            let is_directory = path.is_dir();
            if is_directory {
                self.directories.insert(path.clone());
            }
            if parts.len() == 1
                || (parts.len() == 2 && (is_directory || was_directory || directory_event))
            {
                self.structural = true;
            } else if parts.len() == 2
                && relative
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            {
                self.paths.insert(relative.to_path_buf());
            }
        }
    }

    fn rescan(&self) -> bool {
        self.need_rescan || self.watcher_error || self.structural
    }

    fn causes(&self) -> Vec<Cause> {
        [
            (self.need_rescan, Cause::NeedRescan),
            (self.watcher_error, Cause::WatcherError),
            (self.structural, Cause::Structural),
        ]
        .into_iter()
        .filter_map(|(present, cause)| present.then_some(cause))
        .collect()
    }

    fn clear(&mut self) {
        self.need_rescan = false;
        self.watcher_error = false;
        self.structural = false;
        self.paths.clear();
    }

    fn relevant(&self) -> bool {
        self.rescan() || !self.paths.is_empty()
    }
}

fn rescan(
    graph: &mut Graph,
    reported: &mut Option<String>,
    stats: &WatchStats,
    causes: &[Cause],
) -> bool {
    stats.rescanning(causes);
    match graph.rescan() {
        Ok(()) => {
            if reported.take().is_some() {
                eprintln!("background graph scan recovered");
            }
            true
        }
        Err(error) => {
            let reason = error.to_string();
            if reported.as_deref() != Some(reason.as_str()) {
                eprintln!("background graph scan failed: {reason}");
                *reported = Some(reason);
            }
            false
        }
    }
}

fn poll(graph: &mut Graph, interval: Duration, stats: &WatchStats) {
    stats.set_mode(MODE_POLL);
    let mut reported = None;
    loop {
        std::thread::sleep(interval);
        rescan(graph, &mut reported, stats, &[Cause::Poll]);
    }
}

pub(super) fn spawn(
    root: PathBuf,
    index: PathBuf,
    interval: Duration,
    state: Arc<ScanState>,
    no_watch: bool,
    stats: Arc<WatchStats>,
) {
    let _thread = std::thread::spawn(move || {
        let stats = stats.as_ref();
        let options = OpenOptions::new(&root, &index)
            .with_startup_scan(false)
            .with_scan_state(state);
        let mut graph = match Graph::open(options) {
            Ok(graph) => graph,
            Err(error) => {
                eprintln!("could not start the graph watcher: {error}");
                stats.set_mode(MODE_STOPPED);
                return;
            }
        };
        if no_watch {
            poll(&mut graph, interval, stats);
            return;
        }
        let root = match std::fs::canonicalize(&root) {
            Ok(root) => root,
            Err(error) => {
                eprintln!("filesystem watcher unavailable; falling back to polling: {error}");
                poll(&mut graph, interval, stats);
                return;
            }
        };
        let index = std::fs::canonicalize(&index).unwrap_or(index);
        let (sender, receiver) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |event| {
            let _sent = sender.send(event);
        })
        .and_then(|mut watcher| {
            watcher.watch(&root, RecursiveMode::Recursive)?;
            Ok(watcher)
        });
        let _watcher = match watcher {
            Ok(watcher) => watcher,
            Err(error) => {
                eprintln!("filesystem watcher unavailable; falling back to polling: {error}");
                poll(&mut graph, interval, stats);
                return;
            }
        };
        let mut reported = None;
        let mut batch = Batch::default();
        // Remember directories so a rename-away event can be classified after the path is gone.
        for directory in ["pages", "journals"] {
            if let Ok(entries) = std::fs::read_dir(root.join(directory)) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        batch.directories.insert(entry.path());
                    }
                }
            }
        }
        stats.set_mode(MODE_NOTIFY);
        let mut retry = !rescan(&mut graph, &mut reported, stats, &[Cause::Startup]);
        loop {
            batch.clear();
            if retry {
                std::thread::sleep(interval);
                retry = !rescan(&mut graph, &mut reported, stats, &[Cause::Retry]);
                continue;
            }
            let first = match receiver.recv() {
                Ok(event) => event,
                Err(_) => {
                    poll(&mut graph, interval, stats);
                    return;
                }
            };
            stats.observe(&first);
            batch.add(first, &root, &index);
            if !batch.relevant() {
                continue;
            }
            let deadline = Instant::now() + MAX_DELAY;
            while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                match receiver.recv_timeout(QUIET_WINDOW.min(remaining)) {
                    Ok(event) => {
                        stats.observe(&event);
                        batch.add(event, &root, &index);
                    }
                    Err(_) => break,
                }
            }
            let causes = if batch.rescan() {
                batch.causes()
            } else if graph
                .refresh_paths(&batch.paths.iter().cloned().collect::<Vec<_>>())
                .is_err()
            {
                vec![Cause::RefreshFailed]
            } else {
                continue;
            };
            retry = !rescan(&mut graph, &mut reported, stats, &causes);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(path: &str) -> notify::Result<Event> {
        Ok(Event::new(EventKind::Any).add_path(PathBuf::from(path)))
    }

    #[test]
    fn flat_markdown_paths_are_deduplicated_and_unprojected_paths_ignored() {
        let mut batch = Batch::default();
        for path in [
            "/g/pages/a.md",
            "/g/pages/a.md",
            "/g/journals/a.MD",
            "/g/pages/.a.md",
            "/g/pages/nested/a.md",
            "/g/logseq/bak/a.md",
            "/g/pages/a.md~",
            "/g/pages/a.txt",
            "/other/pages/a.md",
            "/g/pages/index.md",
        ] {
            batch.add(event(path), Path::new("/g"), Path::new("/g/pages/index.md"));
        }
        assert_eq!(
            batch.paths,
            BTreeSet::from([PathBuf::from("pages/a.md"), PathBuf::from("journals/a.MD")])
        );
        assert!(!batch.rescan());
    }

    #[test]
    fn uncertainty_and_configuration_require_scans() {
        for event in [
            event("/g/logseq/config.edn"),
            event("/g/pages"),
            Err(notify::Error::generic("overflow")),
        ] {
            let mut batch = Batch::default();
            batch.add(event, Path::new("/g"), Path::new("/index"));
            assert!(batch.rescan());
        }
        assert!(!Batch::default().relevant());
    }

    #[test]
    fn directory_events_overflow_and_access_are_classified() {
        use notify::event::{CreateKind, Flag, RemoveKind};
        for event in [
            Event::new(EventKind::Create(CreateKind::Folder)).add_path("/g/pages/folder".into()),
            Event::new(EventKind::Remove(RemoveKind::Folder)).add_path("/g/pages/folder.md".into()),
            Event::new(EventKind::Other).set_flag(Flag::Rescan),
        ] {
            let mut batch = Batch::default();
            batch.add(Ok(event), Path::new("/g"), Path::new("/index"));
            assert!(batch.rescan());
        }
        let mut batch = Batch::default();
        batch.add(
            Ok(
                Event::new(EventKind::Access(notify::event::AccessKind::Any))
                    .add_path("/g/pages/a.md".into()),
            ),
            Path::new("/g"),
            Path::new("/index"),
        );
        assert!(!batch.relevant());
        batch
            .directories
            .insert(PathBuf::from("/g/pages/renamed-away"));
        batch.add(
            Ok(
                Event::new(EventKind::Modify(notify::event::ModifyKind::Name(
                    notify::event::RenameMode::From,
                )))
                .add_path("/g/pages/renamed-away".into()),
            ),
            Path::new("/g"),
            Path::new("/index"),
        );
        assert!(batch.rescan());
    }

    #[test]
    fn each_rescan_cause_is_classified_and_counted() {
        use notify::event::Flag;
        let cases = [
            (
                Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)),
                vec![Cause::NeedRescan],
            ),
            (
                Err(notify::Error::generic("watch failed")),
                vec![Cause::WatcherError],
            ),
            (event("/g/logseq/config.edn"), vec![Cause::Structural]),
        ];
        let stats = WatchStats::default();
        for (event, expected) in cases {
            let mut batch = Batch::default();
            stats.observe(&event);
            batch.add(event, Path::new("/g"), Path::new("/index"));
            assert_eq!(batch.causes(), expected);
            stats.rescanning(&batch.causes());
            batch.clear();
            assert!(!batch.relevant());
        }
        stats.rescanning(&[Cause::Startup]);
        stats.rescanning(&[Cause::NeedRescan, Cause::Structural]);
        stats.set_mode(MODE_NOTIFY);
        assert_eq!(
            stats.status(),
            WatchStatus {
                mode: "notify",
                need_rescan_events: 1,
                watcher_errors: 1,
                full_rescans: 5,
                full_rescans_by_cause: RescansByCause {
                    startup: 1,
                    need_rescan: 2,
                    watcher_error: 1,
                    structural: 2,
                    refresh_failed: 0,
                    retry: 0,
                    poll: 0,
                },
            }
        );
    }
}
