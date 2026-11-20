//! Fixture helpers shared by the integration tests.
//!
//! This module is compiled into test binaries only, so it exposes no public production API. Every
//! test binary compiles the whole module and uses a subset of it, which is why unused items are
//! allowed here rather than silenced at each item.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use logseq_graph::document::PageDocument;
use logseq_graph::domain::{BlockSelector, GraphMetadata};
use logseq_graph::engine::{EngineError, Graph, OpenOptions};
use logseq_graph::index::{FileBaseline, Index, IndexError};
use logseq_graph::source::{GraphSource, SourceError};

/// Directory under the sandbox root that holds the graph files.
const GRAPH_DIR: &str = "graph";

/// Includes a UTF-8 fixture from the workspace-level `fixtures/document` directory.
///
/// The path is anchored at `CARGO_MANIFEST_DIR`, so a call site names only the fixture file and
/// never repeats a relative hop out of `crates/graph/tests`. Inclusion remains a compile-time
/// `include_str!`; the fixture bytes and their provenance are unchanged.
///
/// ```ignore
/// const QUERY_FIXTURE: &str = document_fixture!("predecessor-todo-query.md");
/// ```
#[allow(unused_macros)]
macro_rules! document_fixture {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/document/",
            $name
        ))
    };
}

#[allow(unused_imports)]
pub(crate) use document_fixture;

/// A temporary sandbox holding one graph root and one index location, removed when it is dropped.
///
/// The layout is fixed, so a test states only which files the graph contains:
///
/// - `<root>/graph` is the graph root: `pages/`, `journals/`, and `logseq/config.edn` live here.
///   [`graph_root`](Self::graph_root) is what `GraphSource::open` and `OpenOptions::new` take.
/// - `<root>/cache/index.db` is the index file. Its directory is deliberately not created here, so
///   opening a [`Graph`] also proves the engine creates that directory.
///
/// The directory name carries the process id and a process-wide serial, so neither two tests in one
/// binary nor two test binaries can land on the same path.
pub(crate) struct TempGraph {
    root: PathBuf,
}

impl TempGraph {
    /// Creates an empty graph root under the system temporary directory.
    ///
    /// `label` only makes a leftover directory identifiable; uniqueness comes from the process id
    /// and the serial. A directory left behind by an interrupted run is removed first, so it cannot
    /// leak into this one.
    pub(crate) fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "logseq-graph-{label}-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(GRAPH_DIR)).expect("create graph root");
        Self { root }
    }

    /// The sandbox root, which contains the graph root and the index location.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The graph root: the directory that holds `pages/`, `journals/`, and `logseq/config.edn`.
    pub(crate) fn graph_root(&self) -> PathBuf {
        self.root.join(GRAPH_DIR)
    }

    /// The index file, inside a directory that does not exist until a graph is opened.
    pub(crate) fn index_path(&self) -> PathBuf {
        self.root.join("cache").join("index.db")
    }

    /// Writes a graph file, creating the parent directories its path names.
    pub(crate) fn write(&self, relative: &str, contents: impl AsRef<[u8]>) {
        let path = self.graph_root().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(path, contents).expect("write file");
    }

    /// Writes `logseq/config.edn`, the configuration every open and every scan re-reads.
    pub(crate) fn write_config(&self, edn: &str) {
        self.write("logseq/config.edn", edn);
    }

    /// Creates an empty directory, for the cases where a file of that name must not be listed.
    pub(crate) fn make_dir(&self, relative: &str) {
        fs::create_dir_all(self.graph_root().join(relative)).expect("create directory");
    }

    /// Removes a graph file, as another process editing the graph would.
    pub(crate) fn remove(&self, relative: &str) {
        fs::remove_file(self.graph_root().join(relative)).expect("remove file");
    }

    /// Renames a graph file, as another process editing the graph would.
    pub(crate) fn rename(&self, from: &str, to: &str) {
        let root = self.graph_root();
        fs::rename(root.join(from), root.join(to)).expect("rename file");
    }

    /// Modification time currently recorded for `relative`.
    pub(crate) fn modified(&self, relative: &str) -> SystemTime {
        fs::metadata(self.graph_root().join(relative))
            .expect("stat file")
            .modified()
            .expect("the platform reports a modification time")
    }

    /// Moves a file's modification time without touching its bytes.
    pub(crate) fn set_modified(&self, relative: &str, modified: SystemTime) {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(self.graph_root().join(relative))
            .expect("open file for timestamps");
        file.set_times(fs::FileTimes::new().set_modified(modified))
            .expect("set modification time");
    }

    /// The options a [`Graph`] over this graph root and index is opened with.
    pub(crate) fn options(&self) -> OpenOptions {
        OpenOptions::new(self.graph_root(), self.index_path())
    }

    /// Opens the graph through the engine, scanning once.
    pub(crate) fn open(&self) -> Graph {
        Graph::open(self.options()).expect("open graph")
    }

    /// Opens the graph expecting a failure.
    pub(crate) fn open_error(&self) -> EngineError {
        match Graph::open(self.options()) {
            Ok(_) => panic!("opening the graph must fail"),
            Err(error) => error,
        }
    }

    /// Opens the graph root through the source layer, which only discovers files.
    pub(crate) fn open_source(&self) -> GraphSource {
        GraphSource::open(self.graph_root()).expect("open graph")
    }

    /// Opens the graph root through the source layer expecting a failure.
    pub(crate) fn source_error(&self) -> SourceError {
        match GraphSource::open(self.graph_root()) {
            Ok(_) => panic!("opening the graph root must fail"),
            Err(error) => error,
        }
    }
}

impl Drop for TempGraph {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The selector the projection holds for the block a preorder path names, read from the index.
///
/// Paths are the addresses these tests state blocks by: `[0]` is the first top-level block, `[0, 1]`
/// its second child, and so on — the same descent the engine used to perform by handle path. A test
/// cannot spell an ephemeral id, because only the index mints one, so it asks the projection which
/// id the block has. A block that states an `id::` answers a durable selector instead, which is the
/// rule the projection applies (ADR-0012).
///
/// The index is opened on its own connection so a test can resolve an address inside the call that
/// writes: the graph handle stays free for the write, and a committed row is visible here exactly as
/// it is to any other process.
pub(crate) fn bound(graph: &TempGraph, page: &str, path: &[u32]) -> BlockSelector {
    let index = Index::open_at(graph.index_path()).expect("open the index the graph wrote");
    let preorder = preorder_of(&index, page, path);
    let blocks = index.page_blocks(page).expect("read the page's blocks");
    let blocks = blocks.expect("the page is projected");
    blocks
        .iter()
        .find(|block| block.preorder() == preorder)
        .unwrap_or_else(|| panic!("page {page:?} has no row at preorder {preorder}"))
        .selector()
        .clone()
}

/// The preorder a path names on `page`, walked one level per element.
fn preorder_of(index: &Index, page: &str, path: &[u32]) -> u32 {
    let tree = index.page_tree(page).expect("read the page tree");
    let tree = tree.expect("the page is projected");
    let Some((root, deeper)) = path.split_first() else {
        panic!("an empty path names no block");
    };
    let mut found = tree
        .blocks()
        .iter()
        .find(|node| node.preorder() == *root)
        .unwrap_or_else(|| panic!("page {page:?} has no top-level block at preorder {root}"));
    for sibling in deeper {
        let parent = found.preorder();
        let Some(next) = found
            .children()
            .iter()
            .find(|node| node.sibling_index() == *sibling)
        else {
            panic!("page {page:?} has no block at sibling {sibling} under preorder {parent}");
        };
        found = next;
    }
    found.preorder()
}

/// The selector the projection holds for the block at `preorder` on `page`.
///
/// An ephemeral id is minted by the index, so a test cannot spell one: it reads the page and takes
/// the address the read answers with, which is exactly what a caller does. A block that states an
/// `id::` answers a durable selector here, so a test that passes any block of a page through this
/// helper writes through whichever identity that block actually has.
pub(crate) fn selector_at(graph: &mut Graph, page: &str, preorder: u32) -> BlockSelector {
    let read = graph.page_blocks(page).expect("query page blocks");
    let blocks = read.present().expect("page is projected");
    blocks
        .iter()
        .find(|block| block.preorder() == preorder)
        .unwrap_or_else(|| panic!("page {page:?} has no block at preorder {preorder}"))
        .selector()
        .clone()
}

/// Block count of a projected page.
pub(crate) fn block_count(graph: &mut Graph, name: &str) -> u32 {
    graph
        .page_tree(name)
        .expect("query page tree")
        .present()
        .expect("page is projected")
        .page()
        .block_count()
}

/// Project `document` as the page `name`, with the baseline of the file it would have come from.
///
/// A page row carries the path of the file it was projected from and the observation of that file, so
/// this helper materializes the document's source as a real page file and observes it: the crate
/// offers no synthetic `FileMetadata`, and a baseline nobody observed is a row no run could have
/// produced. The result is passed through, because a test that expects the projection to fail asserts
/// on the error.
pub(crate) fn project(
    index: &mut Index,
    name: &str,
    document: &PageDocument,
) -> Result<GraphMetadata, IndexError> {
    let source = document.to_source();
    let graph = TempGraph::new("project");
    graph.write(&format!("pages/{name}.md"), &source);
    let file = graph
        .open_source()
        .discover()
        .expect("discover the page file")
        .into_iter()
        .next()
        .expect("the page file discovery returns");
    index.replace_page_with_baseline(
        file.relative_path(),
        name,
        file.journal_date(),
        document,
        FileBaseline::new(file.metadata(), fingerprint(&source)),
    )
}

/// The content fingerprint of `text`, mirroring the source layer's change detector.
///
/// `source::content_fingerprint` is crate-private, so an integration test recomputes the same FNV-1a
/// values here. The production algorithm is pinned by its own unit test; nothing in these tests
/// compares a baseline against a file, and a test that does uses a real graph instead.
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
