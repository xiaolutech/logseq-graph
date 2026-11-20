//! Round-trip and structure tests over fixtures derived from the predecessor graph.

mod support;

use logseq_graph::document::{parse, LineEnding, LineSpan, PageDocument};
use logseq_graph::domain::BlockMarker;
use support::document_fixture;

const PARSER_FIXTURE: &str = document_fixture!("predecessor-fixture-parser.md");
const QUERY_FIXTURE: &str = document_fixture!("predecessor-todo-query.md");
const JOURNAL_FIXTURE: &str = document_fixture!("predecessor-journal.md");

fn assert_roundtrip(source: &str) {
    let document = parse(source);
    assert_eq!(document.to_source(), source, "fixture round-trip mismatch");
}

#[test]
fn predecessor_fixtures_roundtrip_byte_for_byte() {
    for (name, source) in [
        ("fixture-parser", PARSER_FIXTURE),
        ("todo-query", QUERY_FIXTURE),
        ("journal", JOURNAL_FIXTURE),
    ] {
        let document = parse(source);
        assert_eq!(document.to_source(), source, "{name} round-trip mismatch");
    }
}

#[test]
fn nested_fixture_exposes_preorder_spans_and_hierarchy() {
    let document: PageDocument = parse(PARSER_FIXTURE);

    assert_eq!(document.preamble(), LineSpan::new(0, 3));
    assert_eq!(document.trailing(), LineSpan::new(18, 18));
    assert!(document.ends_with_newline());
    assert!(!document.has_bom());
    assert_eq!(document.blocks().len(), 4);

    let first = &document.blocks()[0];
    assert_eq!(first.marker(), BlockMarker::Bullet);
    assert_eq!(first.own_span(), LineSpan::new(3, 4));
    assert_eq!(first.subtree_span(), LineSpan::new(3, 8));
    assert_eq!(first.children().len(), 2);
    assert_eq!(first.children()[0].own_span(), LineSpan::new(4, 5));
    assert_eq!(first.children()[1].own_span(), LineSpan::new(5, 8));

    let heading = &document.blocks()[1];
    assert_eq!(heading.subtree_span(), LineSpan::new(8, 10));
    assert_eq!(heading.children().len(), 1);

    let code = &document.blocks()[2];
    assert_eq!(code.own_span(), LineSpan::new(10, 11));
    assert_eq!(code.subtree_span(), LineSpan::new(10, 15));
    assert_eq!(code.children().len(), 1);
    assert_eq!(code.children()[0].own_span(), LineSpan::new(11, 15));

    let ordered = &document.blocks()[3];
    assert_eq!(ordered.subtree_span(), LineSpan::new(15, 18));
    assert_eq!(ordered.children().len(), 1);
    assert_eq!(ordered.children()[0].own_span(), LineSpan::new(16, 18));

    let preorder = document.blocks_in_preorder();
    assert_eq!(preorder.len(), 9);
    assert_eq!(preorder[0].own_span(), LineSpan::new(3, 4));
    assert_eq!(preorder[8].own_span(), LineSpan::new(16, 18));
}

#[test]
fn query_fixture_keeps_begin_region_inside_one_block_without_final_newline() {
    let document = parse(QUERY_FIXTURE);

    assert_eq!(document.preamble(), LineSpan::new(0, 2));
    assert!(!document.ends_with_newline());
    assert_eq!(document.blocks().len(), 1);
    assert_eq!(document.blocks()[0].own_span(), LineSpan::new(2, 10));
    assert_eq!(document.blocks()[0].subtree_span(), LineSpan::new(2, 10));
    assert!(document.blocks()[0].children().is_empty());

    let lines = document.raw_lines(document.blocks()[0].own_span());
    assert_eq!(lines[0].text(), "- query-table:: false");
    assert_eq!(lines[1].text(), "  #+BEGIN_QUERY");
    assert_eq!(lines[7].text(), "  #+END_QUERY");
    assert_eq!(lines[7].ending(), None);
}

#[test]
fn journal_fixture_has_tab_indented_children() {
    let document = parse(JOURNAL_FIXTURE);

    assert!(document.preamble().is_empty());
    assert_eq!(document.blocks().len(), 4);
    assert_eq!(document.blocks()[1].children().len(), 1);
    assert_eq!(document.blocks()[1].children()[0].indent(), 1);
    assert_eq!(document.blocks()[3].children().len(), 1);
    assert_eq!(document.blocks()[3].children()[0].indent(), 1);
}

#[test]
fn mixed_line_endings_are_not_normalized() {
    let source = "\u{feff}- a\r\n  continued\n- b\r\n";
    let document = parse(source);

    assert!(document.has_bom());
    assert_eq!(document.lines()[0].ending(), Some(LineEnding::Crlf));
    assert_eq!(document.lines()[1].ending(), Some(LineEnding::Lf));
    assert_eq!(document.lines()[2].ending(), Some(LineEnding::Crlf));
    assert_roundtrip(source);
}

/// YAML frontmatter, a blank line, and no outliner line at all.
///
/// This is the source a reader describes as "a file with only frontmatter", and it has no block: the
/// frontmatter is the preamble and the blank line is trailing. A page in this shape must report
/// `block_count` zero, so nothing downstream may invent a block to go with the file.
#[test]
fn frontmatter_only_page_has_no_blocks() {
    const SOURCE: &str = "---\ntitle: Apr 19th, 2021\n---\n\n";
    let document = parse(SOURCE);

    assert!(document.blocks().is_empty(), "no outliner line, no block");
    assert_eq!(document.preamble(), LineSpan::new(0, 3));
    assert_eq!(document.trailing(), LineSpan::new(3, 4));
    assert_roundtrip(SOURCE);
}

/// The same title, one bare `-` after the blank line, and no final newline.
///
/// A bare marker is an outliner line with no text after it: the file holds one block whose text is
/// empty, and a reader showing a bullet with nothing beside it is describing the file correctly.
/// This test exists so the shape is not mistaken for a parser defect the way it was once reported;
/// the difference from [`frontmatter_only_page_has_no_blocks`] is exactly that dash.
#[test]
fn a_page_whose_only_outliner_line_is_a_bare_dash_has_one_empty_block() {
    const SOURCE: &str = "---\ntitle: Apr 19th, 2021\n---\n\n-";
    let document = parse(SOURCE);

    assert_eq!(document.blocks().len(), 1);
    let block = &document.blocks()[0];
    assert_eq!(block.marker(), BlockMarker::Bullet);
    assert_eq!(block.own_span(), LineSpan::new(4, 5));
    assert!(block.children().is_empty());
    assert_eq!(document.raw_lines(block.own_span())[0].text(), "-");
    assert_roundtrip(SOURCE);
}
