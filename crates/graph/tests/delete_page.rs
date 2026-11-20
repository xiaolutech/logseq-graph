//! Deleting a page: the file moves into `logseq/.recycle/`, the index loses its rows.
//!
//! Every test writes a graph under the system temporary directory, opens it through [`Graph`], and
//! asserts on the bytes on disk as well as on the projection. The bytes are the point of most of
//! them: a delete is a rename, so the file that lands in the recycle bin has to be the file that was
//! in `pages/`, byte for byte, and the page that was beside it has to be untouched.
//!
//! The tests that matter most are the ones about what must *not* happen: a delete must not leave the
//! page file in two places, must not overwrite an earlier recycled copy of the same name, and must
//! not touch a page it was not asked to delete.

use std::fs;

use logseq_graph::engine::{Freshness, Graph, WriteError};

mod support;

use support::TempGraph;

#[cfg(unix)]
#[test]
fn deletion_refuses_recycle_and_source_parents_outside_the_graph() {
    for relative in ["logseq", "logseq/.recycle", "pages"] {
        let graph = TempGraph::new("delete-outside-parent");
        let outside = TempGraph::new("delete-outside-target");
        graph.write("pages/a.md", "- original\n");
        let mut handle = graph.open();
        if relative == "pages" {
            fs::rename(
                graph.graph_root().join("pages"),
                outside.graph_root().join("pages"),
            )
            .expect("move pages outside");
        }
        if relative == "logseq/.recycle" {
            graph.make_dir("logseq");
        }
        let target = if relative == "pages" {
            outside.graph_root().join("pages")
        } else {
            outside.graph_root().to_owned()
        };
        std::os::unix::fs::symlink(&target, graph.graph_root().join(relative))
            .expect("symlink outside parent");
        let before = handle.metadata().expect("metadata").into_value();
        let error = handle.delete_page("a").expect_err("outside parent refused");
        assert_eq!(
            error.write_state(),
            logseq_graph::engine::WriteState::Unwritten
        );
        assert_eq!(
            fs::read(graph.graph_root().join("pages/a.md")).expect("page stays"),
            b"- original\n"
        );
        assert_eq!(handle.metadata().expect("metadata").into_value(), before);
        assert!(!outside.graph_root().join(".recycle").exists());
        assert!(!outside.graph_root().join("pages_a.md").exists());
        assert!(!graph
            .graph_root()
            .join("logseq/.recycle/pages_a.md")
            .exists());
    }
}

#[cfg(unix)]
#[test]
fn deletion_accepts_recycle_parents_resolved_inside_the_graph() {
    let graph = TempGraph::new("delete-inside-parent");
    graph.write("pages/a.md", "");
    graph.make_dir("actual");
    std::os::unix::fs::symlink(
        graph.graph_root().join("actual"),
        graph.graph_root().join("logseq"),
    )
    .expect("symlink inside parent");
    graph
        .open()
        .delete_page("a")
        .expect("delete through inside parent");
    assert_eq!(
        fs::read(graph.graph_root().join("actual/.recycle/pages_a.md"))
            .expect("empty recycled file"),
        b""
    );
    assert!(!graph.graph_root().join("pages/a.md").exists());
}

/// Bytes of a graph file, as they are on disk.
fn read_bytes(graph: &TempGraph, relative: &str) -> Vec<u8> {
    fs::read(graph.graph_root().join(relative)).expect("read graph file")
}

/// Names of the files in `relative`, sorted, for the "which file landed where" assertions.
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

/// Whether the graph holds a file at `relative`.
fn exists(graph: &TempGraph, relative: &str) -> bool {
    graph.graph_root().join(relative).exists()
}

/// Page names the index holds, read without scanning the files again.
fn indexed_names(graph: &mut Graph) -> Vec<String> {
    graph
        .reads(Freshness::LastScan)
        .pages()
        .expect("list pages")
        .into_value()
        .into_iter()
        .map(|page| page.name().to_owned())
        .collect()
}

#[test]
fn a_deleted_page_file_lands_in_the_recycle_bin_byte_for_byte() {
    let graph = TempGraph::new("delete-page-file");
    graph.write("pages/a.md", "- one\n- two\n");
    let mut opened = graph.open();
    let before = read_bytes(&graph, "pages/a.md");

    let deleted = opened.delete_page("a").expect("delete the page");

    assert_eq!(deleted.value().name(), "a");
    assert!(
        !exists(&graph, "pages/a.md"),
        "the page file is no longer under pages/: {:?}",
        file_names(&graph, "pages")
    );
    assert_eq!(
        read_bytes(&graph, "logseq/.recycle/pages_a.md"),
        before,
        "the recycled file is the page's own bytes, not a rewritten copy"
    );
}

#[test]
fn a_deleted_journal_page_carries_the_journals_prefix() {
    // The default journal file name format writes `2024_01_02`; the dash form is the same day in
    // another configured layout, and the recycled name still has to name the directory it came from.
    let graph = TempGraph::new("delete-page-journal");
    graph.write_config(r#"{:journal/file-name-format "yyyy-MM-dd"}"#);
    graph.write("journals/2024-01-02.md", "- journal\n");
    let mut opened = graph.open();
    let name = opened
        .reads(Freshness::CheckNow)
        .pages()
        .expect("list pages")
        .into_value()
        .into_iter()
        .next()
        .expect("the journal is a page")
        .name()
        .to_owned();

    opened.delete_page(&name).expect("delete the journal page");

    assert!(
        !exists(&graph, "journals/2024-01-02.md"),
        "the journal file is no longer under journals/: {:?}",
        file_names(&graph, "journals")
    );
    assert!(
        exists(&graph, "logseq/.recycle/journals_2024-01-02.md"),
        "the journal prefix is what tells a journal apart from a page: {:?}",
        file_names(&graph, "logseq/.recycle")
    );
}

#[test]
fn a_recycled_name_that_is_taken_gains_a_millisecond_stamp() {
    let graph = TempGraph::new("delete-page-collision");
    graph.write("pages/a.md", "- new\n");
    graph.write("logseq/.recycle/pages_a.md", "older copy\n");
    let mut opened = graph.open();

    let deleted = opened.delete_page("a").expect("delete the page");

    assert_eq!(deleted.value().name(), "a");
    assert_eq!(
        read_bytes(&graph, "logseq/.recycle/pages_a.md"),
        b"older copy\n",
        "the earlier recycled copy is not overwritten"
    );
    let stamped: Vec<String> = file_names(&graph, "logseq/.recycle")
        .into_iter()
        .filter(|name| {
            // `pages_a.md` shares the prefix and the extension, so the middle segment has to be the
            // all-digits stamp for the name to be the stamped copy.
            name.strip_prefix("pages_a.")
                .and_then(|rest| rest.strip_suffix(".md"))
                .is_some_and(|stamp| !stamp.is_empty() && stamp.chars().all(|c| c.is_ascii_digit()))
        })
        .collect();
    assert_eq!(
        stamped.len(),
        1,
        "the new copy is one stamped file: {:?}",
        file_names(&graph, "logseq/.recycle")
    );
    let stamp = stamped[0]
        .strip_prefix("pages_a.")
        .and_then(|rest| rest.strip_suffix(".md"))
        .expect("the stamped name keeps `pages_a.` and the extension");
    assert!(
        stamp.chars().all(|c| c.is_ascii_digit()) && !stamp.is_empty(),
        "the stamp is the millisecond count, got `{stamp}`"
    );
    assert_eq!(
        read_bytes(&graph, &format!("logseq/.recycle/{}", stamped[0])),
        b"- new\n",
        "the stamped copy is the deleted page's bytes"
    );
}

#[test]
fn the_deleted_page_is_gone_from_the_index_without_a_scan() {
    let graph = TempGraph::new("delete-page-index");
    graph.write("pages/a.md", "- one\n");
    let mut opened = graph.open();

    opened.delete_page("a").expect("delete the page");

    assert!(
        opened
            .page_metadata("a")
            .expect("query metadata")
            .value()
            .is_none(),
        "the page row is deleted by the write itself"
    );
    assert!(
        !indexed_names(&mut opened).contains(&"a".to_owned()),
        "the page is not listed: {:?}",
        indexed_names(&mut opened)
    );
}

#[test]
fn the_answer_is_the_metadata_the_page_had_before_the_deletion() {
    let graph = TempGraph::new("delete-page-metadata");
    graph.write("pages/a.md", "- one\n- two\n- three\n");
    let mut opened = graph.open();
    let before = opened
        .page_metadata("a")
        .expect("query metadata")
        .into_value()
        .expect("the page is projected");

    let deleted = opened.delete_page("a").expect("delete the page");

    assert_eq!(deleted.value().name(), "a");
    assert_eq!(
        deleted.value().block_count(),
        3,
        "the count is the page's, from before the rows were deleted"
    );
    assert_eq!(
        deleted.value().block_count(),
        before.block_count(),
        "the answer is the pre-deletion projection, not a page reduced to zero blocks"
    );
}

#[test]
fn deleting_a_page_the_graph_does_not_hold_is_refused() {
    let graph = TempGraph::new("delete-page-missing");
    graph.write("pages/a.md", "- one\n");
    let mut opened = graph.open();
    let before = read_bytes(&graph, "pages/a.md");

    let error = opened.delete_page("missing").expect_err("refuse a miss");

    match error {
        WriteError::PageNotFound { page } => assert_eq!(page, "missing"),
        other => panic!("expected PageNotFound, got {other:?}"),
    }
    assert_eq!(
        read_bytes(&graph, "pages/a.md"),
        before,
        "the refused delete must not touch what is there"
    );
    assert!(
        !exists(&graph, "logseq/.recycle/pages_missing.md"),
        "a miss creates nothing in the recycle bin"
    );
}

#[test]
fn deleting_one_page_leaves_the_other_bytes_and_rows_alone() {
    let graph = TempGraph::new("delete-page-siblings");
    graph.write("pages/a.md", "- one\n");
    graph.write("pages/b.md", "- two\n");
    let mut opened = graph.open();
    let b_before = read_bytes(&graph, "pages/b.md");

    opened.delete_page("a").expect("delete one of two pages");

    assert_eq!(
        read_bytes(&graph, "pages/b.md"),
        b_before,
        "the page this write did not name keeps its bytes"
    );
    assert!(
        opened
            .page_metadata("b")
            .expect("query metadata")
            .value()
            .is_some(),
        "the untouched page keeps its rows"
    );
    assert!(indexed_names(&mut opened).contains(&"b".to_owned()));
}

#[test]
fn a_graph_without_a_logseq_directory_gets_one_holding_the_recycled_file() {
    let graph = TempGraph::new("delete-page-no-logseq");
    graph.write("pages/a.md", "- one\n");
    assert!(
        !graph.graph_root().join("logseq").exists(),
        "the fixture must start without logseq/, or this test proves nothing"
    );
    let mut opened = graph.open();
    let before = read_bytes(&graph, "pages/a.md");

    opened.delete_page("a").expect("delete the page");

    assert!(
        graph.graph_root().join("logseq").is_dir(),
        "the recycle directory's parent is created"
    );
    assert_eq!(
        read_bytes(&graph, "logseq/.recycle/pages_a.md"),
        before,
        "the recycled file is inside the directory this call created"
    );
}
