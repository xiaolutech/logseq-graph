// Wire-shape differential against the answers captured from the official Logseq HTTP API.
//
// `fixtures/expected/*.json` holds answers Logseq 0.10.9 gave on 2026-11-20 for the public
// `logseq/docs` graph at tag `v0.9.2`.
// `tests/official_parity.rs` compares the *values* the engine answers, but it runs only when
// `LOGSEQ_GRAPH_OFFICIAL` names that graph. Nothing compared the JSON *shape* the served RPC
// states, and a client reads shapes: it maps `result.blocks`, branches on `has-more?`, and hands
// `block/uuid` back to `Editor.getBlock`. A shape that differs from the official one therefore
// breaks a client without breaking any test. This file closes that hole.
//
// Every capture is replayed against one sandbox server, and every key the official answer states
// must also be stated there with a compatible JSON type, unless the key is listed below as a
// documented difference with the parity row that classifies it. Values are never compared — the
// captures describe another graph — so this test runs in every build, with no environment variable.
//
// A captured method this surface does not implement is asserted to answer `MethodNotExist` and must
// be listed in [`UNIMPLEMENTED`] with its parity row, so a captured method cannot go missing
// silently: the day one is implemented, its test fails and the list and the matrix must move too.
//
// The same file pins the *extension* surface: [`EXTENSION_READS`] is the read methods the Logseq
// Copilot browser extension (EINDEX/logseq-copilot, its markdown client) calls, and each must answer
// something other than `MethodNotExist`. The extension is the client that found the shape bug this
// file exists for.

use std::path::{Path, PathBuf};

use super::{Sandbox, Server};
use serde_json::{json, Value};

/// Workspace-level directory holding the captured official answers.
///
/// Anchored at `CARGO_MANIFEST_DIR` and resolved by walking ancestors, so no relative depth such as
/// `../..` is hardcoded and the test still finds the fixtures if the crate moves in the workspace.
fn fixtures_dir() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .map(|ancestor| ancestor.join("fixtures").join("expected"))
        .find(|candidate| candidate.is_dir())
        .unwrap_or_else(|| {
            panic!(
                "no fixtures/expected directory above {}",
                manifest.display()
            )
        })
}

/// Read and decode one captured answer. Fixtures are opened read-only and never rewritten.
fn fixture(name: &str) -> Value {
    let path = fixtures_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("decode {}: {error}", path.display()))
}

/// The JSON type a client branches on. A shape comparison is exactly this: the same keys, the same
/// types.
fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// The documented differences between one captured answer and this surface's answer.
///
/// `absent` keys are stated by the official answer and deliberately not by this surface; `retyped`
/// keys are stated by both with different JSON types; `empty` arrays hold entries in the official
/// answer and are stated empty here. Every path is `method.result...`, an array entry adds `[]`.
/// Each use site names the matrix row that classifies the difference.
#[derive(Clone, Copy, Default)]
struct Differences {
    absent: &'static [&'static str],
    retyped: &'static [&'static str],
    empty: &'static [&'static str],
}

/// Compare one official answer with ours, collecting every shape difference not listed in
/// `differences`. Missing keys and incompatible types are findings; extra keys are this surface's
/// own additions and are not findings.
fn compare(
    label: &str,
    official: &Value,
    ours: &Value,
    differences: Differences,
    problems: &mut Vec<String>,
) {
    match official {
        Value::Object(official_map) => {
            let Some(ours_map) = ours.as_object() else {
                if differences.retyped.contains(&label) {
                    return;
                }
                problems.push(format!(
                    "{label}: the official answer states an object, this surface states {}",
                    json_type(ours)
                ));
                return;
            };
            for (key, official_value) in official_map {
                let key_path = format!("{label}.{key}");
                if differences.absent.contains(&key_path.as_str()) {
                    continue;
                }
                let Some(our_value) = ours_map.get(key) else {
                    problems.push(format!(
                        "{key_path}: the official answer states it, this surface does not"
                    ));
                    continue;
                };
                if json_type(official_value) != json_type(our_value) {
                    if differences.retyped.contains(&key_path.as_str()) {
                        continue;
                    }
                    problems.push(format!(
                        "{key_path}: official type {}, this surface type {}",
                        json_type(official_value),
                        json_type(our_value)
                    ));
                    continue;
                }
                compare(&key_path, official_value, our_value, differences, problems);
            }
        }
        Value::Array(official_list) => {
            if official_list.is_empty() {
                return;
            }
            let Some(ours_list) = ours.as_array() else {
                if differences.retyped.contains(&label) {
                    return;
                }
                problems.push(format!(
                    "{label}: the official answer states an array, this surface states {}",
                    json_type(ours)
                ));
                return;
            };
            if ours_list.is_empty() {
                if differences.empty.contains(&label) {
                    return;
                }
                problems.push(format!(
                    "{label}: the official answer states {} entries, this surface states none",
                    official_list.len()
                ));
                return;
            }
            // A capture's entries share one shape; its first entry states that shape, and the
            // values are graph-specific and never compared.
            compare(
                &format!("{label}[]"),
                &official_list[0],
                &ours_list[0],
                differences,
                problems,
            );
        }
        // Scalars were compared by type before this call.
        _ => {}
    }
}

/// Assert one captured answer's shape against this surface's answer, failing with every finding.
fn assert_shape(label: &str, official: &Value, ours: &Value, differences: Differences) {
    let mut problems = Vec::new();
    compare(label, official, ours, differences, &mut problems);
    assert!(
        problems.is_empty(),
        "{label}: the served shape differs from the captured official shape:\n  {}",
        problems.join("\n  ")
    );
}

/// One capture file and the parity row that claims its shape.
fn row(file: &str) -> Value {
    fixture(file)
}

/// The captured methods this surface deliberately does not serve, with the matrix row that
/// classifies each. A name here must not resolve; see [`unimplemented_captures_answer_method_not_exist`].
const UNIMPLEMENTED: &[(&str, &str)] = &[
    // Row #14: the engine read exists; the RPC slice does not serve it.
    ("get_pages_from_namespace.json", "row #14"),
    // Row #15: same, for the namespace subtree read.
    ("get_pages_tree_from_namespace.json", "row #15"),
    // Row #16: the engine read answers written property lines, not an entity property table.
    ("get_block_properties.json", "row #16"),
    // Row #17: the engine read takes no property key, and repeated keys are facts.
    ("get_block_property.json", "row #17"),
    // Row #18: the engine read takes a selector; the RPC slice does not serve it.
    ("get_previous_sibling_block.json", "row #18"),
    // Row #19: same, for the next sibling.
    ("get_next_sibling_block.json", "row #19"),
];

/// The read methods the Logseq Copilot extension's markdown client calls, in its own spelling, with
/// one plausible argument each for the replay.
///
/// Source: EINDEX/logseq-copilot, `src/entrypoints/background/logseq/markdown/client.ts`. Each must
/// be answered — with the official shape where the matrix says so — because a client discovers a
/// missing or differently-shaped method only when a person clicks.
fn extension_reads() -> Vec<(&'static str, Value)> {
    vec![
        (
            "logseq.Editor.getBlock",
            json!(["01234567-89ab-cdef-0123-456789abcdef"]),
        ),
        ("logseq.Editor.getPage", json!(["alpha"])),
        ("logseq.DB.q", json!(["\"alpha root\""])),
        ("logseq.App.search", json!(["alpha"])),
        ("logseq.App.getCurrentGraph", json!([])),
        ("logseq.App.getAppInfo", json!([])),
        ("logseq.Editor.getCurrentPage", json!([])),
        ("logseq.Editor.getAllPages", json!([])),
        ("logseq.App.getUserConfigs", json!([])),
        ("logseq.showMsg", json!(["connected"])),
        ("logseq.App.checkCurrentIsDBGraph", json!([])),
    ]
}

/// The extension's write calls that this surface does not answer under the official namespace.
///
/// ADR-0014 makes every served write `logseq.Graph.*` with this repository's own argument shapes;
/// an official `logseq.Editor.*` write name is not an alias for one, so it stays `MethodNotExist`
/// until a row of the matrix says otherwise.
const EXTENSION_WRITES_NOT_SERVED: &[&str] = &[
    "logseq.Editor.appendBlockInPage",
    "logseq.Editor.updateBlock",
    "logseq.Editor.upsertBlockProperty",
];

/// One sandbox graph that holds a hit for every replay: a page, a durable-`id::` block, a backlink,
/// a journal entry, a namespace page, and a page-level property for the page-shape query.
fn sandbox() -> Sandbox {
    let sandbox = Sandbox::new("official-shapes");
    sandbox.write(
        "logseq/config.edn",
        "{:journal/page-title-format \"yyyy-MM-dd EEEE\"}\n",
    );
    sandbox.write("pages/needle.md", "- the needle is here\n- unrelated\n");
    sandbox.write("pages/tagged.md", "status:: active\n\n- a tagged page\n");
    sandbox
}

/// #1 `logseq.Editor.getCurrentGraph`: the official answer states `name`, `path` and `url`.
#[test]
fn get_current_graph_states_the_official_name_path_and_url() {
    let sandbox = sandbox();
    let server = Server::start(&sandbox.graph_root(), "shape-graph");
    let captured = row("get_current_graph.json");
    let ours = server
        .call("logseq.Editor.getCurrentGraph", json!([]))
        .result();

    assert_eq!(
        ours["url"],
        json!(format!(
            "logseq_local_{}",
            sandbox
                .graph_root()
                .canonicalize()
                .expect("the sandbox path canonicalises")
                .display()
        )),
        "the `url` is the identity the official answer states: {ours}"
    );
    assert_shape(
        "getCurrentGraph.result",
        &captured["result"],
        &ours,
        Differences::default(),
    );
}

/// #2 `logseq.Editor.getAllPages`: a file-only page list, so the container and the entity fields
/// are the documented difference; the captured `sample` entry states the official entity shape.
#[test]
fn get_all_pages_states_a_file_page_entry_beside_the_official_entity_fields() {
    let sandbox = sandbox();
    let server = Server::start(&sandbox.graph_root(), "shape-pages");
    let captured = row("get_all_pages.json");
    let sample = &captured["result"]["sample"][0];
    let ours = server.call("logseq.Editor.getAllPages", json!([])).result();
    let entry = &ours["pages"][0];

    // Row #2: the official answer is an array of DB entities; this surface answers one object whose
    // `pages` list holds file pages, so the container itself is the documented difference.
    assert_shape(
        "getAllPages.result.pages[]",
        sample,
        entry,
        Differences {
            absent: &[
                "getAllPages.result.pages[].alias",
                "getAllPages.result.pages[].createdAt",
                "getAllPages.result.pages[].file",
                "getAllPages.result.pages[].format",
                "getAllPages.result.pages[].id",
                "getAllPages.result.pages[].journal?",
                "getAllPages.result.pages[].namespace",
                "getAllPages.result.pages[].originalName",
                "getAllPages.result.pages[].properties",
                "getAllPages.result.pages[].propertiesTextValues",
                "getAllPages.result.pages[].tags",
                "getAllPages.result.pages[].updatedAt",
                "getAllPages.result.pages[].uuid",
            ],
            retyped: &[
                // The official `journal?` is a boolean and `journalDay` a number; this surface
                // states `kind` and `journalDate` instead, which the absent list already covers;
                // `name` is the one shared key and both state it as a string.
            ],
            empty: &[],
        },
    );
}

/// #3 `logseq.App.getUserConfigs`: the graph's own journal title format answers as the official
/// `preferredDateFormat`, and the app-state keys are the documented difference.
#[test]
fn user_configs_answers_the_graph_facts_and_omits_app_state() {
    let sandbox = sandbox();
    let server = Server::start(&sandbox.graph_root(), "shape-configs");
    let captured = row("get_user_configs.json");
    let ours = server.call("logseq.App.getUserConfigs", json!([])).result();

    assert_eq!(
        ours["preferredDateFormat"],
        json!("yyyy-MM-dd EEEE"),
        "the graph's own :journal/page-title-format answers, as the official answer states it: {ours}"
    );
    assert_eq!(
        ours["currentGraph"],
        json!(format!(
            "logseq_local_{}",
            sandbox
                .graph_root()
                .canonicalize()
                .expect("the sandbox path canonicalises")
                .display()
        )),
        "the official `currentGraph` identity: {ours}"
    );
    assert_shape(
        "getUserConfigs.result",
        &captured["result"],
        &ours,
        Differences {
            absent: &[
                // Row #3: the official answer is app state. These keys live in the app's settings,
                // not in a graph file, so a file-first projection cannot state them; the ones that
                // would be config.edn keys (`:preferred-language`, `:preferred-todo`,
                // `:preferred-workflow`) are a follow-up slice with no consumer yet (HANDOFF).
                "getUserConfigs.result.enabledFlashcards",
                "getUserConfigs.result.enabledJournals",
                "getUserConfigs.result.preferredLanguage",
                "getUserConfigs.result.preferredStartOfWeek",
                "getUserConfigs.result.preferredThemeMode",
                "getUserConfigs.result.preferredTodo",
                "getUserConfigs.result.preferredWorkflow",
                "getUserConfigs.result.showBrackets",
            ],
            retyped: &[],
            empty: &[],
        },
    );
}

/// `preferredDateFormat` is read from `config.edn` on each call, so an edit the watcher re-reads
/// cannot leave the answer stale; the call happens after the write, no notification wait needed.
#[test]
fn user_configs_reads_the_date_format_per_call() {
    let sandbox = sandbox();
    let server = Server::start_without_watcher(&sandbox.graph_root(), "shape-configs-fresh");
    assert_eq!(
        server.call("logseq.App.getUserConfigs", json!([])).result()["preferredDateFormat"],
        json!("yyyy-MM-dd EEEE")
    );

    sandbox.write(
        "logseq/config.edn",
        "{:journal/page-title-format \"do MMM yyyy\"}\n",
    );
    assert_eq!(
        server.call("logseq.App.getUserConfigs", json!([])).result()["preferredDateFormat"],
        json!("do MMM yyyy"),
        "the next call states the edited pattern, not the start-up one"
    );
}

/// #4 `logseq.Editor.getPage`: the official top-level entity fields, with the folded `name` the
/// official answer states and the file spelling kept in `originalName`.
#[test]
fn get_page_states_the_official_entity_fields_with_a_folded_name() {
    let sandbox = sandbox();
    let server = Server::start(&sandbox.graph_root(), "shape-page");
    let captured = row("get_page.json");
    let ours = server
        .call("logseq.Editor.getPage", json!(["Bravo"]))
        .result();

    assert_eq!(
        (ours["name"].clone(), ours["originalName"].clone()),
        (json!("bravo"), json!("Bravo")),
        "the official answer folds `name` and keeps the written spelling in `originalName`: {ours}"
    );
    assert_shape(
        "getPage.result",
        &captured["result"],
        &ours,
        Differences {
            absent: &[
                // Rows #4, M3/M8: a file page has no DB id, no created/updated timestamps, no alias
                // entity list and no property-text projection.
                "getPage.result.alias",
                "getPage.result.createdAt",
                "getPage.result.file",
                "getPage.result.id",
                "getPage.result.propertiesTextValues",
                "getPage.result.updatedAt",
            ],
            // Row #4: the official entity is a key/value map; this surface keeps the preamble's
            // ordered, repeatable lines as a list.
            retyped: &["getPage.result.properties"],
            empty: &[],
        },
    );
}

/// #5 `logseq.Editor.getPageBlocksTree`: the official answer is an array of DB block entities; this
/// surface answers a `blocks` list of preorder/depth entries (rows #5, ADR-0012, ADR-0013).
#[test]
fn page_blocks_tree_states_its_documented_entry_shape() {
    let sandbox = sandbox();
    let server = Server::start(&sandbox.graph_root(), "shape-tree");
    let captured = row("get_page_blocks_tree.json");
    let ours = server
        .call("logseq.Editor.getPageBlocksTree", json!(["alpha"]))
        .result();

    assert!(
        ours["blocks"]
            .as_array()
            .is_some_and(|blocks| !blocks.is_empty()),
        "the replay page answers at least one block: {ours}"
    );
    assert_shape(
        "getPageBlocksTree.result.blocks[]",
        &captured["result"][0],
        &ours["blocks"][0],
        Differences {
            absent: &[
                // Row #5: DB entity fields this projection does not state. `content` is `text`
                // here, and the pre-block is not materialized (ADR-0013).
                "getPageBlocksTree.result.blocks[].children",
                "getPageBlocksTree.result.blocks[].content",
                "getPageBlocksTree.result.blocks[].createdAt?",
                "getPageBlocksTree.result.blocks[].format",
                "getPageBlocksTree.result.blocks[].id",
                "getPageBlocksTree.result.blocks[].invalidProperties",
                "getPageBlocksTree.result.blocks[].left",
                "getPageBlocksTree.result.blocks[].level",
                "getPageBlocksTree.result.blocks[].parent",
                "getPageBlocksTree.result.blocks[].pathRefs",
                "getPageBlocksTree.result.blocks[].preBlock?",
                "getPageBlocksTree.result.blocks[].properties",
                "getPageBlocksTree.result.blocks[].propertiesOrder",
                "getPageBlocksTree.result.blocks[].propertiesTextValues",
                "getPageBlocksTree.result.blocks[].refs",
                "getPageBlocksTree.result.blocks[].uuid",
            ],
            // Row #5: the official `page` is an entity reference; this surface states the page name.
            retyped: &["getPageBlocksTree.result.blocks[].page"],
            empty: &[],
        },
    );
}

/// #6 `logseq.Editor.getPageLinkedReferences`: the official grouping answers `[page, blocks]`
/// pairs; this surface answers `backlinks` triples (row #6, M5, M3). The container difference is the
/// whole claim, so it is asserted by name rather than key by key.
#[test]
fn page_linked_references_states_its_documented_shape() {
    let sandbox = sandbox();
    let server = Server::start(&sandbox.graph_root(), "shape-backlinks");
    let captured = row("get_page_linked_references.json");
    let ours = server
        .call("logseq.Editor.getPageLinkedReferences", json!(["alpha"]))
        .result();

    assert!(
        captured["result"][0][0]["originalName"].as_str().is_some(),
        "the capture still groups blocks under a page entity"
    );
    assert!(
        ours["backlinks"]
            .as_array()
            .is_some_and(|backlinks| !backlinks.is_empty()),
        "the replay page has a backlink: {ours}"
    );
    assert!(
        ours["backlinks"][0]["text"].as_str().is_some(),
        "a backlink states the referencing block's text: {ours}"
    );
}

/// #9 `logseq.App.search`: the official groups and the `has-more?` boolean, plus the round trip the
/// extension makes — `block/uuid` from a hit read back through `Editor.getBlock`.
#[test]
fn app_search_states_the_official_groups_and_a_bare_block_uuid_round_trips() {
    let sandbox = sandbox();
    let server = Server::start(&sandbox.graph_root(), "shape-search");
    let captured = row("app_search.json");
    let ours = server.call("logseq.App.search", json!(["alpha"])).result();

    assert_shape(
        "search.result",
        &captured["result"],
        &ours,
        Differences {
            absent: &[],
            // Row #9: official `block/page` is the DB page entity id; a file page has no numeric
            // id, so this surface states the page name.
            retyped: &["search.result.blocks[].block/page"],
            // Row #9: no asset-file or page-content hits exist in this projection; both keys are
            // stated empty, as the official client tolerates.
            empty: &["search.result.files", "search.result.pages-content"],
        },
    );

    // The official client's own path: map `result.blocks`, hand `block/uuid` back to getBlock.
    let blocks = ours["blocks"].as_array().expect("blocks is an array");
    assert!(!blocks.is_empty(), "{ours}");
    assert!(
        blocks.len() <= 10,
        "the official blocks group is capped at 10: {ours}"
    );
    assert!(ours["has-more?"].is_boolean(), "{ours}");
    let uuid = blocks[0]["block/uuid"]
        .as_str()
        .expect("a bare block id, as the official answer states it");
    assert!(
        !uuid.starts_with("durable:") && !uuid.starts_with("ephemeral:"),
        "the official `block/uuid` is the bare id: {uuid}"
    );
    let back = server
        .call("logseq.Editor.getBlock", json!([uuid]))
        .result();
    assert_eq!(
        back["uuid"],
        json!(uuid),
        "a bare id from a search hit must round-trip through getBlock: {back}"
    );
    assert_eq!(
        back["page"]["name"], blocks[0]["block/page"],
        "the block read states the page the hit named: {back}"
    );
}

/// #12 `logseq.DB.q`: the official bare array of page entities, answered from a page-level query.
#[test]
fn db_q_states_the_official_page_entity_fields() {
    let sandbox = sandbox();
    let server = Server::start(&sandbox.graph_root(), "shape-dbq");
    let captured = row("db_q.json");
    let ours = server
        .call("logseq.DB.q", json!(["(page-property status active)"]))
        .result();
    let pages = ours.as_array().expect("the official bare array");
    assert!(!pages.is_empty(), "{ours}");

    assert_shape(
        "dbQ.result[]",
        &captured["result"][0],
        &pages[0],
        Differences {
            absent: &[
                // Row #12: DB entity fields a file page does not hold.
                "dbQ.result[].createdAt",
                "dbQ.result[].file",
                "dbQ.result[].id",
                "dbQ.result[].properties",
                "dbQ.result[].propertiesTextValues",
                "dbQ.result[].tags",
                "dbQ.result[].updatedAt",
            ],
            retyped: &[],
            empty: &[],
        },
    );
    assert_eq!(
        pages[0]["name"],
        json!("tagged"),
        "the official page entry folds `name` and keeps `originalName`: {ours}"
    );
}

/// #13 `logseq.Editor.getBlock`: the official top-level block fields.
#[test]
fn get_block_states_the_official_top_level_fields() {
    let sandbox = sandbox();
    let server = Server::start(&sandbox.graph_root(), "shape-block");
    let captured = row("get_block.json");
    let ours = server
        .call(
            "logseq.Editor.getBlock",
            json!(["01234567-89ab-cdef-0123-456789abcdef"]),
        )
        .result();

    assert_shape(
        "getBlock.result",
        &captured["result"],
        &ours,
        Differences {
            absent: &[
                // Rows #13, ADR-0012: DB entity internals this projection does not state.
                "getBlock.result.children",
                "getBlock.result.id",
                "getBlock.result.left",
                "getBlock.result.parent",
                "getBlock.result.pathRefs",
            ],
            retyped: &[],
            empty: &[],
        },
    );
    assert_eq!(
        ours["uuid"],
        json!("01234567-89ab-cdef-0123-456789abcdef"),
        "the official `uuid` is the bare id: {ours}"
    );
}

/// The captured methods this slice does not serve must answer `MethodNotExist`, and the capture
/// must be listed in [`UNIMPLEMENTED`]: an unimplemented captured method cannot disappear silently.
#[test]
fn unimplemented_captures_answer_method_not_exist() {
    let sandbox = sandbox();
    let server = Server::start(&sandbox.graph_root(), "shape-unimplemented");

    for (file, row) in UNIMPLEMENTED {
        let capture = fixture(file);
        let method = capture["method"]
            .as_str()
            .expect("the capture names a method");
        let args = capture["args"].clone();
        let error = server.call(method, args).expect_code(400, -32601);
        assert_eq!(
            error["data"]["kind"], "method-not-found",
            "{method} ({row}) must state the method-not-found kind: {error}"
        );
    }

    // Every capture on disk is either asserted elsewhere in this file or listed above; a new
    // capture that is neither fails here.
    let asserted = [
        "get_current_graph.json",
        "get_all_pages.json",
        "get_user_configs.json",
        "get_page.json",
        "get_page_blocks_tree.json",
        "get_page_linked_references.json",
        "app_search.json",
        "db_q.json",
        "get_block.json",
    ];
    for file in asserted
        .iter()
        .chain(UNIMPLEMENTED.iter().map(|(file, _)| file))
    {
        assert!(
            fixtures_dir().join(file).is_file(),
            "the shape test names {file}, which is not a capture"
        );
    }
    let mut captured: Vec<String> = std::fs::read_dir(fixtures_dir())
        .expect("list the capture directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json"))
        .collect();
    let mut covered: Vec<String> = asserted
        .iter()
        .chain(UNIMPLEMENTED.iter().map(|(file, _)| file))
        .map(|file| (*file).to_owned())
        .collect();
    captured.sort();
    covered.sort();
    assert_eq!(
        captured, covered,
        "every captured answer must be replayed or listed as unimplemented, so a capture cannot be added and ignored"
    );
}

/// The extension's reads must all answer; a method it calls that this surface does not serve is the
/// failure mode this test exists for.
#[test]
fn extension_reads_are_all_answered() {
    let sandbox = sandbox();
    let server = Server::start(&sandbox.graph_root(), "shape-extension");

    for (method, args) in extension_reads() {
        let response = server.call(method, args.clone());
        assert_eq!(
            response.status, 200,
            "the Logseq Copilot extension calls {method}; this surface answers: {}",
            response.body
        );
    }

    for method in EXTENSION_WRITES_NOT_SERVED {
        let error = server
            .call(method, json!(["alpha", "content"]))
            .expect_code(400, -32601);
        assert_eq!(
            error["data"]["kind"], "method-not-found",
            "{method} is an official write name; ADR-0014 keeps every served write under \
             logseq.Graph.*, so the official name must not silently alias one: {error}"
        );
    }
}

/// `logseq.Editor.getCurrentPage` answers the official "no current page" value: there is no editor
/// (ADR-0003), and the client's feature degrades instead of parsing a method-not-found.
#[test]
fn get_current_page_answers_null_without_an_editor() {
    let sandbox = sandbox();
    let server = Server::start(&sandbox.graph_root(), "shape-current-page");
    assert_eq!(
        server
            .call("logseq.Editor.getCurrentPage", json!([]))
            .result(),
        Value::Null
    );
}

/// The extension's own argument spellings, which the captured fixtures do not carry: a page
/// identity object (`{name}` / `{uuid}`), a `null` where an optional `{includeChildren}` argument
/// was omitted (`JSON.stringify([uuid, undefined])`), and `block.page` handed straight to
/// `Editor.getPage`. Each used to answer a structured invalid-params error and break the search
/// panel after the wire shape was fixed.
#[test]
fn extension_argument_spellings_resolve_without_a_structured_error() {
    let sandbox = sandbox();
    let server = Server::start(&sandbox.graph_root(), "shape-extension-args");

    let by_name = server
        .call("logseq.Editor.getPage", json!([{"name": "alpha"}]))
        .result();
    assert_eq!(by_name["name"], json!("alpha"), "{by_name}");
    let by_uuid = server
        .call("logseq.Editor.getPage", json!([{"uuid": "alpha"}]))
        .result();
    assert_eq!(by_uuid["name"], json!("alpha"), "{by_uuid}");

    let durable = "01234567-89ab-cdef-0123-456789abcdef";
    let with_null = server
        .call("logseq.Editor.getBlock", json!([durable, Value::Null]))
        .result();
    assert_eq!(with_null["uuid"], json!(durable), "{with_null}");
    assert!(
        with_null["page"]["name"].as_str().is_some(),
        "block.page is an object a client can hand to getPage: {with_null}"
    );
    // The recall client drops a block without a numeric `block.page.id`
    // (`file-source.ts`: `if (!block?.uuid || block.page?.id === undefined) return null;`) and
    // then reads the page by `getPage({ id })`. Both must work.
    let numeric = with_null["page"]["id"]
        .as_u64()
        .unwrap_or_else(|| panic!("block.page.id is a number: {with_null}"));
    let by_id = server
        .call("logseq.Editor.getPage", json!([{"id": numeric}]))
        .result();
    assert_eq!(by_id["name"], json!("bravo"), "{by_id}");
    // The extension's `getPage` sends the bare number, not an object:
    // `[id || uuid || name]` from its identity argument.
    let by_bare_number = server
        .call("logseq.Editor.getPage", json!([numeric]))
        .result();
    assert_eq!(by_bare_number["name"], json!("bravo"), "{by_bare_number}");
    // The exact chain the extension runs: getBlock(hit) -> getPage(block.page).
    let page = server
        .call(
            "logseq.Editor.getPage",
            json!([{"name": with_null["page"]["name"]}]),
        )
        .result();
    assert_eq!(page["name"], json!("bravo"), "{page}");

    // `DB.q` entries go through the same `prepareBlock` path as search hits.
    let found = server
        .call("logseq.DB.q", json!(["\"alpha root\""]))
        .result();
    assert_eq!(found[0]["page"]["name"], json!("alpha"), "{found}");
}
