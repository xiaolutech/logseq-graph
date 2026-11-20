//! Page listings through the graph facade: the page list, one page's blocks, and the block a
//! backlink reports.
//!
//! Every test writes graph files, opens a [`Graph`], and asks through the public read methods, so it
//! covers discovery, projection, and the read in one path. The three reads here are the ones a
//! caller prints a graph with: a page list, a page's block text, and a backlink's position and text.
//! Each answer is asserted next to the case that must not be mistaken for it: a page that does not
//! exist, a spelling that folds to the same key, a block that states no durable id, or a page that
//! references one target from several blocks.

use logseq_graph::domain::{BlockIdentityKind, PageKind, PageSummary};
use logseq_graph::engine::Freshness;

mod support;

use support::TempGraph;

/// Names of a page listing, in the order the read returned them.
fn page_names(pages: &[PageSummary]) -> Vec<&str> {
    pages.iter().map(PageSummary::name).collect()
}

/// Block counts of a page listing, in the order the read returned them.
fn block_counts(pages: &[PageSummary]) -> Vec<u32> {
    pages.iter().map(PageSummary::block_count).collect()
}

/// Kinds of a page listing, in the order the read returned them.
fn kinds(pages: &[PageSummary]) -> Vec<PageKind> {
    pages.iter().map(PageSummary::kind).collect()
}

/// A page whose blocks exercise every level and one durable id.
///
/// Blocks as `(preorder, depth)`: `plain root (0, 0)`, `child one (1, 1)`, `grandchild (2, 2)`,
/// `child two (3, 1)`, and `second root (4, 0)`. The id is written in mixed case on purpose; the
/// projection stores the lowercase spelling.
const ALPHA: &str = "\
- plain root
\t- child one
\t\t- grandchild
\t- child two
\t  id:: 01234567-89AB-CDEF-0123-456789ABCDEF
- second root
";

#[test]
fn pages_lists_every_page_with_its_block_count_in_folded_name_order() {
    let graph = TempGraph::new("page-list");
    graph.write("pages/alpha.md", "- one\n- two\n");
    graph.write("pages/Bravo.md", "- only block\n");
    graph.write("journals/2026_08_13.md", "- met [[someone]]\n");
    let mut opened = graph.open();

    let pages = opened.pages().expect("query page list");

    // One row per stored page, in folded name order. Both kinds share the listing, and each row
    // states which one it is, so a caller that wants only journals never issues a second read.
    assert_eq!(
        page_names(pages.value()),
        ["alpha", "Aug 13th, 2026", "Bravo", "someone"],
        "the journal is listed under the date title the configuration renders"
    );
    assert_eq!(
        block_counts(pages.value()),
        [2, 1, 1, 0],
        "each count is that page's own block count"
    );
    assert_eq!(
        kinds(pages.value()),
        [
            PageKind::Page,
            PageKind::Journal,
            PageKind::Page,
            PageKind::Page
        ],
        "each row states the kind of page it is"
    );
    assert!(
        !pages.is_stale(),
        "a successful open leaves the listing verifiable"
    );
}

#[test]
fn an_empty_graph_lists_no_pages() {
    let graph = TempGraph::new("page-list-empty");
    let mut opened = graph.open();

    let pages = opened.pages().expect("query page list of an empty graph");

    assert!(
        pages.value().is_empty(),
        "an empty graph has no page row at all, not one row with an empty name: {:?}",
        pages.value()
    );
}

#[test]
fn page_blocks_returns_every_block_in_preorder_with_its_own_text() {
    let graph = TempGraph::new("page-blocks");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    let read = opened.page_blocks("alpha").expect("query page blocks");
    let blocks = read.present().expect("alpha exists");

    assert_eq!(
        blocks
            .iter()
            .map(|block| block.preorder())
            .collect::<Vec<_>>(),
        [0, 1, 2, 3, 4],
        "blocks arrive in preorder order, not in tree order or storage order"
    );
    assert_eq!(
        blocks.iter().map(|block| block.depth()).collect::<Vec<_>>(),
        [0, 1, 2, 1, 0],
        "each block reports its own depth"
    );
    assert_eq!(
        blocks
            .iter()
            .map(|block| block.parent_preorder())
            .collect::<Vec<_>>(),
        [None, Some(0), Some(1), Some(0), None],
        "a nested block reports the parent it sits under, and a root reports none"
    );
    assert_eq!(
        blocks.iter().map(|block| block.text()).collect::<Vec<_>>(),
        [
            "plain root",
            "child one",
            "grandchild",
            "child two\nid:: 01234567-89AB-CDEF-0123-456789ABCDEF",
            "second root"
        ],
        "each view carries its own text only, property lines included and children excluded"
    );
    assert!(
        blocks.iter().all(|block| block.page() == "alpha"),
        "every view names the page the read asked for"
    );
}

#[test]
fn page_blocks_marks_durable_identity_only_where_a_block_states_an_id() {
    let graph = TempGraph::new("page-block-identity");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    let read = opened.page_blocks("alpha").expect("query page blocks");
    let blocks = read.present().expect("alpha exists");

    let kinds: Vec<BlockIdentityKind> = blocks.iter().map(|block| block.identity()).collect();
    assert_eq!(
        kinds,
        [
            BlockIdentityKind::Ephemeral,
            BlockIdentityKind::Ephemeral,
            BlockIdentityKind::Ephemeral,
            BlockIdentityKind::Durable,
            BlockIdentityKind::Ephemeral,
        ],
        "only the block that states an `id::` is durably named"
    );
    assert_eq!(
        blocks[3].durable_id().map(|id| id.as_str()),
        Some("01234567-89ab-cdef-0123-456789abcdef"),
        "the id is reported in the canonical lowercase spelling"
    );
    assert!(
        blocks[0].durable_id().is_none(),
        "a block that states no id carries none"
    );
}

#[test]
fn page_blocks_matches_the_folded_name_and_answers_absent_for_a_missing_page() {
    let graph = TempGraph::new("page-blocks-lookup");
    graph.write("pages/Ärger.md", "- a block\n");
    let mut opened = graph.open();

    // The page name is an identity, so the other spelling asks the same question.
    let lower = opened.page_blocks("ärger").expect("query page blocks");
    assert_eq!(
        lower
            .present()
            .expect("ärger is the page stored as Ärger")
            .len(),
        1
    );
    assert_eq!(
        lower.value().as_ref().expect("page exists")[0].page(),
        "Ärger",
        "the view keeps the stored spelling, not the one the read used"
    );

    let missing = opened.page_blocks("missing").expect("query page blocks");
    assert!(
        missing.present().is_none(),
        "a page that does not exist is absent, not an empty list"
    );
}

#[test]
fn a_backlink_reports_the_earliest_referencing_block() {
    let graph = TempGraph::new("backlink-block");
    graph.write("pages/Ä.md", "- the target page\n");
    graph.write(
        "pages/bravo.md",
        "- sees [[ä]] and [[Ä]]\n\t- nested [[ä|an alias]]\n- again [[ä]]\n",
    );
    graph.write("pages/charlie.md", "- later [[Ä][a description]]\n");
    let mut opened = graph.open();

    let read = opened.backlinks("ä").expect("query backlinks");
    let backlinks = read.value();

    // One element per page, ordered by folded page name, and the block reported is that page's
    // earliest reference: bravo's first block holds two references and precedes the nested one.
    assert_eq!(
        backlinks.iter().map(|link| link.page()).collect::<Vec<_>>(),
        ["bravo", "charlie"]
    );
    assert_eq!(
        backlinks
            .iter()
            .map(|link| link.preorder())
            .collect::<Vec<_>>(),
        [0, 0],
        "the representative block is the earliest one in the page, not the last"
    );
    assert_eq!(
        backlinks.iter().map(|link| link.text()).collect::<Vec<_>>(),
        ["sees [[ä]] and [[Ä]]", "later [[Ä][a description]]"],
        "the text belongs to the block that holds the reference"
    );
}

#[test]
fn a_backlink_of_a_nested_block_reports_that_blocks_position() {
    let graph = TempGraph::new("backlink-nested");
    graph.write("pages/target.md", "- the target page\n");
    graph.write(
        "pages/source.md",
        "- a root without a reference\n\t- a nested [[target]]\n",
    );
    let mut opened = graph.open();

    let read = opened.backlinks("target").expect("query backlinks");
    let backlinks = read.value();

    assert_eq!(backlinks.len(), 1);
    assert_eq!(
        backlinks[0].preorder(),
        1,
        "the position is the referencing block's own preorder, not its page's"
    );
    assert_eq!(backlinks[0].text(), "a nested [[target]]");
}

#[test]
fn listing_reads_go_through_the_freshness_policy() {
    let graph = TempGraph::new("listing-freshness");
    graph.write("pages/alpha.md", "- first\n");
    let mut opened = graph.open();

    let before = opened.pages().expect("query page list");
    assert_eq!(page_names(before.value()), ["alpha"]);
    let revision = before.meta().revision();

    // A file written by another process becomes visible under `CheckNow` and not before it.
    graph.write("pages/bravo.md", "- second\n");
    let after = opened
        .reads(Freshness::CheckNow)
        .pages()
        .expect("query page list after the scan");
    assert_eq!(page_names(after.value()), ["alpha", "bravo"]);
    assert!(
        after.meta().revision() > revision,
        "projecting the new page commits a newer revision"
    );

    let blocks = opened
        .reads(Freshness::CheckNow)
        .page_blocks("bravo")
        .expect("query page blocks after the scan");
    assert_eq!(
        blocks.present().expect("bravo is projected").len(),
        1,
        "the block read sees the same scan as the listing"
    );
}
