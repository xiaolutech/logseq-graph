//! Where the graph write lock lives.
//!
//! The lock has to be one file every writer of the graph agrees on. These tests pin that place down:
//! inside `<root>/.git` for a Git working tree, inside the graph root when there is none, and
//! through the `gitdir:` pointer for a linked worktree or a submodule.
//!
//! The test that motivates all of this is the last one: two handles opened with different `--index`
//! paths must still contend for one lock, because they write the same files.

use std::fs;
use std::path::Path;
use std::time::Duration;

use logseq_graph::engine::{Graph, OpenOptions, WriteError};
use logseq_graph::source::{LockError, WriteLock};

mod support;

use support::TempGraph;

/// Names of the direct children of `dir`, sorted, for the "no file appeared" assertions.
fn child_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read directory")
        .map(|entry| {
            entry
                .expect("list entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn a_git_graph_takes_the_lock_inside_the_git_directory() {
    let graph = TempGraph::new("lock-git-dir");
    graph.write("pages/alpha.md", "- first\n");
    graph.make_dir(".git");
    let before = child_names(&graph.graph_root());

    let mut opened = graph.open();
    opened
        .append_block("alpha", "second")
        .expect("append a block");

    assert!(
        graph.graph_root().join(".git").join("write.lock").exists(),
        "the lock file must be inside .git"
    );
    assert_eq!(
        child_names(&graph.graph_root()),
        before,
        "the graph root itself must gain no file"
    );
    assert!(
        !graph
            .index_path()
            .parent()
            .expect("index has a parent")
            .join("write.lock")
            .exists(),
        "a Git graph must not also hold a lock in the index directory"
    );
}

#[test]
fn without_a_git_directory_the_lock_stays_in_the_graph_root() {
    let graph = TempGraph::new("lock-index-dir");
    graph.write("pages/alpha.md", "- first\n");
    let mut opened = graph.open();
    opened
        .append_block("alpha", "second")
        .expect("append a block");

    assert!(
        graph.graph_root().join("write.lock").exists(),
        "the fallback must follow the graph root"
    );
    assert_eq!(
        child_names(&graph.graph_root()),
        ["pages", "write.lock"],
        "the only extra graph file is the lock"
    );
}

#[test]
fn a_gitdir_pointer_puts_the_lock_in_the_directory_it_names() {
    let graph = TempGraph::new("lock-gitdir-pointer");
    graph.write("pages/alpha.md", "- first\n");
    // A linked worktree or a submodule keeps a pointer file here instead of a directory.
    let real_git_dir = graph.root().join("real-git-dir");
    fs::create_dir_all(&real_git_dir).expect("create the pointed-to directory");
    graph.write(".git", format!("gitdir: {}\n", real_git_dir.display()));

    let mut opened = graph.open();
    opened
        .append_block("alpha", "second")
        .expect("append a block");

    assert!(
        real_git_dir.join("write.lock").exists(),
        "the lock must follow the gitdir pointer"
    );
    assert!(
        !graph
            .index_path()
            .parent()
            .expect("index has a parent")
            .join("write.lock")
            .exists(),
        "the pointer must replace the fallback, not add to it"
    );
}

#[test]
fn a_relative_gitdir_pointer_is_resolved_against_the_graph_root() {
    let graph = TempGraph::new("lock-gitdir-relative");
    graph.write("pages/alpha.md", "- first\n");
    graph.make_dir("real-git-dir");
    graph.write(".git", "gitdir: real-git-dir\n");

    let mut opened = graph.open();
    opened
        .append_block("alpha", "second")
        .expect("append a block");

    assert!(
        graph.graph_root().join("real-git-dir/write.lock").exists(),
        "a relative pointer is relative to the graph root"
    );
}

#[test]
fn a_git_file_that_is_not_a_pointer_degrades_to_the_graph_root() {
    let graph = TempGraph::new("lock-git-garbage");
    graph.write("pages/alpha.md", "- first\n");
    graph.write(".git", "this is not a gitdir pointer\n");

    // Opening must not fail on a repository layout this slice does not understand.
    let mut opened = graph.open();
    opened
        .append_block("alpha", "second")
        .expect("append a block");

    assert!(
        graph.graph_root().join("write.lock").exists(),
        "an unreadable pointer falls back to the graph root"
    );
}

#[test]
fn a_gitdir_pointer_to_a_missing_directory_degrades_to_the_graph_root() {
    let graph = TempGraph::new("lock-gitdir-dangling");
    graph.write("pages/alpha.md", "- first\n");
    let missing = graph.root().join("no-such-git-dir");
    graph.write(".git", format!("gitdir: {}\n", missing.display()));

    let mut opened = graph.open();
    opened
        .append_block("alpha", "second")
        .expect("append a block");

    assert!(
        graph.graph_root().join("write.lock").exists(),
        "a pointer at nothing falls back to the graph root"
    );
}

#[test]
fn two_handles_with_different_index_paths_contend_for_one_lock() {
    assert_different_indexes_share_lock(true);
}

#[test]
fn non_git_handles_with_different_index_paths_contend_for_one_lock() {
    assert_different_indexes_share_lock(false);
}

fn assert_different_indexes_share_lock(git: bool) {
    let graph = TempGraph::new("lock-two-indexes");
    graph.write("pages/alpha.md", "- first\n");
    let lock_dir = if git {
        graph.make_dir(".git");
        graph.graph_root().join(".git")
    } else {
        graph.graph_root()
    };

    let mut first = Graph::open(graph.options()).expect("open the first handle");
    // A second index location, so before the lock followed the graph these two handles held two
    // different locks while writing the same file.
    let other_index = graph.root().join("other-cache").join("index.db");
    let mut second = Graph::open(
        OpenOptions::new(graph.graph_root(), other_index).with_write_lock_wait(Duration::ZERO),
    )
    .expect("open the second handle");

    // The lock the two handles share is the one an external tool working in this tree can see.
    let held = WriteLock::acquire(&lock_dir, None).expect("hold the lock");
    let failure = second
        .append_block("alpha", "from the second handle")
        .expect_err("a held lock must refuse the write");
    drop(held);

    assert!(
        matches!(&failure, WriteError::Lock(LockError::Busy { .. })),
        "expected Busy from a different index path, got {failure:?}"
    );
    assert_eq!(
        String::from_utf8(fs::read(graph.graph_root().join("pages/alpha.md")).expect("read page"))
            .expect("page is UTF-8"),
        "- first\n",
        "a refused write must not touch the file"
    );

    // The first handle is a writer of the same graph, so it takes the same lock and is refused too.
    let held = WriteLock::acquire(&lock_dir, None).expect("hold the lock");
    let failure = first
        .append_block("alpha", "from the first handle")
        .expect_err("the same lock must refuse both handles");
    drop(held);

    assert!(
        matches!(&failure, WriteError::Lock(LockError::Busy { .. })),
        "expected Busy on the first handle too, got {failure:?}"
    );
}
