//! Creating a page: one new file under `pages/`, one page row, one baseline.
//!
//! Every test writes a graph under the system temporary directory, opens it through [`Graph`], and
//! asserts on the bytes on disk as well as on the projection. The tests that matter most are the ones
//! about what must *not* happen: a create must not replace an existing file, must not leave a second
//! file for one page name, and must not leave an orphan file behind when the index refuses it.

use std::fs;
use std::path::Path;

use logseq_graph::document::PageDocument;
use logseq_graph::engine::{Freshness, WriteError};
use logseq_graph::index::Index;

mod support;

use support::TempGraph;

/// Bytes of a graph file, as they are on disk.
fn read_bytes(graph: &TempGraph, relative: &str) -> Vec<u8> {
    fs::read(graph.graph_root().join(relative)).expect("read graph file")
}

/// Names of the files in `relative`, sorted, for the "no file appeared" assertions.
fn file_names(graph: &TempGraph, relative: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(graph.graph_root().join(relative))
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
fn a_created_page_is_an_empty_file_with_no_blocks() {
    let graph = TempGraph::new("create-empty");
    graph.write("pages/existing.md", "- one\n");
    let mut opened = graph.open();

    let created = opened.create_page("Fresh page").expect("create a page");

    assert_eq!(created.value().name(), "Fresh page");
    assert_eq!(
        created.value().block_count(),
        0,
        "a page created from an empty file has no blocks"
    );
    assert_eq!(
        read_bytes(&graph, "pages/Fresh page.md"),
        b"",
        "the file is empty, not a page with a synthetic block"
    );
    assert!(
        file_names(&graph, "pages").contains(&"Fresh page.md".to_owned()),
        "the page lives under pages/: {:?}",
        file_names(&graph, "pages")
    );

    // The create is what an append builds on: a page with no block is still a page.
    let appended = opened
        .append_block("Fresh page", "first block")
        .expect("append to the created page");
    assert_eq!(appended.value().block_count(), 1);
    assert_eq!(
        String::from_utf8(read_bytes(&graph, "pages/Fresh page.md")).expect("utf-8"),
        "- first block",
        // No trailing newline: the file was empty, and an edit keeps the file's missing final
        // newline instead of inventing one.
    );
}

#[test]
fn creating_an_existing_page_fails_and_keeps_its_bytes() {
    let graph = TempGraph::new("create-exists");
    graph.write("pages/Taken.md", "- original\n");
    let mut opened = graph.open();
    let before = read_bytes(&graph, "pages/Taken.md");

    let error = opened
        .create_page("Taken")
        .expect_err("a create never replaces a file");

    match &error {
        WriteError::PageExists { page, path } => {
            assert_eq!(page, "Taken");
            assert!(
                path.ends_with("pages/Taken.md"),
                "the error names the file that holds the page: {}",
                path.display()
            );
        }
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(
        read_bytes(&graph, "pages/Taken.md"),
        before,
        "the existing bytes are untouched"
    );
    assert_eq!(
        file_names(&graph, "pages"),
        vec!["Taken.md".to_owned()],
        "no second file appeared"
    );
}

#[test]
fn a_case_different_name_is_the_same_page_and_creates_no_file() {
    // `page_name_key` folds case, so `foo` and `Foo` are one page to every read. A create that wrote
    // a second file would make discovery refuse the graph for a duplicate name.
    let graph = TempGraph::new("create-case");
    graph.write("pages/Foo.md", "- one\n");
    let mut opened = graph.open();

    let error = opened
        .create_page("foo")
        .expect_err("the page already exists under another spelling");

    assert!(
        matches!(error, WriteError::PageExists { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(
        file_names(&graph, "pages"),
        vec!["Foo.md".to_owned()],
        "no second file was created"
    );
}

#[test]
fn a_slash_in_the_name_becomes_a_namespace_separator() {
    let graph = TempGraph::new("create-namespace");
    let mut opened = graph.open();

    let created = opened.create_page("a/b").expect("create a nested name");

    assert_eq!(created.value().name(), "a/b");
    assert_eq!(
        file_names(&graph, "pages"),
        vec!["a___b.md".to_owned()],
        "the slash is written as three underscores, not as a directory"
    );
    assert_eq!(read_bytes(&graph, "pages/a___b.md"), b"");

    // The name reads back through discovery, which is the direction that has to agree with Logseq.
    let found = opened
        .reads(Freshness::CheckNow)
        .pages()
        .expect("list pages")
        .into_value();
    assert!(
        found.iter().any(|page| page.name() == "a/b"),
        "the created page is discoverable as `a/b`: {found:?}"
    );
}

#[test]
fn underscore_names_beside_a_slash_stay_distinct_and_discoverable() {
    // The separator is spelled `___`, so a name's own underscores beside it used to be swallowed on
    // the way back. The encoder now escapes every literal underscore when a name holds a slash,
    // which has to leave the two ambiguous names as two pages, not one.
    let graph = TempGraph::new("create-namespace-underscores");
    let mut opened = graph.open();

    opened.create_page("a_/b").expect("create a_/b");
    opened.create_page("a/_b").expect("create a/_b");

    assert_eq!(
        file_names(&graph, "pages"),
        vec!["a%5F___b.md".to_owned(), "a___%5Fb.md".to_owned()],
        "each name is written as its own exact stem"
    );

    let found = opened
        .reads(Freshness::CheckNow)
        .pages()
        .expect("list pages")
        .into_value();
    for name in ["a_/b", "a/_b"] {
        assert!(
            found.iter().any(|page| page.name() == name),
            "the created page is discoverable as `{name}`: {found:?}"
        );
    }
}

#[test]
fn a_name_with_empty_namespace_segments_is_refused_and_creates_no_file() {
    // `a//b` displays as `a/b` in Logseq, whose `make-valid-namespaces` removes empty segments.
    // Writing a file for the name as given would create a page nothing can find again under
    // `a//b`, so the create is refused instead of normalized.
    let graph = TempGraph::new("create-empty-namespace");
    graph.write("pages/existing.md", "- one\n");
    let mut opened = graph.open();

    let error = opened
        .create_page("a//b")
        .expect_err("a non-canonical name is refused");

    match error {
        WriteError::PageNameNotEncodable { name, .. } => assert_eq!(name, "a//b"),
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(
        file_names(&graph, "pages"),
        vec!["existing.md".to_owned()],
        "no file was created"
    );
}

#[test]
fn a_stale_index_that_holds_the_path_is_refused_and_leaves_no_orphan_file() {
    // The index can be behind the files: a row can still name a path whose file another process
    // removed. Step 3 then finds no row for the requested name, and the file open succeeds because
    // there is no file. The insert is what meets the old row, through `pages.relative_path` being
    // unique, and the file this call created has to be removed again — a page file nothing projects
    // would make the next scan see two files for one page name and refuse the whole graph.
    let graph = TempGraph::new("create-stale");
    graph.write("pages/Kept.md", "- one\n");
    let mut opened = graph.open();

    // Reproduce an index that names this path as another page: the page row is replaced by one for
    // `Other`, and then the file disappears without a scan. `Kept` is now in no row and `Other`
    // claims a path that holds nothing.
    let mut index = Index::open_at(graph.index_path()).expect("open the index directly");
    let baseline = index
        .file_baseline(Path::new("pages/Kept.md"))
        .expect("read the baseline")
        .expect("the open projected this file");
    index.remove_page("Kept").expect("remove the page row");
    index
        .replace_page_with_baseline(
            Path::new("pages/Kept.md"),
            "Other",
            None,
            &PageDocument::parse(""),
            baseline,
        )
        .expect("project the path as another page");
    drop(index);
    graph.remove("pages/Kept.md");

    let error = opened
        .create_page("Kept")
        .expect_err("the index still claims this path");

    assert!(
        matches!(error, WriteError::PageExists { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(
        file_names(&graph, "pages"),
        Vec::<String>::new(),
        "the file this call created was removed again, so no orphan is left behind"
    );
}

#[test]
fn a_create_does_not_reproject_a_page_it_does_not_write() {
    // A create re-establishes the one page it makes. A page it does not touch keeps whatever the last
    // scan projected, even when its file changed beyond what a stat can see: refreshing other pages
    // is a read's job.
    let graph = TempGraph::new("create-other-page");
    graph.write("pages/alpha.md", "- first\n");
    graph.write("pages/beta.md", "- one\n");
    let mut opened = graph.open();

    let relative = "pages/beta.md";
    let modified = graph.modified(relative);
    graph.write(relative, "- two\n");
    graph.set_modified(relative, modified);

    opened.create_page("gamma").expect("create a page");

    // No scan ran, so the old text is still what the index holds for beta.
    let old = opened
        .reads(Freshness::LastScan)
        .search("one", 10)
        .expect("search")
        .into_value();
    assert_eq!(
        old.len(),
        1,
        "the create must not have reprojected the page it did not write, got {old:?}"
    );
    let unseen = opened
        .reads(Freshness::LastScan)
        .search("two", 10)
        .expect("search")
        .into_value();
    assert!(
        unseen.is_empty(),
        "the external change must stay unobserved until a read asks for it, got {unseen:?}"
    );

    // Asking for it explicitly is what refreshes the rest of the graph, and the created page is part
    // of what a scan then sees.
    let pages = opened
        .reads(Freshness::VerifyContent)
        .pages()
        .expect("list pages")
        .into_value();
    assert!(
        pages.iter().any(|page| page.name() == "gamma"),
        "the created page is in the index: {pages:?}"
    );
}
