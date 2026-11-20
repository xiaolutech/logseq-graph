use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;
use crate::domain::{JournalDate, QueryContext, QueryOutcome};
use crate::engine::OpenOptions;

const WAIT: Duration = Duration::from_secs(10);

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "graph-read-pool-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(path.join("pages")).expect("pages");
        fs::write(
            path.join("pages/alpha.md"),
            "- Ärger 中文你好\n  kind:: Book\n",
        )
        .expect("page");
        Self(path)
    }

    fn graph(&self) -> Graph {
        Graph::open(OpenOptions::new(&self.0, self.0.join("index.db"))).expect("graph")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn queries_overlap_and_keep_their_snapshot_during_writes_and_shadow_publication() {
    for shadow in [false, true] {
        let sandbox = Sandbox::new();
        let pool = Arc::new(
            SharedGraph::with_readers(sandbox.graph(), NonZeroUsize::MIN.saturating_add(1))
                .expect("pool"),
        );
        let old_path = pool.write(|graph| {
            graph
                .index_storage_path()
                .expect("path")
                .expect("disk")
                .to_owned()
        });
        let old_revision = pool.read(|reader| {
            reader
                .default_reads()
                .metadata()
                .expect("metadata")
                .value()
                .revision()
        });
        let (entered, entering) = mpsc::channel();
        let mut workers = Vec::new();
        let mut releases = Vec::new();
        for _ in 0..2 {
            let pool = Arc::clone(&pool);
            let entered = entered.clone();
            let (release, resume) = mpsc::channel();
            releases.push(release);
            workers.push(thread::spawn(move || {
                pool.read(|reader| {
                    let old = reader
                        .graph
                        .read(|index| {
                            entered.send(()).expect("entered query");
                            resume.recv_timeout(WAIT).expect("resume query");
                            index.page_blocks("alpha")
                        })
                        .expect("old snapshot");
                    assert_eq!(old.meta().graph().revision(), old_revision);
                    assert_eq!(old.value().as_ref().expect("alpha").len(), 1);
                    let new = reader
                        .default_reads()
                        .page_blocks("alpha")
                        .expect("new snapshot");
                    assert_eq!(new.value().as_ref().expect("alpha").len(), 2);
                    assert!(new.meta().graph().revision() > old_revision);
                })
            }));
        }
        entering.recv_timeout(WAIT).expect("first query executes");
        entering
            .recv_timeout(WAIT)
            .expect("second query executes before first finishes");
        if shadow {
            fs::write(sandbox.0.join("pages/alpha.md"), "- before\n- after\n").expect("edit");
            pool.write(|graph| graph.reads(Freshness::VerifyContent).metadata())
                .expect("publish");
        } else {
            pool.write(|graph| graph.append_block("alpha", "after"))
                .expect("append while readers query");
        }
        let new_path = pool.write(|graph| {
            graph
                .index_storage_path()
                .expect("path")
                .expect("disk")
                .to_owned()
        });
        assert_eq!(old_path != new_path, shadow);
        for release in releases {
            release.send(()).expect("resume");
        }
        for worker in workers {
            worker.join().expect("reader");
        }
    }
}

#[test]
fn every_connection_has_search_and_property_functions() {
    let sandbox = Sandbox::new();
    let pool = Arc::new(SharedGraph::new(sandbox.graph()).expect("pool"));
    let (entered, entering) = mpsc::channel();
    let mut workers = Vec::new();
    let mut releases = Vec::new();
    for _ in 0..4 {
        let pool = Arc::clone(&pool);
        let entered = entered.clone();
        let (release, resume) = mpsc::channel();
        releases.push(release);
        workers.push(thread::spawn(move || pool.read(|reader| {
            entered.send(()).expect("leased");
            resume.recv_timeout(WAIT).expect("resume");
            let mut reads = reader.default_reads();
            assert_eq!(reads.search("ärger", 20).expect("folded search").value().len(), 1);
            assert_eq!(reads.search("中文你", 20).expect("trigram search").value().len(), 1);
            assert_eq!(reads.search("中文", 20).expect("short CJK search").value().len(), 1);
            assert!(reads.search("无", 20).expect("short CJK miss").value().is_empty());
            let context = QueryContext::new(JournalDate::new(2026, 11, 19).expect("date"), 20);
            let query = reads.query("(property kind book)", &context).expect("property function");
            assert!(matches!(query.value().outcome(), QueryOutcome::Blocks(blocks) if blocks.len() == 1));
            assert!(reads.search("absent", 20).expect("empty").value().is_empty());
            assert!(reads.page_tree("absent").expect("missing").value().is_none());
        })));
    }
    for _ in 0..4 {
        entering.recv_timeout(WAIT).expect("all connections leased");
    }
    for release in releases {
        release.send(()).expect("resume");
    }
    for worker in workers {
        worker.join().expect("reader");
    }
}

#[test]
fn policies_share_failures_and_recover_on_check_and_verify() {
    let sandbox = Sandbox::new();
    let pool = SharedGraph::new(sandbox.graph()).expect("pool");
    let observed = pool.read(|reader| {
        reader
            .default_reads()
            .metadata()
            .expect("metadata")
            .meta()
            .last_scan()
    });
    fs::write(sandbox.0.join("pages/alpha.md"), "- first\n- second\n").expect("external edit");
    pool.read(|reader| {
        assert_eq!(
            reader
                .default_reads()
                .page_blocks("alpha")
                .expect("last")
                .value()
                .as_ref()
                .expect("page")
                .len(),
            1
        );
        let checked = reader
            .reads(Freshness::CheckNow)
            .page_blocks("alpha")
            .expect("check");
        assert_eq!(checked.value().as_ref().expect("page").len(), 2);
        assert!(checked.meta().last_scan() >= observed);
    });
    fs::write(sandbox.0.join("pages/alpha.md"), b"- broken\xff\n").expect("break file");
    assert!(pool
        .read(|reader| reader.reads(Freshness::VerifyContent).pages())
        .is_err());
    assert!(matches!(
        pool.read(|reader| reader.default_reads().pages()),
        Err(EngineError::StaleIndex { .. })
    ));
    assert!(pool
        .read(|reader| reader.reads(Freshness::AllowStale).pages())
        .expect("allow stale")
        .meta()
        .is_stale());
    fs::write(sandbox.0.join("pages/alpha.md"), "- repaired\n").expect("repair");
    assert!(!pool
        .read(|reader| reader.reads(Freshness::VerifyContent).pages())
        .expect("verify")
        .meta()
        .is_stale());
    assert!(!pool
        .read(|reader| reader.default_reads().pages())
        .expect("last after repair")
        .meta()
        .is_stale());
}

#[test]
fn errors_and_panics_return_the_only_lease_and_defaults_can_scan() {
    let sandbox = Sandbox::new();
    let graph = Graph::open(
        OpenOptions::new(&sandbox.0, sandbox.0.join("index.db"))
            .with_freshness(Freshness::CheckNow),
    )
    .expect("graph");
    let pool = SharedGraph::with_readers(graph, NonZeroUsize::MIN).expect("pool");
    fs::write(sandbox.0.join("pages/beta.md"), "- new\n").expect("external page");
    assert!(pool
        .read(|reader| reader.default_reads().page_tree("beta"))
        .expect("default check")
        .value()
        .is_some());
    let error: Result<(), &str> = pool.read(|_| Err("callback error"));
    assert_eq!(error, Err("callback error"));
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || pool.read(|_| panic!("callback panic"))
    ))
    .is_err());
    assert_eq!(
        pool.read(|reader| reader.default_reads().pages())
            .expect("reused")
            .value()
            .len(),
        2
    );
}

#[test]
fn an_in_memory_index_uses_the_serialized_fallback() {
    let sandbox = Sandbox::new();
    let mut graph = sandbox.graph();
    graph.index = crate::index::Index::open_in_memory().expect("memory");
    let pool = SharedGraph::new(graph).expect("pool");
    assert!(!pool.pooled);
    assert!(pool
        .read(|reader| reader.default_reads().pages())
        .expect("empty memory index")
        .value()
        .is_empty());
}

#[test]
fn a_failure_after_settling_is_rechecked_when_the_snapshot_is_pinned() {
    let sandbox = Sandbox::new();
    let pool = SharedGraph::new(sandbox.graph()).expect("pool");
    for policy in [
        Freshness::LastScan,
        Freshness::AllowStale,
        Freshness::CheckNow,
        Freshness::VerifyContent,
    ] {
        pool.write(Graph::rescan).expect("reset observation");
        pool.read(|reader| {
            reader.graph.settle(policy).expect("settled successfully");
            reader
                .graph
                .scan_state
                .record_failure("later scan failed".to_owned());
            let read = reader
                .graph
                .read_settled(policy, |index, _| Ok(index.pages()?));
            if policy == Freshness::AllowStale {
                assert!(read.expect("stale answer").meta().is_stale());
            } else {
                assert!(matches!(read, Err(EngineError::StaleIndex { .. })));
            }
        });
    }
}

#[test]
fn adopting_an_older_successful_publication_does_not_clear_a_later_failure() {
    let sandbox = Sandbox::new();
    let pool = Arc::new(SharedGraph::new(sandbox.graph()).expect("pool"));
    let (entered, entering) = mpsc::channel();
    let mut workers = Vec::new();
    let mut releases = Vec::new();
    for _ in 0..4 {
        let pool = Arc::clone(&pool);
        let entered = entered.clone();
        let (release, resume) = mpsc::channel();
        releases.push(release);
        workers.push(thread::spawn(move || {
            pool.read(|reader| {
                entered.send(()).expect("leased");
                resume.recv_timeout(WAIT).expect("resume");
                assert!(matches!(
                    reader.default_reads().pages(),
                    Err(EngineError::StaleIndex { .. })
                ));
                assert!(reader
                    .reads(Freshness::AllowStale)
                    .pages()
                    .expect("stale answer")
                    .is_stale());
            })
        }));
    }
    for _ in 0..4 {
        entering
            .recv_timeout(WAIT)
            .expect("all old connections leased");
    }
    pool.write(|graph| graph.reads(Freshness::VerifyContent).metadata())
        .expect("successful publication");
    fs::write(sandbox.0.join("pages/alpha.md"), b"- broken\xff\n").expect("break file");
    assert!(pool.write(Graph::rescan).is_err());
    for release in releases {
        release.send(()).expect("resume");
    }
    for worker in workers {
        worker.join().expect("reader");
    }
}
