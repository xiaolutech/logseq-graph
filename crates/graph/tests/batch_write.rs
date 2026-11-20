//! The three editor write completions: prepending to a page, setting a block's folded state, and
//! inserting a batch of blocks in one page replacement.
//!
//! Every test writes a graph under the system temporary directory, opens it through [`Graph`], and
//! asserts on the bytes on disk, exactly as `insert_block.rs` and `block_property_write.rs` do: the
//! page's full text is the only assertion that pins a position, a replaced line, or an order down.
//! The batch's one-replacement contract is asserted with indirect evidence — every answered handle
//! binds the same revision, which a loop of inserts would have advanced once per block.

use logseq_graph::document::EditError;
use logseq_graph::domain::{BlockSelector, GraphMetadata};
use logseq_graph::engine::{Graph, MoveTarget, WriteError};

mod support;

use support::{bound, TempGraph};

/// A page whose blocks exercise several levels and no `id::` at all.
const ALPHA: &str = "\
- first
\t- child
\t\t- grandchild
\t- second child
- last
";

/// A page with only a preamble property, no blocks: the shape Logseq writes for a page that holds
/// page-level properties and nothing else.
const PREAMBLE_ONLY: &str = "title:: alpha\n\n";

/// The metadata a freshly opened graph reports, which is what a handle is bound to.
fn meta_of(graph: &mut Graph) -> GraphMetadata {
    graph.metadata().expect("read metadata").into_value()
}

/// Open the graph holding `ALPHA` on page `alpha`.
fn open(graph: &TempGraph) -> Graph {
    graph.write("pages/alpha.md", ALPHA);
    graph.open()
}

/// Text of a graph file, as it is on disk.
fn read_text(graph: &TempGraph, relative: &str) -> String {
    std::fs::read_to_string(graph.graph_root().join(relative)).expect("read graph file")
}

/// The revision one `Read` observed, for the one-replacement assertions.
fn revision_of<T>(read: &logseq_graph::domain::Read<T>) -> u64 {
    read.meta().revision()
}

// --- prepend_block_in_page -------------------------------------------------

#[test]
fn prepend_lands_above_the_first_block_of_a_page_that_has_one() {
    let graph = TempGraph::new("prepend-first");
    let mut opened = open(&graph);

    opened
        .prepend_block_in_page("alpha", "new")
        .expect("prepend to a page with blocks");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- new\n- first\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n"
    );
}

#[test]
fn prepend_on_a_page_without_blocks_makes_the_first_block() {
    let graph = TempGraph::new("prepend-empty-page");
    graph.write("pages/alpha.md", "");
    let mut opened = graph.open();

    opened
        .prepend_block_in_page("alpha", "only")
        .expect("prepend to an empty page");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- only",
        "the page's first block is the one the prepend wrote; an empty document has no final newline, and the append preserves that"
    );
}

#[test]
fn prepend_keeps_the_preamble_of_a_page_with_no_blocks() {
    let graph = TempGraph::new("prepend-preamble-page");
    graph.write("pages/alpha.md", PREAMBLE_ONLY);
    let mut opened = graph.open();

    opened
        .prepend_block_in_page("alpha", "only")
        .expect("prepend to a preamble-only page");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "title:: alpha\n- only\n\n",
        "the preamble stays ahead of the page's first block, and the page's trailing blank line stays last"
    );
}

#[test]
fn the_prepend_answer_edits_the_new_block_right_away() {
    let graph = TempGraph::new("prepend-handle-usable");
    let mut opened = open(&graph);

    let prepended = opened
        .prepend_block_in_page("alpha", "new")
        .expect("prepend one block");

    assert!(
        matches!(prepended.value(), BlockSelector::Ephemeral(_)),
        "a block without an id must answer an ephemeral id, got {:?}",
        prepended.value()
    );

    let updated = opened
        .update_block(prepended.value(), "edited")
        .expect("edit the block the prepend just created");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- edited\n- first\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n"
    );
    assert_eq!(updated.value().text(), "edited");
}

#[test]
fn prepend_refuses_illegal_text_and_touches_nothing() {
    let graph = TempGraph::new("prepend-refused");
    let mut opened = open(&graph);

    for (text, expected) in [
        ("", EditError::EmptyContent),
        ("a\nb", EditError::MultilineContent),
    ] {
        match opened.prepend_block_in_page("alpha", text) {
            Err(WriteError::Edit(error)) => assert_eq!(error, expected),
            other => panic!("expected the edit to be refused, got {other:?}"),
        }
    }
    assert_eq!(read_text(&graph, "pages/alpha.md"), ALPHA);
}

// --- set_block_collapsed ---------------------------------------------------

#[test]
fn collapsed_true_writes_the_property_line() {
    let graph = TempGraph::new("collapse-on");
    let mut opened = open(&graph);

    opened
        .set_block_collapsed(&bound(&graph, "alpha", &[0]), true)
        .expect("fold the first block");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t\tcollapsed:: true\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n",
        "the property line aligns to the block's content column, counted in the document's own tab unit"
    );
}

#[test]
fn collapsed_false_replaces_the_line_instead_of_adding_one() {
    let graph = TempGraph::new("collapse-off");
    let mut opened = open(&graph);

    opened
        .set_block_collapsed(&bound(&graph, "alpha", &[0]), true)
        .expect("fold the first block");
    // The fold advanced the revision, so the unfold rebinds the handle to the fold's commit.
    opened
        .set_block_collapsed(&bound(&graph, "alpha", &[0]), false)
        .expect("unfold the first block");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t\tcollapsed:: false\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n",
        "the old line is replaced, not joined by a second collapsed:: line"
    );
}

#[test]
fn the_folded_state_survives_an_update_of_the_block_text() {
    let graph = TempGraph::new("collapse-survives-update");
    let mut opened = open(&graph);

    let folded = opened
        .set_block_collapsed(&bound(&graph, "alpha", &[0]), true)
        .expect("fold the first block");

    // The address the fold answered is the row its own replacement committed, so the next edit
    // needs no read in between: this is the contract every single-block write shares (ADR-0012).
    opened
        .update_block(folded.value().selector(), "first, edited")
        .expect("edit the folded block's text");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first, edited\n\t\tcollapsed:: true\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n",
        "update_block preserves collapsed:: the way it preserves id::"
    );
}

#[test]
fn a_page_that_never_held_the_key_writes_it_in_the_blocks_own_place() {
    let graph = TempGraph::new("collapse-into-nested");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    // `child` is the first child of the first top-level block, at path [0, 0].
    opened
        .set_block_collapsed(&bound(&graph, "alpha", &[0, 0]), true)
        .expect("fold the child block");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t\tcollapsed:: true\n\t\t- grandchild\n\t- second child\n- last\n"
    );
}

// --- insert_batch ----------------------------------------------------------

#[test]
fn a_batch_keeps_the_order_of_the_texts() {
    let graph = TempGraph::new("batch-order");
    let mut opened = open(&graph);

    opened
        .insert_batch(
            MoveTarget::Before(bound(&graph, "alpha", &[0])),
            &["a", "b", "c"],
        )
        .expect("insert three blocks before the first");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- a\n- b\n- c\n- first\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n",
        "the texts must come out in the order given, not reversed"
    );
}

#[test]
fn a_batch_after_the_last_block_appends_the_run_in_order() {
    let graph = TempGraph::new("batch-after-last");
    let mut opened = open(&graph);

    opened
        .insert_batch(MoveTarget::After(bound(&graph, "alpha", &[4])), &["a", "b"])
        .expect("insert two blocks after the last");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n- a\n- b\n"
    );
}

#[test]
fn a_batch_is_one_page_replacement_not_one_per_block() {
    let graph = TempGraph::new("batch-one-replacement");
    let mut opened = open(&graph);
    let meta = meta_of(&mut opened);

    let batch = opened
        .insert_batch(
            MoveTarget::Before(bound(&graph, "alpha", &[0])),
            &["a", "b", "c"],
        )
        .expect("insert three blocks");

    // The evidence for one replacement is the revision: a commit advances it exactly once, so three
    // blocks inserted in one write land at the next revision and not three revisions later. A loop
    // of inserts would have answered at `meta.revision() + 3`.
    assert_eq!(
        revision_of(&batch),
        meta.revision() + 1,
        "three blocks, one page replacement, one revision advance"
    );

    // Every answer is a distinct ephemeral id, because each names one block of the run and the
    // projection mints them one per block (ADR-0012).
    let ids: std::collections::HashSet<&str> = batch
        .value()
        .iter()
        .map(|selector| match selector {
            BlockSelector::Ephemeral(id) => id.as_str(),
            BlockSelector::Durable(_) => panic!("a plain text must answer an ephemeral id"),
        })
        .collect();
    assert_eq!(ids.len(), 3, "each block of the run has its own identity");
    assert!(batch
        .value()
        .iter()
        .all(|selector| matches!(selector, BlockSelector::Ephemeral(_))));
}

#[test]
fn one_illegal_block_refuses_the_whole_batch() {
    let graph = TempGraph::new("batch-refused");
    let mut opened = open(&graph);

    for texts in [vec!["ok", ""], vec!["a\nb", "ok"], vec!["ok", "   "]] {
        match opened.insert_batch(MoveTarget::Before(bound(&graph, "alpha", &[0])), &texts) {
            Err(WriteError::Edit(_)) => {}
            other => panic!("expected the batch to be refused, got {other:?}"),
        }
    }
    // Not one byte moved: every refusal happened before the file was read for writing.
    assert_eq!(read_text(&graph, "pages/alpha.md"), ALPHA);
}

#[test]
fn a_batch_of_one_is_a_single_insert() {
    let graph = TempGraph::new("batch-of-one");
    let mut opened = open(&graph);

    let batch = opened
        .insert_batch(MoveTarget::Before(bound(&graph, "alpha", &[0])), &["only"])
        .expect("insert one block");

    // The batch advanced the revision, so the anchor is read from the projection the batch just
    // committed: `first` is now preorder 1, and a single insert before it is the same edit.
    let anchor = bound(&graph, "alpha", &[1]);
    let direct = opened
        .insert_block(MoveTarget::Before(anchor), "only")
        .expect("insert the same block the single-insert way");

    // Same bytes, same answered path: one text through either door lands in the same place.
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- only\n- only\n- first\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n"
    );
    // Both writes answered a block of their own, so their ids are different: an id names one block,
    // never a position two different writes happened to share.
    assert_ne!(batch.value()[0], *direct.value());
}

#[test]
fn an_empty_batch_writes_nothing_and_answers_an_empty_run() {
    let graph = TempGraph::new("batch-empty");
    let mut opened = open(&graph);
    let meta = meta_of(&mut opened);

    let batch = opened
        .insert_batch(MoveTarget::Before(bound(&graph, "alpha", &[0])), &[])
        .expect("insert nothing");

    assert!(batch.value().is_empty());
    assert_eq!(
        batch.meta().revision(),
        meta.revision(),
        "no write means no revision advance"
    );
    assert_eq!(read_text(&graph, "pages/alpha.md"), ALPHA);
}

#[test]
fn an_anchor_id_that_names_nothing_refuses_the_batch_and_touches_nothing() {
    let graph = TempGraph::new("batch-missing-anchor");
    let mut opened = open(&graph);

    // The anchor is an ephemeral id no row holds, which is what a stale value looks like. The
    // refusal comes before the file is read, so the batch cannot land anywhere.
    let stale = bound(&graph, "alpha", &[0]);
    opened
        .append_block("alpha", "appended")
        .expect("a write that reprojects the page");
    match opened.insert_batch(MoveTarget::After(stale), &["a", "b"]) {
        Err(WriteError::EphemeralNotFound { .. }) => {}
        other => panic!("expected the anchor id to name nothing, got {other:?}"),
    }
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n- appended\n",
        "the refused batch left the appended page alone"
    );
}

#[test]
fn the_first_answered_selector_edits_the_first_block_of_the_run() {
    let graph = TempGraph::new("batch-selector-usable");
    let mut opened = open(&graph);

    let batch = opened
        .insert_batch(
            MoveTarget::Before(bound(&graph, "alpha", &[0])),
            &["a", "b", "c"],
        )
        .expect("insert three blocks");

    // Editing through the first selector must reach the first block of the run, not one of the
    // later ones: the selectors are minted from the projection, and a reversed or shifted run
    // would put the edit on the wrong block.
    opened
        .update_block(&batch.value()[0], "edited a")
        .expect("edit the first block of the run");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- edited a\n- b\n- c\n- first\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n"
    );
}

#[test]
fn a_batch_answers_durable_selectors_for_texts_that_state_ids() {
    let graph = TempGraph::new("batch-durable");
    let mut opened = open(&graph);

    let batch = opened
        .insert_batch(
            MoveTarget::After(bound(&graph, "alpha", &[4])),
            &["id:: 11111111-1111-1111-1111-111111111111", "plain"],
        )
        .expect("insert two blocks, the first carrying an id");

    let BlockSelector::Durable(id) = &batch.value()[0] else {
        panic!("a block that states an id must answer a durable selector");
    };
    assert_eq!(id.as_str(), "11111111-1111-1111-1111-111111111111");
    assert!(matches!(batch.value()[1], BlockSelector::Ephemeral(_)));
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n- id:: 11111111-1111-1111-1111-111111111111\n- plain\n"
    );
}
