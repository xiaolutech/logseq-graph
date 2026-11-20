//! Writing block properties through the engine facade: upsert and remove.
//!
//! Every test writes a graph under the system temporary directory, opens it through [`Graph`], and
//! asserts on the bytes on disk. The bytes are the point of most of them: a property write touches
//! one `key:: value` line, so the written line's spelling, its place among the block's other
//! properties, and every line it must not touch are all pinned by the page's full text. The
//! refusals assert on both sides — the error, and the file left unchanged.

use std::fs;
use std::time::Duration;

use logseq_graph::document::EditError;
use logseq_graph::domain::{BlockId, BlockSelector};
use logseq_graph::engine::{Graph, WriteError};
use logseq_graph::source::WriteLock;

mod support;

use support::{selector_at, TempGraph};

/// The durable id the identified fixture block states.
const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

/// A page whose identified block already carries two properties in distinct spellings.
///
/// `ID::` is written uppercase on purpose: an upsert asked with `id` must keep the file's spelling,
/// so the assertion can tell replacement from re-rendering.
const ALPHA: &str = "\
- first
  ID:: 01234567-89ab-cdef-0123-456789abcdef
  another:: thing
  - child
  - second child
- last
";

/// A tab-indented page whose middle block carries one property.
const TABS: &str = "\
- outer
\t- middle
\t  note:: before
\t- sibling
- top
";

/// The fixture id as a caller would hold it.
fn id() -> BlockId {
    BlockId::parse(ID).expect("valid 8-4-4-4-12 UUID")
}

/// A second valid UUID, for an upsert that replaces the id's value without breaking the
/// projection: a durable row is looked up by the id the committed block states, so a replacement
/// that would leave a non-UUID value behind is a document-layer scenario, not an engine one.
const ID_TWO: &str = "99999999-9999-9999-9999-999999999999";

/// A durable selector for the fixture id.
fn durable() -> BlockSelector {
    BlockSelector::Durable(id())
}

/// Open the graph holding `ALPHA` on page `alpha`.
fn open(graph: &TempGraph) -> Graph {
    graph.write("pages/alpha.md", ALPHA);
    graph.open()
}

/// Open the graph holding `TABS` on page `tabs`.
fn open_tabs(graph: &TempGraph) -> Graph {
    graph.write("pages/tabs.md", TABS);
    graph.open()
}

/// Text of a graph file, as it is on disk.
fn read_text(graph: &TempGraph, relative: &str) -> String {
    fs::read_to_string(graph.graph_root().join(relative)).expect("read graph file")
}

/// The file is exactly `expected`, or the test names what changed.
fn assert_file(graph: &TempGraph, expected: &str) {
    assert_eq!(read_text(graph, "pages/alpha.md"), expected);
}

/// The write failed with `EditError::PropertyNotFound` and the file is unchanged.
fn assert_property_missing(error: WriteError, before: &str, graph: &TempGraph) {
    match error {
        WriteError::Edit(EditError::PropertyNotFound) => {}
        other => panic!("a missing key must be PropertyNotFound, got {other:?}"),
    }
    assert_file(graph, before);
}

/// The write failed with `EditError::InvalidPropertyKey` and the file is unchanged.
fn assert_invalid_key(error: WriteError, before: &str, graph: &TempGraph) {
    match error {
        WriteError::Edit(EditError::InvalidPropertyKey) => {}
        other => panic!("a bad key must be InvalidPropertyKey, got {other:?}"),
    }
    assert_file(graph, before);
}

#[test]
fn ensure_block_id_grants_logseq_identity_after_existing_properties_and_then_writes_nothing() {
    let fixture = TempGraph::new("ensure-block-id");
    let original = "- first\n  note:: before\n  color:: blue\n  - child\n- last\n";
    fixture.write("pages/alpha.md", original);
    let mut graph = fixture.open();
    let selector = selector_at(&mut graph, "alpha", 0);
    let before = graph.metadata().expect("metadata").value().revision();
    let result = graph.ensure_block_id(&selector).expect("grant identity");
    let answer = result.value();
    assert!(answer.created);
    assert_eq!(answer.block.durable_id(), Some(&answer.id));
    assert_eq!(result.meta().revision(), before + 1);
    let expected = format!(
        "- first\n  note:: before\n  color:: blue\n  id:: {}\n  - child\n- last\n",
        answer.id
    );
    assert_file(&fixture, &expected);
    let durable = answer.block.selector().clone();
    let revision = result.meta().revision();
    let id = answer.id.clone();
    let modified = fs::metadata(fixture.graph_root().join("pages/alpha.md"))
        .expect("stat")
        .modified()
        .expect("mtime");
    let result = graph.ensure_block_id(&durable).expect("already durable");
    assert!(!result.value().created);
    assert_eq!(result.value().id, id);
    assert_eq!(result.meta().revision(), revision);
    assert_file(&fixture, &expected);
    assert_eq!(
        fs::metadata(fixture.graph_root().join("pages/alpha.md"))
            .expect("stat")
            .modified()
            .expect("mtime"),
        modified
    );
}

#[test]
fn ensure_block_id_refuses_stale_and_expired_ephemeral_selectors_without_writing() {
    let fixture = TempGraph::new("ensure-stale");
    fixture.write("pages/alpha.md", "- first\n");
    let mut graph = fixture.open();
    let selector = selector_at(&mut graph, "alpha", 0);
    fixture.write("pages/alpha.md", "- neighbour\n- first\n");
    let before = graph.metadata().expect("metadata").value().revision();
    assert!(matches!(
        graph.ensure_block_id(&selector),
        Err(WriteError::StaleVersion { .. })
    ));
    assert_file(&fixture, "- neighbour\n- first\n");
    assert_eq!(
        graph.metadata().expect("metadata").value().revision(),
        before
    );
    graph
        .refresh_paths(&[std::path::PathBuf::from("pages/alpha.md")])
        .expect("refresh");
    assert!(matches!(
        graph.ensure_block_id(&selector),
        Err(WriteError::EphemeralNotFound { .. })
    ));
    assert_file(&fixture, "- neighbour\n- first\n");
}

#[test]
fn ensure_existing_id_observes_the_file_without_rewriting_or_returning_stale_text() {
    let fixture = TempGraph::new("ensure-existing");
    let mut graph = open(&fixture);
    let updated = ALPHA.replace("- first", "- changed");
    fixture.write("pages/alpha.md", &updated);
    let before = graph.metadata().expect("metadata").value().revision();
    let result = graph.ensure_block_id(&durable()).expect("existing id");
    assert!(!result.value().created);
    assert_eq!(result.value().id, id());
    assert!(result.value().block.text().starts_with("changed\nID::"));
    assert_eq!(result.meta().revision(), before);
    assert_file(&fixture, &updated);
}

#[test]
fn ensure_nested_block_without_properties_inserts_after_its_marker_and_keeps_its_view_on_repeat() {
    let fixture = TempGraph::new("ensure-nested");
    fixture.write(
        "pages/alpha.md",
        "- outer\n  - child\n- second\n  - target\n    - grandchild\n",
    );
    let mut graph = fixture.open();
    let selector = selector_at(&mut graph, "alpha", 3);
    let created = graph
        .ensure_block_id(&selector)
        .expect("grant nested identity");
    let answer = created.value();
    assert!(answer.created);
    assert_eq!(answer.block.depth(), 1);
    assert_eq!(answer.block.parent_preorder(), Some(2));
    assert_file(
        &fixture,
        &format!(
            "- outer\n  - child\n- second\n  - target\n    id:: {}\n    - grandchild\n",
            answer.id
        ),
    );
    let repeated = graph
        .ensure_block_id(answer.block.selector())
        .expect("existing nested id");
    assert_eq!(repeated.value().block, answer.block);
    assert!(!repeated.value().created);
    assert_eq!(repeated.meta().revision(), created.meta().revision());
}

/// The write failed with `EditError::MultilineContent` and the file is unchanged.
fn assert_multiline(error: WriteError, before: &str, graph: &TempGraph) {
    match error {
        WriteError::Edit(EditError::MultilineContent) => {}
        other => panic!("a multiline value must be MultilineContent, got {other:?}"),
    }
    assert_file(graph, before);
}

#[test]
fn upsert_appends_after_the_last_property_line() {
    let graph = TempGraph::new("property-append");
    let mut opened = open(&graph);

    opened
        .upsert_block_property(&durable(), "status", "done")
        .expect("upsert a new key");

    assert_file(
        &graph,
        "\
- first
  ID:: 01234567-89ab-cdef-0123-456789abcdef
  another:: thing
  status:: done
  - child
  - second child
- last
",
    );
}

#[test]
fn upsert_replaces_the_value_and_keeps_the_written_key() {
    let graph = TempGraph::new("property-replace");
    let mut opened = open(&graph);

    opened
        .upsert_block_property(&durable(), "id", ID_TWO)
        .expect("upsert the stated key");

    // The key keeps the spelling the file wrote (`ID::`, not `id::`); only the value changed.
    assert_file(
        &graph,
        "\
- first
  ID:: 99999999-9999-9999-9999-999999999999
  another:: thing
  - child
  - second child
- last
",
    );
}

#[test]
fn upsert_leaves_the_other_property_lines_byte_for_byte() {
    let graph = TempGraph::new("property-others");
    let mut opened = open(&graph);

    opened
        .upsert_block_property(&durable(), "ANOTHER", "changed")
        .expect("upsert the stated key");

    assert_file(
        &graph,
        "\
- first
  ID:: 01234567-89ab-cdef-0123-456789abcdef
  another:: changed
  - child
  - second child
- last
",
    );
}

#[test]
fn remove_deletes_the_line_and_keeps_the_other_properties() {
    let graph = TempGraph::new("property-remove");
    let mut opened = open(&graph);

    opened
        .remove_block_property(&durable(), "another")
        .expect("remove the stated key");

    assert_file(
        &graph,
        "\
- first
  ID:: 01234567-89ab-cdef-0123-456789abcdef
  - child
  - second child
- last
",
    );
}

#[test]
fn remove_of_a_missing_key_refuses_and_touches_nothing() {
    let graph = TempGraph::new("property-remove-miss");
    let mut opened = open(&graph);

    let before = read_text(&graph, "pages/alpha.md");
    let error = opened
        .remove_block_property(&durable(), "status")
        .expect_err("no such key");

    assert_property_missing(error, &before, &graph);
    assert_file(&graph, ALPHA);
}

#[test]
fn an_appended_property_line_follows_the_block_content_column() {
    let graph = TempGraph::new("property-tabs");
    let mut opened = open_tabs(&graph);

    // `middle` is the first child of the root at preorder 0, so it is preorder 1.
    let selector = selector_at(&mut opened, "tabs", 1);
    opened
        .upsert_block_property(&selector, "status", "done")
        .expect("upsert a new key");

    // The rule is `update_block_text`'s: the new line aligns to the block's content column counted
    // in characters — `\t- middle` starts its content at character 3 — and the unit is the
    // document's, so the line gets three tab characters. The block's hand-written property line
    // keeps its own bytes.
    assert_eq!(
        read_text(&graph, "pages/tabs.md"),
        "\
- outer
\t- middle
\t  note:: before
\t\t\tstatus:: done
\t- sibling
- top
"
    );
}

#[test]
fn a_value_with_a_line_break_is_refused() {
    let graph = TempGraph::new("property-multiline");
    let mut opened = open(&graph);

    let before = read_text(&graph, "pages/alpha.md");
    let error = opened
        .upsert_block_property(&durable(), "status", "done\nlater")
        .expect_err("a property line is one source line");

    assert_multiline(error, &before, &graph);
}

#[test]
fn a_key_no_property_line_can_carry_is_refused() {
    let graph = TempGraph::new("property-bad-key");
    let mut opened = open(&graph);

    let before = read_text(&graph, "pages/alpha.md");
    for key in ["", "a:: b", " a", "a\nb"] {
        let error = opened
            .upsert_block_property(&durable(), key, "v")
            .expect_err("the key is not writable as a property");
        assert_invalid_key(error, &before, &graph);

        let error = opened
            .remove_block_property(&durable(), key)
            .expect_err("the key is not writable as a property");
        assert_invalid_key(error, &before, &graph);
    }

    assert_file(&graph, ALPHA);
}

#[test]
fn an_ephemeral_handle_writes_the_block_it_names_and_no_other() {
    let graph = TempGraph::new("property-ephemeral");
    // No block states an `id::`: both selectors here are handles.
    graph.write("pages/alpha.md", "- first\n- second\n");
    let mut opened = graph.open();

    let selector = selector_at(&mut opened, "alpha", 0);
    opened
        .upsert_block_property(&selector, "status", "done")
        .expect("upsert a new key by handle");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n  status:: done\n- second\n"
    );
}

#[test]
fn a_nested_upsert_leaves_the_parent_and_siblings_untouched() {
    let graph = TempGraph::new("property-nested");
    let mut opened = open_tabs(&graph);

    // `middle` is the first child of the root at preorder 0, so it is preorder 1.
    let selector = selector_at(&mut opened, "tabs", 1);
    opened
        .upsert_block_property(&selector, "note", "after")
        .expect("replace the middle block's own property");

    // The parent, the sibling, and the top-level block keep their bytes; only the middle block's
    // own line changed.
    assert_eq!(
        read_text(&graph, "pages/tabs.md"),
        "\
- outer
\t- middle
\t  note:: after
\t- sibling
- top
"
    );
}

#[test]
fn upserting_id_makes_the_block_readable_by_durable_selector() {
    let graph = TempGraph::new("property-mint-id");
    graph.write("pages/alpha.md", "- first\n- second\n");
    let mut opened = graph.open();
    let selector = selector_at(&mut opened, "alpha", 0);
    let view = opened
        .upsert_block_property(&selector, "id", ID)
        .expect("assign a durable id")
        .into_value();

    // The write added an `id::` line, so the row this same write committed states durable identity:
    // a block has exactly one address, and the projection is what decides which (ADR-0012). The
    // ephemeral id the block had before the write is gone, which is why the answer is the new row
    // rather than the selector the call was asked with.
    assert_eq!(
        view.identity(),
        logseq_graph::domain::BlockIdentityKind::Durable
    );
    assert_eq!(
        view.durable_id().map(BlockId::as_str),
        Some(ID),
        "the committed block now states the id"
    );
    assert_eq!(view.ephemeral_id(), None);
    assert_eq!(view.selector(), &durable());
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n- second\n"
    );

    // ADR-0009: a durable id is created only by an explicit write, and once written it is the
    // block's address for every later read.
    let found = opened
        .block(&durable())
        .expect("read the block")
        .present()
        .expect("the block is found by the id it was just given")
        .clone();
    assert_eq!(
        found.text(),
        "first\nid:: 01234567-89ab-cdef-0123-456789abcdef"
    );
}

#[test]
fn folding_the_pages_only_block_writes_the_property_and_reads_back_folded() {
    // The page's only block is also its last line, so the property line has no line after it to
    // land before. That shape used to index past the document's lines and panic.
    let graph = TempGraph::new("property-fold-only");
    graph.write("pages/only.md", "- a\n");
    let mut opened = graph.open();

    let selector = selector_at(&mut opened, "only", 0);
    opened
        .set_block_collapsed(&selector, true)
        .expect("the page's only block can be folded");

    assert_eq!(
        read_text(&graph, "pages/only.md"),
        "- a\n  collapsed:: true\n"
    );
    let snapshot = opened
        .page_snapshot("only")
        .expect("read the page snapshot")
        .into_value()
        .expect("the page exists");
    assert!(
        snapshot.blocks()[0].collapsed(),
        "the fold is one read back"
    );
    assert_eq!(snapshot.blocks()[0].text(), "a");
}

#[test]
fn a_held_lock_refuses_a_property_write_without_touching_the_file() {
    // The property writes share `write_block_property`, which checks an ephemeral selector's page
    // fingerprint before it edits. That check means nothing unless the bytes cannot change between
    // the check and the write, so these writes take the graph lock like every other write; this is
    // the test that says so, in the shape `write_path.rs` uses for the append.
    let graph = TempGraph::new("property-locked");
    graph.write("pages/alpha.md", "- first\n- second\n");
    // Zero wait, so the refusal is observed without waiting out the default budget.
    let mut opened =
        Graph::open(graph.options().with_write_lock_wait(Duration::ZERO)).expect("open the graph");
    let before = read_text(&graph, "pages/alpha.md");
    let selector = selector_at(&mut opened, "alpha", 0);

    // The graph root is where the engine takes the lock; holding it here is what another writer
    // would look like.
    let held = WriteLock::acquire(&graph.graph_root(), None).expect("hold the lock");
    let failure = opened
        .upsert_block_property(&selector, "status", "done")
        .expect_err("a held lock must refuse the property write");
    drop(held);

    assert!(
        matches!(&failure, WriteError::Lock(_)),
        "expected a lock error, got {failure:?}"
    );
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        before,
        "a refused write must not touch the file"
    );

    // The lock is the only thing that refused it: the same call lands as soon as the lock is free.
    opened
        .upsert_block_property(&selector, "status", "done")
        .expect("the property write goes through once the lock is released");
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n  status:: done\n- second\n"
    );
}
