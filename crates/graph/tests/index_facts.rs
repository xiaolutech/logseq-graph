//! Content-fact projection through the public index API.
//!
//! The index stores page and block properties, page refs, block refs, and tags, but this slice adds
//! no read path for them: the query API and the facade are the next slice. So the stored row
//! contents are asserted in the `index` unit tests, where SQL is allowed, and these tests assert
//! what a caller can observe today:
//!
//! - a page full of facts projects like any other page and reads back with the same structure,
//! - whole-page replacement still holds when the only change is a fact,
//! - page-level facts need no block,
//! - a page that is too deep, or very wide, is handled by an iterative walk and an iterative fact
//!   extraction rather than by the thread stack.

use logseq_graph::document::{parse, Block, PageFacts};
use logseq_graph::domain::BlockMarker;
use logseq_graph::domain::BlockNode;
use logseq_graph::index::{Index, IndexError, MAX_BLOCK_DEPTH};

mod support;
use support::document_fixture;

/// A page from the predecessor implementation: real text with properties, refs, tags, and code.
const PARSER_FIXTURE: &str = document_fixture!("predecessor-fixture-parser.md");

fn open() -> Index {
    Index::open_in_memory().expect("open index")
}

/// `(preorder, depth, sibling_index, marker)` of every block of a parsed document, in preorder.
fn expected_shape(blocks: &[Block]) -> Vec<(u32, u32, u32, BlockMarker)> {
    fn visit(blocks: &[Block], depth: u32, out: &mut Vec<(u32, u32, u32, BlockMarker)>) {
        for (sibling_index, block) in blocks.iter().enumerate() {
            let preorder = out.len() as u32;
            out.push((preorder, depth, sibling_index as u32, block.marker()));
            visit(block.children(), depth + 1, out);
        }
    }
    let mut out = Vec::new();
    visit(blocks, 0, &mut out);
    out
}

/// The same shape, read from a projected page tree.
fn stored_shape(blocks: &[BlockNode]) -> Vec<(u32, u32, u32, BlockMarker)> {
    fn visit(blocks: &[BlockNode], out: &mut Vec<(u32, u32, u32, BlockMarker)>) {
        for block in blocks {
            out.push((
                block.preorder(),
                block.depth(),
                block.sibling_index(),
                block.marker(),
            ));
            visit(block.children(), out);
        }
    }
    let mut out = Vec::new();
    visit(blocks, &mut out);
    out
}

#[test]
fn a_page_full_of_facts_reads_back_with_the_same_structure() {
    let mut index = open();
    let document = parse(PARSER_FIXTURE);

    // The fixture must actually carry facts, or this test would only prove that a page without any
    // projects. It has more than one block too, so the shape comparison is not trivially empty.
    let facts = PageFacts::extract(&document);
    assert!(
        !facts.properties().is_empty(),
        "the fixture must have page properties"
    );
    assert!(
        facts
            .blocks()
            .iter()
            .any(|block| !block.page_refs().is_empty() || !block.tags().is_empty()),
        "the fixture must reference or tag something"
    );
    assert!(facts.blocks().len() > 1, "the fixture must have blocks");

    support::project(&mut index, "fixture", &document).expect("replace page");

    let tree = index
        .page_tree("fixture")
        .expect("query page tree")
        .expect("page exists");
    assert_eq!(tree.page().block_count(), facts.blocks().len() as u32);
    assert_eq!(
        stored_shape(tree.blocks()),
        expected_shape(document.blocks())
    );
}

#[test]
fn replacing_a_page_whose_only_change_is_a_fact_keeps_its_structure() {
    let mut index = open();
    support::project(
        &mut index,
        "page",
        &parse("title:: first\n\n- a #one [[One]]\n\t- b\n"),
    )
    .expect("first replacement");

    let metadata = support::project(
        &mut index,
        "page",
        &parse("title:: second\n\n- a #two [[Two]]\n\t- b\n"),
    )
    .expect("second replacement");

    // The old page, facts included, is gone and the new one was written by one write: a leftover
    // fact row would have needed a second write to appear, and this revision says there was none.
    assert_eq!(metadata.revision(), 2);
    let tree = index
        .page_tree("second")
        .expect("query page tree")
        .expect("page exists");
    assert_eq!(tree.page().revision(), 2);
    assert_eq!(tree.page().block_count(), 2);
    assert_eq!(
        stored_shape(tree.blocks()),
        [
            (0, 0, 0, BlockMarker::Bullet),
            (1, 1, 0, BlockMarker::Bullet)
        ]
    );
}

#[test]
fn page_facts_need_no_block() {
    let mut index = open();
    support::project(
        &mut index,
        "metadata",
        &parse("title:: properties only\ntags:: alpha, beta\n\n"),
    )
    .expect("replace page");

    let tree = index
        .page_tree("properties only")
        .expect("query page tree")
        .expect("page exists");
    assert_eq!(tree.page().block_count(), 0);
    assert!(tree.blocks().is_empty());
}

#[test]
fn a_deep_page_with_facts_on_every_block_is_rejected_without_writing() {
    // Facts are extracted before the depth limit is checked, so this also proves the extraction
    // itself is iterative: a 10000-level document must not overflow the thread stack on the way to
    // the structured rejection.
    const LEVELS: usize = 10_000;
    let mut source = String::new();
    for depth in 0..LEVELS {
        source.push_str(&"\t".repeat(depth));
        source.push_str("- nested #deep [[Deep Page]]\n");
    }
    let document = parse(&source);
    let mut index = open();

    let error = support::project(&mut index, "deep", &document)
        .expect_err("a page deeper than the limit is rejected");
    match error {
        IndexError::TooDeep { depth, limit } => {
            assert_eq!(depth, MAX_BLOCK_DEPTH + 1);
            assert_eq!(limit, MAX_BLOCK_DEPTH);
        }
        other => panic!("expected a depth limit error, got {other:?}"),
    }

    assert_eq!(index.metadata().expect("read metadata").revision(), 0);
    assert!(index.page_tree("deep").expect("query page tree").is_none());

    drop(document);
}

#[test]
fn many_facts_in_one_page_are_written_by_one_replacement() {
    // Every block carries a tag and a ref, so a page replacement inserts three fact rows per block.
    // The claim is that this is still one transaction with one revision, not that it is fast.
    const BLOCKS: usize = 2_000;
    let mut source = String::new();
    for block in 0..BLOCKS {
        source.push_str(&format!("- block {block} #tag{block} [[Page {block}]]\n"));
    }
    let mut index = open();

    let metadata = support::project(&mut index, "wide", &parse(&source)).expect("replace page");

    assert_eq!(metadata.revision(), 1);
    let tree = index
        .page_tree("wide")
        .expect("query page tree")
        .expect("page exists");
    assert_eq!(tree.page().block_count(), BLOCKS as u32);
    assert_eq!(tree.page().revision(), 1);
}

#[test]
fn a_page_of_tasks_projects_like_any_other_and_a_changed_text_moves_its_ids() {
    // A task marker and a priority are facts read out of block text, so a task page is an ordinary
    // page: the words stay in the text a read hands back, the structure is the text's own, and a
    // replacement behaves like any other page's. The two columns those facts are stored in have no
    // public read path yet, so what this asserts through the public API is what a caller can see.
    let mut index = open();
    let before = "- TODO [#A] buy milk\n\t- DOING [#B] write it\n- plain\n";
    let ephemeral_ids = |index: &Index| -> Vec<Option<String>> {
        index
            .page_blocks("tasks")
            .expect("read blocks")
            .expect("page exists")
            .iter()
            .map(|block| block.ephemeral_id().map(|id| id.as_str().to_owned()))
            .collect()
    };

    support::project(&mut index, "tasks", &parse(before)).expect("first projection");
    let texts: Vec<String> = index
        .page_blocks("tasks")
        .expect("read blocks")
        .expect("page exists")
        .iter()
        .map(|block| block.text().to_owned())
        .collect();
    assert_eq!(
        texts,
        ["TODO [#A] buy milk", "DOING [#B] write it", "plain"]
    );
    let first_ids = ephemeral_ids(&index);
    assert!(
        first_ids.iter().all(Option::is_some),
        "every block of this page states no id::, so each answers an ephemeral one"
    );

    // The same bytes are the same blocks, so a re-projection reuses the ids it already minted.
    support::project(&mut index, "tasks", &parse(before)).expect("second projection");
    assert_eq!(ephemeral_ids(&index), first_ids);

    // Changed text replaces the row set, and every old ephemeral id stops naming anything.
    let metadata = support::project(
        &mut index,
        "tasks",
        &parse("- DONE [#C] buy milk\n\t- plain\n"),
    )
    .expect("third projection");
    assert_eq!(metadata.revision(), 3);
    let replaced = ephemeral_ids(&index);
    assert!(
        replaced
            .iter()
            .flatten()
            .all(|id| !first_ids.iter().flatten().any(|old| old == id)),
        "a changed page mints new ephemeral ids: {replaced:?}"
    );
    let texts: Vec<String> = index
        .page_blocks("tasks")
        .expect("read blocks")
        .expect("page exists")
        .iter()
        .map(|block| block.text().to_owned())
        .collect();
    assert_eq!(texts, ["DONE [#C] buy milk", "plain"]);
}
