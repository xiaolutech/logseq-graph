// Configuration, version-token and snapshot reads (#3, #10, #11).
//
// Every case names its official method and parity-matrix row; see the crate root.

use super::*;
use serde_json::{json, Value};

// ------------------------------------------------------------------------------------------------
// #3 /favorites ↔ logseq.App.getUserConfigs — deliberate-difference. The official method answers
// the whole static configuration; ours has its own `logseq.Graph.getFavorites` read of the
// `:favorites` list, each entry with an `exists` flag instead of being filtered (ADR-0001: a
// pinned page that was deleted is a real state of the configuration, not a missing page).
// ------------------------------------------------------------------------------------------------
#[test]
fn get_favorites_matches_logseq_App_getUserConfigs() {
    let sandbox = Sandbox::new("favorites");
    sandbox.write(
        "logseq/config.edn",
        "{:favorites [\"alpha\" \"Bravo\" \"gone\" \"阅读 notes/page one\"]}\n",
    );
    let server = Server::start(&sandbox.graph_root(), "favorites");

    let result = server.call(METHODS[2], json!([])).result();
    let favorites = result["favorites"].as_array().expect("a favorites array");
    let answers: Vec<(&str, bool)> = favorites
        .iter()
        .map(|favorite| {
            (
                favorite["name"].as_str().expect("a name"),
                favorite["exists"].as_bool().expect("an existence flag"),
            )
        })
        .collect();
    assert_eq!(
        answers,
        [
            ("alpha", true),
            ("Bravo", true),
            ("gone", false),
            ("阅读 notes/page one", true),
        ],
        "the list is answered as written, missing pages included: {result}"
    );
}

// ------------------------------------------------------------------------------------------------
// #10 /page-version ↔ no official method (no `logseq.*` call answers a version token; matrix
// row #10 is out-of-scope) — out-of-scope. The
// official API has no version-token read. ADR-0008's amendment says a read that answers from a
// page file declares no freshness policy; this test keeps that read's own behaviour (two tokens,
// `exists`, and every name answered) without claiming equivalence.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_page_version_is_out_of_scope_and_keeps_its_version_tokens() {
    let sandbox = Sandbox::new("page-version");
    let server = Server::start_without_watcher(&sandbox.graph_root(), "page-version");

    let result = server.call(METHODS[9], json!(["alpha"])).result();
    assert_eq!(result["exists"], json!(true), "alpha is a page: {result}");
    let current = result["disk_version"]
        .as_str()
        .unwrap_or_else(|| panic!("no disk version in {result}"))
        .to_owned();
    assert_eq!(
        result["index_version"],
        json!(current),
        "the startup scan projected these bytes: {result}"
    );
    assert_eq!(result["stale"], json!(false), "the Meta envelope: {result}");
    assert!(result["revision"].is_u64(), "the Meta envelope: {result}");

    let absent = server.call(METHODS[9], json!(["absent"])).result();
    assert_eq!(absent["exists"], json!(false), "{absent}");
    assert_eq!(absent["index_version"], Value::Null, "{absent}");
    assert_eq!(absent["disk_version"], Value::Null, "{absent}");

    // A same-length rewrite with its modification time restored proves the token is neither the
    // size nor the mtime: the file moves but the index side does not.
    let path = sandbox.graph_root().join("pages/alpha.md");
    let observed = fs::metadata(&path)
        .expect("stat alpha")
        .modified()
        .expect("mtime");
    sandbox.write(
        "pages/alpha.md",
        "- alphx root\n\t- alpha child\n\t\t- alpha grandchild\n",
    );
    fs::File::options()
        .write(true)
        .open(&path)
        .expect("open alpha")
        .set_modified(observed)
        .expect("restore mtime");

    let changed = server.call(METHODS[9], json!(["alpha"])).result();
    assert_eq!(
        changed["index_version"],
        json!(current),
        "no scan ran: {changed}"
    );
    assert_ne!(
        changed["disk_version"],
        json!(current),
        "the bytes changed: {changed}"
    );
    assert_eq!(
        changed["exists"],
        json!(true),
        "the page still exists: {changed}"
    );

    fs::remove_file(sandbox.graph_root().join("pages/Bravo.md")).expect("delete Bravo");
    let deleted = server.call(METHODS[9], json!(["Bravo"])).result();
    assert_eq!(deleted["exists"], json!(false), "{deleted}");
    assert!(
        deleted["index_version"].is_string(),
        "the index keeps its token: {deleted}"
    );
    assert_eq!(deleted["disk_version"], Value::Null, "{deleted}");
}

// ------------------------------------------------------------------------------------------------
// #10 /page-version ↔ no official method (no `logseq.*` call answers a version token; out-of-scope)
// — deliberate-difference, and the reason is
// ADR-0008's amendment: this read answers from the file, so a `freshness` policy beside it would
// read like permission to skip the file read. Giving an extra policy must be an error, not a
// silent downgrade.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_page_version_refuses_an_extra_freshness_policy() {
    let sandbox = Sandbox::new("page-version-freshness");
    let server = Server::start(&sandbox.graph_root(), "page-version-freshness");

    for freshness in ["last", "check", "verify"] {
        let error = server
            .call(
                METHODS[9],
                with_options("alpha", json!({"freshness": freshness})),
            )
            .expect_code(400, -32602);
        assert_eq!(error["data"]["kind"], json!("no-policy-allowed"), "{error}");
        assert!(
            error["message"].as_str().unwrap().contains("freshness"),
            "the refusal must say why: {error}"
        );
    }
}

// ------------------------------------------------------------------------------------------------
// #11 /snapshot ↔ logseq.Editor.getPageBlocksTree — deliberate-difference. ADR-0008's amendment:
// this read answers one page's current file bytes under their own `version`, while the official
// method answers the DB. A snapshot states a `selector` only where the file writes `id::`
// (ADR-0012: an ephemeral id is minted by the projection from rows the file read never sees).
// ------------------------------------------------------------------------------------------------
#[test]
fn get_page_snapshot_matches_getPageBlocksTree() {
    let sandbox = Sandbox::new("snapshot");
    sandbox.write(
        "pages/shaped.md",
        "tags:: todo\nrating:: 4\n\n\
         - shaped root\n\t- shaped child\n\t\t- shaped grandchild\n- shaped second\n",
    );
    let server = Server::start(&sandbox.graph_root(), "snapshot");

    let result = server.call(METHODS[10], json!(["shaped"])).result();
    assert_eq!(result["exists"], json!(true), "shaped is a page: {result}");
    let version = result["version"]
        .as_str()
        .unwrap_or_else(|| panic!("no version in {result}"))
        .to_owned();
    let page_version = server.call(METHODS[9], json!(["shaped"])).result();
    assert_eq!(
        page_version["disk_version"],
        json!(version),
        "one observation, two readings: {page_version}"
    );

    let properties: Vec<(&str, &str)> = result["properties"]
        .as_array()
        .expect("a properties array")
        .iter()
        .map(|property| {
            (
                property["key"].as_str().expect("a key"),
                property["value"].as_str().expect("a value"),
            )
        })
        .collect();
    assert_eq!(properties, [("tags", "todo"), ("rating", "4")], "{result}");

    let blocks = result["blocks"].as_array().expect("a blocks array");
    let texts: Vec<&str> = blocks
        .iter()
        .map(|block| block["text"].as_str().expect("a text"))
        .collect();
    assert_eq!(
        texts,
        [
            "shaped root",
            "shaped child",
            "shaped grandchild",
            "shaped second"
        ],
        "depth-first order: {result}"
    );
    let depths: Vec<u64> = blocks
        .iter()
        .map(|block| block["depth"].as_u64().expect("a depth"))
        .collect();
    assert_eq!(depths, [0, 1, 2, 0], "{result}");
    let preorders: Vec<u64> = blocks
        .iter()
        .map(|block| block["preorder"].as_u64().expect("a preorder"))
        .collect();
    assert_eq!(preorders, [0, 1, 2, 3], "{result}");
    assert_eq!(blocks[0]["marker"], json!("-"), "{result}");

    let tree = server.call(METHODS[4], json!(["shaped"])).result();
    let tree_blocks = tree["blocks"].as_array().expect("a blocks array");
    assert_eq!(tree_blocks.len(), blocks.len(), "{tree}");
    for (snapshot, tree) in blocks.iter().zip(tree_blocks) {
        for field in ["page", "preorder", "depth", "marker", "text"] {
            assert_eq!(
                snapshot[field], tree[field],
                "{field} disagrees: {result} / {tree}"
            );
        }
        assert_eq!(
            snapshot["selector"],
            Value::Null,
            "a file read mints no id: {result}"
        );
        assert_eq!(tree["selector"]["kind"], json!("ephemeral"), "{tree}");
    }
}

// ------------------------------------------------------------------------------------------------
// #11 /snapshot ↔ logseq.Editor.getPageBlocksTree — deliberate-difference (ADR-0012). A snapshot is decoded
// from the file alone: a block whose file states no `id::` has no identity to report (`null`), and
// the one identity it can vouch for is the durable id the file writes.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_page_snapshot_states_a_selector_only_for_file_ids() {
    let sandbox = Sandbox::new("snapshot-selector");
    sandbox.write(
        "pages/mixed.md",
        "- plain block\n- identified block\n  id:: 01234567-89ab-cdef-0123-456789abcdef\n",
    );
    let server = Server::start(&sandbox.graph_root(), "snapshot-selector");

    let result = server.call(METHODS[10], json!(["mixed"])).result();
    let blocks = result["blocks"].as_array().expect("a blocks array");
    assert_eq!(blocks.len(), 2, "{result}");
    assert_eq!(
        blocks[0]["selector"],
        Value::Null,
        "no id:: in the file: {result}"
    );
    assert_eq!(
        blocks[1]["selector"],
        json!({"kind": "durable", "id": "01234567-89ab-cdef-0123-456789abcdef"}),
        "the file-stated id: {result}"
    );
}

// ------------------------------------------------------------------------------------------------
// #11 /snapshot ↔ logseq.Editor.getPageBlocksTree — deliberate-difference (ADR-0008's amendment). The answer
// is the file as it is now, not the rows a scan projected, so bytes edited since the scan are
// answered without naming a freshness policy.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_page_snapshot_answers_the_bytes_edited_since_the_scan() {
    let sandbox = Sandbox::new("snapshot-current");
    let server = Server::start_without_watcher(&sandbox.graph_root(), "snapshot-current");

    let indexed = server.call(METHODS[10], json!(["alpha"])).result();
    let indexed_version = indexed["version"].as_str().expect("a version").to_owned();
    sandbox.write("pages/alpha.md", "- alpha rewritten\n");

    let result = server.call(METHODS[10], json!(["alpha"])).result();
    let blocks = result["blocks"].as_array().expect("a blocks array");
    assert_eq!(blocks.len(), 1, "the file now holds one block: {result}");
    assert_eq!(blocks[0]["text"], json!("alpha rewritten"), "{result}");
    assert_ne!(
        result["version"],
        json!(indexed_version),
        "the bytes changed: {result}"
    );

    let versions = server.call(METHODS[9], json!(["alpha"])).result();
    assert_eq!(
        versions["index_version"],
        json!(indexed_version),
        "no scan ran: {versions}"
    );
    assert_ne!(
        versions["disk_version"],
        json!(indexed_version),
        "{versions}"
    );
}

// ------------------------------------------------------------------------------------------------
// #11 /snapshot ↔ logseq.Editor.getPageBlocksTree — deliberate-difference. The official method returns `[]` for
// an alias entity; ours answers `exists: false` for a name it cannot resolve (ADR-0008's
// amendment), so a poll loop branches on a field instead of unwrapping a request error.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_page_snapshot_answers_an_unknown_page_with_exists_false() {
    let sandbox = Sandbox::new("snapshot-absent");
    let server = Server::start(&sandbox.graph_root(), "snapshot-absent");

    let result = server.call(METHODS[10], json!(["absent"])).result();
    assert_eq!(result["exists"], json!(false), "{result}");
    assert_eq!(result["version"], Value::Null, "{result}");
    assert_eq!(result["properties"], json!([]), "{result}");
    assert_eq!(result["blocks"], json!([]), "{result}");
}

// ------------------------------------------------------------------------------------------------
// #11 /snapshot ↔ logseq.Editor.getPageBlocksTree — deliberate-difference (ADR-0008's amendment): like
// `getPageVersion`, this file-tier read declares no freshness policy and refuses one.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_page_snapshot_refuses_an_extra_freshness_policy() {
    let sandbox = Sandbox::new("snapshot-freshness");
    let server = Server::start(&sandbox.graph_root(), "snapshot-freshness");

    for freshness in ["last", "check", "verify"] {
        let error = server
            .call(
                METHODS[10],
                with_options("alpha", json!({"freshness": freshness})),
            )
            .expect_code(400, -32602);
        assert_eq!(error["data"]["kind"], json!("no-policy-allowed"), "{error}");
        assert!(
            error["message"].as_str().unwrap().contains("freshness"),
            "the refusal must say why: {error}"
        );
    }
}

// ------------------------------------------------------------------------------------------------
// #11 /snapshot ↔ logseq.Editor.getPageBlocksTree — deliberate-difference. A file that cannot be decoded is a
// structured internal error naming the file and the cause, not an empty page. The official DB read
// has no file tier, so this failure mode has no official counterpart.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_page_snapshot_reports_an_undecodable_file_as_a_structured_error() {
    let sandbox = Sandbox::new("snapshot-undecodable");
    let server = Server::start_without_watcher(&sandbox.graph_root(), "snapshot-undecodable");
    fs::write(
        sandbox.graph_root().join("pages/alpha.md"),
        [0xff, 0xfe, 0x00],
    )
    .expect("write undecodable bytes");

    let error = server
        .call(METHODS[10], json!(["alpha"]))
        .expect_code(500, -32603);
    assert_eq!(error["data"]["kind"], json!("internal"), "{error}");
    let message = error["message"].as_str().expect("a message");
    assert!(
        message.contains("alpha.md"),
        "the reason names the file: {error}"
    );
    assert!(
        message.contains("UTF-8"),
        "the reason names the cause: {error}"
    );
}
