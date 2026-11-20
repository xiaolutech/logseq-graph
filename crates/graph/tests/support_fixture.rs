//! Tests for the shared fixture module itself.
//!
//! The other test binaries rely on the fixture to isolate their graphs, so the properties that make
//! that isolation true are asserted here: a unique directory per fixture, parent directories created
//! by writes, and a drop that removes everything the test wrote.

mod support;

use std::fs;

use support::TempGraph;

#[test]
fn every_fixture_gets_a_unique_directory_named_after_the_process() {
    let first = TempGraph::new("same-label");
    let second = TempGraph::new("same-label");

    assert_ne!(
        first.root(),
        second.root(),
        "two fixtures with the same label must not share a directory"
    );
    let name = first
        .root()
        .file_name()
        .expect("the fixture root has a name")
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("logseq-graph-same-label-"),
        "the label must stay in the directory name: {name}"
    );
    assert!(
        name.contains(&std::process::id().to_string()),
        "the directory name must carry the process id: {name}"
    );
    assert!(first.graph_root().is_dir(), "the graph root is created");
    assert!(
        !first
            .index_path()
            .parent()
            .expect("the index file has a parent")
            .exists(),
        "the index directory must not exist before the graph is opened"
    );
}

#[test]
fn writing_creates_the_directories_a_path_names() {
    let graph = TempGraph::new("write");
    graph.write("journals/2026_08_13.md", "- journal\n");
    graph.write_config("{:file/name-format :triple-lowbar}");

    assert_eq!(
        fs::read_to_string(graph.graph_root().join("journals/2026_08_13.md")).expect("read file"),
        "- journal\n"
    );
    assert_eq!(
        fs::read_to_string(graph.graph_root().join("logseq/config.edn")).expect("read config"),
        "{:file/name-format :triple-lowbar}"
    );
}

#[test]
fn dropping_a_fixture_removes_the_graph_and_the_index() {
    let root = {
        let graph = TempGraph::new("cleanup");
        graph.write("pages/nested/alpha.md", "- a\n");
        // Opening the graph creates the index directory, which the drop must remove as well.
        graph.open();
        assert!(graph.index_path().is_file(), "the index file was created");

        let root = graph.root().to_path_buf();
        assert!(root.is_dir());
        root
    };

    assert!(
        !root.exists(),
        "dropping the fixture must remove {}",
        root.display()
    );
}
