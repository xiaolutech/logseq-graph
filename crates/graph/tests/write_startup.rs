//! Startup-scan behaviour for a handle that only writes.
//!
//! `Graph::open` scans by default, because the scan is what makes a fresh handle answer a read.
//! With matching configuration generations, a write does not need it: it resolves the one file
//! it edits from the index and the files. A missing or changed signature forces full reprojection. These
//! tests pin what is lost (immediate readability) and what is not (the write still lands, and a page
//! the files no longer hold is still refused).

use std::fs;
use std::path::PathBuf;

use logseq_graph::engine::{Freshness, Graph, OpenOptions, WriteError};
use logseq_graph::index::Index;

mod support;

use support::TempGraph;

#[test]
fn a_write_opens_without_scanning_and_projects_only_the_page_it_writes() {
    let graph = TempGraph::new("write-no-scan");
    graph.write("pages/alpha.md", "- a\n");
    drop(graph.open());
    graph.write("pages/beta.md", "- not scanned\n");

    let mut opened = Graph::open(graph.options().with_startup_scan(false)).expect("open the graph");

    // The matching configuration permits skipping the scan: beta remains unprojected.
    let pages = opened
        .reads(Freshness::LastScan)
        .pages()
        .expect("read pages");
    assert_eq!(pages.value().len(), 1);
    assert_eq!(pages.value()[0].name(), "alpha");

    // The write touches only alpha; beta stays unprojected.
    opened
        .append_block("alpha", "appended without a startup scan")
        .expect("append to a page no scan has projected yet");

    let pages = opened
        .reads(Freshness::LastScan)
        .pages()
        .expect("read pages");
    assert_eq!(pages.value().len(), 1, "the write projected its page");
    assert_eq!(pages.value()[0].name(), "alpha");
    assert_eq!(
        pages.value()[0].block_count(),
        2,
        "the appended block is in the index the write committed"
    );

    let blocks = opened
        .reads(Freshness::LastScan)
        .page_blocks("alpha")
        .expect("read page blocks")
        .into_value()
        .expect("alpha is projected");
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[1].text(), "appended without a startup scan");
}

#[test]
fn the_default_still_scans_on_open() {
    let graph = TempGraph::new("default-scan");

    // The same option set the read commands use, without naming the switch, must ask for the scan:
    // every read path relies on it, so a default of `false` would change what each of them answers.
    assert!(OpenOptions::new(graph.graph_root(), graph.index_path()).startup_scan());
}

/// A journal is named through two formats, so the same path can denote a different page.
///
/// `journals/2024-04-23.md` becomes a date through `:journal/file-name-format` and a page name
/// through `:journal/page-title-format`. Changing only the second one renames the page without
/// touching the file, which leaves the index holding a `pages` row that still names the old page and
/// that same path. A write that trusted the row's path alone would append to a page the caller never
/// named; it has to notice that the path now decodes to another name and fall back to discovery,
/// which finds no page under the requested name.
#[test]
fn a_path_that_now_decodes_to_another_page_is_not_written() {
    let graph = TempGraph::new("renamed-journal");
    graph.write_config(
        "{:journal/file-name-format \"yyyy-MM-dd\"\n \
          :journal/page-title-format \"yyyy-MM-dd\"}\n",
    );
    graph.write("journals/2024-04-23.md", "- a journal entry\n");

    let mut first = graph.open();
    first
        .append_block("2024-04-23", "one more entry")
        .expect("append under the format the graph was indexed with");
    drop(first);
    let before = fs::read_to_string(graph.graph_root().join("journals/2024-04-23.md"))
        .expect("read the journal file");

    // Only the title format changes: the file name still decodes to the day 2024-04-23, but the
    // page it denotes is now `2024_04_23`, so the stored row and the file disagree.
    graph.write_config(
        "{:journal/file-name-format \"yyyy-MM-dd\"\n \
          :journal/page-title-format \"yyyy_MM_dd\"}\n",
    );

    let mut reopened = Graph::open(graph.options().with_startup_scan(false))
        .expect("open the graph without a scan");
    match reopened.append_block("2024-04-23", "must not land") {
        Err(WriteError::PageNotFound { page }) => assert_eq!(page, "2024-04-23"),
        other => panic!("expected PageNotFound, got {other:?}"),
    }

    assert_eq!(
        fs::read_to_string(graph.graph_root().join("journals/2024-04-23.md"))
            .expect("read the journal file"),
        before,
        "a refused append leaves the bytes of the renamed page alone"
    );
    assert_eq!(
        fs::read_dir(graph.graph_root().join("journals"))
            .expect("list the journals directory")
            .count(),
        1,
        "a refused append creates no file"
    );
    let index = Index::open_at(graph.index_path()).expect("open the index directly");
    assert!(index
        .page_file("2024-04-23")
        .expect("old page row")
        .is_none());
    assert_eq!(
        index.page_file("2024_04_23").expect("read the page row"),
        Some(PathBuf::from("journals/2024-04-23.md")),
        "opening reprojects the renamed journal before the refused write"
    );
}

/// A stored path the current configuration cannot decode fails the write closed.
///
/// `:journal/file-name-format` decides whether the path is a journal name at all, so changing it can
/// make the row's path undecodable. The source reports that as an error rather than as "no such
/// page", and the write propagates it: discovery would refuse the same file for the same reason, so
/// there is no page to fall back to. Either way nothing is created, modified, or re-projected.
#[test]
fn a_path_the_current_format_cannot_decode_fails_the_write_closed() {
    let graph = TempGraph::new("journal-format");
    graph.write_config(
        "{:journal/file-name-format \"yyyy-MM-dd\"\n \
          :journal/page-title-format \"yyyy-MM-dd\"}\n",
    );
    graph.write("journals/2024-04-23.md", "- a journal entry\n");

    let mut first = graph.open();
    first
        .append_block("2024-04-23", "one more entry")
        .expect("append under the format the graph was indexed with");
    drop(first);
    let before = fs::read_to_string(graph.graph_root().join("journals/2024-04-23.md"))
        .expect("read the journal file");

    graph.write_config(
        "{:journal/file-name-format \"yyyy_MM_dd\"\n \
          :journal/page-title-format \"yyyy_MM_dd\"}\n",
    );

    let error = Graph::open(graph.options().with_startup_scan(false))
        .expect_err("opening must refuse a configuration whose paths cannot decode");

    assert!(
        matches!(error, logseq_graph::engine::EngineError::Source(_)),
        "the source refuses the name instead of reporting the page absent: {error:?}"
    );
    assert_eq!(
        fs::read_to_string(graph.graph_root().join("journals/2024-04-23.md"))
            .expect("read the journal file"),
        before,
        "a refused append leaves the bytes alone"
    );
    assert_eq!(
        fs::read_dir(graph.graph_root().join("journals"))
            .expect("list the journals directory")
            .count(),
        1,
        "a refused append creates no file"
    );
    let index = Index::open_at(graph.index_path()).expect("open the index directly");
    assert_eq!(
        index.page_file("2024-04-23").expect("read the page row"),
        Some(PathBuf::from("journals/2024-04-23.md")),
        "a refused write leaves the row alone"
    );
}
