//! The `:favorites` read: the graph's pinned page names, as written, each with whether its page
//! exists.
//!
//! Every case writes a real `logseq/config.edn` and real page files, opens the engine, and asks
//! through the public read, so one path covers the configuration scan, the page-name rule the
//! existence lookup uses, and the freshness metadata the answer carries.
//!
//! Two answers are deliberately different: the list is the person's configuration and is answered
//! whole, and `exists` is the separate fact about the graph. A case that filtered the list would
//! lose the entry `gone` below, which is exactly the state a display has to be able to show.

use logseq_graph::domain::Favorite;
use logseq_graph::engine::{Freshness, Graph};

mod support;

use support::TempGraph;

/// The names of a favorites read, in the order the read returned them.
fn names(favorites: &[Favorite]) -> Vec<&str> {
    favorites.iter().map(Favorite::name).collect()
}

/// The existence flags of a favorites read, in the order the read returned them.
fn existence(favorites: &[Favorite]) -> Vec<bool> {
    favorites.iter().map(Favorite::exists).collect()
}

/// One graph whose configuration pins four names: two pages, one page that is not there, and one
/// journal under its displayed title.
fn graph_with_favorites(label: &str) -> TempGraph {
    let graph = TempGraph::new(label);
    graph.write("pages/alpha.md", "- alpha\n");
    graph.write("pages/Inbox.md", "- the inbox page\n");
    graph.write("journals/2026_08_13.md", "- a journal entry\n");
    graph.write_config(
        r#"{:preferred-format :markdown
            :favorites ["alpha" "inbox" "已删除的页面" "Aug 13th, 2026"]}
        "#,
    );
    graph
}

#[test]
fn favorites_are_answered_as_written_and_state_whether_their_page_exists() {
    let graph = graph_with_favorites("favorites-existence");
    let mut opened: Graph = graph.open();

    let read = opened.favorites().expect("read favorites");
    let favorites = read.value();

    assert_eq!(
        names(favorites),
        ["alpha", "inbox", "已删除的页面", "Aug 13th, 2026"],
        "the list is answered in written order, and a name with no page stays in it"
    );
    assert_eq!(
        existence(favorites),
        [true, true, false, true],
        "`inbox` finds the page stored as `Inbox` because the page-name rule folds it, and a journal \
         is named by its displayed title"
    );
    assert!(
        !read.is_stale(),
        "a CheckNow-free open states what it scanned"
    );
}

#[test]
fn a_graph_without_the_key_answers_an_empty_list() {
    let graph = TempGraph::new("favorites-missing-key");
    graph.write("pages/alpha.md", "- alpha\n");
    graph.write_config("{:preferred-format :markdown}\n");
    let mut opened = graph.open();

    let read = opened.favorites().expect("read favorites");
    assert!(read.value().is_empty(), "no key lists no names");
}

#[test]
fn an_empty_list_answers_no_names() {
    let graph = TempGraph::new("favorites-empty");
    graph.write("pages/alpha.md", "- alpha\n");
    graph.write_config("{:favorites []}\n");
    let mut opened = graph.open();

    assert!(opened
        .favorites()
        .expect("read favorites")
        .value()
        .is_empty());
}

#[test]
fn an_edited_list_is_answered_by_the_next_scan() {
    // The list is configuration that a scan re-reads, not a projection of any page file: editing
    // `config.edn` changes what this read answers although no page file changed.
    let graph = graph_with_favorites("favorites-edited");
    let mut opened = graph.open();
    assert_eq!(
        names(opened.favorites().expect("first read").value()),
        ["alpha", "inbox", "已删除的页面", "Aug 13th, 2026"]
    );

    graph.write_config("{:favorites [\"alpha\"]}\n");
    let read = opened
        .reads(Freshness::CheckNow)
        .favorites()
        .expect("read after the edit");
    assert_eq!(names(read.value()), ["alpha"]);
    assert_eq!(existence(read.value()), [true]);
}
