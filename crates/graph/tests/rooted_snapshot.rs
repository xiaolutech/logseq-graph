//! Zooming into one block: the subtree it roots, the breadcrumb above it, and what a `((block ref))`
//! resolves to.
//!
//! Every test writes a graph file, opens a [`Graph`], and asks through [`Graph::block_snapshot`] or
//! [`Graph::resolve_block_ref`], so the whole path from discovery through the file read to the answer
//! is covered. The cases that would look right while being wrong get their own test: a durable id
//! looked up by the position the projection recorded instead of by the id itself, an ephemeral id
//! whose row still exists but whose file moved, a breadcrumb that repeats the root, and a read that
//! creates identity.

use logseq_graph::domain::{
    BlockId, BlockRefResolution, BlockSelector, RootedSnapshot, SnapshotBlock,
};
use logseq_graph::engine::{EngineError, Graph, MoveTarget};

mod support;

use support::{bound, TempGraph};

/// Three levels, the deepest block carrying an `id::` line so it can be addressed by identity.
///
/// Blocks as `(preorder, depth, sibling_index)`: `a (0, 0, 0)`, `b (1, 1, 0)`, `c (2, 2, 0)`.
const NESTED: &str = "\
- a
\t- b
\t\t- c
\t\t  id:: 61745deb-eeed-4426-8020-eb9e5c90d558
";

/// The id [`NESTED`] states.
const C_ID: &str = "61745deb-eeed-4426-8020-eb9e5c90d558";

/// A page whose only block is identified, so the selected block is the page's top level.
const TOP: &str = "\
- only
  id:: fedcba98-7654-3210-fedc-ba9876543210
";

const TOP_ID: &str = "fedcba98-7654-3210-fedc-ba9876543210";

/// Two blocks of one page stating the same `id::` line.
const DUPLICATED: &str = "\
- one
  id:: 11111111-2222-3333-4444-555555555555
- two
  id:: 11111111-2222-3333-4444-555555555555
";

const DUPLICATE_ID: &str = "11111111-2222-3333-4444-555555555555";

/// Two top-level blocks, the selected one under the *second*, so a preorder and a child index
/// disagree: `second` is top-level index 1 and preorder 2, so `target` is `child_path [1, 0]` while
/// a preorder path to it starts at `[2, 0]`.
const SPLIT: &str = "\
- first
\t- nested
- second
\t- target
\t\t- deeper
";

/// Four levels, for the depth of the breadcrumb.
const DEEPER: &str = "\
- a
\t- b
\t\t- c
\t\t\t- d
\t\t\t  id:: 33333333-4444-5555-6666-777777777777
";

const D_ID: &str = "33333333-4444-5555-6666-777777777777";

/// The `id::` UUID as a caller would hold it.
fn id(raw: &str) -> BlockId {
    BlockId::parse(raw).expect("valid 8-4-4-4-12 UUID")
}

/// A durable selector for `raw`.
fn durable(raw: &str) -> BlockSelector {
    BlockSelector::Durable(id(raw))
}

/// Text of a graph file, as it is on disk.
fn read_text(graph: &TempGraph, relative: &str) -> String {
    std::fs::read_to_string(graph.graph_root().join(relative)).expect("read graph file")
}

/// Bytes of a graph file, as they are on disk.
fn read_bytes(graph: &TempGraph, relative: &str) -> Vec<u8> {
    std::fs::read(graph.graph_root().join(relative)).expect("read graph file")
}

/// Texts of every block of `root`, itself first, in preorder.
fn subtree_texts(root: &SnapshotBlock) -> Vec<&str> {
    let mut ordered = Vec::new();
    let mut stack = vec![root];
    while let Some(block) = stack.pop() {
        ordered.push(block.text());
        stack.extend(block.children().iter().rev());
    }
    ordered
}

/// The breadcrumb's texts, outer to inner.
fn ancestor_texts(snapshot: &RootedSnapshot) -> Vec<&str> {
    snapshot
        .ancestors()
        .iter()
        .map(SnapshotBlock::text)
        .collect()
}

/// Page names of an ambiguous resolution.
fn names(pages: &[String]) -> Vec<&str> {
    pages.iter().map(String::as_str).collect()
}

#[test]
fn a_durable_id_opens_the_subtree_it_roots_with_the_chain_above_it() {
    let graph = TempGraph::new("rooted-durable");
    graph.write("pages/Nested.md", NESTED);
    let mut opened = graph.open();

    let read = opened
        .block_snapshot(&durable(C_ID))
        .expect("read the rooted snapshot");
    let rooted = read.present().expect("the id names a block");

    assert_eq!(rooted.page(), "Nested");
    assert_eq!(rooted.root().text(), "c");
    assert!(
        rooted.root().children().is_empty(),
        "the deepest block roots a subtree of one block"
    );
    assert_eq!(
        ancestor_texts(rooted),
        ["a", "b"],
        "the breadcrumb runs from the page root down to the parent, in that order"
    );

    // The reference answer carries the same block, and says so on its own: the renderer gets the
    // page and the view without classifying an error.
    let resolution = resolved(&mut opened, C_ID);
    assert_eq!(resolution.page(), Some("Nested"));
    assert_eq!(
        resolution
            .block()
            .expect("the id names one block")
            .preorder(),
        2,
        "the view is the index's own view of the block the reference names"
    );
    assert!(
        resolution.ambiguous_pages().is_empty(),
        "a hit is not an ambiguity, so it reports no pages besides its own"
    );

    // The page's version, not a version of the subtree: a caller comparing it with a page snapshot of
    // the same page must see one observation of one file.
    let page_read = opened
        .page_snapshot("Nested")
        .expect("read the page snapshot");
    let page = page_read.present().expect("the page exists");
    assert_eq!(
        rooted.version(),
        page.version(),
        "a rooted snapshot and a page snapshot of one page report the same bytes"
    );
}

#[test]
fn the_root_carries_its_whole_subtree_and_its_own_position() {
    let graph = TempGraph::new("rooted-subtree");
    graph.write("pages/Nested.md", NESTED);
    let mut opened = graph.open();

    let by_handle = opened
        .block_snapshot(&bound(&graph, "Nested", &[0]))
        .expect("read the rooted snapshot");
    let rooted = by_handle.present().expect("the path names a block");

    assert_eq!(
        subtree_texts(rooted.root()),
        ["a", "b", "c"],
        "the root keeps the subtree under it, not just its own line"
    );
    assert_eq!(
        rooted.root().child_path(),
        [0],
        "the root is the snapshot's own block, with the paths the page snapshot gave it"
    );
    assert!(
        rooted.ancestors().is_empty(),
        "a top-level root has nothing above it"
    );
}

/// The `((block ref))` answer for an id, as a value.
fn resolved(opened: &mut Graph, raw: &str) -> BlockRefResolution {
    opened
        .resolve_block_ref(&id(raw))
        .expect("resolve the reference")
        .into_value()
}

#[test]
fn a_durable_id_still_names_its_block_after_the_block_moves() {
    let graph = TempGraph::new("rooted-moved");
    graph.write("pages/Nested.md", NESTED);
    let mut opened = graph.open();

    // `c` moves to be the last child of `a`, one level up: same id, same text, different position.
    opened
        .move_block(
            &durable(C_ID),
            MoveTarget::LastChildOf(bound(&graph, "Nested", &[0])),
        )
        .expect("move the identified block under the page's first root");
    assert_eq!(
        read_text(&graph, "pages/Nested.md"),
        "- a\n\t- b\n\t- c\n\t  id:: 61745deb-eeed-4426-8020-eb9e5c90d558\n",
        "the block moved and its identity line moved with it"
    );

    let read = opened
        .block_snapshot(&durable(C_ID))
        .expect("read the rooted snapshot after the move");
    let rooted = read.present().expect("the id still names the block");
    assert_eq!(rooted.root().text(), "c");
    assert_eq!(
        ancestor_texts(rooted),
        ["a"],
        "the breadcrumb is the chain the block has now, not the one it had when it was projected"
    );
    assert_eq!(
        rooted.root().child_path(),
        [0, 1],
        "the position changed; the identity did not"
    );
}

#[test]
fn a_durable_id_is_found_by_its_id_after_an_edit_the_index_has_not_seen() {
    let graph = TempGraph::new("rooted-unscanned-edit");
    graph.write("pages/Nested.md", NESTED);
    let mut opened = graph.open();

    // Another process rewrites the page between the scan and this read: `c` now sits under a new root,
    // and the preorder the index recorded for it (2) belongs to `z` now. A read that located the block
    // by that position would answer `z`; the identity line is what makes the answer right.
    graph.write(
        "pages/Nested.md",
        format!("- x\n\t- y\n\t  id:: {C_ID}\n\t\t- z\n"),
    );

    let read = opened
        .block_snapshot(&durable(C_ID))
        .expect("read the rooted snapshot of an edited page");
    let rooted = read.present().expect("the id still names a block");
    assert_eq!(
        rooted.root().text(),
        "y",
        "the id names `y`, not the block that took its old preorder"
    );
    assert_eq!(subtree_texts(rooted.root()), ["y", "z"]);
    assert_eq!(
        ancestor_texts(rooted),
        ["x"],
        "the chain comes from the file as it is now, not from the projection"
    );
}

#[test]
fn an_id_duplicated_by_an_edit_the_index_has_not_seen_is_refused() {
    let graph = TempGraph::new("rooted-unscanned-duplicate");
    graph.write(
        "pages/Twice.md",
        format!("- one\n  id:: {DUPLICATE_ID}\n- two\n"),
    );
    let mut opened = graph.open();

    // The index still says one block states the id; the file now says two. The duplicate is what this
    // read must refuse, because answering one of them would hand back a block the caller cannot tell
    // from the one it asked for.
    graph.write(
        "pages/Twice.md",
        format!("- one\n  id:: {DUPLICATE_ID}\n- two\n  id:: {DUPLICATE_ID}\n"),
    );

    match opened
        .block_snapshot(&durable(DUPLICATE_ID))
        .expect_err("the file states the id twice")
    {
        EngineError::AmbiguousBlockId { pages, .. } => assert_eq!(
            names(&pages),
            ["Twice"],
            "one page states the id twice, so that page is named once"
        ),
        other => panic!("a duplicated id must be refused: {other:?}"),
    }
}

#[test]
fn an_ephemeral_id_opens_the_subtree_it_was_issued_for() {
    let graph = TempGraph::new("rooted-ephemeral");
    graph.write("pages/Split.md", SPLIT);
    let mut opened = graph.open();

    // A preorder path to `target`: the second top-level block (`second`, preorder 2) and its first
    // child. The selector the projection answers is the ephemeral id of that block.
    let read = opened
        .block_snapshot(&bound(&graph, "Split", &[2, 0]))
        .expect("read the rooted snapshot");
    let rooted = read.present().expect("the selector names a block");
    assert_eq!(rooted.root().text(), "target");
    assert_eq!(
        subtree_texts(rooted.root()),
        ["target", "deeper"],
        "the selector roots the whole subtree, not just the block's own line"
    );
    assert_eq!(ancestor_texts(rooted), ["second"]);

    // A preorder and a child index are different numbers for this block, so the two addresses are
    // not interchangeable. The snapshot reports the child-index path, which counts from the page
    // root and is what a batch of operations addresses (ADR-0011).
    assert_eq!(rooted.root().child_path(), [1, 0]);
    assert_eq!(rooted.ancestors()[0].child_path(), [1]);
}

#[test]
fn an_id_the_graph_has_written_past_names_nothing() {
    let graph = TempGraph::new("rooted-stale");
    graph.write("pages/Nested.md", NESTED);
    let mut opened = graph.open();
    let stale = bound(&graph, "Nested", &[0, 0]);

    // A write reprojects the whole page, so every ephemeral id it held is gone and the block that
    // carried this one has a new id. The old value must answer "nothing", never the block that now
    // stands at that position (ADR-0012).
    opened
        .update_block(&bound(&graph, "Nested", &[0]), "a renamed")
        .expect("write the page");

    let read = opened
        .block_snapshot(&stale)
        .expect("an id no row holds is an answer, not a failure");
    assert!(
        read.present().is_none(),
        "the block has a new id, so the old one opens no subtree"
    );
}

/// Two children of one root, the second with a child of its own.
///
/// Blocks as `(preorder, preorder path)`: `a (0, [0])`, `b (1, [0, 0])`, `c (2, [0, 1])`,
/// `d (3, [0, 1, 0])`.
const TWO_CHILDREN: &str = "\
- a
\t- b
\t- c
\t\t- d
";

#[test]
fn an_id_is_not_followed_onto_a_file_that_moved_under_it() {
    let graph = TempGraph::new("rooted-aliased");
    graph.write("pages/Siblings.md", TWO_CHILDREN);
    let mut opened = graph.open();
    let handle = bound(&graph, "Siblings", &[0, 1]);

    // The id is good against the page as the scan saw it.
    let read = opened
        .block_snapshot(&handle)
        .expect("read the rooted snapshot");
    assert_eq!(
        read.present()
            .expect("the path names a block")
            .root()
            .text(),
        "c"
    );

    // Another process gives `b` a child. The row's child path still names the second child of `a`,
    // and that block is now at preorder 3, not the 2 the row was projected at: the projection no
    // longer describes this file. Without the preorder comparison this read would answer from a file
    // the id never named, which is the aliasing ADR-0009 forbids.
    graph.write(
        "pages/Siblings.md",
        "- a\n\t- b\n\t\t- b1\n\t- c\n\t\t- d\n",
    );

    let aliased = opened
        .block_snapshot(&handle)
        .expect("the id still names a row, so this is an answer and not an error");
    assert!(
        aliased.present().is_none(),
        "the file moved under the id, so the read answers nothing instead of the block there now"
    );
}

#[test]
fn an_id_no_block_states_answers_none_and_resolves_to_missing() {
    let graph = TempGraph::new("rooted-missing");
    graph.write("pages/Nested.md", NESTED);
    let mut opened = graph.open();
    let absent = "00000000-0000-0000-0000-000000000000";

    let read = opened
        .block_snapshot(&durable(absent))
        .expect("an absent id is an answer, not a failure");
    assert!(
        read.present().is_none(),
        "no block states the id, so there is no subtree to open"
    );

    assert_eq!(resolved(&mut opened, absent), BlockRefResolution::Missing);
}

#[test]
fn an_id_two_pages_state_resolves_to_ambiguous_and_fails_the_block_read() {
    let graph = TempGraph::new("rooted-ambiguous-pages");
    graph.write("pages/One.md", format!("- one\n  id:: {C_ID}\n"));
    graph.write("pages/Two.md", format!("- two\n  id:: {C_ID}\n"));
    let mut opened = graph.open();

    let resolution = resolved(&mut opened, C_ID);
    assert_eq!(
        names(resolution.ambiguous_pages()),
        ["One", "Two"],
        "the caller needs every page involved to go remove the duplicate"
    );
    assert!(
        resolution.block().is_none() && resolution.page().is_none(),
        "an ambiguous reference names no single block"
    );

    // The rooted read refuses what the resolution reports, with the same two page names.
    match opened
        .block_snapshot(&durable(C_ID))
        .expect_err("the rooted read refuses an ambiguous id")
    {
        EngineError::AmbiguousBlockId { id: refused, pages } => {
            assert_eq!(refused, id(C_ID));
            assert_eq!(names(&pages), ["One", "Two"]);
        }
        other => panic!("an ambiguous id must be refused: {other:?}"),
    }
}

#[test]
fn an_id_one_page_states_twice_is_refused_and_names_that_page_once() {
    let graph = TempGraph::new("rooted-ambiguous-one-page");
    graph.write("pages/Twice.md", DUPLICATED);
    let mut opened = graph.open();

    match opened
        .block_snapshot(&durable(DUPLICATE_ID))
        .expect_err("two blocks state the id, so the read cannot pick one")
    {
        EngineError::AmbiguousBlockId { pages, .. } => assert_eq!(
            names(&pages),
            ["Twice"],
            "one page states the id twice, so that page is named once"
        ),
        other => panic!("a duplicated id must be refused: {other:?}"),
    }
}

#[test]
fn the_read_creates_no_identity() {
    let graph = TempGraph::new("rooted-read-only");
    // No page states an `id::` line, so a read that fabricated one would be visible in the bytes.
    graph.write("pages/Plain.md", "- a\n\t- b\n\t\t- c\n");
    let mut opened = graph.open();
    let before = read_bytes(&graph, "pages/Plain.md");

    let by_handle = opened
        .block_snapshot(&bound(&graph, "Plain", &[0, 0]))
        .expect("open the subtree by handle");
    assert_eq!(
        by_handle
            .present()
            .expect("the path names a block")
            .root()
            .text(),
        "b"
    );
    resolved(&mut opened, C_ID);

    assert_eq!(
        read_bytes(&graph, "pages/Plain.md"),
        before,
        "a read must not change the file; only a write may add an `id::` line"
    );
}

#[test]
fn a_top_level_block_has_an_empty_breadcrumb() {
    let graph = TempGraph::new("rooted-top-level");
    graph.write("pages/Top.md", TOP);
    let mut opened = graph.open();

    let read = opened
        .block_snapshot(&durable(TOP_ID))
        .expect("read the rooted snapshot");
    let rooted = read.present().expect("the id names a block");
    assert!(
        rooted.ancestors().is_empty(),
        "nothing is above a top-level block, and the root is not repeated in the chain"
    );
    assert_eq!(rooted.root().text(), "only");
}

#[test]
fn the_breadcrumb_length_is_the_blocks_depth() {
    let graph = TempGraph::new("rooted-depth");
    graph.write("pages/Nested.md", NESTED);
    graph.write("pages/Split.md", SPLIT);
    graph.write("pages/Deeper.md", DEEPER);
    let mut opened = graph.open();

    // `a` is at depth 0, `b` at 1, `c` at 2, `d` at 3: the chain holds one block per level above.
    for (selector, expected) in [
        (bound(&graph, "Nested", &[0]), vec![]),
        (bound(&graph, "Nested", &[0, 0]), vec!["a"]),
        (bound(&graph, "Nested", &[0, 0, 0]), vec!["a", "b"]),
        // A handle path that starts at the second top-level block, where a preorder position and a
        // child index are different numbers.
        (bound(&graph, "Split", &[2, 0]), vec!["second"]),
        (durable(D_ID), vec!["a", "b", "c"]),
    ] {
        let read = opened
            .block_snapshot(&selector)
            .expect("read the rooted snapshot");
        let rooted = read.present().expect("the selector names a block");
        assert_eq!(
            ancestor_texts(rooted),
            expected,
            "the chain above the selected block, for {selector}"
        );
    }
}

#[test]
fn a_page_whose_file_is_gone_answers_none() {
    let graph = TempGraph::new("rooted-file-gone");
    graph.write("pages/Nested.md", NESTED);
    let mut opened = graph.open();

    // The index still holds the page row and the id, but the file behind it is gone. Answering `None`
    // is more honest than rebuilding a subtree from rows whose file disappeared.
    graph.remove("pages/Nested.md");

    let read = opened
        .block_snapshot(&durable(C_ID))
        .expect("a row without a file is an answer, not a failure");
    assert!(read.present().is_none());
}
