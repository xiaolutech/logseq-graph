//! Measure the served request surface: one `serve` child on one graph, representative `POST /api`
//! calls timed end to end.
//!
//! Run this on a **temporary copy** of a graph, never on the real one: the `appendBlock` row
//! changes a file. `scripts/bench.sh` makes the copy, starts the child on it, and cleans up.
//!
//! ```text
//! cargo run --release -p logseq-graph-cli --example measure_serve -- \
//!     <serve-bin> <graph-root> <index-path> <poll-ms> <burst-secs> <notify|poll>
//! ```
//!
//! The CLI crate has no library target, so nothing of the server's code is imported here: the
//! example spawns the built binary as a child process and speaks raw HTTP/1.1 to it over
//! `std::net::TcpStream`, exactly as `crates/server/tests/rpc.rs` does — the same flags, the same
//! `listening on` readiness line, the same request bytes — so the timed path is the tested path.
//!
//! What a row measures is the whole request: TCP connect, write, the server's parse, one engine
//! read under the default `last` freshness policy, serialization, and the answer read back to its
//! last byte. Each row answers [`SAMPLES`] requests, drops the first (cold) answer, and reports
//! the median of the rest, the same protocol `crates/graph/examples/measure.rs` uses for its query
//! rows. Page and needle selection run through a separate setup handle on the same index
//! (`with_startup_scan(false)`), never inside a clock.
//!
//! `poll-ms` is the child's `--poll-interval`. `watch` selects the child's watcher mode: `notify`
//! keeps the default filesystem-notification watcher, where the interval is only the failed-scan
//! retry period — `86400000` (one day) leaves the background genuinely quiet, which is the clean
//! adapter row; `poll` passes `--no-watch`, so a short interval makes the burst row overlap real
//! poll passes, which is how the watcher's interference is measured. `burst-secs` is the
//! wall-clock window of the burst row.
//!
//! The numbers are only comparable within one environment: the same machine, the same build
//! profile, and the same graph. `PERFORMANCE.md` records which environment produced which table,
//! and says what a change did to the numbers.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use logseq_graph::domain::PageSummary;
use logseq_graph::engine::{Freshness, Graph, OpenOptions};
use serde_json::{json, Value};

/// How many requests each single-call row answers. The first (cold) answer is dropped and the
/// median of the remaining twenty is reported, matching the query rows of
/// `crates/graph/examples/measure.rs`.
const SAMPLES: u32 = 21;

/// How long the example waits for the child's listening line, for a response, and for the startup
/// scan behind `getCurrentGraph`. The large graph's cold scan is about 17 s, so this is
/// deliberately much larger than any scan this repository has measured.
const TIMEOUT: Duration = Duration::from_secs(180);

/// How long readiness polling sleeps between two `getCurrentGraph` calls.
const READY_POLL: Duration = Duration::from_millis(100);

/// Milliseconds elapsed since `started`.
fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// The median of `samples`, which are already the post-cold set.
///
/// An even count averages the two middle samples and an odd count takes the middle one, the rule
/// `crates/graph/examples/measure.rs` reports its query medians with.
fn median(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

/// The 95th percentile of `samples` by the nearest-rank rule.
fn p95(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (sorted.len() * 95).div_ceil(100).min(sorted.len());
    sorted[rank - 1]
}

/// The bearer token every measured request carries (ADR-0018).
const TOKEN: &str = "bench-token";

/// A running `serve` child and the address it printed, mirroring the `Server` guard of
/// `crates/server/tests/rpc.rs`.
struct Server {
    child: Child,
    addr: String,
}

impl Server {
    /// Start `serve` on one graph copy and wait until its startup scan has finished.
    fn start(
        serve_bin: &Path,
        graph_root: &Path,
        index_path: &Path,
        poll_ms: u64,
        no_watch: bool,
    ) -> Self {
        let server = Self::spawn(serve_bin, graph_root, index_path, poll_ms, no_watch);
        server.wait_until_ready();
        server
    }

    /// Start `serve` and return once its socket is bound, which is before the graph is open. The
    /// flags and their order are the ones `crates/server/tests/rpc.rs` spells: an explicit
    /// `--index`, a kernel-chosen port, a named poll interval, and `--no-watch` when the run
    /// measures the poll loop itself.
    fn spawn(
        serve_bin: &Path,
        graph_root: &Path,
        index_path: &Path,
        poll_ms: u64,
        no_watch: bool,
    ) -> Self {
        let index = index_path.to_string_lossy().into_owned();
        let poll_interval = poll_ms.to_string();
        let mut child = Command::new(serve_bin)
            .arg("--graph")
            .arg(graph_root)
            .args(["--index", &index])
            .args(["serve", "--bind", "127.0.0.1:0"])
            .args(["--api-token", TOKEN])
            .args(["--poll-interval", &poll_interval])
            .args(if no_watch { vec!["--no-watch"] } else { vec![] })
            .stderr(Stdio::piped())
            .stdout(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|error| panic!("spawn serve: {error}"));

        let mut stderr = child.stderr.take().expect("stderr is piped");
        let addr = read_listening_line(&mut stderr);
        eprintln!("serve bound {addr}");
        // The watcher and the startup scan report on stderr; keep draining it so diagnostics can
        // never block the child this example is measuring.
        let _diagnostics = std::thread::spawn(move || {
            let mut buffer = [0; 1024];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(count) => eprint!("{}", String::from_utf8_lossy(&buffer[..count])),
                }
            }
        });
        Self { child, addr }
    }

    /// Poll `logseq.Editor.getCurrentGraph` until it stops answering `"scanning": true`
    /// (ADR-0008): the port answers before the graph is open, and this read is the one method
    /// that answers while it scans.
    fn wait_until_ready(&self) {
        let client = Client {
            addr: self.addr.clone(),
        };
        let deadline = Instant::now() + TIMEOUT;
        loop {
            assert!(
                Instant::now() < deadline,
                "the startup scan did not finish within {TIMEOUT:?}"
            );
            let value = client
                .call("logseq.Editor.getCurrentGraph", json!([]))
                .result();
            match value["scanning"].as_bool() {
                Some(true) => std::thread::sleep(READY_POLL),
                Some(false) => return,
                None => panic!("no scanning field in {value}"),
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read the child's stderr until the listening line arrives.
fn read_listening_line(stderr: &mut impl Read) -> String {
    let deadline = Instant::now() + TIMEOUT;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 256];
    while Instant::now() < deadline {
        match stderr.read(&mut chunk) {
            Ok(0) => panic!("serve exited before printing its listening line"),
            Ok(count) => {
                buffer.extend_from_slice(&chunk[..count]);
                let text = String::from_utf8_lossy(&buffer);
                if let Some(line) = text
                    .split_inclusive('\n')
                    .filter_map(|line| line.strip_suffix('\n'))
                    .find(|line| line.starts_with("listening on "))
                {
                    return line
                        .strip_prefix("listening on ")
                        .expect("the prefix was just matched")
                        .to_owned();
                }
            }
            Err(error) => panic!("reading stderr: {error}"),
        }
    }
    panic!("no listening line within {TIMEOUT:?}");
}

/// The parsed response of one request: its transport status and its body.
struct Response {
    status: u16,
    body: String,
}

impl Response {
    /// Parse one raw HTTP/1.1 response, decoding chunked transfer where the transport chose it.
    ///
    /// Both the framing and the decoding run on bytes, because a chunk boundary may fall inside a
    /// multi-byte character: `tiny_http` switches to chunked transfer above 32 KiB — an
    /// `getAllPages` answer on a large graph is well past it — and cuts the body at byte
    /// boundaries that no `str` index may name.
    fn parse(raw: &[u8]) -> Self {
        let head_end = raw
            .windows(b"\r\n\r\n".len())
            .position(|window| window == b"\r\n\r\n")
            .map(|position| position + b"\r\n\r\n".len())
            .unwrap_or_else(|| {
                panic!("no header terminator in {:?}", String::from_utf8_lossy(raw))
            });
        let (head, tail) = raw.split_at(head_end);
        let head = String::from_utf8_lossy(head);
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line in {head:?}"));
        let chunked = head.lines().any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("transfer-encoding")
                    && value.to_ascii_lowercase().contains("chunked")
            })
        });
        let body = if chunked {
            dechunk(tail)
        } else {
            tail.to_vec()
        };
        Self {
            status,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    /// The bare result value a successful call answers: the official server writes the value
    /// itself, with no JSON-RPC envelope (ADR-0018).
    fn result(&self) -> Value {
        assert_eq!(self.status, 200, "body: {}", self.body);
        serde_json::from_str(&self.body)
            .unwrap_or_else(|error| panic!("the body is one JSON document: {error}: {}", self.body))
    }
}

/// Decode an HTTP/1.1 chunked body, on bytes: a chunk boundary may fall inside a multi-byte
/// character, so the pieces cannot be `str` slices until they are joined.
fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(b"\r\n".len())
            .position(|window| window == b"\r\n")
            .unwrap_or_else(|| panic!("no chunk size in {:?}", String::from_utf8_lossy(body)));
        let size_line = String::from_utf8_lossy(&body[..line_end]);
        let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
            .unwrap_or_else(|_| panic!("not a chunk size: {size_line:?}"));
        if size == 0 {
            break;
        }
        let rest = &body[line_end + b"\r\n".len()..];
        assert!(
            rest.len() >= size + b"\r\n".len(),
            "truncated chunk in {:?}",
            String::from_utf8_lossy(body)
        );
        out.extend_from_slice(&rest[..size]);
        body = &rest[size + b"\r\n".len()..];
    }
    out
}

/// One `POST /api` endpoint: every request opens its own `TcpStream` and closes it with
/// `Connection: close`, the framing `crates/server/tests/rpc.rs` writes.
struct Client {
    addr: String,
}

impl Client {
    /// One `POST /api` call with the official `{method, args}` request shape.
    fn call(&self, method: &str, args: Value) -> Response {
        let body = json!({ "method": method, "args": args }).to_string();
        let raw = format!(
            "POST /api HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.addr,
            TOKEN,
            body.len()
        );
        let mut stream = TcpStream::connect(&self.addr)
            .unwrap_or_else(|error| panic!("connect to {}: {error}", self.addr));
        stream
            .set_read_timeout(Some(TIMEOUT))
            .expect("a read timeout is settable");
        stream.write_all(raw.as_bytes()).expect("write the request");
        let mut buffer = Vec::new();
        stream
            .read_to_end(&mut buffer)
            .expect("read the whole response");
        Response::parse(&buffer)
    }

    /// One request and the milliseconds the whole exchange took, with the answer checked.
    fn timed(&self, method: &str, args: Value) -> (f64, Value) {
        let started = Instant::now();
        let answer = self.call(method, args);
        let elapsed = ms(started);
        (elapsed, answer.result())
    }

    /// One row: [`SAMPLES`] requests, the first (cold) answer dropped, the median reported.
    ///
    /// `label` is given the first answer's `result`, outside the clock, so a row can state what
    /// it measured — a hit count, a result count — the way `crates/graph/examples/measure.rs`
    /// states the shape its own rows were taken on.
    fn median_row(&self, method: &str, args: Value, label: impl FnOnce(&Value) -> String) {
        let mut samples = Vec::with_capacity(SAMPLES as usize);
        let mut first = None;
        for _ in 0..SAMPLES {
            let (elapsed, result) = self.timed(method, args.clone());
            if first.is_none() {
                first = Some(result);
            }
            samples.push(elapsed);
        }
        let result = first.expect("SAMPLES is at least one request");
        println!("| {} | {:.2} ms |", label(&result), median(&samples[1..]));
    }
}

/// The median non-empty page by block count, which is the page a person actually edits — the same
/// selection `crates/graph/examples/measure.rs` makes for its median rows.
fn median_non_empty(pages: &[PageSummary]) -> Option<&PageSummary> {
    let mut non_empty: Vec<&PageSummary> =
        pages.iter().filter(|page| page.block_count() > 0).collect();
    non_empty.sort_by_key(|page| page.block_count());
    non_empty.get(non_empty.len() / 2).copied()
}

/// A search needle that hits this graph, found through the setup handle so the timed row measures
/// a request, not the empty answer a miss would cost.
///
/// `logseq` is the needle the engine-tier row uses, so on a graph where it hits the two rows read
/// beside each other. A graph it misses falls back to a needle cut from the largest page's first
/// non-empty block, which a substring search must hit because the block itself contains it.
fn search_needle(setup: &mut Graph, largest: &str) -> Option<String> {
    for candidate in [
        "logseq".to_owned(),
        needle_from_a_block(setup, largest).unwrap_or_default(),
    ] {
        if candidate.is_empty() {
            continue;
        }
        let hits = setup
            .reads(Freshness::LastScan)
            .search(&candidate, 20)
            .expect("search while selecting a needle")
            .into_value();
        if !hits.is_empty() {
            return Some(candidate);
        }
    }
    None
}

/// Cut a needle out of the first non-empty block text of one page.
fn needle_from_a_block(setup: &mut Graph, page: &str) -> Option<String> {
    let blocks = setup
        .reads(Freshness::LastScan)
        .page_blocks(page)
        .expect("list blocks while selecting a needle")
        .into_value()?;
    let text = blocks
        .iter()
        .map(|block| block.text().trim())
        .find(|text| !text.is_empty())?;
    Some(text.chars().take(12).collect())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 7 {
        eprintln!(
            "usage: measure_serve <serve-bin> <graph-root> <index-path> <poll-ms> <burst-secs> <notify|poll>"
        );
        std::process::exit(2);
    }
    let serve_bin = PathBuf::from(&args[1]);
    let root = PathBuf::from(&args[2]);
    let index = PathBuf::from(&args[3]);
    let poll_ms: u64 = args[4]
        .parse()
        .expect("poll-ms is a number of milliseconds");
    let burst_secs: u64 = args[5].parse().expect("burst-secs is a number of seconds");
    let no_watch = match args[6].as_str() {
        "notify" => false,
        "poll" => true,
        other => panic!("watch mode is `notify` or `poll`, not {other}"),
    };

    // Setup only, never timed: a second handle on the same copy and the same index, opened without
    // a startup scan so it answers from the rows the engine tier above already projected. A run
    // against a fresh index — the example invoked on its own rather than through
    // `scripts/bench.sh` — scans once here, outside any clock, rather than reporting an empty
    // graph.
    let mut setup = Graph::open(OpenOptions::new(&root, &index).with_startup_scan(false))
        .expect("open the setup handle");
    let mut pages = setup
        .reads(Freshness::LastScan)
        .pages()
        .expect("list pages through the setup handle")
        .into_value();
    if pages.is_empty() {
        setup.rescan().expect("scan the setup handle once");
        pages = setup
            .reads(Freshness::LastScan)
            .pages()
            .expect("list pages through the setup handle")
            .into_value();
    }
    let blocks: u64 = pages.iter().map(|page| u64::from(page.block_count())).sum();

    let Some(largest) = pages.iter().max_by_key(|page| page.block_count()) else {
        println!("\n_no pages, so the served rows are skipped_");
        return;
    };
    let largest_name = largest.name().to_owned();
    let largest_blocks = largest.block_count();
    let median_page = median_non_empty(&pages).expect("a graph with pages has a non-empty one");
    let median_name = median_page.name().to_owned();
    let median_blocks = median_page.block_count();

    // The child runs on the copy only: it receives the same root and index this example holds,
    // which `scripts/bench.sh` already copied out of the named graph. It is started before the
    // table is printed, so its stderr diagnostics — the listening line, watcher reports — precede
    // the rows rather than interleaving with them.
    let server = Server::start(&serve_bin, &root, &index, poll_ms, no_watch);
    let client = Client {
        addr: server.addr.clone(),
    };

    println!(
        "\n_served tier: {} pages, {} blocks, {} watcher at a {poll_ms} ms interval_\n",
        pages.len(),
        blocks,
        args[6]
    );
    println!("| Request | Time |");
    println!("| --- | ---: |");

    client.median_row("logseq.Editor.getCurrentGraph", json!([]), |_| {
        "serve: getCurrentGraph".to_owned()
    });
    client.median_row("logseq.Editor.getAllPages", json!([]), |_| {
        format!("serve: getAllPages ({} pages)", pages.len())
    });

    // The page-shaped reads, on the median page and the largest one: a served request's cost
    // follows the page it asks about, and one number only describes the shape it was taken on.
    client.median_row(
        "logseq.Editor.getPageBlocksTree",
        json!([median_name]),
        |_| format!("serve: getPageBlocksTree of the median page ({median_blocks} blocks)"),
    );
    client.median_row(
        "logseq.Editor.getPageBlocksTree",
        json!([largest_name]),
        |_| format!("serve: getPageBlocksTree of the largest page ({largest_blocks} blocks)"),
    );

    // Search. The needle is picked through the setup handle so the row measures a hit; the label
    // states the needle and the answer's size, as the engine-tier row states its hit count.
    match search_needle(&mut setup, &largest_name) {
        Some(needle) => client.median_row(
            "logseq.App.search",
            json!([needle, { "limit": 20 }]),
            |result| {
                let hits = result["hits"].as_array().map(Vec::len).unwrap_or(0);
                format!("serve: search with a hit, limit 20 (needle {needle:?}, {hits} returned)")
            },
        ),
        None => println!("\n_no needle hit, so the search row is skipped_"),
    }

    // One query at the two limits that show whether the answer's size is the cost.
    for limit in [20, 1000] {
        client.median_row(
            "logseq.Graph.query",
            json!(["(task DONE)", { "limit": limit }]),
            |result| {
                let count = result["blocks"].as_array().map(Vec::len).unwrap_or(0);
                let truncated = if result["truncated"].as_bool() == Some(true) {
                    ", truncated"
                } else {
                    ""
                };
                format!("serve: query `(task DONE)` at limit {limit} ({count} results{truncated})")
            },
        );
    }

    client.median_row("logseq.Graph.getPageVersion", json!([median_name]), |_| {
        format!("serve: getPageVersion of the median page ({median_blocks} blocks)")
    });
    client.median_row("logseq.Graph.getPageSnapshot", json!([median_name]), |_| {
        format!("serve: getPageSnapshot of the median page ({median_blocks} blocks)")
    });
    client.median_row(
        "logseq.Graph.getPageSnapshot",
        json!([largest_name]),
        |_| format!("serve: getPageSnapshot of the largest page ({largest_blocks} blocks)"),
    );

    // The burst row: continuous `getPageBlocksTree` requests of the median page for a fixed wall
    // window, reporting the sample count, median, p95, and max on one row. This is the row where
    // a background poll's interference shows — a short poll interval makes the window overlap
    // several poll passes, and the one-day interval leaves the window clean.
    let deadline = Instant::now() + Duration::from_secs(burst_secs);
    let mut burst = Vec::new();
    loop {
        let (elapsed, _) = client.timed("logseq.Editor.getPageBlocksTree", json!([median_name]));
        burst.push(elapsed);
        if Instant::now() >= deadline {
            break;
        }
    }
    println!(
        "| serve: burst getPageBlocksTree of the median page ({median_blocks} blocks), \
         {burst_secs} s window | {} requests, median {:.2} ms, p95 {:.2} ms, max {:.2} ms |",
        burst.len(),
        median(&burst),
        p95(&burst),
        burst.iter().copied().fold(f64::MIN, f64::max),
    );

    // The one mutating row, measured last so every read above it ran on the copy as the caller
    // provided it. Like the engine-tier write rows, the append is not undone: the caller owns a
    // copy.
    client.median_row(
        "logseq.Graph.appendBlock",
        json!([median_name, "measure probe"]),
        |_| format!("serve: appendBlock to the median page ({median_blocks} blocks)"),
    );
}
