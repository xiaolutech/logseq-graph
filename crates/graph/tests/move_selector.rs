//! Block moves addressed by [`BlockSelector`]: the moved block, the anchor, or both named by an
//! ephemeral id instead of a durable one (ADR-0005, ADR-0012).
//!
//! Every test writes a graph under the system temporary directory, opens it through [`Graph`], and
//! asserts on the bytes on disk. The point of the ephemeral cases is the one a durable-only move
//! cannot reach: most blocks state no `id::`, and an editor drag-and-drop moves exactly those,
//! usually next to another block that states none either. The refusals matter as much: an id the
//! graph has moved past is refused before anything is read, and externally changed bytes fail
//! the fingerprint gate before either file of a cross-page move changes.

use logseq_graph::domain::{BlockId, BlockSelector};
use logseq_graph::engine::{MoveTarget, WriteError};

mod support;

use support::{bound, TempGraph};

/// A page whose blocks exercise several levels and no `id::` at all.
///
/// Blocks as `(preorder, depth, sibling_index)`: `first (0, 0, 0)`, `child (1, 1, 0)`,
/// `grandchild (2, 2, 0)`, `second child (3, 1, 1)`, `last (4, 0, 1)`.
const ALPHA: &str = "\
- first
\t- child
\t\t- grandchild
\t- second child
- last
";

/// The same page with one identified block, for the durable-anchor case.
const ALPHA_IDENTIFIED: &str = "\
- first
  id:: 01234567-89ab-cdef-0123-456789abcdef
  - child
- last
";

/// A second page with one identified block, for the cross-page cases.
const BETA: &str = "\
- bravo
  id:: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee
";

/// The `id::` UUID as a caller would hold it.
fn id(raw: &str) -> BlockId {
    BlockId::parse(raw).expect("valid 8-4-4-4-12 UUID")
}

/// Text of a graph file, as it is on disk.
fn read_text(graph: &TempGraph, relative: &str) -> String {
    std::fs::read_to_string(graph.graph_root().join(relative)).expect("read graph file")
}

/// Bytes of a graph file, as they are on disk.
fn read_bytes(graph: &TempGraph, relative: &str) -> Vec<u8> {
    std::fs::read(graph.graph_root().join(relative)).expect("read graph file")
}

#[test]
fn an_ephemeral_move_reaches_blocks_that_state_no_id() {
    let graph = TempGraph::new("ephemeral-move-hit");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    // Both ends are ephemeral: the second root moves to be the last child of `child`.
    opened
        .move_block(
            &bound(&graph, "alpha", &[4]),
            MoveTarget::LastChildOf(bound(&graph, "alpha", &[0, 0])),
        )
        .expect("move the second root under the child by handles alone");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- grandchild\n\t\t- last\n\t- second child\n",
        "the moved root is re-indented to the child's depth, after the child's subtree"
    );
}

#[test]
fn an_ephemeral_move_before_a_sibling_keeps_the_subtree_together() {
    let graph = TempGraph::new("ephemeral-move-sibling");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    // `first` and its two levels of children move after `last`.
    opened
        .move_block(
            &bound(&graph, "alpha", &[0]),
            MoveTarget::After(bound(&graph, "alpha", &[4])),
        )
        .expect("move the first root after the last one");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- last\n- first\n\t- child\n\t\t- grandchild\n\t- second child\n",
        "the whole subtree moves as one run at the top level"
    );
}

#[test]
fn an_ephemeral_moved_block_with_a_durable_anchor_moves_next_to_the_id() {
    let graph = TempGraph::new("ephemeral-move-durable-anchor");
    graph.write("pages/alpha.md", ALPHA_IDENTIFIED);
    let mut opened = graph.open();

    // The moved block is ephemeral (`last`, preorder 2 — `first`'s child is preorder 1); the
    // anchor is the identified `first`. The child indent follows alpha's own two-space unit.
    opened
        .move_block(
            &bound(&graph, "alpha", &[2]),
            MoveTarget::Before(BlockSelector::Durable(id(
                "01234567-89ab-cdef-0123-456789abcdef",
            ))),
        )
        .expect("move the unidentified block before the identified one");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- last\n- first\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n  - child\n",
        "the identified block's identity line and its child survive beside the moved root"
    );
}

#[test]
fn an_ephemeral_anchor_without_an_id_can_anchor_a_cross_page_move() {
    let graph = TempGraph::new("move-cross-ephemeral-anchor-no-id");
    graph.write("pages/alpha.md", ALPHA_IDENTIFIED);
    graph.write("pages/beta.md", "- plain anchor\n");
    let mut opened = graph.open();
    opened
        .move_block(
            &BlockSelector::Durable(id("01234567-89ab-cdef-0123-456789abcdef")),
            MoveTarget::Before(bound(&graph, "beta", &[0])),
        )
        .expect("move beside an ephemeral anchor");
    assert_eq!(read_text(&graph, "pages/alpha.md"), "- last\n");
    assert_eq!(
        read_text(&graph, "pages/beta.md"),
        "- first\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n  - child\n- plain anchor\n"
    );
}

#[test]
fn an_ephemeral_moved_block_can_move_across_pages_without_granting_identity() {
    let graph = TempGraph::new("ephemeral-move-cross-refused");
    graph.write("pages/alpha.md", ALPHA);
    graph.write("pages/beta.md", BETA);
    let mut opened = graph.open();
    opened
        .move_block(
            &bound(&graph, "alpha", &[4]),
            MoveTarget::After(BlockSelector::Durable(id(
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            ))),
        )
        .expect("move an ephemeral block across pages");
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        ALPHA.trim_end_matches("- last\n")
    );
    assert_eq!(
        read_text(&graph, "pages/beta.md"),
        format!("{BETA}- last\n")
    );
}

#[test]
fn ephemeral_blocks_move_between_pages_and_stale_coordinates_change_neither_file() {
    for stale_source in [false, true] {
        let fixture = TempGraph::new("ephemeral-cross-both");
        fixture.write("pages/alpha.md", ALPHA);
        fixture.write("pages/beta.md", "- parent\n\t- anchor\n");
        let mut graph = fixture.open();
        let moved = bound(&fixture, "alpha", &[0, 0]);
        let anchor = bound(&fixture, "beta", &[0, 0]);
        if stale_source {
            fixture.write("pages/alpha.md", format!("- external\n{ALPHA}"));
        }
        let before_source = read_bytes(&fixture, "pages/alpha.md");
        let before_destination = read_bytes(&fixture, "pages/beta.md");
        let result = graph.move_block(&moved, MoveTarget::After(anchor));
        if stale_source {
            assert!(matches!(result, Err(WriteError::StaleVersion { .. })));
            assert_eq!(read_bytes(&fixture, "pages/alpha.md"), before_source);
            assert_eq!(read_bytes(&fixture, "pages/beta.md"), before_destination);
        } else {
            result.expect("move with ephemeral endpoints");
            assert_eq!(
                read_text(&fixture, "pages/alpha.md"),
                "- first\n\t- second child\n- last\n"
            );
            assert_eq!(
                read_text(&fixture, "pages/beta.md"),
                "- parent\n\t- anchor\n\t- child\n\t\t- grandchild\n"
            );
        }
        assert!(!fixture.graph_root().join("write-intent.json").exists());
    }
}

#[test]
fn an_id_the_graph_has_moved_past_is_refused_before_a_move_touches_anything() {
    let graph = TempGraph::new("ephemeral-move-stale");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    // The id of `last`, read before the move that reprojects the whole page.
    let stale = bound(&graph, "alpha", &[4]);
    opened
        .move_block(
            &bound(&graph, "alpha", &[0]),
            MoveTarget::After(bound(&graph, "alpha", &[4])),
        )
        .expect("a move from fresh ids succeeds");

    match opened.move_block(&stale, MoveTarget::After(bound(&graph, "alpha", &[0]))) {
        Err(WriteError::EphemeralNotFound { .. }) => {}
        other => panic!("expected the id to name nothing, got {other:?}"),
    }
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- last\n- first\n\t- child\n\t\t- grandchild\n\t- second child\n",
        "the refused move changed nothing"
    );
}

#[test]
fn an_anchor_id_the_graph_has_moved_past_is_refused_before_a_move_touches_anything() {
    let graph = TempGraph::new("ephemeral-move-stale-anchor");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    // The anchor's id is read before the move that reprojects the page, so it is gone by the time
    // the second call runs: the anchor is refused exactly as the moved block would be.
    let stale_anchor = bound(&graph, "alpha", &[4]);
    opened
        .move_block(
            &bound(&graph, "alpha", &[0]),
            MoveTarget::After(bound(&graph, "alpha", &[4])),
        )
        .expect("a move from fresh ids succeeds");

    match opened.move_block(
        &bound(&graph, "alpha", &[0]),
        MoveTarget::After(stale_anchor),
    ) {
        Err(WriteError::EphemeralNotFound { .. }) => {}
        other => panic!("expected the anchor id to name nothing, got {other:?}"),
    }
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- last\n- first\n\t- child\n\t\t- grandchild\n\t- second child\n",
        "the refused move changed nothing"
    );
}
