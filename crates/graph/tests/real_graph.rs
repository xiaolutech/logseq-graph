//! Optional verification against a real Logseq graph.
//!
//! The tests run only when `LOGSEQ_GRAPH_REAL` names a graph root. Without that variable they print
//! `SKIP=` and pass, so CI and any machine without a checked-out graph still run the suite.
//!
//! The graph named by the variable is reference data and is only read. The first test discovers,
//! reads, and projects it into a temporary index. The second test copies the source directories into
//! a temporary sandbox and changes only the copy, because the scan under test deletes and renames
//! files and must never do that to the reference graph. Both temporary directories are removed when
//! the tests end, so a run cannot leave state behind or reuse an index from an earlier run.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use logseq_graph::document::{PageDocument, PageFacts};
use logseq_graph::domain::PageKind;
use logseq_graph::engine::{Freshness, Graph, OpenOptions};
use logseq_graph::source::{GraphSource, PageFile};

mod support;

use support::TempGraph;

/// Environment variable that names the graph root to verify.
const GRAPH_ENV: &str = "LOGSEQ_GRAPH_REAL";

/// Graph-relative path of the page file the incremental test adds, and the page name it decodes to.
const ADDED_FILE: &str = "pages/Real graph incremental probe.md";
const ADDED_NAME: &str = "Real graph incremental probe";

/// The graph root named by [`GRAPH_ENV`], or `None` when the variable is unset or empty.
///
/// A variable that names something other than a directory is an error, not a skip: the caller
/// asked for a specific graph, and silently passing would report evidence that was never produced.
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

#[test]
fn a_real_graph_discovers_round_trips_and_projects() {
    let Some(root) = graph_root() else {
        println!("SKIP={GRAPH_ENV} is not set; real-graph verification skipped");
        return;
    };

    // The real graph is only read; the index goes into a temporary directory.
    let scratch = TempGraph::new("real-graph");
    let source = GraphSource::open(&root).expect("open the real graph root");
    let files = source.discover().expect("discover the real graph files");

    let pages = files
        .iter()
        .filter(|file| file.kind() == PageKind::Page)
        .count();
    let journals = files
        .iter()
        .filter(|file| file.kind() == PageKind::Journal)
        .count();
    assert!(
        !files.is_empty(),
        "the graph root {} has no Markdown pages or journals",
        root.display()
    );
    println!(
        "REAL_GRAPH discovery pages={pages} journals={journals} total={}",
        files.len()
    );

    // Every file must read, parse, and render back to the bytes it came from. The graph is not
    // modified, so a failure here names a parser or renderer bug, not a stale checkout.
    let mut bytes = 0_usize;
    let mut block_text_bytes = 0_usize;
    let mut parsed_blocks = 0_u64;
    for file in &files {
        let text = file
            .read_text()
            .unwrap_or_else(|error| panic!("read {}: {error}", file.relative_path().display()));
        let document = PageDocument::parse(&text);
        // The same facts the projection will store, measured before the index exists.
        let facts = PageFacts::extract(&document);
        block_text_bytes += facts
            .blocks()
            .iter()
            .map(|block| block.text().len())
            .sum::<usize>();
        parsed_blocks += facts.blocks().len() as u64;
        let rendered = document.to_source();
        assert_eq!(
            rendered,
            text,
            "round trip changed {}",
            file.relative_path().display()
        );
        bytes += text.len();
    }
    println!(
        "REAL_GRAPH roundtrip files={} bytes={bytes} blocks={parsed_blocks} block_text_bytes={block_text_bytes}",
        files.len()
    );

    // Cold start: the index directory does not exist yet, so every discovered file is projected.
    let cold = Instant::now();
    let mut graph =
        Graph::open(OpenOptions::new(&root, scratch.index_path())).expect("project the real graph");
    let cold_elapsed = cold.elapsed();
    let metadata = graph.metadata().expect("read graph metadata").into_value();

    let mut blocks = 0_u64;
    for file in &files {
        let page = graph
            .page_metadata(file.name())
            .unwrap_or_else(|error| panic!("query page {}: {error}", file.name()))
            .into_value()
            .unwrap_or_else(|| panic!("page {} was discovered but not projected", file.name()));
        blocks += u64::from(page.block_count());
    }
    assert_eq!(
        metadata.revision(),
        files.len() as u64,
        "a cold start advances the revision once per projected file"
    );
    assert_eq!(
        blocks, parsed_blocks,
        "the projected block count must match the blocks the same parse produced"
    );
    println!(
        "REAL_GRAPH cold_start pages={} blocks={blocks} revision={} elapsed_ms={}",
        files.len(),
        metadata.revision(),
        cold_elapsed.as_millis()
    );

    // A typical search for this graph's own name. The limit is small on purpose: the read is
    // bounded, so the measurement is of one bounded query rather than of the whole match set.
    let started = Instant::now();
    let hits = graph
        .search("logseq", 20)
        .expect("search the real graph")
        .into_value();
    let search_elapsed = started.elapsed();
    assert!(
        !hits.is_empty(),
        "the real graph must contain the query this evidence line measures"
    );
    println!(
        "REAL_GRAPH search query=logseq limit=20 hits={} elapsed_us={}",
        hits.len(),
        search_elapsed.as_micros()
    );
    drop(graph);

    // Warm start: the same index directory again. Every baseline still matches, so nothing is
    // reprojected and the revision does not move.
    let warm = Instant::now();
    let mut reopened =
        Graph::open(OpenOptions::new(&root, scratch.index_path())).expect("reopen the real graph");
    let warm_elapsed = warm.elapsed();
    let reopened_metadata = reopened
        .metadata()
        .expect("read graph metadata")
        .into_value();
    assert_eq!(
        reopened_metadata.instance_id(),
        metadata.instance_id(),
        "the warm start must reuse the index it was given"
    );
    assert_eq!(
        reopened_metadata.revision(),
        metadata.revision(),
        "an unchanged graph must not be reprojected on a warm start"
    );
    println!(
        "REAL_GRAPH warm_start revision={} reprojected=0 elapsed_ms={}",
        reopened_metadata.revision(),
        warm_elapsed.as_millis()
    );
    drop(reopened);

    // The block text is the part schema version 5 added. Measure it two ways on the real graph: the
    // exact UTF-8 payload the parser produced, and the same index file with every block text blanked
    // and vacuumed. The second number is the storage cost of the column on this graph, not an
    // estimate from a fixture.
    let index_path = scratch.index_path();
    compact(&index_path);
    let index_bytes = fs::metadata(&index_path).map_or(0, |file| file.len());
    let textless_bytes = textless_index_size(&index_path);
    match textless_bytes {
        Some(textless_bytes) => println!(
            "REAL_GRAPH volume files={} blocks={blocks} text_bytes={block_text_bytes} index_bytes={index_bytes} textless_index_bytes={textless_bytes} delta_bytes={}",
            files.len(),
            index_bytes.saturating_sub(textless_bytes)
        ),
        None => println!(
            "REAL_GRAPH volume files={} blocks={blocks} text_bytes={block_text_bytes} index_bytes={index_bytes} textless_index_bytes=unavailable delta_bytes=unavailable",
            files.len()
        ),
    }

    println!("REAL_GRAPH RESULT=PASS");
}

#[test]
fn a_real_graph_rescan_projects_only_what_changed() {
    let Some(root) = graph_root() else {
        println!("SKIP={GRAPH_ENV} is not set; real-graph verification skipped");
        return;
    };

    // Every edit below lands in this copy. The graph named by the environment variable is reference
    // data and is never written, removed, or renamed.
    let sandbox = TempGraph::new("real-graph-copy");
    let copied = copy_graph(&root, &sandbox.graph_root());
    assert!(
        copied.files > 0,
        "the copy of {} holds no source file",
        root.display()
    );

    // The copy is the only graph this test observes, so its own discovery is authoritative for the
    // file names and paths used below.
    let files = GraphSource::open(sandbox.graph_root())
        .expect("open the copied graph root")
        .discover()
        .expect("discover the copied graph files");
    let page_files: Vec<_> = files
        .iter()
        .filter(|file| file.kind() == PageKind::Page)
        .collect();
    assert!(!page_files.is_empty(), "the copied graph has no page files");

    let mut graph = Graph::open(sandbox.options()).expect("project the copied graph");
    let sandbox_metadata = graph.metadata().expect("read graph metadata").into_value();
    assert_eq!(
        sandbox_metadata.revision(),
        files.len() as u64,
        "the copy starts cold, so it advances the revision once per file"
    );
    println!(
        "REAL_GRAPH sandbox files={} bytes={} pages={} revision={}",
        copied.files,
        copied.bytes,
        files.len(),
        sandbox_metadata.revision()
    );

    let mut names: Vec<String> = files.iter().map(|file| file.name().to_owned()).collect();
    let mut revision = sandbox_metadata.revision();

    // Phase 1: edit the text of one existing page. A page near the median block count is chosen so
    // the edit moves a real projection without turning one page into the whole cold-start cost.
    // Only pages that already hold blocks are candidates: a graph may hold mostly empty pages, and
    // an empty median page would prove nothing about how an edit moves a projection. Only that page
    // may be reprojected: the other pages keep the revision of the write that produced their rows,
    // and the page count holds.
    let mut by_blocks: Vec<(&PageFile, u32)> = page_files
        .iter()
        .map(|file| (*file, block_count(&mut graph, file.name())))
        .collect();
    by_blocks.sort_by_key(|(_, blocks)| *blocks);
    let non_empty: Vec<(&PageFile, u32)> = by_blocks
        .iter()
        .filter(|(_, blocks)| *blocks > 0)
        .copied()
        .collect();
    let edit_target = match non_empty.get(non_empty.len() / 2) {
        Some(&target) => Some(target),
        None => {
            println!("REAL_GRAPH edit skipped=no_non_empty_page");
            None
        }
    };
    // An all-empty graph gives the edit phase nothing to prove, but the delete and touch phases
    // still need an existing file to delete around and to touch; the first page stands in.
    let touched = edit_target
        .map(|(file, _)| file)
        .unwrap_or_else(|| page_files.first().expect("the copied graph has page files"));
    let touched_name = touched.name().to_owned();
    let touched_relative = touched
        .relative_path()
        .to_str()
        .expect("the graph root holds UTF-8 paths")
        .to_owned();

    'edit_phase: {
        let Some((edited, edited_blocks)) = edit_target else {
            break 'edit_phase;
        };
        let edited_name = edited.name().to_owned();
        let edited_relative = edited
            .relative_path()
            .to_str()
            .expect("the graph root holds UTF-8 paths")
            .to_owned();
        assert!(
            edited_blocks > 0,
            "the edited page {edited_name} on {} must already have blocks",
            root.display()
        );
        let edited_text = edited
            .read_text()
            .expect("read the page that will be edited");
        sandbox.write(&edited_relative, appended_block(&edited_text));
        let before = snapshot(&mut graph, &names);
        assert_eq!(
            before.len(),
            names.len(),
            "every discovered file is projected on a cold start"
        );

        let started = Instant::now();
        let edited_read = graph
            .reads(Freshness::CheckNow)
            .page_metadata(&edited_name)
            .expect("a check over the edited page must succeed")
            .into_value()
            .expect("the edited page is still projected");
        let edit_elapsed = started.elapsed();
        let after = snapshot(&mut graph, &names);
        let revision_after = committed_revision(&mut graph);

        assert_eq!(
            edited_read.block_count(),
            edited_blocks + 1,
            "appending one block must show up in the edited page"
        );
        assert_eq!(
            edited_read.revision(),
            revision_after,
            "the edited page carries the revision of the write that produced it"
        );
        assert_eq!(
            revision_after,
            revision + 1,
            "one changed page is exactly one committed write"
        );
        assert_eq!(
            after.len(),
            before.len(),
            "an edit must not add or drop a page"
        );
        assert_only_pages_changed(&before, &after, &[&edited_name]);
        println!(
        "REAL_GRAPH edit page={} block_count={} block_count_after={} revision_before={} revision_after={} pages={} elapsed_ms={}",
        evidence(&edited_name),
        edited_blocks,
        edited_read.block_count(),
        revision,
        revision_after,
        after.len(),
        edit_elapsed.as_millis()
    );

        // The appended text is searchable through the same handle, at the position the appended
        // top-level block occupies.
        let started = Instant::now();
        let edited_hits = graph
            .search("real-graph incremental block", 10)
            .expect("search the edited page")
            .into_value();
        let edit_search_elapsed = started.elapsed();
        assert_eq!(
            edited_hits.len(),
            1,
            "the appended text must produce exactly one hit"
        );
        assert_eq!(edited_hits[0].page(), edited_name);
        assert_eq!(
            edited_hits[0].preorder(),
            edited_blocks,
            "the appended top-level block follows every existing block"
        );
        println!(
        "REAL_GRAPH search_after_edit query=real-graph_incremental_block hits=1 page={} preorder={} elapsed_us={}",
        evidence(&edited_name),
        edited_hits[0].preorder(),
        edit_search_elapsed.as_micros()
    );
        revision = revision_after;
    }

    // Phase 2: add a file. The graph grows by one page and every existing page keeps its revision.
    assert!(
        !names.iter().any(|name| name == ADDED_NAME),
        "the graph already has a page named {ADDED_NAME}"
    );
    sandbox.write(ADDED_FILE, "- added by the real-graph verification\n");
    let before = snapshot(&mut graph, &names);

    let started = Instant::now();
    let added_read = graph
        .reads(Freshness::CheckNow)
        .page_metadata(ADDED_NAME)
        .expect("a check over the added file must succeed")
        .into_value()
        .expect("the added page must be readable");
    let add_elapsed = started.elapsed();
    names.push(ADDED_NAME.to_owned());
    let after = snapshot(&mut graph, &names);
    let revision_after = committed_revision(&mut graph);

    assert_eq!(
        added_read.block_count(),
        1,
        "the new page holds the one block its file has"
    );
    assert_eq!(
        revision_after,
        revision + 1,
        "the added file is one committed write"
    );
    assert_eq!(
        after.len(),
        before.len() + 1,
        "adding a file must add exactly one page"
    );
    assert_only_pages_changed(&before, &after, &[ADDED_NAME]);
    println!(
        "REAL_GRAPH add page={} block_count={} revision_before={} revision_after={} pages={} elapsed_ms={}",
        evidence(ADDED_NAME),
        added_read.block_count(),
        revision,
        revision_after,
        after.len(),
        add_elapsed.as_millis()
    );
    revision = revision_after;

    // Phase 3: delete a file the graph shipped with. Its page must disappear rather than error, and
    // no other page may move.
    let deleted = page_files
        .iter()
        .find(|file| file.name() != touched_name && file.name() != ADDED_NAME)
        .expect("the copied graph has a page file other than the touched one");
    let deleted_name = deleted.name().to_owned();
    let deleted_relative = deleted
        .relative_path()
        .to_str()
        .expect("the graph root holds UTF-8 paths")
        .to_owned();
    sandbox.remove(&deleted_relative);
    let before = snapshot(&mut graph, &names);

    let started = Instant::now();
    let deleted_read = graph
        .reads(Freshness::CheckNow)
        .page_metadata(&deleted_name)
        .expect("a check over the deleted file must succeed")
        .into_value();
    let delete_elapsed = started.elapsed();
    names.retain(|name| name != &deleted_name);
    let after = snapshot(&mut graph, &names);
    let revision_after = committed_revision(&mut graph);

    assert!(
        deleted_read.is_none(),
        "a deleted file must leave no readable page behind"
    );
    assert_eq!(
        revision_after,
        revision + 1,
        "removing one page is exactly one committed write"
    );
    assert_eq!(
        after.len(),
        before.len() - 1,
        "deleting a file must remove exactly one page"
    );
    assert_only_pages_changed(&before, &after, &[&deleted_name]);
    println!(
        "REAL_GRAPH delete page={} revision_before={} revision_after={} pages={} elapsed_ms={}",
        evidence(&deleted_name),
        revision,
        revision_after,
        after.len(),
        delete_elapsed.as_millis()
    );
    revision = revision_after;

    // Phase 4: rename a journal file. A journal's page name comes from the date in its file name, so
    // both the file name and the page name move; the page itself does not. How a file name renders
    // to a title is the source layer's business, so the assertions never spell the format out.
    let journal = files
        .iter()
        .find(|file| file.kind() == PageKind::Journal)
        .expect("the copied graph has a journal file");
    let old_name = journal.name().to_owned();
    let old_relative = journal
        .relative_path()
        .to_str()
        .expect("the graph root holds UTF-8 paths")
        .to_owned();

    'rename_phase: {
        let Some((year, month, day)) = journal_parts(
            journal
                .relative_path()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("the journal file has a UTF-8 stem"),
        ) else {
            println!("REAL_GRAPH rename skipped=unsupported_file_name_format");
            break 'rename_phase;
        };
        // A day in the same month that the copy does not already hold as a journal file.
        let new_day = (1..=28)
            .find(|candidate| {
                *candidate != day
                    && !sandbox
                        .graph_root()
                        .join("journals")
                        .join(format!("{year:04}_{month:02}_{candidate:02}.md"))
                        .exists()
            })
            .expect("the journal's month has a free day to rename to");
        let new_relative = format!("journals/{year:04}_{month:02}_{new_day:02}.md");
        let old_blocks = block_count(&mut graph, &old_name);
        sandbox.rename(&old_relative, &new_relative);

        // Discovery is the authority on the new name: it decodes the file name the rename wrote.
        let rediscovered = GraphSource::open(sandbox.graph_root())
            .expect("re-open the copied graph root")
            .discover()
            .expect("discover the copied graph after the rename");
        let renamed = rediscovered
            .iter()
            .find(|file| file.relative_path() == Path::new(&new_relative))
            .expect("the renamed journal must be discovered under its new file name");
        let new_name = renamed.name().to_owned();
        assert_ne!(
            old_name,
            new_name,
            "renaming {} on {} must move the page name",
            old_relative,
            root.display()
        );

        let before = snapshot(&mut graph, &names);
        let started = Instant::now();
        let renamed_read = graph
            .reads(Freshness::CheckNow)
            .page_metadata(&new_name)
            .expect("a check over the renamed journal must succeed")
            .into_value()
            .expect("the renamed journal is projected under its new name");
        let rename_elapsed = started.elapsed();
        let old_read = graph
            .page_metadata(&old_name)
            .expect("query the old journal name");
        names.retain(|name| name != &old_name);
        names.push(new_name.clone());
        let after = snapshot(&mut graph, &names);
        let revision_after = committed_revision(&mut graph);

        assert!(
            old_read.into_value().is_none(),
            "the title the old file name rendered must be gone"
        );
        assert_eq!(
            renamed_read.block_count(),
            old_blocks,
            "a rename must move the page, not change it"
        );
        assert_eq!(
            revision_after,
            revision + 2,
            "removing the old page and projecting the new one are two writes"
        );
        assert_eq!(
            after.len(),
            before.len(),
            "a rename must not change the page count"
        );
        assert_only_pages_changed(&before, &after, &[&old_name, &new_name]);
        println!(
        "REAL_GRAPH rename file_from={} file_to={} page_from={} page_to={} block_count={} revision_before={} revision_after={} pages={} elapsed_ms={}",
        evidence(&old_relative),
        evidence(&new_relative),
        evidence(&old_name),
        evidence(&new_name),
        renamed_read.block_count(),
        revision,
        revision_after,
        after.len(),
        rename_elapsed.as_millis()
    );
        revision = revision_after;
    }

    // Phase 5: touch a file without changing a byte. The baseline is refreshed, so the next scan
    // skips it again, but no derived fact moves: the revision and every page stay where they were.
    let recorded = sandbox.modified(&touched_relative);
    let touched_at = recorded
        .checked_sub(Duration::from_secs(3600))
        .expect("the copy was written long after the epoch");
    sandbox.set_modified(&touched_relative, touched_at);
    assert_ne!(
        sandbox.modified(&touched_relative),
        recorded,
        "the touch must actually move the modification time"
    );
    let before = snapshot(&mut graph, &names);
    let touched_blocks = block_count(&mut graph, &touched_name);

    let started = Instant::now();
    let touched_read = graph
        .reads(Freshness::CheckNow)
        .page_metadata(&touched_name)
        .expect("a check over the touched page must succeed")
        .into_value()
        .expect("the touched page is still projected");
    let touch_elapsed = started.elapsed();
    let after = snapshot(&mut graph, &names);
    let revision_after = committed_revision(&mut graph);

    assert_eq!(
        revision_after, revision,
        "a file whose bytes did not change must not advance the revision"
    );
    assert_eq!(
        touched_read.block_count(),
        touched_blocks,
        "a touch must not change the projected page"
    );
    assert_eq!(
        after, before,
        "a touch must leave every page revision exactly where it was"
    );
    println!(
        "REAL_GRAPH touch page={} block_count={} revision_before={} revision_after={} pages={} elapsed_ms={}",
        evidence(&touched_name),
        touched_read.block_count(),
        revision,
        revision_after,
        after.len(),
        touch_elapsed.as_millis()
    );

    println!("REAL_GRAPH RESULT=PASS");
}

/// One file's `- block` line appended to `text`, separated by a line break.
fn appended_block(text: &str) -> String {
    let mut appended = text.to_owned();
    if !appended.ends_with('\n') {
        appended.push('\n');
    }
    appended.push_str("- real-graph incremental block\n");
    appended
}

/// Fold a write-ahead log into the main file and compact the database.
///
/// A measured file size must include every page SQLite wrote, and a `VACUUM` makes the two measured
/// variants comparable. A missing `sqlite3` binary is not a test failure: the volume evidence then
/// reports raw sizes and marks the derived number unavailable.
fn compact(path: &Path) {
    let _ = std::process::Command::new("sqlite3")
        .arg(path)
        .arg("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
        .status();
}

/// Size of a copy of `path` with every block text replaced by the empty string and vacuumed.
///
/// This is the column's own storage cost on this graph: the schema and every other row stay the
/// same. It deliberately does not claim to be a version 4 database file, whose row header would be
/// one serial type shorter.
fn textless_index_size(path: &Path) -> Option<u64> {
    let copy = path.with_file_name("index-without-text.db");
    fs::copy(path, &copy).ok()?;
    let status = std::process::Command::new("sqlite3")
        .arg(&copy)
        .arg("UPDATE blocks SET text = ''; VACUUM;")
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    fs::metadata(&copy).ok().map(|file| file.len())
}

/// Committed revision of the graph, read under the handle's default freshness without a scan.
fn committed_revision(graph: &mut Graph) -> u64 {
    graph
        .metadata()
        .expect("read graph metadata")
        .into_value()
        .revision()
}

/// Revision of every page in `names`, keyed by name; a page without rows is absent.
fn snapshot(graph: &mut Graph, names: &[String]) -> BTreeMap<String, u64> {
    names
        .iter()
        .filter_map(|name| {
            graph
                .page_metadata(name)
                .unwrap_or_else(|error| panic!("query page {name}: {error}"))
                .into_value()
                .map(|page| (name.clone(), page.revision()))
        })
        .collect()
}

/// Block count of a projected page.
fn block_count(graph: &mut Graph, name: &str) -> u32 {
    graph
        .page_metadata(name)
        .unwrap_or_else(|error| panic!("query page {name}: {error}"))
        .into_value()
        .unwrap_or_else(|| panic!("page {name} is not projected"))
        .block_count()
}

/// Asserts that `before` and `after` agree on every page except those in `changed`.
///
/// This is the control side of "only one page was reprojected": a phase that also rewrote an
/// untouched page would leave a revision that moved, and the assertion names that page. A rename
/// legitimately moves two names, the one it left and the one it created.
fn assert_only_pages_changed(
    before: &BTreeMap<String, u64>,
    after: &BTreeMap<String, u64>,
    changed: &[&str],
) {
    let names: Vec<&String> = before.keys().chain(after.keys()).collect();
    for name in names {
        if changed.contains(&name.as_str()) {
            continue;
        }
        assert_eq!(
            before.get(name),
            after.get(name),
            "page {name} moved although only {} changed",
            changed.join(", ")
        );
    }
}

/// `key=value` value as the evidence line can carry it: the script splits fields on whitespace.
fn evidence(value: &str) -> String {
    value.replace(' ', "_")
}

/// `yyyy_MM_dd` parts of a journal file stem, Logseq's default `:journal/file-name-format`.
fn journal_parts(stem: &str) -> Option<(i32, u32, u32)> {
    let mut parts = stem.split('_');
    let year = parts.next()?;
    let month = parts.next()?;
    let day = parts.next()?;
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    Some((year.parse().ok()?, month.parse().ok()?, day.parse().ok()?))
}

/// What [`copy_graph`] copied.
struct Copied {
    files: usize,
    bytes: u64,
}

/// Copies the directories the source layer reads from `root` into `destination`.
///
/// A real graph also holds assets and screenshots that are not part of the graph model and can be
/// hundreds of megabytes, so only `pages/`, `journals/`, and `logseq/` move. Those are exactly the
/// directories discovery and configuration read, which makes the copy a faithful graph root.
fn copy_graph(root: &Path, destination: &Path) -> Copied {
    let mut copied = Copied { files: 0, bytes: 0 };
    for directory in ["pages", "journals", "logseq"] {
        copy_directory(
            &root.join(directory),
            &destination.join(directory),
            &mut copied,
        );
    }
    copied
}

/// Copies `from` into `to` recursively, following symlinks the way discovery does.
fn copy_directory(from: &Path, to: &Path, copied: &mut Copied) {
    let entries = match fs::read_dir(from) {
        Ok(entries) => entries,
        // A graph may have no journals directory; discovery treats that as no files.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => panic!("read {}: {error}", from.display()),
    };
    fs::create_dir_all(to).unwrap_or_else(|error| panic!("create {}: {error}", to.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("read {}: {error}", from.display()));
        let target = to.join(entry.file_name());
        let metadata = entry
            .metadata()
            .unwrap_or_else(|error| panic!("stat {}: {error}", entry.path().display()));
        if metadata.is_dir() {
            copy_directory(&entry.path(), &target, copied);
        } else if metadata.is_file() {
            let bytes = fs::copy(entry.path(), &target)
                .unwrap_or_else(|error| panic!("copy {}: {error}", entry.path().display()));
            copied.files += 1;
            copied.bytes += bytes;
        }
    }
}
