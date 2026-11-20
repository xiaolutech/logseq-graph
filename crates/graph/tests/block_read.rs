//! Block reads by public target: durable ids and ephemeral ids (ADR-0005, ADR-0012).
//!
//! Every test writes graph files, opens a [`Graph`], and asks through [`Graph::block`], so it covers
//! discovery, projection, and the read in one path. Each hit is asserted next to a near miss that
//! must not answer: an id no block states, an ephemeral id no row holds, a page whose rows were
//! removed, or a page with the same structure and different text. An id that names nothing is an
//! *answer* — `None` — rather than an error, because "the graph moved past this id" is a normal fact
//! about ephemeral identity; a diverged index and an ambiguous durable id are refusals.

use logseq_graph::domain::{BlockId, BlockIdentityKind, BlockMarker, BlockSelector, EphemeralId};
use logseq_graph::engine::{EngineError, Freshness};

mod support;

use support::{bound, TempGraph};

/// A page whose blocks exercise every level and both identity kinds.
///
/// Blocks as `(preorder, depth, sibling_index)`: `plain root (0, 0, 0)`, `child one (1, 1, 0)`,
/// `grandchild (2, 2, 0)`, `child two (3, 1, 1)`, and `second root (4, 0, 1)`. The two ids are
/// written in mixed case on purpose; the projection stores the lowercase spelling.
const ALPHA: &str = "\
- plain root
\t- child one
\t\t- grandchild
\t- child two
\t  id:: 01234567-89AB-CDEF-0123-456789ABCDEF
- second root
  id:: FEDCBA98-7654-3210-FEDC-BA9876543210
";

/// A page with exactly `ALPHA`'s structure and different text, and no durable id at all.
const BETA: &str = "\
- other root
\t- other child one
\t\t- other grandchild
\t- other child two
- other second root
";

const CHILD_TWO_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
const SECOND_ROOT_ID: &str = "fedcba98-7654-3210-fedc-ba9876543210";

/// The `id::` UUID a block states, as a caller would hold it.
fn id(value: &str) -> BlockId {
    BlockId::parse(value).expect("valid 8-4-4-4-12 UUID")
}

/// A durable selector for `value`.
fn durable(value: &str) -> BlockSelector {
    BlockSelector::Durable(id(value))
}

/// An ephemeral id no projection ever minted.
///
/// The shape is public because a caller hands one back; no row holds this value, which is exactly
/// the "this id names nothing" case: a stale id, an id from a rebuilt index, or a value nobody
/// minted. It must answer `None` everywhere, never a block that happens to sit somewhere.
fn unminted() -> BlockSelector {
    BlockSelector::Ephemeral(
        EphemeralId::parse("00000000-0000-0000-0000-000000000001").expect("valid UUID shape"),
    )
}

/// A page with three top-level blocks; the middle one holds property lines.
const PROPS: &str = "\
- first
- middle
	key:: value
	key:: again
	note:: kept
- last
";

/// The page with one top-level block and two children, for sibling tests.
const CHILDREN: &str = "\
- parent
	- child one
	- child two
	- child three
";

/// The page whose two top-level blocks must see each other as siblings.
const TOP_LEVEL: &str = "\
- one
- two
";

#[test]
fn block_views_report_heading_and_bullet_markers() {
    let graph = TempGraph::new("block-view-markers");
    graph.write("pages/Markers.md", "## heading\n- bullet\n");
    let mut opened = graph.open();

    let blocks = opened
        .page_blocks("Markers")
        .expect("query the page's blocks")
        .present()
        .expect("the page is projected")
        .clone();
    assert_eq!(
        blocks
            .iter()
            .map(|block| block.marker())
            .collect::<Vec<_>>(),
        [BlockMarker::Heading { level: 2 }, BlockMarker::Bullet],
        "a page read carries each block's marker beside its text"
    );

    for (block, marker) in blocks
        .iter()
        .zip([BlockMarker::Heading { level: 2 }, BlockMarker::Bullet])
    {
        let read = opened
            .block(block.selector())
            .expect("query the block by the selector its page view carries");
        assert_eq!(
            read.present().expect("the block exists").marker(),
            marker,
            "a single-block read reports the same marker as the page read"
        );
    }

    let durable = BlockSelector::Durable(
        BlockId::parse("01234567-89ab-cdef-0123-456789abcdef").expect("valid durable id"),
    );
    graph.write(
        "pages/Markers.md",
        "## heading\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n- bullet\n",
    );
    let mut reopened = graph.open();
    assert_eq!(
        reopened
            .block(&durable)
            .expect("query the durable block")
            .present()
            .expect("the durable block exists")
            .marker(),
        BlockMarker::Heading { level: 2 },
        "a durable read decodes the same stored marker as an ephemeral read"
    );
}

#[test]
fn block_sibling_views_report_the_markers_of_both_neighbours() {
    let graph = TempGraph::new("block-sibling-markers");
    graph.write("pages/Markers.md", "## previous\n- target\n### next\n");
    let mut opened = graph.open();
    let target = bound(&graph, "Markers", &[1]);

    let (previous, next) = opened
        .block_siblings(&target)
        .expect("query the target's siblings")
        .into_value();
    assert_eq!(
        previous.expect("a previous sibling").marker(),
        BlockMarker::Heading { level: 2 }
    );
    assert_eq!(
        next.expect("a next sibling").marker(),
        BlockMarker::Heading { level: 3 }
    );
}

#[test]
fn block_properties_keep_written_order_and_repeated_keys() {
    let graph = TempGraph::new("block-properties");
    graph.write("pages/Props.md", PROPS);
    let mut opened = graph.open();

    // Both selector kinds reach the same block; the middle one is `[1]` by position — a property
    // line does not create a block — and states no `id::`, so the durable kind is exercised
    // separately in the stale test below.
    let props_read = opened
        .block_properties(&bound(&graph, "Props", &[1]))
        .expect("query the middle block's properties");
    let properties = props_read.present().expect("the located block exists");
    assert_eq!(
        properties,
        &[
            ("key".to_owned(), "value".to_owned()),
            ("key".to_owned(), "again".to_owned()),
            ("note".to_owned(), "kept".to_owned()),
        ],
        "every line survives in written order; last-one-wins is the caller's rule"
    );

    // A block without property lines answers with an empty list, which is a different answer from
    // a block that does not exist.
    let bare_read = opened
        .block_properties(&bound(&graph, "Props", &[0]))
        .expect("query the first block's properties");
    let bare = bare_read.present().expect("the located block exists");
    assert!(bare.is_empty());

    let absent = opened
        .block_properties(&unminted())
        .expect("an id no row holds is an answer");
    assert!(
        absent.present().is_none(),
        "an id that names no block means no property list, not an empty one"
    );
}

#[test]
fn block_siblings_report_the_neighbours_on_both_sides() {
    let graph = TempGraph::new("block-siblings");
    graph.write("pages/Children.md", CHILDREN);
    let mut opened = graph.open();

    // The middle child sees one neighbour on each side, ordered by sibling position.
    let read = opened
        .block_siblings(&bound(&graph, "Children", &[0, 1]))
        .expect("query the middle child's siblings");
    let (previous, next) = read.value();
    let previous = previous
        .as_ref()
        .expect("a middle block has a previous sibling");
    let next = next.as_ref().expect("a middle block has a next sibling");
    assert_eq!(previous.text(), "child one");
    assert_eq!(next.text(), "child three");
    assert_eq!(previous.parent_preorder(), Some(0));
    assert_eq!(next.parent_preorder(), Some(0));

    // The first child has no previous sibling; the last has no next. An edge is `None`, not the
    // block on the other end.
    let first = opened
        .block_siblings(&bound(&graph, "Children", &[0, 0]))
        .expect("query the first child's siblings");
    let (previous, _) = first.value();
    assert!(previous.is_none(), "the first sibling has no previous one");

    let last = opened
        .block_siblings(&bound(&graph, "Children", &[0, 2]))
        .expect("query the last child's siblings");
    let (_, next) = last.value();
    assert!(next.is_none(), "the last sibling has no next one");
}

#[test]
fn two_top_level_blocks_are_siblings_of_each_other() {
    let graph = TempGraph::new("block-siblings-top-level");
    graph.write("pages/Top.md", TOP_LEVEL);
    let mut opened = graph.open();

    // `parent_id` is NULL for a top-level block, and an SQL `= NULL` is never true, so an equality
    // here would report every top-level block as having no siblings. The read must see through it:
    // two roots are neighbours exactly as two children are.
    let first = opened
        .block_siblings(&bound(&graph, "Top", &[0]))
        .expect("query the first root's siblings");
    let (previous, next) = first.value();
    assert!(previous.is_none(), "the first root has no previous sibling");
    assert_eq!(
        next.as_ref().expect("the second root is next").text(),
        "two"
    );

    let second = opened
        .block_siblings(&bound(&graph, "Top", &[1]))
        .expect("query the second root's siblings");
    let (previous, next) = second.value();
    assert_eq!(
        previous
            .as_ref()
            .expect("the first root is previous")
            .text(),
        "one"
    );
    assert!(next.is_none(), "the last root has no next sibling");
}

#[test]
fn an_id_that_names_nothing_answers_none_for_properties_and_siblings_too() {
    let graph = TempGraph::new("block-reads-stale");
    graph.write("pages/Props.md", PROPS);
    let mut opened = graph.open();

    let stop = unminted();
    let properties = opened
        .block_properties(&stop)
        .expect("an id no row holds is an answer");
    assert!(
        properties.present().is_none(),
        "an id that names no block has no property list: {properties:?}"
    );
    let siblings = opened
        .block_siblings(&stop)
        .expect("an id no row holds is an answer");
    let (previous, next) = siblings.value();
    assert!(
        previous.is_none() && next.is_none(),
        "an id that names no block has no neighbours: {siblings:?}"
    );

    // The durable kind refuses instead, when more than one block states the id.
    let duplicated = "\
- a
  id:: 11111111-1111-1111-1111-111111111111
- b
  id:: 11111111-1111-1111-1111-111111111111
";
    graph.write("pages/Props.md", duplicated);
    let mut reopened = graph.open();
    let selector = durable("11111111-1111-1111-1111-111111111111");
    let properties = reopened.block_properties(&selector);
    assert!(
        matches!(properties, Err(EngineError::AmbiguousBlockId { .. })),
        "an ambiguous id fails the property read: {properties:?}"
    );
    let siblings = reopened.block_siblings(&selector);
    assert!(
        matches!(siblings, Err(EngineError::AmbiguousBlockId { .. })),
        "an ambiguous id fails the sibling read: {siblings:?}"
    );
}

/// Open the graph holding `ALPHA` and `BETA`.
fn open(graph: &TempGraph) -> logseq_graph::engine::Graph {
    graph.write("pages/Alpha Page.md", ALPHA);
    graph.write("pages/Beta Page.md", BETA);
    graph.open()
}

#[test]
fn a_durable_target_answers_the_block_that_states_the_id() {
    let graph = TempGraph::new("block-durable-hit");
    let mut opened = open(&graph);

    let read = opened
        .block(&durable(CHILD_TWO_ID))
        .expect("query by durable id");
    let view = read.present().expect("the block that states the id");

    // The page is the spelling `pages.name` stores, not the one the id lookup never mentioned.
    assert_eq!(view.page(), "Alpha Page");
    assert_eq!(view.preorder(), 3);
    assert_eq!(view.depth(), 1);
    assert_eq!(
        view.parent_preorder(),
        Some(0),
        "the parent position is what a walk to the top level needs"
    );
    // The block's own text, property line included, and nothing of its neighbours or its page.
    assert_eq!(
        view.text(),
        "child two\nid:: 01234567-89AB-CDEF-0123-456789ABCDEF"
    );
    assert_eq!(view.identity(), BlockIdentityKind::Durable);
    assert_eq!(
        view.durable_id().map(BlockId::as_str),
        Some(CHILD_TWO_ID),
        "the view reports the normalized id the block states"
    );

    // A top-level block answers with no parent, which is a different answer from preorder 0.
    let root = opened
        .block(&durable(SECOND_ROOT_ID))
        .expect("query the top-level id")
        .present()
        .expect("the second root states the id")
        .clone();
    assert_eq!(root.preorder(), 4);
    assert_eq!(root.depth(), 0);
    assert_eq!(root.parent_preorder(), None);
    assert_eq!(
        root.text(),
        "second root\nid:: FEDCBA98-7654-3210-FEDC-BA9876543210",
        "the text keeps the spelling the file wrote, even though the id is stored folded"
    );
}

#[test]
fn a_durable_target_that_no_block_states_is_none_and_not_an_error() {
    let graph = TempGraph::new("block-durable-miss");
    let mut opened = open(&graph);

    // A durable id can name a block that was deleted, or one that never existed; both are answers.
    // `expect` is the assertion that matters here: this must not be `Err`.
    let read = opened
        .block(&durable("99999999-9999-9999-9999-999999999999"))
        .expect("an id no block states is a normal answer");
    assert!(read.present().is_none());
    assert!(
        !read.is_stale(),
        "a miss still carries the freshness of the read that produced it"
    );

    // The page the ids live on is irrelevant to that: the id is the target.
    let other = opened
        .block(&durable("00000000-0000-0000-0000-000000000000"))
        .expect("still not an error");
    assert!(other.present().is_none());
}

#[test]
fn a_durable_target_ignores_case_the_way_block_id_does() {
    let graph = TempGraph::new("block-durable-case");
    let mut opened = open(&graph);

    let found = opened
        .block(&durable("01234567-89AB-CDEF-0123-456789ABCDEF"))
        .expect("query with the spelling the file wrote")
        .present()
        .map(|view| view.preorder());
    assert_eq!(
        found,
        Some(3),
        "the seek uses the normalized id, so any casing reaches the same block"
    );

    let lowercase = opened
        .block(&durable("FEDCBA98-7654-3210-FEDC-BA9876543210"))
        .expect("query an uppercase file value with a lowercase selector")
        .present()
        .and_then(|view| view.durable_id().map(BlockId::as_str))
        .map(str::to_owned);
    assert_eq!(
        lowercase.as_deref(),
        Some(SECOND_ROOT_ID),
        "and the view reports the normalized spelling"
    );
}

#[test]
fn an_ephemeral_id_answers_the_block_the_projection_minted_it_for() {
    let graph = TempGraph::new("block-ephemeral-hit");
    let mut opened = open(&graph);

    // `child one` is the first child of the first root, so its preorder path is `[0, 0]`. It states
    // no `id::`, so the projection minted an ephemeral id for it and the read answers that block.
    let child = opened
        .block(&bound(&graph, "Alpha Page", &[0, 0]))
        .expect("query the plain child");
    let view = child.present().expect("the block the id names");
    assert_eq!(view.preorder(), 1);
    assert_eq!(view.depth(), 1);
    assert_eq!(view.parent_preorder(), Some(0));
    assert_eq!(view.text(), "child one");
    assert_eq!(view.identity(), BlockIdentityKind::Ephemeral);
    assert_eq!(
        view.durable_id(),
        None,
        "a block without an `id::` has no durable identity"
    );
    assert!(
        view.ephemeral_id().is_some(),
        "the view names the id the projection minted"
    );
    assert_eq!(
        view.selector(),
        &bound(&graph, "Alpha Page", &[0, 0]),
        "the view's selector is the address a write can take straight back"
    );

    let grandchild = opened
        .block(&bound(&graph, "Alpha Page", &[0, 0, 0]))
        .expect("query the grandchild")
        .present()
        .map(|view| (view.preorder(), view.text().to_owned()));
    assert_eq!(grandchild, Some((2, "grandchild".to_owned())));

    // A block that states an `id::` is answered by that id: the two kinds never name one block, so
    // there is no ephemeral id for it at all (ADR-0012).
    let second_root = opened
        .block(&bound(&graph, "Alpha Page", &[4]))
        .expect("query the second root")
        .present()
        .map(|view| {
            (
                view.preorder(),
                view.parent_preorder(),
                view.ephemeral_id().is_some(),
            )
        });
    assert_eq!(second_root, Some((4, None, false)));
    assert_eq!(bound(&graph, "Alpha Page", &[4]), durable(SECOND_ROOT_ID));

    // The page is matched by `page_name_key`, so another spelling reaches the same page, and the
    // view still reports the stored spelling.
    let other_spelling = opened
        .block(&bound(&graph, "alpha page", &[0, 0]))
        .expect("query by another spelling of the page")
        .present()
        .map(|view| view.page().to_owned());
    assert_eq!(other_spelling.as_deref(), Some("Alpha Page"));
}

#[test]
fn an_id_from_a_rebuilt_index_names_nothing() {
    let graph = TempGraph::new("block-ephemeral-rebuilt");
    let stale = {
        let mut opened = open(&graph);
        let selector = bound(&graph, "Alpha Page", &[0, 0]);
        let id = selector
            .ephemeral_id()
            .expect("a plain block answers an ephemeral id")
            .clone();
        assert!(
            opened
                .block(&selector)
                .expect("the id is fresh")
                .present()
                .is_some(),
            "the id must work before the index is rebuilt"
        );
        BlockSelector::Ephemeral(id)
    };

    // Deleting the derived state is the ordinary answer to an incompatible database (ADR-0002). The
    // rebuild parses the same files and mints new ids for them, so a value read from the old index
    // names nothing in the new one — there is no instance or revision to check, because the index
    // itself is the only place an id ever existed.
    std::fs::remove_file(graph.index_path()).expect("remove the index");
    let mut rebuilt = graph.open();

    let read = rebuilt
        .block(&stale)
        .expect("an id no row holds is an answer");
    assert!(
        read.present().is_none(),
        "the rebuilt index holds none of the ids the old one minted"
    );

    // The same block is answerable again, with the id this build minted for it.
    let fresh = bound(&graph, "Alpha Page", &[0, 0]);
    assert_ne!(fresh, stale, "a rebuild mints new ids");
    assert_eq!(
        rebuilt
            .block(&fresh)
            .expect("the fresh id answers")
            .present()
            .map(|view| view.text().to_owned()),
        Some("child one".to_owned())
    );
}

#[test]
fn an_id_no_row_holds_answers_none() {
    let graph = TempGraph::new("block-ephemeral-unminted");
    let mut opened = open(&graph);

    // A well-shaped id nobody minted is the plainest "names nothing" case: no page, no position, no
    // revision, and no error — the block read says what the index says, which is nothing.
    let read = opened
        .block(&unminted())
        .expect("an id no row holds is an answer, not a failure");
    assert!(read.present().is_none());
}

#[test]
fn an_id_does_not_alias_a_page_with_the_same_structure() {
    let graph = TempGraph::new("block-ephemeral-alias");
    let mut opened = open(&graph);

    // Both pages carry a plain block at the same position and they are different blocks, with
    // different ids: the id names a row of one page, and nothing about the other page matches it.
    let alpha = bound(&graph, "Alpha Page", &[0, 0]);
    let beta = bound(&graph, "Beta Page", &[0, 0]);
    assert_ne!(alpha, beta, "two blocks never share one id");

    let alpha_text = opened
        .block(&alpha)
        .expect("query alpha")
        .present()
        .map(|view| view.text().to_owned());
    let beta_text = opened
        .block(&beta)
        .expect("query beta")
        .present()
        .map(|view| view.text().to_owned());

    assert_eq!(alpha_text.as_deref(), Some("child one"));
    assert_eq!(
        beta_text.as_deref(),
        Some("other child one"),
        "the id answers the block of the page it was minted for"
    );
}

#[test]
fn reprojecting_the_page_makes_an_old_id_name_nothing() {
    let graph = TempGraph::new("block-ephemeral-reproject");
    let mut opened = open(&graph);
    let stale = bound(&graph, "Alpha Page", &[0, 0]);
    assert!(
        opened
            .block(&stale)
            .expect("the id is fresh")
            .present()
            .is_some(),
        "the id must work before the page changes"
    );

    // A new first child shifts every later sibling, so the position the row was projected at now
    // belongs to a different block. The scan reprojects the page, which replaces every row and
    // therefore every ephemeral id of it.
    graph.write(
        "pages/Alpha Page.md",
        "- plain root\n\t- inserted before\n\t- child one\n\t\t- grandchild\n\t- child two\n\t  id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n- second root\n  id:: FEDCBA98-7654-3210-FEDC-BA9876543210\n",
    );

    let read = opened
        .reads(Freshness::CheckNow)
        .block(&stale)
        .expect("an id whose page was reprojected is an answer");
    assert!(
        read.present().is_none(),
        "the old id names nothing after the page was projected again"
    );

    // The block that now stands at that position is answered by its own id, and that id is a new
    // value: re-querying is the only way to reach the block, which is what fail-closed means here.
    let current = opened.metadata().expect("read metadata").into_value();
    assert!(
        current.revision() > read.meta().revision() - 1,
        "the scan committed a revision"
    );
    let reused = bound(&graph, "Alpha Page", &[0, 1]);
    assert_ne!(reused, stale);
    assert_eq!(
        opened
            .block(&reused)
            .expect("the new projection answers")
            .present()
            .map(|view| view.text().to_owned())
            .as_deref(),
        Some("child one"),
        "the position now names a different block, which the old id must not have reached"
    );
}

#[test]
fn both_identity_kinds_obey_the_freshness_policy() {
    let graph = TempGraph::new("block-freshness");
    let mut opened = open(&graph);
    // Both selectors are built from the state before the failure, so freshness is the only reason
    // left for either read to be refused.

    // Make the next scan observe a file it cannot project, which is what records a divergence.
    graph.write("pages/Alpha Page.md", b"- a\n\xff\n");
    opened
        .reads(Freshness::CheckNow)
        .block(&durable(CHILD_TWO_ID))
        .expect_err("a check over an unreadable file must fail");

    // The default policy refuses both identity kinds instead of answering from rows it cannot vouch
    // for: a block read is not a way around the freshness rule.
    for selector in [durable(CHILD_TWO_ID), bound(&graph, "Alpha Page", &[0, 1])] {
        match opened.block(&selector) {
            Err(EngineError::StaleIndex { reason }) => assert!(
                reason.contains("UTF-8"),
                "the refusal must carry the scan failure: {reason}"
            ),
            other => panic!("expected {selector} to be refused as stale, got {other:?}"),
        }
    }

    // The policy that accepts stale data answers, and says so, for both kinds as well.
    let durable_view = opened
        .reads(Freshness::AllowStale)
        .block(&durable(CHILD_TWO_ID))
        .expect("an allowed stale read answers");
    assert!(durable_view.is_stale());
    assert_eq!(
        durable_view.present().map(|view| view.preorder()),
        Some(3),
        "the rows from before the failed scan are still the answer"
    );
}

/// The id two pages state in the ambiguity tests below.
const SHARED_ID: &str = "11111111-2222-3333-4444-555555555555";

#[test]
fn an_id_stated_on_two_pages_is_refused_instead_of_picking_one() {
    // Two pages may legitimately state one `id::`: a hand edit, a copy-paste that kept the identity
    // line, two notes merged. The projection records both rows, because refusing the second file
    // would make one bad line hide a whole page. A read is where that becomes unanswerable: it
    // answers about the index, cannot open either file to tell a real duplicate from a stale row,
    // and has no basis for preferring one candidate. So it says so, and names both pages.
    let graph = TempGraph::new("block-ambiguous-pages");
    graph.write(
        "pages/alpha.md",
        "- alpha block\n  id:: 11111111-2222-3333-4444-555555555555\n",
    );
    graph.write(
        "pages/beta.md",
        "- beta block\n  id:: 11111111-2222-3333-4444-555555555555\n",
    );
    let mut opened = graph.open();

    match opened.block(&durable(SHARED_ID)) {
        Err(EngineError::AmbiguousBlockId { id, pages }) => {
            assert_eq!(id.as_str(), SHARED_ID);
            assert_eq!(
                pages,
                vec!["alpha".to_owned(), "beta".to_owned()],
                "both pages are named, in sorted order"
            );
        }
        other => panic!("expected an ambiguous id, got {other:?}"),
    }
}

#[test]
fn an_id_stated_twice_on_one_page_is_refused_too() {
    // The same page is still a refusal, not a page to look at: the read cannot open the file, so it
    // cannot tell whether the two rows are still there or one of them survived a hand edit. Guessing
    // between two rows of one page is no better than guessing between two pages.
    let graph = TempGraph::new("block-ambiguous-one-page");
    graph.write(
        "pages/alpha.md",
        "- first\n  id:: 11111111-2222-3333-4444-555555555555\n- second\n  id:: 11111111-2222-3333-4444-555555555555\n",
    );
    let mut opened = graph.open();

    match opened.block(&durable(SHARED_ID)) {
        Err(EngineError::AmbiguousBlockId { id, pages }) => {
            assert_eq!(id.as_str(), SHARED_ID);
            assert_eq!(
                pages,
                vec!["alpha".to_owned()],
                "one page is named once, even though two blocks state the id"
            );
        }
        other => panic!("expected an ambiguous id, got {other:?}"),
    }
}
