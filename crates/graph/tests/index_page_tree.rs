//! Page projection and page-tree query tests over the SQLite-backed index.

use logseq_graph::document::parse;
use logseq_graph::domain::BlockMarker;
use logseq_graph::domain::BlockNode;
use logseq_graph::index::{Index, IndexError, MAX_BLOCK_DEPTH};

mod support;

/// A page owned by this test: properties, nesting, a heading marker, and a fenced region.
const PAGE: &str = "\
title:: Index slice
tags:: index, test

- root one #tag
\t- child one
\t\t- grandchild
\t- child two
- ## section heading
\t- under heading
- root two
\t- ```bash
\t  echo hi
\t  ```
";

fn open() -> Index {
    Index::open_in_memory().expect("open index")
}

fn marker_name(marker: BlockMarker) -> String {
    match marker {
        BlockMarker::Bullet => "bullet".to_owned(),
        BlockMarker::Heading { level } => format!("heading{level}"),
    }
}

/// Flatten a block tree in preorder as `(preorder, depth, sibling_index, marker)` tuples.
fn flatten(nodes: &[BlockNode]) -> Vec<(u32, u32, u32, String)> {
    let mut out = Vec::new();
    fn visit(nodes: &[BlockNode], out: &mut Vec<(u32, u32, u32, String)>) {
        for node in nodes {
            out.push((
                node.preorder(),
                node.depth(),
                node.sibling_index(),
                marker_name(node.marker()),
            ));
            visit(node.children(), out);
        }
    }
    visit(nodes, &mut out);
    out
}

fn block(preorder: u32, depth: u32, sibling_index: u32, marker: &str) -> (u32, u32, u32, String) {
    (preorder, depth, sibling_index, marker.to_owned())
}

/// A page whose blocks nest one level per line, up to `levels` levels.
fn nested_source(levels: usize) -> String {
    let mut source = String::new();
    for depth in 0..levels {
        source.push_str(&"\t".repeat(depth));
        source.push_str("- nested\n");
    }
    source
}

#[test]
fn projected_page_tree_preserves_hierarchy_and_order() {
    let mut index = open();
    support::project(&mut index, "slice", &parse(PAGE)).expect("replace page");

    let tree = index
        .page_tree("Index slice")
        .expect("query page tree")
        .expect("page exists");

    assert_eq!(tree.page().name(), "Index slice");
    assert_eq!(tree.page().block_count(), 8);
    assert_eq!(tree.blocks().len(), 3);

    // Nested access proves the parent/child links, not just a flat ordered list.
    assert_eq!(tree.blocks()[0].children().len(), 2);
    assert_eq!(tree.blocks()[0].children()[0].children().len(), 1);
    assert_eq!(tree.blocks()[1].children().len(), 1);
    assert_eq!(tree.blocks()[2].children().len(), 1);
    assert_eq!(tree.blocks()[2].children()[0].children().len(), 0);

    assert_eq!(
        flatten(tree.blocks()),
        vec![
            block(0, 0, 0, "bullet"),
            block(1, 1, 0, "bullet"),
            block(2, 2, 0, "bullet"),
            block(3, 1, 1, "bullet"),
            block(4, 0, 1, "bullet"),
            block(5, 1, 0, "bullet"),
            block(6, 0, 2, "bullet"),
            block(7, 1, 0, "bullet"),
        ]
    );
}

#[test]
fn page_tree_queries_do_not_need_source_lines() {
    let mut index = open();

    // The parsed document is dropped before the query: everything below comes from index rows.
    {
        let document = parse("- a\n\t- b\n\t\t- c\n\t- d\n- e\n");
        support::project(&mut index, "nested", &document).expect("replace page");
    }

    let tree = index
        .page_tree("nested")
        .expect("query page tree")
        .expect("page exists");

    assert_eq!(tree.page().block_count(), 5);
    assert_eq!(
        flatten(tree.blocks()),
        vec![
            block(0, 0, 0, "bullet"),
            block(1, 1, 0, "bullet"),
            block(2, 2, 0, "bullet"),
            block(3, 1, 1, "bullet"),
            block(4, 0, 1, "bullet"),
        ]
    );
}

#[test]
fn replacing_a_page_advances_the_revision_once() {
    let mut index = open();
    let instance = index
        .metadata()
        .expect("read metadata")
        .instance_id()
        .to_owned();

    let first = support::project(&mut index, "page", &parse("- a\n\t- b\n\t\t- c\n"))
        .expect("first replacement");
    assert_eq!(first.revision(), 1);
    assert_eq!(first.instance_id(), instance);

    let second =
        support::project(&mut index, "page", &parse("- only\n")).expect("second replacement");
    assert_eq!(second.revision(), 2);
    assert_eq!(second.instance_id(), instance);

    let tree = index
        .page_tree("page")
        .expect("query page tree")
        .expect("page exists");
    assert_eq!(tree.page().block_count(), 1);
    assert_eq!(tree.page().revision(), 2);
    assert_eq!(flatten(tree.blocks()), vec![block(0, 0, 0, "bullet")]);

    // Reading does not advance the revision.
    assert_eq!(index.metadata().expect("read metadata").revision(), 2);
}

#[test]
fn replacing_one_page_leaves_other_pages_untouched() {
    let mut index = open();

    support::project(&mut index, "alpha", &parse("- a\n\t- b\n")).expect("replace alpha");
    support::project(&mut index, "beta", &parse("- x\n\t- y\n\t- z\n")).expect("replace beta");
    let last =
        support::project(&mut index, "alpha", &parse("- replaced\n")).expect("replace alpha again");

    assert_eq!(last.revision(), 3);
    assert_eq!(
        index
            .page_metadata("alpha")
            .expect("read alpha")
            .expect("alpha exists")
            .block_count(),
        1
    );

    let beta = index
        .page_tree("beta")
        .expect("query beta")
        .expect("beta exists");
    assert_eq!(beta.page().revision(), 2);
    assert_eq!(
        flatten(beta.blocks()),
        vec![
            block(0, 0, 0, "bullet"),
            block(1, 1, 0, "bullet"),
            block(2, 1, 1, "bullet"),
        ]
    );
}

#[test]
fn heading_markers_survive_projection() {
    let mut index = open();
    support::project(
        &mut index,
        "headings",
        &parse("## second\n\t- child\n###### sixth\n"),
    )
    .expect("replace page");

    let tree = index
        .page_tree("headings")
        .expect("query page tree")
        .expect("page exists");

    assert_eq!(
        flatten(tree.blocks()),
        vec![
            block(0, 0, 0, "heading2"),
            block(1, 1, 0, "bullet"),
            block(2, 0, 1, "heading6"),
        ]
    );
}

#[test]
fn page_without_blocks_is_projected_and_read_back_empty() {
    let mut index = open();
    support::project(&mut index, "page", &parse("- a\n\t- b\n")).expect("replace page with blocks");

    let metadata = support::project(&mut index, "page", &parse("title:: metadata only\n\n"))
        .expect("replace page without blocks");
    assert_eq!(metadata.revision(), 2);

    let tree = index
        .page_tree("metadata only")
        .expect("query page tree")
        .expect("page exists");
    assert_eq!(tree.page().block_count(), 0);
    assert!(tree.blocks().is_empty());
}

#[test]
fn deepest_supported_nesting_is_projected() {
    let mut index = open();
    support::project(
        &mut index,
        "deep",
        &parse(&nested_source(MAX_BLOCK_DEPTH as usize + 1)),
    )
    .expect("replace page at the depth limit");

    let tree = index
        .page_tree("deep")
        .expect("query page tree")
        .expect("page exists");
    assert_eq!(tree.page().block_count(), MAX_BLOCK_DEPTH + 1);

    let mut node = &tree.blocks()[0];
    let mut depth = 0;
    while let Some(child) = node.children().first() {
        node = child;
        depth += 1;
    }
    assert_eq!(depth, MAX_BLOCK_DEPTH);
}

#[test]
fn deep_page_is_rejected_and_its_document_is_dropped() {
    // The full public path at a depth that aborted the process before `Block` gained an iterative
    // drop: parse, project, reject, and then destroy the parsed document. Parsing and projection
    // are iterative, so the only claim here is that the rejection is structured and that the
    // document's normal drop no longer recurses.
    const LEVELS: usize = 10_000;
    let mut index = open();
    let document = parse(&nested_source(LEVELS));

    let error = support::project(&mut index, "deep", &document)
        .expect_err("a page deeper than the limit is rejected");
    match error {
        IndexError::TooDeep { depth, limit } => {
            assert_eq!(depth, MAX_BLOCK_DEPTH + 1);
            assert_eq!(limit, MAX_BLOCK_DEPTH);
        }
        other => panic!("expected a depth limit error, got {other:?}"),
    }

    // A rejected replacement writes nothing at all.
    assert_eq!(index.metadata().expect("read metadata").revision(), 0);
    assert!(index.page_tree("deep").expect("query page tree").is_none());

    // Destroying the parsed document is part of the claim: it must finish instead of aborting.
    drop(document);
}

#[test]
fn unknown_page_has_no_derived_rows() {
    let index = open();

    assert!(index
        .page_metadata("missing")
        .expect("read metadata")
        .is_none());
    assert!(index
        .page_tree("missing")
        .expect("query page tree")
        .is_none());
    assert_eq!(index.metadata().expect("read metadata").revision(), 0);
}
