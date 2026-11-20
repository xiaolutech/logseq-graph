// Served writes: end-to-end effects and the refusals ADR-0014 preserves.
//
// Every case names its official method and parity-matrix row; see the crate root.

use super::*;
use serde_json::{json, Value};

const DURABLE_BLOCK: &str = "01234567-89ab-cdef-0123-456789abcdef";
const CREATE_JOURNAL: &str = "logseq.Graph.createJournal";
const INSERT_BLOCK: &str = "logseq.Graph.insertBlock";
const INSERT_BATCH: &str = "logseq.Graph.insertBatch";
const PREPEND_BLOCK: &str = "logseq.Graph.prependBlockInPage";
const UPDATE_BLOCK: &str = "logseq.Graph.updateBlock";
const REMOVE_BLOCK: &str = "logseq.Graph.removeBlock";
const REMOVE_BLOCK_RANGE: &str = "logseq.Graph.removeBlockRange";
const UPSERT_BLOCK_PROPERTY: &str = "logseq.Graph.upsertBlockProperty";
const REMOVE_BLOCK_PROPERTY: &str = "logseq.Graph.removeBlockProperty";
const SET_BLOCK_COLLAPSED: &str = "logseq.Graph.setBlockCollapsed";
const DELETE_PAGE: &str = "logseq.Graph.deletePage";

#[test]
fn remove_block_range_deletes_siblings_and_reports_typed_refusals() {
    let sandbox = Sandbox::new("remove-block-range");
    sandbox.write("pages/alpha.md", "- first\n  - child\n- last\n- keep\n");
    let server = Server::start(&sandbox.graph_root(), "remove-block-range");
    let first = ephemeral_selector(&server, "alpha", 0);
    let child = ephemeral_selector(&server, "alpha", 1);
    let last = ephemeral_selector(&server, "alpha", 2);
    let before = sandbox.read("pages/alpha.md");
    for (args, kind, edit_kind) in [
        (json!([last, first]), "write-edit", Some("range-reversed")),
        (
            json!([first, child]),
            "write-edit",
            Some("range-not-siblings"),
        ),
        (json!([first, DURABLE_BLOCK]), "range-across-pages", None),
    ] {
        let error = server
            .call(REMOVE_BLOCK_RANGE, args)
            .expect_code(400, -32602);
        assert_eq!(error["data"]["kind"], kind);
        assert_eq!(error["data"]["write_state"], "unwritten");
        if let Some(edit_kind) = edit_kind {
            assert_eq!(error["data"]["edit_kind"], edit_kind);
        }
        assert_eq!(sandbox.read("pages/alpha.md"), before);
    }
    let removed = server
        .call(REMOVE_BLOCK_RANGE, json!([first, last]))
        .result();
    assert_eq!(removed["page"]["name"], "alpha");
    assert_eq!(removed["page"]["block_count"], 1);
    assert_eq!(sandbox.read("pages/alpha.md"), b"- keep\n");
    for args in [
        json!([]),
        json!([first]),
        json!(["", last]),
        json!([first, last, "extra"]),
    ] {
        server
            .call(REMOVE_BLOCK_RANGE, args)
            .expect_code(400, -32602);
    }
}

#[test]
fn ensure_block_id_explicitly_grants_identity_and_reports_a_noop_on_repeat() {
    let sandbox = Sandbox::new("ensure-block-id");
    let server = Server::start(&sandbox.graph_root(), "ensure-block-id");
    let method = "logseq.Graph.ensureBlockId";
    let selector = ephemeral_selector(&server, "Bravo", 0);
    let answer = server.call(method, json!([selector])).result();
    assert_eq!(answer["created"], true);
    assert_eq!(answer["block"]["selector"]["kind"], "durable");
    assert_eq!(answer["id"], answer["block"]["selector"]["id"]);
    let id = answer["id"].as_str().expect("bare UUID");
    assert!(logseq_graph::domain::BlockId::parse(id).is_ok());
    let bytes = sandbox.read("pages/Bravo.md");
    assert!(String::from_utf8(bytes.clone())
        .expect("UTF-8")
        .contains(&format!("id:: {id}")));
    let repeated = server.call(method, json!([id])).result();
    assert_eq!(repeated["created"], false);
    assert_eq!(repeated["id"], answer["id"]);
    assert_eq!(repeated["revision"], answer["revision"]);
    assert_eq!(sandbox.read("pages/Bravo.md"), bytes);
    for args in [json!([]), json!([""]), json!([id, "extra"])] {
        server.call(method, args).expect_code(400, -32602);
        assert_eq!(sandbox.read("pages/Bravo.md"), bytes);
    }
    let error = server
        .call(method, json!(["ffffffff-ffff-4fff-8fff-ffffffffffff"]))
        .expect_code(404, -32004);
    assert_eq!(error["data"]["write_state"], "unwritten");
    assert_eq!(sandbox.read("pages/Bravo.md"), bytes);
}

#[test]
fn journal_writes_use_the_configuration_published_by_the_watcher() {
    let sandbox = Sandbox::new("journal-config-write");
    let server =
        Server::spawn_with_poll_interval(&sandbox.graph_root(), "journal-config-write", 20);
    server.wait_until_ready("journal-config-write");
    sandbox.write(
        "logseq/config.edn",
        "{:journal/page-title-format \"yyyy-MM-dd\"}\n",
    );
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let pages = server.call(METHODS[1], json!([])).result();
        if pages["pages"]
            .as_array()
            .expect("pages")
            .iter()
            .any(|page| page["name"] == "2026-08-13")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not publish the new title"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let answer = server
        .call(APPEND_BLOCK, json!(["2026-08-13", "new title append"]))
        .result();
    assert_eq!(answer["page"]["name"], "2026-08-13");
    let bytes = sandbox.read("journals/2026_08_13.md");
    let error = server
        .call(APPEND_BLOCK, json!(["Aug 13th, 2026", "obsolete title"]))
        .expect_code(404, -32004);
    assert_eq!(error["data"]["write_state"], "unwritten");
    assert_eq!(sandbox.read("journals/2026_08_13.md"), bytes);
    let versions = server.call(METHODS[9], json!(["2026-08-13"])).result();
    assert_eq!(versions["index_version"], versions["disk_version"]);
    assert!(String::from_utf8(bytes)
        .expect("UTF-8")
        .contains("new title append"));
}

fn page_version(server: &Server, page: &str) -> String {
    server
        .call("logseq.Graph.getPageSnapshot", json!([page]))
        .result()["version"]
        .as_str()
        .expect("a page snapshot states its source version")
        .to_owned()
}

fn tree_texts(server: &Server, page: &str) -> Vec<String> {
    server
        .call("logseq.Editor.getPageBlocksTree", json!([page]))
        .result()["blocks"]
        .as_array()
        .expect("a page tree states blocks")
        .iter()
        .map(|block| {
            block["text"]
                .as_str()
                .expect("a tree block states text")
                .to_owned()
        })
        .collect()
}

fn ephemeral_selector(server: &Server, page: &str, preorder: u64) -> String {
    let tree = server
        .call("logseq.Editor.getPageBlocksTree", json!([page]))
        .result();
    let block = tree["blocks"]
        .as_array()
        .expect("a page tree states blocks")
        .iter()
        .find(|block| block["preorder"] == json!(preorder))
        .unwrap_or_else(|| panic!("no block at preorder {preorder}: {tree}"));
    assert_eq!(block["selector"]["kind"], json!("ephemeral"), "{block}");
    let id = block["selector"]["id"]
        .as_str()
        .expect("an ephemeral selector states its id");
    format!("ephemeral:{id}")
}

fn assert_selector(value: &Value, kind: &str) {
    assert_eq!(value["kind"], json!(kind), "{value}");
    assert!(value["id"].as_str().is_some(), "{value}");
}

fn selector_text(value: &Value) -> String {
    format!(
        "{}:{}",
        value["kind"].as_str().expect("a selector kind"),
        value["id"].as_str().expect("a selector id")
    )
}

fn commit(server: &Server, page: &str, base_version: &str, operations: Value) -> Response {
    server.call(
        COMMIT_PAGE,
        json!([
            page,
            base_version,
            operations.as_array().expect("operations are an array")
        ]),
    )
}

// logseq.Editor.createPage — out-of-scope. The official write table is not the thirteen-read
// parity basis. This served write creates one file-backed page and the ordinary read sees it
// without an intervening scan request (ADR-0014).
#[test]
fn a_created_page_is_immediately_readable_matches_logseq_Editor_createPage() {
    let sandbox = Sandbox::new("write-create");
    let server = Server::start(&sandbox.graph_root(), "write-create");
    let before = sandbox.revision_of(&server.call(METHODS[0], json!([])).result());

    let created = server.call(CREATE_PAGE, json!(["created"])).result();
    assert_eq!(created["page"]["name"], json!("created"), "{created}");
    assert_eq!(created["page"]["kind"], json!("page"), "{created}");
    assert_eq!(created["page"]["block_count"], json!(0), "{created}");
    assert_eq!(sandbox.revision_of(&created), before + 1, "{created}");

    let read = server.call(METHODS[3], json!(["created"])).result();
    assert_eq!(read["page"]["name"], json!("created"), "{read}");
    assert_eq!(read["revision"], created["revision"], "read-your-writes");
    assert_eq!(sandbox.read("pages/created.md"), b"");
}

// logseq.Editor.appendBlockInPage — out-of-scope. The served append uses the existing page-name
// and text arguments, projects that page, and makes its new block visible to the next default read.
#[test]
fn an_appended_block_is_immediately_readable_matches_logseq_Editor_appendBlockInPage() {
    let sandbox = Sandbox::new("write-append");
    let server = Server::start(&sandbox.graph_root(), "write-append");

    let appended = server
        .call(APPEND_BLOCK, json!(["alpha", "appended by RPC"]))
        .result();
    assert_eq!(appended["page"]["name"], json!("alpha"), "{appended}");
    assert_eq!(appended["page"]["block_count"], json!(4), "{appended}");
    assert_eq!(
        tree_texts(&server, "alpha"),
        [
            "alpha root",
            "alpha child",
            "alpha grandchild",
            "appended by RPC"
        ]
    );
    assert_eq!(
        sandbox.read("pages/alpha.md"),
        b"- alpha root\n\t- alpha child\n\t\t- alpha grandchild\n- appended by RPC\n"
    );
}

// logseq.Editor.insertBlock — out-of-scope. The target object names a child position, the returned
// selector is immediately usable, and the bytes land exactly where the target says (the adapter
// delegates the document edit rather than choosing its own bytes).
#[test]
fn insert_block_positions_the_new_block_and_returns_a_usable_selector_matches_logseq_Editor_insertBlock(
) {
    let sandbox = Sandbox::new("write-insert");
    let server = Server::start(&sandbox.graph_root(), "write-insert");

    let inserted = server
        .call(
            INSERT_BLOCK,
            json!([
                {"firstChildOf": DURABLE_BLOCK},
                "inserted by RPC"
            ]),
        )
        .result();
    assert_selector(&inserted["selector"], "ephemeral");
    assert_eq!(
        tree_texts(&server, "Bravo"),
        [
            "bravo sees [[alpha]]",
            "bravo tagged #project",
            "stable block\nid:: 01234567-89AB-CDEF-0123-456789ABCDEF",
            "inserted by RPC"
        ]
    );
    assert_eq!(
        sandbox.read("pages/Bravo.md"),
        b"- bravo sees [[alpha]]\n- bravo tagged #project\n\t- stable block\n\t  id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n\t\t- inserted by RPC\n"
    );

    let edited = server
        .call(
            UPDATE_BLOCK,
            json!([selector_text(&inserted["selector"]), "edited"]),
        )
        .result();
    assert_eq!(edited["block"]["text"], json!("edited"), "{edited}");
    assert_eq!(
        sandbox.read("pages/Bravo.md"),
        b"- bravo sees [[alpha]]\n- bravo tagged #project\n\t- stable block\n\t  id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n\t\t- edited\n"
    );
}

// logseq.Editor.insertBatch — out-of-scope. A run is one page replacement and answers one selector
// per block in array order; the caller can edit the first selector without another read.
#[test]
fn insert_batch_lands_in_order_and_answers_one_selector_per_block_matches_logseq_Editor_insertBatch(
) {
    let sandbox = Sandbox::new("write-insert-batch");
    let server = Server::start(&sandbox.graph_root(), "write-insert-batch");
    let before = sandbox.revision_of(&server.call(METHODS[0], json!([])).result());

    let batch = server
        .call(
            INSERT_BATCH,
            json!([
                {"before": DURABLE_BLOCK},
                ["first by RPC", "plain by RPC"]
            ]),
        )
        .result();
    let selectors = batch["selectors"]
        .as_array()
        .expect("a batch states selectors");
    assert_eq!(selectors.len(), 2, "{batch}");
    assert_selector(&selectors[0], "ephemeral");
    assert_selector(&selectors[1], "ephemeral");
    assert_eq!(sandbox.revision_of(&batch), before + 1, "{batch}");
    assert_eq!(
        tree_texts(&server, "Bravo"),
        [
            "bravo sees [[alpha]]",
            "bravo tagged #project",
            "first by RPC",
            "plain by RPC",
            "stable block\nid:: 01234567-89AB-CDEF-0123-456789ABCDEF"
        ]
    );

    server
        .call(
            UPDATE_BLOCK,
            json!([selector_text(&selectors[0]), "first edited by RPC"]),
        )
        .result();
    assert_eq!(
        sandbox.read("pages/Bravo.md"),
        b"- bravo sees [[alpha]]\n- bravo tagged #project\n\t- first edited by RPC\n\t- plain by RPC\n\t- stable block\n\t  id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n"
    );
}

// logseq.Editor.prependBlockInPage — out-of-scope. The name is positional, not an anchor, and the
// ordinary tree read sees the new first block without an intervening scan request.
#[test]
fn prepend_block_lands_before_the_first_block_matches_logseq_Editor_prependBlockInPage() {
    let sandbox = Sandbox::new("write-prepend");
    let server = Server::start(&sandbox.graph_root(), "write-prepend");

    let prepended = server
        .call(PREPEND_BLOCK, json!(["alpha", "prepended by RPC"]))
        .result();
    assert_selector(&prepended["selector"], "ephemeral");
    assert_eq!(
        tree_texts(&server, "alpha"),
        [
            "prepended by RPC",
            "alpha root",
            "alpha child",
            "alpha grandchild"
        ]
    );
    assert_eq!(
        sandbox.read("pages/alpha.md"),
        b"- prepended by RPC\n- alpha root\n\t- alpha child\n\t\t- alpha grandchild\n"
    );
}

// logseq.Editor.updateBlock — out-of-scope. The durable id remains usable, the id property line is
// preserved, and the block answer reads the text back from the committed projection.
#[test]
fn update_block_rewrites_the_block_and_preserves_identity_matches_logseq_Editor_updateBlock() {
    let sandbox = Sandbox::new("write-update");
    let server = Server::start(&sandbox.graph_root(), "write-update");

    let updated = server
        .call(UPDATE_BLOCK, json!([DURABLE_BLOCK, "edited by RPC"]))
        .result();
    assert_eq!(updated["block"]["page"], json!("Bravo"), "{updated}");
    assert_eq!(
        updated["block"]["text"],
        json!("edited by RPC\nid:: 01234567-89AB-CDEF-0123-456789ABCDEF"),
        "{updated}"
    );
    assert_eq!(
        updated["block"]["selector"]["id"],
        json!(DURABLE_BLOCK),
        "{updated}"
    );
    assert_eq!(
        sandbox.read("pages/Bravo.md"),
        b"- bravo sees [[alpha]]\n- bravo tagged #project\n\t- edited by RPC\n\t\t\tid:: 01234567-89AB-CDEF-0123-456789ABCDEF\n"
    );
}

// logseq.Editor.updateBlock — out-of-scope. The block in a write answer is the same composed read
// as getBlock, so updating a heading reports its actual marker rather than a bullet fallback.
#[test]
fn update_block_answer_reports_the_heading_marker() {
    let sandbox = Sandbox::new("write-update-heading-marker");
    sandbox.write("pages/headings.md", "## section heading\n");
    let server = Server::start(&sandbox.graph_root(), "write-update-heading-marker");
    let selector = ephemeral_selector(&server, "headings", 0);

    let updated = server
        .call(UPDATE_BLOCK, json!([selector, "edited heading"]))
        .result();
    assert_eq!(
        updated["block"]["text"],
        json!("edited heading"),
        "{updated}"
    );
    assert_eq!(updated["block"]["marker"], json!("##"), "{updated}");
}

// logseq.Editor.removeBlock — out-of-scope. Deleting one durable block removes its whole subtree,
// and the default read already sees the smaller page without a scan request.
#[test]
fn remove_block_deletes_the_subtree_matches_logseq_Editor_removeBlock() {
    let sandbox = Sandbox::new("write-remove");
    let server = Server::start(&sandbox.graph_root(), "write-remove");

    let removed = server.call(REMOVE_BLOCK, json!([DURABLE_BLOCK])).result();
    assert_eq!(removed["page"]["name"], json!("Bravo"), "{removed}");
    assert_eq!(removed["page"]["block_count"], json!(2), "{removed}");
    assert_eq!(
        tree_texts(&server, "Bravo"),
        ["bravo sees [[alpha]]", "bravo tagged #project"]
    );
    assert_eq!(
        sandbox.read("pages/Bravo.md"),
        b"- bravo sees [[alpha]]\n- bravo tagged #project\n"
    );
}

// logseq.Editor.moveBlock — out-of-scope. The same page accepts either the object spelling or the
// direction/anchor spelling; both move a whole subtree and leave one committed page shape.
#[test]
fn move_block_accepts_both_target_spellings_on_one_page_matches_logseq_Editor_moveBlock() {
    let sandbox = Sandbox::new("write-move-spellings");
    let server = Server::start(&sandbox.graph_root(), "write-move-spellings");
    let first = ephemeral_selector(&server, "Bravo", 0);

    let object_moved = server
        .call(MOVE_BLOCK, json!([DURABLE_BLOCK, {"before": first}]))
        .result();
    assert_eq!(
        object_moved["page"]["name"],
        json!("Bravo"),
        "{object_moved}"
    );
    assert_eq!(
        sandbox.read("pages/Bravo.md"),
        b"- stable block\n  id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n- bravo sees [[alpha]]\n- bravo tagged #project\n"
    );

    let second = ephemeral_selector(&server, "Bravo", 1);
    let spelled_moved = server
        .call(MOVE_BLOCK, json!([DURABLE_BLOCK, "after", second]))
        .result();
    assert_eq!(
        spelled_moved["page"]["name"],
        json!("Bravo"),
        "{spelled_moved}"
    );
    assert_eq!(
        tree_texts(&server, "Bravo"),
        [
            "bravo sees [[alpha]]",
            "stable block\nid:: 01234567-89AB-CDEF-0123-456789ABCDEF",
            "bravo tagged #project"
        ]
    );
    assert_eq!(
        sandbox.read("pages/Bravo.md"),
        b"- bravo sees [[alpha]]\n- stable block\n  id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n- bravo tagged #project\n"
    );
}

// logseq.Editor.upsertBlockProperty — out-of-scope. A new property line lands after the block
// content, while a durable id can still address the block and the ordinary read sees the property.
#[test]
fn upsert_block_property_writes_the_property_line_matches_logseq_Editor_upsertBlockProperty() {
    let sandbox = Sandbox::new("write-upsert-property");
    let server = Server::start(&sandbox.graph_root(), "write-upsert-property");

    let upserted = server
        .call(
            UPSERT_BLOCK_PROPERTY,
            json!([DURABLE_BLOCK, "status", "done by RPC"]),
        )
        .result();
    assert_eq!(upserted["block"]["page"], json!("Bravo"), "{upserted}");
    assert_eq!(
        upserted["block"]["text"],
        json!("stable block\nid:: 01234567-89AB-CDEF-0123-456789ABCDEF\nstatus:: done by RPC"),
        "{upserted}"
    );
    assert_eq!(
        sandbox.read("pages/Bravo.md"),
        b"- bravo sees [[alpha]]\n- bravo tagged #project\n\t- stable block\n\t  id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n\t\t\tstatus:: done by RPC\n"
    );
}

// logseq.Editor.removeBlockProperty — out-of-scope. A missing key is a structured refusal that
// writes nothing; removing the `id::` line changes the block's committed address from durable to
// ephemeral, and the answer names that new address.
#[test]
fn remove_block_property_refuses_a_missing_key_then_answers_the_new_identity() {
    let sandbox = Sandbox::new("write-remove-property");
    let server = Server::start(&sandbox.graph_root(), "write-remove-property");
    let before = sandbox.read("pages/Bravo.md");

    let error = server
        .call(REMOVE_BLOCK_PROPERTY, json!([DURABLE_BLOCK, "missing"]))
        .expect_code(400, -32602);
    assert_eq!(error["data"]["kind"], json!("write-edit"), "{error}");
    assert_eq!(
        error["data"]["edit_kind"],
        json!("property-not-found"),
        "{error}"
    );
    assert_eq!(error["data"]["write_state"], json!("unwritten"), "{error}");
    assert_eq!(sandbox.read("pages/Bravo.md"), before);

    let removed_property = server
        .call(REMOVE_BLOCK_PROPERTY, json!([DURABLE_BLOCK, "id"]))
        .result();
    assert_eq!(
        removed_property["block"]["text"],
        json!("stable block"),
        "{removed_property}"
    );
    assert_selector(&removed_property["block"]["selector"], "ephemeral");
    assert_eq!(
        sandbox.read("pages/Bravo.md"),
        b"- bravo sees [[alpha]]\n- bravo tagged #project\n\t- stable block\n"
    );
}

// logseq.Editor.setBlockCollapsed — out-of-scope. The fold is an ordinary `collapsed::` property
// line, and the write answer's durable selector remains usable after the projection is replaced.
#[test]
fn set_block_collapsed_writes_the_fold_property_matches_logseq_Editor_setBlockCollapsed() {
    let sandbox = Sandbox::new("write-collapse");
    let server = Server::start(&sandbox.graph_root(), "write-collapse");

    let folded = server
        .call(SET_BLOCK_COLLAPSED, json!([DURABLE_BLOCK, true]))
        .result();
    assert_eq!(folded["block"]["page"], json!("Bravo"), "{folded}");
    assert_eq!(
        folded["block"]["text"],
        json!("stable block\nid:: 01234567-89AB-CDEF-0123-456789ABCDEF\ncollapsed:: true"),
        "{folded}"
    );
    assert_eq!(
        sandbox.read("pages/Bravo.md"),
        b"- bravo sees [[alpha]]\n- bravo tagged #project\n\t- stable block\n\t  id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n\t\t\tcollapsed:: true\n"
    );

    let unfolded = server
        .call(SET_BLOCK_COLLAPSED, json!([DURABLE_BLOCK, false]))
        .result();
    assert_eq!(
        unfolded["block"]["text"],
        json!("stable block\nid:: 01234567-89AB-CDEF-0123-456789ABCDEF\ncollapsed:: false"),
        "{unfolded}"
    );
    assert_eq!(
        sandbox.read("pages/Bravo.md"),
        b"- bravo sees [[alpha]]\n- bravo tagged #project\n\t- stable block\n\t  id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n\t\t\tcollapsed:: false\n"
    );
}

// logseq.Graph.commitPage — out-of-scope. A page-file fingerprint, not the graph revision, guards
// the whole batch. A stale call carries the current snapshot and leaves both source and projection
// untouched; repeating the unchanged request does not rebase it (ADR-0014).
#[test]
fn a_commit_checks_its_base_version_and_a_stale_commit_changes_nothing() {
    let sandbox = Sandbox::new("write-commit-version");
    let server = Server::start(&sandbox.graph_root(), "write-commit-version");
    let base_version = page_version(&server, "alpha");

    let committed = commit(
        &server,
        "alpha",
        &base_version,
        json!([{"op": "edit_text", "block": [0], "text": "edited by RPC"}]),
    )
    .result();
    assert_eq!(committed["applied"], json!(1), "{committed}");
    assert_eq!(
        committed["snapshot"]["version"],
        json!(page_version(&server, "alpha")),
        "{committed}"
    );
    assert_eq!(tree_texts(&server, "alpha")[0], "edited by RPC");
    let committed_version = page_version(&server, "alpha");
    let committed_bytes = sandbox.read("pages/alpha.md");
    let committed_revision = sandbox.revision_of(&committed);

    let error = commit(
        &server,
        "alpha",
        &base_version,
        json!([{"op": "edit_text", "block": [0], "text": "stale edit"}]),
    )
    .expect_code(409, -32009);
    assert_eq!(error["data"]["kind"], json!("stale-version"), "{error}");
    assert_eq!(error["data"]["page"], json!("alpha"), "{error}");
    assert_eq!(error["data"]["baseVersion"], json!(base_version), "{error}");
    assert_eq!(
        error["data"]["current"]["version"],
        json!(committed_version),
        "the client can recompute its positions: {error}"
    );
    assert_eq!(
        error["data"]["current"]["blocks"][0]["text"],
        json!("edited by RPC"),
        "{error}"
    );
    assert_eq!(sandbox.read("pages/alpha.md"), committed_bytes);
    assert_eq!(
        sandbox.revision_of(&server.call(METHODS[3], json!(["alpha"])).result()),
        committed_revision
    );
}

// logseq.Editor.createJournal — out-of-scope. A day, not a title, names the journal; the default
// formats write an empty journals/ file and the ordinary page read sees it without a scan request.
#[test]
fn a_created_journal_is_named_by_its_date_and_is_immediately_readable() {
    let sandbox = Sandbox::new("write-create-journal");
    let server = Server::start(&sandbox.graph_root(), "write-create-journal");

    let created = server.call(CREATE_JOURNAL, json!(["2026-11-12"])).result();
    assert_eq!(
        created["page"]["name"],
        json!("Nov 12th, 2026"),
        "{created}"
    );
    assert_eq!(created["page"]["kind"], json!("journal"), "{created}");
    assert_eq!(
        created["page"]["journal_date"],
        json!("2026-11-12"),
        "{created}"
    );
    assert_eq!(created["page"]["block_count"], json!(0), "{created}");
    assert_eq!(sandbox.read("journals/2026_11_12.md"), b"");

    let read = server.call(METHODS[3], json!(["Nov 12th, 2026"])).result();
    assert_eq!(read["revision"], created["revision"], "{read}");
    assert_eq!(read["page"]["kind"], json!("journal"), "{read}");
    assert_eq!(read["page"]["block_count"], json!(0), "{read}");
}

// logseq.Editor.deletePage — out-of-scope. The graph never unlinks a page: the source file moves
// byte-for-byte to Logseq's recycle directory, and the ordinary page read then reports a miss.
#[test]
fn a_deleted_page_is_recycled_byte_for_byte_and_retains_incoming_claims() {
    let sandbox = Sandbox::new("write-delete-page");
    let server = Server::start(&sandbox.graph_root(), "write-delete-page");
    let before = sandbox.read("pages/alpha.md");

    let deleted = server.call(DELETE_PAGE, json!(["alpha"])).result();
    assert_eq!(deleted["page"]["name"], json!("alpha"), "{deleted}");
    assert_eq!(deleted["page"]["block_count"], json!(3), "{deleted}");
    assert!(!sandbox.graph_root().join("pages/alpha.md").exists());
    assert_eq!(sandbox.read("logseq/.recycle/pages_alpha.md"), before);
    let page = server.call(METHODS[3], json!(["alpha"])).result();
    assert_eq!(page["page"]["name"], "alpha");
    assert_eq!(page["page"]["has_file"], false);
    assert_eq!(page["properties"], json!([]));
}

// logseq.Editor.appendBlockInPage — out-of-scope. A write has no freshness policy: the page-file
// guard is independent of read scans, and no write option may request a graph scan (ADR-0014).
#[test]
fn served_writes_do_not_accept_a_freshness_policy() {
    let sandbox = Sandbox::new("write-no-freshness");
    let server = Server::start(&sandbox.graph_root(), "write-no-freshness");
    let version = page_version(&server, "alpha");
    let alpha_before = sandbox.read("pages/alpha.md");

    for response in [
        server.call(CREATE_PAGE, json!(["not-created", {"freshness": "verify"}])),
        server.call(
            APPEND_BLOCK,
            json!(["alpha", "not-appended", {"freshness": "check"}]),
        ),
        server.call(
            COMMIT_PAGE,
            json!(["alpha", version, [], {"freshness": "last"}]),
        ),
    ] {
        let error = response.expect_code(400, -32602);
        assert_eq!(error["data"]["parameter"], json!("freshness"), "{error}");
        assert_eq!(error["data"]["allowed"], json!([]), "{error}");
    }

    assert!(!sandbox.graph_root().join("pages/not-created.md").exists());
    assert_eq!(sandbox.read("pages/alpha.md"), alpha_before);
}

// logseq.Graph.commitPage — out-of-scope. ADR-0008's startup refusal covers writes too: an open
// socket does not authorize a mutation to race graph opening.
#[test]
fn a_write_is_refused_while_the_startup_scan_is_running() {
    let sandbox = Sandbox::new("write-startup");
    sandbox.write_filler_pages(800);
    let expected = sandbox.page_file_count() as u64;
    let server = Server::spawn(&sandbox.graph_root(), "write-startup");

    let deadline = Instant::now() + TIMEOUT;
    loop {
        assert!(
            Instant::now() < deadline,
            "the startup scan did not finish within {TIMEOUT:?}"
        );
        let progress = server.call(METHODS[0], json!([])).result();
        match progress["scanning"].as_bool() {
            Some(true) if progress["total"] == json!(expected) => {
                let error = server
                    .call(APPEND_BLOCK, json!(["alpha", "must not land"]))
                    .expect_code(503, -32003);
                assert_eq!(error["data"]["kind"], json!("scanning"), "{error}");
                assert_eq!(error["data"]["scanning"], json!(true), "{error}");
                assert_eq!(error["data"]["total"], json!(expected), "{error}");
                assert!(error["data"]["scanned"].as_u64().is_some(), "{error}");
                break;
            }
            Some(true) => std::thread::sleep(Duration::from_millis(2)),
            Some(false) => panic!("no write was attempted while the startup scan ran"),
            None => panic!("no scanning field in {progress}"),
        }
    }

    server.wait_until_ready("write-startup");
    assert!(!tree_texts(&server, "alpha").contains(&"must not land".to_owned()));
}

// logseq.Editor.moveBlock — out-of-scope. Cross-page moves record coordinates, not ephemeral ids.
#[test]
fn a_cross_page_move_of_an_ephemeral_block_answers_page_metadata() {
    let sandbox = Sandbox::new("write-ephemeral-move");
    let server = Server::start(&sandbox.graph_root(), "write-ephemeral-move");
    let alpha_before = sandbox.read("pages/alpha.md");
    let bravo_before = sandbox.read("pages/Bravo.md");
    let ephemeral = ephemeral_selector(&server, "alpha", 0);

    let moved = server
        .call(MOVE_BLOCK, json!([ephemeral, "after", DURABLE_BLOCK]))
        .result();
    assert_eq!(moved["page"]["name"], json!("Bravo"), "{moved}");
    assert_ne!(sandbox.read("pages/alpha.md"), alpha_before);
    assert_ne!(sandbox.read("pages/Bravo.md"), bravo_before);
}

// logseq.Editor.updateBlock / logseq.Editor.moveBlock — out-of-scope. Missing durable ids are
// structured errors on both routes and leave the page byte-identical; an anchor miss names its
// selector kind so a caller can tell a durable miss from a stale ephemeral handle.
#[test]
fn missing_durable_ids_and_anchors_are_refused_without_writing() {
    let sandbox = Sandbox::new("write-move-missing-anchor");
    let server = Server::start(&sandbox.graph_root(), "write-move-missing-anchor");
    let before = sandbox.read("pages/Bravo.md");

    let missing = "00000000-0000-0000-0000-000000000000";
    let update_error = server
        .call(UPDATE_BLOCK, json!([missing, "must not land"]))
        .expect_code(404, -32004);
    assert_eq!(
        update_error["data"]["kind"],
        json!("block-not-found"),
        "{update_error}"
    );
    assert_eq!(update_error["data"]["id"], json!(missing), "{update_error}");
    assert_eq!(sandbox.read("pages/Bravo.md"), before);

    let move_error = server
        .call(MOVE_BLOCK, json!([DURABLE_BLOCK, "after", missing]))
        .expect_code(404, -32004);
    assert_eq!(
        move_error["data"]["kind"],
        json!("block-not-found"),
        "{move_error}"
    );
    assert_eq!(move_error["data"]["id"], json!(missing), "{move_error}");
    assert_eq!(
        move_error["data"]["selector_kind"],
        json!("durable"),
        "{move_error}"
    );
    assert_eq!(sandbox.read("pages/Bravo.md"), before);
}

// logseq.Editor.removeBlock — out-of-scope. A durable id has to name one block. A second fixture
// page stating the same id makes the ambiguity observable through the adapter, and both files
// remain byte-identical (ADR-0014).
#[test]
fn a_duplicate_durable_id_across_pages_is_refused_without_writing() {
    let sandbox = Sandbox::new("write-duplicate-id");
    sandbox.write(
        "pages/duplicate.md",
        "- duplicate block\n\tid:: 01234567-89ab-cdef-0123-456789abcdef\n",
    );
    let server = Server::start(&sandbox.graph_root(), "write-duplicate-id");
    let bravo_before = sandbox.read("pages/Bravo.md");
    let duplicate_before = sandbox.read("pages/duplicate.md");

    let error = server
        .call(UPDATE_BLOCK, json!([DURABLE_BLOCK, "must not land"]))
        .expect_code(409, -32009);
    assert_eq!(
        error["data"]["kind"],
        json!("ambiguous-block-id"),
        "{error}"
    );
    assert_eq!(error["data"]["id"], json!(DURABLE_BLOCK), "{error}");
    assert_eq!(
        error["data"]["pages"],
        json!(["Bravo", "duplicate"]),
        "{error}"
    );
    assert_eq!(sandbox.read("pages/Bravo.md"), bravo_before);
    assert_eq!(sandbox.read("pages/duplicate.md"), duplicate_before);
}

// logseq.Graph.commitPage — out-of-scope. A refused operation rejects the complete batch before
// source bytes change; the error identifies the exact operation rather than reporting a prefix as
// committed (ADR-0014).
#[test]
fn a_refused_batch_operation_writes_no_prefix_and_names_the_operation() {
    let sandbox = Sandbox::new("write-batch-refusal");
    let server = Server::start(&sandbox.graph_root(), "write-batch-refusal");
    let version = page_version(&server, "alpha");
    let before = sandbox.read("pages/alpha.md");

    let error = commit(
        &server,
        "alpha",
        &version,
        json!([
            {"op": "edit_text", "block": [0], "text": "uncommitted prefix"},
            {"op": "indent_block", "block": [0]}
        ]),
    )
    .expect_code(400, -32602);
    assert_eq!(error["data"]["kind"], json!("operation"), "{error}");
    assert_eq!(error["data"]["page"], json!("alpha"), "{error}");
    assert_eq!(error["data"]["index"], json!(1), "{error}");
    assert_eq!(error["data"]["operation"], json!("IndentBlock"), "{error}");
    assert_eq!(sandbox.read("pages/alpha.md"), before);
    assert_eq!(tree_texts(&server, "alpha")[0], "alpha root");
}

// A partial move, re-read-after-write or index-after-write failure requires fault injection inside
// the write pipeline. No stable external-process simulation exists, so this file deliberately adds
// no test that would confuse a pre-write error with an error whose bytes already landed.
#[allow(dead_code)]
fn partial_write_failures_need_an_internal_fault_injection_point() {}
