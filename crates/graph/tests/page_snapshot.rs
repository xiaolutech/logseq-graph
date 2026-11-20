//! Whole-page snapshots: one read that answers a page's structure, its blocks' editable text, its
//! properties, and a version token for the bytes they came from.
//!
//! Every test writes a graph file, opens a [`Graph`], and asks through
//! [`Graph::page_snapshot`], so the whole path from discovery through the file read to the returned
//! snapshot is covered. Each behaviour is asserted next to the case that would look right while
//! being wrong: a position that agrees with the sibling index by coincidence, a property line that
//! survives into the editable text, a version that moves when another page is written.

use logseq_graph::document::{BlockLocator, PageDocument};
use logseq_graph::domain::{BlockMarker, PageSnapshot, SnapshotBlock};
use logseq_graph::engine::Graph;

mod support;

use support::{bound, TempGraph};

/// A page whose blocks exercise several levels, as ADR-0005's example spells it out.
const NESTED: &str = "- a\n\t- b\n\t\t- c\n\t- d\n- e\n";

/// A UUID that parses as a durable block id.
const UUID: &str = "61745deb-eeed-4426-8020-eb9e5c90d558";

/// Opens a graph holding `source` as the single page `page`.
fn open(page: &str, source: &str) -> (TempGraph, Graph) {
    let graph = TempGraph::new("page-snapshot");
    graph.write(&format!("pages/{page}.md"), source);
    let opened = graph.open();
    (graph, opened)
}

/// The snapshot of `page`, which must exist.
fn snapshot_of(graph: &mut Graph, page: &str) -> PageSnapshot {
    graph
        .page_snapshot(page)
        .expect("read the page snapshot")
        .into_value()
        .expect("the page exists")
}

/// Every block of a snapshot, parents before their children.
fn preorder(blocks: &[SnapshotBlock]) -> Vec<&SnapshotBlock> {
    let mut ordered = Vec::new();
    let mut stack: Vec<&SnapshotBlock> = blocks.iter().rev().collect();
    while let Some(block) = stack.pop() {
        ordered.push(block);
        stack.extend(block.children().iter().rev());
    }
    ordered
}

/// The text of every block of a snapshot, in preorder.
fn texts(snapshot: &PageSnapshot) -> Vec<&str> {
    preorder(snapshot.blocks())
        .into_iter()
        .map(SnapshotBlock::text)
        .collect()
}

/// The keys of every property of a snapshot's block, in written order.
fn block_property_keys(block: &SnapshotBlock) -> Vec<&str> {
    block
        .properties()
        .iter()
        .map(|property| property.key())
        .collect()
}

/// The child-index path of every block of a snapshot, in preorder.
fn child_paths(snapshot: &PageSnapshot) -> Vec<Vec<u32>> {
    preorder(snapshot.blocks())
        .into_iter()
        .map(|block| block.child_path().to_vec())
        .collect()
}

#[test]
fn a_child_path_counts_from_the_page_root_not_from_a_preorder() {
    let (_graph, mut opened) = open("nested", NESTED);
    let snapshot = snapshot_of(&mut opened, "nested");

    // The preorder and the child-index path of every block. On `e` the two numbers disagree: it is
    // the second top-level block, so its child-path is `[1]`, while its preorder is 4 because the
    // top-level preorders count the blocks nested under `a` first. A snapshot that reported a
    // preorder first element would answer `[4]` here and name no block of any page.
    let actual: Vec<(u32, Vec<u32>)> = preorder(snapshot.blocks())
        .into_iter()
        .map(|block| (block.preorder(), block.child_path().to_vec()))
        .collect();
    assert_eq!(
        actual,
        vec![
            (0, vec![0]),
            (1, vec![0, 0]),
            (2, vec![0, 0, 0]),
            (3, vec![0, 1]),
            (4, vec![1]),
        ]
    );

    // The same positions, stated as the nesting the page actually has.
    assert_eq!(snapshot.blocks().len(), 2, "two top-level blocks");
    assert_eq!(
        snapshot.blocks()[0].children().len(),
        2,
        "`a` holds `b` and `d`"
    );
    assert_eq!(snapshot.blocks()[0].children()[0].children().len(), 1);
    assert_eq!(texts(&snapshot), vec!["a", "b", "c", "d", "e"]);
}

#[test]
fn an_empty_page_answers_some_with_no_blocks_and_a_version() {
    let (_graph, mut opened) = open("empty", "");
    let snapshot = snapshot_of(&mut opened, "empty");

    assert!(snapshot.blocks().is_empty(), "no block to report");
    assert!(snapshot.properties().is_empty());
    assert_eq!(snapshot.page(), "empty");
    assert!(
        !snapshot.version().as_str().is_empty(),
        "an empty file still has one version of its bytes"
    );
}

#[test]
fn a_missing_page_answers_none_without_creating_it() {
    let (graph, mut opened) = open("present", "- one\n");

    assert!(
        opened
            .page_snapshot("absent")
            .expect("read a page that does not exist")
            .into_value()
            .is_none(),
        "a name with no page row names no page"
    );
    assert!(opened
        .page_snapshot("present")
        .expect("read a page that exists")
        .into_value()
        .is_some());
    assert!(
        !graph.graph_root().join("pages/absent.md").exists(),
        "the read must not create the file it could not find"
    );
}

#[test]
fn a_row_whose_file_disappeared_answers_none_instead_of_failing() {
    let (graph, mut opened) = open("gone", "- one\n");
    std::fs::remove_file(graph.graph_root().join("pages/gone.md")).expect("remove the page file");

    let read = opened
        .page_snapshot("gone")
        .expect("a row whose file is gone is a stale row, not an error");
    assert!(
        read.into_value().is_none(),
        "no page lives there any more, so there is no page to answer with"
    );
}

#[test]
fn a_file_no_scan_has_projected_is_not_a_page_yet() {
    let (graph, mut opened) = open("present", "- one\n");
    graph.write("pages/late.md", "- late\n");

    assert!(
        opened
            .page_snapshot("late")
            .expect("read a file no scan has seen")
            .into_value()
            .is_none(),
        "the default policy does not scan, so the index has no row to locate this page by"
    );
}

#[test]
fn a_row_that_no_longer_decodes_to_the_requested_name_answers_none() {
    // The default title format names this journal `Aug 13th, 2026`; the index row is projected
    // under that name. A later configuration change makes the same file denote `2026-08-13`, and a
    // handle opened over the un-re-projected index must not hand back the journal's content under
    // the old name.
    let graph = TempGraph::new("renamed");
    graph.write("journals/2026_08_13.md", "- journal\n");
    {
        let mut before = graph.open();
        assert!(before
            .page_snapshot("Aug 13th, 2026")
            .expect("read the journal")
            .into_value()
            .is_some());
    }

    graph.write_config(r#"{:journal/page-title-format "yyyy-MM-dd"}"#);
    let mut after =
        Graph::open(graph.options().with_startup_scan(false)).expect("open without scanning");

    assert!(
        after
            .page_snapshot("Aug 13th, 2026")
            .expect("read the renamed page")
            .into_value()
            .is_none(),
        "the row's path now decodes to another page, so the row is stale for this name"
    );
}

#[test]
fn the_version_token_is_sixteen_lowercase_hex_digits() {
    let (_graph, mut opened) = open("token", "- one\n");
    let snapshot = snapshot_of(&mut opened, "token");

    let token = snapshot.version().as_str();
    assert_eq!(token.len(), 16, "{token:?}");
    assert!(
        token
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "{token:?} is not lowercase hexadecimal"
    );
    assert_eq!(
        token,
        snapshot.version().to_string(),
        "Display is the token itself"
    );
}

#[test]
fn a_durable_id_is_reported_only_when_its_uuid_parses() {
    let (_graph, mut opened) = open(
        "ids",
        &format!("- no id\n- id:: {UUID}\n- id:: not-a-uuid\n- id:: 61745deb\n"),
    );
    let snapshot = snapshot_of(&mut opened, "ids");
    let blocks = preorder(snapshot.blocks());

    assert!(blocks[0].durable_id().is_none(), "no `id::` line");
    assert_eq!(
        blocks[1].durable_id().map(|id| id.as_str()),
        Some(UUID),
        "a well-formed `id::` is the block's durable identity"
    );
    assert!(blocks[2].durable_id().is_none(), "not a UUID");
    assert!(blocks[3].durable_id().is_none(), "a truncated UUID");
}

#[test]
fn markers_keep_the_form_the_source_wrote() {
    let (_graph, mut opened) = open(
        "markers",
        "# one\n## two\n###### six\n* star\n+ plus\n- dash\n",
    );
    let snapshot = snapshot_of(&mut opened, "markers");

    let actual: Vec<(BlockMarker, &str)> = preorder(snapshot.blocks())
        .into_iter()
        .map(|block| (block.marker(), block.text()))
        .collect();
    assert_eq!(
        actual,
        vec![
            (BlockMarker::Heading { level: 1 }, "one"),
            (BlockMarker::Heading { level: 2 }, "two"),
            (BlockMarker::Heading { level: 6 }, "six\n* star\n+ plus"),
            (BlockMarker::Bullet, "dash"),
        ],
        "only `-` is a bullet: `*` and `+` stay continuation text of the block above them"
    );
}

#[test]
fn a_folded_block_reads_back_from_the_property_the_write_states() {
    let (graph, mut opened) = open("fold", NESTED);
    let selector = bound(&graph, "fold", &[0]);
    opened
        .set_block_collapsed(&selector, true)
        .expect("fold the first block");

    let snapshot = snapshot_of(&mut opened, "fold");
    assert!(
        snapshot.blocks()[0].collapsed(),
        "the value `set_block_collapsed` writes is the one that means folded"
    );
    assert_eq!(
        block_property_keys(&snapshot.blocks()[0]),
        vec!["collapsed"],
        "the write states the property, and the read reports it beside the text"
    );
    assert_eq!(texts(&snapshot), vec!["a", "b", "c", "d", "e"]);

    // Unfolding replaces the line; the property stays and now reads as unfolded.
    let selector = bound(&graph, "fold", &[0]);
    opened
        .set_block_collapsed(&selector, false)
        .expect("unfold the first block");
    let snapshot = snapshot_of(&mut opened, "fold");
    assert!(!snapshot.blocks()[0].collapsed());
    assert_eq!(
        snapshot.blocks()[0].properties()[0].value(),
        "false",
        "the line is still there, holding the value the unfold wrote"
    );
    assert!(
        std::fs::read_to_string(graph.graph_root().join("pages/fold.md"))
            .expect("read the page file")
            .contains("collapsed:: false"),
        "the read agrees with the bytes on disk"
    );
}

#[test]
fn a_hand_written_folded_property_is_reported_and_kept_out_of_the_text() {
    let (_graph, mut opened) = open(
        "hand",
        "- folded\n  collapsed:: true\n- unfolded\n- wrong\n  collapsed:: yes\n",
    );
    let snapshot = snapshot_of(&mut opened, "hand");
    let blocks = preorder(snapshot.blocks());

    assert!(blocks[0].collapsed());
    assert!(!blocks[1].collapsed(), "no property means unfolded");
    assert!(
        !blocks[2].collapsed(),
        "only the literal `true` folds a block, so `yes` does not"
    );
    assert_eq!(
        texts(&snapshot),
        vec!["folded", "unfolded", "wrong"],
        "the property line is not part of the editable text"
    );
}

#[test]
fn properties_keep_their_order_and_stay_out_of_the_editable_text() {
    let (_graph, mut opened) = open(
        "props",
        &format!(
            "title:: Alpha\nsource_url:: a:: b\n\n- body\n  id:: {UUID}\n  collapsed:: true\n  custom:: value\n"
        ),
    );
    let snapshot = snapshot_of(&mut opened, "Alpha");

    let page_keys: Vec<&str> = snapshot
        .properties()
        .iter()
        .map(|property| property.key())
        .collect();
    assert_eq!(page_keys, vec!["title", "source_url"]);
    assert_eq!(snapshot.properties()[1].value(), "a:: b");

    let block = &snapshot.blocks()[0];
    assert_eq!(block.text(), "body", "no property line reaches the text");
    assert_eq!(
        block_property_keys(block),
        vec!["id", "collapsed", "custom"],
        "block properties come back in written order"
    );
    assert_eq!(block.properties()[2].value(), "value");
}

#[test]
fn unicode_links_and_code_survive_without_normalization() {
    let source = format!(
        "- emoji 🎉 与 e\u{301}\n\t- 行内 `code` 与 `[[隐藏]]`\n\t- [[引用]] 与 (({UUID}))\n\t- ```\n\t  - 伪块\n\t  ```\n"
    );
    let (_graph, mut opened) = open("unicode", &source);
    let snapshot = snapshot_of(&mut opened, "unicode");

    let linked = format!("[[引用]] 与 (({UUID}))");
    assert_eq!(
        texts(&snapshot),
        vec![
            "emoji 🎉 与 e\u{301}",
            "行内 `code` 与 `[[隐藏]]`",
            linked.as_str(),
            "```\n- 伪块\n```",
        ],
        "text is the source bytes of the block's own lines, not a rendering of them"
    );
    assert_eq!(
        preorder(snapshot.blocks()).len(),
        4,
        "a marker line inside a code fence is text, not a block"
    );
    assert_eq!(
        child_paths(&snapshot)[3],
        vec![0, 2],
        "the fence is the third child of the first block, not a block of its own"
    );
}

#[test]
fn a_byte_order_mark_and_carriage_returns_do_not_reach_the_text() {
    let (_graph, mut opened) = open("crlf", "\u{feff}- a\r\n\t- b\r\n- c");
    let snapshot = snapshot_of(&mut opened, "crlf");

    assert_eq!(texts(&snapshot), vec!["a", "b", "c"]);
    assert_eq!(
        snapshot.blocks().len(),
        2,
        "the structure is the same as with `\\n`"
    );
    assert_eq!(snapshot.blocks()[0].children().len(), 1);
    for text in texts(&snapshot) {
        assert!(
            !text.contains('\r'),
            "{text:?} must not carry a line ending"
        );
        assert!(
            !text.contains('\u{feff}'),
            "{text:?} must not carry the BOM"
        );
    }
}

#[test]
fn a_version_is_one_observation_of_this_page_bytes() {
    let (graph, mut opened) = open("version", "- first\n");
    graph.write("pages/other.md", "- elsewhere\n");

    let first = snapshot_of(&mut opened, "version").version().clone();
    let again = snapshot_of(&mut opened, "version").version().clone();
    assert_eq!(first, again, "no write means no new version");

    // Writing another page commits a revision but changes nothing about this page's bytes.
    opened
        .append_block("other", "one more")
        .expect("append to another page");
    assert_eq!(
        snapshot_of(&mut opened, "version").version(),
        &first,
        "the token is page-level"
    );

    // Writing this page changes it, and the snapshot answers the new content.
    opened
        .append_block("version", "second")
        .expect("append to this page");
    let after = snapshot_of(&mut opened, "version");
    assert_ne!(
        after.version(),
        &first,
        "a committed edit to the page produces a new version"
    );
    assert_eq!(texts(&after), vec!["first", "second"]);
}

#[test]
fn a_snapshot_agrees_with_the_page_tree_block_for_block() {
    let source = format!("# head\n\t- child\n\t\t- grandchild\n* star\n- id:: {UUID}\n\t- under\n");
    let (_graph, mut opened) = open("cross", &source);
    let snapshot = snapshot_of(&mut opened, "cross");
    let tree = opened
        .page_tree("cross")
        .expect("read the page tree")
        .into_value()
        .expect("the page exists");

    let mut flat: Vec<(u32, BlockMarker, usize)> = Vec::new();
    let mut stack: Vec<&logseq_graph::domain::BlockNode> = tree.blocks().iter().rev().collect();
    while let Some(node) = stack.pop() {
        flat.push((node.preorder(), node.marker(), node.children().len()));
        stack.extend(node.children().iter().rev());
    }
    let from_snapshot: Vec<(u32, BlockMarker, usize)> = preorder(snapshot.blocks())
        .into_iter()
        .map(|block| (block.preorder(), block.marker(), block.children().len()))
        .collect();

    assert_eq!(
        from_snapshot, flat,
        "position, marker, and shape must agree"
    );
}

#[test]
fn the_editable_text_is_what_an_edit_round_trips() {
    // `update_block_text` preserves a block's `id::` and `collapsed::` lines and drops any other
    // property line, so the round trip is stated on the properties an edit keeps. That a custom
    // property never reaches the text is asserted where a page states one.
    let source = format!("- 正文\n  id:: {UUID}\n  collapsed:: true\n");
    let (_graph, mut opened) = open("edit", &source);
    let snapshot = snapshot_of(&mut opened, "edit");

    let text = snapshot.blocks()[0].text().to_owned();
    assert_eq!(text, "正文");
    let document = PageDocument::parse(&source);
    let updated = document
        .update_block_text(BlockLocator::Id(UUID), &text)
        .expect("an editable text is an edit this page accepts");
    assert_eq!(
        updated, source,
        "writing the snapshot's text back leaves every property line in place"
    );
}

#[test]
fn reading_a_snapshot_does_not_advance_the_revision() {
    let (_graph, mut opened) = open("quiet", "- one\n");
    let before = opened.metadata().expect("read metadata").into_value();

    snapshot_of(&mut opened, "quiet");

    let after = opened.metadata().expect("read metadata").into_value();
    assert_eq!(
        before.revision(),
        after.revision(),
        "a read writes nothing and commits nothing"
    );
}
