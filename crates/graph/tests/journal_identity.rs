//! Journal identity tests: a journal is named by its date, not by its file name.
//!
//! Every test builds its own graph under the system temporary directory, so nothing depends on a
//! checked-out real graph, on absolute fixture paths, or on network access.

use std::path::Path;

use logseq_graph::domain::PageKind;
use logseq_graph::source::{GraphSource, SourceError};

mod support;

use support::TempGraph;

#[test]
fn default_formats_name_a_journal_by_its_date_title() {
    let graph = TempGraph::new("defaults");
    graph.write("journals/2026_08_13.md", "- journal\n");

    let source = graph.open_source();
    let files = source.discover().expect("discover pages");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].kind(), PageKind::Journal);
    assert_eq!(files[0].name(), "Aug 13th, 2026");
    assert_eq!(files[0].comparison_name(), "aug 13th, 2026");
    assert_eq!(
        files[0].relative_path(),
        Path::new("journals/2026_08_13.md")
    );
}

#[test]
fn custom_page_title_format_changes_the_journal_name() {
    let graph = TempGraph::new("custom-title");
    graph.write_config(r#"{:journal/page-title-format "yyyy-MM-dd EEEE"}"#);
    graph.write("journals/2026_08_13.md", "- journal\n");

    let source = graph.open_source();
    let files = source.discover().expect("discover pages");
    assert_eq!(files[0].name(), "2026-08-13 Thursday");
    assert_eq!(files[0].comparison_name(), "2026-08-13 thursday");
}

#[test]
fn custom_file_name_format_reads_another_layout() {
    let graph = TempGraph::new("custom-file-name");
    graph.write_config(r#"{:journal/file-name-format "yyyy-MM-dd"}"#);
    graph.write("journals/2026-08-13.md", "- journal\n");

    let source = graph.open_source();
    let files = source.discover().expect("discover pages");
    assert_eq!(files[0].name(), "Aug 13th, 2026");
}

#[test]
fn journal_file_name_of_another_shape_is_rejected() {
    // The default file name format is `yyyy_MM_dd`; a journal file named in another layout is not
    // a date this build can read, and guessing one would rename the page.
    let graph = TempGraph::new("wrong-shape");
    graph.write("journals/2026-08-13.md", "- journal\n");

    let source = graph.open_source();
    let error = source
        .discover()
        .expect_err("unreadable journal file name is an error");
    match error {
        SourceError::InvalidPageName { path, reason } => {
            assert_eq!(path, source.root().join("journals/2026-08-13.md"));
            assert!(
                reason.contains("yyyy_MM_dd"),
                "reason does not name the format: {reason}"
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn impossible_journal_dates_are_rejected() {
    for name in ["2026_13_01", "2026_02_30", "2026_04_31", "2026_00_10"] {
        let graph = TempGraph::new("impossible-date");
        graph.write(&format!("journals/{name}.md"), "- journal\n");

        let source = graph.open_source();
        let error = source.discover().expect_err("impossible date is an error");
        match error {
            SourceError::InvalidPageName { reason, .. } => assert!(
                reason.contains("is not a calendar day"),
                "unexpected reason for {name}: {reason}"
            ),
            other => panic!("unexpected error for {name}: {other}"),
        }
    }
}

#[test]
fn leap_day_is_accepted_only_in_a_leap_year() {
    let leap = TempGraph::new("leap");
    leap.write("journals/2024_02_29.md", "- journal\n");
    assert_eq!(
        leap.open_source().discover().expect("discover")[0].name(),
        "Feb 29th, 2024"
    );

    let common = TempGraph::new("common-year");
    common.write("journals/2026_02_29.md", "- journal\n");
    let error = common
        .open_source()
        .discover()
        .expect_err("2026 is not a leap year");
    assert!(
        matches!(error, SourceError::InvalidPageName { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn unsupported_page_title_format_fails_open() {
    let graph = TempGraph::new("unsupported-title-format");
    graph.write_config(r#"{:journal/page-title-format "yyyy-MM-dd HH:mm"}"#);

    let error = GraphSource::open(graph.graph_root()).expect_err("unsupported pattern is refused");
    match error {
        SourceError::UnsupportedConfig { key, value, .. } => {
            assert_eq!(key, ":journal/page-title-format");
            assert_eq!(value, "yyyy-MM-dd HH:mm");
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn unsupported_file_name_format_fails_open() {
    let graph = TempGraph::new("unsupported-file-name-format");
    graph.write_config(r#"{:journal/file-name-format "yy_MM_dd"}"#);

    let error = GraphSource::open(graph.graph_root()).expect_err("two-digit year is refused");
    match error {
        SourceError::UnsupportedConfig { key, value, .. } => {
            assert_eq!(key, ":journal/file-name-format");
            assert_eq!(value, "yy_MM_dd");
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn a_page_named_like_the_journal_title_is_rejected() {
    let graph = TempGraph::new("cross-kind-title-collision");
    graph.write("pages/Aug 13th, 2026.md", "- page\n");
    graph.write("journals/2026_08_13.md", "- journal\n");

    let source = graph.open_source();
    let error = source
        .discover()
        .expect_err("page and journal share the displayed name");
    match error {
        SourceError::DuplicateName {
            name,
            first,
            second,
        } => {
            assert_eq!(name, "aug 13th, 2026");
            assert_eq!(first, Path::new("pages/Aug 13th, 2026.md"));
            assert_eq!(second, Path::new("journals/2026_08_13.md"));
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn the_title_collision_is_case_insensitive() {
    let graph = TempGraph::new("cross-kind-title-case");
    graph.write("pages/aug 13TH, 2026.md", "- page\n");
    graph.write("journals/2026_08_13.md", "- journal\n");

    let source = graph.open_source();
    let error = source
        .discover()
        .expect_err("names compare case-insensitively");
    assert!(
        matches!(error, SourceError::DuplicateName { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn a_custom_title_format_moves_the_collision_with_it() {
    // With `yyyy-MM-dd` titles, the file name layout no longer matters and the date title does.
    let graph = TempGraph::new("custom-title-collision");
    graph.write_config(r#"{:journal/page-title-format "yyyy-MM-dd"}"#);
    graph.write("pages/2026-08-13.md", "- page\n");
    graph.write("journals/2026_08_13.md", "- journal\n");

    let source = graph.open_source();
    let error = source
        .discover()
        .expect_err("page name equals the rendered title");
    match error {
        SourceError::DuplicateName { name, .. } => assert_eq!(name, "2026-08-13"),
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn journals_and_pages_keep_their_own_name_rules() {
    // `:file/name-format` decodes page file names only: a journal file name is read as a date, so
    // percent escapes and `___` in it are not decoded into a page name.
    let graph = TempGraph::new("separate-rules");
    graph.write("pages/term___a%2Fb.md", "- page\n");
    graph.write("journals/2026_08_13.md", "- journal\n");

    let source = graph.open_source();
    let files = source.discover().expect("discover pages");
    assert_eq!(files[0].name(), "term/a/b");
    assert_eq!(files[1].name(), "Aug 13th, 2026");
}
