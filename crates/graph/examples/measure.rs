//! Measure the read and write surface against one graph, and print Markdown table rows.
//!
//! Run this on a **temporary copy** of a graph, never on the real one: the write rows change
//! files. `scripts/bench.sh` makes the copy and cleans up.
//!
//! ```text
//! cargo run --release --example measure -- <graph-root> <index-path>
//! ```
//!
//! The numbers are only comparable within one environment: the same machine, the same build
//! profile, and the same graph. `PERFORMANCE.md` records which environment produced which table,
//! and says what a change did to the numbers.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use logseq_graph::document::{BlockLocator, PageDocument};
use logseq_graph::domain::{
    BlockSelector, JournalDate, PageCommit, PageKind, PageOperation, PageSnapshot, PageSummary,
    QueryContext, QueryOutcome, SnapshotBlock,
};
use logseq_graph::engine::{Freshness, Graph, MoveTarget, OpenOptions, WriteError};
use logseq_graph::source::{GraphSource, WriteLock};

/// Milliseconds elapsed since `started`, to two decimals.
fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// How many times the version-only read is repeated for its row.
///
/// It is one stat and one read of a page that is a few KB, which is below what a single sample on
/// this machine resolves; averaging a hundred states the per-read cost instead of the clock's noise.
const VERSION_READS: u32 = 100;

/// One Markdown table row.
fn row(label: &str, started: Instant) {
    println!("| {label} | {:.2} ms |", ms(started));
}

/// Timed answers of one query at one limit, with the first (cold) answer dropped.
///
/// `MEASURE_QUERY_SAMPLES` overrides [`QUERY_SAMPLES`] so a row that turns out to cost seconds can
/// be probed once before the full set of repeats is paid for. The report states which count was
/// used, so an overridden run is never mistaken for the standard one.
const QUERY_SAMPLES: u32 = 20;

/// One query's timings, the number of results, and whether the limit cut them short.
struct QueryTiming {
    samples: Vec<f64>,
    results: usize,
    truncated: bool,
}

/// Answer one query `QUERY_SAMPLES + 1` times and keep every sample but the first.
fn time_query(
    graph: &mut Graph,
    text: &str,
    limit: u32,
    today: JournalDate,
    rounds: u32,
) -> QueryTiming {
    let context = QueryContext::new(today, limit);
    let mut samples = Vec::with_capacity(rounds as usize);
    let mut results = 0;
    let mut truncated = false;
    for round in 0..=rounds {
        let started = Instant::now();
        let answer = graph
            .reads(Freshness::LastScan)
            .query(text, &context)
            .unwrap_or_else(|error| panic!("query {text} at limit {limit}: {error}"));
        let elapsed = ms(started);
        let value = answer.into_value();
        results = match value.outcome() {
            QueryOutcome::Blocks(blocks) => blocks.len(),
            QueryOutcome::Pages(pages) => pages.len(),
        };
        truncated = value.truncated();
        if round > 0 {
            samples.push(elapsed);
        }
    }
    QueryTiming {
        samples,
        results,
        truncated,
    }
}

/// One query row: limit, median, best, worst, sample count, and how many results came back.
fn query_row(label: &str, limit: u32, timing: QueryTiming) {
    let mut samples = timing.samples;
    if samples.is_empty() {
        println!("| {label} | {limit} | _no samples_ | | | 0 | 0 |");
        return;
    }
    samples.sort_by(f64::total_cmp);
    let middle = samples.len() / 2;
    let median = if samples.len().is_multiple_of(2) {
        (samples[middle - 1] + samples[middle]) / 2.0
    } else {
        samples[middle]
    };
    let (best, worst) = (samples[0], samples[samples.len() - 1]);
    let truncated = if timing.truncated { " (truncated)" } else { "" };
    println!(
        "| {label} | {limit} | {median:.2} ms | {best:.2} ms | {worst:.2} ms | {} | {}{truncated} |",
        samples.len(),
        timing.results
    );
}

/// The block a rooted read is measured on: the middle top-level block of the page, or its first
/// child when it has one.
///
/// Children are the point: a child's chain from the page root is non-empty, so the row measures the
/// breadcrumb as well as the subtree. A top-level block measures a chain of zero.
fn mid_tree_block(snapshot: &PageSnapshot) -> Option<&SnapshotBlock> {
    let blocks = snapshot.blocks();
    if blocks.is_empty() {
        return None;
    }
    let middle = &blocks[blocks.len() / 2];
    Some(middle.children().first().unwrap_or(middle))
}

/// Number of blocks a rooted read covers: the block itself and every descendant.
///
/// The shape a `block_snapshot` costs is its subtree, not the page it sits in, so the row states
/// both rather than letting the page's block count stand for a cost that is mostly elsewhere.
fn subtree_size(block: &SnapshotBlock) -> usize {
    1 + block.children().iter().map(subtree_size).sum::<usize>()
}

/// Child index paths and the text to write for the first `limit` top-level blocks of the page that an
/// [`EditText`](PageOperation::EditText) actually replaces.
///
/// Not every block accepts that edit. A block that states a property line between two of its content
/// lines has no single place for its text ([`EditError::InterleavedProperties`]), and a large page's
/// blocks often state their `id::` line that way. The rule belongs to the document layer, so this asks
/// it over the page's own text instead of restating it, and nothing here writes: the edit is tried and
/// its result dropped. One block's answer does not depend on another's edit being applied first, which
/// is why the trials do not have to be sequential — an `EditText` replaces content lines and moves
/// neither a property line nor any structure.
///
/// `None` means the page or its file could not be read, and an empty answer means no top-level block
/// accepts the edit.
///
/// [`EditError::InterleavedProperties`]: logseq_graph::document::EditError::InterleavedProperties
/// The median non-empty page, by block count, which is the page a person actually edits.
///
/// It exists as a separate function because the write rows below measure it as well as the largest
/// page: measuring only the worst case makes every write look index-bound, and the row that decides
/// whether a schema change was worth its cost is this one.
fn median_non_empty(pages: &[PageSummary]) -> Option<&PageSummary> {
    let mut non_empty: Vec<&PageSummary> =
        pages.iter().filter(|page| page.block_count() > 0).collect();
    non_empty.sort_by_key(|page| page.block_count());
    non_empty.get(non_empty.len() / 2).copied()
}

/// A durable block on `page`, resolved through the same public read a caller would use.
///
/// The page's first durable row is not assumed to be a usable address: a duplicate `id::` or a row
/// whose file moved can make it unresolvable. Trying the public read keeps this selector discovery
/// out of the timed write and lets a graph with no durable blocks skip the cross-page row.
fn durable_block(graph: &mut Graph, page: &str) -> Option<BlockSelector> {
    let blocks = graph
        .reads(Freshness::LastScan)
        .page_blocks(page)
        .expect("list blocks while selecting a cross-page move")
        .into_value()?;
    for block in blocks {
        if block.durable_id().is_some()
            && graph
                .block(block.selector())
                .expect("resolve a durable block while selecting a cross-page move")
                .into_value()
                .is_some()
        {
            return Some(block.selector().clone());
        }
    }
    None
}

/// One real cross-page move, including both durable file replacements and the two-page commit.
///
/// The largest page with a resolvable durable block is the source and the next-largest is the
/// destination, so a graph whose largest pages state no `id::` still exercises two substantial
/// replacements without minting identity for the benchmark. Selector and subtree-size discovery
/// are setup and stay outside the clock. A graph with fewer than two such pages says so and skips.
fn measure_cross_page_move(graph: &mut Graph, pages: &[PageSummary]) {
    let mut candidates = Vec::new();
    for page in pages.iter().filter(|page| page.block_count() > 0) {
        if let Some(selector) = durable_block(graph, page.name()) {
            candidates.push((page.name().to_owned(), page.block_count(), selector));
        }
    }
    candidates.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let [source, destination, ..] = candidates.as_slice() else {
        println!(
            "\n_fewer than two pages have a resolvable durable block, so the cross-page \
             `move_block` row is skipped_"
        );
        return;
    };
    let moved_subtree = graph
        .block_snapshot(&source.2)
        .expect("snapshot the block selected for a cross-page move")
        .into_value()
        .map(|snapshot| subtree_size(snapshot.root()))
        .expect("the selected durable block has a rooted snapshot");
    let started = Instant::now();
    graph
        .move_block(&source.2, MoveTarget::After(destination.2.clone()))
        .expect("move a durable block across pages");
    row(
        &format!(
            "`move_block` across pages ({}-block source, {}-block destination, \
             {moved_subtree}-block subtree)",
            source.1, destination.1
        ),
        started,
    );
}

fn editable_top_level(
    graph: &mut Graph,
    name: &str,
    path: &Path,
    limit: usize,
) -> Option<Vec<(Vec<u32>, String)>> {
    let snapshot = graph.page_snapshot(name).ok()?.into_value()?;
    let document = PageDocument::parse(&fs::read_to_string(path).ok()?);
    Some(
        snapshot
            .blocks()
            .iter()
            .filter_map(|block| {
                let text = format!("{} measure", block.text());
                document
                    .set_block_text_text(BlockLocator::Indices(block.child_path()), &text)
                    .ok()?;
                Some((block.child_path().to_vec(), text))
            })
            .take(limit)
            .collect(),
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: measure <graph-root> <index-path>");
        std::process::exit(2);
    }
    let root = PathBuf::from(&args[1]);
    let index = PathBuf::from(&args[2]);

    println!("| Operation | Time |");
    println!("| --- | ---: |");

    // Cold start: no index exists yet, so every file is read, parsed, and projected once.
    let started = Instant::now();
    let mut graph = Graph::open(OpenOptions::new(&root, &index)).expect("cold open");
    // The same root through the source, so a durability row exercises the real `write_text` rather
    // than a copy of it: temp file, fsync, rename, fsync the directory.
    let source = GraphSource::open(&root).expect("open the graph's source");
    row("cold start (project every file)", started);

    let pages = graph
        .reads(Freshness::LastScan)
        .pages()
        .expect("list pages")
        .into_value();
    let blocks: u64 = pages.iter().map(|page| u64::from(page.block_count())).sum();
    let storage = graph
        .index_storage_path()
        .expect("resolve index")
        .expect("file-backed index")
        .to_owned();
    // Closing the only connection checkpoints WAL so the size includes every projected row.
    drop(graph);
    println!(
        "\n_{} pages, {} blocks, index {} bytes_\n",
        pages.len(),
        blocks,
        fs::metadata(storage).expect("stat index").len()
    );

    // Warm open: the index is current, so every file is compared and skipped.
    let started = Instant::now();
    let mut graph = Graph::open(OpenOptions::new(&root, &index)).expect("warm open");
    row("warm open (stat every file, project none)", started);

    let started = Instant::now();
    graph
        .reads(Freshness::CheckNow)
        .metadata()
        .expect("check now");
    row("`CheckNow` (stat every file)", started);

    let started = Instant::now();
    graph
        .reads(Freshness::VerifyContent)
        .metadata()
        .expect("verify content");
    row("`VerifyContent` (read and hash every file)", started);

    // Reads. Each one is a single index query, which is what a served request costs.
    let started = Instant::now();
    let hits = graph
        .reads(Freshness::LastScan)
        .search("logseq", 20)
        .expect("search")
        .into_value();
    row(
        &format!("`search` with a hit, limit 20 ({} returned)", hits.len()),
        started,
    );

    let started = Instant::now();
    graph
        .reads(Freshness::LastScan)
        .search("no-such-token-8f3c21d7", 20)
        .expect("search miss");
    row("`search` with no hit", started);

    // Newline-separated literal needles let the same harness measure different public corpora.
    if let Ok(needles) = std::env::var("MEASURE_SEARCH_QUERIES") {
        for needle in needles.lines().filter(|needle| !needle.is_empty()) {
            let mut samples = Vec::new();
            let mut returned = 0;
            for round in 0..=QUERY_SAMPLES {
                let started = Instant::now();
                let hits = graph
                    .reads(Freshness::LastScan)
                    .search(needle, 20)
                    .expect("measure literal search")
                    .into_value();
                let elapsed = ms(started);
                returned = hits.len();
                if round > 0 {
                    samples.push(elapsed);
                }
            }
            samples.sort_by(f64::total_cmp);
            let middle = samples.len() / 2;
            let median = (samples[middle - 1] + samples[middle]) / 2.0;
            println!(
                "| `search({needle:?}, 20)` median, {QUERY_SAMPLES} samples ({returned} returned) | {median:.4} ms |"
            );
        }
    }

    let started = Instant::now();
    graph.reads(Freshness::LastScan).pages().expect("pages");
    row(&format!("`pages`, all {}", pages.len()), started);

    let started = Instant::now();
    let tags = graph
        .reads(Freshness::LastScan)
        .tags()
        .expect("tags")
        .into_value();
    row(&format!("`tags`, all {}", tags.len()), started);

    let Some(largest) = pages.iter().max_by_key(|page| page.block_count()) else {
        println!("\n_no pages, so the per-page rows are skipped_");
        return;
    };
    let largest_name = largest.name().to_owned();
    let largest_blocks = largest.block_count();

    let started = Instant::now();
    graph
        .reads(Freshness::LastScan)
        .page_blocks(&largest_name)
        .expect("page blocks");
    row(
        &format!("`page_blocks` of the largest page ({largest_blocks} blocks)"),
        started,
    );

    let started = Instant::now();
    graph
        .reads(Freshness::LastScan)
        .page_tree(&largest_name)
        .expect("page tree");
    row(
        &format!("`page_tree` of the largest page ({largest_blocks} blocks)"),
        started,
    );

    // Snapshot reads. These are the only reads that answer from the file instead of from the
    // projection: one stat, one read of the whole page, one parse, one facts extraction. Two shapes
    // are measured on purpose — the largest page and the median non-empty one — because one number
    // only describes the shape it was taken on.
    let started = Instant::now();
    let largest_read = graph
        .page_snapshot(&largest_name)
        .expect("snapshot of the largest page");
    row(
        &format!("`page_snapshot` of the largest page ({largest_blocks} blocks)"),
        started,
    );
    let largest_snapshot = largest_read.present();

    // The versions-only read on the same page: one baseline row, one stat, one read, two fingerprints,
    // and no parse. The pair of rows is the choice a caller makes between answering "did this page
    // change, and did the index see it?" and "what is on it?".
    let started = Instant::now();
    graph
        .page_versions(&largest_name)
        .expect("versions of the largest page");
    row(
        &format!("`page_versions` of the largest page ({largest_blocks} blocks)"),
        started,
    );

    let mut non_empty: Vec<_> = pages.iter().filter(|page| page.block_count() > 0).collect();
    non_empty.sort_by_key(|page| page.block_count());
    match non_empty.get(non_empty.len() / 2) {
        Some(median) => {
            let median_name = median.name();
            let median_blocks = median.block_count();
            let started = Instant::now();
            graph
                .page_snapshot(median_name)
                .expect("snapshot of the median page");
            row(
                &format!("`page_snapshot` of the median non-empty page ({median_blocks} blocks)"),
                started,
            );

            let started = Instant::now();
            for _ in 0..VERSION_READS {
                graph
                    .page_versions(median_name)
                    .expect("versions of the median page");
            }
            println!(
                "| `page_versions` of the median non-empty page ({median_blocks} blocks), \
                 {VERSION_READS} reads averaged | {:.4} ms |",
                ms(started) / f64::from(VERSION_READS)
            );
        }
        None => println!("\n_no non-empty page, so the median `page_snapshot` row is skipped_"),
    }

    // A rooted read, addressed by the selector the projection holds for a block the snapshot above
    // named, so the row measures locating a block by its recorded identity as well as the read
    // itself. The selector is read rather than spelled: an ephemeral id comes from the index and
    // nowhere else (ADR-0012). The row is skipped rather than fatal when the page has no snapshot,
    // no block, or no projection row for the block, because the write rows below still stand.
    match largest_snapshot.and_then(mid_tree_block) {
        Some(block) => {
            let selector = graph
                .page_blocks(&largest_name)
                .expect("blocks of the largest page")
                .present()
                .and_then(|blocks| {
                    blocks
                        .iter()
                        .find(|candidate| candidate.preorder() == block.preorder())
                        .map(|candidate| candidate.selector().clone())
                });
            match selector {
                Some(selector) => {
                    let started = Instant::now();
                    let rooted = graph
                        .block_snapshot(&selector)
                        .expect("rooted snapshot of the mid-tree block");
                    if rooted.value().is_some() {
                        let depth = block.child_path().len() - 1;
                        let subtree = subtree_size(block);
                        row(
                            &format!(
                                "`block_snapshot` rooted at a mid-tree block of the largest page \
                                 (depth {depth}, {subtree}-block subtree, {largest_blocks} blocks)"
                            ),
                            started,
                        );
                    } else {
                        println!(
                            "\n_the mid-tree selector names no block, so the `block_snapshot` row \
                             is skipped_"
                        );
                    }
                }
                None => println!(
                    "\n_the projection holds no row for the mid-tree block, so the \
                     `block_snapshot` row is skipped_"
                ),
            }
        }
        None => println!(
            "\n_the largest page has no snapshot or no block, so the `block_snapshot` row is \
             skipped_"
        ),
    }

    // Simple queries. Each row is one whole read as a caller spells it: the text, one freshness
    // settle, one parse, one SQL statement, one ordered and limited answer. `Freshness::LastScan`
    // costs nothing but a field read, so the row is the query itself plus one index metadata read.
    //
    // The operands are picked from the graph rather than invented, because a query row on a value
    // no block states measures the empty answer instead of the read:
    //
    // * `github.com` is the page the most `page_refs` rows point at (6546 of them on this graph), so
    //   its `[[...]]` row is the widest recursive subtree the graph has;
    // * `collapsed`/`true` is the most used block property key and its most used value (2136 rows);
    // * `TODO` is the rarest task marker here (15 blocks) and `DONE` the most common (2937), which
    //   is what makes the pair of them show whether `limit` changes anything.
    let today = JournalDate::new(2025, 11, 6).expect("a calendar day");
    let rounds = std::env::var("MEASURE_QUERY_SAMPLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(QUERY_SAMPLES);
    println!("\n_{rounds} samples per query row, first answer dropped_\n");
    println!("| Query | limit | median | best | worst | samples | results |");
    println!("| --- | ---: | ---: | ---: | ---: | ---: | ---: |");
    for (text, label, limits) in [
        ("(task TODO)", "`(task TODO)`", [20, 1000]),
        ("(task DONE)", "`(task DONE)`", [20, 1000]),
        ("(priority A)", "`(priority A)`", [20, 1000]),
        (
            "[[github.com]]",
            "`[[github.com]]` (6546 direct refs)",
            [20, 1000],
        ),
        (
            "(property collapsed true)",
            "`(property collapsed true)` (2136 rows)",
            [20, 1000],
        ),
        (
            "(not (task TODO))",
            "`(not (task TODO))` (complement over 113007 blocks)",
            [20, 1000],
        ),
        (
            "(page-property :alias zmi)",
            "`(page-property :alias zmi)` (list-valued property)",
            [20, 1000],
        ),
        (
            "(and (task TODO) (priority A))",
            "`(and (task TODO) (priority A))` (intersection)",
            [20, 1000],
        ),
    ] {
        for limit in limits {
            query_row(
                label,
                limit,
                time_query(&mut graph, text, limit, today, rounds),
            );
        }
    }

    // Name lookup is a new read, so it gets its own row rather than a comparison with a row that
    // did not exist before. Averaged like `page_versions`: one call is below the clock's noise.
    println!("\n| Name lookup | Time |");
    println!("| --- | ---: |");
    let lookup_started = Instant::now();
    for _ in 0..rounds {
        let candidates = graph
            .reads(Freshness::LastScan)
            .page_candidates("zmi", 20)
            .expect("name lookup")
            .into_value();
        std::hint::black_box(candidates.len());
    }
    println!(
        "| `page_candidates(\"zmi\", 20)`, {rounds} reads averaged | {:.3} ms |",
        ms(lookup_started) / f64::from(rounds)
    );

    // Writes. These change files, which is why the caller must be a temporary copy.
    let started = Instant::now();
    let relative = PathBuf::from(format!("pages/{largest_name}.md"));
    let path = root.join(&relative);
    let original = fs::read_to_string(&path).expect("read the largest page");
    fs::write(&path, format!("{original}- measure probe\n")).expect("touch the largest page");
    graph
        .refresh_paths(std::slice::from_ref(&relative))
        .expect("refresh one path");
    row(
        &format!("`refresh_paths` of one file ({largest_blocks} blocks)"),
        started,
    );
    fs::write(&path, &original).expect("restore the largest page");
    graph
        .refresh_paths(std::slice::from_ref(&relative))
        .expect("refresh one path back");

    // This one is not undone: putting the file back would mean re-deriving the exact bytes
    // `append_block` wrote, and the caller already owns a copy.
    let started = Instant::now();
    graph
        .append_block(&largest_name, "measure probe")
        .expect("append to the largest page");
    row(
        &format!("`append_block` to the largest page ({largest_blocks} blocks)"),
        started,
    );

    measure_cross_page_move(&mut graph, &pages);

    // The same two writes on the median non-empty page. `append_block` and `commit_page` take a page
    // name, so neither needs the file path the refresh row above builds; the rows are skipped when
    // the median is the largest page (a graph of one page), because then they repeat the rows above.
    match median_non_empty(&pages) {
        Some(median) if median.name() != largest_name => {
            let median_name = median.name().to_owned();
            let median_blocks = median.block_count();

            let started = Instant::now();
            graph
                .append_block(&median_name, "measure probe")
                .expect("append to the median page");
            row(
                &format!("`append_block` to the median page ({median_blocks} blocks)"),
                started,
            );

            // The `EditText` target is chosen before the clock starts, from the page's own snapshot
            // and its file; a journal's file does not live under `pages/`, so the row is skipped
            // there rather than measured against a path this example guessed wrong.
            let median_path = match median.kind() {
                PageKind::Page => Some(root.join(format!("pages/{median_name}.md"))),
                PageKind::Journal => None,
            };
            // Where the write's time goes. `append_block` above is one number, and a number that is
            // not separated cannot be argued about: "the index is slow" and "two fsyncs per edit"
            // ask for opposite changes. Each row below drives one step of the engine's own write
            // path through the same public API, on the same page.
            if let Some(path) = median_path.as_ref() {
                let med_relative = PathBuf::from(format!("pages/{median_name}.md"));
                let bytes = fs::read_to_string(path).expect("read the median page");

                let started = Instant::now();
                drop(
                    WriteLock::acquire(&root, Some(std::time::Duration::from_secs(5)))
                        .expect("take the write lock"),
                );
                row("take the write lock (open + flock)", started);

                let started = Instant::now();
                let reread = fs::read_to_string(path).expect("read the median page again");
                row("read the median page's bytes", started);

                let started = Instant::now();
                let _ = PageDocument::parse(&reread);
                row("parse the median page", started);

                // The real durable replacement, through the source: temporary file, fsync, rename,
                // fsync the directory. Writing the same bytes back leaves the projection valid.
                let started = Instant::now();
                source
                    .write_text(path, &bytes)
                    .expect("replace the median page's bytes");
                row(
                    "replace one file durably (two fsyncs and a rename)",
                    started,
                );

                // What the 11 ms above is made of, on the same directory: a plain write, then the
                // file's flush, then the directory's. `sync_all` is `F_FULLFSYNC` on macOS, which
                // asks the drive to flush its own cache — that is a hardware round trip, and it is
                // the number the rows above are dominated by.
                let scratch = root.join("pages/measure-scratch.md");
                let started = Instant::now();
                fs::write(&scratch, &bytes).expect("write the scratch file");
                row("write one small file, no flush", started);

                let started = Instant::now();
                std::fs::File::open(&scratch)
                    .expect("open the scratch file")
                    .sync_all()
                    .expect("flush the scratch file");
                row("flush one small file", started);

                let started = Instant::now();
                std::fs::File::open(root.join("pages"))
                    .expect("open the pages directory")
                    .sync_all()
                    .expect("flush the pages directory");
                row("flush the directory", started);
                fs::remove_file(&scratch).expect("remove the scratch file");

                let started = Instant::now();
                graph
                    .refresh_paths(std::slice::from_ref(&med_relative))
                    .expect("refresh an unchanged page");
                row("project nothing: stat an unchanged page", started);

                // A changed page, projected: read, parse, hash, one transaction. This is the row
                // that separates the index's commit from everything the file side does.
                let touched = format!("{bytes}- measure probe\n");
                source
                    .write_text(path, &touched)
                    .expect("touch the median page");
                let started = Instant::now();
                graph
                    .refresh_paths(std::slice::from_ref(&med_relative))
                    .expect("refresh the touched page");
                row(
                    "project a changed page (read, parse, one transaction)",
                    started,
                );
                source
                    .write_text(path, &bytes)
                    .expect("restore the median page");
                graph
                    .refresh_paths(std::slice::from_ref(&med_relative))
                    .expect("refresh the restored page");
            }

            match median_path
                .and_then(|path| editable_top_level(&mut graph, &median_name, &path, 1))
            {
                Some(candidates) if !candidates.is_empty() => {
                    let operations: Vec<PageOperation> = candidates
                        .into_iter()
                        .map(|(block, text)| PageOperation::EditText { block, text })
                        .collect();
                    if let Some(base) = graph
                        .page_snapshot(&median_name)
                        .expect("snapshot of the median page")
                        .into_value()
                    {
                        let commit =
                            PageCommit::new(&median_name, base.version().clone(), operations);
                        let started = Instant::now();
                        graph
                            .commit_page(&commit)
                            .expect("commit on the median page");
                        row(
                            &format!(
                                "`commit_page` with 1 `EditText` on the median page \
                                 ({median_blocks} blocks)"
                            ),
                            started,
                        );
                    }
                }
                _ => println!(
                    "\n_no top-level block of the median page accepts an `EditText`, so its \
                     `commit_page` row is skipped_"
                ),
            }
        }
        Some(_) => println!(
            "\n_the median page is the largest page, so the median write rows are skipped_"
        ),
        None => println!("\n_no non-empty page, so the median write rows are skipped_"),
    }

    // Versioned batch commits. Each row's base version comes from the `page_snapshot` taken
    // immediately before it, which is what the CAS asks a caller to do. That read is setup, not the
    // write, and it has a row of its own above, so it runs outside the clock.
    //
    // Which blocks to edit is decided before the clock starts too. An `EditText` is refused on a
    // block that states a property line between two of its content lines
    // (`EditError::InterleavedProperties`), and the first block of a large page often states its
    // `id::` that way. The selection is not part of what a commit costs, so it is not measured.
    match editable_top_level(&mut graph, &largest_name, &path, 10) {
        Some(candidates) if !candidates.is_empty() => {
            // The label states how many edits ran, so a page with fewer than ten editable top-level
            // blocks cannot report ten. The paths still name the same blocks in the second batch:
            // the first commit changed one block's text and no structure.
            let applied = candidates.len();
            let operations: Vec<PageOperation> = candidates
                .into_iter()
                .map(|(block, text)| PageOperation::EditText { block, text })
                .collect();
            let batches = [
                (String::from("1 `EditText`"), vec![operations[0].clone()]),
                (format!("{applied} `EditText`"), operations),
            ];
            for (shape, operations) in batches {
                match graph
                    .page_snapshot(&largest_name)
                    .expect("snapshot before a commit")
                    .into_value()
                {
                    Some(base) => {
                        let commit =
                            PageCommit::new(&largest_name, base.version().clone(), operations);
                        let started = Instant::now();
                        graph.commit_page(&commit).expect("commit the text edits");
                        row(
                            &format!(
                                "`commit_page` with {shape} on the largest page \
                                 ({largest_blocks} blocks)"
                            ),
                            started,
                        );
                    }
                    None => println!(
                        "\n_the largest page has no snapshot, so the `commit_page` with {shape} \
                         row is skipped_"
                    ),
                }
            }
        }
        _ => println!(
            "\n_no top-level block of the largest page accepts an `EditText`, so the \
             `commit_page` rows are skipped_"
        ),
    }

    // The guard a stale base version hits. The version comes from bytes that an outside writer then
    // changed, so the commit must be refused with `StaleVersion` instead of writing: the check runs
    // before any operation is applied, and the batch is empty so that the row is about the check and
    // nothing else. Measuring it also has to put the page back, which the rows above do not.
    //
    // Only the refused commit is inside the clock: the snapshot, the write by the outside writer,
    // and the restore are all setup or cleanup, and the write in particular must not be counted as
    // time the guard took.
    let Some(base) = graph
        .page_snapshot(&largest_name)
        .expect("snapshot before the stale commit")
        .into_value()
    else {
        println!("\n_the largest page has no snapshot, so the stale row is skipped_");
        return;
    };
    let before_stale = fs::read_to_string(&path).expect("read the page before the stale commit");
    fs::write(&path, format!("{before_stale}- measure external edit\n"))
        .expect("edit the page outside the graph");
    let commit = PageCommit::new(&largest_name, base.version().clone(), Vec::new());
    let started = Instant::now();
    let refused = graph.commit_page(&commit);
    assert!(
        matches!(refused, Err(WriteError::StaleVersion { .. })),
        "a base version the file has moved past must be refused, not written: {refused:?}"
    );
    row(
        &format!(
            "`commit_page` refused as `StaleVersion` on the largest page ({largest_blocks} blocks)"
        ),
        started,
    );
    // Put the page back byte for byte and re-observe it, so the example leaves the graph at the page
    // its own writes left behind and a second run measures the same shape.
    fs::write(&path, &before_stale).expect("restore the page after the stale commit");
    graph
        .refresh_paths(std::slice::from_ref(&relative))
        .expect("refresh the restored page");
}
