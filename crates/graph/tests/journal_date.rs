//! The calendar day a journal is, as every read of it states it.
//!
//! A journal is identified by a day, but its name is `:journal/page-title-format`'s rendering of
//! that day, so the day cannot be read back out of the name. These tests pin where it comes from
//! instead: discovery parses it out of the file name under `:journal/file-name-format`, the
//! projection stores it, and the page reads hand it back unchanged.

use logseq_graph::domain::{JournalDate, PageKind};
use logseq_graph::engine::Freshness;

mod support;

use support::TempGraph;

fn day(year: i64, month: u32, day: u32) -> JournalDate {
    JournalDate::new(year, month, day).expect("a day of the calendar")
}

#[test]
fn discovery_reads_the_day_out_of_a_journal_file_name() {
    let graph = TempGraph::new("journal-date-discovery");
    graph.write("journals/2026_08_13.md", "- a journal\n");
    graph.write("pages/alpha.md", "- a page\n");

    let source = graph.open_source();
    let files = source.discover().expect("discover pages");

    let journal = files
        .iter()
        .find(|file| file.kind() == PageKind::Journal)
        .expect("the journal file is discovered");
    assert_eq!(journal.journal_date(), Some(day(2026, 8, 13)));
    // The name is the day rendered, so the file name never appears in it and the day is not
    // recovered from the name: it is the other way round.
    assert_eq!(journal.name(), "Aug 13th, 2026");

    let page = files
        .iter()
        .find(|file| file.kind() == PageKind::Page)
        .expect("the page file is discovered");
    assert_eq!(
        page.journal_date(),
        None,
        "a regular page is named by its file name, not by a day"
    );
}

#[test]
fn a_page_read_and_a_page_listing_state_the_journal_day() {
    let graph = TempGraph::new("journal-date-reads");
    graph.write("journals/2026_08_13.md", "- a journal\n");
    graph.write("pages/alpha.md", "- a page\n");
    let mut opened = graph.open();

    let metadata = opened
        .page_metadata("Aug 13th, 2026")
        .expect("query journal metadata");
    let journal = metadata.present().expect("the journal is projected");
    assert_eq!(journal.kind(), PageKind::Journal);
    assert_eq!(journal.journal_date(), Some(day(2026, 8, 13)));

    let metadata = opened.page_metadata("alpha").expect("query page metadata");
    let page = metadata.present().expect("the page is projected");
    assert_eq!(page.kind(), PageKind::Page);
    assert_eq!(page.journal_date(), None);

    let summaries = opened
        .reads(Freshness::LastScan)
        .pages()
        .expect("list pages");
    let summaries = summaries.value();
    let journal = summaries
        .iter()
        .find(|summary| summary.name() == "Aug 13th, 2026")
        .expect("the journal is listed under its date title");
    assert_eq!(journal.journal_date(), Some(day(2026, 8, 13)));
    let page = summaries
        .iter()
        .find(|summary| summary.name() == "alpha")
        .expect("the page is listed");
    assert_eq!(page.journal_date(), None);
}

#[test]
fn a_listing_orders_journals_by_name_and_a_client_orders_them_by_day() {
    // The stored day is the only field that can order journals by time. The listing keeps its one
    // order for both kinds — the folded name — and for a journal that name is a label: it sorts
    // `Aug` before `Dec` before `Jan`, which is not the year those three belong to.
    let graph = TempGraph::new("journal-date-order");
    graph.write("journals/2026_01_05.md", "- January\n");
    graph.write("journals/2025_12_31.md", "- December\n");
    graph.write("journals/2026_08_13.md", "- August\n");
    let mut opened = graph.open();

    let summaries = opened
        .reads(Freshness::LastScan)
        .pages()
        .expect("list pages");
    let names: Vec<&str> = summaries
        .value()
        .iter()
        .map(|summary| summary.name())
        .collect();
    assert_eq!(
        names,
        ["Aug 13th, 2026", "Dec 31st, 2025", "Jan 5th, 2026"],
        "the listing stays in folded-name order"
    );

    let mut days: Vec<JournalDate> = summaries
        .value()
        .iter()
        .filter_map(|summary| summary.journal_date())
        .collect();
    days.sort();
    assert_eq!(
        days,
        [day(2025, 12, 31), day(2026, 1, 5), day(2026, 8, 13)],
        "sorting the same rows by their day answers the chronological order"
    );
}
