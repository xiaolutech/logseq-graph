//! Differential tests against the official Logseq graph parser's answer for one corpus.
//!
//! `fixtures/corpus/docs-v0.9.2.json` holds what the official parser answered for `logseq/docs` at
//! tag `v0.9.2`, captured by `scripts/capture_corpus_parity.sh`. The corpus is public and pinned, so
//! the fixture describes one fixed body of Markdown rather than one machine's graph, and the facts
//! below are cross-implementation facts: a page per file, the name a page answers to, how many
//! content blocks it holds, and a digest over those blocks' texts.
//!
//! The fixture holds digests, not texts, on purpose. The corpus is AGPL-3.0 and this repository is
//! MIT, so what is committed here is our own computation over a public corpus, not a copy of it.
//! A digest says that a page differs; re-running `scripts/capture_corpus_parity.sh --with-texts`
//! into a scratch path says how, and that output never enters the tree.
//!
//! Three normalization rules make the two sides comparable, and each is a stated invariant rather
//! than a widened threshold:
//!
//! - **A leading Markdown heading marker is removed from every block's text.** This repository
//!   carries a block's marker beside its text, so the text of `## Usage` is `Usage` here and
//!   `## Usage` there. That is an answer-shape difference (ADR-0012 keeps the marker in
//!   `BlockNode`), not a corpus-content difference.
//! - **A page's pre-block is not a content block.** The official projection puts a page's preamble
//!   properties in a pre-block entity; this repository keeps them in the page document. The
//!   pre-block is exactly the entity with no counterpart on this side.
//! - **The page digest is order-independent.** It is the wrapping sum of the per-block FNV-1a
//!   hashes, modulo 2^64. Order is not compared because the official sibling order is a derived
//!   `:block/left` linked list that does not reconstruct into a preorder on this corpus (measured:
//!   two blocks of `pages/Changelog.md` name the same left neighbour), so pinning order against it
//!   would pin noise. Preorder is pinned by this crate's own tests instead.
//!
//! What is left over is pinned, not filtered: a page that differs today is listed with the reason it
//! differs, and a page that starts differing fails the test. `docs/corpus-parity.md` states each
//! class and what to do about it.
//!
//! The tests run only when `LOGSEQ_GRAPH_CORPUS` names a checkout of the corpus. Without it they
//! print `SKIP=` and pass, so a plain checkout needs no graph. A variable that names something other
//! than a directory is an error, not a skip. The corpus is read only: the index is projected into a
//! temporary directory that the test removes on exit.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use logseq_graph::engine::{Freshness, Graph, OpenOptions};
use logseq_graph::index::Index;
use logseq_graph::source::GraphSource;
use serde_json::Value;

mod support;

use support::TempGraph;

/// Environment variable that names the corpus checkout the fixture was captured from.
const GRAPH_ENV: &str = "LOGSEQ_GRAPH_CORPUS";

/// Fixture file, relative to the workspace-level `fixtures/corpus` directory.
const FIXTURE: &str = "docs-v0.9.2.json";

/// Content-block counts agree on every page of the corpus. They did not until only `-` opened a
/// block: the official parser treats `*` and `+` as text, and four pages carried such lines
/// (`Tasks` 26 against 29, `Refactoring_of_logseq` 9 against 15, `term/bullet` 7 against 9,
/// `Built-in Properties` 21 against 23). That difference is now a test with an empty expectation,
/// so a page that starts differing fails it.
const BLOCK_COUNT_DIFFERENCES: &[(&str, u64, u64)] = &[];

/// Pages whose normalized block texts differ, named by the rule that makes them differ.
///
/// Bare-marker continuation text now follows the official parser too.
const BLOCK_TEXT_DIFFERENCES: &[&str] = &[];

/// One page as the official parser answered it.
struct CapturedPage {
    name: String,
    blocks: u64,
    digest: String,
}

/// One page as this repository answers it.
struct LocalPage {
    name: String,
    blocks: u64,
    digest: String,
}

/// The workspace-level captured fixture, decoded.
fn fixture() -> Value {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = manifest
        .ancestors()
        .map(|ancestor| ancestor.join("fixtures").join("corpus").join(FIXTURE))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| {
            panic!(
                "no fixtures/corpus/{FIXTURE} above {}; run scripts/capture_corpus_parity.sh",
                manifest.display()
            )
        });
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("decode {}: {error}", path.display()))
}

/// The captured pages, keyed by graph-relative path.
fn captured() -> BTreeMap<String, CapturedPage> {
    let fixture = fixture();
    fixture["pages"]
        .as_array()
        .expect("fixture pages")
        .iter()
        .map(|page| {
            let path = page["path"].as_str().expect("page path").to_owned();
            let captured = CapturedPage {
                name: page["name"].as_str().expect("page name").to_owned(),
                blocks: page["blocks"].as_u64().expect("page blocks"),
                digest: page["digest"].as_str().expect("page digest").to_owned(),
            };
            (path, captured)
        })
        .collect()
}

/// The corpus root, or `None` when the variable is unset or empty.
fn graph_root() -> Option<PathBuf> {
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
    Some(root)
}

/// FNV-1a, 64-bit, over the text's UTF-8 bytes.
///
/// The same function the capture script runs; [`fnv1a64_matches_the_published_vectors`] is what
/// makes the two comparable instead of coincidental.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A block's text with a leading Markdown heading marker removed.
fn normalized(text: &str) -> &str {
    let hashes = text.bytes().take_while(|byte| *byte == b'#').count();
    if (1..=6).contains(&hashes) && text.as_bytes().get(hashes) == Some(&b' ') {
        &text[hashes + 1..]
    } else {
        text
    }
}

/// The page digest: the wrapping sum of every block's normalized-text hash, as 16 hex digits.
fn page_digest<'a>(texts: impl Iterator<Item = &'a str>) -> String {
    let sum = texts.fold(0_u64, |sum, text| {
        sum.wrapping_add(fnv1a64(normalized(text).as_bytes()))
    });
    format!("{sum:016x}")
}

/// Every page this repository projects from the corpus, keyed by graph-relative path.
fn local_pages(root: &Path) -> BTreeMap<String, LocalPage> {
    let source = GraphSource::open(root).expect("open the corpus");
    let files = source.discover().expect("discover the corpus");
    let scratch = TempGraph::new("corpus-parity");
    let mut graph =
        Graph::open(OpenOptions::new(root, scratch.index_path()).with_startup_scan(true))
            .expect("open the corpus index");
    let index = Index::open_at(scratch.index_path()).expect("open the projected index");

    files
        .iter()
        .map(|file| {
            let path = file.relative_path().to_string_lossy().into_owned();
            let name = index
                .page_name_for_path(file.relative_path())
                .expect("look up the projected name")
                .unwrap_or_else(|| panic!("{path} has no projected name"));
            let blocks = graph
                .reads(Freshness::LastScan)
                .page_blocks(&name)
                .unwrap_or_else(|error| panic!("read the blocks of {path}: {error}"))
                .value()
                .clone()
                .unwrap_or_else(|| panic!("{path} is a discovered page with no projected blocks"));
            let texts: Vec<&str> = blocks.iter().map(|block| block.text()).collect();
            let page = LocalPage {
                name,
                blocks: texts.len() as u64,
                digest: page_digest(texts.into_iter()),
            };
            (path, page)
        })
        .collect()
}

/// Both sides for the whole corpus, or `None` for a skip.
fn both_sides() -> Option<(BTreeMap<String, CapturedPage>, BTreeMap<String, LocalPage>)> {
    let root = graph_root()?;
    Some((captured(), local_pages(&root)))
}

#[test]
fn fnv1a64_matches_the_published_vectors() {
    assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a64(b"hello"), 0xa430_d846_80aa_bd0b);
}

#[test]
fn the_corpus_file_pages_match_the_captured_paths() {
    let Some((captured, local)) = both_sides() else {
        println!("SKIP={GRAPH_ENV} is not set; corpus parity skipped");
        return;
    };

    let captured_paths: BTreeSet<&String> = captured.keys().collect();
    let local_paths: BTreeSet<&String> = local.keys().collect();
    assert_eq!(
        captured_paths, local_paths,
        "the corpus holds a different set of page files than the fixture captured"
    );
    assert_eq!(
        captured.len(),
        274,
        "the fixture must describe the pinned corpus: 274 Markdown pages and journals"
    );
}

#[test]
fn page_names_agree_including_titles_from_properties_and_yaml() {
    let Some((captured, local)) = both_sides() else {
        println!("SKIP={GRAPH_ENV} is not set; corpus parity skipped");
        return;
    };

    let mut differing: Vec<(String, String, String)> = Vec::new();
    for path in captured.keys() {
        let (theirs, ours) = (&captured[path].name, &local[path].name);
        if theirs != ours {
            differing.push((path.clone(), ours.clone(), theirs.clone()));
        }
    }
    assert!(
        differing.is_empty(),
        "page names differ from the capture: {differing:?}"
    );
}

#[test]
fn content_block_counts_agree_on_every_page() {
    let Some((captured, local)) = both_sides() else {
        println!("SKIP={GRAPH_ENV} is not set; corpus parity skipped");
        return;
    };

    let mut differing: Vec<(String, u64, u64)> = Vec::new();
    for path in captured.keys() {
        let (ours, theirs) = (local[path].blocks, captured[path].blocks);
        if ours != theirs {
            differing.push((path.clone(), ours, theirs));
        }
    }
    let expected: Vec<(String, u64, u64)> = BLOCK_COUNT_DIFFERENCES
        .iter()
        .map(|(path, ours, theirs)| ((*path).to_owned(), *ours, *theirs))
        .collect();
    assert_eq!(
        differing, expected,
        "content-block counts differ from the captured counts; the corpus agreed page for page, so \
         any entry here is a new difference"
    );
}

#[test]
fn normalized_block_texts_agree_except_the_indented_regions() {
    let Some((captured, local)) = both_sides() else {
        println!("SKIP={GRAPH_ENV} is not set; corpus parity skipped");
        return;
    };

    let mut differing: Vec<String> = Vec::new();
    let mut detail: Vec<String> = Vec::new();
    for path in captured.keys() {
        let ours = &local[path];
        let theirs = &captured[path];
        if ours.digest != theirs.digest {
            differing.push(path.clone());
            detail.push(format!(
                "  {path}: {} blocks, digest {} here; {} blocks, digest {} captured",
                ours.blocks, ours.digest, theirs.blocks, theirs.digest
            ));
        }
    }
    let expected: Vec<String> = BLOCK_TEXT_DIFFERENCES
        .iter()
        .map(|path| (*path).to_owned())
        .collect();
    assert_eq!(
        differing,
        expected,
        "the block texts of these pages differ from the captured texts:\n{}",
        detail.join("\n")
    );
}
