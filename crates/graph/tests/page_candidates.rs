//! Name lookups through the public engine API.
//!
//! A lookup answers pages, not blocks: a page's own name and every value of its `alias::`
//! property are first-class spellings. The cases below pin the order a caller offers the list in,
//! the fact that a shared alias answers several pages rather than one merged entity, and the rule
//! that a query and a lookup agree on when two names denote the same page.

mod support;

use logseq_graph::domain::{JournalDate, PageCandidate, PageMatch, QueryContext, QueryOutcome};
use logseq_graph::engine::Graph;

use support::TempGraph;

/// The day relative query operands count from; the queries here use none.
fn today() -> JournalDate {
    JournalDate::new(2026, 8, 13).expect("a day of the calendar")
}

fn context(limit: u32) -> QueryContext {
    QueryContext::new(today(), limit)
}

fn candidates(graph: &mut Graph, text: &str, limit: u32) -> Vec<PageCandidate> {
    graph
        .page_candidates(text, limit)
        .unwrap_or_else(|error| panic!("page candidates {text:?}: {error}"))
        .into_value()
}

fn query_pages(graph: &mut Graph, text: &str) -> Vec<String> {
    match graph
        .query(text, &context(50))
        .unwrap_or_else(|error| panic!("query {text:?}: {error}"))
        .into_value()
        .outcome()
        .clone()
    {
        QueryOutcome::Pages(pages) => pages.iter().map(|page| page.name().to_owned()).collect(),
        other => panic!("{text:?} answered blocks: {other:?}"),
    }
}

/// Two pages share `zmi` under different spellings; another page is itself named `紫米`, which a
/// third page also declares as an alias. The graph also has a page whose name is a prefix and one
/// whose name only contains the word, so rank is observable.
fn alias_graph() -> TempGraph {
    let graph = TempGraph::new("page-candidates");
    graph.write(
        "pages/ADR.md",
        "alias:: 架构决策记录, ADRs\n\n- a decision record\n",
    );
    graph.write("pages/紫米.md", "alias:: zmi\n");
    graph.write("pages/酷态科.md", "alias:: 紫米, ZMI\n");
    graph.write("pages/Relax.md", "alias:: 放松\n");
    graph.write("pages/term.md", "- exact\n");
    graph.write("pages/term-one.md", "- prefix\n");
    graph.write("pages/x-term.md", "- infix\n");
    graph
}

#[test]
fn a_shared_alias_answers_every_page_that_states_it() {
    let mut opened = alias_graph().open();
    let found = candidates(&mut opened, "zmi", 20);

    assert_eq!(
        found.len(),
        2,
        "one entry per page, not one merged entity: {found:?}"
    );
    let names: Vec<&str> = found.iter().map(PageCandidate::name).collect();
    assert!(
        names.contains(&"紫米"),
        "紫米 states alias:: zmi: {names:?}"
    );
    assert!(names.contains(&"酷态科"), "酷态科 states ZMI: {names:?}");
    for candidate in &found {
        assert_eq!(candidate.via(), PageMatch::Alias, "{candidate:?}");
        assert!(
            candidate.alias().is_some(),
            "an alias hit states its alias: {candidate:?}"
        );
    }
}

#[test]
fn an_alias_only_name_finds_the_page_and_states_the_alias() {
    let mut opened = alias_graph().open();
    let found = candidates(&mut opened, "架构决策记录", 20);

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].name(), "ADR");
    assert_eq!(found[0].via(), PageMatch::Alias);
    assert_eq!(found[0].alias(), Some("架构决策记录"));
}

#[test]
fn a_pages_own_name_answers_first_and_other_aliases_still_answer() {
    let mut opened = alias_graph().open();
    let found = candidates(&mut opened, "紫米", 20);

    assert_eq!(
        found.len(),
        2,
        "the real page and the alias hit both answer: {found:?}"
    );
    assert_eq!(found[0].name(), "紫米", "the page name is first: {found:?}");
    assert_eq!(found[0].via(), PageMatch::Name);
    assert_eq!(found[0].alias(), None);
    assert_eq!(found[1].name(), "酷态科");
    assert_eq!(found[1].via(), PageMatch::Alias);
    assert_eq!(found[1].alias(), Some("紫米"));
}

#[test]
fn the_page_name_rule_folds_alias_case() {
    let mut opened = alias_graph().open();
    assert_eq!(
        candidates(&mut opened, "zmi", 20),
        candidates(&mut opened, "ZMI", 20),
        "zmi and ZMI are one page name, in a lookup as in every page read"
    );
}

#[test]
fn exact_answers_before_prefix_before_infix() {
    let mut opened = alias_graph().open();
    let found = candidates(&mut opened, "term", 20);

    assert_eq!(
        found.iter().map(PageCandidate::name).collect::<Vec<_>>(),
        ["term", "term-one", "x-term"],
        "rank is a property of the rows, not of insertion order: {found:?}"
    );
    assert!(
        found
            .iter()
            .all(|candidate| candidate.via() == PageMatch::Name),
        "{found:?}"
    );
}

#[test]
fn a_name_hit_outranks_an_alias_hit_on_the_same_page() {
    let mut opened = alias_graph().open();
    let found = candidates(&mut opened, "adr", 20);

    let adr = found
        .iter()
        .find(|candidate| candidate.name() == "ADR")
        .expect("ADR answers");
    assert_eq!(
        adr.via(),
        PageMatch::Name,
        "one entry per page, the name one: {found:?}"
    );
    assert_eq!(
        found[0].name(),
        "ADR",
        "an exact name is the first jump: {found:?}"
    );
}

#[test]
fn an_empty_query_and_a_zero_limit_answer_no_candidates() {
    let mut opened = alias_graph().open();
    assert!(candidates(&mut opened, "", 20).is_empty());
    assert!(candidates(&mut opened, "zmi", 0).is_empty());
    assert_eq!(
        candidates(&mut opened, "zmi", 1).len(),
        1,
        "the limit bounds the list"
    );
}

#[test]
fn a_query_and_a_lookup_agree_on_one_alias_value() {
    let mut opened = alias_graph().open();

    // `owners_matching` and `page_candidates` both fold list items with `page_name_key`. A raw
    // `==` in either one would make this pair of queries disagree with the lookup above.
    for text in ["(page-property :alias zmi)", "(page-property :alias ZMI)"] {
        let mut pages = query_pages(&mut opened, text);
        pages.sort();
        assert_eq!(
            pages,
            ["紫米".to_owned(), "酷态科".to_owned()],
            "{text}: {pages:?}"
        );
    }
}
