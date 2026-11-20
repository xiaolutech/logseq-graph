//! Content search through the public engine API.
//!
//! These tests pin what a caller can observe: a hit names its page as written and the block's
//! preorder position, carries a bounded verbatim snippet, obeys the limit in SQL, and follows the
//! freshness policy. They also pin the two deliberate limits of the rule: the match is a literal
//! substring, and the search fold is a per-character Unicode fold rather than the whole-string
//! Unicode page-name key.

mod support;

use logseq_graph::domain::{
    BlockId, BlockSelector, JournalDate, QueryContext, QueryOutcome, SearchHit,
};
use logseq_graph::engine::{Freshness, Graph};

use support::TempGraph;

/// The day every relative date in these tests counts from; the searches here use none.
fn today() -> JournalDate {
    JournalDate::new(2026, 8, 13).expect("a day of the calendar")
}

/// Hits of `query` under the indexed ranking threshold or the scan fallback.
fn search(graph: &mut Graph, query: &str, limit: u32) -> Vec<SearchHit> {
    graph
        .search(query, limit)
        .unwrap_or_else(|error| panic!("search {query:?}: {error}"))
        .into_value()
}

/// `(page, preorder)` of every hit, sorted so an unspecified scan order cannot make a test flaky.
fn locations(hits: &[SearchHit]) -> Vec<(&str, u32)> {
    let mut out: Vec<(&str, u32)> = hits
        .iter()
        .map(|hit| (hit.page(), hit.preorder()))
        .collect();
    out.sort();
    out
}

#[test]
fn a_hit_locates_the_block_in_the_page_tree() {
    let graph = TempGraph::new("search-locate");
    graph.write(
        "pages/Alpha Page.md",
        "- The Quick brown fox\n\t- nested NEEDLE line\n- last\n",
    );
    graph.write("pages/Beta.md", "- needle in beta\n");

    let mut graph = graph.open();
    let mut hits = search(&mut graph, "nEeDlE", 10);
    hits.sort_by_key(|hit| (hit.page().to_owned(), hit.preorder()));

    // One row per block, and the page name is the spelling the file produced, not the query.
    assert_eq!(locations(&hits), [("Alpha Page", 1), ("Beta", 0)]);
    // Both blocks are short, so the whole reconstructed block text is the snippet: no marker, no
    // alignment indentation, and the match keeps the spelling the text used.
    assert_eq!(hits[0].snippet(), "nested NEEDLE line");
    assert_eq!(hits[1].snippet(), "needle in beta");

    // The preorder is the position a page-tree read reports, which is what lets a caller relocate
    // the block at the revision the search answered with.
    let tree = graph
        .page_tree("Alpha Page")
        .expect("read page tree")
        .into_value()
        .expect("page exists");
    assert_eq!(
        tree.blocks()[0].children()[0].preorder(),
        hits[0].preorder()
    );
}

#[test]
fn a_hit_carries_the_selector_the_query_answer_names_the_same_block() {
    let graph = TempGraph::new("search-selector");
    graph.write(
        "pages/Alpha.md",
        "- NEEDLE without an id\n- NEEDLE with an id\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n",
    );

    let mut graph = graph.open();
    let hits = search(&mut graph, "NEEDLE", 10);
    assert_eq!(hits.len(), 2, "both blocks answer: {hits:?}");

    // The block that states an `id::` answers by it, and the selector is the same value the block
    // read answers with: one identity, wherever the block is reached from.
    let durable = hits
        .iter()
        .find(|hit| hit.preorder() == 1)
        .expect("the second block answers");
    assert_eq!(
        durable.selector(),
        &BlockSelector::Durable(
            BlockId::parse("01234567-89ab-cdef-0123-456789abcdef").expect("a valid id")
        )
    );
    let read = graph
        .block(durable.selector())
        .expect("read by the selector the hit carries")
        .into_value()
        .expect("the hit's selector names an existing block");
    assert_eq!(read.page(), "Alpha");
    assert_eq!(read.preorder(), 1);

    // A block without an `id::` answers by the ephemeral id the projection minted, and that
    // selector addresses the same block a query answer for the same text does.
    let ephemeral = hits
        .iter()
        .find(|hit| hit.preorder() == 0)
        .expect("the first block answers");
    assert!(matches!(ephemeral.selector(), BlockSelector::Ephemeral(_)));
    let queried = graph
        .query("(page Alpha)", &QueryContext::new(today(), 10))
        .expect("query the same page")
        .into_value();
    let QueryOutcome::Blocks(blocks) = queried.outcome() else {
        panic!("a block-level query answers blocks");
    };
    let same = blocks
        .iter()
        .find(|block| block.preorder() == 0)
        .expect("the same block answers the query");
    assert_eq!(
        ephemeral.selector(),
        same.selector(),
        "search and query name one block with one selector"
    );
}

#[test]
fn a_long_block_yields_a_bounded_snippet_around_the_match() {
    let graph = TempGraph::new("search-snippet");
    let text = format!("- {}TargetWord{}\n", "a".repeat(200), "b".repeat(200));
    graph.write("pages/Long.md", text);

    let mut graph = graph.open();
    let hits = search(&mut graph, "targetword", 5);

    assert_eq!(hits.len(), 1);
    let snippet = hits[0].snippet();
    assert!(
        snippet.contains("TargetWord"),
        "the match must stay: {snippet}"
    );
    assert!(
        snippet.starts_with('…') && snippet.ends_with('…'),
        "both cut edges must be marked: {snippet}"
    );
    // 48 characters on each side, the ten-character query, and two ellipses.
    assert!(
        snippet.chars().count() <= 2 * 48 + "targetword".len() + 2,
        "the snippet must stay bounded, got {} characters",
        snippet.chars().count()
    );
}

#[test]
fn the_limit_bounds_the_hits_and_a_zero_limit_asks_for_nothing() {
    let graph = TempGraph::new("search-limit");
    for index in 0..4 {
        graph.write(
            &format!("pages/Page {index}.md"),
            "- needle needle twice in one block\n",
        );
    }

    let mut graph = graph.open();

    // A block with two occurrences is still one hit: the row is a block, not a match.
    assert_eq!(search(&mut graph, "needle", 2).len(), 2);
    assert_eq!(search(&mut graph, "needle", 100).len(), 4);
    assert!(search(&mut graph, "needle", 0).is_empty());
    assert!(
        search(&mut graph, "", 10).is_empty(),
        "an empty query is not a wildcard"
    );
    // A literal substring, not a word: the needle may start inside a word.
    assert_eq!(search(&mut graph, "eedl", 100).len(), 4);
}

#[test]
fn unicode_case_folds_in_both_a_search_and_a_query_content_word() {
    let sandbox = TempGraph::new("search-unicode");
    sandbox.write("pages/Page.md", "- ÄRGER und Straße\n- plain text\n");
    let mut graph = sandbox.open();

    // The search read folds both sides through the same rule, so either spelling finds the block
    // and the snippet keeps the spelling the file wrote.
    for needle in ["ärger", "Ärger", "ÄRGER"] {
        let hits = search(&mut graph, needle, 10);
        assert_eq!(locations(&hits), [("Page", 0)], "needle {needle:?}");
        assert!(hits[0].snippet().contains("ÄRGER"));
    }
    assert!(search(&mut graph, "STRASSE", 10).is_empty(), "no ss fold");

    // A bare word in a simple query is the same content match through the same fold.
    let queried = graph
        .query("ärger", &QueryContext::new(today(), 10))
        .expect("query the folded text")
        .into_value();
    let QueryOutcome::Blocks(blocks) = queried.outcome() else {
        panic!("a block-level query answers blocks");
    };
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].preorder(), 0);
}

#[test]
fn search_follows_the_freshness_policy_instead_of_the_files() {
    let sandbox = TempGraph::new("search-freshness");
    sandbox.write("pages/Page.md", "- alpha only\n");

    let mut graph = sandbox.open();
    assert!(search(&mut graph, "beta", 10).is_empty());

    // Another process changes the file. The default LastScan read serves what the last scan stored,
    // so the new text is not searchable and the old text still is.
    sandbox.write("pages/Page.md", "- beta arrived\n");
    assert!(search(&mut graph, "beta", 10).is_empty());
    assert_eq!(search(&mut graph, "alpha", 10).len(), 1);

    // CheckNow scans first, so the same handle then answers from the new text.
    let hits = graph
        .reads(Freshness::CheckNow)
        .search("beta", 10)
        .expect("search after a check")
        .into_value();
    assert_eq!(locations(&hits), [("Page", 0)]);
    assert!(search(&mut graph, "alpha", 10).is_empty());
}

#[test]
fn block_text_is_the_own_content_without_marker_or_continuation_indentation() {
    let graph = TempGraph::new("search-continuation");
    graph.write(
        "pages/Page.md",
        "- first line\n  second line\n\t- child needle\n",
    );

    let mut graph = graph.open();

    // The continuation belongs to the first block and is stored without its alignment indentation.
    let hits = search(&mut graph, "first line\nsecond line", 10);
    assert_eq!(locations(&hits), [("Page", 0)]);
    assert_eq!(hits[0].snippet(), "first line\nsecond line");

    // The child's text is searched on its own; it does not inherit the parent's lines.
    assert_eq!(
        locations(&search(&mut graph, "second line", 10)),
        [("Page", 0)]
    );
    assert_eq!(
        locations(&search(&mut graph, "child needle", 10)),
        [("Page", 1)]
    );
}

#[test]
fn chinese_substrings_match_at_every_query_length() {
    let sandbox = TempGraph::new("search-chinese");
    sandbox.write(
        "pages/Page.md",
        "- 今天学习了 Rust 编程和中文书\n- 今 天 学 习 了\n",
    );
    let mut graph = sandbox.open();
    for needle in ["书", "中文", "今天学习", "学习了 Rust", "rUsT"] {
        assert_eq!(
            locations(&search(&mut graph, needle, 10)),
            [("Page", 0)],
            "{needle}"
        );
    }
    assert!(search(&mut graph, "不存在", 10).is_empty());
}

#[test]
fn short_cjk_phrases_require_contiguous_characters() {
    let sandbox = TempGraph::new("search-short-cjk-literal");
    sandbox.write(
        "pages/Page.md",
        "- 中文书\n- 中x文\n- 中，文\n- 中 文\n- 中、文\n- 中・文\n- a中\n",
    );
    let mut graph = sandbox.open();
    assert_eq!(locations(&search(&mut graph, "中文", 10)), [("Page", 0)]);
    assert_eq!(locations(&search(&mut graph, "书", 10)), [("Page", 0)]);
    assert_eq!(locations(&search(&mut graph, "a中", 10)), [("Page", 6)]);
    assert_eq!(locations(&search(&mut graph, "A中", 10)), [("Page", 6)]);
    assert_eq!(locations(&search(&mut graph, "，文", 10)), [("Page", 2)]);
    assert!(search(&mut graph, "齉", 10).is_empty());
    assert!(search(&mut graph, "齉齾", 10).is_empty());
}

#[test]
fn short_cjk_search_keeps_script_variants_and_combining_marks_literal() {
    let sandbox = TempGraph::new("search-short-cjk-scripts");
    let needles = [
        "かな",
        "カナ",
        "が",
        "が",
        "ｶﾞ",
        "ガ",
        "한글",
        "가",
        "가",
        "﨑",
        "\u{3400}",
        "\u{20000}",
        "\u{2a700}",
        "\u{2b740}",
        "\u{2b820}",
        "\u{2ceb0}",
        "\u{2ebf0}",
        "\u{2f800}",
        "\u{30000}",
        "\u{31350}",
        "\u{323b0}",
        "\u{33479}",
        "々",
        "〇",
    ];
    let text = needles
        .iter()
        .map(|needle| format!("- {needle}\n"))
        .collect::<String>();
    sandbox.write("pages/Page.md", text);
    let mut graph = sandbox.open();
    for (preorder, needle) in needles.into_iter().enumerate() {
        let hits = search(&mut graph, needle, 100);
        assert_eq!(locations(&hits), [("Page", preorder as u32)], "{needle:?}");
        assert_eq!(hits[0].snippet(), needle);
    }
}

#[test]
fn short_cjk_hits_are_ranked_before_applying_the_limit() {
    let sandbox = TempGraph::new("search-short-cjk-rank");
    sandbox.write(
        "pages/Weak.md",
        format!("- 书中文 {}\n", "天地".repeat(100)),
    );
    sandbox.write("pages/Strong.md", "- 书书书中文中文中文\n");
    let mut graph = sandbox.open();
    for needle in ["书", "中文"] {
        let hits = search(&mut graph, needle, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].page(), "Strong", "{needle}");
        assert_eq!(search(&mut graph, needle, 1)[0].page(), "Strong");
    }
}

#[test]
fn rescanning_rewrites_and_deletes_short_cjk_hits() {
    let sandbox = TempGraph::new("search-short-cjk-rescan");
    sandbox.write("pages/Page.md", "- 中文书\n");
    let mut graph = sandbox.open();
    for needle in ["书", "中文"] {
        assert_eq!(search(&mut graph, needle, 10).len(), 1);
    }
    sandbox.write("pages/Page.md", "- 新的文字\n");
    graph
        .reads(Freshness::CheckNow)
        .search("新的", 10)
        .expect("rescan");
    for needle in ["书", "中文"] {
        assert!(search(&mut graph, needle, 10).is_empty());
    }
    assert_eq!(search(&mut graph, "新", 10).len(), 1);
    assert_eq!(search(&mut graph, "新的", 10).len(), 1);
    sandbox.remove("pages/Page.md");
    let hits = graph
        .reads(Freshness::CheckNow)
        .search("新", 10)
        .expect("rescan deletion")
        .into_value();
    assert!(hits.is_empty());
    assert!(search(&mut graph, "新的", 10).is_empty());
}

#[test]
fn fts_syntax_is_literal_text_not_a_query_language() {
    let sandbox = TempGraph::new("search-literal");
    sandbox.write(
        "pages/Page.md",
        "- literal \" AND * - end\n- literal AND end\n",
    );
    let mut graph = sandbox.open();
    for needle in ["\" AND * -", "AND *", "* - end", "literal \"", "\" AND"] {
        assert_eq!(
            locations(&search(&mut graph, needle, 10)),
            [("Page", 0)],
            "{needle}"
        );
    }
    assert!(search(&mut graph, "\" OR absent*", 10).is_empty());
}

#[test]
fn ranked_search_returns_the_strongest_match_before_applying_the_limit() {
    let sandbox = TempGraph::new("search-rank");
    sandbox.write(
        "pages/Weak.md",
        format!("- needle {}\n", "unrelated ".repeat(100)),
    );
    sandbox.write("pages/Strong.md", "- needle needle needle\n");
    let mut graph = sandbox.open();
    let hits = search(&mut graph, "needle", 10);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].page(), "Strong");
    assert_eq!(search(&mut graph, "needle", 1)[0].page(), "Strong");
}

#[test]
fn rescanning_rewrites_and_removes_fts_rows() {
    let sandbox = TempGraph::new("search-fts-rescan");
    sandbox.write(
        "pages/Page.md",
        "- 今天学习了 Rust 编程\n- removable marker\n",
    );
    let mut graph = sandbox.open();
    assert_eq!(search(&mut graph, "今天学习", 10).len(), 1);
    assert_eq!(search(&mut graph, "removable", 10).len(), 1);
    sandbox.write("pages/Page.md", "- 明天继续编程\n");
    let hits = graph
        .reads(Freshness::CheckNow)
        .search("明天继续", 10)
        .expect("search rewritten page")
        .into_value();
    assert_eq!(locations(&hits), [("Page", 0)]);
    assert!(search(&mut graph, "今天学习", 10).is_empty());
    assert!(search(&mut graph, "removable", 10).is_empty());
}
