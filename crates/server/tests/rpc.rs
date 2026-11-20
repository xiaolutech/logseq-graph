//! End-to-end tests of the `serve` command's HTTP `POST /api` surface.
//!
//! Every case writes a real graph into a temporary directory, runs the built binary as a child
//! process with `serve`, and speaks raw HTTP/1.1 over `std::net::TcpStream`, so the tests cover
//! request parsing, the engine, the freshness pass-through, and the JSON shapes in one path without
//! an HTTP client dependency. The child is killed between cases, and the last one checks that the
//! port is free again afterwards.
//!
//! # Why every test names an official method
//!
//! This surface is our own JSON-RPC contract, not a Logseq compatibility layer. Each test states
//! (a) the official Logseq method it corresponds to, (b) the parity-matrix row `#N`, and (c) its
//! classification: `deliberate-difference` or `out-of-scope`. The matrix classifies every row that
//! way; where one field or one protocol rule does agree with the official method, the comment says
//! so at that granularity and still classifies the test as a whole. A deliberate difference names
//! its reason: an ADR clause or a measured fact from the reference implementation's
//! `docs/compatibility.md`. Row numbers follow the order of the thirteen former GET endpoints:
//!
//! | # | former endpoint | official method | our RPC method |
//! | --- | --- | --- | --- |
//! | 1 | `/info` | `logseq.Editor.getCurrentGraph` | `logseq.Editor.getCurrentGraph` |
//! | 2 | `/pages` | `logseq.Editor.getAllPages` | `logseq.Editor.getAllPages` |
//! | 3 | `/favorites` | `logseq.App.getUserConfigs` | `logseq.Graph.getFavorites` |
//! | 4 | `/page` | `logseq.Editor.getPage` | `logseq.Editor.getPage` |
//! | 5 | `/tree` | `logseq.Editor.getPageBlocksTree` | `logseq.Editor.getPageBlocksTree` |
//! | 6 | `/backlinks` | `logseq.Editor.getPageLinkedReferences` | `logseq.Editor.getPageLinkedReferences` |
//! | 7 | `/tags` | `getAllPages` (tag pages) | `logseq.Graph.getAllTags` |
//! | 8 | `/tag` | `logseq.DB.q(page-tags)` | `logseq.Graph.getPagesWithTag` |
//! | 9 | `/search` | `logseq.App.search` | `logseq.App.search` |
//! | 10 | `/page-version` | none | `logseq.Graph.getPageVersion` |
//! | 11 | `/snapshot` | `getPageBlocksTree` | `logseq.Graph.getPageSnapshot` |
//! | 12 | `/query` | `logseq.DB.q` | `logseq.Graph.query` |
//! | 13 | `/block` | `logseq.Editor.getBlock` | `logseq.Editor.getBlock` |
//!
//! Methods in the `logseq.Graph.*` namespace are this repository's own reads; they are not part of
//! the official API contract. `#10` is out-of-scope in the parity matrix: the official API has no
//! version-token read, so no test claims equivalence for it.

// The test names keep the official method's capitalisation (`logseq_Editor_getPage`), which is
// the whole point of naming them after the official API.
#![allow(non_snake_case)]

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The binary cargo built for this test run.
const BIN: &str = env!("CARGO_BIN_EXE_logseq-graph");

/// How long a test waits for the child's listening line or for a connection to fail.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The names a `method-not-found` error lists, so an agent can pick a real method.
const METHODS: &[&str] = &[
    "logseq.Editor.getCurrentGraph",
    "logseq.Editor.getAllPages",
    "logseq.Graph.getFavorites",
    "logseq.Editor.getPage",
    "logseq.Editor.getPageBlocksTree",
    "logseq.Editor.getPageLinkedReferences",
    "logseq.Graph.getAllTags",
    "logseq.Graph.getPagesWithTag",
    "logseq.App.search",
    "logseq.Graph.getPageVersion",
    "logseq.Graph.getPageSnapshot",
    "logseq.Graph.query",
    "logseq.Editor.getBlock",
    "logseq.App.checkCurrentIsDBGraph",
    "logseq.App.getUserConfigs",
    "logseq.App.getAppInfo",
    "logseq.showMsg",
    "logseq.DB.q",
    "logseq.Editor.getCurrentPage",
];

const CREATE_PAGE: &str = "logseq.Graph.createPage";
const APPEND_BLOCK: &str = "logseq.Graph.appendBlock";
const MOVE_BLOCK: &str = "logseq.Graph.moveBlock";
const COMMIT_PAGE: &str = "logseq.Graph.commitPage";

/// The bearer tokens every served request may carry (ADR-0018): `serve` accepts any configured
/// token, and the harness passes both to pin that rule.
const TOKEN: &str = "test-token-alpha";
const TOKEN_BETA: &str = "test-token-beta";

/// A running `serve` child and the address it printed.
struct Server {
    child: Child,
    addr: String,
}

impl Server {
    /// Start `serve` and wait until the startup scan has finished.
    fn start(graph_root: &Path, label: &str) -> Self {
        let server = Self::spawn(graph_root, label);
        server.wait_until_ready(label);
        server
    }

    /// Start `serve` with no watcher and no poll inside the test's lifetime, and wait until the
    /// startup scan has finished. A test that edits a file after the server started and then asserts
    /// what the index still holds must use this: with the watcher on, a notification or a poll can
    /// reproject the edit at any moment and the assertion races it.
    fn start_without_watcher(graph_root: &Path, label: &str) -> Self {
        let server = Self::spawn_with_mode(graph_root, label, 600_000, true);
        server.wait_until_ready(label);
        server
    }

    /// Start `serve` and return once its socket is bound, which is before the graph is open.
    fn spawn(graph_root: &Path, label: &str) -> Self {
        Self::spawn_with_poll_interval(graph_root, label, 1_000)
    }

    /// Start with a short poll period when a test exercises the background watcher.
    fn spawn_with_poll_interval(graph_root: &Path, label: &str, milliseconds: u64) -> Self {
        Self::spawn_with_mode(graph_root, label, milliseconds, false)
    }

    fn spawn_with_mode(graph_root: &Path, label: &str, milliseconds: u64, no_watch: bool) -> Self {
        let poll_interval = milliseconds.to_string();
        let mut child = Command::new(BIN)
            .arg("--graph")
            .arg(graph_root)
            .args(["--index", &Sandbox::index_path(graph_root, label)])
            .args(["serve", "--bind", "127.0.0.1:0"])
            .args(["--api-token", TOKEN])
            .args(["--api-token", TOKEN_BETA])
            .args(["--poll-interval", &poll_interval])
            .args(if no_watch { vec!["--no-watch"] } else { vec![] })
            .stderr(Stdio::piped())
            .stdout(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|error| panic!("{label}: spawn serve: {error}"));

        let mut stderr = child.stderr.take().expect("stderr is piped");
        let addr = read_listening_line(&mut stderr, label);
        eprintln!("{label}: serve bound {addr}");
        // The watcher reports repeated failures on stderr. Keep draining it so diagnostics can never
        // block the server this test is exercising.
        let _diagnostics = std::thread::spawn(move || {
            let mut buffer = [0; 1024];
            loop {
                match std::io::Read::read(&mut stderr, &mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(count) => eprint!("{}", String::from_utf8_lossy(&buffer[..count])),
                }
            }
        });
        Self { child, addr }
    }

    /// Poll `getCurrentGraph` until it stops reporting `scanning` (ADR-0008).
    fn wait_until_ready(&self, label: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            assert!(
                Instant::now() < deadline,
                "{label}: the startup scan did not finish within {TIMEOUT:?}"
            );
            let result = self.call(METHODS[0], json!([])).result();
            match result["scanning"].as_bool() {
                Some(true) => std::thread::sleep(Duration::from_millis(2)),
                Some(false) => return,
                None => panic!("{label}: no scanning field in {result}"),
            }
        }
    }

    /// One `POST /api` call with the official `{method, args}` request shape.
    fn call(&self, method: &str, args: Value) -> Response {
        self.post(&json!({ "method": method, "args": args }).to_string())
    }

    /// One call with a caller-supplied `id`, or any other request object.
    fn call_object(&self, document: Value) -> Response {
        self.post(&document.to_string())
    }

    /// One `POST /api` call with a raw (possibly invalid) body.
    fn post(&self, body: &str) -> Response {
        self.raw("POST", "/api", body)
    }

    /// One `POST /api` call with a caller-chosen `Authorization` value, for the authentication
    /// cases. `None` sends no `Authorization` header at all.
    fn post_with(&self, body: &str, authorization: Option<&str>) -> Response {
        self.raw_with("POST", "/api", body, authorization)
    }

    /// One raw request with a caller-chosen `Authorization` value; `None` sends none.
    fn raw_with(
        &self,
        verb: &str,
        path: &str,
        body: &str,
        authorization: Option<&str>,
    ) -> Response {
        let header =
            authorization.map_or(String::new(), |value| format!("Authorization: {value}\r\n"));
        self.raw_headers(
            verb,
            path,
            body,
            &format!(
                "Host: {}\r\nContent-Type: application/json\r\n{header}",
                self.addr
            ),
        )
    }

    /// One raw HTTP/1.1 request and the response head, as one string with the body attached.
    /// Carries the bearer token `serve` requires (ADR-0018); `raw_headers` is the entry point for
    /// requests that need a different header set.
    fn raw(&self, verb: &str, path: &str, body: &str) -> Response {
        self.raw_headers(
            verb,
            path,
            body,
            &format!(
                "Host: {}\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\n",
                self.addr, TOKEN
            ),
        )
    }

    fn raw_headers(&self, verb: &str, path: &str, body: &str, headers: &str) -> Response {
        let mut stream = TcpStream::connect(&self.addr)
            .unwrap_or_else(|error| panic!("connect to {}: {error}", self.addr));
        stream
            .set_read_timeout(Some(TIMEOUT))
            .expect("a read timeout is settable");
        let framed = headers.lines().any(|line| {
            line.split_once(':').is_some_and(|(name, _)| {
                name.eq_ignore_ascii_case("Content-Length")
                    || name.eq_ignore_ascii_case("Transfer-Encoding")
            })
        });
        let length = if framed {
            String::new()
        } else {
            format!("Content-Length: {}\r\n", body.len())
        };
        let raw =
            format!("{verb} {path} HTTP/1.1\r\n{headers}{length}Connection: close\r\n\r\n{body}");
        stream.write_all(raw.as_bytes()).expect("write the request");
        let mut buffer = Vec::new();
        stream
            .read_to_end(&mut buffer)
            .expect("read the whole response");
        Response::parse(&buffer)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The parsed response of one request: status, body, and whether the connection closed.
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Response {
    fn parse(raw: &[u8]) -> Self {
        let text = String::from_utf8_lossy(raw).into_owned();
        let status = text
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line in {text:?}"));
        let (head, tail) = text
            .split_once("\r\n\r\n")
            .unwrap_or_else(|| panic!("no header terminator in {text:?}"));
        // Accept either framing in this client; the server sends Content-Length and closes.
        let chunked = head.lines().any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("transfer-encoding")
                    && value.to_ascii_lowercase().contains("chunked")
            })
        });
        let body = if chunked {
            let offset = head.len() + 4;
            dechunk(&raw[offset..])
        } else {
            tail.to_owned()
        };
        let headers = head
            .lines()
            .skip(1)
            .filter_map(|line| {
                line.split_once(':')
                    .map(|(name, value)| (name.to_owned(), value.trim().to_owned()))
            })
            .collect();
        Self {
            status,
            headers,
            body,
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The whole response document, which must be one JSON object.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|error| panic!("the body is one JSON document: {error}: {}", self.body))
    }

    /// The bare result value a successful call answers: the official server writes the value
    /// itself, with no JSON-RPC envelope (ADR-0018).
    fn result(self) -> Value {
        assert_eq!(self.status, 200, "body: {}", self.body);
        self.json()
    }

    /// The error document of a failed call, with the transport status beside it. Failures answer
    /// the Fastify shape — `statusCode`, `error`, `message` — with this surface's `code` and `data`
    /// beside it (ADR-0018).
    fn error(self) -> (u16, Value) {
        let document = self.json();
        assert_eq!(
            document["statusCode"].as_u64(),
            Some(u64::from(self.status)),
            "statusCode states the transport status: {}",
            self.body
        );
        assert!(
            document["error"].is_string(),
            "error names the status: {}",
            self.body
        );
        assert!(
            document["message"].is_string(),
            "message is a string: {}",
            self.body
        );
        assert!(document["code"].is_i64(), "code is a number: {}", self.body);
        assert!(
            document["data"].is_object(),
            "data is structured: {}",
            self.body
        );
        (self.status, document)
    }

    /// Assert one JSON-RPC `code`, and return the error document for further checks.
    fn expect_code(self, status: u16, code: i64) -> Value {
        let (actual_status, error) = self.error();
        assert_eq!(actual_status, status, "transport status: {error}");
        assert_eq!(error["code"].as_i64(), Some(code), "error: {error}");
        error
    }
}

/// Decode an HTTP/1.1 chunked body on bytes: a chunk boundary may split a multi-byte character,
/// so slicing a `&str` by the byte count can panic or corrupt the text.
fn dechunk(mut body: &[u8]) -> String {
    let mut out = Vec::new();
    loop {
        let split = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .unwrap_or_else(|| panic!("no chunk size in {body:?}"));
        let size_line = std::str::from_utf8(&body[..split]).expect("a chunk size is ASCII");
        let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
            .unwrap_or_else(|_| panic!("not a chunk size: {size_line:?}"));
        let rest = &body[split + 2..];
        if size == 0 {
            break;
        }
        assert!(rest.len() >= size + 2, "truncated chunk in {body:?}");
        out.extend_from_slice(&rest[..size]);
        body = &rest[size + 2..];
    }
    String::from_utf8(out).expect("the dechunked body is UTF-8")
}

/// Read the child's stderr until the listening line arrives.
fn read_listening_line(stderr: &mut impl std::io::Read, label: &str) -> String {
    let deadline = Instant::now() + TIMEOUT;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 256];
    while Instant::now() < deadline {
        match stderr.read(&mut chunk) {
            Ok(0) => panic!("{label}: serve exited before printing its listening line"),
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
            Err(error) => panic!("{label}: reading stderr: {error}"),
        }
    }
    panic!("{label}: no listening line within {TIMEOUT:?}");
}

/// A temporary sandbox with the same three-page graph `cli.rs` populates, plus one page whose name
/// holds spaces, Chinese characters, and a slash.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn index_path(graph_root: &Path, label: &str) -> String {
        graph_root
            .parent()
            .expect("the graph root has a parent")
            .join(format!("{label}-index.db"))
            .to_string_lossy()
            .into_owned()
    }

    fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "logseq-graph-rpc-{label}-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let sandbox = Self { root };
        for directory in [
            "graph/pages",
            "graph/journals",
            "graph/logseq",
            "cache",
            "home",
        ] {
            fs::create_dir_all(sandbox.root.join(directory)).expect("create sandbox directory");
        }
        sandbox.write("logseq/config.edn", "{}\n");
        sandbox.write(
            "pages/alpha.md",
            "- alpha root\n\t- alpha child\n\t\t- alpha grandchild\n",
        );
        sandbox.write(
            "pages/Bravo.md",
            "- bravo sees [[alpha]]\n\
             - bravo tagged #project\n\
             \t- stable block\n\
             \t  id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n",
        );
        sandbox.write("journals/2026_08_13.md", "- journal entry [[alpha]]\n");
        // Logseq spells a namespace separator as `___` in the file name, so the file holds
        // `阅读 notes___page one.md` and the page is `阅读 notes/page one`.
        sandbox.write(
            "pages/阅读 notes___page one.md",
            "- spaced root\n\t- spaced child\n\t\t- spaced grandchild\n\t\t\t- spaced great\n",
        );
        sandbox
    }

    fn graph_root(&self) -> PathBuf {
        self.root.join("graph")
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.graph_root().join(relative);
        fs::create_dir_all(path.parent().expect("file has a parent")).expect("create parent");
        fs::write(path, contents).expect("write graph file");
    }

    fn read(&self, relative: &str) -> Vec<u8> {
        fs::read(self.graph_root().join(relative)).expect("read graph file")
    }

    /// Add `count` one-block pages, so a cold projection is long enough to be observed from a
    /// second process instead of finishing between two requests.
    fn write_filler_pages(&self, count: usize) {
        for index in 0..count {
            self.write(&format!("pages/filler {index}.md"), "- a filler block\n");
        }
    }

    /// How many page files the graph holds, which is what `pages: N` must answer.
    fn page_file_count(&self) -> usize {
        fs::read_dir(self.graph_root().join("pages"))
            .expect("list pages")
            .count()
            + fs::read_dir(self.graph_root().join("journals"))
                .expect("list journals")
                .count()
    }

    /// The blocks one `getCurrentGraph` answer must count: 3 alpha, 3 Bravo, 1 journal, 4 spaced.
    fn block_count(&self) -> u64 {
        11
    }

    fn revision_of(&self, result: &Value) -> u64 {
        result["revision"]
            .as_u64()
            .unwrap_or_else(|| panic!("no revision in {result}"))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The trailing options object of `args`, for calls that name a freshness policy or a limit.
fn options(pairs: Value) -> Value {
    json!([pairs])
}

fn with_options(argument: &str, pairs: Value) -> Value {
    json!([argument, pairs])
}

// ------------------------------------------------------------------------------------------------
// Transport harness regression (parity row #2, `logseq.Editor.getAllPages` — deliberate-difference).
// This test pins the raw HTTP test client, not the method's classification: the served-benchmark
// run in LESSONS hit a `getAllPages` answer above 32 KiB holding Chinese page names, and the `&str`
// decoder panicked at byte 8192 where a chunk boundary cut a multi-byte character.
// ------------------------------------------------------------------------------------------------
#[test]
fn dechunk_joins_a_utf8_character_split_across_chunks() {
    let name = "\u{4e2d}".repeat(3_000);
    let document = json!({ "name": name }).to_string();
    let bytes = document.as_bytes();
    let split = 8_192; // the chunk size the sender used in the served-benchmark run
    assert!(bytes.len() > split, "two chunks are needed");
    assert!(
        std::str::from_utf8(&bytes[..split]).is_err(),
        "the cut must land inside a multi-byte character"
    );

    let mut raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for chunk in [&bytes[..split], &bytes[split..]] {
        raw.extend_from_slice(format!("{:X}\r\n", chunk.len()).as_bytes());
        raw.extend_from_slice(chunk);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"0\r\n\r\n");

    let response = Response::parse(&raw);
    assert_eq!(
        response.body, document,
        "the pieces are joined before the bytes become text"
    );
    assert_eq!(response.json()["name"], json!(name));
}

#[path = "rpc/files.rs"]
mod files;

#[path = "rpc/official_shapes.rs"]
mod official_shapes;
#[path = "rpc/pages.rs"]
mod pages;
#[path = "rpc/protocol.rs"]
mod protocol;
#[path = "rpc/queries.rs"]
mod queries;
#[path = "rpc/watch.rs"]
mod watch;
#[path = "rpc/writes.rs"]
mod writes;
