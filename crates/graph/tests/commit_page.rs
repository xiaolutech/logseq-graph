//! Versioned batch commits: one page, one base version, an ordered list of positional operations,
//! applied in order and landed as one file replacement (ADR-0011).
//!
//! Every test writes a graph under the system temporary directory, opens it through [`Graph`], and
//! asserts on the bytes on disk and on the snapshot the commit answers. The behaviours are the ones
//! the contract is made of: an operation sees the document the previous ones left, the whole batch
//! costs one file replacement and one revision, a stale base version writes nothing, a refused
//! operation writes nothing at all, a path that names nothing is refused rather than redirected, and
//! the bytes outside the edited lines are copied unchanged.

use logseq_graph::document::{EditError, PageDocument};
use logseq_graph::domain::{
    BatchMoveTarget, BlockMarker, PageCommit, PageCommitResult, PageOperation, PageSnapshot,
    PageVersion, SnapshotBlock,
};
use logseq_graph::engine::{Graph, WriteError};

mod support;

use support::TempGraph;

/// A page with two levels, two children, and no property line: the shape every single-operation
/// test commits against.
const NESTED: &str = "\
- alpha
  - beta
  - gamma
- delta
";

/// The page of the "one commit writes once" tests: four top-level blocks, so five operations fit
/// without any of them changing another's path.
const FOUR: &str = "\
- a
- b
- c
- d
";

/// Opens a graph holding `source` as the page `page`.
fn open(page: &str, source: &str) -> (TempGraph, Graph) {
    let graph = TempGraph::new("commit-page");
    graph.write(&format!("pages/{page}.md"), source);
    let opened = graph.open();
    (graph, opened)
}

/// Text of a graph file, as it is on disk.
fn read_text(graph: &TempGraph, relative: &str) -> String {
    std::fs::read_to_string(graph.graph_root().join(relative)).expect("read graph file")
}

/// The page's snapshot, which must exist.
fn snapshot_of(graph: &mut Graph, page: &str) -> PageSnapshot {
    graph
        .page_snapshot(page)
        .expect("read the page snapshot")
        .into_value()
        .expect("the page exists")
}

/// The version of the page's bytes now, which is the base version a fresh commit has to state.
fn base_version(graph: &mut Graph, page: &str) -> PageVersion {
    snapshot_of(graph, page).version().clone()
}

/// Commit `operations` against `page` at the version its bytes have now.
fn commit(
    graph: &mut Graph,
    page: &str,
    operations: Vec<PageOperation>,
) -> Result<logseq_graph::domain::Read<PageCommitResult>, WriteError> {
    let base = base_version(graph, page);
    graph.commit_page(&PageCommit::new(page, base, operations))
}

/// The page's revision as the index records it, or `None` when it has no row.
fn revision(graph: &mut Graph, page: &str) -> Option<u64> {
    graph
        .page_metadata(page)
        .expect("read page metadata")
        .into_value()
        .map(|metadata| metadata.revision())
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

/// The child index path of every block of a snapshot, in preorder.
fn child_paths(snapshot: &PageSnapshot) -> Vec<Vec<u32>> {
    preorder(snapshot.blocks())
        .into_iter()
        .map(|block| block.child_path().to_vec())
        .collect()
}

/// A single operation against `NESTED`, its expected file bytes, and its expected block texts.
///
/// One case per [`PageOperation`] variant, so a variant that loses its mapping to a document edit
/// fails here rather than in whichever feature noticed first.
fn single_cases() -> Vec<(PageOperation, &'static str, Vec<&'static str>)> {
    vec![
        (
            PageOperation::EditText {
                block: vec![0, 1],
                text: "GAMMA".to_owned(),
            },
            "- alpha\n  - beta\n  - GAMMA\n- delta\n",
            vec!["alpha", "beta", "GAMMA", "delta"],
        ),
        (
            PageOperation::SplitBlock {
                block: vec![1],
                offset: 2,
            },
            "- alpha\n  - beta\n  - gamma\n- de\n- lta\n",
            vec!["alpha", "beta", "gamma", "de", "lta"],
        ),
        (
            PageOperation::MergeBlock { block: vec![1] },
            "- alphadelta\n  - beta\n  - gamma\n",
            vec!["alphadelta", "beta", "gamma"],
        ),
        (
            PageOperation::UnwrapBlock { block: vec![0] },
            "- beta\n- gamma\n- delta\n",
            vec!["beta", "gamma", "delta"],
        ),
        (
            PageOperation::IndentBlock { block: vec![0, 1] },
            "- alpha\n  - beta\n    - gamma\n- delta\n",
            vec!["alpha", "beta", "gamma", "delta"],
        ),
        (
            PageOperation::OutdentBlock { block: vec![0, 1] },
            "- alpha\n  - beta\n- gamma\n- delta\n",
            vec!["alpha", "beta", "gamma", "delta"],
        ),
        (
            PageOperation::InsertBlock {
                after: Some(vec![0, 0]),
                text: "new".to_owned(),
            },
            "- alpha\n  - beta\n  - new\n  - gamma\n- delta\n",
            vec!["alpha", "beta", "new", "gamma", "delta"],
        ),
        (
            PageOperation::InsertBlock {
                after: None,
                text: "last".to_owned(),
            },
            "- alpha\n  - beta\n  - gamma\n- delta\n- last\n",
            vec!["alpha", "beta", "gamma", "delta", "last"],
        ),
        (
            PageOperation::MoveBlock {
                block: vec![1],
                target: BatchMoveTarget::Before { anchor: vec![0] },
            },
            "- delta\n- alpha\n  - beta\n  - gamma\n",
            vec!["delta", "alpha", "beta", "gamma"],
        ),
        (
            PageOperation::MoveBlock {
                block: vec![1],
                target: BatchMoveTarget::After { anchor: vec![0, 0] },
            },
            "- alpha\n  - beta\n  - delta\n  - gamma\n",
            vec!["alpha", "beta", "delta", "gamma"],
        ),
        (
            PageOperation::MoveBlock {
                block: vec![1],
                target: BatchMoveTarget::FirstChildOf { anchor: vec![0] },
            },
            "- alpha\n  - delta\n  - beta\n  - gamma\n",
            vec!["alpha", "delta", "beta", "gamma"],
        ),
        (
            PageOperation::MoveBlock {
                block: vec![1],
                target: BatchMoveTarget::LastChildOf { anchor: vec![0, 0] },
            },
            "- alpha\n  - beta\n    - delta\n  - gamma\n",
            vec!["alpha", "beta", "delta", "gamma"],
        ),
        (
            PageOperation::SetCollapsed {
                block: vec![0, 0],
                collapsed: true,
            },
            "- alpha\n  - beta\n    collapsed:: true\n  - gamma\n- delta\n",
            vec!["alpha", "beta", "gamma", "delta"],
        ),
        (
            PageOperation::SetCollapsed {
                block: vec![1],
                collapsed: false,
            },
            "- alpha\n  - beta\n  - gamma\n- delta\n  collapsed:: false\n",
            vec!["alpha", "beta", "gamma", "delta"],
        ),
        (
            PageOperation::SetMarker {
                block: vec![1],
                marker: BlockMarker::Heading { level: 3 },
            },
            "- alpha\n  - beta\n  - gamma\n### delta\n",
            vec!["alpha", "beta", "gamma", "delta"],
        ),
    ]
}

#[test]
fn every_operation_variant_is_one_document_edit() {
    for (operation, expected_bytes, expected_texts) in single_cases() {
        let (graph, mut opened) = open("alpha", NESTED);
        let name = format!("{operation:?}");

        let before = revision(&mut opened, "alpha").expect("the page is projected");
        let result = commit(&mut opened, "alpha", vec![operation.clone()])
            .unwrap_or_else(|error| panic!("{name} must commit: {error}"));

        assert_eq!(
            read_text(&graph, "pages/alpha.md"),
            expected_bytes,
            "{name} on disk"
        );
        assert_eq!(texts(result.value().snapshot()), expected_texts, "{name}");
        assert_eq!(result.value().applied(), 1, "{name}");
        assert_eq!(
            result.meta().revision(),
            before + 1,
            "{name} advances one revision"
        );
    }
}

#[test]
fn an_edit_after_an_insert_addresses_the_new_block() {
    let (graph, mut opened) = open("alpha", NESTED);

    let result = commit(
        &mut opened,
        "alpha",
        vec![
            PageOperation::InsertBlock {
                after: Some(vec![0]),
                text: "new".to_owned(),
            },
            PageOperation::EditText {
                block: vec![1],
                text: "NEW".to_owned(),
            },
        ],
    )
    .expect("the second operation sees the block the first one minted");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- alpha\n  - beta\n  - gamma\n- NEW\n- delta\n"
    );
    assert_eq!(result.value().applied(), 2);
    assert_eq!(
        texts(result.value().snapshot()),
        vec!["alpha", "beta", "gamma", "NEW", "delta"]
    );
}

#[test]
fn an_indent_after_an_insert_holds_the_new_block() {
    let (graph, mut opened) = open("alpha", NESTED);

    commit(
        &mut opened,
        "alpha",
        vec![
            PageOperation::InsertBlock {
                after: Some(vec![0]),
                text: "new".to_owned(),
            },
            PageOperation::IndentBlock { block: vec![1] },
        ],
    )
    .expect("the inserted block is addressable when the indent runs");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- alpha\n  - beta\n  - gamma\n  - new\n- delta\n"
    );
}

#[test]
fn a_collapse_after_a_move_holds_the_moved_block() {
    let (graph, mut opened) = open("alpha", NESTED);

    let result = commit(
        &mut opened,
        "alpha",
        vec![
            PageOperation::MoveBlock {
                block: vec![1],
                target: BatchMoveTarget::Before { anchor: vec![0] },
            },
            PageOperation::SetCollapsed {
                block: vec![0],
                collapsed: true,
            },
        ],
    )
    .expect("the moved block is addressable at its new path");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- delta\n  collapsed:: true\n- alpha\n  - beta\n  - gamma\n"
    );
    let snapshot = result.value().snapshot();
    assert_eq!(snapshot.blocks()[0].text(), "delta");
    assert!(snapshot.blocks()[0].collapsed());
    assert!(!snapshot.blocks()[1].collapsed());
}

#[test]
fn one_commit_advances_one_revision_however_many_operations_it_holds() {
    let (graph, mut opened) = open("alpha", FOUR);
    let before = revision(&mut opened, "alpha").expect("the page is projected");

    let operations = || {
        vec![
            PageOperation::EditText {
                block: vec![0],
                text: "A".to_owned(),
            },
            PageOperation::EditText {
                block: vec![1],
                text: "B".to_owned(),
            },
            PageOperation::EditText {
                block: vec![2],
                text: "C".to_owned(),
            },
            PageOperation::EditText {
                block: vec![3],
                text: "D".to_owned(),
            },
            PageOperation::InsertBlock {
                after: None,
                text: "e".to_owned(),
            },
        ]
    };

    let first = commit(&mut opened, "alpha", operations()).expect("commit five operations");
    assert_eq!(first.value().applied(), 5);
    assert_eq!(
        first.meta().revision(),
        before + 1,
        "five operations are one commit, not five"
    );
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- A\n- B\n- C\n- D\n- e\n"
    );

    // The same batch again, against the version the first commit produced. It applies again and
    // advances one more revision: a commit writes, so a batch never short-circuits on equal bytes.
    let second = opened
        .commit_page(&PageCommit::new(
            "alpha",
            first.value().snapshot().version().clone(),
            operations(),
        ))
        .expect("commit the same operations against the version the first one produced");
    assert_eq!(second.meta().revision(), before + 2);
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- A\n- B\n- C\n- D\n- e\n- e\n"
    );
}

#[test]
fn an_empty_commit_writes_nothing_and_reports_the_page() {
    let (graph, mut opened) = open("alpha", NESTED);
    let before = revision(&mut opened, "alpha").expect("the page is projected");
    let bytes = read_text(&graph, "pages/alpha.md");

    let result = commit(&mut opened, "alpha", Vec::new()).expect("an empty commit is legal");

    assert_eq!(result.value().applied(), 0);
    assert_eq!(result.meta().revision(), before, "nothing was committed");
    assert_eq!(read_text(&graph, "pages/alpha.md"), bytes);
    assert_eq!(
        result.value().snapshot(),
        &snapshot_of(&mut opened, "alpha")
    );
}

#[test]
fn a_commit_at_the_current_version_succeeds() {
    let (graph, mut opened) = open("alpha", NESTED);

    commit(
        &mut opened,
        "alpha",
        vec![PageOperation::EditText {
            block: vec![0],
            text: "ALPHA".to_owned(),
        }],
    )
    .expect("the base version is the page's own version");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- ALPHA\n  - beta\n  - gamma\n- delta\n"
    );
}

#[test]
fn a_write_to_another_page_does_not_make_this_pages_base_version_stale() {
    let (graph, mut opened) = open("alpha", NESTED);
    graph.write("pages/beta.md", "- other\n");
    let base = base_version(&mut opened, "alpha");

    // Another page's write advances the index revision; this page's bytes do not move.
    opened
        .append_block("beta", "added")
        .expect("write the other page through the engine");
    assert_eq!(base, base_version(&mut opened, "alpha"));

    let result = opened
        .commit_page(&PageCommit::new(
            "alpha",
            base,
            vec![PageOperation::EditText {
                block: vec![1],
                text: "DELTA".to_owned(),
            }],
        ))
        .expect("the version guard is page-scoped");

    assert_eq!(result.value().snapshot().page(), "alpha");
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- alpha\n  - beta\n  - gamma\n- DELTA\n"
    );
}

#[test]
fn an_external_edit_makes_the_base_version_stale_and_writes_nothing() {
    let (graph, mut opened) = open("alpha", NESTED);
    let base = base_version(&mut opened, "alpha");

    graph.write("pages/alpha.md", "- alpha\n- changed\n");
    let bytes = read_text(&graph, "pages/alpha.md");
    let before = revision(&mut opened, "alpha");

    let error = opened
        .commit_page(&PageCommit::new(
            "alpha",
            base.clone(),
            vec![PageOperation::EditText {
                block: vec![1],
                text: "x".to_owned(),
            }],
        ))
        .expect_err("the file no longer holds the version the paths were computed from");

    match error {
        WriteError::StaleVersion {
            page,
            base_version,
            current,
        } => {
            assert_eq!(page, "alpha");
            assert_eq!(base_version, base);
            assert_eq!(current.page(), "alpha", "the snapshot is this page");
            assert_eq!(texts(&current), vec!["alpha", "changed"]);
            assert_eq!(
                current.version(),
                snapshot_of(&mut opened, "alpha").version(),
                "the carried snapshot describes the bytes that were read"
            );
        }
        other => panic!("expected a stale version, got {other:?}"),
    }

    assert_eq!(read_text(&graph, "pages/alpha.md"), bytes);
    assert_eq!(revision(&mut opened, "alpha"), before);
}

#[test]
fn an_empty_commit_at_a_stale_version_is_still_refused() {
    let (graph, mut opened) = open("alpha", NESTED);
    let base = base_version(&mut opened, "alpha");
    graph.write("pages/alpha.md", "- alpha\n");
    let bytes = read_text(&graph, "pages/alpha.md");
    let before = revision(&mut opened, "alpha");

    let error = opened
        .commit_page(&PageCommit::new("alpha", base, Vec::new()))
        .expect_err("the guard runs before the empty batch is answered");

    assert!(
        matches!(error, WriteError::StaleVersion { .. }),
        "expected a stale version, got {error:?}"
    );
    assert_eq!(read_text(&graph, "pages/alpha.md"), bytes);
    assert_eq!(revision(&mut opened, "alpha"), before);
}

#[test]
fn a_refused_second_operation_leaves_the_first_one_unapplied() {
    let (graph, mut opened) = open("alpha", NESTED);
    let bytes = read_text(&graph, "pages/alpha.md");
    let before = revision(&mut opened, "alpha");

    let error = commit(
        &mut opened,
        "alpha",
        vec![
            PageOperation::EditText {
                block: vec![0, 0],
                text: "BETA".to_owned(),
            },
            // `IndentBlock` of the page's first block has no previous sibling to indent under.
            PageOperation::IndentBlock { block: vec![0] },
            PageOperation::EditText {
                block: vec![1],
                text: "unreached".to_owned(),
            },
        ],
    )
    .expect_err("the second operation is illegal, so the batch is");

    match error {
        WriteError::Operation {
            page,
            index,
            operation,
            cause,
        } => {
            assert_eq!(page, "alpha");
            assert_eq!(index, 1, "the failure names its own position");
            assert_eq!(operation, "IndentBlock");
            assert_eq!(cause, EditError::NoPreviousSibling);
        }
        other => panic!("expected a refused operation, got {other:?}"),
    }

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        bytes,
        "no prefix of the batch was written"
    );
    assert_eq!(revision(&mut opened, "alpha"), before);
}

#[test]
fn a_refused_first_operation_names_index_zero() {
    let (graph, mut opened) = open("alpha", NESTED);
    let bytes = read_text(&graph, "pages/alpha.md");
    let before = revision(&mut opened, "alpha");

    let error = commit(
        &mut opened,
        "alpha",
        vec![
            PageOperation::OutdentBlock { block: vec![0] },
            PageOperation::EditText {
                block: vec![0],
                text: "unreached".to_owned(),
            },
        ],
    )
    .expect_err("a top-level block has no parent to outdent to");

    match error {
        WriteError::Operation {
            index,
            operation,
            cause,
            ..
        } => {
            assert_eq!(index, 0);
            assert_eq!(operation, "OutdentBlock");
            assert_eq!(cause, EditError::TopLevelCannotOutdent);
        }
        other => panic!("expected a refused operation, got {other:?}"),
    }

    assert_eq!(read_text(&graph, "pages/alpha.md"), bytes);
    assert_eq!(revision(&mut opened, "alpha"), before);
}

/// Assert that committing `block` refused the operation as a positional miss.
fn refuses_path(graph: &mut Graph, page: &str, block: Vec<u32>) {
    let error = commit(
        graph,
        page,
        vec![PageOperation::EditText {
            block,
            text: "x".to_owned(),
        }],
    )
    .expect_err("the path names no block");
    match error {
        WriteError::Operation { cause, .. } => assert_eq!(cause, EditError::PathOutOfRange),
        other => panic!("expected a path refusal, got {other:?}"),
    }
}

#[test]
fn the_empty_path_names_no_block() {
    let (graph, mut opened) = open("alpha", NESTED);
    let bytes = read_text(&graph, "pages/alpha.md");

    refuses_path(&mut opened, "alpha", Vec::new());

    assert_eq!(read_text(&graph, "pages/alpha.md"), bytes);
}

#[test]
fn a_path_past_the_last_sibling_is_refused() {
    let (graph, mut opened) = open("alpha", NESTED);
    let bytes = read_text(&graph, "pages/alpha.md");

    // Two top-level blocks, so position 2 names nothing; the same for a child position.
    refuses_path(&mut opened, "alpha", vec![2]);
    refuses_path(&mut opened, "alpha", vec![0, 2]);

    assert_eq!(read_text(&graph, "pages/alpha.md"), bytes);
}

#[test]
fn a_path_under_a_block_an_earlier_operation_deleted_is_refused() {
    let (graph, mut opened) = open("alpha", NESTED);
    let bytes = read_text(&graph, "pages/alpha.md");
    let before = revision(&mut opened, "alpha");

    // `UnwrapBlock` deletes `alpha` and lifts `beta` and `gamma` to the top level, so `[0, 0]` —
    // the path that named a child of `alpha` — names nothing. The refusal is the point: editing
    // whatever now sits at position zero would change a block the caller never named.
    let error = commit(
        &mut opened,
        "alpha",
        vec![
            PageOperation::UnwrapBlock { block: vec![0] },
            PageOperation::EditText {
                block: vec![0, 0],
                text: "x".to_owned(),
            },
        ],
    )
    .expect_err("the deleted block's child path no longer names a block");

    match error {
        WriteError::Operation { index, cause, .. } => {
            assert_eq!(index, 1);
            assert_eq!(cause, EditError::PathOutOfRange);
        }
        other => panic!("expected a path refusal, got {other:?}"),
    }

    assert_eq!(read_text(&graph, "pages/alpha.md"), bytes);
    assert_eq!(revision(&mut opened, "alpha"), before);
}

#[test]
fn bytes_outside_the_edited_line_are_copied_unchanged() {
    // A BOM, CRLF endings, tab indentation, an `id::`, a `collapsed::`, a custom property, and no
    // final newline: every shape the document layer promises to copy, in one file.
    const SOURCE: &str = "\u{feff}- first\r\n\t- second\r\n\t\tid:: 11111111-1111-1111-1111-111111111111\r\n\t\tcollapsed:: true\r\n\t\tcustom:: kept\r\n- third";
    let (graph, mut opened) = open("alpha", SOURCE);
    // The fixture itself has to survive a parse, or the byte comparison below would prove nothing.
    assert_eq!(PageDocument::parse(SOURCE).to_source(), SOURCE);

    commit(
        &mut opened,
        "alpha",
        vec![PageOperation::EditText {
            block: vec![1],
            text: "THIRD".to_owned(),
        }],
    )
    .expect("commit one text edit");

    let file = read_text(&graph, "pages/alpha.md");
    assert_eq!(file, SOURCE.replace("third", "THIRD"));
    assert_eq!(
        PageDocument::parse(&file).to_source(),
        file,
        "the file reads back byte for byte"
    );
}

#[test]
fn inline_syntax_survives_an_edit_verbatim() {
    let (graph, mut opened) = open("alpha", "- old\n");
    let text = "keep `code`, [[ref]], ((61745deb-eeed-4426-8020-eb9e5c90d558)), e\u{301}, 😀";

    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::EditText {
            block: vec![0],
            text: text.to_owned(),
        }],
    )
    .expect("the edit writes the text verbatim");

    assert_eq!(read_text(&graph, "pages/alpha.md"), format!("- {text}\n"));
    assert_eq!(texts(result.value().snapshot()), vec![text]);
}

#[test]
fn a_split_offset_inside_a_character_is_refused() {
    let (graph, mut opened) = open("alpha", "- 😀😀\n");
    let bytes = read_text(&graph, "pages/alpha.md");

    // A caret cannot sit inside a character: offset 1 is the middle of the first emoji.
    let error = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::SplitBlock {
            block: vec![0],
            offset: 1,
        }],
    )
    .expect_err("offset 1 splits the first emoji");
    match error {
        WriteError::Operation { cause, .. } => {
            assert_eq!(cause, EditError::TextOffsetSplitsCharacter { offset: 1 });
        }
        other => panic!("expected a split refusal, got {other:?}"),
    }
    assert_eq!(read_text(&graph, "pages/alpha.md"), bytes);

    // A boundary offset splits, and each half reads back as its own block.
    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::SplitBlock {
            block: vec![0],
            offset: 4,
        }],
    )
    .expect("offset 4 is a character boundary");
    assert_eq!(read_text(&graph, "pages/alpha.md"), "- 😀\n- 😀\n");
    assert_eq!(texts(result.value().snapshot()), vec!["😀", "😀"]);
}

#[test]
fn a_code_fence_inside_a_block_survives_an_edit_of_another_block() {
    const SOURCE: &str = "\
- note
  ```sh
  echo hi
  ```
- tail
";
    let (graph, mut opened) = open("alpha", SOURCE);
    assert_eq!(PageDocument::parse(SOURCE).to_source(), SOURCE);

    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::EditText {
            block: vec![1],
            text: "TAIL".to_owned(),
        }],
    )
    .expect("edit the block after the fence");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        SOURCE.replace("- tail", "- TAIL")
    );
    assert_eq!(
        texts(result.value().snapshot()),
        vec!["note\n```sh\necho hi\n```", "TAIL"],
        "the fence is the first block's own text, and the edit did not touch it"
    );
}

#[test]
fn a_collapse_writes_one_property_line_and_leaves_the_other_block_alone() {
    const SOURCE: &str = "\
- alpha
  collapsed:: false
  - beta
- gamma
- delta
";
    let (graph, mut opened) = open("alpha", SOURCE);

    commit(
        &mut opened,
        "alpha",
        vec![PageOperation::SetCollapsed {
            block: vec![1],
            collapsed: true,
        }],
    )
    .expect("fold the second block");

    // The line lands after the folded block's own line and before its next sibling, and the other
    // block's `collapsed:: false` is copied unchanged: `false` is written, never removed.
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "\
- alpha
  collapsed:: false
  - beta
- gamma
  collapsed:: true
- delta
"
    );
}

#[test]
fn a_marker_change_rewrites_the_marker_bytes_and_nothing_else() {
    let (graph, mut opened) = open("alpha", NESTED);

    commit(
        &mut opened,
        "alpha",
        vec![PageOperation::SetMarker {
            block: vec![1],
            marker: BlockMarker::Heading { level: 3 },
        }],
    )
    .expect("turn the bullet into a heading");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        NESTED.replace("- delta", "### delta")
    );

    // And back, from a page whose first block is a heading.
    let (graph, mut opened) = open("bravo", "### head\n- x\n");
    commit(
        &mut opened,
        "bravo",
        vec![PageOperation::SetMarker {
            block: vec![0],
            marker: BlockMarker::Bullet,
        }],
    )
    .expect("turn the heading into a bullet");
    assert_eq!(read_text(&graph, "pages/bravo.md"), "- head\n- x\n");
}

#[test]
fn the_answered_snapshot_is_the_next_read_of_the_page() {
    let (_graph, mut opened) = open("alpha", NESTED);

    let result = commit(
        &mut opened,
        "alpha",
        vec![
            PageOperation::InsertBlock {
                after: Some(vec![0, 1]),
                text: "inserted".to_owned(),
            },
            PageOperation::SetMarker {
                block: vec![1],
                marker: BlockMarker::Heading { level: 1 },
            },
            PageOperation::SetCollapsed {
                block: vec![0],
                collapsed: true,
            },
        ],
    )
    .expect("commit three operations");

    let snapshot = result.value().snapshot();
    assert_eq!(snapshot, &snapshot_of(&mut opened, "alpha"));
    assert_eq!(
        child_paths(snapshot),
        vec![vec![0], vec![0, 0], vec![0, 1], vec![0, 2], vec![1]]
    );
    assert_eq!(
        texts(snapshot),
        vec!["alpha", "beta", "gamma", "inserted", "delta"]
    );
    assert!(snapshot.blocks()[0].collapsed());
    assert_eq!(snapshot.blocks()[0].marker(), BlockMarker::Bullet);
    assert_eq!(
        snapshot.blocks()[1].marker(),
        BlockMarker::Heading { level: 1 }
    );
    assert_eq!(
        snapshot.version(),
        snapshot_of(&mut opened, "alpha").version(),
        "the answer describes the bytes on disk"
    );
}

#[test]
fn a_false_collapse_writes_the_property_line_on_a_block_that_states_none() {
    // `false` is written, never removed: unfolding is a state the file states, and the byte a batch
    // writes has to be the byte `Graph::set_block_collapsed` writes.
    let (graph, mut opened) = open("alpha", NESTED);

    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::SetCollapsed {
            block: vec![1],
            collapsed: false,
        }],
    )
    .expect("a block that states no `collapsed::` takes the line");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- alpha\n  - beta\n  - gamma\n- delta\n  collapsed:: false\n"
    );
    let snapshot = result.value().snapshot();
    let delta = &snapshot.blocks()[1];
    assert_eq!(delta.text(), "delta");
    assert!(!delta.collapsed(), "`false` reads back as unfolded");
    assert_eq!(
        delta
            .properties()
            .iter()
            .find(|property| property.normalized_key() == "collapsed")
            .map(|property| property.value()),
        Some("false"),
        "the line is in the file and belongs to this block"
    );
}

#[test]
fn a_collapse_on_a_single_block_page_ends_the_file_with_the_property_line() {
    // The page's only block is also its last line, so the property line has no line to land before.
    // That shape is the ordinary one for a page created a moment ago, and it used to panic.
    let (graph, mut opened) = open("only", "- a\n");
    let before = revision(&mut opened, "only").expect("the page is projected");

    let result = commit(
        &mut opened,
        "only",
        vec![PageOperation::SetCollapsed {
            block: vec![0],
            collapsed: true,
        }],
    )
    .expect("folding the page's only block is one legal operation");

    assert_eq!(result.value().applied(), 1);
    assert_eq!(
        read_text(&graph, "pages/only.md"),
        "- a\n  collapsed:: true\n"
    );
    assert!(result.value().snapshot().blocks()[0].collapsed());
    assert_eq!(result.meta().revision(), before + 1);
}

// --- conformance with the editor's reference operations --------------------------------------------
//
// The editor states each structure operation twice: as a ProseMirror transaction and as the oracle in
// `packages/graph-client/src/operations.ts` (`applyOperation`). The tests below state that oracle's
// answer for one tree beside the bytes and blocks this layer answers, so a divergence names the
// operation it belongs to instead of surfacing as a wrong page later.
//
// The oracle's block is `BlockSnapshot { text, marker, collapsed, children }` — the same four facts
// `SnapshotBlock` carries, with the property lines this layer keeps beside the text outside the
// reference's model, which has no property channel at all.

/// A page with three levels and a sibling after the first child: the shape every ordering below
/// differs on, because the only interesting question they have is where a subtree lands relative to
/// a parent's other children.
const THREE_LEVELS: &str = "\
- parent
  - first
    - grandchild
  - second
- after
";

#[test]
fn split_keeps_the_head_s_children_and_puts_the_tail_after_the_whole_subtree() {
    // The oracle's `split-block` splices the tail in at the original's index + 1 in its parent, so in
    // a tree it lands after the original's whole subtree: `parent[first[grandchild], second], after`
    // becomes `parent[first[grandchild], second], ent, after`. The children stay with the head,
    // which is what Logseq does when Enter lands in a block that has children.
    let (graph, mut opened) = open("alpha", THREE_LEVELS);

    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::SplitBlock {
            block: vec![0],
            offset: 3,
        }],
    )
    .expect("split the first top-level block");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- par\n  - first\n    - grandchild\n  - second\n- ent\n- after\n"
    );
    assert_eq!(
        child_paths(result.value().snapshot()),
        vec![
            vec![0],
            vec![0, 0],
            vec![0, 0, 0],
            vec![0, 1],
            vec![1],
            vec![2]
        ]
    );
    assert_eq!(
        texts(result.value().snapshot()),
        vec!["par", "first", "grandchild", "second", "ent", "after"]
    );
}

#[test]
fn split_a_first_child_leaves_its_own_children_with_the_head_and_its_tail_before_its_next_sibling()
{
    let (graph, mut opened) = open("alpha", THREE_LEVELS);

    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::SplitBlock {
            block: vec![0, 0],
            offset: 2,
        }],
    )
    .expect("split a block that has a child");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- parent\n  - fi\n    - grandchild\n  - rst\n  - second\n- after\n"
    );
    assert_eq!(
        child_paths(result.value().snapshot()),
        vec![
            vec![0],
            vec![0, 0],
            vec![0, 0, 0],
            vec![0, 1],
            vec![0, 2],
            vec![1]
        ]
    );
    assert_eq!(
        texts(result.value().snapshot()),
        vec!["parent", "fi", "grandchild", "rst", "second", "after"]
    );
}

#[test]
fn merge_of_a_first_child_appends_the_text_and_lifts_its_children_to_the_front() {
    // The oracle's `merge-block` for a first child folds into the parent and *unshifts* the merged
    // block's children, because that is the position the merged block held; the parent's remaining
    // children stay behind them. Folding `first[grandchild]` into `parent` therefore answers
    // `parentfirst[grandchild, second]`, not `parentfirst[second, grandchild]`.
    let (graph, mut opened) = open("alpha", THREE_LEVELS);

    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::MergeBlock { block: vec![0, 0] }],
    )
    .expect("fold the first child into its parent");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- parentfirst\n  - grandchild\n  - second\n- after\n"
    );
    assert_eq!(
        child_paths(result.value().snapshot()),
        vec![vec![0], vec![0, 0], vec![0, 1], vec![1]]
    );
    assert_eq!(
        texts(result.value().snapshot()),
        vec!["parentfirst", "grandchild", "second", "after"]
    );
}

#[test]
fn merge_of_a_sibling_adopts_its_children_after_the_target_s_own() {
    // The other half of the same rule: a previous sibling keeps its children first and takes the
    // merged block's after them, because that is where the merged block sat.
    let (graph, mut opened) = open("alpha", "- first\n  - kept\n- second\n  - adopted\n");

    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::MergeBlock { block: vec![1] }],
    )
    .expect("fold the second block into the first");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- firstsecond\n  - kept\n  - adopted\n"
    );
    assert_eq!(
        texts(result.value().snapshot()),
        vec!["firstsecond", "kept", "adopted"]
    );
}

#[test]
fn merge_of_the_first_top_level_block_is_refused_and_writes_nothing() {
    // The oracle throws `OperationError` here; this layer names the operation and its position, and
    // the whole commit is thrown away.
    let (graph, mut opened) = open("alpha", THREE_LEVELS);

    match commit(
        &mut opened,
        "alpha",
        vec![PageOperation::MergeBlock { block: vec![0] }],
    ) {
        Err(WriteError::Operation {
            index,
            operation,
            cause,
            ..
        }) => {
            assert_eq!(index, 0);
            assert_eq!(operation, "MergeBlock");
            assert_eq!(cause, EditError::NoPreviousSibling);
        }
        other => panic!("expected the merge to be refused, got {other:?}"),
    }
    assert_eq!(read_text(&graph, "pages/alpha.md"), THREE_LEVELS);
}

#[test]
fn unwrap_lifts_the_children_into_the_deleted_block_s_place() {
    let (graph, mut opened) = open("alpha", THREE_LEVELS);

    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::UnwrapBlock { block: vec![0, 0] }],
    )
    .expect("delete the first child and lift its child");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- parent\n  - grandchild\n  - second\n- after\n"
    );
    assert_eq!(
        texts(result.value().snapshot()),
        vec!["parent", "grandchild", "second", "after"]
    );
}

#[test]
fn indent_makes_the_block_the_last_child_of_its_previous_sibling() {
    let (graph, mut opened) = open("alpha", THREE_LEVELS);

    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::IndentBlock { block: vec![0, 1] }],
    )
    .expect("indent the second child under the first");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- parent\n  - first\n    - grandchild\n    - second\n- after\n"
    );
    assert_eq!(
        child_paths(result.value().snapshot()),
        vec![vec![0], vec![0, 0], vec![0, 0, 0], vec![0, 0, 1], vec![1]]
    );
}

#[test]
fn outdent_lands_the_block_and_its_subtree_right_after_its_parent_s_subtree() {
    let (graph, mut opened) = open("alpha", THREE_LEVELS);

    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::OutdentBlock { block: vec![0, 0] }],
    )
    .expect("outdent the first child to the level above its parent");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- parent\n  - second\n- first\n  - grandchild\n- after\n"
    );
    assert_eq!(
        child_paths(result.value().snapshot()),
        vec![vec![0], vec![0, 0], vec![1], vec![1, 0], vec![2]]
    );
    assert_eq!(
        texts(result.value().snapshot()),
        vec!["parent", "second", "first", "grandchild", "after"]
    );
}

#[test]
fn a_move_into_the_moved_block_s_own_subtree_is_refused() {
    let (graph, mut opened) = open("alpha", THREE_LEVELS);

    match commit(
        &mut opened,
        "alpha",
        vec![PageOperation::MoveBlock {
            block: vec![0],
            target: BatchMoveTarget::LastChildOf {
                anchor: vec![0, 0, 0],
            },
        }],
    ) {
        Err(WriteError::Operation { cause, .. }) => {
            assert_eq!(cause, EditError::TargetInsideMovedSubtree);
        }
        other => panic!("expected the move to be refused, got {other:?}"),
    }
    assert_eq!(read_text(&graph, "pages/alpha.md"), THREE_LEVELS);
}

#[test]
fn a_split_offset_counts_utf8_bytes_so_a_utf16_offset_is_the_adapter_s_conversion() {
    // The oracle states its offset in UTF-16 code units, because it holds a JavaScript string; this
    // layer states UTF-8 bytes of the editable text (ADR-0011). `你`, `好`, `世`, and `界` are three
    // bytes and one code unit each, so the reference's offset 1 is this layer's 3 and its offset 2 is
    // 6. Converting is the adapter's job; this test pins both ends of that conversion.
    for (offset, expected) in [
        (3, "- 你\n- 好世界\n- a😀b\n"),
        (6, "- 你好\n- 世界\n- a😀b\n"),
    ] {
        let (graph, mut opened) = open("alpha", "- 你好世界\n- a😀b\n");
        commit(
            &mut opened,
            "alpha",
            vec![PageOperation::SplitBlock {
                block: vec![0],
                offset,
            }],
        )
        .expect("split the CJK block");
        assert_eq!(
            read_text(&graph, "pages/alpha.md"),
            expected,
            "offset {offset}"
        );
    }

    // An emoji is one code unit pair and four bytes: byte 1 ends the `a` and the whole character goes
    // to the tail, exactly as code-unit offset 1 would.
    let (graph, mut opened) = open("alpha", "- a😀b\n");
    commit(
        &mut opened,
        "alpha",
        vec![PageOperation::SplitBlock {
            block: vec![0],
            offset: 1,
        }],
    )
    .expect("split before an emoji");
    assert_eq!(read_text(&graph, "pages/alpha.md"), "- a\n- 😀b\n");

    // Neither layer may split a character, so an offset inside one is refused — which includes the
    // JavaScript offset 2 of `a😀b`, the position between the two halves of the surrogate pair.
    let (_graph, mut opened) = open("alpha", "- 你好世界\n- a😀b\n");
    for (block, offset) in [(vec![0], 2), (vec![1], 2)] {
        match commit(
            &mut opened,
            "alpha",
            vec![PageOperation::SplitBlock { block, offset }],
        ) {
            Err(WriteError::Operation { cause, .. }) => {
                assert_eq!(cause, EditError::TextOffsetSplitsCharacter { offset });
            }
            other => panic!("expected the split at {offset} to be refused, got {other:?}"),
        }
    }
}

#[test]
fn a_block_snapshot_carries_the_four_fields_the_editor_s_block_has() {
    // `BlockSnapshot` is `{ text, marker, collapsed, children }`, and this layer answers the same four.
    // A block's property lines are the one place the two models differ: the oracle has no property
    // channel, so they sit beside `text` here and never inside it — which is also why an `EditText`
    // that replaces the whole text leaves an `id::` and a `collapsed::` untouched.
    let source = "\
- alpha
  id:: 11111111-1111-1111-1111-111111111111
  collapsed:: true
  - child
";
    let (graph, mut opened) = open("alpha", source);

    let snapshot = snapshot_of(&mut opened, "alpha");
    let block = &snapshot.blocks()[0];
    assert_eq!(block.text(), "alpha");
    assert_eq!(block.marker(), BlockMarker::Bullet);
    assert!(block.collapsed());
    assert_eq!(
        block
            .children()
            .iter()
            .map(SnapshotBlock::text)
            .collect::<Vec<_>>(),
        vec!["child"]
    );
    assert_eq!(
        block.properties().len(),
        2,
        "the id and the fold are the block's, not part of its text"
    );

    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::EditText {
            block: vec![0],
            text: "edited".to_owned(),
        }],
    )
    .expect("replace the block's text");
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- edited\n  id:: 11111111-1111-1111-1111-111111111111\n  collapsed:: true\n  - child\n"
    );
    let after = &result.value().snapshot().blocks()[0];
    assert_eq!(after.text(), "edited");
    assert!(after.collapsed());
    assert!(after.durable_id().is_some());
}

#[test]
fn a_split_tail_that_starts_with_whitespace_loses_that_character() {
    // The oracle states `" world"` as the tail's text. A block's text can never begin with whitespace
    // — the parser strips the whole run after the marker — so this layer answers `"world"`, and the
    // one character is not representable in the file at all. An adapter cannot recover it either.
    let (graph, mut opened) = open("alpha", "- hello world\n  - child\n");

    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::SplitBlock {
            block: vec![0],
            offset: 5,
        }],
    )
    .expect("split between the two words");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- hello\n  - child\n-  world\n"
    );
    assert_eq!(
        texts(result.value().snapshot()),
        vec!["hello", "child", "world"]
    );
}

#[test]
fn an_insert_block_with_no_text_is_refused_where_the_reference_would_mint_an_empty_block() {
    // The oracle's `insert-block` accepts any text, `""` included; this layer refuses empty and
    // whitespace-only content, because the block it mints is written from one line of text. An empty
    // block has exactly one route through a commit: split a block at its start or its end.
    let (graph, mut opened) = open("alpha", "- only\n");
    for text in ["", "   "] {
        match commit(
            &mut opened,
            "alpha",
            vec![PageOperation::InsertBlock {
                after: None,
                text: text.to_owned(),
            }],
        ) {
            Err(WriteError::Operation { cause, .. }) => assert_eq!(cause, EditError::EmptyContent),
            other => panic!("expected the insert of {text:?} to be refused, got {other:?}"),
        }
    }
    assert_eq!(read_text(&graph, "pages/alpha.md"), "- only\n");

    let result = commit(
        &mut opened,
        "alpha",
        vec![PageOperation::SplitBlock {
            block: vec![0],
            offset: 4,
        }],
    )
    .expect("split the last block at its end");
    assert_eq!(read_text(&graph, "pages/alpha.md"), "- only\n-\n");
    assert_eq!(texts(result.value().snapshot()), vec!["only", ""]);
}

#[test]
fn a_split_offset_past_the_end_is_refused_where_the_reference_clamps_it() {
    // The oracle clamps an offset into `0..=text.length`; this layer refuses one past the end, because
    // an offset that names no position is a caller's mistake rather than a position to guess. An
    // adapter therefore clamps before it builds the operation, never relies on the library to.
    let (graph, mut opened) = open("alpha", "- alpha\n");

    match commit(
        &mut opened,
        "alpha",
        vec![PageOperation::SplitBlock {
            block: vec![0],
            offset: 6,
        }],
    ) {
        Err(WriteError::Operation { cause, .. }) => {
            assert_eq!(cause, EditError::TextOffsetOutOfRange { offset: 6, len: 5 })
        }
        other => panic!("expected the split to be refused, got {other:?}"),
    }
    assert_eq!(read_text(&graph, "pages/alpha.md"), "- alpha\n");
}

#[test]
fn an_edit_text_the_file_cannot_read_back_verbatim_is_refused() {
    // The oracle's `edit-text` stores any string. This layer refuses the two shapes a block's own
    // text cannot hold: a run that starts with whitespace, which the parser strips after the marker,
    // and a line that reads as a property line. Either would read back as different content, so the
    // edit is refused rather than silently rewritten.
    let (graph, mut opened) = open("alpha", "- alpha\n");

    for text in [" leading", "id:: 11111111-1111-1111-1111-111111111111"] {
        match commit(
            &mut opened,
            "alpha",
            vec![PageOperation::EditText {
                block: vec![0],
                text: text.to_owned(),
            }],
        ) {
            Err(WriteError::Operation { cause, .. }) => assert_eq!(
                cause,
                EditError::ContinuationChangesStructure,
                "the edit of {text:?}"
            ),
            other => panic!("expected the edit of {text:?} to be refused, got {other:?}"),
        }
    }
    assert_eq!(read_text(&graph, "pages/alpha.md"), "- alpha\n");
}
