//! The cross-page move path: two durable ids on two pages, one intent, two file writes, one
//! multi-page commit — and the recovery of every state a crash between them can leave.
//!
//! The live path is exercised end to end: bytes on disk, both projections, the revision, the
//! intent table, and the metadata the call answers. A crash cannot be produced from an integration
//! test, so each recovery state is seeded through the public document, source, and index APIs: run
//! the real move far enough to compute the intent, write the files by hand to the state being
//! tested, and store that intent with the graph journal. The fingerprints a seeded intent
//! carries are recomputed with the same FNV-1a the source layer uses, so the states are the ones
//! the production code would have recorded. The engine's own unit tests cover the live partial
//! failure through a test-only, instance-scoped fault.
//!
//! Recovery runs on every [`Graph::open`], before the startup scan, which is why the tests that
//! assert projections after a restart read with [`Freshness::LastScan`]: the recovery commit, not a
//! scan, is what those rows must show.

use std::path::PathBuf;

use logseq_graph::document::PageDocument;
use logseq_graph::domain::{BlockId, BlockSelector, MoveBlockAddress};
use logseq_graph::engine::{EngineError, Freshness, Graph, MoveTarget, WriteError};
use logseq_graph::index::{Index, MoveAnchorKind, MoveIntent};
use logseq_graph::source::{GraphSource, SourceError};
use logseq_graph::source::{MoveJournal, WriteLock};

mod support;

use support::TempGraph;

/// The durable ids the fixtures state.
const ID_A: &str = "01234567-89ab-cdef-0123-456789abcdef";
const ID_B: &str = "fedcba98-7654-3210-fedc-ba9876543210";
const ID_C: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// The page a move starts on.
const ALPHA_PATH: &str = "pages/alpha.md";

/// The page a move lands in.
const BETA_PATH: &str = "pages/beta.md";

/// A source page with an identified block that has a child, and an identified sibling after it.
const ALPHA: &str = "\
- one
  id:: 01234567-89ab-cdef-0123-456789abcdef
  - child
- two
  id:: fedcba98-7654-3210-fedc-ba9876543210
";

/// A destination page with one identified block.
const BETA: &str = "\
- bravo
  id:: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee
";

/// `alpha` after the `one` subtree moves out of it.
const ALPHA_AFTER: &str = "- two\n  id:: fedcba98-7654-3210-fedc-ba9876543210\n";

/// `beta` after the `one` subtree lands after `bravo`.
const BETA_AFTER: &str = "\
- bravo
  id:: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee
- one
  id:: 01234567-89ab-cdef-0123-456789abcdef
  - child
";

/// The `id::` UUID as a caller would hold it.
fn id(raw: &str) -> BlockId {
    BlockId::parse(raw).expect("valid 8-4-4-4-12 UUID")
}

/// The id no fixture states.
fn missing() -> BlockId {
    id("00000000-0000-0000-0000-000000000000")
}

/// Bytes of a graph file, as they are on disk.
fn read_bytes(graph: &TempGraph, relative: &str) -> Vec<u8> {
    std::fs::read(graph.graph_root().join(relative)).expect("read graph file")
}

/// Text of a graph file, as it is on disk.
fn read_text(graph: &TempGraph, relative: &str) -> String {
    String::from_utf8(read_bytes(graph, relative)).expect("graph file is UTF-8")
}

/// The content fingerprint of `text`, the same FNV-1a the source layer stores in baselines and
/// the intent table.
fn fingerprint(text: &str) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut fingerprint = OFFSET_BASIS;
    for byte in text.as_bytes() {
        fingerprint ^= u64::from(*byte);
        fingerprint = fingerprint.wrapping_mul(PRIME);
    }
    fingerprint
}

/// The pending intent the live move of `one` from `alpha` after `bravo` would record.
fn seeded_intent() -> MoveIntent {
    MoveIntent {
        moved: MoveBlockAddress::Durable(id(ID_A)),
        anchor: MoveBlockAddress::Durable(id(ID_C)),
        anchor_kind: MoveAnchorKind::After,
        from_relative_path: PathBuf::from(ALPHA_PATH),
        from_page: "alpha".to_owned(),
        from_pre_hash: fingerprint(ALPHA),
        from_post_hash: fingerprint(ALPHA_AFTER),
        to_relative_path: PathBuf::from(BETA_PATH),
        to_page: "beta".to_owned(),
        to_pre_hash: fingerprint(BETA),
        to_post_hash: fingerprint(BETA_AFTER),
    }
}

/// An index over the sandbox's index path, for seeding states a crash would have left.
fn open_index(graph: &TempGraph) -> Index {
    Index::open_at(graph.index_path()).expect("open index at the sandbox path")
}

/// The revision the index is at, read without opening a `Graph` (which would recover).
fn stored_revision(graph: &TempGraph) -> u64 {
    open_index(graph)
        .metadata()
        .expect("read metadata")
        .revision()
}

fn seed_journal(graph: &TempGraph) {
    let lock = WriteLock::acquire(&graph.graph_root(), None).expect("lock");
    MoveJournal::new(&lock)
        .expect("journal")
        .write(&seeded_intent())
        .expect("seed intent");
}

fn intent_exists(graph: &TempGraph) -> bool {
    let lock = WriteLock::acquire(&graph.graph_root(), None).expect("lock");
    MoveJournal::new(&lock)
        .expect("journal")
        .read()
        .expect("read intent")
        .is_some()
}

#[test]
fn legacy_intents_are_migrated_before_schema_discard() {
    let graph = TempGraph::new("legacy-intent");
    graph.write(ALPHA_PATH, ALPHA);
    graph.write(BETA_PATH, BETA_AFTER);
    drop(graph.open());
    let conn = rusqlite::Connection::open(
        Index::open_at(graph.index_path())
            .expect("resolve active index")
            .storage_path()
            .expect("file-backed index"),
    )
    .expect("legacy index");
    conn.execute_batch("CREATE TABLE write_intent (id INTEGER PRIMARY KEY, operation TEXT, moved_block_id TEXT, anchor_block_id TEXT, anchor_kind TEXT, from_relative_path TEXT, from_page TEXT, from_pre_hash INTEGER, from_post_hash INTEGER, to_relative_path TEXT, to_page TEXT, to_pre_hash INTEGER, to_post_hash INTEGER); UPDATE graph SET schema_version = 14").expect("legacy schema");
    let intent = seeded_intent();
    conn.execute("INSERT INTO write_intent VALUES (1, 'move_block', ?1, ?2, 'after', ?3, 'alpha', ?4, ?5, ?6, 'beta', ?7, ?8)", rusqlite::params![ID_A, ID_C, ALPHA_PATH, intent.from_pre_hash as i64, intent.from_post_hash as i64, BETA_PATH, intent.to_pre_hash as i64, intent.to_post_hash as i64]).expect("legacy intent");
    drop(conn);
    assert!(
        Index::open_at(graph.index_path()).is_err(),
        "direct index open must not discard recovery state"
    );
    let _opened = graph.open();
    assert_eq!(read_text(&graph, ALPHA_PATH), ALPHA_AFTER);
    assert_eq!(read_text(&graph, BETA_PATH), BETA_AFTER);
    assert!(!intent_exists(&graph));
    let conn = rusqlite::Connection::open(
        Index::open_at(graph.index_path())
            .expect("resolve active index")
            .storage_path()
            .expect("file-backed index"),
    )
    .expect("new index");
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'write_intent'",
            [],
            |row| row.get::<_, u32>(0)
        )
        .expect("no legacy table"),
        0
    );
}

#[cfg(unix)]
#[test]
fn a_read_only_graph_without_a_lock_refuses_to_recover_a_pending_move() {
    use std::fs;
    use std::io::ErrorKind;
    use std::os::unix::fs::PermissionsExt as _;

    let graph = TempGraph::new("read-only-pending-move");
    graph.write(ALPHA_PATH, ALPHA);
    graph.write(BETA_PATH, BETA_AFTER);
    seed_journal(&graph);
    let root = graph.graph_root();
    let lock_path = root.join("write.lock");
    fs::remove_file(&lock_path).expect("simulate a missing lock");
    let permissions = fs::metadata(&root).expect("graph root").permissions();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).expect("make root read-only");
    match fs::File::create_new(&lock_path) {
        Ok(_) => {
            fs::set_permissions(&root, permissions).expect("restore graph root mode");
            return;
        }
        Err(error) => assert_eq!(error.kind(), ErrorKind::PermissionDenied),
    }
    let opened = Graph::open(graph.options());
    fs::set_permissions(&root, permissions).expect("restore graph root mode");

    assert!(matches!(
        opened,
        Err(EngineError::Lock(logseq_graph::source::LockError::Io(error)))
            if error.kind() == ErrorKind::PermissionDenied
    ));
    assert_eq!(read_text(&graph, ALPHA_PATH), ALPHA);
    assert_eq!(read_text(&graph, BETA_PATH), BETA_AFTER);
    assert!(intent_exists(&graph), "recovery state must survive");
}

#[cfg(unix)]
#[test]
fn journal_symlinks_are_refused_without_changing_pages_or_the_target() {
    let graph = TempGraph::new("journal-containment");
    let outside = TempGraph::new("journal-outside");
    graph.write(ALPHA_PATH, ALPHA);
    graph.write(BETA_PATH, BETA);
    let mut opened = graph.open();
    let target = outside.graph_root().join("intent");
    std::os::unix::fs::symlink(&target, graph.graph_root().join("write-intent.json"))
        .expect("journal symlink");
    assert!(opened
        .move_block(
            &BlockSelector::Durable(id(ID_A)),
            MoveTarget::After(BlockSelector::Durable(id(ID_C)))
        )
        .is_err());
    assert_eq!(read_text(&graph, ALPHA_PATH), ALPHA);
    assert_eq!(read_text(&graph, BETA_PATH), BETA);
    assert!(!target.exists());
    drop(opened);
    assert!(Graph::open(graph.options()).is_err());
    assert!(!target.exists());
}

#[test]
fn journals_round_trip_and_refuse_conflicting_or_malformed_operations() {
    let graph = TempGraph::new("journal-roundtrip");
    let lock = WriteLock::acquire(&graph.graph_root(), None).expect("lock");
    let journal = MoveJournal::new(&lock).expect("journal");
    assert!(journal.read().expect("empty").is_none());
    let intent = seeded_intent();
    journal.write(&intent).expect("write");
    journal.write(&intent).expect("idempotent migration");
    assert_eq!(journal.read().expect("roundtrip"), Some(intent.clone()));
    let mut different = intent;
    different.to_post_hash ^= 1;
    assert!(journal.write(&different).is_err());
    journal.clear().expect("clear");
    journal.clear().expect("clear absent");
    graph.write("write-intent.json", "not JSON");
    assert!(journal.read().is_err());
    assert!(journal.write(&different).is_err());
    assert_eq!(read_text(&graph, "write-intent.json"), "not JSON");
}

#[test]
fn journals_accept_legacy_durable_keys_and_round_trip_positional_endpoints() {
    let graph = TempGraph::new("journal-addresses");
    let lock = WriteLock::acquire(&graph.graph_root(), None).expect("lock");
    let journal = MoveJournal::new(&lock).expect("journal");
    let durable = seeded_intent();
    journal.write(&durable).expect("write durable");
    let legacy: serde_json::Value =
        serde_json::from_str(&read_text(&graph, "write-intent.json")).expect("JSON");
    assert_eq!(legacy["moved_block_id"], ID_A);
    assert_eq!(legacy["anchor_block_id"], ID_C);
    assert!(legacy.get("moved_block_path").is_none());
    assert!(legacy.get("anchor_block_path").is_none());
    assert_eq!(journal.read().expect("legacy durable keys"), Some(durable));
    for moved in [
        MoveBlockAddress::Durable(id(ID_A)),
        MoveBlockAddress::Path(vec![1, 0]),
    ] {
        for anchor in [
            MoveBlockAddress::Durable(id(ID_C)),
            MoveBlockAddress::Path(vec![0, 1]),
        ] {
            journal.clear().expect("clear");
            let intent = MoveIntent {
                moved: moved.clone(),
                anchor,
                ..seeded_intent()
            };
            journal.write(&intent).expect("write endpoints");
            assert_eq!(journal.read().expect("round trip"), Some(intent));
        }
    }
    for (id_key, path_key) in [
        ("moved_block_id", "moved_block_path"),
        ("anchor_block_id", "anchor_block_path"),
    ] {
        for malformed in [
            None,
            Some(serde_json::json!([0])),
            Some(serde_json::json!([])),
            Some(serde_json::json!([-1])),
            Some(serde_json::json!([4294967296_u64])),
            Some(serde_json::Value::Null),
        ] {
            let mut value = legacy.clone();
            if let Some(path) = malformed {
                value[path_key] = path;
            } else {
                value.as_object_mut().expect("object").remove(id_key);
            }
            graph.write("write-intent.json", value.to_string());
            assert!(journal.read().is_err(), "must reject {value}");
            if value.get(path_key).is_some() && value[path_key] != serde_json::json!([0]) {
                value.as_object_mut().expect("object").remove(id_key);
                graph.write("write-intent.json", value.to_string());
                assert!(journal.read().is_err(), "must reject invalid path {value}");
            }
        }
    }
}

#[test]
fn a_cross_page_move_writes_both_files_and_commits_both_pages_once() {
    let graph = TempGraph::new("cross-happy");
    graph.write(ALPHA_PATH, ALPHA);
    graph.write(BETA_PATH, BETA);
    // A third page, so "unrelated pages are not reprojected" is observable: its file is edited
    // afterwards in a way a stat cannot see, and the index must still hold the old text.
    graph.write("pages/gamma.md", "- unseen old\n");
    let mut opened = graph.open();
    let revision_before = stored_revision(&graph);
    let modified = graph.modified("pages/gamma.md");
    graph.write("pages/gamma.md", "- unseen new\n");
    graph.set_modified("pages/gamma.md", modified);

    let moved = opened
        .move_block(
            &BlockSelector::Durable(id(ID_A)),
            MoveTarget::After(BlockSelector::Durable(id(ID_C))),
        )
        .expect("move the block across pages");

    // Exact bytes, both sides: the cut and the paste are line operations, so everything outside
    // the moved run survives byte for byte.
    assert_eq!(read_text(&graph, ALPHA_PATH), ALPHA_AFTER);
    assert_eq!(read_text(&graph, BETA_PATH), BETA_AFTER);

    // The answer names the destination page, at the revision the write committed.
    assert_eq!(moved.value().name(), "beta");
    assert_eq!(moved.value().block_count(), 3);
    assert_eq!(moved.meta().revision(), revision_before + 1);
    assert_eq!(stored_revision(&graph), revision_before + 1);

    // The success path commits the intent away; nothing is pending after the call.
    assert!(!intent_exists(&graph));

    // Both pages are projected from the new bytes without another scan, and the durable block
    // now resolves only on the destination.
    let mut reads = opened.reads(Freshness::LastScan);
    let alpha = reads
        .page_blocks("alpha")
        .expect("read alpha")
        .into_value()
        .expect("alpha is projected");
    assert_eq!(
        alpha
            .iter()
            .map(|block| block.text().lines().next().unwrap())
            .collect::<Vec<_>>(),
        vec!["two"],
        "the source page lost the moved subtree"
    );
    let mut reads = opened.reads(Freshness::LastScan);
    let beta = reads
        .page_blocks("beta")
        .expect("read beta")
        .into_value()
        .expect("beta is projected");
    assert_eq!(
        beta.iter()
            .map(|block| block.text().lines().next().unwrap())
            .collect::<Vec<_>>(),
        vec!["bravo", "one", "child"]
    );
    let mut reads = opened.reads(Freshness::LastScan);
    let durable = reads
        .block(&BlockSelector::Durable(id(ID_A)))
        .expect("read the moved block")
        .into_value()
        .expect("the moved block is projected");
    assert_eq!(durable.page(), "beta");

    // Exactly one revision advance for two pages, so no other page was committed either.
    assert_eq!(stored_revision(&graph), revision_before + 1);

    // A page outside the move is not reprojected, even when its file changed beyond what a stat
    // can see: refreshing other pages is a read's job, and the caller asks for it.
    let mut reads = opened.reads(Freshness::LastScan);
    let old = reads.search("unseen old", 10).expect("search").into_value();
    assert_eq!(old.len(), 1, "the unrelated page keeps its last projection");
    let mut reads = opened.reads(Freshness::LastScan);
    let unseen = reads.search("unseen new", 10).expect("search").into_value();
    assert!(
        unseen.is_empty(),
        "the move must not have projected a page it did not write, got {unseen:?}"
    );
}

#[test]
fn refusals_before_the_intent_leave_both_files_and_the_intent_table_untouched() {
    let graph = TempGraph::new("cross-refusals");
    graph.write(ALPHA_PATH, ALPHA);
    graph.write(BETA_PATH, BETA);
    let mut opened = graph.open();
    let alpha_before = read_bytes(&graph, ALPHA_PATH);
    let beta_before = read_bytes(&graph, BETA_PATH);

    // An unknown source id, an unknown anchor id, and an ambiguous id are all resolved through
    // the durable lookup before either file is read, exactly as the same-page path refuses.
    let failure = opened
        .move_block(
            &BlockSelector::Durable(missing()),
            MoveTarget::After(BlockSelector::Durable(id(ID_C))),
        )
        .expect_err("no block states the id being moved");
    assert!(
        matches!(&failure, WriteError::BlockNotFound { id } if id == &missing()),
        "expected BlockNotFound for the moved block, got {failure:?}"
    );

    let failure = opened
        .move_block(
            &BlockSelector::Durable(id(ID_A)),
            MoveTarget::After(BlockSelector::Durable(missing())),
        )
        .expect_err("no block states the anchor id");
    assert!(
        matches!(&failure, WriteError::BlockNotFound { id } if id == &missing()),
        "expected BlockNotFound for the anchor, got {failure:?}"
    );

    // An id stated on two pages cannot say which block was meant, before any file is read.
    graph.write("pages/gamma.md", format!("- gamma one\n  id:: {ID_C}\n"));
    let mut rescanned = graph.open();
    let failure = rescanned
        .move_block(
            &BlockSelector::Durable(id(ID_A)),
            MoveTarget::After(BlockSelector::Durable(id(ID_C))),
        )
        .expect_err("the anchor id is stated on two pages");
    assert!(
        matches!(&failure, WriteError::AmbiguousBlockId { pages, .. }
            if pages == &["beta".to_owned(), "gamma".to_owned()]),
        "expected AmbiguousBlockId for the anchor, got {failure:?}"
    );

    assert_eq!(read_bytes(&graph, ALPHA_PATH), alpha_before);
    assert_eq!(read_bytes(&graph, BETA_PATH), beta_before);
    assert!(!intent_exists(&graph), "no refusal may leave an intent");
}

#[test]
fn a_failed_edit_before_the_intent_leaves_both_files_byte_identical() {
    // A cross-page edit can still be refused by the document layer after both files are read —
    // an ambiguous id inside one file is the case the durable lookup cannot see — and that
    // refusal happens before the intent, so nothing changes at all.
    let graph = TempGraph::new("cross-edit-refusal");
    // `ID_A` twice on the source page: the lookup answers one page, the document refuses.
    graph.write(
        ALPHA_PATH,
        format!("- one\n  id:: {ID_A}\n- one again\n  id:: {ID_A}\n- two\n  id:: {ID_B}\n"),
    );
    graph.write(BETA_PATH, BETA);
    let mut opened = graph.open();
    let alpha_before = read_bytes(&graph, ALPHA_PATH);
    let beta_before = read_bytes(&graph, BETA_PATH);

    let failure = opened
        .move_block(
            &BlockSelector::Durable(id(ID_A)),
            MoveTarget::After(BlockSelector::Durable(id(ID_C))),
        )
        .expect_err("the source file states the id twice");
    assert!(
        matches!(&failure, WriteError::Edit(_)),
        "expected the document refusal, got {failure:?}"
    );

    assert_eq!(read_bytes(&graph, ALPHA_PATH), alpha_before);
    assert_eq!(read_bytes(&graph, BETA_PATH), beta_before);
    assert!(!intent_exists(&graph));
}

#[test]
fn a_same_page_move_still_takes_the_single_file_path_without_an_intent() {
    let graph = TempGraph::new("cross-same-page");
    graph.write(ALPHA_PATH, ALPHA);
    let mut opened = graph.open();

    opened
        .move_block(
            &BlockSelector::Durable(id(ID_A)),
            MoveTarget::After(BlockSelector::Durable(id(ID_B))),
        )
        .expect("move the block within its page");

    assert_eq!(
        read_text(&graph, ALPHA_PATH),
        format!("- two\n  id:: {ID_B}\n- one\n  id:: {ID_A}\n  - child\n"),
    );
    assert!(!intent_exists(&graph), "a same-page move records no intent");
}

#[test]
fn a_restart_with_both_files_pre_clears_the_intent_without_a_write() {
    // A crash between the intent and the destination write: both files are pre, nothing was
    // written, so recovery clears the intent, writes nothing, and advances no revision.
    let graph = TempGraph::new("cross-recover-both-pre");
    graph.write(ALPHA_PATH, ALPHA);
    graph.write(BETA_PATH, BETA);
    seed_journal(&graph);

    // The recovery open runs with the startup scan off, so the only thing this open can change
    // is the intent; the file bytes below prove it changed nothing else.
    Graph::open(graph.options().with_startup_scan(false)).expect("open recovers");

    assert_eq!(read_text(&graph, ALPHA_PATH), ALPHA, "no file was written");
    assert_eq!(read_text(&graph, BETA_PATH), BETA, "no file was written");
    assert!(!intent_exists(&graph), "the spent intent was cleared");

    // A second open, now with the startup scan, projects the two unchanged pages and nothing
    // more: the recovery left no work behind for the scan.
    let mut opened = graph.open();
    assert_eq!(
        stored_revision(&graph),
        2,
        "one revision per projected page, and none for the recovery"
    );
    let mut reads = opened.reads(Freshness::LastScan);
    let alpha = reads
        .page_blocks("alpha")
        .expect("read alpha")
        .into_value()
        .expect("alpha is projected");
    assert_eq!(alpha.len(), 3, "the move never happened");
}

#[test]
fn a_restart_with_destination_post_finishes_the_move_from_the_current_source() {
    // A crash after the destination write: recovery re-derives the source removal from the file
    // as it is now, verifies it against the recorded post fingerprint, writes it, and commits
    // both pages in one transaction.
    let graph = TempGraph::new("cross-recover-dest-post");
    graph.write(ALPHA_PATH, ALPHA);
    graph.write(BETA_PATH, BETA_AFTER);
    drop(graph.open());
    seed_journal(&graph);
    let revision_before = stored_revision(&graph);

    // The recovery runs on open, with the startup scan off: recovery must not depend on it.
    let _opened =
        Graph::open(graph.options().with_startup_scan(false)).expect("open recovers the move");

    assert_eq!(read_text(&graph, ALPHA_PATH), ALPHA_AFTER);
    assert_eq!(read_text(&graph, BETA_PATH), BETA_AFTER);
    assert!(!intent_exists(&graph));
    assert_eq!(stored_revision(&graph), revision_before + 1);

    // The index holds both fresh pages, which the next open can serve without a scan.
    let mut opened =
        Graph::open(graph.options().with_startup_scan(false)).expect("reopen without a scan");
    let mut reads = opened.reads(Freshness::LastScan);
    let beta = reads
        .page_blocks("beta")
        .expect("read beta")
        .into_value()
        .expect("beta is projected by recovery");
    assert_eq!(
        beta.iter()
            .map(|block| block.text().lines().next().unwrap())
            .collect::<Vec<_>>(),
        vec!["bravo", "one", "child"]
    );
    let mut reads = opened.reads(Freshness::LastScan);
    let alpha = reads
        .page_blocks("alpha")
        .expect("read alpha")
        .into_value()
        .expect("alpha is projected by recovery");
    assert_eq!(
        alpha
            .iter()
            .map(|block| block.text().lines().next().unwrap())
            .collect::<Vec<_>>(),
        vec!["two"]
    );
}

#[test]
fn a_restart_with_both_files_post_commits_the_move_without_writing() {
    // A crash between the source write and the index commit: the bytes are already the intended
    // result, so recovery only commits both pages and clears the intent.
    let graph = TempGraph::new("cross-recover-both-post");
    graph.write(ALPHA_PATH, ALPHA_AFTER);
    graph.write(BETA_PATH, BETA_AFTER);
    drop(graph.open());
    seed_journal(&graph);
    let revision_before = stored_revision(&graph);

    let _opened =
        Graph::open(graph.options().with_startup_scan(false)).expect("open recovers the move");

    assert_eq!(
        read_text(&graph, ALPHA_PATH),
        ALPHA_AFTER,
        "no file changes"
    );
    assert_eq!(read_text(&graph, BETA_PATH), BETA_AFTER, "no file changes");
    assert!(!intent_exists(&graph));
    assert_eq!(stored_revision(&graph), revision_before + 1);

    let mut opened =
        Graph::open(graph.options().with_startup_scan(false)).expect("reopen without a scan");
    let mut reads = opened.reads(Freshness::LastScan);
    let beta = reads
        .page_blocks("beta")
        .expect("read beta")
        .into_value()
        .expect("beta is projected by recovery");
    assert_eq!(beta.len(), 3);
}

#[test]
fn a_restart_with_an_external_edit_projects_both_files_as_they_are() {
    // Both hashes match nothing the intent recorded: an outside edit. Recovery does not attempt
    // the move; it preserves the bytes and projects both files as they actually are, which also
    // clears the intent and advances the revision once.
    let externally_edited = "- one was here\n- two\n  id:: fedcba98-7654-3210-fedc-ba9876543210\n";
    let graph = TempGraph::new("cross-recover-interference");
    graph.write(ALPHA_PATH, externally_edited);
    graph.write(BETA_PATH, BETA);
    drop(graph.open());
    seed_journal(&graph);
    let revision_before = stored_revision(&graph);

    let _opened = Graph::open(graph.options().with_startup_scan(false))
        .expect("open reconciles the interference");

    assert_eq!(
        read_text(&graph, ALPHA_PATH),
        externally_edited,
        "the bytes an outsider wrote are not touched"
    );
    assert_eq!(read_text(&graph, BETA_PATH), BETA);
    assert!(!intent_exists(&graph));
    assert_eq!(
        stored_revision(&graph),
        revision_before + 1,
        "the reconciling commit is one revision advance"
    );

    // The projection describes the files as they are, not as the intent wanted them.
    let mut opened =
        Graph::open(graph.options().with_startup_scan(false)).expect("reopen without a scan");
    let mut reads = opened.reads(Freshness::LastScan);
    let alpha = reads
        .page_blocks("alpha")
        .expect("read alpha")
        .into_value()
        .expect("alpha is projected as it actually is");
    assert_eq!(
        alpha
            .iter()
            .map(|block| block.text().lines().next().unwrap())
            .collect::<Vec<_>>(),
        vec!["one was here", "two"]
    );
}

#[test]
fn a_restart_with_destination_pre_and_source_post_reconciles_instead_of_completing() {
    // Destination pre with source post cannot arise from this write's own destination-first
    // order, so recovery refuses to read it as a move and projects both files as they are.
    let graph = TempGraph::new("cross-recover-source-post-only");
    graph.write(ALPHA_PATH, ALPHA_AFTER);
    graph.write(BETA_PATH, BETA);
    drop(graph.open());
    seed_journal(&graph);
    let revision_before = stored_revision(&graph);

    let _opened = Graph::open(graph.options().with_startup_scan(false))
        .expect("open reconciles the impossible state");

    assert_eq!(read_text(&graph, ALPHA_PATH), ALPHA_AFTER);
    assert_eq!(read_text(&graph, BETA_PATH), BETA);
    assert!(!intent_exists(&graph));
    assert_eq!(stored_revision(&graph), revision_before + 1);

    let mut opened =
        Graph::open(graph.options().with_startup_scan(false)).expect("reopen without a scan");
    let mut reads = opened.reads(Freshness::LastScan);
    let alpha = reads
        .page_blocks("alpha")
        .expect("read alpha")
        .into_value()
        .expect("alpha is projected as it actually is");
    assert_eq!(
        alpha
            .iter()
            .map(|block| block.text().lines().next().unwrap())
            .collect::<Vec<_>>(),
        vec!["two"],
        "the source page is projected as its bytes say, without re-deriving the move"
    );
}

#[test]
fn a_missing_file_keeps_the_intent_and_fails_the_open() {
    // A file the intent names is gone: no branch of recovery describes that, so the intent stays
    // and the open fails instead of guessing.
    let graph = TempGraph::new("cross-recover-missing-file");
    graph.write(ALPHA_PATH, ALPHA);
    graph.write(BETA_PATH, BETA);
    seed_journal(&graph);
    graph.remove(BETA_PATH);

    let failure = match Graph::open(graph.options().with_startup_scan(false)) {
        Ok(_) => panic!("the open must fail: the destination file the intent names is gone"),
        Err(error) => error,
    };
    // The failure is the destination file, named by the path the intent records, with the
    // reason recovery refused to guess. Anything else — a scan error, a source error about the
    // source file, a missing index — means the open answered the wrong question.
    let EngineError::Source(SourceError::Io { path, source }) = &failure else {
        panic!("the open fails on the file the intent names, not on anything else: {failure}")
    };
    assert_eq!(
        path,
        // The engine canonicalizes the graph root at open, so on macOS the reported path spells
        // `/private/var` where `env::temp_dir` hands out `/var`.
        &std::fs::canonicalize(graph.graph_root())
            .expect("canonical graph root")
            .join(BETA_PATH),
        "the missing destination file is the one reported: {failure}"
    );
    assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
    assert!(
        source
            .to_string()
            .contains("the file a pending move intent names is gone"),
        "the reason says the intent names a file that is gone: {source}"
    );
    assert!(intent_exists(&graph), "the intent is kept for a human");
}

#[test]
fn recovery_does_not_depend_on_the_startup_scan_for_a_stale_index() {
    // A caller that only writes disables the startup scan. Recovery must still run and must
    // still leave an index that serves the recovered pages: the projections here are read with
    // LastScan from a handle that never scanned, so they can only come from the recovery commit.
    let graph = TempGraph::new("cross-recover-no-startup-scan");
    graph.write(ALPHA_PATH, ALPHA_AFTER);
    graph.write(BETA_PATH, BETA_AFTER);
    seed_journal(&graph);

    let mut opened = Graph::open(graph.options().with_startup_scan(false))
        .expect("open recovers without a startup scan");
    assert!(!intent_exists(&graph));
    let beta = opened
        .reads(Freshness::LastScan)
        .page_tree("beta")
        .expect("read beta")
        .into_value()
        .expect("beta is served from the recovery commit alone");
    assert_eq!(beta.page().block_count(), 3);
}

#[test]
fn a_live_move_with_the_startup_scan_disabled_still_commits_and_reads() {
    // The write-only caller from the OpenOptions docs: no startup scan, then a cross-page move.
    // The move resolves both pages from the durable lookup and re-lands the paths through the
    // source, so it needs no scan; the commit it makes must be readable without one.
    let graph = TempGraph::new("cross-live-no-startup-scan");
    graph.write(ALPHA_PATH, ALPHA);
    graph.write(BETA_PATH, BETA);
    let mut index = open_index(&graph);
    // The pages are projected through the index API, since no scan will do it.
    let source = GraphSource::open(graph.graph_root()).expect("open source");
    for (relative, name, text) in [(ALPHA_PATH, "alpha", ALPHA), (BETA_PATH, "beta", BETA)] {
        let file = source
            .page_at(std::path::Path::new(relative))
            .expect("resolve page file")
            .expect("the page file exists");
        let document = PageDocument::parse(text);
        index
            .replace_page_with_baseline(
                file.relative_path(),
                name,
                file.journal_date(),
                &document,
                logseq_graph::index::FileBaseline::new(file.metadata(), fingerprint(text)),
            )
            .expect("project the page");
    }
    drop(index);

    let mut opened =
        Graph::open(graph.options().with_startup_scan(false)).expect("open without a scan");
    let moved = opened
        .move_block(
            &BlockSelector::Durable(id(ID_A)),
            MoveTarget::After(BlockSelector::Durable(id(ID_C))),
        )
        .expect("move across pages without a startup scan");

    assert_eq!(read_text(&graph, ALPHA_PATH), ALPHA_AFTER);
    assert_eq!(read_text(&graph, BETA_PATH), BETA_AFTER);
    assert_eq!(moved.value().name(), "beta");
    assert!(!intent_exists(&graph));
}
