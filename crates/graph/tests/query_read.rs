//! Simple queries through the graph facade.
//!
//! Each test writes graph files, opens a [`Graph`], and asks one query, so the path under test is
//! discovery, projection, the executor, and the result shape together. The cases are chosen so that
//! each answer sits beside the one it must not be confused with: a nested block under a reference
//! beside the block that states it, a page-level predicate beside the blocks of the pages it
//! matches, and a block with no `id::` beside one that states an id.

use logseq_graph::domain::{
    BlockIdentityKind, BlockSelector, JournalDate, PageKind, QueryBlock, QueryContext, QueryError,
    QueryExpr, QueryOutcome, QueryResult,
};
use logseq_graph::engine::{EngineError, Freshness, Graph};

mod support;

use support::TempGraph;

/// The day every relative date in these tests counts from.
fn today() -> JournalDate {
    JournalDate::new(2026, 8, 13).expect("a day of the calendar")
}

fn context(limit: u32) -> QueryContext {
    QueryContext::new(today(), limit)
}

/// Run one query text and return its blocks, panicking on a page answer.
fn blocks(graph: &mut Graph, text: &str, limit: u32) -> Vec<QueryBlock> {
    match graph
        .query(text, &context(limit))
        .unwrap_or_else(|error| panic!("query {text:?}: {error}"))
        .into_value()
        .outcome()
        .clone()
    {
        QueryOutcome::Blocks(blocks) => blocks,
        QueryOutcome::Pages(pages) => panic!("{text:?} answered {} pages", pages.len()),
    }
}

/// Run one query text and return the `(page, preorder)` pairs it matched.
fn positions(graph: &mut Graph, text: &str, limit: u32) -> Vec<(String, u32)> {
    blocks(graph, text, limit)
        .into_iter()
        .map(|block| (block.page().to_owned(), block.preorder()))
        .collect()
}

/// One page holding tasks, a reference, and a nested block.
fn task_graph() -> TempGraph {
    let graph = TempGraph::new("query-read");
    graph.write(
        "pages/project.md",
        "- TODO write the query engine\n  - child of the task\n- DONE ship it\n",
    );
    graph.write(
        "pages/notes.md",
        "- a note that references [[project]]\n  - nested under the reference\n- unrelated\n",
    );
    graph.write(
        "pages/done.md",
        "- TODO another task #project\n- no marker here\n",
    );
    graph
}

#[test]
fn a_task_query_answers_the_blocks_whose_text_leads_with_that_marker() {
    let graph = task_graph();
    let mut opened = graph.open();

    let matched = blocks(&mut opened, "(task TODO)", 10);
    assert_eq!(
        matched
            .iter()
            .map(|block| (block.page(), block.text()))
            .collect::<Vec<_>>(),
        [
            ("done", "TODO another task #project"),
            ("project", "TODO write the query engine"),
        ],
        "both TODOs answer, and the DONE does not"
    );
    assert!(matched
        .iter()
        .all(|block| block.task_marker().is_some() && block.priority().is_none()));
}

#[test]
fn a_reference_answer_carries_the_subtree_of_every_referencing_block() {
    // Logseq's path references are inherited: a block that references a page makes its descendants
    // part of the answer too, and the page's own blocks answer as well.
    let graph = task_graph();
    let mut opened = graph.open();

    let matched = positions(&mut opened, "[[project]]", 20);
    assert_eq!(
        matched,
        [
            ("done".to_owned(), 0),
            ("notes".to_owned(), 0),
            ("notes".to_owned(), 1),
            ("project".to_owned(), 0),
            ("project".to_owned(), 1),
            ("project".to_owned(), 2),
        ],
        "the page's own blocks, the referencing block, its child, and the tag hit"
    );
}

#[test]
fn a_page_predicate_matches_the_blocks_of_the_pages_it_names() {
    let graph = task_graph();
    let mut opened = graph.open();

    assert_eq!(
        positions(&mut opened, "(page project)", 10),
        [
            ("project".to_owned(), 0),
            ("project".to_owned(), 1),
            ("project".to_owned(), 2),
        ]
    );
}

#[test]
fn a_not_query_answers_everything_outside_its_predicate() {
    let graph = task_graph();
    let mut opened = graph.open();
    let everything = blocks(&mut opened, "(not (page nobody))", 50).len();

    let todos = blocks(&mut opened, "(task TODO)", 50).len();
    let not_todos = blocks(&mut opened, "(not (task TODO))", 50).len();
    assert_eq!(
        not_todos,
        everything - todos,
        "the complement is taken over every block of the graph"
    );
}

#[test]
fn a_limit_cuts_after_ordering_and_says_so() {
    let graph = task_graph();
    let mut opened = graph.open();

    let full = positions(&mut opened, "(not (page nobody))", 50);
    let read = opened
        .query("(not (page nobody))", &context(2))
        .expect("query the graph")
        .into_value();
    assert!(read.truncated(), "two results cannot be the whole answer");
    match read.outcome() {
        QueryOutcome::Blocks(blocks) => {
            let first_two: Vec<(String, u32)> = blocks
                .iter()
                .map(|block| (block.page().to_owned(), block.preorder()))
                .collect();
            assert_eq!(first_two, full[..2], "the limit keeps the first N in order");
        }
        other => panic!("expected blocks, got {other:?}"),
    }
}

#[test]
fn the_same_query_twice_answers_the_same_order() {
    let graph = task_graph();
    let mut opened = graph.open();

    let first = positions(&mut opened, "(journal)", 50);
    let second = positions(&mut opened, "(journal)", 50);
    assert_eq!(first, second);

    // The order is a function of the rows, so a rebuilt index answers it again.
    drop(opened);
    std::fs::remove_file(graph.index_path()).expect("remove the index");
    let mut reopened = graph.open();
    assert_eq!(positions(&mut reopened, "(journal)", 50), first);
}

#[test]
fn an_unsupported_query_is_an_error_and_not_an_empty_answer() {
    let graph = task_graph();
    let mut opened = graph.open();

    for text in [
        "[:find ?b :where [?b :block/text]]",
        "(sort-by created-at)",
        "(and)",
        "unbalanced (",
    ] {
        match opened.query(text, &context(10)) {
            Err(EngineError::Query(QueryError::Syntax { .. }))
            | Err(EngineError::Query(QueryError::Unsupported { .. })) => {}
            other => panic!("{text:?} must be refused, got {other:?}"),
        }
    }
}

#[test]
fn a_syntax_error_costs_no_scan() {
    // The parse runs before the freshness policy, so an unusable query cannot observe the graph.
    // A page that appeared after the scan is the evidence: `CheckNow` would project it, and a
    // rejected query must not.
    let graph = task_graph();
    let mut opened = graph.open();
    let before = opened.metadata().expect("read metadata").value().revision();

    graph.write("pages/late.md", "- written after the scan\n");
    assert!(matches!(
        opened.query("(and)", &context(10)),
        Err(EngineError::Query(QueryError::Syntax { .. }))
    ));

    let after = opened.metadata().expect("read metadata").value().revision();
    assert_eq!(
        before, after,
        "a refused query must not advance the revision"
    );
    assert!(opened
        .pages()
        .expect("list pages")
        .value()
        .iter()
        .all(|page| page.name() != "late"));
}

#[test]
fn a_query_answer_can_be_written_through_the_selector_it_carries() {
    let graph = task_graph();
    let mut opened = graph.open();

    let matched = blocks(&mut opened, "(task DONE)", 10);
    let block = matched.first().expect("the DONE block answers");
    assert_eq!(
        block.selector().kind(),
        BlockIdentityKind::Ephemeral,
        "no block here states an id::"
    );
    let selector = block.selector().clone();
    assert!(matches!(selector, BlockSelector::Ephemeral(_)));

    opened
        .update_block(&selector, "DONE shipped it")
        .expect("the selector a query answered addresses the block it named");

    let after = blocks(&mut opened, "(task DONE)", 10);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].text(), "DONE shipped it");
    assert_ne!(
        after[0].selector(),
        &selector,
        "an edit replaces the page's bytes, so that page's ephemeral ids are retired (ADR-0012)"
    );
    let retired = opened
        .block(&selector)
        .expect("a retired id is a miss, not a read failure");
    assert!(
        retired.value().is_none(),
        "the retired id names nothing rather than a neighbour"
    );
}

#[test]
fn an_edit_retires_the_ids_of_that_page_only() {
    let graph = task_graph();
    let mut opened = graph.open();

    let project = blocks(&mut opened, "(and (task TODO) (page project))", 10);
    let done = blocks(&mut opened, "(and (task TODO) (page done))", 10);
    let untouched = done[0].selector().clone();

    opened
        .update_block(project[0].selector(), "TODO write the query executor")
        .expect("write one page");

    let after = blocks(&mut opened, "(task TODO)", 10);
    let project_after = after
        .iter()
        .find(|block| block.page() == "project")
        .expect("the edited task answers");
    assert_ne!(
        project_after.selector(),
        project[0].selector(),
        "the edited page's ids are retired"
    );
    let done_after = after
        .iter()
        .find(|block| block.page() == "done")
        .expect("the other page's task answers");
    assert_eq!(
        done_after.selector(),
        &untouched,
        "another page's ids survive"
    );
}

#[test]
fn a_parsed_expression_answers_what_the_text_answers() {
    let graph = task_graph();
    let mut opened = graph.open();

    let text = "(and (task TODO) (not (page done)))";
    let expr = QueryExpr::parse(text).expect("parse the query");
    let from_text = positions(&mut opened, text, 10);
    let from_expr = opened
        .query_expr(&expr, &context(10))
        .expect("run the parsed query");
    let from_expr: Vec<(String, u32)> = match from_expr.value().outcome() {
        QueryOutcome::Blocks(blocks) => blocks
            .iter()
            .map(|block| (block.page().to_owned(), block.preorder()))
            .collect(),
        other => panic!("expected blocks, got {other:?}"),
    };
    assert_eq!(from_text, from_expr);
}

#[test]
fn a_page_level_query_answers_pages() {
    let graph = TempGraph::new("query-pages");
    graph.write("pages/alpha.md", "tags:: [[kind]]\n\n- one\n");
    graph.write("pages/beta.md", "type:: [[kind]]\n\n- two\n");
    graph.write("pages/gamma.md", "- three\n");
    let mut opened = graph.open();

    let read = opened
        .query("(page-tags kind)", &context(10))
        .expect("query the pages");
    match read.value().outcome() {
        QueryOutcome::Pages(pages) => {
            assert_eq!(
                pages.iter().map(|page| page.name()).collect::<Vec<_>>(),
                ["alpha"],
                "only the page whose tags:: names the tag answers"
            );
            assert_eq!(pages[0].kind(), PageKind::Page);
            assert_eq!(pages[0].journal_date(), None);
        }
        other => panic!("expected pages, got {other:?}"),
    }

    let property = opened
        .query("(page-property type kind)", &context(10))
        .expect("query by page property");
    match property.value().outcome() {
        QueryOutcome::Pages(pages) => {
            assert_eq!(
                pages.iter().map(|page| page.name()).collect::<Vec<_>>(),
                ["beta"]
            );
        }
        other => panic!("expected pages, got {other:?}"),
    }
}

#[test]
fn a_between_query_bounds_the_journals_it_answers() {
    let graph = TempGraph::new("query-between");
    graph.write("journals/2026_08_10.md", "- ten\n");
    graph.write("journals/2026_08_12.md", "- twelve\n");
    graph.write("journals/2026_08_15.md", "- fifteen\n");
    let mut opened = graph.open();

    assert_eq!(
        positions(&mut opened, "(between -7d today)", 20),
        [
            ("Aug 10th, 2026".to_owned(), 0),
            ("Aug 12th, 2026".to_owned(), 0),
        ],
        "the interval is closed and measured from the context's day"
    );

    assert_eq!(
        positions(
            &mut opened,
            "(between [[Aug 12th, 2026]] [[Aug 12th, 2026]])",
            20
        ),
        [("Aug 12th, 2026".to_owned(), 0)]
    );

    match opened.query("(between [[no such day]] today)", &context(10)) {
        Err(EngineError::Query(QueryError::UnresolvedDate { .. })) => {}
        other => panic!("a name no journal holds must be refused, got {other:?}"),
    }

    assert_eq!(
        positions(&mut opened, "(between today -7d)", 20),
        positions(&mut opened, "(between -7d today)", 20)
    );
}

#[test]
fn a_property_query_reads_the_last_line_under_a_key() {
    let graph = TempGraph::new("query-property");
    graph.write(
        "pages/tasks.md",
        "- status:: todo\n  status:: done\n  a block whose key is stated twice\n- status:: todo\n  another block\n",
    );
    let mut opened = graph.open();

    assert_eq!(
        positions(&mut opened, "(property status done)", 10),
        [("tasks".to_owned(), 0)],
        "the last line under the key wins, like every other property read"
    );
    assert_eq!(
        blocks(&mut opened, "(property status)", 10).len(),
        2,
        "a key with no value asks whether the key is stated"
    );
}

#[test]
fn a_freshness_policy_reaches_the_query() {
    let graph = task_graph();
    let mut opened = graph.open();
    assert_eq!(blocks(&mut opened, "(task TODO)", 10).len(), 2);

    graph.write("pages/late.md", "- TODO written after the scan\n");

    let unscanned = opened
        .reads(Freshness::LastScan)
        .query("(task TODO)", &context(10))
        .expect("a stale-but-readable index answers under this policy");
    match unscanned.value().outcome() {
        QueryOutcome::Blocks(blocks) => {
            assert_eq!(
                blocks.len(),
                2,
                "the file written after the scan is not there yet"
            )
        }
        other => panic!("expected blocks, got {other:?}"),
    }

    let fresh = opened
        .reads(Freshness::CheckNow)
        .query("(task TODO)", &context(10))
        .expect("a check scans and answers");
    match fresh.value().outcome() {
        QueryOutcome::Blocks(blocks) => assert_eq!(blocks.len(), 3),
        other => panic!("expected blocks, got {other:?}"),
    }
}

#[test]
fn a_query_result_states_whether_it_was_cut_short() {
    let graph = task_graph();
    let mut opened = graph.open();

    let all = opened
        .query("(task TODO)", &context(10))
        .expect("query")
        .into_value();
    assert!(!all.truncated());

    let none = opened
        .query("(task TODO)", &context(0))
        .expect("a zero limit asks for nothing")
        .into_value();
    assert!(
        none.truncated(),
        "a zero limit still reports that more exist"
    );
    match none.outcome() {
        QueryOutcome::Blocks(blocks) => assert!(blocks.is_empty()),
        other => panic!("expected blocks, got {other:?}"),
    }
}

/// Type check: a result's outcome is the only place a caller reads blocks or pages from.
#[allow(dead_code)]
fn _outcome_type(_: &QueryResult) {}

#[test]
fn a_namespace_binds_every_placeholder_it_writes() {
    // The namespace leaf writes `?` three times and must bind three values: a mismatch would make
    // SQLite compare the namespace against the wrong one and answer a neighbouring set.
    let graph = TempGraph::new("query-namespace");
    graph.write("pages/area.md", "- the namespace page\n");
    graph.write("pages/area___one.md", "- a page under it\n");
    graph.write("pages/unrelated.md", "- not in the namespace\n");
    let mut opened = graph.open();

    // `(namespace …)` is page-level, so its answer is pages; the three bound values are what this
    // test is about, and a wrong binding would answer `unrelated` or nothing at all.
    match opened
        .query("(namespace area)", &context(20))
        .expect("query the namespace")
        .into_value()
        .outcome()
        .clone()
    {
        QueryOutcome::Pages(pages) => assert_eq!(
            pages.iter().map(|page| page.name()).collect::<Vec<_>>(),
            ["area", "area/one"],
            "the page and everything under it, and nothing else"
        ),
        other => panic!("expected pages, got {other:?}"),
    }
    assert_eq!(
        positions(&mut opened, "(and (namespace area) (journal))", 20),
        Vec::<(String, u32)>::new(),
        "promoted into a block query it is a filter over those pages"
    );
}

#[test]
fn a_property_value_is_interpreted_for_its_own_key_only() {
    // A block states two properties, and only one of them names the page. A naive
    // "join every reference of the block" would answer this block for both keys.
    let graph = TempGraph::new("query-property-scope");
    graph.write(
        "pages/mixed.md",
        "- type:: [[book]]\n  status:: [[review]]\n  a block with two properties\n",
    );
    let mut opened = graph.open();

    assert_eq!(
        positions(&mut opened, "(property type book)", 10),
        [("mixed".to_owned(), 0)],
        "the type key names the page"
    );
    assert!(
        blocks(&mut opened, "(property status book)", 10).is_empty(),
        "the status key does not, so the block must not answer for it"
    );
}

#[test]
fn a_property_query_answers_every_attribute_of_the_block_that_matches() {
    // The opposite failure: a block whose *other* property names the page still matches when the
    // queried key is the one that names it, and the answer is the block, not the property line.
    let graph = TempGraph::new("query-property-answer");
    graph.write(
        "pages/two.md",
        "- tags:: alpha, beta\n  note:: plain\n  a block with a list property\n",
    );
    let mut opened = graph.open();

    assert_eq!(
        positions(&mut opened, "(property tags beta)", 10),
        [("two".to_owned(), 0)],
        "one element of a built-in list key matches"
    );
    assert!(
        blocks(&mut opened, "(property note beta)", 10).is_empty(),
        "a key that is not a list key matches on its own text only"
    );
}

#[test]
fn a_page_predicate_inside_a_block_query_narrows_to_those_pages() {
    let graph = TempGraph::new("query-promotion");
    graph.write(
        "pages/alpha.md",
        "kind:: [[work]]\n\n- alpha one\n- alpha two\n",
    );
    graph.write("pages/beta.md", "kind:: [[home]]\n\n- beta one\n");
    let mut opened = graph.open();

    assert_eq!(
        positions(
            &mut opened,
            "(and (page-property kind work) (task TODO))",
            10
        ),
        Vec::<(String, u32)>::new(),
        "the promotion is a filter, not a match-everything: no block is a TODO"
    );
    assert_eq!(
        positions(
            &mut opened,
            "(and (page-property kind work) (page alpha))",
            10
        ),
        [("alpha".to_owned(), 0), ("alpha".to_owned(), 1)],
        "the page predicate contributes the blocks of the pages it matched"
    );
}

#[test]
fn a_reversed_interval_sorts_its_ends_like_logseq() {
    let graph = TempGraph::new("query-reversed");
    graph.write("journals/2026_08_10.md", "- ten\n");
    graph.write("journals/2026_08_20.md", "- twenty\n");
    let mut opened = graph.open();

    assert_eq!(
        blocks(
            &mut opened,
            "(between [[Aug 20th, 2026]] [[Aug 10th, 2026]])",
            10
        )
        .len(),
        2,
        "Logseq sorts the interval endpoints"
    );
    assert_eq!(
        positions(
            &mut opened,
            "(between [[Aug 10th, 2026]] [[Aug 20th, 2026]])",
            20
        )
        .len(),
        2,
        "the same two days, the right way round, answer both"
    );
}

#[test]
fn nested_property_sets_preserve_the_boolean_tree() {
    let graph = TempGraph::new("query-nested-properties");
    graph.write("pages/work.md", "kind:: [[work]]\n\n- TODO [#A] first\n  status:: open\n- DOING [#B] second\n  status:: closed\n- DONE [#C] third\n  status:: open\n");
    graph.write(
        "pages/home.md",
        "kind:: [[home]]\n\n- TODO [#B] fourth\n  status:: open\n",
    );
    let mut opened = graph.open();
    for (query, expected) in [
        ("(task todo doing)", 3),
        ("(priority a b)", 3),
        ("(and (task todo doing) (priority a b) (property status open) (page-property kind work))", 1),
        ("(and (property status open) (or (priority a) (priority b)))", 2),
        ("(and (priority b) (or (property status open) (property status closed)))", 2),
        ("(not (or (property status open) (property status closed)))", 0),
        ("(and (property status open) (not (and (task TODO) (page-property kind work))))", 2),
        ("(and (property status missing) (task todo))", 0),
        ("(property status Open)", 3),
    ] {
        assert_eq!(blocks(&mut opened, query, 20).len(), expected, "{query}");
    }
    let read = opened.query("(or (page-property kind work) (and (page-property kind home) (not (page-property kind work))))", &context(20)).expect("page combination");
    match read.value().outcome() {
        QueryOutcome::Pages(pages) => assert_eq!(
            pages.iter().map(|p| p.name()).collect::<Vec<_>>(),
            ["home", "work"]
        ),
        other => panic!("expected pages, got {other:?}"),
    }
}

#[test]
fn unbracketed_property_values_match_by_page_name_case() {
    let graph = TempGraph::new("query-property-page-name-case");
    graph.write(
        "pages/library.md",
        "type:: Book\n\n- first\n  type:: Book\n- second\n  type:: Article\n",
    );
    let mut opened = graph.open();
    assert_eq!(
        positions(&mut opened, "(property type book)", 20),
        [("library".to_owned(), 0)]
    );
    assert!(blocks(&mut opened, "(property type missing)", 20).is_empty());
    let read = opened
        .query("(page-property type book)", &context(20))
        .expect("page property");
    match read.value().outcome() {
        QueryOutcome::Pages(pages) => assert_eq!(
            pages.iter().map(|p| p.name()).collect::<Vec<_>>(),
            ["library"]
        ),
        other => panic!("expected pages, got {other:?}"),
    }
}

#[test]
fn integer_journal_days_and_relative_dates_only_select_journal_blocks() {
    let graph = TempGraph::new("query-journal-integers");
    graph.write("journals/2026_08_12.md", "- TODO yesterday\n");
    graph.write("journals/2026_08_13.md", "- DOING today\n");
    graph.write("journals/2026_08_14.md", "- TODO tomorrow\n");
    graph.write("pages/20260813.md", "- TODO not a journal\n");
    let mut opened = graph.open();
    for query in [
        "(between 20260812 20260814)",
        "(between yesterday tomorrow)",
        "(between +1w -7d)",
    ] {
        assert_eq!(blocks(&mut opened, query, 20).len(), 3, "{query}");
    }
    assert_eq!(
        blocks(
            &mut opened,
            "(and (between 20260813 20260813) (task doing))",
            20
        )
        .len(),
        1
    );
    assert!(blocks(&mut opened, "(between 20260101 20260102)", 20).is_empty());
    assert!(matches!(
        opened.query("(between 20260230 today)", &context(20)),
        Err(EngineError::Query(QueryError::InvalidArgument { .. }))
    ));
    assert!(matches!(
        opened.query(
            "[:find ?b :where [?b :block/marker \"TODO\"]]",
            &context(20)
        ),
        Err(EngineError::Query(QueryError::Unsupported { .. }))
    ));
}
