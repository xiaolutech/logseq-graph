//! Discovery and read tests over temporary graph directories.
//!
//! Every test builds its own graph under the system temporary directory, so nothing depends on a
//! checked-out real graph, on absolute fixture paths, or on network access.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use logseq_graph::document::parse;
use logseq_graph::domain::PageKind;
use logseq_graph::source::{GraphSource, SourceError};

mod support;

use support::TempGraph;

/// Restores the process working directory when it is dropped.
struct CurrentDirGuard {
    previous: PathBuf,
}

impl CurrentDirGuard {
    fn capture() -> Self {
        Self {
            previous: std::env::current_dir().expect("read working directory"),
        }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous);
    }
}

/// Relative paths of the discovered files, in discovery order.
fn relative_paths(graph: &GraphSource) -> Vec<String> {
    graph
        .discover()
        .expect("discover pages")
        .iter()
        .map(|page| page.relative_path().to_string_lossy().replace('\\', "/"))
        .collect()
}

#[test]
fn discovery_orders_pages_before_journals_and_skips_non_pages() {
    let graph = TempGraph::new("order");
    graph.write_config("{:file/name-format :triple-lowbar}");
    graph.write("pages/zeta.md", "- zeta\n");
    graph.write("pages/Alpha.md", "- alpha\n");
    graph.write("pages/notes.org", "* org page\n");
    graph.write("pages/.hidden.md", "- hidden\n");
    graph.write("pages/assets/logo.png", "png");
    graph.make_dir("pages/dir.md");
    graph.write("journals/2026_08_14.md", "- journal\n");
    graph.write("journals/2020_01_01.md", "- older\n");
    graph.write("journals/2020_01_02.org", "* org journal\n");
    graph.write("README.md", "# not a page\n");
    graph.write("scratch.md", "- not a page either\n");

    let source = graph.open_source();
    assert_eq!(
        relative_paths(&source),
        vec![
            "pages/Alpha.md",
            "pages/zeta.md",
            "journals/2020_01_01.md",
            "journals/2026_08_14.md",
        ]
    );

    let pages = source.discover().expect("discover pages");
    assert_eq!(pages[0].kind(), PageKind::Page);
    assert_eq!(pages[1].kind(), PageKind::Page);
    assert_eq!(pages[2].kind(), PageKind::Journal);
    assert_eq!(pages[3].kind(), PageKind::Journal);
}

#[test]
fn discovery_is_repeatable() {
    let graph = TempGraph::new("repeatable");
    graph.write("pages/b.md", "- b\n");
    graph.write("pages/a.md", "- a\n");
    graph.write("pages/c.md", "- c\n");

    let source = graph.open_source();
    let first = source.discover().expect("first discovery");
    let second = source.discover().expect("second discovery");
    assert_eq!(first, second);
}

#[test]
fn triple_lowbar_decodes_namespaces_and_percent_escapes() {
    let graph = TempGraph::new("decode");
    graph.write_config("{:file/name-format :triple-lowbar}");
    graph.write("pages/setting___enable journals.md", "- setting\n");
    graph.write("pages/New to Logseq%3F.md", "- question\n");
    graph.write("pages/term___a%2Fb.md", "- nested\n");

    let source = graph.open_source();
    let pages = source.discover().expect("discover pages");
    let names: Vec<&str> = pages.iter().map(|page| page.name()).collect();
    assert_eq!(
        names,
        vec!["New to Logseq?", "setting/enable journals", "term/a/b",]
    );
    assert_eq!(pages[1].comparison_name(), "setting/enable journals");
}

#[test]
fn comparison_name_lowercases_unicode_and_keeps_the_original() {
    let graph = TempGraph::new("comparison");
    graph.write("pages/Ärger ÜBER.md", "- text\n");
    graph.write("pages/Straße.md", "- text\n");

    let source = graph.open_source();
    let pages = source.discover().expect("discover pages");
    assert_eq!(pages[0].name(), "Straße");
    assert_eq!(pages[0].comparison_name(), "straße");
    assert_eq!(pages[1].name(), "Ärger ÜBER");
    assert_eq!(pages[1].comparison_name(), "ärger über");
}

#[test]
fn read_text_preserves_bom_line_endings_and_missing_final_newline() {
    let graph = TempGraph::new("bytes");
    let bytes = "\u{feff}- one\r\n- two\n- three\r\n- four".as_bytes();
    graph.write("pages/mixed.md", bytes);

    let source = graph.open_source();
    let page = source.discover().expect("discover pages").remove(0);
    let text = page.read_text().expect("read page");

    assert_eq!(text.as_bytes(), bytes);
    assert_eq!(parse(&text).to_source(), text);
    assert!(parse(&text).has_bom());
}

#[test]
fn non_utf8_file_is_an_error_not_an_empty_page() {
    let graph = TempGraph::new("non-utf8");
    graph.write("pages/broken.md", [b'-', b' ', 0xff, b'\n']);

    let source = graph.open_source();
    let page = source.discover().expect("discover pages").remove(0);
    let error = page.read_text().expect_err("invalid UTF-8 is rejected");
    match error {
        SourceError::NotUtf8 {
            valid_up_to, len, ..
        } => {
            assert_eq!(valid_up_to, 2);
            assert_eq!(len, 4);
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn read_failure_after_discovery_is_reported() {
    let graph = TempGraph::new("vanished");
    graph.write("pages/gone.md", "- gone\n");

    let source = graph.open_source();
    let page = source.discover().expect("discover pages").remove(0);
    fs::remove_file(page.path()).expect("remove file");

    let error = page.read_text().expect_err("missing file is an error");
    match error {
        SourceError::Io { source, .. } => assert_eq!(source.kind(), io::ErrorKind::NotFound),
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn metadata_records_the_size_needed_for_change_detection() {
    let graph = TempGraph::new("metadata");
    graph.write("pages/sized.md", "- 123456789\n");

    let source = graph.open_source();
    let page = source.discover().expect("discover pages").remove(0);
    assert_eq!(page.metadata().size(), 12);
    assert!(page.metadata().modified().is_some());

    graph.write("pages/sized.md", "- 123456789012345\n");
    let updated = source.discover().expect("discover pages").remove(0);
    assert_eq!(updated.metadata().size(), 18);
}

#[test]
fn missing_configuration_uses_logseq_defaults() {
    let graph = TempGraph::new("defaults");
    graph.write("pages/term___block.md", "- block\n");

    let source = graph.open_source();
    let pages = source.discover().expect("discover pages");
    assert_eq!(pages[0].name(), "term/block");
}

#[test]
fn empty_configuration_file_uses_logseq_defaults() {
    let graph = TempGraph::new("empty-config");
    graph.write_config("\n; nothing configured\n");
    graph.write("pages/term___block.md", "- block\n");

    let source = graph.open_source();
    assert_eq!(
        source.discover().expect("discover pages")[0].name(),
        "term/block"
    );
}

#[test]
fn unsupported_name_format_fails_open() {
    let graph = TempGraph::new("unsupported-name-format");
    graph.write_config("{:file/name-format :percent-encoding}");

    let error = GraphSource::open(graph.graph_root()).expect_err("unsupported format is refused");
    match error {
        SourceError::UnsupportedConfig { key, value, .. } => {
            assert_eq!(key, ":file/name-format");
            assert_eq!(value, ":percent-encoding");
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn unsupported_preferred_format_fails_open() {
    let graph = TempGraph::new("unsupported-format");
    graph.write_config("{:preferred-format \"Org\"}");

    let error = GraphSource::open(graph.graph_root()).expect_err("Org graph is refused");
    match error {
        SourceError::UnsupportedConfig { key, value, .. } => {
            assert_eq!(key, ":preferred-format");
            assert_eq!(value, "Org");
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn malformed_configuration_fails_open() {
    let graph = TempGraph::new("malformed-config");
    graph.write_config("{:file/name-format :triple-lowbar");

    let error = GraphSource::open(graph.graph_root()).expect_err("unclosed map is refused");
    assert!(matches!(error, SourceError::InvalidConfig { .. }));
}

#[test]
fn opening_a_missing_root_fails() {
    let graph = TempGraph::new("missing-root");
    let missing = graph.graph_root().join("nowhere");
    let error = GraphSource::open(&missing).expect_err("missing root is refused");
    match error {
        SourceError::Io { source, .. } => assert_eq!(source.kind(), io::ErrorKind::NotFound),
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn opening_a_file_as_a_root_fails() {
    let graph = TempGraph::new("file-root");
    graph.write("not-a-directory", "x");

    let error = GraphSource::open(graph.graph_root().join("not-a-directory"))
        .expect_err("a file is not a graph root");
    match error {
        SourceError::Io { source, .. } => assert_eq!(source.kind(), io::ErrorKind::NotADirectory),
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn empty_graph_discovers_nothing() {
    let graph = TempGraph::new("empty");
    let source = graph.open_source();
    assert!(source.discover().expect("discover pages").is_empty());
}

#[test]
fn two_encodings_of_the_same_name_are_rejected() {
    let graph = TempGraph::new("collision");
    graph.write("pages/a___b.md", "- one\n");
    graph.write("pages/a%2Fb.md", "- two\n");

    let source = graph.open_source();
    let error = source.discover().expect_err("collision is reported");
    match error {
        SourceError::DuplicateName {
            name,
            first,
            second,
        } => {
            assert_eq!(name, "a/b");
            assert_eq!(first, Path::new("pages/a%2Fb.md"));
            assert_eq!(second, Path::new("pages/a___b.md"));
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn a_page_and_a_journal_with_the_same_file_stem_are_distinct() {
    // A journal's identity is its date title, not its file name, so a page whose file name happens
    // to spell the journal's file name is a different page. The collision case is
    // `pages/Aug 13th, 2026.md` against `journals/2026_08_13.md`.
    let graph = TempGraph::new("cross-kind-same-stem");
    graph.write("pages/2020_01_01.md", "- page\n");
    graph.write("journals/2020_01_01.md", "- journal\n");

    let source = graph.open_source();
    let files = source.discover().expect("distinct names do not collide");
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].kind(), PageKind::Page);
    assert_eq!(files[0].name(), "2020_01_01");
    assert_eq!(files[1].kind(), PageKind::Journal);
    assert_eq!(files[1].name(), "Jan 1st, 2020");
    assert_eq!(files[1].comparison_name(), "jan 1st, 2020");
}

#[test]
fn trailing_lowbar_decodes_to_no_empty_trailing_segment() {
    // Logseq's `make-valid-namespaces` drops empty segments, so the trailing `___` leaves no
    // trailing slash: the file reads as one namespace level, not as a name ending in `/`.
    let graph = TempGraph::new("trailing-lowbar");
    graph.write(
        "pages/Tweet___This 1 Tiny Time Managem...___.md",
        "- tweet\n",
    );

    let source = graph.open_source();
    let pages = source.discover().expect("discover pages");
    assert_eq!(pages[0].name(), "Tweet/This 1 Tiny Time Managem...");
    assert_eq!(
        pages[0].comparison_name(),
        "tweet/this 1 tiny time managem..."
    );
}

#[test]
fn doubled_lowbar_reads_as_one_separator_not_an_empty_segment() {
    // `a______b` holds two runs of `___`, which decode to `a//b` before Logseq's
    // `make-valid-namespaces` removes the empty segment, so the page is `a/b` — the same page
    // `a___b` names. Writing both files would be a duplicate, and a create of `a//b` must be
    // refused instead of producing one of them.
    let graph = TempGraph::new("empty-namespace-segments");
    graph.write("pages/a______b.md", "- page\n");

    let source = graph.open_source();
    let pages = source.discover().expect("discover pages");
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].name(), "a/b");
    assert_eq!(pages[0].comparison_name(), "a/b");
}

#[test]
fn pages_that_is_a_file_fails_discovery() {
    let graph = TempGraph::new("pages-is-file");
    graph.write("pages", "not a directory");

    let source = graph.open_source();
    let error = source
        .discover()
        .expect_err("a file cannot be listed as a directory");
    match error {
        SourceError::Io { source, .. } => assert_eq!(source.kind(), io::ErrorKind::NotADirectory),
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn non_empty_text_formats_fails_open() {
    let graph = TempGraph::new("text-formats");
    graph.write_config("{:text-formats [\"Org\"]}");

    let error = GraphSource::open(graph.graph_root()).expect_err("extra text formats are refused");
    match error {
        SourceError::UnsupportedConfig { key, value, .. } => {
            assert_eq!(key, ":text-formats");
            assert_eq!(value, "[\"Org\"]");
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn empty_text_formats_keeps_the_graph_openable() {
    let graph = TempGraph::new("empty-text-formats");
    graph.write_config("{:text-formats [] :preferred-format \"Markdown\"}");
    graph.write("pages/p.md", "- p\n");

    let source = graph.open_source();
    assert_eq!(source.discover().expect("discover pages").len(), 1);
}

#[test]
fn configuration_key_without_a_value_fails_open() {
    for (label, text) in [("empty-map", "{:}"), ("missing-value", "{:hidden}")] {
        let graph = TempGraph::new(label);
        graph.write_config(text);

        let error = GraphSource::open(graph.graph_root()).expect_err("missing value is refused");
        assert!(
            matches!(error, SourceError::InvalidConfig { .. }),
            "unexpected error for {text}: {error}"
        );
    }
}

#[test]
fn configuration_with_a_leading_bom_is_accepted() {
    let graph = TempGraph::new("config-bom");
    graph.write_config("\u{feff}{:file/name-format :triple-lowbar}");
    graph.write("pages/a___b.md", "- a\n");

    let source = graph.open_source();
    assert_eq!(source.discover().expect("discover pages")[0].name(), "a/b");
}

#[test]
fn open_canonicalizes_the_root() {
    let graph = TempGraph::new("canonical");
    graph.write("pages/p.md", "- p\n");

    let indirect = graph.graph_root().join("pages").join("..");
    let source = GraphSource::open(&indirect).expect("open graph through a parent hop");
    assert_eq!(
        source.root(),
        fs::canonicalize(graph.graph_root()).expect("canonical root")
    );
    assert!(source.root().is_absolute());

    let page = source.discover().expect("discover pages").remove(0);
    assert!(page.path().starts_with(source.root()));
    assert_eq!(page.relative_path(), Path::new("pages/p.md"));
}

#[test]
fn reads_survive_a_working_directory_change() {
    // Every other test in this binary uses absolute paths, so moving the process working directory
    // for the duration of one test cannot affect them.
    let graph = TempGraph::new("cwd");
    graph.write("pages/p.md", "- p\n");

    let _guard = CurrentDirGuard::capture();
    std::env::set_current_dir(graph.graph_root()).expect("enter graph");
    let source = GraphSource::open(".").expect("open relative root");
    let pages = source.discover().expect("discover pages");
    // Leave the graph directory before reading, so the read cannot rely on the working directory.
    std::env::set_current_dir(std::env::temp_dir()).expect("leave graph");

    let page = &pages[0];
    assert!(page.path().is_absolute());
    assert_eq!(page.read_text().expect("read page"), "- p\n");
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_names_are_skipped_unless_they_are_markdown() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let graph = TempGraph::new("non-utf8-name");
    graph.write("pages/ok.md", "- ok\n");
    let pages_dir = graph.graph_root().join("pages");

    // A non-Markdown entry with an undecodable name is not a page.
    fs::write(pages_dir.join(OsStr::from_bytes(b"notes\xff.txt")), "x").expect("write");
    let source = graph.open_source();
    assert_eq!(source.discover().expect("discover pages").len(), 1);

    // A Markdown entry with an undecodable name is a page this build cannot name.
    fs::write(pages_dir.join(OsStr::from_bytes(b"page\xff.md")), "- x\n").expect("write");
    let error = source
        .discover()
        .expect_err("undecodable page name is an error");
    assert!(
        matches!(error, SourceError::InvalidPageName { .. }),
        "unexpected error: {error}"
    );
}
