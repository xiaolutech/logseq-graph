//! Block writes by ephemeral id: update and remove, addressed by the id the projection minted
//! (ADR-0012).
//!
//! Every test writes a graph under the system temporary directory, opens it through [`Graph`], and
//! asks one write with a selector read out of that same graph. The point of most assertions is the
//! bytes on disk: an ephemeral write must reach exactly the block its row names in the file, which is
//! the only place most blocks live — they state no `id::`, so the index row alone could never have
//! carried the edit. The near misses matter as much: an id this write's own reprojection replaced
//! (an ephemeral id names a block only while its page's bytes are unchanged), an id no row holds,
//! and a write that must not silently land on a neighbouring block.

use logseq_graph::engine::{Freshness, Graph, WriteError};

mod support;

use support::{bound, TempGraph};

/// A page whose blocks exercise several levels and no `id::` at all.
///
/// Blocks as `(preorder, depth, sibling_index)`: `first (0, 0, 0)`, `child (1, 1, 0)`,
/// `grandchild (2, 2, 0)`, `second child (3, 1, 1)`, `last (4, 0, 1)`. Nothing states an `id::`,
/// which is the condition a durable-only write cannot reach and this file exists for.
const ALPHA: &str = "\
- first
\t- child
\t\t- grandchild
\t- second child
- last
";

/// Open the graph holding `ALPHA`.
fn open(graph: &TempGraph) -> Graph {
    graph.write("pages/alpha.md", ALPHA);
    graph.open()
}

/// Text of a graph file, as it is on disk.
fn read_text(graph: &TempGraph, relative: &str) -> String {
    std::fs::read_to_string(graph.graph_root().join(relative)).expect("read graph file")
}

/// Block count of a page, read without scanning the files again.
///
/// `LastScan` is the point: an ephemeral write must leave the index readable at its own result, so
/// a read that does not scan has to see the new shape.
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
fn an_ephemeral_update_reaches_a_block_that_states_no_id() {
    let graph = TempGraph::new("ephemeral-update-hit");
    let mut opened = open(&graph);

    let updated = opened
        .update_block(&bound(&graph, "alpha", &[0, 0, 0]), "changed")
        .expect("update the block by path");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- changed\n\t- second child\n- last\n",
        "the grandchild's own lines are replaced; every other byte stays"
    );
    // Read-your-writes: the write committed the page it edited, so the view and the index describe
    // the new revision without another scan.
    assert_eq!(updated.value().text(), "changed");
    assert_eq!(updated.value().page(), "alpha");
    assert_eq!(
        updated.value().identity(),
        logseq_graph::domain::BlockIdentityKind::Ephemeral
    );
    assert_eq!(
        updated.value().durable_id(),
        None,
        "a block without `id::` answers no durable identity"
    );
    assert_eq!(block_count_without_scan(&mut opened, "alpha"), 5);
}

#[test]
fn an_ephemeral_update_of_a_top_level_block_keeps_its_subtree() {
    let graph = TempGraph::new("ephemeral-update-root");
    let mut opened = open(&graph);

    opened
        .update_block(&bound(&graph, "alpha", &[0]), "rewritten")
        .expect("update the first root");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- rewritten\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n",
        "the root's own lines change and its children keep their indentation"
    );
}

#[test]
fn a_path_reaches_a_later_top_level_block_by_counting_its_preorder() {
    let graph = TempGraph::new("ephemeral-update-second-root");
    let mut opened = open(&graph);

    // `last` is preorder 4, not position 1: the walk has to add the four blocks of `first`'s
    // subtree to the counter. A counter that added one per root would call `last` preorder 1 and
    // miss it, which is the mistake this test exists to catch.
    opened
        .update_block(&bound(&graph, "alpha", &[4]), "rewritten last")
        .expect("update the second root");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- grandchild\n\t- second child\n- rewritten last\n",
        "the later root is reached by its preorder, and the subtree before it is untouched"
    );
}

#[test]
fn a_deeper_path_element_picks_a_sibling_position() {
    let graph = TempGraph::new("ephemeral-update-sibling");
    let mut opened = open(&graph);

    // `[0, 1]` is `first`'s second child, `second child`. The element after the root is a position
    // among that level's siblings, not a preorder: `grandchild` sits at preorder 2, and a path that
    // treated the element as a preorder would edit it instead.
    opened
        .update_block(&bound(&graph, "alpha", &[0, 1]), "rewritten second child")
        .expect("update the second child of the first root");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- grandchild\n\t- rewritten second child\n- last\n",
        "the sibling position names the second child, not the grandchild at preorder 2"
    );
}

#[test]
fn an_ephemeral_remove_takes_the_subtree_with_it() {
    let graph = TempGraph::new("ephemeral-remove-hit");
    let mut opened = open(&graph);

    let removed = opened
        .remove_block(&bound(&graph, "alpha", &[0]))
        .expect("remove the first root and its subtree");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- last\n",
        "the root, its children, and its grandchild leave the file together"
    );
    assert_eq!(removed.value().name(), "alpha");
    assert_eq!(block_count_without_scan(&mut opened, "alpha"), 1);
}

#[test]
fn an_ephemeral_id_is_stale_after_its_own_pages_write() {
    let graph = TempGraph::new("ephemeral-write-stale");
    let mut opened = open(&graph);

    // The id of `second child`, read before the write that reprojects the page.
    let stale = bound(&graph, "alpha", &[0, 1]);
    opened
        .update_block(&bound(&graph, "alpha", &[0, 0, 0]), "changed")
        .expect("the first write succeeds");

    // The same id again: the write replaced every row of the page, so the row this value named no
    // longer exists and the value names nothing. This is the contract, not a bug — a re-projection
    // mints ids, and an edit is a re-projection (ADR-0012).
    match opened.update_block(&stale, "again") {
        Err(WriteError::EphemeralNotFound { id }) => {
            assert_eq!(stale.ephemeral_id(), Some(&id));
        }
        other => panic!("expected the id to name nothing, got {other:?}"),
    }
    // The refused write changed nothing: the bytes are the first write's, and the index agrees.
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- changed\n\t- second child\n- last\n"
    );
    assert_eq!(block_count_without_scan(&mut opened, "alpha"), 5);
}

#[test]
fn a_stale_id_is_refused_before_remove_touches_anything() {
    let graph = TempGraph::new("ephemeral-remove-stale");
    let mut opened = open(&graph);

    // Read before another write reprojects the page, so by the time the removal runs it names no
    // row at all: the removal refuses instead of deleting whatever stands at that position now.
    let stale = bound(&graph, "alpha", &[0]);
    opened
        .update_block(&bound(&graph, "alpha", &[4]), "moved last")
        .expect("a write from a fresh id succeeds");

    match opened.remove_block(&stale) {
        Err(WriteError::EphemeralNotFound { .. }) => {}
        other => panic!("expected the id to name nothing, got {other:?}"),
    }
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- grandchild\n\t- second child\n- moved last\n",
        "the refused removal changed nothing"
    );
}

#[test]
fn an_ephemeral_write_to_a_page_that_is_gone_is_page_not_found() {
    let graph = TempGraph::new("ephemeral-write-page-miss");
    graph.write("pages/gone.md", "- a\n- b\n");
    let mut opened = graph.open();
    let selector = bound(&graph, "gone", &[0]);

    // Another process removes the file. Nothing has scanned since, so the row still names this page
    // and the id still names its block: the refusal is the file's absence, not the identity's.
    graph.remove("pages/gone.md");

    match opened.update_block(&selector, "nope") {
        Err(WriteError::PageNotFound { page }) => assert_eq!(page, "gone"),
        other => panic!("expected a page miss, got {other:?}"),
    }
}
