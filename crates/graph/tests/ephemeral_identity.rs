//! Projection-scoped ephemeral block identity (ADR-0012): the lifecycle, and the gate that makes it
//! safe to write through.
//!
//! An ephemeral id is a random value the projection mints for a block that states no `id::`. It lives
//! only in the index, so every question about it is a question about rows: which rows hold it, which
//! of them survive an edit, and what a write does when a row still holds an id whose page has moved
//! on underneath it. The last one is the reason this file exists: the id cannot be derived, so the
//! only way a stale one could reach a *neighbouring* block is a write that trusts the position a row
//! recorded against bytes the row was never projected from. The fingerprint gate is what refuses
//! that, with zero bytes written.

use std::collections::HashSet;

use logseq_graph::domain::{BlockIdentityKind, BlockSelector, EphemeralId};
use logseq_graph::engine::{Freshness, Graph, MoveTarget, WriteError};

mod support;

use support::{selector_at, TempGraph};

/// A page with three levels, none of them identified.
const ALPHA: &str = "\
- first
\t- child
\t\t- grandchild
\t- second child
- last
";

/// A second page, so the "one page changed, the others did not" tests have something to compare.
const BETA: &str = "\
- beta one
- beta two
";

/// A page whose blocks are easy to reorder by hand, for the external-edit cases.
const ORDERED: &str = "\
- a
\t- b
\t- c
- d
";

/// Text of a graph file, as it is on disk.
fn read_text(graph: &TempGraph, relative: &str) -> String {
    std::fs::read_to_string(graph.graph_root().join(relative)).expect("read graph file")
}

/// The ephemeral id of the block at `preorder` on `page`, read from the graph.
///
/// Panics when the block states an `id::` instead, which is the identity rule these tests keep
/// pinning: a block has one address, and an identified block has no ephemeral id at all.
fn ephemeral_at(graph: &mut Graph, page: &str, preorder: u32) -> EphemeralId {
    let selector = selector_at(graph, page, preorder);
    match selector {
        BlockSelector::Ephemeral(id) => id,
        BlockSelector::Durable(id) => panic!("the block at {preorder} states the id {id}"),
    }
}

/// Every ephemeral id of one page, in preorder order.
fn ids_of(graph: &mut Graph, page: &str) -> HashSet<EphemeralId> {
    let read = graph.page_blocks(page).expect("read the page's blocks");
    read.present()
        .expect("the page is projected")
        .iter()
        .filter_map(|block| block.ephemeral_id().cloned())
        .collect()
}

/// Whether a string is the `8-4-4-4-12` lowercase hexadecimal shape an id is spelled in.
fn has_uuid_shape(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(position, byte)| {
            if matches!(position, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                // Lowercase only: the projection stores the canonical spelling, so an id compared as
                // text by another surface cannot miss on case.
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

#[test]
fn a_block_without_an_id_gets_a_well_formed_id_and_two_reads_agree() {
    let graph = TempGraph::new("ephemeral-shape");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    let read = opened.page_blocks("alpha").expect("read the page");
    let blocks = read.present().expect("the page is projected");
    assert_eq!(blocks.len(), 5);
    for block in blocks {
        assert_eq!(
            block.identity(),
            BlockIdentityKind::Ephemeral,
            "no block of this page states an id::"
        );
        let id = block
            .ephemeral_id()
            .expect("an unidentified block carries the id the projection minted");
        assert!(
            has_uuid_shape(id.as_str()),
            "{:?} is not an 8-4-4-4-12 lowercase UUID",
            id.as_str()
        );
        assert_eq!(
            block.durable_id(),
            None,
            "the two identities are exclusive, so a minted block states no durable one"
        );
    }

    // The same revision answers the same ids: an id is stored state, not a value recomputed per read.
    let again = opened.page_blocks("alpha").expect("read the page again");
    assert_eq!(
        again.into_value(),
        Some(read.into_value().expect("the page is projected")),
        "two reads at one revision must agree about every id"
    );
}

#[test]
fn one_page_and_one_graph_never_hand_out_one_id_twice() {
    let graph = TempGraph::new("ephemeral-distinct");
    graph.write("pages/alpha.md", ALPHA);
    graph.write("pages/beta.md", BETA);
    let mut opened = graph.open();

    let alpha = ids_of(&mut opened, "alpha");
    let beta = ids_of(&mut opened, "beta");
    assert_eq!(alpha.len(), 5, "every block of alpha has its own id");
    assert_eq!(beta.len(), 2, "every block of beta has its own id");
    assert!(
        alpha.is_disjoint(&beta),
        "ids are unique across the whole index, not per page"
    );
}

#[test]
fn editing_one_page_replaces_its_ids_and_leaves_every_other_page_alone() {
    let graph = TempGraph::new("ephemeral-page-scoped");
    graph.write("pages/alpha.md", ALPHA);
    graph.write("pages/beta.md", BETA);
    let mut opened = graph.open();

    let alpha_before = ids_of(&mut opened, "alpha");
    let beta_before = ids_of(&mut opened, "beta");
    let changed = ephemeral_at(&mut opened, "alpha", 0);

    let selector = BlockSelector::Ephemeral(changed.clone());
    opened
        .update_block(&selector, "first, edited")
        .expect("edit one block of alpha");

    let alpha_after = ids_of(&mut opened, "alpha");
    let beta_after = ids_of(&mut opened, "beta");
    assert!(
        alpha_before.is_disjoint(&alpha_after),
        "a page edit reprojects the page, so none of its old ids survive"
    );
    assert_eq!(
        beta_before, beta_after,
        "another page's ids are not this write's business: the granularity is the page, not the graph"
    );
}

#[test]
fn an_id_from_before_an_edit_names_nothing_after_it() {
    let graph = TempGraph::new("ephemeral-expired");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    let stale = BlockSelector::Ephemeral(ephemeral_at(&mut opened, "alpha", 1));
    let live = BlockSelector::Ephemeral(ephemeral_at(&mut opened, "alpha", 0));
    opened
        .update_block(&live, "first, edited")
        .expect("edit the page");

    // The read says what the index says: the row is gone, so the id names nothing. It is an answer,
    // not an error, and it is never the block that now stands at that position.
    assert!(
        opened
            .block(&stale)
            .expect("an id no row holds is an answer")
            .present()
            .is_none(),
        "the old id must not resolve to anything"
    );

    // The write refuses explicitly instead of guessing, and writes nothing.
    let before = read_text(&graph, "pages/alpha.md");
    match opened.update_block(&stale, "wrong block") {
        Err(WriteError::EphemeralNotFound { id }) => {
            assert_eq!(Some(&id), stale.ephemeral_id());
        }
        other => panic!("expected the id to name nothing, got {other:?}"),
    }
    assert_eq!(read_text(&graph, "pages/alpha.md"), before);
}

/// The regression this whole change exists for.
///
/// Another program edits the file and nothing scans it: the index still holds the page's rows, so an
/// id read before that edit still exists and its row still names a preorder. The bytes it names are
/// different bytes, though, and the block at that preorder now is a different block. Without the
/// fingerprint gate the write would apply the row's coordinates to the new text and edit — or
/// delete, or move — whichever block stands there.
#[test]
fn an_id_whose_file_changed_unseen_is_refused_instead_of_reaching_a_neighbour() {
    let graph = TempGraph::new("ephemeral-unseen-edit");
    graph.write("pages/ordered.md", ORDERED);
    let mut opened = graph.open();

    // `b` is preorder 1 and `d` is preorder 3.
    let b = BlockSelector::Ephemeral(ephemeral_at(&mut opened, "ordered", 1));
    let d = BlockSelector::Ephemeral(ephemeral_at(&mut opened, "ordered", 3));

    // The id still resolves in the index, which is exactly the danger: nothing here knows the file
    // moved on.
    assert!(
        opened
            .block(&b)
            .expect("the read answers from the index")
            .present()
            .is_some(),
        "the stale id still names a row, which is why the index alone cannot be trusted"
    );

    // Another process restructures the page without touching the index: `b` moves under `d`, so
    // preorder 1 is now `c` and preorder 3 is now `b`.
    let external = "- a\n\t- c\n- b\n\t- c2\n- d\n";
    graph.write("pages/ordered.md", external);

    // An update through the old id of `b` must not edit `c`.
    match opened.update_block(&b, "edited through a stale id") {
        Err(WriteError::StaleVersion { page, .. }) => assert_eq!(page, "ordered"),
        other => panic!("expected the write to be refused as stale, got {other:?}"),
    }
    assert_eq!(
        read_text(&graph, "pages/ordered.md"),
        external,
        "a refused write leaves the bytes another program wrote exactly as they were"
    );

    // The same gate protects a removal, which would otherwise take a neighbouring subtree with it.
    match opened.remove_block(&b) {
        Err(WriteError::StaleVersion { page, .. }) => assert_eq!(page, "ordered"),
        other => panic!("expected the removal to be refused as stale, got {other:?}"),
    }
    assert_eq!(read_text(&graph, "pages/ordered.md"), external);

    // And a move, whose two ends are both coordinates in those bytes.
    match opened.move_block(&b, MoveTarget::After(d)) {
        Err(WriteError::StaleVersion { page, .. }) => assert_eq!(page, "ordered"),
        other => panic!("expected the move to be refused as stale, got {other:?}"),
    }
    assert_eq!(read_text(&graph, "pages/ordered.md"), external);

    // A new anchor introduces a block between the old positions, which is the other way an unseen
    // edit moves a neighbour onto the position an inserted block would land in.
    match opened.insert_block(MoveTarget::Before(b), "inserted through a stale id") {
        Err(WriteError::StaleVersion { page, .. }) => assert_eq!(page, "ordered"),
        other => panic!("expected the insert to be refused as stale, got {other:?}"),
    }
    assert_eq!(read_text(&graph, "pages/ordered.md"), external);
}

#[test]
fn a_reordered_file_under_an_old_id_writes_nothing() {
    let graph = TempGraph::new("ephemeral-reordered");
    graph.write("pages/ordered.md", "- b\n- a\n- c\n");
    let mut opened = graph.open();

    // The id of `b`, which is the page's first block as the projection saw it.
    let b = BlockSelector::Ephemeral(ephemeral_at(&mut opened, "ordered", 0));

    // Somebody puts the page in alphabetical order without the index hearing about it.
    let external = "- a\n- b\n- c\n";
    graph.write("pages/ordered.md", external);

    // Writing through the old id would edit `a`, one sibling away. It must refuse instead.
    match opened.update_block(&b, "not a") {
        Err(WriteError::StaleVersion { page, .. }) => assert_eq!(page, "ordered"),
        other => panic!("expected the write to be refused as stale, got {other:?}"),
    }
    assert_eq!(
        read_text(&graph, "pages/ordered.md"),
        external,
        "the reordered file keeps every byte"
    );

    // Once a scan projects the reordered bytes, the page's blocks answer with new ids, and the old
    // value keeps naming nothing.
    opened
        .reads(Freshness::CheckNow)
        .page_blocks("ordered")
        .expect("scan the reordered page");
    let fresh = selector_at(&mut opened, "ordered", 0);
    assert_ne!(fresh, b, "a reprojected page mints new ids");
    assert!(
        opened
            .block(&b)
            .expect("an id no row holds is an answer")
            .present()
            .is_none(),
        "the old id still names nothing after the scan"
    );
}

#[test]
fn a_reprojection_that_changes_no_bytes_keeps_every_id() {
    let graph = TempGraph::new("ephemeral-reuse");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();
    let before = ids_of(&mut opened, "alpha");

    // `VerifyContent` reads every file instead of trusting `(size, mtime)`, so every page is parsed
    // and written again with the same bytes. Reusing the ids is what keeps that from expiring every
    // selector in the graph, which is the granularity ADR-0009 rejected (ADR-0012).
    opened
        .reads(Freshness::VerifyContent)
        .page_blocks("alpha")
        .expect("a full verification scan");

    let after = ids_of(&mut opened, "alpha");
    assert_eq!(
        before, after,
        "the same bytes parse to the same blocks, so their ids survive the re-projection"
    );

    // A stat-only refresh is the same fact from the other direction: the observation is new, the
    // content fingerprint is not, so nothing is re-projected and nothing is re-minted.
    let later = graph.modified("pages/alpha.md") + std::time::Duration::from_secs(60);
    graph.set_modified("pages/alpha.md", later);
    opened
        .reads(Freshness::CheckNow)
        .page_blocks("alpha")
        .expect("a stat-only refresh");
    assert_eq!(before, ids_of(&mut opened, "alpha"));
}

#[test]
fn a_block_that_states_an_id_has_no_ephemeral_id() {
    let graph = TempGraph::new("ephemeral-durable-exclusive");
    graph.write(
        "pages/mixed.md",
        "- plain\n- identified\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n",
    );
    let mut opened = graph.open();

    let read = opened.page_blocks("mixed").expect("read the page");
    let blocks = read.present().expect("the page is projected");
    assert_eq!(blocks.len(), 2);
    assert!(matches!(blocks[0].selector(), BlockSelector::Ephemeral(_)));
    assert!(blocks[0].ephemeral_id().is_some());
    assert_eq!(
        blocks[1].selector(),
        &BlockSelector::Durable(
            logseq_graph::domain::BlockId::parse("01234567-89ab-cdef-0123-456789abcdef")
                .expect("valid UUID")
        )
    );
    assert_eq!(
        blocks[1].ephemeral_id(),
        None,
        "a block that states an id:: is addressed by it and by nothing else"
    );
    assert_eq!(
        blocks[1].identity(),
        BlockIdentityKind::Durable,
        "the identity kind is read from the row, not from what a caller asked"
    );
}

#[test]
fn every_read_answer_edits_the_block_it_named_without_reading_again() {
    let graph = TempGraph::new("ephemeral-round-trip");
    graph.write("pages/alpha.md", ALPHA);
    let mut opened = graph.open();

    // Three different reads answer the same block; each answer's selector is a usable address.
    let second_child = BlockSelector::Ephemeral(ephemeral_at(&mut opened, "alpha", 3));
    let from_block = opened
        .block(&second_child)
        .expect("read by id")
        .present()
        .expect("the block the id names")
        .selector()
        .clone();
    let from_page_blocks = selector_at(&mut opened, "alpha", 3);
    let from_siblings = {
        let neighbours = opened
            .block_siblings(&from_page_blocks)
            .expect("read the neighbours");
        let (previous, _) = neighbours.value();
        previous
            .as_ref()
            .expect("the second child has a previous sibling")
            .selector()
            .clone()
    };
    assert_eq!(from_block, from_page_blocks);
    assert_eq!(
        from_siblings,
        selector_at(&mut opened, "alpha", 1),
        "the neighbour's selector names the neighbour, not the block that was asked about"
    );

    // Every selector a write answers is usable the same way: the insert's own answer names the block
    // it created, so the two writes chain with no read in between.
    let inserted = opened
        .insert_block(MoveTarget::LastChildOf(from_block.clone()), "new")
        .expect("insert under the block just read")
        .into_value();
    let edited = opened
        .update_block(&inserted, "new, edited")
        .expect("edit the block the insert answered with");
    assert_eq!(edited.value().text(), "new, edited");
    assert_ne!(
        edited.value().selector(),
        &inserted,
        "the edit reprojected the page, so even the block it edited answers a new id"
    );

    // The chain keeps working: the answer of one write addresses the next one, whatever value the
    // projection chose this time.
    let again = opened
        .update_block(edited.value().selector(), "new, edited twice")
        .expect("edit through the answer of the edit");
    assert_eq!(again.value().text(), "new, edited twice");
}

#[test]
fn two_handles_on_one_index_see_each_others_edits_as_missing_ids() {
    let graph = TempGraph::new("ephemeral-two-handles");
    graph.write("pages/alpha.md", ALPHA);
    graph.write("pages/beta.md", BETA);

    let mut writer = graph.open();
    let mut reader = graph.open();

    let alpha_id = BlockSelector::Ephemeral(ephemeral_at(&mut reader, "alpha", 0));
    let beta_id = BlockSelector::Ephemeral(ephemeral_at(&mut reader, "beta", 0));

    writer
        .update_block(&alpha_id, "first, edited")
        .expect("one handle edits the page");

    // The other handle's id for that page is gone: the page's rows were replaced by a commit it did
    // not make, and an id is only ever a row.
    match reader.update_block(&alpha_id, "through the other handle") {
        Err(WriteError::EphemeralNotFound { .. }) => {}
        other => panic!("expected the id to name nothing, got {other:?}"),
    }
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first, edited\n\t- child\n\t\t- grandchild\n\t- second child\n- last\n"
    );

    // The other page is untouched by that commit, so its id still works from this handle.
    reader
        .update_block(&beta_id, "beta one, edited")
        .expect("an id of an untouched page still names its block");
    assert_eq!(
        read_text(&graph, "pages/beta.md"),
        "- beta one, edited\n- beta two\n"
    );
}
