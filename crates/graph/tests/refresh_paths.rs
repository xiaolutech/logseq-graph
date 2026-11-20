//! Refreshing the index from a set of changed paths, one file at a time.
//!
//! A scan re-lists the graph and stats every file; a refresh is given the paths a caller already
//! knows changed. These tests pin the two properties that make that worth doing: a refresh touches
//! only the paths it was given (other pages keep their revision), and it refuses to run when
//! `logseq/config.edn` changed, because page names are derived from the configuration and only a
//! full scan can re-derive them.
//!
//! Every read here uses `Freshness::LastScan`: a refresh must leave the index answerable at its own
//! result, so a read that does not scan is the assertion that the refresh itself applied the change.

use std::path::PathBuf;
use std::time::SystemTime;

use logseq_graph::engine::{EngineError, Freshness, Graph};

mod support;

use support::TempGraph;

/// One path, in the coordinate system a refresh takes.
fn path(relative: &str) -> PathBuf {
    PathBuf::from(relative)
}

/// Block count of a page as the index holds it, without scanning the files again.
fn block_count_without_scan(graph: &mut Graph, page: &str) -> u32 {
    graph
        .reads(Freshness::LastScan)
        .page_tree(page)
        .expect("query the page tree")
        .present()
        .expect("the page is projected")
        .page()
        .block_count()
}

/// Revision the index is at, read without scanning the files again.
fn revision_without_scan(graph: &mut Graph) -> u64 {
    graph
        .reads(Freshness::LastScan)
        .metadata()
        .expect("read metadata")
        .value()
        .revision()
}

/// Revision of one page's rows, read without scanning the files again.
fn page_revision(graph: &mut Graph, page: &str) -> u64 {
    graph
        .reads(Freshness::LastScan)
        .page_metadata(page)
        .expect("query page metadata")
        .present()
        .expect("the page is projected")
        .revision()
}

#[test]
fn refreshing_a_changed_file_projects_only_that_page() {
    let graph = TempGraph::new("refresh-changed");
    graph.write("pages/alpha.md", "- a\n");
    graph.write("pages/beta.md", "- b\n");
    let mut opened = graph.open();
    assert_eq!(revision_without_scan(&mut opened), 2);
    let alpha_revision = page_revision(&mut opened, "alpha");
    let beta_revision = page_revision(&mut opened, "beta");

    graph.write("pages/alpha.md", "- a\n\t- a2\n");
    let report = opened
        .refresh_paths(&[path("pages/alpha.md")])
        .expect("refresh the changed file");

    assert_eq!(report.paths(), 1);
    assert_eq!(report.projected(), 1);
    assert_eq!(report.baseline_refreshed(), 0);
    assert_eq!(report.removed(), 0);
    assert_eq!(report.unchanged(), 0);
    assert_eq!(report.absent(), 0);

    // The page the refresh projected advanced once, and it is the one the read sees.
    assert_eq!(block_count_without_scan(&mut opened, "alpha"), 2);
    assert_eq!(revision_without_scan(&mut opened), 3);
    assert!(
        page_revision(&mut opened, "alpha") > alpha_revision,
        "the refreshed page must carry a newer revision"
    );
    assert_eq!(
        page_revision(&mut opened, "beta"),
        beta_revision,
        "a page the refresh did not name must keep its revision"
    );
}

#[test]
fn a_touched_file_with_unchanged_bytes_only_refreshes_its_baseline() {
    let graph = TempGraph::new("refresh-baseline");
    graph.write("pages/alpha.md", "- a\n");
    let mut opened = graph.open();
    assert_eq!(revision_without_scan(&mut opened), 1);

    // Same bytes, different stat: the baseline no longer matches, so the file is read and
    // fingerprinted, and only its baseline moves.
    graph.set_modified("pages/alpha.md", SystemTime::UNIX_EPOCH);

    let report = opened
        .refresh_paths(&[path("pages/alpha.md")])
        .expect("refresh the touched file");

    assert_eq!(report.projected(), 1);
    assert_eq!(
        report.baseline_refreshed(),
        1,
        "the report must say the projection only moved the baseline"
    );
    assert_eq!(
        revision_without_scan(&mut opened),
        1,
        "an unchanged page must not advance the revision"
    );
}

#[test]
fn refreshing_a_file_the_index_already_matches_changes_nothing() {
    let graph = TempGraph::new("refresh-unchanged");
    graph.write("pages/alpha.md", "- a\n");
    let mut opened = graph.open();
    let revision = revision_without_scan(&mut opened);

    let report = opened
        .refresh_paths(&[path("pages/alpha.md")])
        .expect("refresh the untouched file");

    assert_eq!(report.unchanged(), 1);
    assert_eq!(report.projected(), 0);
    assert_eq!(revision_without_scan(&mut opened), revision);
}

#[test]
fn refreshing_a_deleted_file_removes_its_page() {
    let graph = TempGraph::new("refresh-deleted");
    graph.write("pages/alpha.md", "- a\n");
    graph.write("pages/beta.md", "- b\n");
    let mut opened = graph.open();
    assert_eq!(revision_without_scan(&mut opened), 2);
    let beta_revision = page_revision(&mut opened, "beta");

    graph.remove("pages/alpha.md");
    let report = opened
        .refresh_paths(&[path("pages/alpha.md")])
        .expect("refresh the deleted file");

    assert_eq!(report.removed(), 1);
    assert_eq!(report.projected(), 0);
    assert!(
        opened
            .reads(Freshness::LastScan)
            .page_tree("alpha")
            .expect("query the removed page")
            .present()
            .is_none(),
        "the removed page must be gone from the index"
    );
    assert_eq!(
        revision_without_scan(&mut opened),
        3,
        "removing a page advances the revision once"
    );
    assert_eq!(
        page_revision(&mut opened, "beta"),
        beta_revision,
        "a page the refresh did not name must keep its revision"
    );
}

#[test]
fn refreshing_a_new_file_projects_it() {
    let graph = TempGraph::new("refresh-new");
    graph.write("pages/alpha.md", "- a\n");
    let mut opened = graph.open();
    assert_eq!(revision_without_scan(&mut opened), 1);

    graph.write("pages/gamma.md", "- g\n");
    let report = opened
        .refresh_paths(&[path("pages/gamma.md")])
        .expect("refresh the new file");

    assert_eq!(report.projected(), 1);
    assert_eq!(block_count_without_scan(&mut opened, "gamma"), 1);
    assert_eq!(revision_without_scan(&mut opened), 2);
}

#[test]
fn one_call_with_a_changed_and_an_unchanged_path_advances_the_revision_once() {
    let graph = TempGraph::new("refresh-mixed");
    graph.write("pages/alpha.md", "- a\n");
    graph.write("pages/beta.md", "- b\n");
    let mut opened = graph.open();
    assert_eq!(revision_without_scan(&mut opened), 2);
    let beta_revision = page_revision(&mut opened, "beta");

    graph.write("pages/alpha.md", "- a\n- a2\n");
    let report = opened
        .refresh_paths(&[path("pages/alpha.md"), path("pages/beta.md")])
        .expect("refresh both paths");

    assert_eq!(report.paths(), 2);
    assert_eq!(report.projected(), 1);
    assert_eq!(report.unchanged(), 1);
    assert_eq!(
        revision_without_scan(&mut opened),
        3,
        "only the changed path may advance the revision, and only once"
    );
    assert_eq!(page_revision(&mut opened, "beta"), beta_revision);
}

#[test]
fn two_changed_paths_advance_the_revision_once_each() {
    let graph = TempGraph::new("refresh-two-changed");
    graph.write("pages/alpha.md", "- a\n");
    graph.write("pages/beta.md", "- b\n");
    let mut opened = graph.open();
    assert_eq!(revision_without_scan(&mut opened), 2);

    graph.write("pages/alpha.md", "- a\n- a2\n");
    graph.write("pages/beta.md", "- b\n- b2\n");
    let report = opened
        .refresh_paths(&[path("pages/alpha.md"), path("pages/beta.md")])
        .expect("refresh both changed paths");

    assert_eq!(report.projected(), 2);
    assert_eq!(
        revision_without_scan(&mut opened),
        4,
        "one replacement per path, not one per call"
    );
}

#[test]
fn a_path_that_was_never_projected_is_reported_absent_and_changes_nothing() {
    let graph = TempGraph::new("refresh-absent");
    graph.write("pages/alpha.md", "- a\n");
    let mut opened = graph.open();
    assert_eq!(revision_without_scan(&mut opened), 1);

    let report = opened
        .refresh_paths(&[path("pages/never.md"), path("pages/also-never.md")])
        .expect("refresh paths nothing ever backed");

    assert_eq!(report.paths(), 2);
    assert_eq!(report.absent(), 2);
    assert_eq!(revision_without_scan(&mut opened), 1);
}

#[test]
fn a_configuration_change_refuses_the_refresh_and_leaves_every_row_alone() {
    let graph = TempGraph::new("refresh-config");
    graph.write("journals/2026_08_13.md", "- j\n");
    let mut opened = graph.open();
    // Logseq's default `:journal/page-title-format` renders the date as a title.
    assert_eq!(block_count_without_scan(&mut opened, "Aug 13th, 2026"), 1);

    // The journal file is untouched; only the configuration moved, which renames the page.
    graph.write_config("{:journal/page-title-format \"yyyy-MM-dd\"}");

    let failure = opened
        .refresh_paths(&[path("journals/2026_08_13.md")])
        .expect_err("a refresh under a changed configuration must be refused");
    assert!(
        matches!(failure, EngineError::ConfigurationChanged),
        "expected a configuration refusal, got {failure:?}"
    );

    // Nothing was projected under the new name and nothing was deleted: the refusal precedes every
    // write, so the index still holds exactly the rows the old configuration produced. The refusal
    // marks the handle stale, so the rows are read as they are with `AllowStale`.
    assert_eq!(
        opened
            .reads(Freshness::AllowStale)
            .page_tree("Aug 13th, 2026")
            .expect("query the old title")
            .present()
            .expect("the old page is still projected")
            .page()
            .block_count(),
        1
    );
    assert!(
        opened
            .reads(Freshness::AllowStale)
            .page_tree("2026-08-13")
            .expect("query the new title")
            .present()
            .is_none(),
        "the refused refresh must not project anything under the new name"
    );
    assert_eq!(
        opened
            .reads(Freshness::AllowStale)
            .metadata()
            .expect("read metadata")
            .value()
            .revision(),
        1,
        "the refused refresh must not advance the revision"
    );

    // A full scan is the documented fallback, and it re-derives the name.
    assert_eq!(
        opened
            .reads(Freshness::CheckNow)
            .page_tree("2026-08-13")
            .expect("the check must succeed")
            .present()
            .expect("the journal is projected under its new name")
            .page()
            .block_count(),
        1
    );
    assert!(
        opened
            .page_tree("Aug 13th, 2026")
            .expect("query the old title")
            .present()
            .is_none(),
        "the title the old configuration produced must be gone"
    );
}
