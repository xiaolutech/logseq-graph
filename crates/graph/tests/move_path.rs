//! The move path: two durable ids, one page, through the engine facade.
//!
//! Every test writes a graph under the system temporary directory, opens it through [`Graph`], and
//! asserts on the bytes on disk as well as on the projection. The bytes are the point of most of
//! them: a move cuts one contiguous run of lines out of a page and pastes it back in, so everything
//! outside that run has to come out exactly as it was.
//!
//! The tests that matter most are the ones about what must *not* happen: an unknown id must not
//! rewrite anything, a move must not reproject a page it did not write, and no move may scan the
//! graph first. The cross-page move has its own file, `move_cross_page.rs`, including its refusal
//! and recovery cases.

use std::fs;

use logseq_graph::domain::{BlockId, BlockSelector};
use logseq_graph::engine::{Freshness, Graph, MoveTarget, WriteError};

mod support;

use support::TempGraph;

/// The three durable ids the fixtures below state.
const ID_A: &str = "01234567-89ab-cdef-0123-456789abcdef";
const ID_B: &str = "fedcba98-7654-3210-fedc-ba9876543210";
const ID_C: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// A page with an identified block that has a child, and an identified sibling after it.
const ALPHA: &str = "\
- one
  id:: 01234567-89ab-cdef-0123-456789abcdef
  - child
- two
  id:: fedcba98-7654-3210-fedc-ba9876543210
";

/// The `id::` UUID as a caller would hold it.
fn id(raw: &str) -> BlockId {
    BlockId::parse(raw).expect("valid 8-4-4-4-12 UUID")
}

/// The id no fixture states.
fn missing() -> BlockId {
    id("00000000-0000-0000-0000-000000000000")
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
/// `LastScan` is the point: a move must leave the index readable at its own result, so a read that
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
fn a_move_reorders_the_blocks_and_keeps_the_page_shape() {
    let graph = TempGraph::new("move-after-sibling");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    let moved = opened
        .move_block(
            &BlockSelector::Durable(id(ID_A)),
            MoveTarget::After(BlockSelector::Durable(id(ID_B))),
        )
        .expect("move the block and its child after the sibling");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        format!("- two\n  id:: {ID_B}\n- one\n  id:: {ID_A}\n  - child\n"),
        "the moved subtree lands after the sibling, and the other lines keep their bytes"
    );
    assert_eq!(moved.value().name(), "alpha");
    assert_eq!(
        moved.value().block_count(),
        3,
        "a move keeps every block: one, its child, and two"
    );
    assert_eq!(
        block_count_without_scan(&mut opened, "alpha"),
        3,
        "the index holds the new shape without another scan"
    );

    // The tree the next read answers with: the two top-level blocks changed order, and the child is
    // still a child of the block that moved.
    let read = opened
        .reads(Freshness::LastScan)
        .page_tree("alpha")
        .expect("query the page tree");
    let tree = read.present().expect("the page is projected");
    assert_eq!(tree.blocks().len(), 2, "two top-level blocks");
    assert_eq!(
        tree.blocks()[1].children().len(),
        1,
        "the moved block kept its child"
    );
    let hits = opened
        .reads(Freshness::LastScan)
        .search("child", 10)
        .expect("search")
        .into_value();
    assert_eq!(hits.len(), 1, "the moved text is searchable, got {hits:?}");
}

#[test]
fn a_move_that_deepens_a_block_changes_its_depth_in_the_tree() {
    let graph = TempGraph::new("move-first-child");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    opened
        .move_block(
            &BlockSelector::Durable(id(ID_B)),
            MoveTarget::FirstChildOf(BlockSelector::Durable(id(ID_A))),
        )
        .expect("move the sibling under the identified block");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        format!("- one\n  id:: {ID_A}\n  - two\n    id:: {ID_B}\n  - child\n"),
        "the moved block lands before the existing child and gains one indentation level"
    );

    let read = opened
        .reads(Freshness::LastScan)
        .page_tree("alpha")
        .expect("query the page tree");
    let tree = read.present().expect("the page is projected");
    assert_eq!(tree.blocks().len(), 1, "one top-level block is left");
    assert_eq!(
        tree.blocks()[0].children().len(),
        2,
        "the moved block and the old child are siblings under it"
    );
}

#[test]
fn an_unknown_id_is_reported_per_side_and_leaves_the_files_alone() {
    let graph = TempGraph::new("move-missing");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();
    let before = read_bytes(&graph, "pages/alpha.md");

    let source_failure = opened
        .move_block(
            &BlockSelector::Durable(missing()),
            MoveTarget::After(BlockSelector::Durable(id(ID_B))),
        )
        .expect_err("no block states the id being moved");
    assert!(
        matches!(&source_failure, WriteError::BlockNotFound { id } if id == &missing()),
        "expected BlockNotFound for the moved block, got {source_failure:?}"
    );

    let anchor_failure = opened
        .move_block(
            &BlockSelector::Durable(id(ID_A)),
            MoveTarget::After(BlockSelector::Durable(missing())),
        )
        .expect_err("no block states the target id");
    assert!(
        matches!(&anchor_failure, WriteError::BlockNotFound { id } if id == &missing()),
        "expected BlockNotFound for the target, got {anchor_failure:?}"
    );

    assert_eq!(
        read_bytes(&graph, "pages/alpha.md"),
        before,
        "a move that resolved no page must not touch the file"
    );
}

#[test]
fn a_move_does_not_reproject_a_page_it_does_not_write() {
    // A write re-establishes one page: the one whose file it replaces. A page it does not touch
    // keeps whatever the last scan projected, even when the file behind it changed beyond what a
    // stat can see. Refreshing other pages is a read's job, and it is the caller's to ask for.
    let graph = TempGraph::new("move-other-page");
    graph.write("pages/alpha.md", ALPHA);
    graph.write("pages/beta.md", "- unseen old\n");
    let mut opened = graph.open();

    // Same size, same modification time: a stat-based scan cannot see this, a content scan can.
    let relative = "pages/beta.md";
    let modified = graph.modified(relative);
    graph.write(relative, "- unseen new\n");
    graph.set_modified(relative, modified);

    opened
        .move_block(
            &BlockSelector::Durable(id(ID_A)),
            MoveTarget::After(BlockSelector::Durable(id(ID_B))),
        )
        .expect("move the identified block after its sibling");

    let old = opened
        .reads(Freshness::LastScan)
        .search("unseen old", 10)
        .expect("search")
        .into_value();
    assert!(
        !old.is_empty(),
        "the write must not have reprojected the page it did not touch, got {old:?}"
    );
    let unseen = opened
        .reads(Freshness::LastScan)
        .search("unseen new", 10)
        .expect("search")
        .into_value();
    assert!(
        unseen.is_empty(),
        "the unprojected text must not be in the index yet, got {unseen:?}"
    );
}

#[test]
fn an_id_stated_on_two_pages_is_refused_for_either_side_of_a_move() {
    // A move resolves two ids, and each of them can be ambiguous on its own. Both go through the same
    // lookup, so the refusal is one rule rather than one per operand, and it comes before the file
    // the move would rewrite is read: neither page changes for either attempt.
    let graph = TempGraph::new("move-ambiguous-pages");
    // `ID_A` and `ID_B` are each stated by both pages; `ID_C` is stated once, so it can play the
    // unambiguous side of both attempts.
    graph.write(
        "pages/alpha.md",
        format!("- one\n  id:: {ID_A}\n- two\n  id:: {ID_B}\n- three\n  id:: {ID_C}\n"),
    );
    graph.write(
        "pages/beta.md",
        format!("- bravo one\n  id:: {ID_A}\n- bravo two\n  id:: {ID_B}\n"),
    );
    let mut opened = graph.open();
    let alpha_before = read_bytes(&graph, "pages/alpha.md");
    let beta_before = read_bytes(&graph, "pages/beta.md");

    // The block to move is stated on two pages.
    let failure = opened
        .move_block(
            &BlockSelector::Durable(id(ID_A)),
            MoveTarget::After(BlockSelector::Durable(id(ID_C))),
        )
        .expect_err("a block stated on two pages cannot be moved");

    match &failure {
        WriteError::AmbiguousBlockId { id: refused, pages } => {
            assert_eq!(refused.as_str(), ID_A);
            assert_eq!(
                pages,
                &["alpha".to_owned(), "beta".to_owned()],
                "every page stating the moved id is named, in sorted order"
            );
        }
        other => panic!("expected AmbiguousBlockId from the moved id, got {other:?}"),
    }

    // The block to move is unambiguous; the anchor it names is not.
    let failure = opened
        .move_block(
            &BlockSelector::Durable(id(ID_C)),
            MoveTarget::After(BlockSelector::Durable(id(ID_B))),
        )
        .expect_err("an anchor stated on two pages cannot say where to move");

    match &failure {
        WriteError::AmbiguousBlockId { id: refused, pages } => {
            assert_eq!(refused.as_str(), ID_B);
            assert_eq!(
                pages,
                &["alpha".to_owned(), "beta".to_owned()],
                "every page stating the anchor id is named, in sorted order"
            );
        }
        other => panic!("expected AmbiguousBlockId from the anchor id, got {other:?}"),
    }

    assert_eq!(
        read_bytes(&graph, "pages/alpha.md"),
        alpha_before,
        "a refused move must not rewrite the page it would have moved from"
    );
    assert_eq!(
        read_bytes(&graph, "pages/beta.md"),
        beta_before,
        "a refused move must not rewrite the page the ambiguity names"
    );
}
