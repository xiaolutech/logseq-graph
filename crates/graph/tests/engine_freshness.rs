//! Freshness policy and incremental rescan tests (ADR-0006).
//!
//! Every test changes the graph files directly and then asks the graph for a read, so nothing here
//! depends on a watcher, a thread, or a timer. Each freshness policy is exercised against the same
//! observable event: a scan that could not complete because a file it observed was unreadable, or a
//! change that a stat-based scan cannot see.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use logseq_graph::engine::{EngineError, Freshness, Graph};
use logseq_graph::index::Index;
use logseq_graph::source::{LockError, SourceError, WriteLock};

mod support;

use support::{block_count, TempGraph};

/// Break `pages/alpha.md` so the next scan observes a file it cannot project.
fn break_alpha(graph: &TempGraph) {
    // Not valid UTF-8, and a different size from the projected text, so the scan cannot skip it.
    graph.write("pages/alpha.md", b"- a\n\xff\n");
}

fn later() -> SystemTime {
    SystemTime::now() + Duration::from_secs(3_600)
}

#[test]
fn scans_and_path_refreshes_exclude_the_same_graph_write_lock() {
    for populated in [false, true] {
        let graph = TempGraph::new("scan-write-exclusion");
        if populated {
            graph.write("pages/alpha.md", "- before\n");
        }
        let mut opened = Graph::open(graph.options().with_write_lock_wait(Duration::ZERO))
            .expect("open the graph");
        let revision = opened.metadata().expect("metadata").value().revision();
        graph.write("pages/alpha.md", "- before\n- after\n");
        let held = WriteLock::acquire(&graph.graph_root(), None).expect("hold graph lock");
        for result in [
            opened.rescan(),
            opened
                .refresh_paths(&[PathBuf::from("pages/alpha.md")])
                .map(|_| ()),
        ] {
            assert!(matches!(
                result,
                Err(EngineError::Lock(LockError::Busy { .. }))
            ));
        }
        assert_eq!(
            opened
                .reads(Freshness::AllowStale)
                .metadata()
                .expect("stale metadata")
                .value()
                .revision(),
            revision
        );
        drop(held);
        opened
            .rescan()
            .expect("rescan after the writer releases the lock");
        assert_eq!(
            opened
                .page_tree("alpha")
                .expect("updated tree")
                .value()
                .as_ref()
                .expect("alpha exists")
                .blocks()
                .len(),
            2
        );
    }
}

#[cfg(unix)]
#[test]
fn scans_and_path_refreshes_still_exclude_a_writer_with_a_read_only_lock_file() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    let graph = TempGraph::new("scan-read-only-lock");
    graph.write("pages/alpha.md", "- before\n");
    let options = || graph.options().with_write_lock_wait(Duration::ZERO);
    let mut opened = Graph::open(options()).expect("open the graph");
    let root = graph.graph_root();
    let lock = root.join("write.lock");
    let held = WriteLock::acquire(&root, None).expect("hold graph lock");
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o444)).expect("make lock read-only");
    graph.write("pages/alpha.md", "- before\n- after\n");
    let paths = [PathBuf::from("pages/alpha.md")];

    assert!(matches!(
        opened.rescan(),
        Err(EngineError::Lock(LockError::Busy { .. }))
    ));
    assert!(matches!(
        opened.refresh_paths(&paths),
        Err(EngineError::Lock(LockError::Busy { .. }))
    ));
    drop(held);

    assert_eq!(
        opened.refresh_paths(&paths).expect("refresh").projected(),
        1
    );
    opened.rescan().expect("rescan");
    let mut reopened = Graph::open(options()).expect("open with a read-only lock file");
    assert_eq!(block_count(&mut reopened, "alpha"), 2);
}

/// Rewrite `pages/alpha.md` as `- omega` while keeping its byte length and modification time.
///
/// `- alpha\n` and `- omega\n` are both 8 bytes, so the stat a baseline stores is identical before
/// and after this edit: only reading the file can tell the two texts apart.
fn rewrite_alpha_keeping_stat(graph: &TempGraph, observed: SystemTime) {
    let rewritten = "- omega\n";
    assert_eq!(rewritten.len(), "- alpha\n".len());
    graph.write("pages/alpha.md", rewritten);
    graph.set_modified("pages/alpha.md", observed);
}

#[test]
fn last_scan_refuses_a_read_once_a_scan_is_known_to_have_failed() {
    let graph = TempGraph::new("last-scan");
    graph.write("pages/alpha.md", "- a\n\t- a1\n");
    let mut opened = graph.open();

    let first = opened.page_tree("alpha").expect("query alpha");
    assert!(!first.meta().is_stale(), "nothing has failed yet");
    assert_eq!(first.meta().revision(), 1);
    assert!(first.meta().last_scan() <= SystemTime::now());

    break_alpha(&graph);

    // The check observes a file it cannot project, so it fails and the divergence is recorded.
    let failure = opened
        .reads(Freshness::CheckNow)
        .page_tree("alpha")
        .expect_err("a check over an unreadable file must fail");
    assert!(
        matches!(failure, EngineError::Source(SourceError::NotUtf8 { .. })),
        "expected the unreadable file to fail the check, got {failure:?}"
    );

    // The default policy now refuses instead of answering with the rows from before the change.
    match opened
        .page_tree("alpha")
        .expect_err("a known-stale index must not answer")
    {
        EngineError::StaleIndex { reason } => assert!(
            reason.contains("UTF-8"),
            "the refusal must carry the scan failure: {reason}"
        ),
        other => panic!("expected the read to be refused as stale, got {other:?}"),
    }
}

#[test]
fn allow_stale_answers_from_the_last_scan_and_marks_the_result_stale() {
    let graph = TempGraph::new("allow-stale");
    graph.write("pages/alpha.md", "- a\n\t- a1\n");
    let mut opened = graph.open();

    // The flag is not always on: an untroubled scan is not stale.
    let fresh = opened
        .reads(Freshness::AllowStale)
        .page_tree("alpha")
        .expect("query alpha");
    assert!(!fresh.meta().is_stale());
    assert_eq!(
        fresh
            .present()
            .expect("alpha is projected")
            .page()
            .block_count(),
        2
    );

    break_alpha(&graph);
    opened
        .reads(Freshness::CheckNow)
        .page_tree("alpha")
        .expect_err("the check must fail");

    let stale = opened
        .reads(Freshness::AllowStale)
        .page_tree("alpha")
        .expect("stale data is explicitly allowed");
    assert!(
        stale.meta().is_stale(),
        "the stale flag must be reachable, not permanently false"
    );
    assert_eq!(
        stale
            .present()
            .expect("the last committed scan still has rows")
            .page()
            .block_count(),
        2,
        "the answer is the last committed scan, not the broken file"
    );
    assert_eq!(
        stale.meta().revision(),
        1,
        "the failed scan committed nothing"
    );
}

#[test]
fn check_now_applies_the_change_before_it_answers() {
    let graph = TempGraph::new("check-now");
    graph.write("pages/alpha.md", "- a\n");
    let mut opened = graph.open();
    assert_eq!(block_count(&mut opened, "alpha"), 1);

    graph.write("pages/alpha.md", "- a\n\t- a1\n\t- a2\n");

    let read = opened
        .reads(Freshness::CheckNow)
        .page_tree("alpha")
        .expect("the check must succeed");
    assert_eq!(
        read.present()
            .expect("alpha is projected")
            .page()
            .block_count(),
        3,
        "the read must answer with the file as it is now"
    );
    assert!(!read.meta().is_stale());
    assert_eq!(
        read.meta().revision(),
        2,
        "the check projected exactly the changed page"
    );

    // The default policy answers the same state afterwards, so a successful check leaves no
    // divergence behind.
    let after = opened.page_tree("alpha").expect("query alpha again");
    assert!(!after.is_stale());
    assert_eq!(
        after
            .present()
            .expect("alpha is projected")
            .page()
            .block_count(),
        3
    );
}

#[test]
fn check_now_recovers_after_the_file_becomes_readable_again() {
    let graph = TempGraph::new("recover");
    graph.write("pages/alpha.md", "- a\n");
    let mut opened = graph.open();

    break_alpha(&graph);
    opened
        .reads(Freshness::CheckNow)
        .page_tree("alpha")
        .expect_err("the check must fail");
    opened
        .page_tree("alpha")
        .expect_err("the failed check must leave the index known-stale");

    graph.write("pages/alpha.md", "- a\n\t- a1\n\t- a2\n\t- a3\n");

    let recovered = opened
        .reads(Freshness::CheckNow)
        .page_tree("alpha")
        .expect("a completed scan clears the divergence");
    assert_eq!(
        recovered
            .present()
            .expect("alpha is projected")
            .page()
            .block_count(),
        4
    );
    assert!(!recovered.is_stale());

    let after = opened.page_tree("alpha").expect("no divergence remains");
    assert!(!after.is_stale());
    assert_eq!(
        after
            .present()
            .expect("alpha is projected")
            .page()
            .block_count(),
        4
    );
}

#[test]
fn a_deleted_file_loses_its_page_and_its_baseline() {
    let graph = TempGraph::new("deleted");
    graph.write("pages/alpha.md", "- a\n");
    graph.write("pages/beta.md", "- b\n\t- b1\n");
    let mut opened = graph.open();
    assert_eq!(
        opened.metadata().expect("read metadata").value().revision(),
        2
    );
    assert_eq!(block_count(&mut opened, "beta"), 2);

    graph.remove("pages/beta.md");

    let read = opened
        .reads(Freshness::CheckNow)
        .page_tree("beta")
        .expect("the check must succeed");
    assert!(
        read.present().is_none(),
        "a deleted file must not keep a readable page"
    );
    assert_eq!(
        read.meta().revision(),
        3,
        "removing the page is the only committed change"
    );
    drop(opened);

    let index = Index::open_at(graph.index_path()).expect("open index directly");
    assert_eq!(
        index.page_names().expect("read page names"),
        ["alpha"],
        "the deleted page row must be gone, not just unreadable by name"
    );
    assert_eq!(
        index.file_baseline_paths().expect("read baseline paths"),
        [PathBuf::from("pages/alpha.md")],
        "the deleted file's baseline must be gone as well"
    );
}

#[test]
fn a_renamed_file_moves_its_page_and_its_baseline() {
    let graph = TempGraph::new("renamed");
    graph.write("pages/alpha.md", "- a\n\t- a1\n");
    let mut opened = graph.open();
    assert_eq!(
        opened.metadata().expect("read metadata").value().revision(),
        1
    );

    graph.rename("pages/alpha.md", "pages/renamed.md");

    let read = opened
        .reads(Freshness::CheckNow)
        .page_tree("renamed")
        .expect("the check must succeed");
    assert_eq!(
        read.present()
            .expect("the renamed page is projected")
            .page()
            .block_count(),
        2
    );
    assert_eq!(
        read.meta().revision(),
        3,
        "removing the old page and projecting the new one are two writes"
    );
    assert!(
        opened
            .page_tree("alpha")
            .expect("query the old name")
            .present()
            .is_none(),
        "a rename must not leave the old page behind"
    );
    drop(opened);

    let index = Index::open_at(graph.index_path()).expect("open index directly");
    assert_eq!(
        index.page_names().expect("read page names"),
        ["renamed"],
        "the old page name must be gone"
    );
    assert_eq!(
        index.file_baseline_paths().expect("read baseline paths"),
        [PathBuf::from("pages/renamed.md")],
        "the old path's baseline must be gone"
    );
}

#[test]
fn a_configuration_change_reprojects_every_file() {
    let graph = TempGraph::new("config");
    graph.write("journals/2026_08_13.md", "- j\n\t- j1\n");
    let mut opened = graph.open();
    // Logseq's default `:journal/page-title-format` renders the date as a title.
    assert_eq!(block_count(&mut opened, "Aug 13th, 2026"), 2);
    assert_eq!(
        opened.metadata().expect("read metadata").value().revision(),
        1
    );

    // The journal file is untouched, so its baseline still matches; only the configuration moved.
    graph.write_config("{:journal/page-title-format \"yyyy-MM-dd\"}");

    let read = opened
        .reads(Freshness::CheckNow)
        .page_tree("2026-08-13")
        .expect("the check must succeed");
    assert_eq!(
        read.present()
            .expect("the journal is projected under its new name")
            .page()
            .block_count(),
        2
    );
    assert_eq!(
        read.meta().revision(),
        3,
        "dropping the old rows and projecting the file again are two writes"
    );
    assert!(
        opened
            .page_tree("Aug 13th, 2026")
            .expect("query the old title")
            .present()
            .is_none(),
        "the title the old configuration produced must be gone"
    );
    drop(opened);

    let index = Index::open_at(graph.index_path()).expect("open index directly");
    assert_eq!(
        index.page_names().expect("read page names"),
        ["2026-08-13"],
        "the configuration change must rename the page, not duplicate it"
    );
}

#[test]
fn every_read_carries_the_instance_revision_and_observation_time() {
    let graph = TempGraph::new("read-meta");
    graph.write("pages/alpha.md", "- a\n");
    let mut opened = graph.open();

    let read = opened.metadata().expect("read metadata");
    assert_eq!(read.value(), read.meta().graph());
    assert_eq!(read.meta().revision(), 1);
    assert_eq!(read.meta().instance_id().len(), 32);
    assert!(!read.meta().is_stale());
    let scanned = read.meta().last_scan();
    assert!(scanned <= SystemTime::now());

    // A miss is a read result too, so freshness is never silently dropped.
    let miss = opened.page_tree("missing").expect("query a missing page");
    assert!(miss.present().is_none());
    assert_eq!(miss.meta().revision(), 1);
    assert_eq!(
        miss.meta().last_scan(),
        scanned,
        "no scan ran between the two reads"
    );

    // The graph-wide default and the per-read policy report the same instance.
    let allowed = opened
        .reads(Freshness::AllowStale)
        .metadata()
        .expect("read metadata");
    assert_eq!(allowed.meta().instance_id(), read.meta().instance_id());
}

#[test]
fn a_configuration_change_that_breaks_discovery_keeps_the_old_rows() {
    let graph = TempGraph::new("config-broken");
    graph.write("journals/2026_01_01.md", "- jan\n");
    graph.write("journals/2026_08_13.md", "- aug\n");
    let mut opened = graph.open();
    assert_eq!(block_count(&mut opened, "Jan 1st, 2026"), 1);
    assert_eq!(block_count(&mut opened, "Aug 13th, 2026"), 1);
    assert_eq!(
        opened.metadata().expect("read metadata").value().revision(),
        2
    );

    // Rendering the title from the year alone makes both journals the same page, which discovery
    // rejects. The configuration is therefore changed and the graph unreadable at the same time.
    graph.write_config("{:journal/page-title-format \"yyyy\"}");

    let failure = opened
        .reads(Freshness::CheckNow)
        .page_tree("Aug 13th, 2026")
        .expect_err("discovery must fail when two journals share a title");
    assert!(
        matches!(
            failure,
            EngineError::Source(SourceError::DuplicateName { .. })
        ),
        "expected a duplicate-name failure, got {failure:?}"
    );

    // Nothing was deleted on the way to that failure: the rows of the previous configuration are
    // still there, at the revision that produced them. Clearing first would have left an empty index
    // at a higher revision, which is the state this ordering exists to avoid.
    let kept = opened
        .reads(Freshness::AllowStale)
        .page_tree("Aug 13th, 2026")
        .expect("the rows of the last successful scan must survive a failed discovery");
    assert!(kept.is_stale(), "the failed scan is still reported");
    assert_eq!(
        kept.present()
            .expect("the old rows are still readable")
            .page()
            .block_count(),
        1
    );
    assert_eq!(
        kept.meta().revision(),
        2,
        "a failed discovery must not commit anything"
    );
    drop(opened);

    let index = Index::open_at(graph.index_path()).expect("open index directly");
    assert_eq!(
        index.page_names().expect("read page names"),
        ["Aug 13th, 2026", "Jan 1st, 2026"],
        "the derived rows of the old configuration must still be indexable"
    );
}

#[test]
fn a_change_that_keeps_size_and_modification_time_is_not_detected() {
    let graph = TempGraph::new("stat-hint");
    graph.write("pages/alpha.md", "- a\n\t- a1\n");
    let observed = graph.modified("pages/alpha.md");
    let mut opened = graph.open();
    assert_eq!(block_count(&mut opened, "alpha"), 2);
    assert_eq!(
        opened.metadata().expect("read metadata").value().revision(),
        1
    );

    // Different text of the same length, with the modification time restored: exactly the case a
    // stat-based baseline cannot see, which is what makes the baseline a hint rather than a proof.
    let rewritten = "- b\n\t- b1\n";
    assert_eq!(rewritten.len(), "- a\n\t- a1\n".len());
    graph.write("pages/alpha.md", rewritten);
    graph.set_modified("pages/alpha.md", observed);

    let read = opened
        .reads(Freshness::CheckNow)
        .page_tree("alpha")
        .expect("the check must succeed");
    assert!(!read.is_stale());
    assert_eq!(
        read.meta().revision(),
        1,
        "a matching size and modification time must skip the file, even though its text changed"
    );
    assert_eq!(
        read.present()
            .expect("alpha is projected")
            .page()
            .revision(),
        1,
        "the rows still come from the scan before the unnoticed edit"
    );
}

#[test]
fn a_change_that_keeps_size_and_modification_time_is_detected_by_verify_content() {
    let graph = TempGraph::new("verify-content");
    graph.write("pages/alpha.md", "- alpha\n");
    let observed = graph.modified("pages/alpha.md");
    let mut opened = graph.open();
    assert_eq!(
        opened.metadata().expect("read metadata").value().revision(),
        1
    );
    assert_eq!(opened.search("alpha", 10).expect("search").value().len(), 1);

    rewrite_alpha_keeping_stat(&graph, observed);

    // The same edit [`a_change_that_keeps_size_and_modification_time_is_not_detected`] shows a
    // stat-based scan cannot see. Reading every file is what makes it visible.
    let read = opened
        .reads(Freshness::VerifyContent)
        .page_tree("alpha")
        .expect("the verified scan must succeed");
    assert!(!read.is_stale());
    assert_eq!(
        read.meta().revision(),
        2,
        "a matching stat must not stop a scan that reads every file"
    );
    assert_eq!(
        read.present()
            .expect("alpha is projected")
            .page()
            .revision(),
        2
    );
    assert_eq!(
        opened.search("omega", 10).expect("search").value().len(),
        1,
        "the rows must come from the new text"
    );
    assert_eq!(
        opened.search("alpha", 10).expect("search").value().len(),
        0,
        "the old text must be gone from the index"
    );
}

#[test]
fn the_same_change_stays_hidden_from_check_now() {
    let graph = TempGraph::new("stat-hint-control");
    graph.write("pages/alpha.md", "- alpha\n");
    let observed = graph.modified("pages/alpha.md");
    let mut opened = graph.open();
    assert_eq!(
        opened.metadata().expect("read metadata").value().revision(),
        1
    );

    rewrite_alpha_keeping_stat(&graph, observed);

    // The control for the test above: the stat-based policy is unchanged by the new one and still
    // cannot see this edit, which is what makes the two policies observably different.
    let read = opened
        .reads(Freshness::CheckNow)
        .page_tree("alpha")
        .expect("the check must succeed");
    assert_eq!(read.meta().revision(), 1);
    assert_eq!(
        opened.search("alpha", 10).expect("search").value().len(),
        1,
        "the old text is still what the stat-based scan holds"
    );
    assert_eq!(opened.search("omega", 10).expect("search").value().len(), 0);
}

#[test]
fn verify_content_publishes_a_new_revision_when_the_fingerprint_matches() {
    let graph = TempGraph::new("verify-content-unchanged");
    graph.write("pages/alpha.md", "- alpha\n");
    let observed = graph.modified("pages/alpha.md");
    let mut opened = graph.open();
    assert_eq!(
        opened.metadata().expect("read metadata").value().revision(),
        1
    );

    // The bytes are rewritten identically and the modification time is restored, as a sync client
    // that believes nothing changed would do: reading the file finds the same fingerprint.
    graph.write("pages/alpha.md", "- alpha\n");
    graph.set_modified("pages/alpha.md", observed);

    let read = opened
        .reads(Freshness::VerifyContent)
        .page_tree("alpha")
        .expect("the verified scan must succeed");
    assert!(
        read.meta().revision() > 1,
        "a full shadow publication advances the revision even when bytes match"
    );
    assert_eq!(
        read.present()
            .expect("alpha is projected")
            .page()
            .revision(),
        read.meta().revision()
    );
}

#[test]
fn verify_content_reprojects_a_file_whose_content_really_changed() {
    let graph = TempGraph::new("verify-content-changed");
    graph.write("pages/alpha.md", "- alpha\n");
    let mut opened = graph.open();
    assert_eq!(
        opened.metadata().expect("read metadata").value().revision(),
        1
    );

    graph.write("pages/alpha.md", "- alpha\n\t- alpha1\n");
    graph.set_modified("pages/alpha.md", later());

    // Reading every file must not change what a real change does: the page is reprojected once and
    // the revision moves once.
    let read = opened
        .reads(Freshness::VerifyContent)
        .page_tree("alpha")
        .expect("the verified scan must succeed");
    assert_eq!(read.meta().revision(), 2);
    assert_eq!(
        read.present()
            .expect("alpha is projected")
            .page()
            .block_count(),
        2
    );
    assert_eq!(
        opened.search("alpha1", 10).expect("search").value().len(),
        1,
        "the new block must be searchable"
    );
}

#[test]
fn verify_content_fails_the_read_when_its_scan_fails() {
    let graph = TempGraph::new("verify-content-failure");
    graph.write("pages/alpha.md", "- a\n\t- a1\n");
    let mut opened = graph.open();

    break_alpha(&graph);

    let failure = opened
        .reads(Freshness::VerifyContent)
        .page_tree("alpha")
        .expect_err("a verified scan over an unreadable file must fail");
    assert!(
        matches!(failure, EngineError::Source(SourceError::NotUtf8 { .. })),
        "expected the unreadable file to fail the scan, got {failure:?}"
    );

    // The failure is recorded exactly as under `CheckNow`, so the default policy refuses next time.
    match opened
        .page_tree("alpha")
        .expect_err("a known-stale index must not answer")
    {
        EngineError::StaleIndex { reason } => assert!(
            reason.contains("UTF-8"),
            "the refusal must carry the scan failure: {reason}"
        ),
        other => panic!("expected the read to be refused as stale, got {other:?}"),
    }
}

#[test]
fn a_failed_scan_does_not_move_the_observation_time() {
    let graph = TempGraph::new("last-scan-time");
    graph.write("pages/alpha.md", "- a\n");
    let mut opened = graph.open();
    let scanned = opened.metadata().expect("read metadata").meta().last_scan();

    break_alpha(&graph);
    opened
        .reads(Freshness::CheckNow)
        .page_tree("alpha")
        .expect_err("the check must fail");

    let after_failure = opened
        .reads(Freshness::AllowStale)
        .metadata()
        .expect("read metadata");
    assert!(after_failure.is_stale());
    assert_eq!(
        after_failure.meta().last_scan(),
        scanned,
        "a scan that did not complete observed nothing, so it must not report a newer time"
    );

    graph.write("pages/alpha.md", "- a\n\t- a1\n");
    let after_success = opened
        .reads(Freshness::CheckNow)
        .metadata()
        .expect("the check must succeed");
    assert!(!after_success.is_stale());
    assert!(
        after_success.meta().last_scan() >= scanned,
        "a completed scan moves the observation time forward"
    );
}
