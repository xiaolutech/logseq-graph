//! The remove path: one durable id, one subtree, through the engine facade.
//!
//! Every test writes a graph under the system temporary directory, opens it through [`Graph`], and
//! asserts on the bytes on disk as well as on the projection. The bytes are the point of most of
//! them: a removal deletes one contiguous run of lines, so the preamble, the siblings, the trailing
//! blank lines, and the line endings have to come out exactly as they were.
//!
//! The tests that matter most are the ones about what must *not* happen: a refused removal must not
//! touch the file, a missing id must not create or rewrite anything, and a removal must not
//! reproject a page it did not write.

use std::fs;

use logseq_graph::document::EditError;
use logseq_graph::domain::{BlockId, BlockSelector};
use logseq_graph::engine::{Freshness, Graph, WriteError};

mod support;

use support::TempGraph;

/// The durable id the fixtures below state.
const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

/// A page with one identified block that has two children, and a sibling after it.
const ALPHA: &str = "\
- first
  id:: 01234567-89ab-cdef-0123-456789abcdef
  - child one
  - child two
- last
";

/// A page holding one identified block and a page property above it.
const SOLO: &str = "\
title:: x
- first
  id:: 01234567-89ab-cdef-0123-456789abcdef
";

/// The `id::` UUID as a caller would hold it.
fn id() -> BlockId {
    BlockId::parse(ID).expect("valid 8-4-4-4-12 UUID")
}

/// A durable selector for the fixture id.
fn durable() -> BlockSelector {
    BlockSelector::Durable(id())
}

/// A durable selector for an id spelled in the test itself.
fn durable_of(value: &str) -> BlockSelector {
    BlockSelector::Durable(BlockId::parse(value).expect("valid 8-4-4-4-12 UUID"))
}

/// Bytes of a graph file, as they are on disk.
fn read_bytes(graph: &TempGraph, relative: &str) -> Vec<u8> {
    fs::read(graph.graph_root().join(relative)).expect("read graph file")
}

/// Text of a graph file, as they are on disk.
fn read_text(graph: &TempGraph, relative: &str) -> String {
    String::from_utf8(read_bytes(graph, relative)).expect("graph file is UTF-8")
}

/// Block count of a page, read without scanning the files again.
///
/// `LastScan` is the point: a removal must leave the index readable at its own result, so a read that
/// does not scan has to see the new shape.
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

#[test]
fn a_removed_subtree_is_gone_from_the_file_and_the_tree() {
    let graph = TempGraph::new("remove-subtree");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    let removed = opened
        .remove_block(&durable())
        .expect("remove the block and its children");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- last\n",
        "the target's whole subtree is deleted, and the sibling keeps its bytes"
    );
    assert_eq!(removed.value().name(), "alpha");
    assert_eq!(
        removed.value().block_count(),
        1,
        "three blocks went, one stayed"
    );
    assert_eq!(
        block_count_without_scan(&mut opened, "alpha"),
        1,
        "the index holds the new shape without another scan"
    );

    // The tree the next read answers with: one top-level block, no children, and no trace of the
    // deleted text.
    let read = opened
        .reads(Freshness::LastScan)
        .page_tree("alpha")
        .expect("query the page tree");
    let tree = read.present().expect("the page is projected");
    assert_eq!(tree.blocks().len(), 1, "only the sibling is left");
    assert!(
        tree.blocks()[0].children().is_empty(),
        "the deleted children must not hang under the sibling"
    );
    let hits = opened
        .reads(Freshness::LastScan)
        .search("child", 10)
        .expect("search")
        .into_value();
    assert!(
        hits.is_empty(),
        "the deleted blocks are not searchable, got {hits:?}"
    );
}

#[test]
fn removing_the_only_block_leaves_the_page_and_its_preamble() {
    let graph = TempGraph::new("remove-last-block");
    graph.write("pages/alpha.md", SOLO);
    let mut opened = graph.open();

    let removed = opened
        .remove_block(&durable())
        .expect("a page may be reduced to its preamble");

    assert_eq!(removed.value().block_count(), 0, "a page with no blocks");
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "title:: x\n",
        "the file stays, the page line stays, and only the block's lines are gone"
    );
    let read = opened
        .reads(Freshness::LastScan)
        .page_tree("x")
        .expect("query the page tree");
    let tree = read.present().expect("the page is projected");
    assert!(tree.blocks().is_empty(), "no block is left");
    assert_eq!(block_count_without_scan(&mut opened, "x"), 0);
}

#[test]
fn an_id_the_file_dropped_is_refused_by_the_edit_rather_than_the_lookup() {
    // The index still holds the row, because the external edit below kept the size. The row is
    // enough to find the right file — the name it carries is correct — and the file then refuses
    // the edit, because it no longer states the id. That is the accurate report: the block is gone
    // from the file, and the stale row did not invent one.
    let graph = TempGraph::new("remove-dropped-id");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    // An external edit that removes the identity line, as a person deleting it in an editor would.
    graph.write(
        "pages/alpha.md",
        "- first\n  - child one\n  - child two\n- last\n",
    );
    let before = read_bytes(&graph, "pages/alpha.md");

    let failure = opened
        .remove_block(&durable())
        .expect_err("the id is no longer stated");

    assert!(
        matches!(&failure, WriteError::Edit(EditError::BlockNotFound)),
        "expected Edit(BlockNotFound) from the file, got {failure:?}"
    );
    assert_eq!(
        read_bytes(&graph, "pages/alpha.md"),
        before,
        "the file the caller edited must not be rewritten"
    );
}

#[test]
fn an_unknown_id_is_an_error_and_leaves_the_file_unchanged() {
    let graph = TempGraph::new("remove-missing");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();
    let before = read_bytes(&graph, "pages/alpha.md");

    let failure = opened
        .remove_block(&durable_of("00000000-0000-0000-0000-000000000000"))
        .expect_err("an id no block states must fail");

    assert!(
        matches!(&failure, WriteError::BlockNotFound { id } if id.as_str() == "00000000-0000-0000-0000-000000000000"),
        "expected BlockNotFound, got {failure:?}"
    );
    assert_eq!(
        read_bytes(&graph, "pages/alpha.md"),
        before,
        "a removal that found nothing must not touch the file"
    );
}

#[test]
fn a_remove_does_not_reproject_a_page_it_does_not_write() {
    // A write re-establishes one page: the one whose file it replaces. A page it does not touch
    // keeps whatever the last scan projected, even when the file behind it changed beyond what a
    // stat can see. Refreshing other pages is a read's job, and it is the caller's to ask for.
    let graph = TempGraph::new("remove-other-page");
    graph.write("pages/alpha.md", ALPHA);
    graph.write("pages/beta.md", "- one\n");
    let mut opened = graph.open();

    // Same size, same modification time: a stat-based scan cannot see this, a content scan can.
    let relative = "pages/beta.md";
    let modified = graph.modified(relative);
    graph.write(relative, "- two\n");
    graph.set_modified(relative, modified);

    opened
        .remove_block(&durable())
        .expect("remove the identified block");

    let old = opened
        .reads(Freshness::LastScan)
        .search("one", 10)
        .expect("search")
        .into_value();
    assert_eq!(
        old.len(),
        1,
        "the write must not have reprojected the page it did not touch, got {old:?}"
    );
    let unseen = opened
        .reads(Freshness::LastScan)
        .search("two", 10)
        .expect("search")
        .into_value();
    assert!(
        unseen.is_empty(),
        "the unprojected text must not be in the index yet, got {unseen:?}"
    );
}

#[test]
fn an_id_stated_on_two_pages_is_refused_and_neither_file_changes() {
    // The refusal is the same one the update makes and happens in the same lookup: a removal reads one
    // file, so two pages stating the id means it cannot tell whose subtree to delete. Deleting one of
    // them would silently keep the other, which is the state a caller would have to find by hand.
    let graph = TempGraph::new("remove-ambiguous-pages");
    graph.write("pages/alpha.md", ALPHA);
    graph.write("pages/beta.md", SOLO);
    let mut opened = graph.open();
    let alpha_before = read_bytes(&graph, "pages/alpha.md");
    let beta_before = read_bytes(&graph, "pages/beta.md");

    let failure = opened
        .remove_block(&durable())
        .expect_err("an id stated on two pages must be refused");

    match &failure {
        WriteError::AmbiguousBlockId { id: refused, pages } => {
            assert_eq!(refused.as_str(), ID);
            assert_eq!(
                pages,
                &["alpha".to_owned(), "x".to_owned()],
                "every page stating the id is named, in sorted order"
            );
        }
        other => panic!("expected AmbiguousBlockId, got {other:?}"),
    }

    assert_eq!(
        read_bytes(&graph, "pages/alpha.md"),
        alpha_before,
        "the page one candidate is on keeps its bytes"
    );
    assert_eq!(
        read_bytes(&graph, "pages/beta.md"),
        beta_before,
        "the page the other candidate is on keeps its bytes"
    );
}
