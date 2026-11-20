//! Where a name-addressed write gets its target file from.
//!
//! A page row records the file it was projected from, so resolving a page name to its file is one
//! indexed read and one stat instead of a discovery of the whole graph. These tests assert the two
//! halves of that: the row stores the path of the file the projection used, and the write still
//! reaches the file when that path has gone stale, because discovery is the fallback and the
//! projection then corrects the row.
//!
//! Counting filesystem calls would be the direct proof that no traversal happens, and it is not
//! honest here: nothing in the crate reports how many directory listings a call made. The
//! structural proof lives in `write_path.rs`, where a file the write must not read is made
//! unreadable and the write still succeeds.

use std::fs;
use std::path::PathBuf;

use logseq_graph::engine::WriteError;
use logseq_graph::index::Index;

mod support;

use support::TempGraph;

/// Graph-relative path of the file the index records for `page`, read from the index file itself.
///
/// The engine exposes no read for this: it is the internal lookup a write performs, so a test that
/// asserts what the row holds opens the same database directly.
fn stored_page_file(graph: &TempGraph, page: &str) -> Option<PathBuf> {
    let index = Index::open_at(graph.index_path()).expect("open the index file directly");
    index.page_file(page).expect("read the page's file")
}

/// Text of a graph file, as it is on disk.
fn read_text(graph: &TempGraph, relative: &str) -> String {
    fs::read_to_string(graph.graph_root().join(relative)).expect("read graph file")
}

/// Names of the files in `relative`, sorted, for the "no file appeared" assertion.
fn file_names(graph: &TempGraph, relative: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(graph.graph_root().join(relative))
        .expect("read directory")
        .map(|entry| {
            entry
                .expect("list entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn a_projected_page_records_the_file_it_came_from() {
    let graph = TempGraph::new("page-file-recorded");
    graph.write("pages/alpha.md", "- first\n");
    let mut opened = graph.open();

    opened
        .append_block("alpha", "second")
        .expect("append a block");

    assert_eq!(
        stored_page_file(&graph, "alpha"),
        Some(PathBuf::from("pages/alpha.md")),
        "the page row must name the file the write projected"
    );
}

#[test]
fn a_page_whose_path_went_stale_is_found_by_discovery_and_the_row_is_corrected() {
    let graph = TempGraph::new("page-file-moved");
    graph.write("pages/alpha.md", "- first\n");
    let mut opened = graph.open();
    opened
        .append_block("alpha", "second")
        .expect("append a block");

    // Another process renames the file behind the index's back. The new file name decodes to the same
    // page name — a decoded name is trimmed, so `alpha .md` is the page `alpha` — which is what makes
    // this the stale-path case rather than a rename to a different page: the row still answers for
    // the name, and only the path it stores is wrong.
    graph.rename("pages/alpha.md", "pages/alpha .md");

    opened
        .append_block("alpha", "third")
        .expect("the write must reach the file that holds the page now");

    assert_eq!(
        read_text(&graph, "pages/alpha .md"),
        "- first\n- second\n- third\n"
    );
    assert_eq!(
        stored_page_file(&graph, "alpha"),
        Some(PathBuf::from("pages/alpha .md")),
        "the write must correct the path it could not use"
    );
}

#[test]
fn a_deleted_file_is_a_missing_page_and_no_file_is_created() {
    let graph = TempGraph::new("page-file-deleted");
    graph.write("pages/alpha.md", "- first\n");
    let mut opened = graph.open();
    opened
        .append_block("alpha", "second")
        .expect("append a block");

    graph.remove("pages/alpha.md");

    let failure = opened
        .append_block("alpha", "third")
        .expect_err("the page has no file any more");

    assert!(
        matches!(&failure, WriteError::PageNotFound { page } if page == "alpha"),
        "expected PageNotFound, got {failure:?}"
    );
    assert_eq!(
        file_names(&graph, "pages"),
        Vec::<String>::new(),
        "a write must never create a page file"
    );
}
