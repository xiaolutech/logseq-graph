//! Creating a journal: one date, two formats, one new file under `journals/`.
//!
//! A journal's identity is a calendar day, so these tests always start from a [`JournalDate`] and
//! assert on the two things the configured formats render from it: the file under `journals/` and
//! the page title every read answers with. The tests that matter most are the ones about what must
//! *not* happen: a create must not replace an existing file, must not rewrite a journal that already
//! exists, and must not turn a name that looks like a journal title into a date.

use std::fs;

use logseq_graph::domain::JournalDate;
use logseq_graph::domain::PageKind;
use logseq_graph::engine::WriteError;

mod support;

use support::TempGraph;

/// A date the tests can rely on, built through the guarded constructor.
fn day(year: i64, month: u32, day: u32) -> JournalDate {
    JournalDate::new(year, month, day).expect("a day of the calendar")
}

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
fn the_default_formats_name_the_journal_of_a_date() {
    let graph = TempGraph::new("journal-create-default");
    let mut opened = graph.open();

    let created = opened
        .create_journal(day(2026, 8, 13))
        .expect("create a journal");

    assert_eq!(
        created.value().name(),
        "Aug 13th, 2026",
        "the page is named by `:journal/page-title-format`"
    );
    assert_eq!(
        created.value().block_count(),
        0,
        "an empty file is a page with no blocks"
    );
    assert_eq!(
        created.value().kind(),
        PageKind::Journal,
        "what `create_journal` answers is a journal, not a regular page"
    );
    assert_eq!(
        read_bytes(&graph, "journals/2026_08_13.md"),
        b"",
        "the file is empty and its name comes from `:journal/file-name-format`"
    );
    assert_eq!(
        file_names(&graph, "journals"),
        vec!["2026_08_13.md".to_owned()],
        "exactly one journal file appeared"
    );
    assert!(
        !graph.graph_root().join("pages").exists(),
        "a journal never lands under pages/"
    );

    // The name every read uses is the one the title format rendered, and the file is a journal.
    let stored = opened
        .page_metadata("Aug 13th, 2026")
        .expect("read page metadata")
        .into_value();
    assert_eq!(
        stored.expect("the page is projected").name(),
        "Aug 13th, 2026"
    );

    let files = graph.open_source().discover().expect("discover the graph");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].kind(), PageKind::Journal);
    assert_eq!(files[0].name(), "Aug 13th, 2026");
}

#[test]
fn custom_formats_name_the_file_and_the_page_it_renders() {
    // Both directions are configurable and independent: the file name can use dashes while the
    // title is Chinese, and the created journal has to honour both at once.
    let graph = TempGraph::new("journal-create-custom");
    graph.write_config(
        r#"{:journal/file-name-format "yyyy-MM-dd" :journal/page-title-format "yyyy年M月d日"}"#,
    );
    let mut opened = graph.open();

    let created = opened
        .create_journal(day(2026, 8, 13))
        .expect("create a journal");

    assert_eq!(created.value().name(), "2026年8月13日");
    assert_eq!(created.value().block_count(), 0);
    assert_eq!(read_bytes(&graph, "journals/2026-08-13.md"), b"");
    assert_eq!(
        file_names(&graph, "journals"),
        vec!["2026-08-13.md".to_owned()]
    );
}

#[test]
fn a_second_create_of_the_same_day_refuses_and_keeps_every_byte() {
    let graph = TempGraph::new("journal-create-idempotent");
    let mut opened = graph.open();

    opened
        .create_journal(day(2026, 8, 13))
        .expect("create a journal");

    // Text this process never wrote, standing in for a journal another editor filled in. A second
    // create must not touch it: idempotence here is about bytes, not about calls.
    graph.write("journals/2026_08_13.md", "- 手写的一行\n");
    let before = read_bytes(&graph, "journals/2026_08_13.md");

    let error = opened
        .create_journal(day(2026, 8, 13))
        .expect_err("a second create of the same day is refused");

    match &error {
        WriteError::PageExists { page, path } => {
            assert_eq!(page, "Aug 13th, 2026");
            assert!(
                path.ends_with("journals/2026_08_13.md"),
                "the error names the file that holds the journal: {}",
                path.display()
            );
        }
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(
        read_bytes(&graph, "journals/2026_08_13.md"),
        before,
        "the existing bytes are untouched, so the sentence is still there"
    );
    assert_eq!(
        file_names(&graph, "journals"),
        vec!["2026_08_13.md".to_owned()],
        "no second file appeared"
    );
}

#[test]
fn a_file_created_after_the_open_is_refused_by_the_create_itself() {
    // The one case the index check cannot see: the file exists on disk and no page row names it,
    // because it appeared after the scan this graph opened with. The refusal then comes from
    // `create_text`, not from step 3, and the file that is already there keeps every byte.
    let graph = TempGraph::new("journal-create-after-open");
    let mut opened = graph.open();

    graph.write("journals/2026_08_13.md", "- 刚写的一行\n");
    let before = read_bytes(&graph, "journals/2026_08_13.md");
    assert!(
        opened
            .page_metadata("Aug 13th, 2026")
            .expect("read page metadata")
            .into_value()
            .is_none(),
        "the index does not know a file it never scanned, so this refusal has to come from the create"
    );

    let error = opened
        .create_journal(day(2026, 8, 13))
        .expect_err("the file is already there");

    match &error {
        WriteError::PageExists { page, path } => {
            assert_eq!(page, "Aug 13th, 2026");
            assert!(
                path.ends_with("journals/2026_08_13.md"),
                "the error names the file on disk: {}",
                path.display()
            );
        }
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(
        read_bytes(&graph, "journals/2026_08_13.md"),
        before,
        "the file that was already there is untouched and was not removed"
    );
    assert_eq!(
        file_names(&graph, "journals"),
        vec!["2026_08_13.md".to_owned()],
        "no second file appeared"
    );
}

#[test]
fn a_journal_written_outside_is_refused_with_its_blocks_intact() {
    // The file exists and the open projected it, so the create stops before writing anything. This is
    // the ordinary case of a journal Logseq itself wrote.
    let graph = TempGraph::new("journal-create-external");
    graph.write("journals/2026_08_13.md", "- 手写\n");
    let mut opened = graph.open();
    let before = read_bytes(&graph, "journals/2026_08_13.md");

    let error = opened
        .create_journal(day(2026, 8, 13))
        .expect_err("a journal that already exists is refused");

    assert!(
        matches!(error, WriteError::PageExists { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(
        read_bytes(&graph, "journals/2026_08_13.md"),
        before,
        "the handwritten bytes are untouched"
    );

    // The block is still the one the file states, read through the name the title format renders.
    let tree = opened
        .page_tree("Aug 13th, 2026")
        .expect("query page tree")
        .into_value()
        .expect("the journal is projected");
    assert_eq!(tree.page().block_count(), 1);
}

#[test]
fn a_plain_page_holding_the_journal_title_blocks_the_create() {
    // A journal's title is compared like any other page name, so an ordinary page that happens to be
    // named `Aug 13th, 2026` is the same page — and the journal create must not write a second file
    // that decodes to that name.
    let graph = TempGraph::new("journal-create-cross-page");
    let mut opened = graph.open();

    opened
        .create_page("Aug 13th, 2026")
        .expect("create the ordinary page");
    let before = read_bytes(&graph, "pages/Aug 13th, 2026.md");

    let error = opened
        .create_journal(day(2026, 8, 13))
        .expect_err("the name is already taken by an ordinary page");

    match &error {
        WriteError::PageExists { path, .. } => assert!(
            path.ends_with("pages/Aug 13th, 2026.md"),
            "the error points at the page that holds the name: {}",
            path.display()
        ),
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(read_bytes(&graph, "pages/Aug 13th, 2026.md"), before);
    assert!(
        !graph.graph_root().join("journals").exists(),
        "no journal directory, let alone a journal file, was created"
    );
}

#[test]
fn a_created_journal_blocks_a_plain_page_of_the_same_title() {
    let graph = TempGraph::new("journal-create-cross-journal");
    let mut opened = graph.open();

    opened
        .create_journal(day(2026, 8, 13))
        .expect("create the journal");
    let before = read_bytes(&graph, "journals/2026_08_13.md");

    let error = opened
        .create_page("Aug 13th, 2026")
        .expect_err("the title is already taken by a journal");

    assert!(
        matches!(error, WriteError::PageExists { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(
        read_bytes(&graph, "journals/2026_08_13.md"),
        before,
        "the journal is untouched"
    );
    assert!(
        !graph.graph_root().join("pages").exists(),
        "no ordinary page file was created"
    );
}

#[test]
fn a_date_that_is_not_on_the_calendar_has_no_journal() {
    // The type is the guard: there is no `JournalDate` for February 30th, so no call can ask for
    // that day's journal. The leap day is the one that flips with the year.
    assert!(JournalDate::new(2026, 2, 30).is_none());
    assert!(JournalDate::new(2026, 2, 29).is_none());
    assert!(JournalDate::new(2026, 13, 1).is_none());
    assert!(JournalDate::new(2026, 4, 31).is_none());
    assert!(JournalDate::new(2026, 1, 0).is_none());
    assert!(JournalDate::new(2024, 2, 29).is_some());
    // `yyyy` is four digits in the file name, so a year outside that range has no journal either:
    // the day would render a stem discovery reads as an invalid page name.
    assert!(JournalDate::new(10_000, 1, 1).is_none());
    assert!(JournalDate::new(-1, 1, 1).is_none());
    assert!(JournalDate::new(9999, 12, 31).is_some());

    let leap = day(2024, 2, 29);
    assert_eq!((leap.year(), leap.month(), leap.day()), (2024, 2, 29));
    assert_eq!(leap.to_string(), "2024-02-29");
}
