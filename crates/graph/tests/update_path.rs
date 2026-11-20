//! The update path: one durable id, one block's text, through the engine facade.
//!
//! Every test writes a graph under the system temporary directory, opens it through [`Graph`], and
//! asserts on the bytes on disk as well as on the projection. The bytes are the point of most of
//! them: an update replaces one block's own lines, so everything else in the file — the marker, the
//! `id::` line, the children, the BOM, the line endings, a missing final newline, and every other
//! page — has to come out unchanged. The tests that matter most are the ones about what must *not*
//! happen: a refused edit must not touch the file, a miss must not create or rewrite anything, and an
//! update must not lose the identity it was addressed by.

use std::fs;
use std::time::Duration;

use logseq_graph::document::EditError;
use logseq_graph::domain::{BlockId, BlockSelector};
use logseq_graph::engine::{Freshness, Graph, WriteError};
use logseq_graph::source::WriteLock;

mod support;

use support::TempGraph;

/// The durable id the fixtures below state.
const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

/// A page with one identified block that has two children, and a sibling after it.
///
/// Blocks as `(preorder, depth, sibling_index)`: `first (0, 0, 0)`, `child one (1, 1, 0)`,
/// `child two (2, 1, 1)`, `last (3, 0, 1)`. The id is written lowercase on purpose, so an assertion
/// on the block text can tell the file's spelling apart from the projection's normalization.
const ALPHA: &str = "\
- first
  id:: 01234567-89ab-cdef-0123-456789abcdef
  - child one
  - child two
- last
";

/// A page holding one identified block and nothing else.
const SOLO: &str = "\
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

/// Bytes of a graph file, as they are on disk.
fn read_bytes(graph: &TempGraph, relative: &str) -> Vec<u8> {
    fs::read(graph.graph_root().join(relative)).expect("read graph file")
}

/// Text of a graph file, as it is on disk.
fn read_text(graph: &TempGraph, relative: &str) -> String {
    String::from_utf8(read_bytes(graph, relative)).expect("graph file is UTF-8")
}

/// Block count of a page, read without scanning the files again.
///
/// `LastScan` is the point: an update must leave the index readable at its own result, so a read that
/// does not scan has to see the new text and the unchanged shape.
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
fn an_updated_block_reaches_the_file_and_the_next_read() {
    let graph = TempGraph::new("update-visible");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    let updated = opened
        .update_block(&durable(), "changed")
        .expect("update the block");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- changed\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n  - child one\n  - child two\n- last\n"
    );
    // Read-your-writes: no scan after the update, so the index itself already holds the new text.
    assert_eq!(
        updated.value().text(),
        "changed\nid:: 01234567-89ab-cdef-0123-456789abcdef"
    );
    assert_eq!(updated.value().page(), "alpha");
    assert_eq!(
        opened
            .reads(Freshness::LastScan)
            .block(&durable())
            .expect("query by durable id")
            .present()
            .expect("the block that states the id")
            .text(),
        "changed\nid:: 01234567-89ab-cdef-0123-456789abcdef",
        "the index must hold the new text without another scan"
    );
}

#[test]
fn one_update_advances_the_revision_exactly_once_and_keeps_the_block_count() {
    let graph = TempGraph::new("update-revision");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    let before = opened.metadata().expect("read metadata").meta().revision();
    let updated = opened
        .update_block(&durable(), "changed")
        .expect("update the block");

    assert_eq!(
        updated.meta().revision(),
        before + 1,
        "the write commits one revision"
    );
    assert_eq!(
        opened.metadata().expect("read metadata").meta().revision(),
        before + 1,
        "the scan before the write changed nothing, so only the replacement may advance"
    );
    assert_eq!(block_count_without_scan(&mut opened, "alpha"), 4);
}

#[test]
fn the_identity_survives_the_update() {
    let graph = TempGraph::new("update-identity");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    opened
        .update_block(&durable(), "changed")
        .expect("update the block");

    // The most important assertion in this file: editing the text must not delete the `id::` line.
    // A lost id breaks every `((...))` that points at the block and makes the block unreachable by
    // the address this call was given.
    assert!(
        read_text(&graph, "pages/alpha.md").contains("id:: 01234567-89ab-cdef-0123-456789abcdef"),
        "the file must still state the id"
    );
    let read = opened.block(&durable()).expect("query by durable id");
    let view = read.present().expect("the block that states the id");
    assert_eq!(
        view.text(),
        "changed\nid:: 01234567-89ab-cdef-0123-456789abcdef"
    );
}

#[test]
fn the_children_of_the_updated_block_are_untouched() {
    let graph = TempGraph::new("update-children");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    opened
        .update_block(&durable(), "changed")
        .expect("update the block");

    let tree = opened
        .reads(Freshness::LastScan)
        .page_tree("alpha")
        .expect("query the page tree")
        .present()
        .expect("the page is projected")
        .clone();
    let parent = &tree.blocks()[0];
    assert_eq!(parent.preorder(), 0);
    assert_eq!(parent.depth(), 0);
    assert_eq!(parent.children().len(), 2, "both children must survive");
    assert_eq!(parent.children()[0].preorder(), 1);
    assert_eq!(parent.children()[1].preorder(), 2);
    assert_eq!(parent.children()[0].depth(), 1);
    assert_eq!(parent.children()[1].depth(), 1);
    assert_eq!(tree.blocks().len(), 2, "the page keeps its two roots");
    assert_eq!(
        tree.blocks()[1].preorder(),
        3,
        "the sibling keeps its position"
    );

    // The child lines are copied byte for byte, so their indentation and text are unchanged.
    let text = read_text(&graph, "pages/alpha.md");
    assert!(
        text.contains("  - child one\n  - child two\n"),
        "the children must keep their own lines, got {text:?}"
    );
}

#[test]
fn an_unknown_id_is_an_error_and_leaves_the_file_unchanged() {
    let graph = TempGraph::new("update-missing");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();
    let before = read_bytes(&graph, "pages/alpha.md");

    let failure = opened
        .update_block(
            &BlockSelector::Durable(
                BlockId::parse("00000000-0000-0000-0000-000000000000").expect("valid UUID"),
            ),
            "changed",
        )
        .expect_err("an id no block states must fail");

    assert!(
        matches!(&failure, WriteError::BlockNotFound { id } if id.as_str() == "00000000-0000-0000-0000-000000000000"),
        "expected BlockNotFound, got {failure:?}"
    );
    assert_eq!(
        read_bytes(&graph, "pages/alpha.md"),
        before,
        "a write that found nothing must not touch the file"
    );
}

#[test]
fn an_id_the_file_dropped_is_refused_by_the_edit_rather_than_the_lookup() {
    // The index still holds the row, because the external edit below kept the size and the write no
    // longer scans before it starts. The row is enough to find the right file — the name it carries is
    // correct — and the file then refuses the edit, because it no longer states the id. That is the
    // accurate report: the block is gone from the file, and the stale row did not invent one.
    let graph = TempGraph::new("update-dropped-id");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    // An external edit that removes the identity line, as a person deleting it in an editor would.
    graph.write(
        "pages/alpha.md",
        "- first\n  - child one\n  - child two\n- last\n",
    );
    let before = read_bytes(&graph, "pages/alpha.md");

    let failure = opened
        .update_block(&durable(), "changed")
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
fn two_blocks_stating_one_id_are_refused_as_a_corrupt_file() {
    let graph = TempGraph::new("update-ambiguous");
    // A durable id names exactly one block (ADR-0005). A file where two claim it is corrupt, and
    // picking either one would write the edit into an arbitrary place, so the edit is refused.
    graph.write(
        "pages/alpha.md",
        "- one\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n- two\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n",
    );
    let mut opened = graph.open();
    let before = read_bytes(&graph, "pages/alpha.md");

    let failure = opened
        .update_block(&durable(), "changed")
        .expect_err("an ambiguous id must be refused");

    assert!(
        matches!(&failure, WriteError::Edit(EditError::AmbiguousBlockId)),
        "expected Edit(AmbiguousBlockId), got {failure:?}"
    );
    assert_eq!(
        read_bytes(&graph, "pages/alpha.md"),
        before,
        "a refused edit must not touch the file"
    );
}

#[test]
fn an_id_the_index_has_never_seen_is_found_by_one_fallback_scan() {
    // A page created outside this process after the last scan: no row names its id, so the cheap
    // lookup misses. Reporting a miss there would be wrong — every file still states the id — so the
    // write pays for one full scan and asks again. This test only passes because of that fallback.
    let graph = TempGraph::new("update-unseen-id");
    graph.write("pages/alpha.md", "- first\n");
    let mut opened = graph.open();

    graph.write("pages/gamma.md", SOLO);

    let updated = opened
        .update_block(&durable(), "changed")
        .expect("update the block the index has not seen");

    assert_eq!(updated.value().page(), "gamma");
    assert_eq!(
        read_text(&graph, "pages/gamma.md"),
        "- changed\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n",
        "the edit must land in the file that was created after the last scan"
    );
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n",
        "the page that does not state the id must not be touched"
    );
}

#[test]
fn a_continuation_line_that_would_become_a_child_is_refused() {
    let graph = TempGraph::new("update-structure");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();
    let before = read_bytes(&graph, "pages/alpha.md");

    // A second line indented to the block's content column becomes a marker line, so this would turn
    // "change the text" into a structural change that the next scan projects into the index.
    let failure = opened
        .update_block(&durable(), "changed\n- nested")
        .expect_err("a structural change must be refused");

    assert!(
        matches!(
            &failure,
            WriteError::Edit(EditError::ContinuationChangesStructure)
        ),
        "expected Edit(ContinuationChangesStructure), got {failure:?}"
    );
    assert_eq!(
        read_bytes(&graph, "pages/alpha.md"),
        before,
        "a refused edit must not touch the file"
    );
}

#[test]
fn multiline_text_becomes_one_block_with_three_lines() {
    let graph = TempGraph::new("update-multiline");
    graph.write("pages/alpha.md", SOLO);
    let mut opened = graph.open();

    let updated = opened
        .update_block(&durable(), "one\ntwo\nthree")
        .expect("update the block");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- one\n  two\n  three\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n"
    );
    assert_eq!(
        updated.value().text(),
        "one\ntwo\nthree\nid:: 01234567-89ab-cdef-0123-456789abcdef",
        "the three lines must parse back as one block's text"
    );
    assert_eq!(
        block_count_without_scan(&mut opened, "alpha"),
        1,
        "continuation lines are text, not blocks"
    );
}

#[test]
fn the_marker_bytes_survive_the_update() {
    let graph = TempGraph::new("update-marker");
    // A bare `-`, not the `- ` this repository appends with: the marker belongs to the file.
    graph.write(
        "pages/alpha.md",
        "-\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n",
    );
    let mut opened = graph.open();

    opened
        .update_block(&durable(), "a2")
        .expect("update the block");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- a2\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n",
        "the edit must keep the marker the file used"
    );
}

#[test]
fn the_written_bytes_keep_the_bom_crlf_and_a_missing_final_newline() {
    let graph = TempGraph::new("update-bytes");
    // A byte order mark, CRLF endings, and no final newline: the three things a rewrite can silently
    // normalize. The assertions are on the bytes on disk, not on the edit's return value, because
    // what survives the file replacement is the part that matters.
    graph.write(
        "pages/alpha.md",
        "\u{feff}- first\r\n  id:: 01234567-89ab-cdef-0123-456789abcdef\r\n- last",
    );
    let mut opened = graph.open();

    opened
        .update_block(&durable(), "changed")
        .expect("update the block");

    assert_eq!(
        read_bytes(&graph, "pages/alpha.md"),
        "\u{feff}- changed\r\n  id:: 01234567-89ab-cdef-0123-456789abcdef\r\n- last".as_bytes(),
        "the written bytes must keep the BOM, use CRLF, and stay without a final newline"
    );
}

#[test]
fn a_held_lock_refuses_the_update_without_touching_the_file() {
    let graph = TempGraph::new("update-locked");
    graph.write("pages/alpha.md", ALPHA);
    // Zero wait, so the refusal is observed without waiting out the default budget.
    let mut opened =
        Graph::open(graph.options().with_write_lock_wait(Duration::ZERO)).expect("open the graph");
    let before = read_bytes(&graph, "pages/alpha.md");

    // The graph root is where the engine takes the lock for a graph that is not a Git
    // repository; holding it here is what another writer would look like.
    let held = WriteLock::acquire(&graph.graph_root(), None).expect("hold the lock");
    let failure = opened
        .update_block(&durable(), "changed")
        .expect_err("a held lock must refuse the write");
    drop(held);

    assert!(
        matches!(&failure, WriteError::Lock(_)),
        "expected a lock error, got {failure:?}"
    );
    assert_eq!(
        read_bytes(&graph, "pages/alpha.md"),
        before,
        "a refused write must not touch the file"
    );
}

#[test]
fn updating_one_page_leaves_another_page_byte_for_byte_unchanged() {
    let graph = TempGraph::new("update-other-page");
    graph.write("pages/alpha.md", ALPHA);
    let beta = "\u{feff}- other\r\n  id:: fedcba98-7654-3210-fedc-ba9876543210\r\n- second";
    graph.write("pages/beta.md", beta);
    let mut opened = graph.open();

    opened
        .update_block(&durable(), "changed")
        .expect("update the block");

    assert_eq!(
        read_bytes(&graph, "pages/beta.md"),
        beta.as_bytes(),
        "a write to one page must not rewrite another"
    );
}

#[test]
fn an_id_stated_on_two_pages_is_refused_and_neither_file_changes() {
    // Two pages stating one `id::` is a real state a graph reaches by hand editing or a copy-paste
    // that kept the identity line, and the projection records it as it is. A write reads exactly one
    // file, so it cannot settle which block the caller meant, and picking one is worse than
    // arbitrary: the row it picked can be replaced by the reprojection this write commits, so a
    // build could rewrite one page and then report the other. The refusal comes before any file is
    // read, so both pages keep their bytes, and the error names both of them.
    let graph = TempGraph::new("update-ambiguous-pages");
    graph.write("pages/alpha.md", ALPHA);
    graph.write("pages/beta.md", SOLO);
    let mut opened = graph.open();
    let alpha_before = read_bytes(&graph, "pages/alpha.md");
    let beta_before = read_bytes(&graph, "pages/beta.md");

    let failure = opened
        .update_block(&durable(), "changed")
        .expect_err("an id stated on two pages must be refused");

    match &failure {
        WriteError::AmbiguousBlockId { id: refused, pages } => {
            assert_eq!(refused.as_str(), ID);
            assert_eq!(
                pages,
                &["alpha".to_owned(), "beta".to_owned()],
                "every page stating the id is named, in sorted order"
            );
        }
        other => panic!("expected AmbiguousBlockId, got {other:?}"),
    }
    let message = failure.to_string();
    for page in ["alpha", "beta"] {
        assert!(
            message.contains(page),
            "the message must name {page:?} so a person can find the duplicate: {message}"
        );
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
