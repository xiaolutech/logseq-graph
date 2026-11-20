// Simple queries and block reads (#12, #13).
//
// Every case names its official method and parity-matrix row; see the crate root.

use super::*;
use serde_json::json;

// ------------------------------------------------------------------------------------------------
// #12 /query ↔ logseq.DB.q — deliberate-difference. ADR-0013: our simple-query grammar is
// answered by the index with `truncated`, `today`, a `selector` per block and a freshness policy.
// `docs/compatibility.md` measured that official `DB.q` returns `null` for general Datalog
// (matrix row #25, out-of-scope here); ours refuses that input with a structured error instead.
// ------------------------------------------------------------------------------------------------
#[test]
fn query_matches_logseq_DB_q() {
    let sandbox = Sandbox::new("query");
    sandbox.write("pages/todo.md", "- TODO one\n- TODO two\n- DONE three\n");
    let server = Server::start(&sandbox.graph_root(), "query");

    let result = server
        .call(
            METHODS[11],
            with_options("(task TODO)", json!({"today": "2026-08-10"})),
        )
        .result();
    assert_eq!(result["query"], json!("(task TODO)"), "{result}");
    assert_eq!(result["today"], json!("2026-08-10"), "{result}");
    assert_eq!(result["truncated"], json!(false), "{result}");
    assert_eq!(
        result["pages"].as_array().map(Vec::len),
        Some(0),
        "{result}"
    );
    let blocks = result["blocks"].as_array().expect("blocks");
    assert!(
        !blocks.is_empty(),
        "the fixture states TODO blocks: {result}"
    );
    for block in blocks {
        assert!(block["page"].as_str().is_some(), "{result}");
        assert!(block["preorder"].as_u64().is_some(), "{result}");
        assert!(block["text"].as_str().is_some(), "{result}");
        let kind = block["selector"]["kind"].as_str().expect("a selector kind");
        assert!(kind == "durable" || kind == "ephemeral", "{kind}");
        assert!(
            block["selector"]["id"].as_str().is_some(),
            "a selector id: {result}"
        );
    }
}

// ------------------------------------------------------------------------------------------------
// #12 /query ↔ logseq.DB.q — deliberate-difference. The official method has no `truncated` field;
// ours states it so a caller never reads a cut answer as the whole answer (ADR-0013).
// ------------------------------------------------------------------------------------------------
#[test]
fn query_states_that_its_answer_was_truncated() {
    let sandbox = Sandbox::new("query-truncated");
    sandbox.write("pages/todo.md", "- TODO one\n- TODO two\n- TODO three\n");
    let server = Server::start(&sandbox.graph_root(), "query-truncated");

    let result = server
        .call(
            METHODS[11],
            with_options("(task TODO)", json!({"limit": 1})),
        )
        .result();
    assert_eq!(result["limit"], json!(1), "{result}");
    assert_eq!(
        result["blocks"].as_array().map(Vec::len),
        Some(1),
        "{result}"
    );
    assert_eq!(
        result["truncated"],
        json!(true),
        "a cut answer says so: {result}"
    );
}

// ------------------------------------------------------------------------------------------------
// #12 /query ↔ logseq.DB.q — deliberate-difference (ADR-0012). A query answer's `selector` is the
// spelling `getBlock` accepts, which makes the answer actionable instead of read-only.
// ------------------------------------------------------------------------------------------------
#[test]
fn query_selector_reads_the_block_it_names() {
    let sandbox = Sandbox::new("query-selector-block");
    sandbox.write("pages/todo.md", "- TODO one\n- TODO two\n");
    let server = Server::start(&sandbox.graph_root(), "query-selector-block");

    let result = server
        .call(
            METHODS[11],
            with_options("(task TODO)", json!({"limit": 1})),
        )
        .result();
    let block = &result["blocks"][0];
    let kind = block["selector"]["kind"].as_str().expect("kind");
    let id = block["selector"]["id"].as_str().expect("id");
    let spelling = if kind == "durable" {
        id.to_owned()
    } else {
        format!("ephemeral:{id}")
    };
    let read = server.call(METHODS[12], json!([spelling])).result();
    assert_eq!(read["block"]["text"], block["text"], "{read}");
    assert_eq!(read["block"]["page"], block["page"], "{read}");
}

// ------------------------------------------------------------------------------------------------
// #12 /query ↔ logseq.DB.q — deliberate-difference. `docs/compatibility.md` measured that
// official `DB.q` answers `null` for Datalog or unparseable input; ours reports a structured
// invalid-params error naming the query, so an unanswerable query is never an empty answer.
// ------------------------------------------------------------------------------------------------
#[test]
fn a_query_the_grammar_refuses_is_a_structured_error_not_an_empty_result() {
    let sandbox = Sandbox::new("query-refused");
    let server = Server::start(&sandbox.graph_root(), "query-refused");

    for refused in ["(sort-by priority)", "[:find ?b]"] {
        let error = server
            .call(METHODS[11], json!([refused]))
            .expect_code(400, -32602);
        assert_eq!(error["data"]["kind"], json!("invalid-query"), "{error}");
        assert_eq!(error["data"]["parameter"], json!("query"), "{error}");
        assert_eq!(error["data"]["value"], json!(refused), "{error}");
        assert!(
            error["message"].as_str().unwrap().contains("query"),
            "the message names the input: {error}"
        );
    }

    // A missing positional argument, a bad day, and a limit above the ceiling are all structured
    // refusals, not empty answers either.
    server.call(METHODS[11], json!([])).expect_code(400, -32602);
    let bad_day = server
        .call(
            METHODS[11],
            with_options("(task TODO)", json!({"today": "2026-02-30"})),
        )
        .expect_code(400, -32602);
    assert_eq!(bad_day["data"]["parameter"], json!("today"), "{bad_day}");
    let big_limit = server
        .call(
            METHODS[11],
            with_options("(task TODO)", json!({"limit": 1001})),
        )
        .expect_code(400, -32602);
    assert_eq!(big_limit["data"]["max"], json!(1000), "{big_limit}");
}

// ------------------------------------------------------------------------------------------------
// #12 /query ↔ logseq.DB.q — deliberate-difference (ADR-0013). A page-level expression answers
// pages, and the empty block list states which shape the answer is instead of making a caller
// branch on a key that appears.
// ------------------------------------------------------------------------------------------------
#[test]
fn a_page_level_query_answers_pages() {
    let sandbox = Sandbox::new("query-pages");
    sandbox.write("pages/area.md", "kind:: [[work]]\n\n- the namespace page\n");
    sandbox.write(
        "pages/area___one.md",
        "kind:: [[work]]\n\n- a page under it\n",
    );
    sandbox.write("pages/other.md", "kind:: [[home]]\n\n- not it\n");
    let server = Server::start(&sandbox.graph_root(), "query-pages");

    let result = server
        .call(
            METHODS[11],
            with_options("(page-property kind work)", json!({"today": "2026-08-10"})),
        )
        .result();
    let pages = result["pages"].as_array().expect("pages");
    let names: Vec<&str> = pages
        .iter()
        .filter_map(|page| page["name"].as_str())
        .collect();
    assert_eq!(names, ["area", "area/one"], "{result}");
    assert_eq!(
        result["blocks"].as_array().map(Vec::len),
        Some(0),
        "{result}"
    );
    for page in pages {
        assert_eq!(page["kind"], json!("page"), "{result}");
        assert!(page["journal_date"].is_null(), "not a journal: {result}");
    }
}

// ------------------------------------------------------------------------------------------------
// #13 /block ↔ logseq.Editor.getBlock — deliberate-difference. The official method takes an
// `id::` uuid; ours accepts that bare uuid plus the `durable:`/`ephemeral:` selector spellings
// (ADR-0009 and ADR-0012), which are an own API choice rather than an official contract.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_block_matches_logseq_Editor_getBlock() {
    let sandbox = Sandbox::new("block");
    let server = Server::start(&sandbox.graph_root(), "block");

    for spelling in [
        "01234567-89ab-cdef-0123-456789abcdef",
        "durable:01234567-89ab-cdef-0123-456789abcdef",
    ] {
        let result = server.call(METHODS[12], json!([spelling])).result();
        assert_eq!(result["block"]["page"], json!("Bravo"), "{result}");
        assert_eq!(result["block"]["preorder"], json!(2), "{result}");
        assert!(
            result["block"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("stable block")),
            "{result}"
        );
    }

    // The official lookup answers the snake leaf too — `get_block` is the spelling the Logseq
    // Copilot extension calls to resolve a block ref.
    let canonical = server
        .call(METHODS[12], json!(["01234567-89ab-cdef-0123-456789abcdef"]))
        .result();
    for spelling in ["get_block", "logseq.Editor.get_block"] {
        let bare = server
            .call(
                spelling,
                json!(["01234567-89ab-cdef-0123-456789abcdef", null]),
            )
            .result();
        assert_eq!(
            bare["block"], canonical["block"],
            "{spelling} answers the same block: {bare}"
        );
    }

    let malformed = server
        .call(METHODS[12], json!(["not-an-id"]))
        .expect_code(400, -32602);
    assert_eq!(
        malformed["data"]["parameter"],
        json!("selector"),
        "{malformed}"
    );
    assert!(
        malformed["message"].as_str().unwrap().contains("selector"),
        "the message states the spelling: {malformed}"
    );
}

// ------------------------------------------------------------------------------------------------
// #13 /block ↔ logseq.Editor.getBlock — deliberate-difference. The selector returned by the tree
// addresses the same heading through this read, whose marker must describe that block rather than
// assuming every block is a bullet.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_block_reports_the_heading_marker_of_the_selected_block() {
    let sandbox = Sandbox::new("block-heading-marker");
    sandbox.write(
        "pages/headings.md",
        "### section heading\n- ordinary bullet\n",
    );
    let server = Server::start(&sandbox.graph_root(), "block-heading-marker");

    let tree = server.call(METHODS[4], json!(["headings"])).result();
    let heading = &tree["blocks"][0];
    assert_eq!(heading["text"], json!("section heading"), "{tree}");
    assert_eq!(heading["selector"]["kind"], json!("ephemeral"), "{tree}");
    let id = heading["selector"]["id"]
        .as_str()
        .expect("the tree heading has an ephemeral id");

    let read = server
        .call(METHODS[12], json!([format!("ephemeral:{id}")]))
        .result();
    assert_eq!(read["block"]["text"], heading["text"], "{read}");
    assert_eq!(read["block"]["marker"], json!("###"), "{read}");
}

// ------------------------------------------------------------------------------------------------
// #13 /block ↔ logseq.Editor.getBlock — deliberate-difference (ADR-0005 and the engine's measured
// contract). A durable id stated by more than one block is a conflict naming every page involved;
// the official API silently picks one row. The error `data.pages` carries all of them.
// ------------------------------------------------------------------------------------------------
#[test]
fn an_ambiguous_block_id_names_every_page() {
    let sandbox = Sandbox::new("ambiguous");
    sandbox.write(
        "pages/dup-one.md",
        "- one\n  id:: 11111111-2222-3333-4444-555555555555\n",
    );
    sandbox.write(
        "pages/dup-two.md",
        "- two\n  id:: 11111111-2222-3333-4444-555555555555\n",
    );
    let server = Server::start(&sandbox.graph_root(), "ambiguous");

    let error = server
        .call(METHODS[12], json!(["11111111-2222-3333-4444-555555555555"]))
        .expect_code(409, -32009);
    assert_eq!(
        error["data"]["kind"],
        json!("ambiguous-block-id"),
        "{error}"
    );
    assert_eq!(
        error["data"]["id"],
        json!("11111111-2222-3333-4444-555555555555"),
        "{error}"
    );
    assert_eq!(
        error["data"]["pages"],
        json!(["dup-one", "dup-two"]),
        "every page involved: {error}"
    );
}

// ------------------------------------------------------------------------------------------------
// #12 /query ↔ logseq.DB.q — deliberate-difference. `logseq.DB.q` is the official spelling and
// answers the official array shape (blocks or pages, no object wrapper and no read metadata); the
// own `logseq.Graph.query` keeps the object shape with `truncated`, `today` and a selector per
// block. Both are the same read under ADR-0013.
// ------------------------------------------------------------------------------------------------
#[test]
fn db_q_is_the_official_spelling_and_answers_the_official_array_shape() {
    let sandbox = Sandbox::new("db-q");
    sandbox.write("pages/needle.md", "- the needle is here\n- unrelated\n");
    sandbox.write("pages/tagged.md", "status:: active\n\n- a tagged page\n");
    sandbox.write(
        "journals/2026_08_14.md",
        "status:: active\n\n- a tagged journal\n",
    );
    let server = Server::start(&sandbox.graph_root(), "db-q");

    // A content query is block-level: the official answer is an array of block entries.
    let blocks = server
        .call("logseq.DB.q", json!(["\"the needle is here\""]))
        .result();
    let blocks = blocks.as_array().expect("an array, not our object wrapper");
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    assert_eq!(
        blocks[0]["content"],
        json!("the needle is here"),
        "{blocks:?}"
    );
    assert_eq!(blocks[0]["page"]["name"], json!("needle"), "{blocks:?}");
    assert!(blocks[0]["uuid"].as_str().is_some(), "{blocks:?}");

    // A page-level query is an array of page entries.
    let pages = server
        .call("logseq.DB.q", json!(["(page-property status active)"]))
        .result();
    let pages = pages.as_array().expect("an array of pages");
    assert!(
        pages.iter().any(|page| page["name"] == "tagged"),
        "{pages:?}"
    );
    let journal = pages
        .iter()
        .find(|page| page["journal?"] == json!(true))
        .expect("the tagged journal page is listed");
    assert_eq!(
        journal["journalDay"],
        json!(20260814),
        "the official `journalDay` is the number YYYYMMDD: {journal}"
    );

    // No match is an empty array, not an error, and it carries no metadata object.
    assert_eq!(
        server
            .call("logseq.DB.q", json!(["\"nothing matches this\""]))
            .result(),
        json!([])
    );

    // The own spelling still answers the object shape for the same query.
    let own = server
        .call("logseq.Graph.query", json!(["\"the needle is here\""]))
        .result();
    assert_eq!(own["blocks"].as_array().map(Vec::len), Some(1), "{own}");
}
