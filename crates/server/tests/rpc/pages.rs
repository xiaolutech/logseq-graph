// Graph, page, tree, backlink, tag and search-shape reads (#1-#9).
//
// Every case names its official method and parity-matrix row; see the crate root.

use super::*;
use serde_json::{json, Value};

// ------------------------------------------------------------------------------------------------
// #1 /info ↔ logseq.Editor.getCurrentGraph — deliberate-difference. The official method reports
// the graph's name and path; ADR-0008 requires this served process to report its own index facts
// and startup-scan progress instead, flattened beside the ReadMeta envelope.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_current_graph_matches_logseq_Editor_getCurrentGraph() {
    let sandbox = Sandbox::new("info");
    let server = Server::start(&sandbox.graph_root(), "info");

    let result = server.call(METHODS[0], json!([])).result();

    assert_eq!(
        result["pages"].as_u64(),
        Some(sandbox.page_file_count() as u64),
        "pages: {result}"
    );
    // Four page files, and 3 + 3 + 1 + 4 = 11 blocks across them.
    assert_eq!(
        result["blocks"].as_u64(),
        Some(sandbox.block_count()),
        "{result}"
    );
    assert!(result["index_path"].as_str().is_some(), "index: {result}");
    let index_size = result["index_size"].as_u64().unwrap();
    let index = Path::new(result["index_path"].as_str().unwrap());
    let pointer = index.with_file_name(format!(
        "{}.current",
        index.file_name().unwrap().to_str().unwrap()
    ));
    let live = fs::read(&pointer).map_or_else(
        |_| index.to_owned(),
        |bytes| {
            let pointer: Value = serde_json::from_slice(&bytes).expect("pointer");
            index.with_file_name(pointer["file"].as_str().expect("generation file"))
        },
    );
    assert!(
        index_size >= fs::metadata(&live).expect("live generation").len(),
        "index_size counts the published generation {}, not only the logical path: {result}",
        live.display()
    );
    assert!(result["index_building_size"].as_u64().is_some(), "{result}");
    assert!(result["index_retired_size"].as_u64().is_some(), "{result}");
    assert_eq!(result["stale"], json!(false), "stale: {result}");
    assert!(
        result["last_scan"]
            .as_str()
            .is_some_and(|t| t.ends_with('Z')),
        "last_scan is an RFC3339 timestamp: {result}"
    );
    assert_eq!(result["scanning"], json!(false), "ready: {result}");
    assert_eq!(
        sandbox.revision_of(&result),
        4,
        "one revision per projected file: {result}"
    );
}

// ------------------------------------------------------------------------------------------------
// #1 /info ↔ logseq.Editor.getCurrentGraph — deliberate-difference. ADR-0008 binds the socket
// before the startup scan and answers progress from that first moment; the official API has no
// startup scan, so it has no `scanning`/`scanned`/`total` state. Every other method reports the
// same three fields in its error `data` while the graph is opening.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_current_graph_reports_the_startup_scan_progress() {
    let sandbox = Sandbox::new("scanning");
    sandbox.write_filler_pages(800);
    let expected = sandbox.page_file_count() as u64;
    let server = Server::spawn(&sandbox.graph_root(), "scanning");

    let mut progress = Vec::new();
    let mut refused = None;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        assert!(
            Instant::now() < deadline,
            "the startup scan did not finish within {TIMEOUT:?}: {progress:?}"
        );
        let result = server.call(METHODS[0], json!([])).result();
        match result["scanning"].as_bool() {
            Some(true) => {
                let scanned = result["scanned"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("no scanned in {result}"));
                let total = result["total"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("no total in {result}"));
                assert!(scanned <= total, "body: {result}");
                progress.push((scanned, total));
                if refused.is_none() && total == expected && scanned < expected {
                    refused = Some(server.call(METHODS[1], json!([])));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Some(false) => break,
            None => panic!("no scanning field in {result}"),
        }
    }

    assert!(
        !progress.is_empty(),
        "the scan finished before one answer could report it"
    );
    assert!(
        progress
            .windows(2)
            .all(|pair| pair[0].0 <= pair[1].0 && pair[0].1 <= pair[1].1),
        "the count and the total only move forward: {progress:?}"
    );
    assert!(
        progress
            .iter()
            .any(|&(scanned, total)| total == expected && scanned > 0 && scanned < expected),
        "an answer read inside the scan must name the discovered total: {progress:?}"
    );

    let error = refused
        .expect("a refused request must have been made while the scan ran")
        .expect_code(503, -32003);
    assert_eq!(error["data"]["kind"], json!("scanning"), "{error}");
    assert_eq!(error["data"]["scanning"], json!(true), "{error}");
    assert!(
        error["data"]["scanned"].as_u64().is_some(),
        "scanned: {error}"
    );
    assert_eq!(
        error["data"]["total"],
        json!(expected),
        "the same total: {error}"
    );
    assert_eq!(
        error["data"]["http_status"],
        json!(503),
        "the migrated 503: {error}"
    );

    let result = server.call(METHODS[0], json!([])).result();
    assert_eq!(result["scanning"], json!(false), "{result}");
    assert_eq!(result["pages"].as_u64(), Some(expected), "pages: {result}");
    server.call(METHODS[1], json!([])).result();
}

// ------------------------------------------------------------------------------------------------
// #2 /pages ↔ logseq.Editor.getAllPages — deliberate-difference. `docs/compatibility.md` measured
// the official `getAllPages` at 34,881 entities including tag, namespace and referenced ghost
// pages; ours lists file pages only (ADR-0001: Markdown files own all product facts, ADR-0002:
// SQLite is a derived read model). No file-less entity is built.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_all_pages_matches_logseq_Editor_getAllPages() {
    let sandbox = Sandbox::new("pages");
    let server = Server::start(&sandbox.graph_root(), "pages");

    let result = server.call(METHODS[1], json!([])).result();

    let pages = result["pages"].as_array().expect("a page array");
    assert_eq!(
        pages.iter().filter(|page| page["has_file"] == true).count(),
        sandbox.page_file_count(),
        "one entry per page file: {result}"
    );
    let alpha = pages
        .iter()
        .find(|page| page["name"] == "alpha")
        .expect("alpha is listed");
    assert_eq!(alpha["block_count"], json!(3), "alpha: {result}");
    assert_eq!(
        alpha["kind"],
        json!("page"),
        "the entry says which kind: {result}"
    );
    assert_eq!(
        alpha["journal_date"],
        Value::Null,
        "a regular page is not a day: {result}"
    );

    let journal = pages
        .iter()
        .find(|page| page["name"] == "Aug 13th, 2026")
        .expect("the journal is listed under its date title");
    assert_eq!(journal["kind"], json!("journal"), "journal: {result}");
    assert_eq!(
        journal["journal_date"],
        json!("2026-08-13"),
        "a journal entry states the day its title renders: {result}"
    );
}

// ------------------------------------------------------------------------------------------------
// #2 /pages ↔ logseq.Editor.getAllPages and #4 /page ↔ logseq.Editor.getPage —
// deliberate-difference. `docs/compatibility.md` measured that the official `getPage(alias)`
// returns an alias-derived no-file entity. Here an alias is a spelling, not a page entity: the
// official no-file entity model is not implemented (ADR-0001 requires a `file` to own the facts,
// and `pages.relative_path` is NOT NULL UNIQUE), so `getPage` on an alias is not found and
// `getAllPages` lists only file pages.
// ------------------------------------------------------------------------------------------------
#[test]
fn an_alias_is_a_no_file_page_and_merges_linked_references() {
    let sandbox = Sandbox::new("alias");
    sandbox.write("pages/aliased.md", "alias:: Old Name\n\n- a file page\n");
    sandbox.write("pages/mentions.md", "- [[Old Name]]\n- [[aliased]]\n");
    let server = Server::start(&sandbox.graph_root(), "alias");

    let alias = server.call(METHODS[3], json!(["Old Name"])).result();
    assert_eq!(alias["page"]["name"], "Old Name");
    assert_eq!(alias["page"]["has_file"], false);
    assert_eq!(alias["properties"], json!([]));

    for name in ["aliased", "Old Name"] {
        let refs = server.call(METHODS[5], json!([name])).result();
        assert_eq!(
            refs["backlinks"],
            json!([{"page": "mentions", "preorder": 0, "text": "[[Old Name]]"}])
        );
    }

    let result = server.call(METHODS[1], json!([])).result();
    let names: Vec<&str> = result["pages"]
        .as_array()
        .expect("a page array")
        .iter()
        .filter_map(|page| page["name"].as_str())
        .collect();
    assert!(
        names.contains(&"aliased"),
        "the file page is listed: {result}"
    );
    assert!(
        names.contains(&"Old Name"),
        "the alias entity is listed: {result}"
    );
}

// ------------------------------------------------------------------------------------------------
// #4 /page ↔ logseq.Editor.getPage — deliberate-difference. The official answer is an entity with
// id/uuid/journalDay/properties; ours is file-page metadata under ADR-0008's `Meta` envelope
// (revision/stale/last_scan). At field level the `journal_date` value is the official `journalDay`
// day; the answer shape around it is not, so the row stays a deliberate-difference.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_page_matches_logseq_Editor_getPage() {
    let sandbox = Sandbox::new("page");
    let server = Server::start(&sandbox.graph_root(), "page");

    let journal = server.call(METHODS[3], json!(["Aug 13th, 2026"])).result();
    assert_eq!(
        journal["page"]["journal_date"],
        json!("2026-08-13"),
        "the day the listing states: {journal}"
    );
    assert!(journal["revision"].is_u64(), "the Meta envelope: {journal}");

    let page = server.call(METHODS[3], json!(["alpha"])).result();
    assert_eq!(page["page"]["name"], json!("alpha"), "{page}");
    assert_eq!(page["page"]["block_count"], json!(3), "{page}");
    assert_eq!(page["page"]["kind"], json!("page"), "{page}");
    assert_eq!(
        page["page"]["journal_date"],
        Value::Null,
        "a regular page states no day, and the field is present: {page}"
    );
}

// ------------------------------------------------------------------------------------------------
// #4 /page ↔ logseq.Editor.getPage — deliberate-difference (the matrix row is not a match on the
// whole answer). One sub-item agrees: the official method takes the page name as a JSON string in
// `args`, and ours does too, so a name with spaces, Chinese characters and a namespace separator
// needs no URL percent-decoding and answers its stored spelling byte for byte.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_page_accepts_a_unicode_name_as_a_json_string_matches_logseq_Editor_getPage() {
    let sandbox = Sandbox::new("unicode");
    let server = Server::start(&sandbox.graph_root(), "unicode");

    let name = "阅读 notes/page one";
    let page = server.call(METHODS[3], json!([name])).result();
    assert_eq!(
        page["page"]["name"],
        json!(name),
        "the stored spelling: {page}"
    );
    assert_eq!(page["page"]["block_count"], json!(4), "{page}");

    let tree = server.call(METHODS[4], json!([name])).result();
    let blocks = tree["blocks"].as_array().expect("a block array");
    assert_eq!(blocks.len(), 4, "tree: {tree}");
    assert_eq!(blocks[0]["text"], json!("spaced root"), "{tree}");
    assert_eq!(blocks[0]["depth"], json!(0), "{tree}");
    assert_eq!(blocks[3]["depth"], json!(3), "{tree}");
}

// ------------------------------------------------------------------------------------------------
// #5 /tree ↔ logseq.Editor.getPageBlocksTree — deliberate-difference. The official answer is a
// nested tree carrying preBlock?/level/parent/page/refs; ours is a flat preorder list with `depth`,
// a Markdown `marker`, and a `selector` per entry (ADR-0012: every block a boundary reports is
// actionable). The selector spelling is an own API choice, not an official contract.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_page_blocks_tree_matches_logseq_Editor_getPageBlocksTree() {
    let sandbox = Sandbox::new("tree");
    let server = Server::start(&sandbox.graph_root(), "tree");

    let tree = server
        .call(METHODS[4], json!(["阅读 notes/page one"]))
        .result();
    let blocks = tree["blocks"].as_array().expect("a block array");
    assert_eq!(blocks.len(), 4, "tree: {tree}");
    for block in blocks {
        assert!(
            block["selector"]["kind"] == "durable" || block["selector"]["kind"] == "ephemeral",
            "a selector kind: {block}"
        );
        assert_eq!(
            block["selector"]["id"].as_str().map(str::len),
            Some(36),
            "a UUID: {tree}"
        );
    }
    assert_eq!(
        blocks[0]["selector"]["kind"],
        json!("ephemeral"),
        "no block of this page states an id::, so every selector is the projection's: {tree}"
    );

    let id = blocks[1]["selector"]["id"].as_str().expect("an id");
    let read = server
        .call(METHODS[12], json!([format!("ephemeral:{id}")]))
        .result();
    assert_eq!(read["block"]["preorder"], blocks[1]["preorder"], "{read}");
    assert_eq!(read["block"]["text"], blocks[1]["text"], "{read}");
}

// ------------------------------------------------------------------------------------------------
// #5 /tree ↔ logseq.Editor.getPageBlocksTree — deliberate-difference. Each flat entry reports the
// marker that opens its source block, including the complete heading level and an ordinary bullet.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_page_blocks_tree_reports_heading_and_bullet_markers() {
    let sandbox = Sandbox::new("tree-block-markers");
    sandbox.write(
        "pages/headings.md",
        "### section heading\n- ordinary bullet\n",
    );
    let server = Server::start(&sandbox.graph_root(), "tree-block-markers");

    let tree = server.call(METHODS[4], json!(["headings"])).result();
    let blocks = tree["blocks"].as_array().expect("a block array");
    assert_eq!(blocks.len(), 2, "{tree}");
    assert_eq!(blocks[0]["text"], json!("section heading"), "{tree}");
    assert_eq!(blocks[0]["marker"], json!("###"), "{tree}");
    assert_eq!(blocks[1]["text"], json!("ordinary bullet"), "{tree}");
    assert_eq!(blocks[1]["marker"], json!("-"), "{tree}");
}

// ------------------------------------------------------------------------------------------------
// #6 /backlinks ↔ logseq.Editor.getPageLinkedReferences — deliberate-difference.
// `docs/compatibility.md` measured that the official server merges alias references into the
// declaring page's references (10 versus 1 on the measured graph); ours answers direct references
// to file pages only, because no file-less alias entity exists (ADR-0001).
// ------------------------------------------------------------------------------------------------
#[test]
fn get_page_linked_references_matches_logseq_Editor_getPageLinkedReferences() {
    let sandbox = Sandbox::new("backlinks");
    let server = Server::start(&sandbox.graph_root(), "backlinks");

    let result = server.call(METHODS[5], json!(["alpha"])).result();
    let backlinks = result["backlinks"].as_array().expect("an array");
    assert_eq!(backlinks.len(), 2, "Bravo and the journal: {result}");
    // The engine answers in folded page-name order, so the journal's date title comes first.
    assert_eq!(backlinks[0]["page"], json!("Aug 13th, 2026"), "{result}");
    assert_eq!(backlinks[1]["page"], json!("Bravo"), "{result}");
    assert!(backlinks[0]["text"].as_str().is_some(), "{result}");

    let empty = server
        .call(METHODS[5], json!(["nobody-references-this"]))
        .result();
    assert!(
        empty["backlinks"].as_array().expect("an array").is_empty(),
        "{empty}"
    );
}

// ------------------------------------------------------------------------------------------------
// #7 /tags ↔ official `getAllPages` tag pages — deliberate-difference. The official DB edition
// exposes tags as ghost page entities inside `getAllPages` (measured in `docs/compatibility.md`);
// ours has a dedicated `logseq.Graph.getAllTags` read of the derived tag index (ADR-0002), with a
// `page_count` instead of an entity. The method name is our own, not official contract.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_all_tags_matches_getAllPages_tag_pages() {
    let sandbox = Sandbox::new("tags");
    let server = Server::start(&sandbox.graph_root(), "tags");

    let result = server.call(METHODS[6], json!([])).result();
    assert_eq!(result["tags"][0]["name"], json!("project"), "{result}");
    assert_eq!(result["tags"][0]["page_count"], json!(1), "{result}");
}

// ------------------------------------------------------------------------------------------------
// #8 /tag ↔ logseq.DB.q(page-tags) — deliberate-difference. The official API answers tag
// membership through `DB.q` over page entities; ours has a dedicated own method
// `logseq.Graph.getPagesWithTag` answering file page names directly (ADR-0002). `DB.q` itself is
// deliberately not served by this surface.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_pages_with_tag_matches_logseq_DB_q_page_tags() {
    let sandbox = Sandbox::new("tag");
    let server = Server::start(&sandbox.graph_root(), "tag");

    let result = server.call(METHODS[7], json!(["project"])).result();
    assert_eq!(result["pages"][0], json!("Bravo"), "{result}");
}

// ------------------------------------------------------------------------------------------------
// #9 /search ↔ logseq.App.search — deliberate-difference. `docs/compatibility.md` measured the
// official `App.search` answer as four sections with fixed caps and `has-more?`; ours answers one
// bounded list of block hits with an explicit `limit`, a `freshness` policy (ADR-0008) and a
// `selector` per hit (ADR-0012). The `text` field keeps its published name.
// ------------------------------------------------------------------------------------------------
#[test]
fn app_search_matches_logseq_App_search() {
    let sandbox = Sandbox::new("search");
    let server = Server::start(&sandbox.graph_root(), "search");

    let result = server
        .call(
            METHODS[8],
            with_options("alpha", json!({"freshness": "last"})),
        )
        .result();
    let hits = result["hits"].as_array().expect("a hit array");
    assert_eq!(hits.len(), 5, "every block naming alpha: {result}");
    assert_eq!(result["query"], json!("alpha"), "{result}");
    assert_eq!(result["limit"], json!(20), "the default: {result}");
    assert!(
        hits.iter().all(|hit| hit["text"].as_str().is_some()),
        "{result}"
    );

    let bounded = server
        .call(METHODS[8], with_options("alpha", json!({"limit": 2})))
        .result();
    assert_eq!(
        bounded["hits"].as_array().expect("a hit array").len(),
        2,
        "the limit bounds the hits: {bounded}"
    );

    let one = server
        .call(METHODS[8], with_options("grandchild", json!({"limit": 1})))
        .result();
    assert_eq!(one["hits"][0]["page"], json!("alpha"), "{one}");
    assert_eq!(one["hits"][0]["preorder"], json!(2), "{one}");
}

// ------------------------------------------------------------------------------------------------
// #9 /search ↔ logseq.App.search — deliberate-difference. ADR-0012 gives every search hit a
// `selector` the `getBlock` read accepts; the official `App.search` block entries carry no block
// identity a caller can write through. The selector spelling is not an official contract.
// ------------------------------------------------------------------------------------------------
#[test]
fn app_search_hits_carry_an_actionable_selector() {
    let sandbox = Sandbox::new("search-selector");
    let server = Server::start(&sandbox.graph_root(), "search-selector");

    let result = server.call(METHODS[8], json!(["alpha"])).result();
    let hits = result["hits"].as_array().expect("a hit array");
    for hit in hits {
        let kind = hit["selector"]["kind"].as_str().expect("a selector kind");
        assert!(kind == "durable" || kind == "ephemeral", "{kind}");
        assert_eq!(
            hit["selector"]["id"].as_str().map(str::len),
            Some(36),
            "a UUID: {result}"
        );
    }

    let grandchild = hits
        .iter()
        .find(|hit| hit["preorder"] == json!(2) && hit["page"] == "alpha")
        .expect("alpha#2 is a hit");
    assert_eq!(
        grandchild["text"],
        json!("alpha grandchild"),
        "the value behind the published field name: {result}"
    );
    let kind = grandchild["selector"]["kind"].as_str().expect("kind");
    let id = grandchild["selector"]["id"].as_str().expect("id");
    let spelling = if kind == "durable" {
        id.to_owned()
    } else {
        format!("ephemeral:{id}")
    };
    let read = server.call(METHODS[12], json!([spelling])).result();
    assert_eq!(read["block"]["page"], json!("alpha"), "{read}");
    assert_eq!(read["block"]["preorder"], json!(2), "{read}");
    assert_eq!(read["block"]["text"], json!("alpha grandchild"), "{read}");
    assert_eq!(
        read["block"]["selector"], grandchild["selector"],
        "one identity however the block is reached: {read}"
    );
}

// ------------------------------------------------------------------------------------------------
// #9 /search ↔ logseq.App.search — deliberate-difference. The official method caps its sections
// at 10/10/100 (measured); ours refuses a `limit` above 1000 with a structured invalid-params
// error rather than quietly cutting the answer. A limit of zero is a legal "answer nothing".
// ------------------------------------------------------------------------------------------------
#[test]
fn app_search_refuses_a_limit_above_the_ceiling() {
    let sandbox = Sandbox::new("search-limit");
    let server = Server::start(&sandbox.graph_root(), "search-limit");

    let error = server
        .call(METHODS[8], with_options("alpha", json!({"limit": 1001})))
        .expect_code(400, -32602);
    assert_eq!(error["data"]["kind"], json!("invalid-limit"), "{error}");
    assert_eq!(error["data"]["parameter"], json!("limit"), "{error}");
    assert_eq!(error["data"]["max"], json!(1000), "{error}");

    let zero = server
        .call(METHODS[8], with_options("alpha", json!({"limit": 0})))
        .result();
    assert!(
        zero["hits"].as_array().expect("an array").is_empty(),
        "{zero}"
    );
}
