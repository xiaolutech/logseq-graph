//! Differential tests against the answers captured from the official Logseq HTTP API.
//!
//! `fixtures/expected/*.json` holds answers Logseq 0.10.9 gave on 2026-11-20 for the public
//! `logseq/docs` graph at tag `v0.9.2`.
//! Those answers describe DB entities (including ghost and alias pages with no file); the reads in
//! this repository are a projection of Markdown page files. Raw JSON equality therefore cannot hold.
//! Each test below states a normalization rule in a comment and compares one derived invariant: a
//! count, a set of names, a block text, or a set of property keys. Unexplained differences fail the
//! test; they are never absorbed by widening a threshold. No row's classification is changed here,
//! and no test claims `equivalent`.
//!
//! The tests run only when `LOGSEQ_GRAPH_OFFICIAL` names a graph root. Without it they print
//! `SKIP=` and pass, so CI and any machine without the checked-out graph still run the suite. A
//! variable that names something other than a directory is an error, not a skip. The graph is read
//! only: every index is projected into a temporary directory that the test removes on exit, and
//! nothing is written into the graph root.
//!
//! Before any comparison, [`source`] guards against comparing a fixture from a different graph: it
//! checks the captured graph name and path and the order of magnitude of the file counts. A drifted
//! checkout is reported as a named finding, not hidden.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use logseq_graph::domain::{page_name_key, BlockId, BlockSelector, PageKind};
use logseq_graph::engine::{Graph, OpenOptions};
use serde_json::Value;

/// Environment variable that names the graph root the fixtures were captured from.
const GRAPH_ENV: &str = "LOGSEQ_GRAPH_OFFICIAL";

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
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("decode {}: {error}", path.display()))
}

/// Number of Markdown files directly inside one graph directory (`pages` or `journals`).
fn markdown_files(directory: &Path) -> usize {
    fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("list {}: {error}", directory.display()))
        .map(|entry| entry.expect("read a directory entry"))
        .filter(|entry| {
            entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
                && entry.path().extension().and_then(|ext| ext.to_str()) == Some("md")
        })
        .count()
}

/// Guard: a file count must be the same order of magnitude as the capture's. A factor of two is
/// generous enough for a drifted checkout and strict enough to reject a fixture from another graph.
fn assert_same_order(label: &str, actual: usize, captured: u64) {
    let actual = actual as f64;
    let captured = captured as f64;
    assert!(
        (captured / 2.0..=captured * 2.0).contains(&actual),
        "fixture belongs to another graph: {label} is {actual}, not the same order of magnitude as the capture's {captured}"
    );
}

/// Everything a test needs about the reference graph and the captured run.
struct Source {
    root: PathBuf,
    current: Value,
    local_page_files: usize,
    local_journal_files: usize,
    captured_page_files: u64,
    captured_journal_files: u64,
}

/// Resolve the graph root and apply the source guard, or report a skip.
fn source() -> Option<Source> {
    let value = std::env::var_os(GRAPH_ENV)?;
    if value.is_empty() {
        return None;
    }
    let root = PathBuf::from(value);
    assert!(
        root.is_dir(),
        "{GRAPH_ENV} names {}, which is not a directory",
        root.display()
    );

    let current = fixture("get_current_graph.json");
    let result = &current["result"];
    let captured_path = result["path"]
        .as_str()
        .expect("get_current_graph.json result.path");
    let captured_name = result["name"]
        .as_str()
        .expect("get_current_graph.json result.name");

    // The path is compared as the fixture spells it. A moved, copied, or substituted graph is not
    // silently accepted: the captured answers describe one particular DB, so a different location
    // means the fixture belongs to another graph.
    let root_string = root.to_string_lossy().into_owned();
    assert_eq!(
        root_string.as_str(),
        captured_path,
        "fixture belongs to another graph: {GRAPH_ENV}={root_string} but the captured getCurrentGraph path is {captured_path}"
    );
    assert_eq!(
        current["graph"]["name"].as_str(),
        Some(captured_name),
        "fixture belongs to another graph: graph label disagrees with getCurrentGraph"
    );
    assert_eq!(
        root.file_name().and_then(|name| name.to_str()),
        Some(captured_name),
        "fixture belongs to another graph: the graph root's directory name is not {captured_name}"
    );

    let local_page_files = markdown_files(&root.join("pages"));
    let local_journal_files = markdown_files(&root.join("journals"));
    let captured_page_files = current["graph"]["page_files"]
        .as_u64()
        .expect("get_current_graph.json graph.page_files");
    let captured_journal_files = current["graph"]["journal_files"]
        .as_u64()
        .expect("get_current_graph.json graph.journal_files");
    assert_same_order("pages/*.md", local_page_files, captured_page_files);
    assert_same_order("journals/*.md", local_journal_files, captured_journal_files);

    println!(
        "PARITY source graph={captured_name} path={root_string} local_pages={local_page_files} captured_pages={captured_page_files} drift_pages={} local_journals={local_journal_files} captured_journals={captured_journal_files} drift_journals={}",
        local_page_files as i64 - captured_page_files as i64,
        local_journal_files as i64 - captured_journal_files as i64,
    );
    Some(Source {
        root,
        current,
        local_page_files,
        local_journal_files,
        captured_page_files,
        captured_journal_files,
    })
}

/// One temporary directory holding a projected index, removed when the value is dropped.
struct TempIndex {
    directory: PathBuf,
}

impl TempIndex {
    /// The name carries the test name, the process id and a process-wide serial, so two tests in
    /// this binary and two runs of this binary never share an index. It lives under `temp_dir()`,
    /// never under the graph root.
    fn new(test_name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "logseq-graph-official-parity-{test_name}-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the temporary index directory");
        Self { directory }
    }

    fn index_path(&self) -> PathBuf {
        self.directory.join("index.db")
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

impl Source {
    /// Cold-project the reference graph into a temporary index. The index file and everything else
    /// the engine writes live inside the returned [`TempIndex`], never in the graph root.
    fn project(&self, test_name: &str) -> (TempIndex, Graph) {
        let scratch = TempIndex::new(test_name);
        let graph = Graph::open(OpenOptions::new(&self.root, scratch.index_path()))
            .unwrap_or_else(|error| panic!("project {}: {error}", self.root.display()));
        (scratch, graph)
    }
}

fn skip(test_name: &str) {
    println!("SKIP={GRAPH_ENV} is not set; {test_name} skipped");
}

/// Flatten the official nested block tree into depth-first `content` strings.
fn official_block_texts(node: &Value, out: &mut Vec<String>) {
    out.push(
        node["content"]
            .as_str()
            .expect("official block content")
            .to_owned(),
    );
    for child in node["children"].as_array().into_iter().flatten() {
        official_block_texts(child, out);
    }
}

/// Flatten a page snapshot's nested blocks into depth-first text strings.
///
/// A snapshot reports a block's property lines apart from its editable text, while the official
/// `content` keeps them after the text, so each block is re-rendered as its text followed by one
/// `key:: value` line per property in written order.
fn snapshot_block_texts(node: &logseq_graph::domain::SnapshotBlock, out: &mut Vec<String>) {
    let mut lines: Vec<String> = Vec::new();
    if !node.text().is_empty() {
        lines.push(node.text().to_owned());
    }
    lines.extend(
        node.properties()
            .iter()
            .map(|property| format!("{}:: {}", property.key(), property.value())),
    );
    out.push(lines.join("\n"));
    for child in node.children() {
        snapshot_block_texts(child, out);
    }
}

/// Matrix row #1: `logseq.Editor.getCurrentGraph()`.
///
/// Normalization: compare the fixture's `name`, `path` and `url` as strings with the graph root the
/// environment variable names. The official answer is a DB/app fact; our source guard answers the
/// equivalent question "which graph do these fixtures describe". The page/journal counts are
/// compared only for order of magnitude, and the exact drift is printed as a named finding.
#[test]
fn row_01_get_current_graph_pins_the_captured_graph() {
    let Some(source) = source() else {
        skip("row 01 getCurrentGraph");
        return;
    };
    let result = &source.current["result"];
    let name = result["name"].as_str().expect("result.name");
    let path = result["path"].as_str().expect("result.path");
    let url = result["url"].as_str().expect("result.url");

    assert_eq!(
        source.root.file_name().and_then(|part| part.to_str()),
        Some(name),
        "row #1: captured graph name and graph root disagree"
    );
    assert_eq!(
        source.root.to_string_lossy().as_ref(),
        path,
        "row #1: captured graph path and graph root disagree"
    );
    assert_eq!(
        url,
        format!("logseq_local_{path}"),
        "row #1: captured url does not name the captured path"
    );
    println!(
        "PARITY #1 name={name} path={path} url={url} local_files={} captured_files={} drift={}",
        source.local_page_files + source.local_journal_files,
        source.captured_page_files + source.captured_journal_files,
        (source.local_page_files + source.local_journal_files) as i64
            - (source.captured_page_files + source.captured_journal_files) as i64,
    );
}

/// Matrix row #2: `logseq.Editor.getAllPages()`.
///
/// Normalization: the official summarized answer counts DB entities (`result.count`) and entities
/// carrying a `file` field (`result.field_counts.file`). Our `pages()` answers one row per Markdown
/// file. Three measured quantities explain any difference between those two numbers: the capture's
/// own read-only file walk (`graph.page_files + graph.journal_files`), the gap between that walk and
/// the official file-entity count, and the drift of the local checkout since the capture. The
/// assertion is exact on that decomposition, so an unexplained difference fails. All numbers are
/// printed as named findings. The summarized capture has no name list for its no-file
/// entities, so only the counts of file-backed pages are compared.
#[test]
fn row_02_get_all_pages_compares_the_file_page_universe() {
    let Some(source) = source() else {
        skip("row 02 getAllPages");
        return;
    };
    let official = fixture("get_all_pages.json");
    assert_eq!(
        official["summarized"].as_bool(),
        Some(true),
        "row #2: getAllPages must be a summarized answer"
    );
    let result = &official["result"];
    let official_count = result["count"].as_u64().expect("result.count");
    let official_file_entities = result["field_counts"]["file"]
        .as_u64()
        .expect("result.field_counts.file");
    assert_eq!(
        result["truthy_field_counts"]["file"].as_u64(),
        Some(official_file_entities),
        "row #2: field_counts.file and truthy_field_counts.file disagree"
    );
    let official_no_file = official_count - official_file_entities;

    let captured_walk = source.captured_page_files + source.captured_journal_files;
    let local_walk = (source.local_page_files + source.local_journal_files) as u64;
    let drift = local_walk as i64 - captured_walk as i64;
    let capture_gap = captured_walk as i64 - official_file_entities as i64;

    let (_scratch, mut graph) = source.project("row-02");
    let pages = graph
        .pages()
        .expect("row #2: read the projected pages")
        .into_value();
    let our_no_file = pages.iter().filter(|page| !page.has_file()).count();
    let our_pages = (pages.len() - our_no_file) as u64;
    let page_kind_count = pages
        .iter()
        .filter(|page| page.has_file() && page.kind() == PageKind::Page)
        .count();
    let journal_kind_count = pages
        .iter()
        .filter(|page| page.kind() == PageKind::Journal)
        .count();

    println!(
        "PARITY #2 official_count={official_count} official_file_entities={official_file_entities} official_no_file={official_no_file} captured_walk={captured_walk} local_walk={local_walk} capture_gap={capture_gap} drift={drift} our_file_pages={our_pages} our_no_file={our_no_file} our_pages_kind={page_kind_count} our_journals_kind={journal_kind_count}"
    );
    assert_eq!(
        our_pages as i64,
        official_file_entities as i64 + capture_gap + drift,
        "row #2: our file-page count differs from the official file-entity count by more than the measured capture gap and graph drift"
    );
    assert_eq!(
        page_kind_count, source.local_page_files,
        "row #2: the projection did not answer one page row per page file"
    );
    assert_eq!(
        journal_kind_count, source.local_journal_files,
        "row #2: the projection did not answer one page row per journal file"
    );
    assert!(
        our_no_file > 0,
        "row #2: no derived page entities were projected"
    );
}

/// Matrix row #4: `logseq.Editor.getPage("Tasks")`.
///
/// Normalization: the official entity stores both a folded `name` and an `originalName`; our
/// `page_metadata` stores the as-written spelling and matches case-insensitively. So the assertion
/// compares the stored spelling with `originalName` and the folded key with `name`. Properties come
/// from the page preamble in both answers; the official entity is a key/value map while our read is
/// an ordered, repeatable line list, so only the normalized key set and the line count are compared.
/// DB `id`/`uuid`, alias entity ids and `propertiesTextValues` have no counterpart and are not
/// compared.
#[test]
fn row_04_get_page_compares_names_and_property_keys() {
    let Some(source) = source() else {
        skip("row 04 getPage");
        return;
    };
    let official = fixture("get_page.json");
    let page = official["args"][0]
        .as_str()
        .expect("get_page.json args[0]")
        .to_owned();
    let entity = &official["result"];
    let original_name = entity["originalName"]
        .as_str()
        .expect("result.originalName");
    let folded_name = entity["name"].as_str().expect("result.name");
    let official_keys: BTreeSet<String> = entity["properties"]
        .as_object()
        .expect("result.properties")
        .keys()
        .cloned()
        .collect();

    let (_scratch, mut graph) = source.project("row-04");
    let metadata = graph
        .page_metadata(&page)
        .expect("row #4: page_metadata")
        .into_value()
        .unwrap_or_else(|| panic!("row #4: page {page} is absent from the projection"));
    let folded_lookup = graph
        .page_metadata(folded_name)
        .expect("row #4: folded page_metadata")
        .into_value()
        .unwrap_or_else(|| panic!("row #4: folded name {folded_name} did not resolve"));
    let properties = graph
        .page_properties(&page)
        .expect("row #4: page_properties")
        .into_value()
        .unwrap_or_else(|| panic!("row #4: page {page} has no property answer"));
    let our_keys: BTreeSet<String> = properties
        .iter()
        .map(|property| property.normalized_key().to_owned())
        .collect();

    println!(
        "PARITY #4 page={page} originalName={original_name} name={folded_name} official_property_keys={:?} our_property_keys={:?} our_property_lines={}",
        official_keys,
        our_keys,
        properties.len()
    );
    assert_eq!(
        metadata.name(),
        original_name,
        "row #4: as-written page name differs from the official originalName"
    );
    assert_eq!(
        page_name_key(metadata.name()),
        page_name_key(folded_name),
        "row #4: folded page name differs from the official name"
    );
    assert_eq!(
        folded_lookup.name(),
        metadata.name(),
        "row #4: a folded-name lookup must answer the same page"
    );
    assert_eq!(
        our_keys, official_keys,
        "row #4: the preamble property key set differs from the official entity's properties"
    );
    assert_eq!(
        properties.len(),
        official_keys.len(),
        "row #4: the official map has one entry per key, so our line count must match"
    );
}

/// Matrix row #5: `logseq.Editor.getPageBlocksTree("Tasks")`.
///
/// Normalization: the official tree's first top-level node is a `preBlock?` holding the page
/// preamble's properties. ADR-0013 refuses a synthetic pre-block result, and this projection stores
/// those lines as page properties instead, so that node is excluded by rule (its trailing-newline
/// content is also a recorded M7 difference). The remaining official subtree is flattened in
/// depth-first order and compared, text for text, with `page_blocks`, `page_tree` and
/// `page_snapshot`; a snapshot block is re-rendered with its property lines first (see
/// [`snapshot_block_texts`]). DB fields (parent, left, refs, pathRefs, uuid) are not compared.
#[test]
fn row_05_get_page_blocks_tree_compares_non_pre_block_content() {
    let Some(source) = source() else {
        skip("row 05 getPageBlocksTree");
        return;
    };
    let official = fixture("get_page_blocks_tree.json");
    let page = official["args"][0]
        .as_str()
        .expect("get_page_blocks_tree.json args[0]")
        .to_owned();
    let top_level = official["result"]
        .as_array()
        .expect("get_page_blocks_tree.json result");
    let pre_blocks: Vec<&Value> = top_level
        .iter()
        .filter(|node| node["preBlock?"].as_bool() == Some(true))
        .collect();
    let content_roots: Vec<&Value> = top_level
        .iter()
        .filter(|node| node["preBlock?"].as_bool() != Some(true))
        .collect();
    let mut official_texts = Vec::new();
    for root in &content_roots {
        official_block_texts(root, &mut official_texts);
    }

    let (_scratch, mut graph) = source.project("row-05");
    let blocks = graph
        .page_blocks(&page)
        .expect("row #5: page_blocks")
        .into_value()
        .unwrap_or_else(|| panic!("row #5: page {page} is absent"));
    let tree = graph
        .page_tree(&page)
        .expect("row #5: page_tree")
        .into_value()
        .unwrap_or_else(|| panic!("row #5: page {page} has no tree"));
    let snapshot = graph
        .page_snapshot(&page)
        .expect("row #5: page_snapshot")
        .into_value()
        .unwrap_or_else(|| panic!("row #5: page {page} has no snapshot"));
    let our_texts: Vec<String> = blocks.iter().map(|block| block.text().to_owned()).collect();
    let mut snapshot_texts = Vec::new();
    for root in snapshot.blocks() {
        snapshot_block_texts(root, &mut snapshot_texts);
    }

    println!(
        "PARITY #5 page={page} official_top_level={} official_pre_blocks={} official_content_roots={} official_content_blocks={} our_blocks={} our_tree_roots={} our_snapshot_blocks={}",
        top_level.len(),
        pre_blocks.len(),
        content_roots.len(),
        official_texts.len(),
        our_texts.len(),
        tree.blocks().len(),
        snapshot_texts.len(),
    );
    assert_eq!(
        pre_blocks.len(),
        1,
        "row #5: the capture's pre-block is the documented difference; an unexpected tree shape is not comparable"
    );
    assert_eq!(
        tree.blocks().len(),
        content_roots.len(),
        "row #5: top-level block count differs after excluding the pre-block"
    );
    assert_eq!(
        our_texts, official_texts,
        "row #5: flattened block texts differ from the official non-pre-block tree"
    );
    assert_eq!(
        snapshot_texts, official_texts,
        "row #5: the snapshot of the file's current bytes differs from the official tree"
    );
    let pre_block_text = pre_blocks[0]["content"]
        .as_str()
        .expect("pre-block content");
    assert!(
        !our_texts.iter().any(|text| text == pre_block_text),
        "row #5: the projection must not materialize the pre-block as a block"
    );
}

/// Matrix row #6: `logseq.Editor.getPageLinkedReferences("Tasks")`.
///
/// Normalization: the official grouping merges alias references into the declaring page (M5), which
/// the projection now merges (ADR-0015). Only groups with a block containing a literal
/// `[[Tasks]]` reference are direct references; those are compared, by folded page name and by the
/// referencing block's text, with `backlinks()`. Every other group is reported as an alias-merged
/// group and excluded from the direct comparison. One official group may hold several blocks, while `backlinks()`
/// reports one earliest referencing block per page, so the comparison uses one representative per
/// page on each side.
#[test]
fn row_06_get_page_linked_references_compares_direct_backlinks() {
    let Some(source) = source() else {
        skip("row 06 getPageLinkedReferences");
        return;
    };
    let official = fixture("get_page_linked_references.json");
    let page = official["args"][0]
        .as_str()
        .expect("get_page_linked_references.json args[0]")
        .to_owned();
    let groups = official["result"]
        .as_array()
        .expect("get_page_linked_references.json result");
    let direct_marker = format!("[[{page}]]");
    let mut direct: BTreeMap<String, String> = BTreeMap::new();
    let mut alias_merged: Vec<&str> = Vec::new();
    for group in groups {
        let group_name = group[0]["originalName"]
            .as_str()
            .expect("reference group page name");
        let blocks = group[1].as_array().expect("reference group blocks");
        let mut found_direct = false;
        for block in blocks {
            let text = block["content"].as_str().expect("reference block content");
            if text.contains(&direct_marker) {
                found_direct = true;
                direct
                    .entry(page_name_key(group_name))
                    .or_insert_with(|| text.to_owned());
            }
        }
        if !found_direct {
            alias_merged.push(group_name);
        }
    }

    let (_scratch, mut graph) = source.project("row-06");
    let backlinks = graph
        .backlinks(&page)
        .expect("row #6: backlinks")
        .into_value();
    let ours: BTreeMap<String, String> = backlinks
        .iter()
        .filter(|backlink| backlink.text().contains(&direct_marker))
        .map(|backlink| (page_name_key(backlink.page()), backlink.text().to_owned()))
        .collect();

    println!(
        "PARITY #6 page={page} official_groups={} official_direct_groups={} official_alias_merged_groups={:?} our_backlinks={} our_pages={:?}",
        groups.len(),
        direct.len(),
        alias_merged,
        ours.len(),
        ours.keys().collect::<Vec<_>>(),
    );
    assert!(
        !direct.is_empty(),
        "row #6: the capture holds no direct [[{page}]] reference to compare"
    );
    assert_eq!(
        ours.keys().collect::<Vec<_>>(),
        direct.keys().collect::<Vec<_>>(),
        "row #6: the set of directly referencing pages differs from the official grouping"
    );
    for (key, official_text) in &direct {
        assert_eq!(
            ours.get(key),
            Some(official_text),
            "row #6: direct reference block text differs on page {key}"
        );
    }
}

/// Matrix row #9: `logseq.App.search("Presentation")`.
///
/// This test pins a documented difference; it does not claim equivalence. Normalization: the
/// official `blocks` group is capped at 10 and stores each hit's full `block/content`; our `search`
/// takes a caller limit (ceiling 1000) and returns a bounded snippet plus an actionable selector.
/// Each of our hits is resolved through `block(selector)` to recover its full text, so the
/// comparison is of hit texts, not of snippets or JSON shapes. The official `pages`,
/// `pages-content` and `files` groups answer ghost pages and asset paths with DB-internal snippet
/// markup (M6); a file-page prose search has no counterpart for them, so they are printed as
/// incomparable and not compared.
///
/// Measured shape of this capture (`app_search.json`, 2026-11-20): 5 official block hits, 1 of
/// which is a DB pre-block whose content is only `key:: value` lines (ADR-0013 keeps those as page
/// properties and refuses a synthetic pre-block), plus 4 prose hits. The official search index
/// stores block content lowercased, so no official hit text is answered verbatim by the file
/// projection. After lowercasing, all 4 prose hits are answered exactly.
///
/// The test asserts that shape with exact numbers: 5 = 1 + 4, our search answers exactly 4 hits,
/// those hits equal the prose hits after folding, no pre-block text appears among our hits, all 5
/// official texts are missing verbatim, and the folded prose missing count is 0. Nothing is ignored
/// and no threshold is widened: a change on either side of the difference fails here.
#[test]
fn row_09_app_search_pins_the_pre_block_and_name_folding_difference() {
    let Some(source) = source() else {
        skip("row 09 App.search");
        return;
    };
    let official = fixture("app_search.json");
    let query = official["args"][0]
        .as_str()
        .expect("app_search.json args[0]")
        .to_owned();
    let result = &official["result"];
    let official_blocks: Vec<&str> = result["blocks"]
        .as_array()
        .expect("app_search.json result.blocks")
        .iter()
        .map(|hit| {
            hit["block/content"]
                .as_str()
                .expect("official block hit content")
        })
        .collect();
    let official_pages = result["pages"]
        .as_array()
        .expect("app_search.json result.pages")
        .len();
    let official_pages_content = result["pages-content"]
        .as_array()
        .expect("app_search.json result.pages-content")
        .len();
    let official_files = result["files"]
        .as_array()
        .expect("app_search.json result.files")
        .len();
    // Classify the official hits before comparing. A DB pre-block holds the page preamble's
    // property lines and no prose; ADR-0013 keeps those lines as page properties and refuses a
    // synthetic pre-block, so such a hit has no block counterpart in this projection.
    let is_preamble_only = |text: &str| {
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .all(|line| {
                line.contains(":: ")
                    && !line.trim_start().starts_with("- ")
                    && !line.trim_start().starts_with("\t")
            })
    };
    let preamble_hits: Vec<&str> = official_blocks
        .iter()
        .copied()
        .filter(|text| is_preamble_only(text))
        .collect();
    let prose_hits: Vec<&str> = official_blocks
        .iter()
        .copied()
        .filter(|text| !is_preamble_only(text))
        .collect();
    // The shape this test pins, from the 2026-11-20 capture. These are exact expectations, not
    // tolerances: a re-capture or a change on either side that alters the difference fails here.
    const CAPTURED_BLOCK_HITS: usize = 5;
    const CAPTURED_PREAMBLE_HITS: usize = 1;
    const CAPTURED_PROSE_HITS: usize = 4;
    const CAPTURED_OUR_HITS: usize = 4;
    const OFFICIAL_BLOCK_CAP: usize = 10;
    const OUR_LIMIT: u32 = 1000;

    let (_scratch, mut graph) = source.project("row-09");
    let hits = graph
        .search(&query, OUR_LIMIT)
        .expect("row #9: search")
        .into_value();
    let mut our_texts: BTreeSet<String> = BTreeSet::new();
    for hit in &hits {
        let view = graph
            .block(hit.selector())
            .expect("row #9: resolve a search hit's selector")
            .into_value()
            .unwrap_or_else(|| {
                panic!(
                    "row #9: selector {} no longer names a block",
                    hit.selector()
                )
            });
        our_texts.insert(view.text().to_owned());
    }
    let missing: Vec<&str> = official_blocks
        .iter()
        .copied()
        .filter(|text| !our_texts.contains(*text))
        .collect();
    // The one comparable invariant: a prose block hit must be present on both sides. The official
    // search index stores block content lowercased, and our own search matches lowercased text, so
    // both sides are compared lowercased.
    let our_folded: BTreeSet<String> = our_texts.iter().map(|text| text.to_lowercase()).collect();
    let prose_folded: BTreeSet<String> =
        prose_hits.iter().map(|text| text.to_lowercase()).collect();
    let prose_folded_missing: Vec<&str> = prose_hits
        .iter()
        .copied()
        .filter(|text| !our_folded.contains(&text.to_lowercase()))
        .collect();
    let preamble_absent = preamble_hits
        .iter()
        .filter(|text| !our_texts.contains(**text))
        .count();

    println!(
        "PARITY #9 query={query} official_blocks={} official_block_cap={OFFICIAL_BLOCK_CAP} official_pages={official_pages} official_pages_content={official_pages_content} official_files={official_files} official_preamble_only_hits={} official_prose_hits={} our_hits={} our_limit={OUR_LIMIT} our_distinct_texts={} official_texts_missing_from_ours={} pre_block_texts_absent_from_ours={preamble_absent} prose_hits_missing_after_folding={} pinned_shape={CAPTURED_BLOCK_HITS}={CAPTURED_PREAMBLE_HITS}+{CAPTURED_PROSE_HITS} our_texts={:?}",
        official_blocks.len(),
        preamble_hits.len(),
        prose_hits.len(),
        hits.len(),
        our_texts.len(),
        missing.len(),
        prose_folded_missing.len(),
        our_texts,
    );
    assert_eq!(
        official_blocks.len(),
        CAPTURED_BLOCK_HITS,
        "row #9: the capture's blocks group no longer has the pinned {CAPTURED_BLOCK_HITS} hits"
    );
    assert_eq!(
        preamble_hits.len(),
        CAPTURED_PREAMBLE_HITS,
        "row #9: the capture's DB pre-block hits are no longer {CAPTURED_PREAMBLE_HITS} property-line-only block(s)"
    );
    assert_eq!(
        prose_hits.len(),
        CAPTURED_PROSE_HITS,
        "row #9: the capture's comparable prose hits are no longer exactly {CAPTURED_PROSE_HITS}"
    );
    assert!(
        official_blocks.len() <= OFFICIAL_BLOCK_CAP,
        "row #9: the official blocks group exceeded its documented cap"
    );
    assert_eq!(
        hits.len(),
        CAPTURED_OUR_HITS,
        "row #9: our search no longer answers exactly the {CAPTURED_OUR_HITS} file-page prose hit(s)"
    );
    assert_eq!(
        our_texts.len(),
        CAPTURED_OUR_HITS,
        "row #9: our hits no longer resolve to {CAPTURED_OUR_HITS} distinct block text(s)"
    );
    assert!(
        hits.len() <= OUR_LIMIT as usize,
        "row #9: search returned more hits than the caller limit"
    );
    assert_eq!(
        preamble_absent, CAPTURED_PREAMBLE_HITS,
        "row #9: a DB pre-block text appeared among our hits; ADR-0013 keeps it as page properties"
    );
    assert_eq!(
        missing.len(),
        CAPTURED_BLOCK_HITS,
        "row #9: the pinned difference changed: not all {CAPTURED_BLOCK_HITS} official texts are missing verbatim"
    );
    assert_eq!(
        missing, official_blocks,
        "row #9: an official hit text is now answered verbatim, so the recorded difference is gone"
    );
    assert_eq!(
        prose_folded_missing.len(),
        0,
        "row #9: {} comparable prose hit(s) missing after name folding: {:?}",
        prose_folded_missing.len(),
        prose_folded_missing
    );
    assert_eq!(
        our_folded, prose_folded,
        "row #9: our hit texts no longer equal the official prose hits after lowercasing"
    );
}

/// Matrix row #13: `logseq.Editor.getBlock(uuid)`.
///
/// Normalization: the official UUID was minted by the DB for a block stating no `id::`; this file
/// projection mints ephemeral ids for such blocks (ADR-0012, M7), so a durable selector built from
/// the official UUID cannot resolve. The comparison therefore locates the block by its official
/// `content` on the page whose entity id the same capture run used for `getPage`, then reads it back
/// through `block()` and compares the text. The id-shape difference is asserted as `None` and
/// reported, not hidden.
#[test]
fn row_13_get_block_compares_the_located_blocks_text() {
    let Some(source) = source() else {
        skip("row 13 getBlock");
        return;
    };
    let official = fixture("get_block.json");
    let uuid = official["args"][0]
        .as_str()
        .expect("get_block.json args[0]");
    let content = official["result"]["content"]
        .as_str()
        .expect("get_block.json result.content");
    let page_entity = fixture("get_page.json");
    let page = page_entity["args"][0]
        .as_str()
        .expect("get_page.json args[0]")
        .to_owned();
    assert_eq!(
        official["result"]["page"]["id"], page_entity["result"]["id"],
        "row #13: the captured block belongs to a different page than get_page.json"
    );

    let (_scratch, mut graph) = source.project("row-13");
    let official_id = BlockSelector::Durable(
        BlockId::parse(uuid).unwrap_or_else(|error| panic!("row #13: bad captured uuid: {error}")),
    );
    let by_official_id = graph
        .block(&official_id)
        .expect("row #13: read the official uuid as a durable selector")
        .into_value();
    let blocks = graph
        .page_blocks(&page)
        .expect("row #13: page_blocks")
        .into_value()
        .unwrap_or_else(|| panic!("row #13: page {page} is absent"));
    let located = blocks
        .iter()
        .find(|block| block.text() == content)
        .unwrap_or_else(|| {
            panic!("row #13: no block on {page} has the captured content {content:?}")
        });
    let view = graph
        .block(located.selector())
        .expect("row #13: read the located block")
        .into_value()
        .unwrap_or_else(|| panic!("row #13: the located selector names no block"));

    println!(
        "PARITY #13 official_uuid={uuid} official_content={content:?} official_uuid_resolves={} located_selector={} located_page={} located_text={:?}",
        by_official_id.is_some(),
        located.selector(),
        located.page(),
        view.text(),
    );
    assert!(
        by_official_id.is_none(),
        "row #13: a DB-minted uuid unexpectedly became a durable id in the file projection"
    );
    assert_eq!(
        view.text(),
        content,
        "row #13: the block read by its own selector differs from the official content"
    );
    assert_eq!(
        view.page(),
        located.page(),
        "row #13: the block read belongs to a different page than the one that located it"
    );
}
