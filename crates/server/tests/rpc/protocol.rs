// Freshness, method naming, error objects, lifecycle and page candidates.
//
// Every case names its official method and parity-matrix row; see the crate root.

use super::*;
use serde_json::json;

#[test]
fn incomplete_bodies_do_not_block_other_clients_and_large_bodies_are_refused() {
    let sandbox = Sandbox::new("body-limits");
    let server = Server::start(&sandbox.graph_root(), "body-limits");
    let mut stalled = TcpStream::connect(&server.addr).expect("connect slow client");
    write!(stalled, "POST /api HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: 100000\r\n\r\n{{", server.addr, TOKEN).expect("send incomplete body");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            sender
                .send(server.call(METHODS[0], json!([])).status)
                .expect("send status");
        });
        let status = receiver.recv_timeout(Duration::from_secs(3));
        drop(stalled);
        assert_eq!(
            status.expect("another client must not wait for the stalled body"),
            200
        );
    });
    let mut oversized = TcpStream::connect(&server.addr).expect("connect oversized client");
    oversized
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("set timeout");
    write!(oversized, "POST /api HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: 16777217\r\n\r\n", server.addr, TOKEN).expect("send oversized length without body");
    let mut reader = std::io::BufReader::new(oversized);
    let mut status = String::new();
    std::io::BufRead::read_line(&mut reader, &mut status).expect("read early rejection");
    assert!(status.contains("413"), "{status}");
    let mut rest = Vec::new();
    reader
        .read_to_end(&mut rest)
        .expect("read declared-length rejection");
    let response = Response::parse(&[status.as_bytes(), &rest].concat());
    assert_transport_error(&response, 413, -32600, "invalid-request");
    drop(reader);

    let body = "x".repeat(16 * 1024 * 1024 + 1);
    let chunked = format!("{:x}\r\n{body}\r\n0\r\n\r\n", body.len());
    let response = server.raw_headers(
        "POST",
        "/api",
        &chunked,
        &format!(
            "Host: {}\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nTransfer-Encoding: chunked\r\n",
            server.addr, TOKEN
        ),
    );
    assert_eq!(response.status, 413, "{}", response.body);
    assert_transport_error(&response, 413, -32600, "invalid-request");
    assert!(!sandbox.read("pages/alpha.md").is_empty());
}

#[test]
fn stalled_clients_do_not_exhaust_the_request_workers() {
    let sandbox = Sandbox::new("stalled-workers");
    let server = Server::start(&sandbox.graph_root(), "stalled-workers");
    let mut stalled = Vec::new();
    for _ in 0..16 {
        let mut client = TcpStream::connect(&server.addr).expect("connect slow client");
        write!(client, "POST /api HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: 100000\r\n\r\n{{", server.addr, TOKEN).expect("send incomplete body");
        stalled.push(client);
    }
    std::thread::sleep(Duration::from_secs(2));
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            sender
                .send(server.call(METHODS[0], json!([])).status)
                .expect("send status");
        });
        let status = receiver.recv_timeout(Duration::from_secs(3));
        drop(stalled);
        assert_eq!(
            status.expect("another client must not wait for the stalled bodies"),
            200
        );
    });
}

fn assert_transport_error(response: &Response, status: u16, code: i32, kind: &str) {
    assert_eq!(response.status, status, "{}", response.body);
    assert_eq!(response.header("Content-Type"), Some("application/json"));
    assert_eq!(response.header("Connection"), Some("close"));
    assert_eq!(
        response.header("Content-Length"),
        Some(response.body.len().to_string().as_str())
    );
    let document = response.json();
    assert_eq!(document["statusCode"], status);
    assert_eq!(document["code"], code);
    assert_eq!(document["data"]["kind"], kind);
    assert_eq!(document["data"]["http_status"], status);
    assert!(document["message"].is_string());
}

#[test]
fn expect_continue_is_sent_after_the_transport_guard_passes() {
    let sandbox = Sandbox::new("expect-continue");
    let server = Server::start(&sandbox.graph_root(), "expect-continue");
    let mut stream = TcpStream::connect(&server.addr).expect("connect expecting client");
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("set timeout");
    let body = json!({"method": METHODS[0], "args": [], "id": 7}).to_string();
    write!(stream, "POST /api HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\nExpect: 100-continue\r\n\r\n", server.addr, TOKEN, body.len()).expect("send request head");
    let mut interim = [0; 25];
    stream
        .read_exact(&mut interim)
        .expect("read interim response before sending body");
    assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
    stream.write_all(body.as_bytes()).expect("send body");
    let mut bytes = Vec::new();
    stream
        .read_to_end(&mut bytes)
        .expect("read response and close");
    let response = Response::parse(&bytes);
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(
        response.json().get("id").is_none(),
        "the request id is not echoed: {}",
        response.body
    );
    assert_eq!(response.header("Content-Type"), Some("application/json"));
    assert_eq!(response.header("Connection"), Some("close"));

    let chunked = format!("{:x}\r\n{body}\r\n0\r\n\r\n", body.len());
    let response = server.raw_headers(
        "POST",
        "/api",
        &chunked,
        &format!(
            "Host: {}\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nTransfer-Encoding: chunked\r\n",
            server.addr, TOKEN
        ),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(
        response.json().get("id").is_none(),
        "the request id is not echoed: {}",
        response.body
    );

    let rejected = server.raw_headers(
        "POST",
        "/api",
        "",
        "Host: attacker.example\r\nContent-Type: application/json\r\nExpect: 100-continue\r\n",
    );
    assert_transport_error(&rejected, 403, -32600, "invalid-request");
}

#[test]
fn excess_stalled_connections_receive_a_quick_json_rpc_503() {
    let sandbox = Sandbox::new("connection-cap");
    let server = Server::start(&sandbox.graph_root(), "connection-cap");
    let mut stalled = Vec::new();
    for _ in 0..64 {
        let mut client = TcpStream::connect(&server.addr).expect("connect slow client");
        write!(client, "POST /api HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: 100000\r\n\r\n{{", server.addr, TOKEN).expect("send incomplete body");
        stalled.push(client);
    }
    std::thread::sleep(Duration::from_secs(2));
    let start = Instant::now();
    let response = server.call(METHODS[0], json!([]));
    let elapsed = start.elapsed();
    drop(stalled);
    assert_transport_error(&response, 503, -32003, "unavailable");
    assert!(elapsed < Duration::from_secs(1), "503 took {elapsed:?}");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let response = server.call(METHODS[0], json!([]));
        if response.status == 200 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "connection slots were not released"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn transport_guards_check_the_head_and_token_before_dispatch() {
    let sandbox = Sandbox::new("transport-guards");
    let server = Server::start(&sandbox.graph_root(), "transport-guards");
    let page = sandbox.graph_root().join("pages/alpha.md");
    let before = fs::read(&page).expect("read original page");
    let body = json!({"method": APPEND_BLOCK, "args": ["alpha", "transport test"]}).to_string();
    let host = &server.addr;
    let port = host.rsplit_once(':').expect("bound port").1;
    let cases = [
        (format!("Host: attacker.example:{port}\r\nContent-Type: application/json\r\n"), 403),
        ("Host: localhost:0\r\nContent-Type: application/json\r\n".to_owned(), 403),
        (format!("Host: {host}\r\nOrigin: http://{host}\r\nOrigin: http://{host}\r\nContent-Type: application/json\r\n"), 403),
        (format!("Host: {host}\r\nContent-Type: text/plain\r\n"), 415),
        (format!("Host: {host}\r\n"), 415),
        (format!("Host: {host}\r\nContent-Type: application/json\r\nContent-Type: text/plain\r\n"), 415),
    ];
    for (headers, status) in cases {
        let response = server.raw_headers("POST", "/api", &body, &headers);
        assert_eq!(response.status, status, "{headers}: {}", response.body);
        assert_transport_error(&response, status, -32600, "invalid-request");
        if status == 403 {
            assert_eq!(
                response.json()["error"],
                json!("Forbidden"),
                "the Fastify `error` member names the status: {}",
                response.body
            );
        }
        assert_eq!(fs::read(&page).expect("read refused page"), before);
    }
    // A foreign `Origin` is no refusal on its own (ADR-0018): without the token the call is an
    // ordinary 401 before dispatch, whatever scheme or authority the page runs from.
    for origin in [
        "https://untrusted.example".to_owned(),
        "null".to_owned(),
        format!("http://localhost:{port}"),
        "chrome-extension://abcdefghijklmnop".to_owned(),
    ] {
        let refused = server.raw_headers(
            "POST",
            "/api",
            &body,
            &format!("Host: {host}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\n"),
        );
        assert_transport_error(&refused, 401, -32001, "unauthorized");
        assert_eq!(fs::read(&page).expect("read refused page"), before);
        // With the token the same origin is answered, exactly as the official `origin "*"`
        // policy lets the Logseq Copilot extension call the surface.
        let answered = server.raw_headers(
            "POST",
            "/api",
            &json!({"method": METHODS[1], "args": options(json!({}))}).to_string(),
            &format!("Host: {host}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nAuthorization: Bearer {TOKEN}\r\n"),
        );
        assert_eq!(answered.status, 200, "{origin}: {}", answered.body);
        assert!(
            answered.json()["pages"].is_array(),
            "a real read is answered for {origin}: {}",
            answered.body
        );
    }
    let empty = server.raw_headers(
        "POST",
        "/api",
        "",
        &format!(
            "Host: {host}\r\nContent-Type: application/json\r\nAuthorization: Bearer {TOKEN}\r\n"
        ),
    );
    assert_eq!(empty.status, 400);
    assert_eq!(fs::read(&page).expect("read unchanged page"), before);
    server
        .call(APPEND_BLOCK, json!(["alpha", "non-browser JSON"]))
        .result();
    server.raw_headers("POST", "/api", &body, &format!("Host: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Type: Application/JSON; charset=utf-8\r\nAuthorization: Bearer {TOKEN}\r\n")).result();
    let after = fs::read_to_string(page).expect("read appended page");
    assert!(after.contains("non-browser JSON"));
    assert!(after.contains("transport test"));
}

// ------------------------------------------------------------------------------------------------
// #2 /pages ↔ logseq.Editor.getAllPages — deliberate-difference (ADR-0008). Every request
// declares exactly one freshness policy, defaulting to `last`; the official API has no freshness
// concept. `check` retains the revision on an unchanged graph; `verify` builds and publishes a
// complete shadow projection with a higher revision.
// ------------------------------------------------------------------------------------------------
#[test]
fn freshness_is_one_policy_per_request() {
    let sandbox = Sandbox::new("freshness");
    let server = Server::start(&sandbox.graph_root(), "freshness");

    let error = server
        .call(METHODS[1], options(json!({"freshness": "bogus"})))
        .expect_code(400, -32602);
    assert_eq!(error["data"]["parameter"], json!("freshness"), "{error}");
    assert_eq!(
        error["data"]["allowed"],
        json!(["last", "check", "verify"]),
        "an agent can read the legal values: {error}"
    );

    let last = server
        .call(METHODS[1], options(json!({"freshness": "last"})))
        .result();
    let check = server
        .call(METHODS[1], options(json!({"freshness": "check"})))
        .result();
    let verify = server
        .call(METHODS[1], options(json!({"freshness": "verify"})))
        .result();
    assert_eq!(sandbox.revision_of(&last), sandbox.revision_of(&check));
    assert!(sandbox.revision_of(&verify) > sandbox.revision_of(&check));

    // Two options objects would be two policies in one request; ADR-0008 allows exactly one.
    let duplicate = server
        .call(
            METHODS[1],
            json!([{"freshness": "last"}, {"freshness": "check"}]),
        )
        .expect_code(400, -32602);
    assert_eq!(
        duplicate["data"]["kind"],
        json!("multiple-policies"),
        "{duplicate}"
    );
}

// A polling watcher is transport-independent infrastructure: the method and answer shapes stay
// unchanged, while an external file edit becomes visible to the default `last` policy.
#[test]
fn an_external_edit_reaches_the_default_served_read() {
    let sandbox = Sandbox::new("poll-update");
    let server = Server::spawn_with_poll_interval(&sandbox.graph_root(), "poll-update", 10);
    server.wait_until_ready("poll-update");

    sandbox.write("pages/alpha.md", "- externally changed\n\t- second block\n");

    let deadline = Instant::now() + TIMEOUT;
    loop {
        let tree = server.call(METHODS[4], json!(["alpha"])).result();
        if tree["blocks"][0]["text"] == json!("externally changed") {
            assert_eq!(tree["blocks"].as_array().expect("blocks").len(), 2);
            assert_eq!(tree["stale"], json!(false));
            return;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not project the edit"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

// A failed poll makes `last` fail closed, never silently stale. The next successful poll clears the
// condition; no request needs to repair it with `check`.
#[test]
fn a_failed_poll_fails_closed_and_a_later_poll_recovers() {
    let sandbox = Sandbox::new("poll-failure");
    let server = Server::spawn_with_poll_interval(&sandbox.graph_root(), "poll-failure", 10);
    server.wait_until_ready("poll-failure");

    fs::write(
        sandbox.graph_root().join("pages/alpha.md"),
        b"- old\n\xff\n",
    )
    .expect("write invalid UTF-8");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let response = server.call(METHODS[4], json!(["alpha"]));
        if response.status == 500 {
            let error = response.expect_code(500, -32603);
            assert!(error["message"]
                .as_str()
                .expect("message")
                .contains("UTF-8"));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the failed scan was not reported"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    sandbox.write("pages/alpha.md", "- repaired by another process\n");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let response = server.call(METHODS[4], json!(["alpha"]));
        if response.status == 200 {
            let tree = response.result();
            assert_eq!(
                tree["blocks"][0]["text"],
                json!("repaired by another process")
            );
            assert_eq!(tree["stale"], json!(false));
            return;
        }
        assert!(
            Instant::now() < deadline,
            "a successful poll did not clear staleness; last response: {}",
            response.body
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

// The watcher owns another engine handle, so a background scan cannot hold the request-state mutex.
// Poll frequently while requests race it; even a request that overlaps a scan must answer promptly.
#[test]
fn default_reads_are_not_blocked_by_a_background_scan() {
    let sandbox = Sandbox::new("poll-concurrency");
    sandbox.write_filler_pages(1_500);
    let server = Server::spawn_with_poll_interval(&sandbox.graph_root(), "poll-concurrency", 1);
    server.wait_until_ready("poll-concurrency");

    let mut slowest = Duration::ZERO;
    for _ in 0..30 {
        let started = Instant::now();
        let result = server.call(METHODS[0], json!([])).result();
        assert_eq!(result["scanning"], json!(false));
        assert_eq!(result["stale"], json!(false));
        slowest = slowest.max(started.elapsed());
    }
    assert!(
        slowest < Duration::from_secs(1),
        "a default read waited for a background scan: {slowest:?}"
    );
}

// ------------------------------------------------------------------------------------------------
// #4 /page ↔ logseq.Editor.getPage — deliberate-difference (the matrix row is not a match on the
// whole answer). The measured official name rule resolves a `logseq.` name by the snake form of
// its leaf (`logseq.Editor.getPage` and `logseq.getPage` are both `get_page`), and a bare name as
// written; ours applies it to reads while `logseq.Graph.*` writes keep their exact names.
// ------------------------------------------------------------------------------------------------
#[test]
fn method_names_with_and_without_the_sub_namespace_are_equivalent() {
    let sandbox = Sandbox::new("namespace");
    let server = Server::start(&sandbox.graph_root(), "namespace");
    let without_scan_time = |mut answer: Value| {
        let last_scan = answer.get_mut("last_scan").expect("last_scan is present");
        assert!(last_scan.is_string(), "last_scan is a timestamp string");
        *last_scan = Value::Null;
        answer
    };

    let full = server
        .call("logseq.Editor.getPage", json!(["alpha"]))
        .result();
    let short = server.call("logseq.getPage", json!(["alpha"])).result();
    assert_eq!(
        without_scan_time(full),
        without_scan_time(short),
        "one method, two spellings"
    );

    let own_full = server.call("logseq.Graph.getFavorites", json!([])).result();
    let own_short = server.call("logseq.getFavorites", json!([])).result();
    assert_eq!(
        without_scan_time(own_full),
        without_scan_time(own_short),
        "the short form covers our own methods too"
    );

    let error = server
        .call("logseq.Editor.nope", json!([]))
        .expect_code(400, -32601);
    assert_eq!(error["data"]["kind"], json!("method-not-found"), "{error}");
    assert_eq!(
        error["data"]["method"],
        json!("logseq.Editor.nope"),
        "{error}"
    );
    let known = error["data"]["methods"]
        .as_array()
        .expect("the known methods");
    assert_eq!(
        known.len(),
        METHODS.len(),
        "an agent can discover the surface: {error}"
    );
    assert!(known.contains(&json!("logseq.Editor.getPage")), "{error}");
}

// ------------------------------------------------------------------------------------------------
// #2 /pages ↔ logseq.Editor.getAllPages — deliberate-difference. The thirteen REST GET endpoints
// are deleted: only `POST /api` is answered, and it answers exactly one call (no batch). Errors
// are JSON-RPC objects with `code`/`message`/`data`, not HTTP status lines with a one-line body.
// ------------------------------------------------------------------------------------------------
#[test]
fn rest_endpoints_are_gone_and_only_post_api_is_answered() {
    let sandbox = Sandbox::new("routing");
    let server = Server::start(&sandbox.graph_root(), "routing");

    for (verb, path) in [
        ("GET", "/pages"),
        ("GET", "/info"),
        ("POST", "/query"),
        ("GET", "/"),
    ] {
        let error = server.raw(verb, path, "").expect_code(400, -32600);
        assert_eq!(error["data"]["kind"], json!("invalid-request"), "{error}");
    }

    let not_json = server.post("this is not JSON").expect_code(400, -32700);
    assert_eq!(not_json["data"]["kind"], json!("parse-error"), "{not_json}");

    let batch = server
        .call_object(json!([{"method": METHODS[1], "args": []}]))
        .expect_code(400, -32600);
    assert_eq!(batch["data"]["kind"], json!("invalid-request"), "{batch}");

    let unknown_field = server
        .call_object(json!({"method": METHODS[1], "args": [], "freshness": "check"}))
        .expect_code(400, -32600);
    assert_eq!(
        unknown_field["data"]["field"],
        json!("freshness"),
        "{unknown_field}"
    );
}

// ------------------------------------------------------------------------------------------------
// #4 /page ↔ logseq.Editor.getPage and #13 /block ↔ logseq.Editor.getBlock —
// deliberate-difference. The former 404/400/409/500/503 HTTP error model is migrated item by item
// into JSON-RPC error objects whose `data` carries the former status, a machine-readable `kind`,
// and the facts an agent needs (which resource, which name, which values).
// ------------------------------------------------------------------------------------------------
#[test]
fn the_old_http_error_model_is_a_structured_error_object() {
    let sandbox = Sandbox::new("errors");
    let server = Server::start(&sandbox.graph_root(), "errors");

    for method in [METHODS[3], METHODS[4]] {
        let error = server
            .call(method, json!(["nope"]))
            .expect_code(404, -32004);
        assert_eq!(error["data"]["kind"], json!("not-found"), "{error}");
        assert_eq!(error["data"]["resource"], json!("page"), "{error}");
        assert_eq!(error["data"]["name"], json!("nope"), "{error}");
        assert_eq!(error["data"]["http_status"], json!(404), "{error}");
    }

    let missing = server
        .call(METHODS[12], json!(["fedcba98-7654-3210-fedc-ba9876543210"]))
        .expect_code(404, -32004);
    assert_eq!(missing["data"]["resource"], json!("block"), "{missing}");

    let bad_arg = server.call(METHODS[3], json!([])).expect_code(400, -32602);
    assert_eq!(
        bad_arg["data"]["kind"],
        json!("invalid-params"),
        "{bad_arg}"
    );
    assert_eq!(bad_arg["data"]["expected"], json!(1), "{bad_arg}");
    assert_eq!(bad_arg["data"]["actual"], json!(0), "{bad_arg}");

    let unknown_option = server
        .call(METHODS[3], with_options("alpha", json!({"limit": 2})))
        .expect_code(400, -32602);
    assert_eq!(
        unknown_option["data"]["parameter"],
        json!("limit"),
        "{unknown_option}"
    );
    assert_eq!(
        unknown_option["data"]["allowed"],
        json!(["freshness"]),
        "the option is refused with the legal set: {unknown_option}"
    );

    // The `id` a caller supplies is ignored: success answers the bare value and failure the
    // Fastify shape, so no request member is echoed (ADR-0018).
    let with_id = server
        .call_object(json!({"id": 7, "method": METHODS[3], "args": ["alpha"]}))
        .result();
    assert_eq!(with_id["page"]["name"], json!("alpha"), "{with_id}");
    let echoed = server
        .call_object(json!({"id": 7, "method": METHODS[3], "args": ["nope"]}))
        .json();
    assert!(
        echoed.get("id").is_none(),
        "the request id is not echoed: {echoed}"
    );
    assert_eq!(echoed["statusCode"], json!(404), "{echoed}");
}

// ------------------------------------------------------------------------------------------------
// #1 /info ↔ logseq.Editor.getCurrentGraph — out-of-scope: port release has no official
// counterpart. ADR-0008 binds the socket before the startup scan and the accept loop owns it until
// the process dies; the official API specifies no port-release contract.
// ------------------------------------------------------------------------------------------------
#[test]
fn the_port_is_released_after_the_server_stops() {
    let sandbox = Sandbox::new("stop");
    let server = Server::start(&sandbox.graph_root(), "stop");
    let addr = server.addr.clone();

    server.call(METHODS[0], json!([])).result();
    drop(server);

    let deadline = Instant::now() + TIMEOUT;
    loop {
        match TcpListener::bind(&addr) {
            Ok(listener) => {
                drop(listener);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                assert!(
                    Instant::now() < deadline,
                    "the port {addr} was not released after the server stopped"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("binding {addr}: {error}"),
        }
    }
}

// ------------------------------------------------------------------------------------------------
// #9 /search ↔ logseq.App.search — deliberate-difference: `pages` carries the official shape
// (page names as strings) and the name/`alias::` candidates live beside it in `page_candidates`,
// which are spellings of file pages, not entities (ADR-0001, ADR-0002).
// ------------------------------------------------------------------------------------------------
#[test]
fn app_search_answers_page_candidates_beside_hits() {
    let sandbox = Sandbox::new("search-pages");
    sandbox.write(
        "pages/ADR.md",
        "alias:: 架构决策记录, ADRs\n\n- a decision\n",
    );
    sandbox.write("pages/紫米.md", "alias:: zmi\n");
    sandbox.write("pages/酷态科.md", "alias:: 紫米, ZMI\n");
    let server = Server::start(&sandbox.graph_root(), "search-pages");

    // A shared alias answers two pages, each stating how it matched. The field is always there,
    // including when nothing answers.
    let answer = server.call(METHODS[8], json!(["zmi"])).result();
    let pages = answer["page_candidates"].as_array().expect("a page array");
    assert_eq!(pages.len(), 2, "one entry per page: {answer}");
    assert!(
        pages.iter().all(|page| page["via"] == "alias"),
        "both answered through an alias: {answer}"
    );
    assert!(
        pages.iter().all(|page| page["alias"].as_str().is_some()),
        "an alias hit states the alias: {answer}"
    );

    // A page's own name comes first and states no alias.
    let named = server.call(METHODS[8], json!(["ADR"])).result();
    let pages = named["page_candidates"].as_array().expect("a page array");
    assert_eq!(pages[0]["name"], "ADR", "{named}");
    assert_eq!(pages[0]["via"], "name", "{named}");
    assert!(pages[0]["alias"].is_null(), "{named}");

    let empty = server
        .call(METHODS[8], json!(["nothing-answers-this"]))
        .result();
    assert_eq!(
        empty["page_candidates"].as_array().map(Vec::len),
        Some(0),
        "the field is always present: {empty}"
    );
    assert_eq!(
        empty["pages"].as_array().map(Vec::len),
        Some(0),
        "the official `pages` list is always present too: {empty}"
    );
}

// ------------------------------------------------------------------------------------------------
// Envelope authentication (ADR-0018). These cases pin a transport rule, not a read, so they name
// no parity row: the official server applies the same rule to every method, and the matrix
// classifies methods rather than their envelope. The accepted and refused `Authorization` values
// are the ones the official `server-auth-test` table records.
// ------------------------------------------------------------------------------------------------
#[test]
fn a_request_without_a_token_is_refused_before_any_method() {
    let sandbox = Sandbox::new("auth");
    let server = Server::start(&sandbox.graph_root(), "auth");
    let body = json!({"method": METHODS[0], "args": []}).to_string();

    for authorization in [
        None,
        Some(""),
        Some("Bearer"),
        Some("Bearer "),
        Some("Basic test-token-alpha"),
        Some("prefix Bearer test-token-alpha"),
        Some("Bearer test-token-alpha extra"),
        Some("Bearer wrong-token"),
    ] {
        let response = server.post_with(&body, authorization);
        assert!(
            !response.body.contains(TOKEN),
            "a refusal must not echo the token: {}",
            response.body
        );
        let error = response.expect_code(401, -32001);
        assert_eq!(error["data"]["kind"], json!("unauthorized"), "{error}");
        assert_eq!(error["data"]["http_status"], json!(401), "{error}");
        assert_eq!(error["error"], json!("Unauthorized"), "{error}");
        assert_eq!(error["message"], json!("Access Denied!"), "{error}");
    }
}

#[test]
fn a_token_list_and_a_case_insensitive_scheme_are_accepted() {
    let sandbox = Sandbox::new("auth-accepted");
    let server = Server::start(&sandbox.graph_root(), "auth-accepted");
    let body = json!({"method": METHODS[0], "args": []}).to_string();

    for authorization in [
        "bearer test-token-alpha",
        "BEARER test-token-alpha",
        "Bearer\ttest-token-alpha",
        "Bearer test-token-beta",
    ] {
        let result = server.post_with(&body, Some(authorization)).result();
        assert_eq!(result["scanning"], json!(false), "{authorization}");
    }
}

#[test]
fn an_unauthorized_write_writes_nothing() {
    let sandbox = Sandbox::new("auth-write");
    let server = Server::start(&sandbox.graph_root(), "auth-write");
    let body =
        json!({"method": "logseq.Graph.createPage", "args": ["unauthorized-page"]}).to_string();

    let error = server.post_with(&body, None).expect_code(401, -32001);
    assert_eq!(error["data"]["kind"], json!("unauthorized"), "{error}");
    assert!(
        !sandbox
            .graph_root()
            .join("pages/unauthorized-page.md")
            .exists(),
        "an unauthorized write must not reach the library"
    );
}

#[test]
fn serve_refuses_to_start_without_a_token() {
    let sandbox = Sandbox::new("auth-missing");
    let output = Command::new(BIN)
        .arg("--graph")
        .arg(sandbox.graph_root())
        .arg("--index")
        .arg(Sandbox::index_path(&sandbox.graph_root(), "auth-missing"))
        .args(["serve", "--bind", "127.0.0.1:0"])
        .env_remove("LOGSEQ_API_TOKEN")
        .output()
        .expect("run serve without a token");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("no API token configured"), "{stderr}");
    assert!(
        !stderr.contains(TOKEN),
        "the refusal must not echo a token: {stderr}"
    );
}

// ------------------------------------------------------------------------------------------------
// CORS — the official server registers `@fastify/cors` with `origin "*"`. The Logseq Copilot
// extension fetches with `mode: "cors"` and an `Authorization` header from a
// `chrome-extension://<id>` origin, so the preflight and the origin header are what let a browser
// client call this surface at all. Without them the browser blocks every call before the POST.
// ------------------------------------------------------------------------------------------------
#[test]
fn a_cors_preflight_is_answered_before_the_token() {
    let sandbox = Sandbox::new("cors-preflight");
    let server = Server::start(&sandbox.graph_root(), "cors-preflight");
    // The preflight carries no Authorization header, exactly as a browser sends it.
    let response = server.raw_with("OPTIONS", "/api", "", None);
    assert_eq!(
        response.status, 204,
        "a preflight is answered, not refused: {}",
        response.body
    );
    assert_eq!(response.header("access-control-allow-origin"), Some("*"));
    assert_eq!(
        response.header("access-control-allow-headers"),
        Some("authorization,content-type")
    );
    assert!(
        response.header("access-control-allow-methods").is_some(),
        "the methods are stated"
    );
    assert!(response.body.is_empty(), "a preflight has no body");
}

#[test]
fn an_answer_states_the_cors_origin() {
    let sandbox = Sandbox::new("cors-answer");
    let server = Server::start(&sandbox.graph_root(), "cors-answer");
    let response = server.call(METHODS[1], options(json!({})));
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(
        response.header("access-control-allow-origin"),
        Some("*"),
        "every answer carries the official origin policy"
    );
}

#[test]
fn an_extension_origin_calls_with_the_token() {
    let sandbox = Sandbox::new("cors-extension");
    let server = Server::start(&sandbox.graph_root(), "cors-extension");
    // The Logseq Copilot extension's actual head: `chrome-extension://<id>` plus the bearer token.
    let response = server.raw_headers(
        "POST",
        "/api",
        &json!({"method": METHODS[1], "args": options(json!({}))}).to_string(),
        &format!(
            "Host: {}\r\nOrigin: chrome-extension://abcdefghijklmnop\r\nContent-Type: application/json\r\nAuthorization: Bearer {TOKEN}\r\n",
            server.addr
        ),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(response.json()["pages"].is_array(), "{}", response.body);
    assert_eq!(
        response.header("access-control-allow-origin"),
        Some("*"),
        "the answer is readable by the extension origin"
    );
}

// ------------------------------------------------------------------------------------------------
// #28 logseq.App.checkCurrentIsDBGraph — deliberate-difference. The official client asks which
// edition it is connected to before choosing its method dialect; measured live, the official
// file-edition server refuses the probe with `MethodNotExist: check_current_is_db_graph`, but a
// client asking a file graph should learn `false`, not parse a refusal — and this projection is
// file-first by ADR-0001, so `false` is the answer.
// ------------------------------------------------------------------------------------------------
#[test]
fn check_current_is_db_graph_answers_false_for_a_file_first_projection() {
    let sandbox = Sandbox::new("db-probe");
    let server = Server::start(&sandbox.graph_root(), "db-probe");

    assert_eq!(
        server
            .call("logseq.App.checkCurrentIsDBGraph", json!([]))
            .result(),
        json!(false)
    );
    // The short form resolves the same method, like every other read.
    assert_eq!(
        server
            .call("logseq.checkCurrentIsDBGraph", json!([]))
            .result(),
        json!(false)
    );

    // It takes no positional arguments and declares no policy.
    let policy = server
        .call(
            "logseq.App.checkCurrentIsDBGraph",
            options(json!({"freshness": "check"})),
        )
        .expect_code(400, -32602);
    assert_eq!(
        policy["data"]["kind"],
        json!("no-policy-allowed"),
        "{policy}"
    );

    let arity = server
        .call("logseq.App.checkCurrentIsDBGraph", json!(["extra"]))
        .expect_code(400, -32602);
    assert_eq!(arity["data"]["expected"], json!(0), "{arity}");
    assert_eq!(arity["data"]["actual"], json!(1), "{arity}");
}

// ------------------------------------------------------------------------------------------------
// #20 logseq.showMsg — out-of-scope: there is no GUI (ADR-0003), so the toast is accepted and
// dropped. The answer keeps the official shape (a message-id string) so a client that only checks
// for success can finish connecting, but nothing is displayed and no parity is claimed.
// ------------------------------------------------------------------------------------------------
#[test]
fn show_msg_is_accepted_and_dropped_without_a_gui() {
    let sandbox = Sandbox::new("show-msg");
    let server = Server::start(&sandbox.graph_root(), "show-msg");

    let result = server
        .call("logseq.showMsg", json!(["Logseq Copilot Connect!"]))
        .result();
    assert_eq!(result, json!("notification"));

    // The official call takes an optional status or options argument; this surface accepts the
    // message alone, and states that shape when a caller sends none.
    let arity = server
        .call("logseq.showMsg", json!([]))
        .expect_code(400, -32602);
    assert_eq!(arity["data"]["expected"], json!(1), "{arity}");
    assert_eq!(arity["data"]["actual"], json!(0), "{arity}");
}

// ------------------------------------------------------------------------------------------------
// #21 logseq.App.getAppInfo — out-of-scope: the official method answers the app version and whether
// the app supports DB graphs. This build is the file edition (ADR-0001), so it answers its own
// version and `supportDb: false`; the official client reads `version` for its connection message.
// ------------------------------------------------------------------------------------------------
#[test]
fn get_app_info_states_the_version_and_that_this_is_not_a_db_edition() {
    let sandbox = Sandbox::new("app-info");
    let server = Server::start(&sandbox.graph_root(), "app-info");

    let result = server.call("logseq.App.getAppInfo", json!([])).result();
    assert!(
        result["version"].as_str().is_some_and(|v| !v.is_empty()),
        "{result}"
    );
    assert_eq!(result["supportDb"], json!(false), "{result}");

    let arity = server
        .call("logseq.App.getAppInfo", json!(["extra"]))
        .expect_code(400, -32602);
    assert_eq!(arity["data"]["expected"], json!(0), "{arity}");

    let policy = server
        .call(
            "logseq.App.getAppInfo",
            options(json!({"freshness": "check"})),
        )
        .expect_code(400, -32602);
    assert_eq!(
        policy["data"]["kind"],
        json!("no-policy-allowed"),
        "{policy}"
    );
}
