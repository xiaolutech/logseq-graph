//! Inserting one new block at a position named by [`MoveTarget`], and the selector it answers.
//!
//! Every test writes a graph under the system temporary directory, opens it through [`Graph`], and
//! asserts on the bytes on disk: an insertion is one added line at a place the four target kinds
//! define, so the page's full text is the only assertion that pins the position down. The point of
//! most of the file is the second half of the answer: the write returns the new block's selector,
//! and that selector has to be usable immediately (it is the reason this write exists), refused once
//! the graph has moved past it (ADR-0009's fail-closed rule), and durable when the text the caller
//! wrote states an `id::`.

use logseq_graph::document::EditError;
use logseq_graph::domain::{BlockId, BlockSelector};
use logseq_graph::engine::{Freshness, Graph, MoveTarget, WriteError};

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

/// The `id::` UUID the durable-insert test writes, and the one a durable read uses.
const ID_A: &str = "11111111-1111-1111-1111-111111111111";

/// Open the graph holding `ALPHA` on page `alpha`.
fn open(graph: &TempGraph) -> Graph {
    graph.write("pages/alpha.md", ALPHA);
    graph.open()
}

/// Text of a graph file, as it is on disk.
fn read_text(graph: &TempGraph, relative: &str) -> String {
    std::fs::read_to_string(graph.graph_root().join(relative)).expect("read graph file")
}

#[test]
fn before_inserts_a_sibling_above_the_anchor() {
    let graph = TempGraph::new("insert-before");
    let mut opened = open(&graph);

    opened
        .insert_block(MoveTarget::Before(bound(&graph, "alpha", &[0])), "new")
        .expect("insert before the first block");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- new\n- first\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n"
    );
}

#[test]
fn after_inserts_a_sibling_after_the_anchor() {
    let graph = TempGraph::new("insert-after");
    let mut opened = open(&graph);

    opened
        .insert_block(MoveTarget::After(bound(&graph, "alpha", &[4])), "new")
        .expect("insert after the last block");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n- new\n"
    );
}

#[test]
fn after_skips_the_anchor_subtree_instead_of_landing_inside_it() {
    let graph = TempGraph::new("insert-after-subtree");
    let mut opened = open(&graph);

    // `first` has a child and a grandchild. "After first" is after all of that, as the next
    // top-level sibling — not the first child's position, which is where `first`'s own lines end.
    opened
        .insert_block(MoveTarget::After(bound(&graph, "alpha", &[0])), "new")
        .expect("insert after the whole subtree");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- grandchild\n\t- second child\n- new\n- last\n"
    );
}

#[test]
fn first_child_of_lands_above_the_existing_children() {
    let graph = TempGraph::new("insert-first-child");
    let mut opened = open(&graph);

    opened
        .insert_block(
            MoveTarget::FirstChildOf(bound(&graph, "alpha", &[0])),
            "new",
        )
        .expect("insert as the first child");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- new\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n"
    );
}

#[test]
fn last_child_of_lands_below_the_existing_children() {
    let graph = TempGraph::new("insert-last-child");
    let mut opened = open(&graph);

    opened
        .insert_block(MoveTarget::LastChildOf(bound(&graph, "alpha", &[0])), "new")
        .expect("insert as the last child");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- grandchild\n\t- second child\n\t- new\n- last\n"
    );
}

#[test]
fn the_returned_selector_edits_the_new_block_right_away() {
    let graph = TempGraph::new("insert-handle-usable");
    let mut opened = open(&graph);

    let inserted = opened
        .insert_block(MoveTarget::After(bound(&graph, "alpha", &[4])), "new")
        .expect("insert a new last block");

    // The text states no `id::`, so the answer is the ephemeral kind: the id the projection minted
    // for the block this write created (ADR-0012).
    assert!(
        matches!(inserted.value(), BlockSelector::Ephemeral(_)),
        "a block without an id must answer an ephemeral id, got {:?}",
        inserted.value()
    );

    let updated = opened
        .update_block(inserted.value(), "edited")
        .expect("edit the block the insertion just created");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n- edited\n"
    );
    assert_eq!(updated.value().text(), "edited");
    // Read-your-writes: the update committed a new revision, so the row it answers with reads the
    // edit back without another scan. The insertion's own id is the write's casualty — the page was
    // reprojected — so it names nothing any more, which is what the next test pins down.
    let reread = opened
        .reads(Freshness::LastScan)
        .block(updated.value().selector())
        .expect("read the new block back");
    assert_eq!(
        reread.present().expect("the block the answer names").text(),
        "edited",
        "the index must hold the edit without another scan"
    );
    assert!(
        opened
            .reads(Freshness::LastScan)
            .block(inserted.value())
            .expect("an id no row holds is an answer")
            .present()
            .is_none(),
        "the id from before the edit names nothing after it"
    );
}

#[test]
fn the_returned_selector_is_refused_after_another_write() {
    let graph = TempGraph::new("insert-id-stale");
    let mut opened = open(&graph);

    let inserted = opened
        .insert_block(MoveTarget::After(bound(&graph, "alpha", &[4])), "new")
        .expect("insert a new last block");
    assert!(matches!(inserted.value(), BlockSelector::Ephemeral(_)));

    // Another write to the same page reprojects it, and a re-projection mints every ephemeral id of
    // the page again, so the id this insertion answered with names nothing from here on.
    opened
        .append_block("alpha", "appended")
        .expect("append one more block");

    match opened.update_block(inserted.value(), "edited") {
        Err(WriteError::EphemeralNotFound { id }) => {
            assert_eq!(Some(&id), inserted.value().ephemeral_id());
        }
        other => panic!("expected the id to name nothing, got {other:?}"),
    }
    // The refused write changed nothing.
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n- new\n- appended\n"
    );
}

#[test]
fn text_that_states_an_id_answers_a_durable_selector() {
    let graph = TempGraph::new("insert-durable");
    let mut opened = open(&graph);

    // Logseq writes an `id::` as the block's own line, and the parser reads that line as a property
    // of the block, so a one-line insertion can carry durable identity exactly as an editor does.
    let inserted = opened
        .insert_block(
            MoveTarget::After(bound(&graph, "alpha", &[4])),
            &format!("id:: {ID_A}"),
        )
        .expect("insert a block that states an id");

    let BlockSelector::Durable(id) = inserted.value() else {
        panic!("a block that states an id must answer a durable selector");
    };
    assert_eq!(id.as_str(), ID_A);

    // The id reads the same block back, and that block is the one the insertion wrote.
    let read = opened
        .reads(Freshness::LastScan)
        .block(&BlockSelector::Durable(id.clone()))
        .expect("read the new block by its id");
    let view = read.present().expect("the block that states the id");
    assert_eq!(view.text(), format!("id:: {ID_A}"));
    assert_eq!(
        view.durable_id(),
        Some(&BlockId::parse(ID_A).expect("valid UUID"))
    );
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n- id:: 11111111-1111-1111-1111-111111111111\n"
    );
}

#[test]
fn empty_and_multiline_content_are_refused() {
    let graph = TempGraph::new("insert-refused-content");
    let mut opened = open(&graph);

    for (text, expected) in [
        ("", EditError::EmptyContent),
        ("a\nb", EditError::MultilineContent),
    ] {
        match opened.insert_block(MoveTarget::After(bound(&graph, "alpha", &[4])), text) {
            Err(WriteError::Edit(error)) => assert_eq!(error, expected),
            other => panic!("expected the edit to be refused, got {other:?}"),
        }
    }
    // Neither refusal read or wrote a file.
    assert_eq!(read_text(&graph, "pages/alpha.md"), ALPHA);
}

#[test]
fn a_missing_anchor_is_a_move_target_miss_not_a_block_miss() {
    let graph = TempGraph::new("insert-missing-anchor");
    // The index projects this id, so the durable anchor resolves to this page and no scan runs.
    graph.write(
        "pages/alpha.md",
        format!("- first\n  id:: {ID_A}\n- last\n"),
    );
    let mut opened = graph.open();
    let id = BlockId::parse(ID_A).expect("valid UUID");

    // The file no longer states the id the index row remembers. The anchor's page is still found —
    // the row is what sent the write there — and the edit is what refuses, so the miss is the
    // target's: `MoveTargetNotFound`, not `BlockNotFound`.
    graph.write("pages/alpha.md", "- first\n- last\n");

    match opened.insert_block(MoveTarget::After(BlockSelector::Durable(id)), "new") {
        Err(WriteError::Edit(EditError::MoveTargetNotFound)) => {}
        other => panic!("expected a move target miss, got {other:?}"),
    }
    // The refusal changed nothing: the file is the externally written text, untouched.
    assert_eq!(read_text(&graph, "pages/alpha.md"), "- first\n- last\n");
}

#[test]
fn the_new_line_takes_the_documents_own_indent_unit() {
    // A tab-indented page gains a tab, not two spaces: the unit is read from the document, never
    // assumed (LESSONS.md records the earlier version of this bug).
    let tabs = TempGraph::new("insert-indent-tabs");
    tabs.write("pages/alpha.md", ALPHA);
    let mut opened = tabs.open();
    opened
        .insert_block(MoveTarget::FirstChildOf(bound(&tabs, "alpha", &[0])), "new")
        .expect("insert as the first child");
    assert_eq!(
        read_text(&tabs, "pages/alpha.md"),
        "- first\n\t- new\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n"
    );

    // A two-space page gains two spaces, from the same rule.
    let spaces = TempGraph::new("insert-indent-spaces");
    spaces.write("pages/alpha.md", "- first\n  - child\n- last\n");
    let mut opened = spaces.open();
    opened
        .insert_block(
            MoveTarget::FirstChildOf(bound(&spaces, "alpha", &[0])),
            "new",
        )
        .expect("insert as the first child");
    assert_eq!(
        read_text(&spaces, "pages/alpha.md"),
        "- first\n  - new\n  - child\n- last\n"
    );
}

#[test]
fn inserting_on_one_page_leaves_every_other_page_byte_identical() {
    let graph = TempGraph::new("insert-one-page");
    graph.write("pages/alpha.md", ALPHA);
    graph.write("pages/beta.md", "- beta one\n- beta two\n");
    let mut opened = graph.open();
    let alpha_before = read_text(&graph, "pages/alpha.md");

    // The anchor is on beta, so the new block is beta's; alpha is not this write's to touch.
    opened
        .insert_block(MoveTarget::Before(bound(&graph, "beta", &[0])), "new")
        .expect("insert on the second page");

    assert_eq!(
        read_text(&graph, "pages/beta.md"),
        "- new\n- beta one\n- beta two\n"
    );
    assert_eq!(read_text(&graph, "pages/alpha.md"), alpha_before);
}

#[test]
fn an_insertion_point_inside_an_unclosed_region_is_refused() {
    let graph = TempGraph::new("insert-opaque-region");
    // The block's own lines open a BEGIN region that never closes, so the page ends inside it: inserting
    // after the block would paste the new line into that region's content, not between blocks.
    const FENCED: &str = "- a\n  #+BEGIN_SRC\ncode\n";
    graph.write("pages/alpha.md", FENCED);
    let mut opened = graph.open();

    match opened.insert_block(MoveTarget::After(bound(&graph, "alpha", &[0])), "new") {
        Err(WriteError::Edit(EditError::EndsInsideOpaqueRegion)) => {}
        other => panic!("expected the opaque region to be refused, got {other:?}"),
    }
    assert_eq!(read_text(&graph, "pages/alpha.md"), FENCED);
}

#[test]
fn an_unclosed_code_fence_does_not_hide_an_inserted_sibling() {
    let graph = TempGraph::new("insert-unclosed-fence");
    graph.write("pages/alpha.md", "- a\n  ```\ncode\n");
    let mut opened = graph.open();
    opened
        .insert_block(MoveTarget::After(bound(&graph, "alpha", &[0])), "new")
        .expect("an unmatched code fence leaves sibling markers visible");
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- a\n  ```\ncode\n- new\n"
    );
}
