//! Configuration generations must agree across source handles and persisted projections.

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use logseq_graph::engine::{Graph, OpenOptions, ScanState, WriteState};
use logseq_graph::index::Index;

mod support;
use support::TempGraph;

const OLD_NAME: &str = "Jan 2nd, 2024";
const NEW_NAME: &str = "2024-01-02";

fn change_config(graph: &TempGraph) {
    graph.write_config(r#"{:journal/page-title-format "yyyy-MM-dd"}"#);
}

fn assert_consistent(graph: &TempGraph, opened: &mut Graph) {
    opened
        .append_block(NEW_NAME, "appended")
        .expect("write new journal name");
    assert_eq!(
        opened
            .append_block(OLD_NAME, "must not land")
            .expect_err("old name refused")
            .write_state(),
        WriteState::Unwritten
    );
    assert_eq!(
        fs::read_to_string(graph.graph_root().join("journals/2024_01_02.md")).expect("journal"),
        "- original\n- appended\n"
    );
    let pages = opened.pages().expect("pages");
    assert_eq!(pages.value().len(), 1);
    assert_eq!(pages.value()[0].name(), NEW_NAME);
    assert_eq!(pages.value()[0].block_count(), 2);
    assert!(opened
        .page_metadata(OLD_NAME)
        .expect("old name")
        .value()
        .is_none());
}

#[test]
fn offline_configuration_changes_reproject_even_when_startup_scan_is_disabled() {
    let graph = TempGraph::new("offline-config-generation");
    graph.write("journals/2024_01_02.md", "- original\n");
    drop(graph.open());
    change_config(&graph);
    let mut opened = Graph::open(graph.options().with_startup_scan(false)).expect("reopen");
    assert_eq!(
        opened.pages().expect("reprojected pages").value()[0].name(),
        NEW_NAME
    );
    assert_consistent(&graph, &mut opened);
}

#[test]
fn shared_configuration_reprojects_each_independent_index_before_writing() {
    let graph = TempGraph::new("shared-config-generation");
    graph.write("journals/2024_01_02.md", "- original\n");
    let state = Arc::new(ScanState::new());
    let mut scanner =
        Graph::open(graph.options().with_scan_state(Arc::clone(&state))).expect("scanner");
    let mut writer = Graph::open(
        OpenOptions::new(graph.graph_root(), graph.root().join("second.db"))
            .with_scan_state(state)
            .with_write_lock_wait(Duration::ZERO),
    )
    .expect("writer");
    change_config(&graph);
    scanner.rescan().expect("publish config");
    assert_eq!(
        writer.pages().expect("old independent index").value()[0].name(),
        OLD_NAME
    );
    assert_consistent(&graph, &mut writer);
    writer.rescan().expect("warm rescan");
    assert_eq!(
        writer.pages().expect("consistent after scan").value()[0].block_count(),
        2
    );
}

#[test]
fn a_legacy_index_without_a_signature_is_rebuilt() {
    let graph = TempGraph::new("legacy-config-generation");
    graph.write("journals/2024_01_02.md", "- original\n");
    drop(graph.open());
    let conn = rusqlite::Connection::open(
        Index::open_at(graph.index_path())
            .expect("resolve active index")
            .storage_path()
            .expect("file-backed index"),
    )
    .expect("legacy index");
    conn.execute_batch(
        "ALTER TABLE graph DROP COLUMN config_signature; UPDATE graph SET schema_version = 13",
    )
    .expect("legacy schema without signature");
    drop(conn);
    change_config(&graph);
    let mut opened =
        Graph::open(graph.options().with_startup_scan(false)).expect("rebuild legacy index");
    assert_consistent(&graph, &mut opened);
}

#[test]
fn a_changed_index_generation_refreshes_a_handle_without_shared_scan_state() {
    let graph = TempGraph::new("unshared-config-generation");
    graph.write("journals/2024_01_02.md", "- original\n");
    let mut scanner = graph.open();
    let mut writer = graph.open();
    change_config(&graph);
    scanner.rescan().expect("reproject shared index");
    assert_consistent(&graph, &mut writer);
}

#[test]
fn a_missing_signature_reprojects_unchanged_files_and_empty_graphs() {
    for populated in [false, true] {
        let graph = TempGraph::new("missing-config-generation");
        if populated {
            graph.write("pages/alpha.md", "- original\n");
        }
        let revision = graph
            .open()
            .metadata()
            .expect("metadata")
            .value()
            .revision();
        let conn = rusqlite::Connection::open(
            Index::open_at(graph.index_path())
                .expect("resolve active index")
                .storage_path()
                .expect("file-backed index"),
        )
        .expect("index");
        conn.execute("UPDATE graph SET config_signature = NULL", [])
            .expect("unknown generation");
        drop(conn);
        let mut opened = Graph::open(graph.options().with_startup_scan(false)).expect("reproject");
        assert_eq!(
            opened.pages().expect("pages").value().len(),
            usize::from(populated)
        );
        if populated {
            assert!(
                opened
                    .metadata()
                    .expect("reprojected revision")
                    .value()
                    .revision()
                    > revision
            );
        }
        let conn = rusqlite::Connection::open(
            Index::open_at(graph.index_path())
                .expect("resolve active index")
                .storage_path()
                .expect("file-backed index"),
        )
        .expect("index");
        assert!(conn
            .query_row("SELECT config_signature IS NOT NULL FROM graph", [], |r| {
                r.get::<_, bool>(0)
            })
            .expect("signature published"));
    }
}

#[test]
fn failed_reprojection_leaves_the_write_unwritten_and_retries_the_generation() {
    let graph = TempGraph::new("failed-config-generation");
    graph.write("journals/2024_01_02.md", "- original\n");
    let state = Arc::new(ScanState::new());
    let mut scanner =
        Graph::open(graph.options().with_scan_state(Arc::clone(&state))).expect("scanner");
    let mut writer = Graph::open(
        OpenOptions::new(graph.graph_root(), graph.root().join("writer.db")).with_scan_state(state),
    )
    .expect("writer");
    change_config(&graph);
    scanner.rescan().expect("new configuration");
    graph.write("pages/broken.md", b"\xff");
    let error = writer
        .append_block(NEW_NAME, "must not land")
        .expect_err("reprojection fails");
    assert_eq!(error.write_state(), WriteState::Unwritten);
    assert_eq!(
        fs::read_to_string(graph.graph_root().join("journals/2024_01_02.md")).expect("journal"),
        "- original\n"
    );
    fs::remove_file(graph.graph_root().join("pages/broken.md")).expect("remove broken page");
    assert_consistent(&graph, &mut writer);
    assert!(Index::open_at(graph.root().join("writer.db"))
        .expect("index")
        .page_file(OLD_NAME)
        .expect("old row")
        .is_none());
}
