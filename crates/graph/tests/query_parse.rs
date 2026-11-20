//! The simple-query grammar, as text in and a typed tree out.
//!
//! These tests pin the whole contract the rest of the crate depends on: which forms exist, how they
//! spell their arguments, which spellings are the same predicate, how a failure is reported, and what
//! cannot be written at all. Nothing here touches a graph, a database, or a clock — a query is parsed
//! on its own, which is exactly what makes the grammar testable by fixture.
//!
//! The one rule worth stating twice: a query that cannot be answered is an error, never an empty
//! expression. An empty answer reads as "the graph has no such block", so a refused query must not be
//! able to look like one.

use logseq_graph::domain::{DateOperand, JournalDate, Priority, QueryError, QueryExpr, TaskMarker};

// --- helpers ---------------------------------------------------------------------------------------

/// Parse a query that must parse.
fn parse(input: &str) -> QueryExpr {
    QueryExpr::parse(input).unwrap_or_else(|error| panic!("parse {input:?}: {error}"))
}

/// Parse a query that must not parse.
fn failure(input: &str) -> QueryError {
    match QueryExpr::parse(input) {
        Ok(expr) => panic!("{input:?} parsed as {expr:?}, and it should have been refused"),
        Err(error) => error,
    }
}

fn syntax(input: &str) -> (usize, String) {
    match failure(input) {
        QueryError::Syntax { position, detail } => (position, detail),
        other => panic!("{input:?}: expected a syntax error, got {other:?}"),
    }
}

fn unsupported(input: &str) -> String {
    match failure(input) {
        QueryError::Unsupported { form } => form,
        other => panic!("{input:?}: expected an unsupported form, got {other:?}"),
    }
}

fn invalid(input: &str) -> (String, String) {
    match failure(input) {
        QueryError::InvalidArgument { form, argument, .. } => (form, argument),
        other => panic!("{input:?}: expected an invalid argument, got {other:?}"),
    }
}

/// The `detail` of an invalid-argument refusal: the sentence that names what the form accepts.
fn invalid_detail(input: &str) -> String {
    match failure(input) {
        QueryError::InvalidArgument { detail, .. } => detail,
        other => panic!("{input:?}: expected an invalid argument, got {other:?}"),
    }
}

fn content(text: &str) -> QueryExpr {
    QueryExpr::Content(text.to_owned())
}

fn page_ref(name: &str) -> QueryExpr {
    QueryExpr::PageRef(name.to_owned())
}

fn and(children: Vec<QueryExpr>) -> QueryExpr {
    QueryExpr::And(children)
}

fn or(children: Vec<QueryExpr>) -> QueryExpr {
    QueryExpr::Or(children)
}

fn not(inner: QueryExpr) -> QueryExpr {
    QueryExpr::Not(Box::new(inner))
}

fn property(key: &str, value: Option<&str>) -> QueryExpr {
    QueryExpr::Property {
        key: key.to_owned(),
        value: value.map(str::to_owned),
    }
}

fn page_property(key: &str, value: Option<&str>) -> QueryExpr {
    QueryExpr::PageProperty {
        key: key.to_owned(),
        value: value.map(str::to_owned),
    }
}

fn between(start: DateOperand, end: DateOperand) -> QueryExpr {
    QueryExpr::Between(start, end)
}

#[test]
fn journal_day_integers_are_validated_as_calendar_dates() {
    assert_eq!(
        parse("(between 20240229 20240301)"),
        between(
            DateOperand::Day(JournalDate::new(2024, 2, 29).expect("leap day")),
            DateOperand::Day(JournalDate::new(2024, 3, 1).expect("day")),
        )
    );
    for day in ["20230229", "20241301", "20240100"] {
        assert_eq!(invalid(&format!("(between {day} today)")).0, "between");
    }
}

// --- forms -----------------------------------------------------------------------------------------

#[test]
fn every_form_lowers_to_the_variant_it_names() {
    let cases: Vec<(&str, QueryExpr)> = vec![
        ("(and [[a]] [[b]])", and(vec![page_ref("a"), page_ref("b")])),
        (
            "(or (page x) (journal))",
            or(vec![QueryExpr::Page("x".to_owned()), QueryExpr::Journal]),
        ),
        ("(journal)", QueryExpr::Journal),
        ("(page Logseq)", QueryExpr::Page("Logseq".to_owned())),
        ("(namespace a/b)", QueryExpr::Namespace("a/b".to_owned())),
        (
            "(page-tags #book #note)",
            QueryExpr::PageTags(vec!["book".to_owned(), "note".to_owned()]),
        ),
        (
            "(task TODO DOING)",
            QueryExpr::Task(vec![TaskMarker::Todo, TaskMarker::Doing]),
        ),
        (
            "(priority A C)",
            QueryExpr::Priority(vec![Priority::A, Priority::C]),
        ),
        ("(property type)", property("type", None)),
        ("(property :type book)", property("type", Some("book"))),
        ("(page-property :journal?)", page_property("journal?", None)),
        (
            "(page-property last_edited)",
            page_property("last_edited", None),
        ),
        ("[[a page]]", page_ref("a page")),
        ("#tag", page_ref("tag")),
        ("(page-ref x)", page_ref("x")),
        ("hello", content("hello")),
        ("\"hello world\"", content("hello world")),
        (
            "(between today tomorrow)",
            between(DateOperand::Today, DateOperand::OffsetDays(1)),
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(parse(input), expected, "parsing {input:?}");
    }
}

#[test]
fn form_names_are_case_insensitive_but_the_arguments_keep_their_spelling() {
    // A query is written by a person, so the spelling of a form name carries no meaning.
    assert_eq!(parse("(AND [[a]])"), parse("(and [[a]])"));
    assert_eq!(parse("(PaGe X)"), parse("(page X)"));
    assert_eq!(
        parse("(Between TODAY tomorrow)"),
        parse("(between today tomorrow)")
    );
    // A page name is not folded here: the executor compares it under the page-name key, and folding
    // it twice would mean two rules for one question.
    assert_eq!(parse("(page Logseq)"), QueryExpr::Page("Logseq".to_owned()));
    assert_ne!(parse("(page Logseq)"), parse("(page logseq)"));
}

#[test]
fn task_and_priority_words_fold_case_without_a_second_spelling() {
    for spelling in ["TODO", "todo", "Todo", "tOdO"] {
        assert_eq!(
            parse(&format!("(task {spelling})")),
            QueryExpr::Task(vec![TaskMarker::Todo]),
            "spelling {spelling}"
        );
    }
    for spelling in ["A", "a"] {
        assert_eq!(
            parse(&format!("(priority {spelling})")),
            QueryExpr::Priority(vec![Priority::A]),
            "spelling {spelling}"
        );
    }
}

#[test]
fn page_ref_tag_and_page_ref_form_are_one_predicate() {
    assert_eq!(parse("[[book]]"), page_ref("book"));
    assert_eq!(parse("#book"), page_ref("book"));
    assert_eq!(parse("(page-ref book)"), page_ref("book"));
    // A page name may hold a space, and only the bracket spelling can say so.
    assert_eq!(parse("[[book list]]"), page_ref("book list"));
}

#[test]
fn adjacent_top_level_expressions_are_an_implicit_and() {
    assert_eq!(
        parse("[[a]] [[b]]"),
        and(vec![page_ref("a"), page_ref("b")])
    );
    assert_eq!(
        parse("(task TODO) \"buy milk\""),
        and(vec![
            QueryExpr::Task(vec![TaskMarker::Todo]),
            content("buy milk"),
        ])
    );
    // One expression is not wrapped: the tree a caller reads is the tree it wrote.
    assert_eq!(parse("[[a]]"), page_ref("a"));
}

#[test]
fn boolean_nesting_keeps_its_shape() {
    assert_eq!(
        parse("(or (and [[a]] (page x)) (not (journal)))"),
        or(vec![
            and(vec![page_ref("a"), QueryExpr::Page("x".to_owned())]),
            not(QueryExpr::Journal),
        ])
    );
    // A one-child `and` is kept, not collapsed: the shape is what the caller wrote.
    assert_eq!(parse("(and [[a]])"), and(vec![page_ref("a")]));
}

#[test]
fn a_multi_argument_not_is_the_negation_of_an_and() {
    let expected = not(and(vec![page_ref("a"), page_ref("b")]));
    assert_eq!(parse("(not [[a]] [[b]])"), expected);
    assert_eq!(parse("(not (and [[a]] [[b]]))"), expected);
    // One argument stays one argument: there is no `and` of one child inside a `not`.
    assert_eq!(parse("(not [[a]])"), not(page_ref("a")));
}

#[test]
fn strings_decode_escapes_and_keep_unicode() {
    let cases: Vec<(&str, &str)> = vec![
        (r#""a\nb""#, "a\nb"),
        (r#""a\tb""#, "a\tb"),
        (r#""a\rb""#, "a\rb"),
        (r#""a\"b""#, "a\"b"),
        (r#""a\\b""#, "a\\b"),
        ("\"日本語 テスト\"", "日本語 テスト"),
        ("\"\"", ""),
        // A quoted string is one argument whatever it holds, which is why it exists.
        ("\"(and a b)\"", "(and a b)"),
    ];
    for (input, expected) in cases {
        assert_eq!(parse(input), content(expected), "parsing {input}");
    }
    assert_eq!(
        parse("(property :type \"book shelf\")"),
        property("type", Some("book shelf"))
    );
}

#[test]
fn a_property_key_sheds_its_leading_colon_and_is_not_folded() {
    assert_eq!(parse("(property :type)"), property("type", None));
    assert_eq!(parse("(property type)"), property("type", None));
    assert_eq!(
        parse("(property :page-type book)"),
        property("page-type", Some("book"))
    );
    // `_` is not folded here either: `document::Property::normalized_key` is the one rule, and it
    // runs where the key is read.
    assert_eq!(
        parse("(property last_edited)"),
        property("last_edited", None)
    );
}

// --- dates -----------------------------------------------------------------------------------------

#[test]
fn every_date_operand_lowers_to_the_arithmetic_it_means() {
    let cases: Vec<(&str, DateOperand)> = vec![
        ("today", DateOperand::Today),
        ("now", DateOperand::Today),
        ("TODAY", DateOperand::Today),
        ("yesterday", DateOperand::OffsetDays(-1)),
        ("tomorrow", DateOperand::OffsetDays(1)),
        ("+3d", DateOperand::OffsetDays(3)),
        ("-2d", DateOperand::OffsetDays(-2)),
        ("+0d", DateOperand::OffsetDays(0)),
        // A week is exactly seven days; folding it here is what keeps a unit out of the tree.
        ("+2w", DateOperand::OffsetDays(14)),
        ("-1w", DateOperand::OffsetDays(-7)),
        // A month is not a fixed number of days, so it stays a month.
        ("+1m", DateOperand::OffsetMonths(1)),
        ("-1m", DateOperand::OffsetMonths(-1)),
        ("+1y", DateOperand::OffsetMonths(12)),
        ("-2y", DateOperand::OffsetMonths(-24)),
        (
            "[[Dec 5th, 2020]]",
            DateOperand::JournalPage("Dec 5th, 2020".to_owned()),
        ),
        (
            "[[2020_12_05]]",
            DateOperand::JournalPage("2020_12_05".to_owned()),
        ),
        (
            "[[2025-09-17 Wednesday]]",
            DateOperand::JournalPage("2025-09-17 Wednesday".to_owned()),
        ),
    ];
    for (spelling, expected) in cases {
        let query = format!("(between {spelling} {spelling})");
        assert_eq!(
            parse(&query),
            between(expected.clone(), expected),
            "parsing {query}"
        );
    }
}

#[test]
fn an_absolute_operand_is_kept_as_a_page_name_because_only_a_graph_can_resolve_it() {
    // The journal title format is configuration, so parsing it back into a day here would be a
    // second copy of that rule. The name travels to the executor, which resolves it against the
    // index and reports `UnresolvedDate` when it names nothing.
    assert_eq!(
        parse("(between [[Dec 5th, 2020]] today)"),
        between(
            DateOperand::JournalPage("Dec 5th, 2020".to_owned()),
            DateOperand::Today
        )
    );
}

// --- refusals --------------------------------------------------------------------------------------

#[test]
fn an_empty_query_is_refused() {
    // "Nothing matched" and "you forgot the query" must not look the same.
    for input in ["", "   \n\t "] {
        let (_, detail) = syntax(input);
        assert!(
            detail.contains("no expression"),
            "detail for {input:?}: {detail}"
        );
    }
}

#[test]
fn an_empty_argument_list_is_refused() {
    for input in [
        "(and)",
        "(or)",
        "(not)",
        "(task)",
        "(priority)",
        "(page-tags)",
        "(property)",
        "(page)",
        "(namespace)",
        "(between today)",
        "(between)",
    ] {
        let (_, detail) = syntax(input);
        assert!(detail.contains("takes"), "detail for {input:?}: {detail}");
    }
}

#[test]
fn an_unclosed_form_string_or_reference_is_refused() {
    for input in [
        "(and [[a]]",
        "(and (or x)",
        "\"abc",
        "[[abc",
        "(and (page x)",
    ] {
        let (position, detail) = syntax(input);
        assert!(
            detail.contains("never closed"),
            "detail for {input:?}: {detail}"
        );
        assert!(position <= input.len(), "position for {input:?}");
    }
}

#[test]
fn an_empty_form_or_reference_is_refused() {
    assert!(syntax("()").1.contains("no name"));
    assert!(syntax("[[]]").1.contains("names no page"));
    assert!(syntax("# x").1.contains("names no tag"));
}

#[test]
fn a_stray_closing_delimiter_is_refused() {
    for input in [")", "]", "}", "[[a]] )"] {
        let (_, detail) = syntax(input);
        assert!(
            detail.contains("closes a form that was not opened"),
            "detail for {input:?}: {detail}"
        );
    }
}

#[test]
fn an_argument_count_a_form_does_not_take_is_refused() {
    // A fixed-arity form given too many arguments is a syntax error rather than a dropped argument:
    // silently ignoring `(page a b)` would answer a question nobody asked.
    for input in [
        "(journal x)",
        "(page a b)",
        "(namespace a b)",
        "(page-ref a b)",
        "(property a b c)",
        "(between today yesterday tomorrow)",
    ] {
        let (_, detail) = syntax(input);
        assert!(detail.contains("takes"), "detail for {input:?}: {detail}");
    }
}

#[test]
fn nesting_past_the_limit_is_refused_and_nesting_at_it_is_not() {
    let nested = |levels: usize| {
        let mut query = "x".to_owned();
        for _ in 0..levels {
            query = format!("(and {query})");
        }
        query
    };
    // 32 levels is the documented limit, so 32 nests and 33 does not.
    parse(&nested(QueryExpr::MAX_DEPTH));
    let (_, detail) = syntax(&nested(QueryExpr::MAX_DEPTH + 1));
    assert!(
        detail.contains("nests deeper than"),
        "detail at one level past the limit: {detail}"
    );
}

#[test]
fn a_form_this_build_does_not_run_is_refused_by_name() {
    let cases: Vec<(&str, &str)> = vec![
        ("(sort-by created-at)", "sort-by"),
        ("(sample 5)", "sample"),
        ("(all-page-tags)", "all-page-tags"),
        ("(scheduled today)", "scheduled"),
        ("(deadline tomorrow)", "deadline"),
        ("(unknown x)", "unknown"),
    ];
    for (input, expected) in cases {
        assert_eq!(unsupported(input), expected, "parsing {input}");
    }
    // A Datalog vector and an advanced-query map are queries, just not simple ones: the text is
    // echoed so the caller can see what was refused.
    let vector = unsupported("[:find ?b :where [?b :block/text]]");
    assert!(vector.starts_with("[:find"), "vector form: {vector}");
    assert!(vector.ends_with(']'), "vector form: {vector}");
    let map = unsupported("{:title \"x\" :query [:find ?b]}");
    assert!(map.starts_with("{:title"), "map form: {map}");
    // Nested inside a form, so the refusal comes from lowering rather than from the top level.
    assert_eq!(unsupported("(and [[a]] (sample 5))"), "sample");
}

#[test]
fn an_argument_a_form_does_not_denote_is_refused_by_form_and_argument() {
    let cases: Vec<(&str, &str, &str)> = vec![
        ("(task NOTAMARKER)", "task", "NOTAMARKER"),
        // Two L's is `CANCELED` misspelled, not a marker Logseq has.
        ("(task CANCELLED)", "task", "CANCELLED"),
        ("(priority 7)", "priority", "7"),
        ("(priority D)", "priority", "D"),
        ("(priority AB)", "priority", "AB"),
        ("(between gemini yesterday)", "between", "gemini"),
        ("(between today +3q)", "between", "+3q"),
        ("(between today 2020-12-05)", "between", "2020-12-05"),
        ("(between today -)", "between", "-"),
        ("(property : book)", "property", ":"),
    ];
    for (input, expected_form, expected_argument) in cases {
        let (form, argument) = invalid(input);
        assert_eq!(form, expected_form, "form for {input}");
        assert_eq!(argument, expected_argument, "argument for {input}");
    }
    // An offset that would overflow a day count is refused rather than wrapped.
    let (form, _) = invalid("(between today +9223372036854775807w)");
    assert_eq!(form, "between");
}

#[test]
fn a_refused_word_names_every_word_the_domain_lists() {
    // The sentence is built from `ALL`, so a marker or priority added there has to appear here
    // without anyone editing a string. This is what makes that true rather than merely intended.
    let task = invalid_detail("(task NOPE)");
    for marker in TaskMarker::ALL {
        assert!(
            task.contains(marker.as_str()),
            "the task message omits {marker}: {task}"
        );
    }
    let priority_detail = invalid_detail("(priority 7)");
    for priority in Priority::ALL {
        assert!(
            priority_detail.contains(priority.as_str()),
            "the priority message omits {priority}: {priority_detail}"
        );
    }
    // The shape is pinned once: an Oxford comma before the last word, and the case note after it.
    assert!(
        task.contains(", or CANCELED, in any casing"),
        "task message: {task}"
    );
    assert!(
        priority_detail.contains(", or C, in any casing"),
        "priority message: {priority_detail}"
    );
}

#[test]
fn an_unknown_string_escape_is_refused_rather_than_guessed() {
    let (position, detail) = syntax(r#""a\qb""#);
    assert!(detail.contains("is not an escape"), "detail: {detail}");
    assert_eq!(position, 2, "the backslash is where the problem is");
    assert!(syntax("\"a\\").1.contains("ends with a backslash"));
}

// --- the page/block split --------------------------------------------------------------------------

#[test]
fn only_page_questions_are_page_level() {
    let page_level = [
        "(page-property type book)",
        "(page-tags #book)",
        "(namespace a/b)",
        "(and (page-tags #book) (page-property type))",
        "(or (namespace a) (page-tags #b))",
        "(not (namespace a))",
        "(and (not (page-tags #a)))",
    ];
    for input in page_level {
        assert!(parse(input).is_page_level(), "{input} should answer pages");
    }
    let block_level = [
        "(page x)",
        "(journal)",
        "(task TODO)",
        "(priority A)",
        "(property type)",
        "[[a]]",
        "#a",
        "(between today yesterday)",
        "word",
        "(and (page-tags #a) [[x]])",
        "(or (page-tags #a) (task TODO))",
        "(not [[x]])",
    ];
    for input in block_level {
        assert!(
            !parse(input).is_page_level(),
            "{input} should answer blocks"
        );
    }
}

// --- the error type --------------------------------------------------------------------------------

#[test]
fn every_refusal_reads_as_a_sentence_a_person_can_act_on() {
    let cases = ["(and)", "(sort-by x)", "(task NOPE)", "(property :)"];
    for input in cases {
        let error = failure(input);
        let message = error.to_string();
        assert!(
            message.len() > 20,
            "message for {input:?} is too short: {message}"
        );
        assert!(
            !message.contains("Ok(") && !message.contains("Some("),
            "message for {input:?} leaks a debug shape: {message}"
        );
    }
}

#[test]
fn the_contract_is_an_error_type_a_caller_can_compose() {
    // `EngineError::Query` carries this type, so it has to be a `std::error::Error` and a `Send +
    // Sync + 'static` value: a caller stores it in its own error.
    fn assert_error<E: std::error::Error + Send + Sync + 'static>(error: &E) -> String {
        error.to_string()
    }
    let error = failure("(and)");
    assert!(!assert_error(&error).is_empty());

    // All four variants exist now, including the one only an executor can produce, so that a
    // failure reported by the executor does not need a type of its own.
    let unresolved = QueryError::UnresolvedDate {
        operand: "[[Dec 5th, 2020]]".to_owned(),
        detail: "no journal page of this graph has that title".to_owned(),
    };
    assert!(unresolved.to_string().contains("Dec 5th, 2020"));
    let invalid_argument = QueryError::InvalidArgument {
        form: "priority".to_owned(),
        argument: "D".to_owned(),
        detail: "expected a priority letter: A, B, or C, in any casing".to_owned(),
    };
    assert!(invalid_argument.to_string().contains("(priority"));
}
