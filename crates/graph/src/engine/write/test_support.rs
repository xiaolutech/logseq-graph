use super::TestFault;
use crate::engine::Graph;
use std::fs;
use std::path::PathBuf;

/// A temporary graph whose file and index a fault test can inspect.
pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) graph_root: PathBuf,
    pub(super) index_path: PathBuf,
}

impl Fixture {
    pub(super) fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "logseq-graph-engine-fault-{label}-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let graph_root = root.join("graph");
        fs::create_dir_all(graph_root.join("pages")).expect("create pages");
        Self {
            index_path: root.join("cache/index.db"),
            root,
            graph_root,
        }
    }

    pub(super) fn write(&self, relative: &str, text: &str) {
        fs::write(self.graph_root.join(relative), text).expect("write page");
    }

    pub(super) fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.graph_root.join(relative)).expect("read page")
    }

    pub(super) fn open(&self, fault: Option<TestFault>) -> Graph {
        let mut graph = Graph::open(
            crate::engine::OpenOptions::new(&self.graph_root, &self.index_path)
                .with_startup_scan(true),
        )
        .expect("open graph");
        graph.test_fault = fault;
        graph
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
