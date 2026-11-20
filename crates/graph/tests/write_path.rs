//! The write path: one page, one appended block, through the engine facade.
//!
//! Every test writes a graph under the system temporary directory, opens it through [`Graph`], and
//! asserts on the bytes on disk as well as on the projection, because the two are what a write has
//! to keep in step. The tests that matter most here are the ones about what must *not* happen: a
//! write must not create a page, must not replace a file it refused to edit, must not write while
//! another writer holds the lock, and must not overwrite a change it never observed.

use std::fs;
use std::time::Duration;

use logseq_graph::engine::{EngineError, Freshness, WriteError};
use logseq_graph::source::WriteLock;

mod support;

use support::TempGraph;

/// Bytes of a graph file, as they are on disk.
fn read_bytes(graph: &TempGraph, relative: &str) -> Vec<u8> {
    fs::read(graph.graph_root().join(relative)).expect("read graph file")
}

/// Text of a graph file, as it is on disk.
fn read_text(graph: &TempGraph, relative: &str) -> String {
    String::from_utf8(read_bytes(graph, relative)).expect("graph file is UTF-8")
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

/// Block count of a page, read without scanning the files again.
///
/// `LastScan` is the point: a write must leave the index readable at its own result, so a read that
/// does not scan has to see the appended block.
fn block_count_without_scan(graph: &mut logseq_graph::engine::Graph, page: &str) -> u32 {
    graph
        .reads(logseq_graph::engine::Freshness::LastScan)
        .page_tree(page)
        .expect("query the page tree")
        .present()
        .expect("the page is projected")
        .page()
        .block_count()
}

#[test]
fn an_appended_block_reaches_the_file_and_the_next_read() {
    let graph = TempGraph::new("append-visible");
    graph.write("pages/alpha.md", "- first\n");
    let mut opened = graph.open();

    opened
        .append_block("alpha", "second")
        .expect("append a block");

    assert_eq!(read_text(&graph, "pages/alpha.md"), "- first\n- second\n");
    // Read-your-writes: no scan after the append, so the index itself already holds the block.
    assert_eq!(block_count_without_scan(&mut opened, "alpha"), 2);
}

#[test]
fn one_append_advances_the_revision_exactly_once() {
    let graph = TempGraph::new("append-revision");
    graph.write("pages/alpha.md", "- first\n");
    let mut opened = graph.open();

    // `LastScan` does not scan, so this reads the revision the open committed.
    let before = opened.metadata().expect("read metadata").meta().revision();
    let appended = opened
        .append_block("alpha", "second")
        .expect("append a block");

    assert_eq!(appended.meta().revision(), before + 1);
    assert_eq!(
        appended.value().revision(),
        before + 1,
        "the page row must carry the revision of the write that produced it"
    );
    assert_eq!(
        opened.metadata().expect("read metadata").meta().revision(),
        before + 1,
        "the scan before the write changed nothing, so only the replacement may advance"
    );
    assert_eq!(appended.value().block_count(), 2);
}

#[test]
fn two_appends_keep_both_blocks_in_order() {
    let graph = TempGraph::new("append-twice");
    graph.write("pages/alpha.md", "- first\n");
    let mut opened = graph.open();

    opened
        .append_block("alpha", "second")
        .expect("first append");
    opened
        .append_block("alpha", "third")
        .expect("second append");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n- second\n- third\n"
    );
    assert_eq!(block_count_without_scan(&mut opened, "alpha"), 3);
}

#[test]
fn a_missing_page_is_an_error_and_creates_no_file() {
    let graph = TempGraph::new("append-missing");
    graph.write("pages/other.md", "- a block\n");
    let mut opened = graph.open();
    let before = file_names(&graph, "pages");

    let failure = opened
        .append_block("absent", "content")
        .expect_err("appending to a missing page must fail");

    assert!(
        matches!(&failure, WriteError::PageNotFound { page } if page == "absent"),
        "expected PageNotFound, got {failure:?}"
    );
    assert_eq!(
        file_names(&graph, "pages"),
        before,
        "a write must never create a page file"
    );
}

#[test]
fn the_page_name_is_matched_without_case() {
    let graph = TempGraph::new("append-case");
    graph.write("pages/Foo.md", "- first\n");
    let mut opened = graph.open();

    opened
        .append_block("foo", "second")
        .expect("append a block");

    assert_eq!(read_text(&graph, "pages/Foo.md"), "- first\n- second\n");
    assert_eq!(
        file_names(&graph, "pages"),
        vec!["Foo.md".to_owned()],
        "the existing file must be written, not a second one next to it"
    );
}

#[test]
fn the_edit_is_applied_to_the_bytes_on_disk_even_when_the_stat_lied() {
    // The write reads the file it is about to edit, so an external change survives even when no stat
    // can see it. That is the correctness the write owes, and it does not depend on a scan: the edit
    // is a transformation of the bytes on disk, never of an index row.
    let graph = TempGraph::new("append-every-file");
    graph.write("pages/alpha.md", "- first\n");
    let mut opened = graph.open();

    // An external edit that keeps both the size and the modification time, as `rsync -a`, `cp -p`,
    // and a sync client can. A projected row still describes the old content.
    let relative = "pages/alpha.md";
    let modified = graph.modified(relative);
    let size = fs::metadata(graph.graph_root().join(relative))
        .expect("stat the file")
        .len();
    let external = "- FIFTH\n";
    assert_eq!(
        external.len() as u64,
        size,
        "the replacement keeps the size"
    );
    graph.write(relative, external);
    graph.set_modified(relative, modified);

    opened
        .append_block("alpha", "second")
        .expect("append a block");

    let written = read_text(&graph, relative);
    assert!(
        written.contains("- FIFTH"),
        "the external change must survive the write, got {written:?}"
    );
    assert!(
        written.contains("- second"),
        "the appended block must be there too, got {written:?}"
    );
}

#[test]
fn a_write_does_not_reproject_a_page_it_does_not_write() {
    // A write re-establishes one page: the one whose file it replaces. A page it does not touch keeps
    // whatever the last scan projected, even when the file behind it changed beyond what a stat can
    // see. Refreshing other pages is a read's job, and it is the caller's to ask for.
    let graph = TempGraph::new("append-other-page");
    graph.write("pages/alpha.md", "- first\n");
    graph.write("pages/beta.md", "- one\n");
    let mut opened = graph.open();

    // Same size, same modification time: a stat-based scan cannot see this, a content scan can.
    // The replacement text differs in a way `search` can tell apart (it folds case, so a case-only
    // change would match either way).
    let relative = "pages/beta.md";
    let modified = graph.modified(relative);
    graph.write(relative, "- two\n");
    graph.set_modified(relative, modified);

    opened
        .append_block("alpha", "second")
        .expect("append a block");

    // Read without scanning: the write is now the only thing that has run since the open, and it did
    // not project beta at all, so the old text is still what the index holds.
    let old = opened
        .reads(Freshness::LastScan)
        .search("one", 10)
        .expect("search")
        .into_value();
    assert_eq!(
        old.len(),
        1,
        "the write must not have reprojected the page it did not touch, got {old:?}"
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

    // Asking for it explicitly is what refreshes the rest of the graph.
    let refreshed = opened
        .reads(Freshness::VerifyContent)
        .search("two", 10)
        .expect("search")
        .into_value();
    assert_eq!(
        refreshed.len(),
        1,
        "a content scan sees the change, got {refreshed:?}"
    );
}

#[test]
fn a_write_does_not_read_the_other_files_of_the_graph() {
    // The structural proof that the write path stopped scanning: a file it has no business reading is
    // made unreadable, and the write still succeeds. A scan of any kind would fail here, and it would
    // fail before the write reached the page it edits.
    use std::os::unix::fs::PermissionsExt as _;

    let graph = TempGraph::new("append-unreadable-other");
    graph.write("pages/alpha.md", "- first\n");
    graph.write("pages/beta.md", "- one\n");
    let mut opened = graph.open();

    let beta = graph.graph_root().join("pages/beta.md");
    fs::set_permissions(&beta, fs::Permissions::from_mode(0o000)).expect("make beta unreadable");

    let written = opened.append_block("alpha", "second");

    // Restored before the assertion, so a failure here still leaves a temporary directory that can be
    // removed.
    fs::set_permissions(&beta, fs::Permissions::from_mode(0o644)).expect("restore beta");

    written.expect("the write must not care about a file it does not touch");
    assert_eq!(read_text(&graph, "pages/alpha.md"), "- first\n- second\n");
    assert_eq!(
        read_text(&graph, "pages/beta.md"),
        "- one\n",
        "the unreadable page must not have been rewritten"
    );
}

#[test]
fn a_write_reports_the_staleness_of_the_last_scan_and_nothing_more() {
    let graph = TempGraph::new("append-stale");
    graph.write("pages/alpha.md", "- first\n");
    let mut opened = graph.open();

    // The open scanned successfully, so there is nothing the write has to disclaim.
    let fresh = opened
        .append_block("alpha", "second")
        .expect("append a block");
    assert!(
        !fresh.meta().is_stale(),
        "a write on a graph whose last scan succeeded is not stale"
    );

    // A file the scanner cannot decode makes the next scan fail, and the failure stays recorded on the
    // handle. The write does not scan, so it cannot clear it, and it must not claim the rest of the
    // graph is fresh.
    graph.write("pages/damaged.md", [0xffu8, 0xfe]);
    let failure = opened
        .reads(Freshness::CheckNow)
        .pages()
        .expect_err("the scan must fail on the undecodable file");
    assert!(
        matches!(&failure, EngineError::Source(_)),
        "expected a source error from the scan, got {failure:?}"
    );

    let written = opened
        .append_block("alpha", "third")
        .expect("append a block");
    assert!(
        written.meta().is_stale(),
        "the write did not re-establish the freshness of the graph, so it must say so"
    );
    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n- second\n- third\n"
    );
}

#[test]
fn a_refused_edit_leaves_the_file_byte_for_byte_unchanged() {
    let graph = TempGraph::new("append-refused");
    // The file ends inside an unclosed BEGIN region, so the appended line would become its content
    // rather than a block of its own.
    graph.write("pages/alpha.md", "- first\n#+BEGIN_SRC\nunclosed\n");
    let mut opened = graph.open();
    let before = read_bytes(&graph, "pages/alpha.md");

    let failure = opened
        .append_block("alpha", "second")
        .expect_err("the edit must be refused");

    assert!(
        matches!(&failure, WriteError::Edit(_)),
        "expected an edit error, got {failure:?}"
    );
    assert_eq!(
        read_bytes(&graph, "pages/alpha.md"),
        before,
        "a refused edit must not touch the file"
    );
}

#[test]
fn a_held_lock_refuses_the_write_without_touching_the_file() {
    let graph = TempGraph::new("append-locked");
    graph.write("pages/alpha.md", "- first\n");
    // Zero wait, so the refusal is observed without waiting out the default budget.
    let mut opened =
        logseq_graph::engine::Graph::open(graph.options().with_write_lock_wait(Duration::ZERO))
            .expect("open the graph");
    let before = read_bytes(&graph, "pages/alpha.md");

    // The graph root is where the engine takes the lock; holding it here is what another writer
    // would look like.
    let held = WriteLock::acquire(&graph.graph_root(), None).expect("hold the lock");
    let failure = opened
        .append_block("alpha", "second")
        .expect_err("a held lock must refuse the write");
    drop(held);

    assert!(
        matches!(&failure, WriteError::Lock(_)),
        "expected a lock error, got {failure:?}"
    );
    assert_eq!(
        read_bytes(&graph, "pages/alpha.md"),
        before,
        "a refused write must not touch the file"
    );
}

#[test]
fn the_lock_is_released_when_the_write_returns() {
    let graph = TempGraph::new("append-lock-released");
    graph.write("pages/alpha.md", "- first\n");
    // Zero wait makes this a real assertion: the second append succeeds only if the first released
    // the lock before it returned, not merely after some timeout.
    let mut opened =
        logseq_graph::engine::Graph::open(graph.options().with_write_lock_wait(Duration::ZERO))
            .expect("open the graph");

    opened
        .append_block("alpha", "second")
        .expect("first append");
    opened
        .append_block("alpha", "third")
        .expect("second append");

    assert_eq!(
        read_text(&graph, "pages/alpha.md"),
        "- first\n- second\n- third\n"
    );
}

#[test]
fn the_written_bytes_keep_the_bom_crlf_and_a_missing_final_newline() {
    let graph = TempGraph::new("append-bytes");
    // A byte order mark, CRLF endings, and no final newline: the three things a rewrite can silently
    // normalize. The assertions are on the bytes on disk, not on the edit's return value, because
    // what survives the file replacement is the part that matters.
    graph.write("pages/alpha.md", "\u{feff}- first\r\n- last");
    let mut opened = graph.open();

    opened
        .append_block("alpha", "appended")
        .expect("append a block");

    let written = read_bytes(&graph, "pages/alpha.md");
    assert_eq!(
        written,
        "\u{feff}- first\r\n- last\r\n- appended".as_bytes(),
        "the written bytes must keep the BOM, use CRLF, and stay without a final newline"
    );
}
