use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use logseq_graph::domain::ReadMeta;
use logseq_graph::engine::{Graph, ReadHandle, ScanProgress, SharedGraph};

use super::answer::Meta;
use super::error::RequestError;
use super::watch::WatchStats;

/// What the serve loop needs: the graph once the startup scan landed it, the index location, and how
/// far that scan has got.
pub(super) struct Served {
    pub(super) state: Mutex<State>,
    /// The graph root, stated by `getCurrentGraph` as `name` / `path` / `url`.
    pub(super) graph_root: PathBuf,
    pub(super) index_path: PathBuf,
    pub(super) progress: Arc<ScanProgress>,
    /// What the background watcher has seen; see [`WatchStats`].
    pub(super) watch: Arc<WatchStats>,
}

/// Which of the three things this process is: still scanning, ready, or unable to open the graph.
///
/// The graph is not a `Mutex<Graph>` that exists from the first request, because the socket is bound
/// before the startup scan (ADR-0008) and there is no graph to lock while it runs. The three states
/// are answered rather than unwrapped: a call reached too early is a scanning error and one reached
/// after a failed open is an internal error that states why, and neither is a panic.
pub(super) enum State {
    /// Bound and scanning: `getCurrentGraph` answers the progress, every other method refuses.
    Scanning,
    /// The startup scan finished; every method reads this graph.
    ///
    /// Shared so the startup-state lock need not be held during engine operations.
    Ready(Arc<SharedGraph>),
    /// The startup scan failed, so it will not finish. Every method answers an internal error with
    /// this reason instead of the process exiting: the port is already bound, so a caller polling it
    /// can be told what went wrong instead of reading a refused connection as a server that died.
    Failed(String),
}

impl Served {
    /// Run one complete write operation under the engine's writer lock.
    pub(super) fn with_graph<T>(
        &self,
        operation: impl FnOnce(&mut Graph) -> Result<T, RequestError>,
    ) -> Result<T, RequestError> {
        self.graph()?.write(operation)
    }

    fn graph(&self) -> Result<Arc<SharedGraph>, RequestError> {
        #[expect(
            clippy::expect_used,
            reason = "a poisoned state lock cannot be recovered"
        )]
        let state = self
            .state
            .lock()
            .expect("the graph state lock is not poisoned");
        match &*state {
            State::Ready(graph) => Ok(Arc::clone(graph)),
            State::Scanning => {
                let (scanned, total) = self.progress.snapshot();
                Err(RequestError::Scanning { scanned, total })
            }
            State::Failed(reason) => Err(RequestError::Internal(format!(
                "the graph could not be opened: {reason}"
            ))),
        }
    }

    /// Run the engine reads one method needs, under the request's freshness policy.
    ///
    /// The state lock is released before leasing a read-only connection. The watcher has its own
    /// handle and is never called through this lock. The closure
    /// returns the metadata of the read that produced its value, so the answer states the
    /// observation it was actually made under rather than one the adapter assumed.
    pub(super) fn read<T>(
        &self,
        reads: impl FnOnce(&mut ReadHandle<'_>) -> Result<(T, ReadMeta), RequestError>,
    ) -> Result<(T, Meta), RequestError> {
        self.graph()?.read(|graph| {
            let (value, read_meta) = reads(graph)?;
            Ok((value, Meta::of(&read_meta)))
        })
    }

    /// The startup scan's progress, while it has not finished.
    pub(super) fn scanning(&self) -> Option<(u64, u64)> {
        #[expect(
            clippy::expect_used,
            reason = "a poisoned state lock means another thread panicked while holding it; there is \
                      no recoverable state to read afterwards, so refusing to continue is the answer"
        )]
        let state = self
            .state
            .lock()
            .expect("the graph state lock is not poisoned");
        match &*state {
            State::Scanning => Some(self.progress.snapshot()),
            State::Ready(_) | State::Failed(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use logseq_graph::engine::OpenOptions;
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn request_reads_release_the_state_and_writer_locks() {
        let root = std::env::temp_dir().join(format!(
            "rpc-read-pool-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("pages")).expect("pages");
        std::fs::write(root.join("pages/alpha.md"), "- before\n").expect("page");
        let index = root.join("index.db");
        let graph = SharedGraph::new(Graph::open(OpenOptions::new(&root, &index)).expect("graph"))
            .expect("pool");
        let served = Arc::new(Served {
            state: Mutex::new(State::Ready(Arc::new(graph))),
            graph_root: root.clone(),
            index_path: index,
            progress: Arc::new(ScanProgress::new()),
            watch: Arc::new(WatchStats::default()),
        });
        let (entered, entering) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let first = Arc::clone(&served);
        let worker = std::thread::spawn(move || {
            first.read(|reader| {
                entered.send(()).expect("entered");
                resume
                    .recv_timeout(Duration::from_secs(10))
                    .expect("resume");
                let read = reader
                    .default_reads()
                    .pages()
                    .map_err(super::super::error::internal)?;
                Ok((read.value().len(), read.meta().clone()))
            })
        });
        entering
            .recv_timeout(Duration::from_secs(10))
            .expect("first request executing");
        let second = served
            .read(|reader| {
                let read = reader
                    .default_reads()
                    .pages()
                    .map_err(super::super::error::internal)?;
                Ok((read.value().len(), read.meta().clone()))
            })
            .expect("second request finishes while first is paused");
        assert_eq!(second.0, 1);
        served
            .with_graph(|graph| {
                graph
                    .append_block("alpha", "after")
                    .map(|_| ())
                    .map_err(|error| RequestError::Internal(error.to_string()))
            })
            .expect("write while first read is paused");
        release.send(()).expect("release");
        worker
            .join()
            .expect("request thread")
            .expect("first request");
        drop(served);
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
