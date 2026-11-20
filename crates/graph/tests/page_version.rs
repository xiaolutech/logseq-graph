//! One page's two versions — the bytes the index projected and the bytes on disk now — without a
//! snapshot and without a scan.
//!
//! This is the read an editor polls while one page is open. It has to answer whether the file behind
//! that page changed at a cost that does not grow with the graph, and it has to answer it in a way
//! that says which side moved: a caller holding only the file's token sees the same value whether
//! the index is current or stuck behind it. Every test writes a graph file, opens a [`Graph`], and
//! asks through [`Graph::page_versions`], so the path from the row through the baseline, the stat,
//! and the fingerprint is covered — including the cases where row and file disagree, which is why
//! the read exists at all.

use logseq_graph::domain::{PageVersion, PageVersions};
use logseq_graph::engine::{Freshness, Graph};

mod support;

use support::TempGraph;

/// Opens a graph holding `source` as the single page `page`.
fn open(page: &str, source: &str) -> (TempGraph, Graph) {
    let graph = TempGraph::new("page-version");
    graph.write(&format!("pages/{page}.md"), source);
    let opened = graph.open();
    (graph, opened)
}

/// The versions of `page`, which the index must know about.
fn versions_of(graph: &mut Graph, page: &str) -> PageVersions {
    graph
        .page_versions(page)
        .expect("read the page versions")
        .into_value()
        .expect("the page exists")
}

/// The disk version of `page`, which must exist on both sides.
fn disk_version(graph: &mut Graph, page: &str) -> PageVersion {
    versions_of(graph, page)
        .disk()
        .cloned()
        .expect("the page file exists")
}

#[test]
fn both_versions_are_the_snapshot_token_of_the_bytes_they_describe() {
    // A page mid-write: the block structure is odd and nothing about it needs to be right for a
    // token to mean something, which is the point — the question is about bytes.
    let (_graph, mut opened) = open("agree", "```\n\t- inside an unclosed fence\n");

    let versions = versions_of(&mut opened, "agree");
    let snapshot = opened
        .page_snapshot("agree")
        .expect("read the page snapshot")
        .into_value()
        .expect("the page exists");

    assert_eq!(
        versions.disk(),
        Some(snapshot.version()),
        "one page's bytes have one token, whichever read answers it"
    );
    assert_eq!(
        versions.index(),
        snapshot.version(),
        "nothing changed since the scan, so the indexed bytes are the file's bytes"
    );
}

#[test]
fn a_version_follows_the_file_when_the_index_does_not() {
    let graph = TempGraph::new("page-version-file");
    graph.write("pages/alpha.md", "- a\n\t- a1\n");
    let observed = graph.modified("pages/alpha.md");
    let mut opened = graph.open();
    let before = versions_of(&mut opened, "alpha");

    // Different text of the same length, with the modification time restored: the case a stat-based
    // baseline cannot see, and the reason a token is a fingerprint of the bytes instead.
    let rewritten = "- b\n\t- b1\n";
    assert_eq!(rewritten.len(), "- a\n\t- a1\n".len());
    graph.write("pages/alpha.md", rewritten);
    graph.set_modified("pages/alpha.md", observed);

    let after = versions_of(&mut opened, "alpha");
    assert_eq!(
        after.index(),
        before.index(),
        "no scan ran, so the indexed bytes are still the ones before the edit"
    );
    assert_ne!(
        after.disk(),
        Some(before.index()),
        "a restored modification time must not hide a change to the bytes"
    );
    assert_eq!(
        opened
            .page_metadata("alpha")
            .expect("read the page row")
            .into_value()
            .expect("the page row still exists")
            .block_count(),
        2,
        "no scan ran, so the row still describes the same shape: the disk token came from the file"
    );
    assert_eq!(
        versions_of(&mut opened, "alpha"),
        after,
        "nothing changed since the read before it, so neither token may move"
    );
}

#[test]
fn a_scan_brings_the_indexed_version_up_to_the_disk_version() {
    let graph = TempGraph::new("page-version-scan");
    graph.write("pages/alpha.md", "- alpha\n");
    let mut opened = graph.open();
    let before = versions_of(&mut opened, "alpha");
    graph.write("pages/alpha.md", "- omega\n");

    let behind = versions_of(&mut opened, "alpha");
    assert_ne!(
        behind.disk(),
        Some(behind.index()),
        "the file moved and the index has not: this is the state a scan resolves"
    );

    // A scan under a policy that reads the file, not just its stat.
    opened
        .reads(Freshness::VerifyContent)
        .page_metadata("alpha")
        .expect("scan and read the page");

    let after = versions_of(&mut opened, "alpha");
    assert_eq!(
        after.disk(),
        Some(after.index()),
        "the scan projected the file, so the two sides agree again"
    );
    assert_ne!(
        after.index(),
        before.index(),
        "the indexed bytes are the new ones, not the old token"
    );
}

#[test]
fn a_missing_page_answers_none_without_creating_it() {
    let (graph, mut opened) = open("present", "- one\n");

    assert!(
        opened
            .page_versions("absent")
            .expect("read a page that does not exist")
            .into_value()
            .is_none(),
        "a name with no page row names no page"
    );
    assert!(
        !graph.graph_root().join("pages/absent.md").exists(),
        "the read must not create the file it could not find"
    );
}

#[test]
fn a_row_whose_file_disappeared_keeps_its_indexed_version() {
    let (graph, mut opened) = open("gone", "- one\n");
    let before = versions_of(&mut opened, "gone");
    std::fs::remove_file(graph.graph_root().join("pages/gone.md")).expect("remove the page file");

    let after = versions_of(&mut opened, "gone");
    assert_eq!(
        after.disk(),
        None,
        "no file holds this page any more, so there is no disk version to report"
    );
    assert_eq!(
        after.index(),
        before.index(),
        "the index still holds what it projected: a deleted page is not an unknown one"
    );
}

#[test]
fn a_file_no_scan_has_projected_is_not_a_page_yet() {
    let (graph, mut opened) = open("present", "- one\n");
    graph.write("pages/late.md", "- late\n");

    assert!(
        opened
            .page_versions("late")
            .expect("read a file no scan has seen")
            .into_value()
            .is_none(),
        "the read locates the file through the index, and no scan has projected this one"
    );
}

#[test]
fn reading_a_version_writes_nothing() {
    let (_graph, mut opened) = open("quiet", "- one\n");
    let before = opened.metadata().expect("read metadata").into_value();

    disk_version(&mut opened, "quiet");

    let after = opened.metadata().expect("read metadata").into_value();
    assert_eq!(
        before.revision(),
        after.revision(),
        "a read writes nothing and commits nothing"
    );
}

#[test]
fn a_file_that_is_not_utf8_is_not_a_page_version() {
    let (graph, mut opened) = open("binary", "- one\n");
    // A truncated multi-byte character is what a write in progress can leave behind. There is no
    // text to fingerprint, so this is the source layer's error rather than a token: every
    // fingerprint in this graph is defined on text, and one made up from bytes would compare with
    // nothing a scan ever stored.
    graph.write("pages/binary.md", b"- \xf0\x9f");

    assert!(
        opened.page_versions("binary").is_err(),
        "a file with no text has no version, and inventing one would answer a different question"
    );
}
