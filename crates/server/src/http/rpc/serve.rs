use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use logseq_graph::engine::{Graph, OpenOptions, ScanProgress, ScanState};
use serde_json::{json, Value};

use super::call::parse_call;
use super::error::{request_error, RequestError};
use super::state::{Served, State};
use super::watch::WatchStats;
use super::{handle, transport, watch};

fn loopback_authority(authority: &str, port: u16) -> bool {
    if let Ok(address) = authority.parse::<std::net::SocketAddr>() {
        return address.ip().is_loopback() && address.port() == port;
    }
    authority.rsplit_once(':').is_some_and(|(host, value)| {
        host.eq_ignore_ascii_case("localhost") && value.parse::<u16>() == Ok(port)
    }) || (port == 80 && authority.eq_ignore_ascii_case("localhost"))
}

/// The bearer token in one `Authorization` value, under the rule the official server applies:
/// the scheme is case-insensitive, at least one space or tab separates it from the token, and the
/// token is the rest of the value and holds no whitespace. The whole value must match, so a prefix
/// or a suffix is not a header the official server accepts (ADR-0018).
fn bearer_token(authorization: &str) -> Option<&str> {
    let (scheme, rest) = authorization.split_at_checked(6)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    // The separator is required and is a space or tab, not any Unicode whitespace: `Bearerx` is a
    // different scheme, and the official rule does not fold NBSP into the separator.
    let rest = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('\t'))?;
    let token = rest.trim_start_matches([' ', '\t']);
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return None;
    }
    Some(token)
}

pub(super) fn transport_guard(
    request: &transport::Head,
    port: u16,
    tokens: &[String],
) -> Result<(), (u16, &'static str)> {
    let single = |name: &'static str| {
        let mut headers = request
            .headers
            .iter()
            .filter(|(field, _)| field.eq_ignore_ascii_case(name));
        let value = headers.next().map(|(_, value)| value.as_str());
        if headers.next().is_some() {
            None
        } else {
            value
        }
    };
    if single("Host")
        .filter(|host| loopback_authority(host, port))
        .is_none()
    {
        return Err((
            403,
            "Host must be a loopback address or localhost at the bound port",
        ));
    }
    // The bearer token below, not the `Origin` header, is the cross-origin gate: the official
    // server answers `Access-Control-Allow-Origin: *`, and an extension page calls this surface
    // from a `chrome-extension://` origin (ADR-0018). Any origin may therefore POST, but a head
    // carrying more than one `Origin` is still ambiguous and refused.
    let origins = request
        .headers
        .iter()
        .filter(|(field, _)| field.eq_ignore_ascii_case("Origin"))
        .count();
    if origins > 1 {
        return Err((403, "a request carries at most one Origin"));
    }
    if request.method == "POST"
        && request.path == "/api"
        && !single("Content-Type").is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return Err((415, "POST /api requires Content-Type: application/json"));
    }
    // Authentication runs after the header guards but still before the body is read: a request
    // without a token learns one fact — 401 — and nothing about which methods or arguments exist
    // (ADR-0018). The token is never echoed in a message, a log line, or an error body.
    let authorization = request
        .headers
        .iter()
        .find(|(field, _)| field.eq_ignore_ascii_case("Authorization"))
        .map(|(_, value)| value.as_str());
    let authorized = authorization
        .and_then(bearer_token)
        .is_some_and(|token| tokens.iter().any(|valid| valid == token));
    if !authorized {
        return Err((401, "Access Denied!"));
    }
    Ok(())
}

/// Answer one request after checking its HTTP authority, origin, media type and bearer token.
fn respond(served: &Arc<Served>, request: transport::Request) -> transport::Response {
    // A browser sends a CORS preflight before a cross-origin POST that carries an `Authorization`
    // header, and a preflight never carries the token. `read_request` skipped the guards for it;
    // answer the official policy here, before anything else (ADR-0018).
    if request.head.method == "OPTIONS" {
        return transport::Response::empty(204);
    }
    let http_method = request.head.method;
    let path = request.head.path;
    let body = request.body;

    let outcome: Result<Value, RequestError> = (|| {
        if http_method != "POST" {
            return Err(request_error(
                format!("only POST /api is answered, not {http_method}"),
                json!({"method": http_method, "expected": "POST"}),
            ));
        }
        if path != "/api" {
            return Err(request_error(
                format!("only POST /api is answered, not {path}"),
                json!({"path": path, "expected": "/api"}),
            ));
        }
        let value: Value = serde_json::from_slice(&body)
            .map_err(|error| RequestError::Parse(error.to_string()))?;
        let call = parse_call(value)?;
        handle(served, &call)
    })();

    // The success body is the official shape: the answer value alone, with no envelope. An official
    // client parses the whole body as its result (`resp.json()` then `data.blocks`), so wrapping it
    // would break every browser client while looking correct in a terminal.
    let (status, document) = match outcome {
        Ok(result) => (200, result),
        Err(error) => (error.status(), error.error_body()),
    };

    transport::Response::json(status, document)
}

/// Bind `bind`, then index the graph in the background and serve reads until the process is killed.
///
/// The socket is bound first and the projection runs on its own thread, so the port answers for the
/// whole time it is indexing (ADR-0008): `getCurrentGraph` states the scan's progress and every
/// other method is refused until the graph is open, and a served read never repeats the cold-start
/// scan after that, it only names a freshness policy. The one diagnostic line goes to standard error
/// as soon as the socket is bound, so a caller that launched the process learns the bound address
/// even when it asked the kernel for port 0. That line announces the socket, not a ready graph: a
/// caller that needs an answer polls `getCurrentGraph` until it stops saying `scanning`. There is
/// no access log: a request line printed per request is a logging framework waiting to happen.
pub(crate) fn serve(
    graph_root: &Path,
    index_path: &Path,
    bind: &str,
    poll_interval: Duration,
    no_watch: bool,
    tokens: Vec<String>,
) -> Result<(), String> {
    // Fail closed, and before the socket is bound: a server that cannot authenticate anyone must
    // not look like a server that is ready to answer (ADR-0018).
    if tokens.is_empty() {
        return Err("no API token configured: pass --api-token or set LOGSEQ_API_TOKEN".to_owned());
    }
    let bound: std::net::SocketAddr = bind
        .to_socket_addrs()
        .map_err(|error| format!("could not resolve {bind}: {error}"))?
        .next()
        .ok_or_else(|| format!("could not resolve {bind}: no address"))?;
    if !bound.ip().is_loopback() {
        return Err(format!(
            "{bind} is not a loopback address; the read surface serves the machine it runs on"
        ));
    }
    let server = std::net::TcpListener::bind(bound)
        .map_err(|error| format!("could not serve on {bind}: {error}"))?;
    let addr = server
        .local_addr()
        .map_err(|error| format!("could not read bound address: {error}"))?;
    eprintln!("listening on {addr}");

    let served = Arc::new(Served {
        state: Mutex::new(State::Scanning),
        graph_root: graph_root.to_owned(),
        index_path: index_path.to_owned(),
        progress: Arc::new(ScanProgress::new()),
        watch: Arc::new(WatchStats::default()),
    });
    let tokens = Arc::new(tokens);
    let scan_state = Arc::new(ScanState::new());

    // The startup projection runs here, on its own thread, and lands the graph in `state` when it is
    // done. A failure is published rather than exiting: the port is bound, so the caller can be told
    // why it will never be ready instead of inferring a crash from a refused connection.
    let scan = Arc::clone(&served);
    let root = graph_root.to_owned();
    let index = index_path.to_owned();
    let watcher_root = graph_root.to_owned();
    let watcher_index = index_path.to_owned();
    let startup_scan_state = Arc::clone(&scan_state);
    let watch_stats = Arc::clone(&served.watch);
    let _scan = std::thread::spawn(move || {
        let options = OpenOptions::new(root, index)
            .with_startup_scan(true)
            .with_scan_state(startup_scan_state);
        let opened = Graph::open_with_progress(options, Arc::clone(&scan.progress))
            .and_then(logseq_graph::engine::SharedGraph::new);
        let reached = match opened {
            Ok(graph) => {
                spawn_watcher(
                    watcher_root,
                    watcher_index,
                    poll_interval,
                    scan_state,
                    no_watch,
                    watch_stats,
                );
                State::Ready(Arc::new(graph))
            }
            Err(error) => {
                let reason = error.to_string();
                eprintln!("could not open the graph: {reason}");
                State::Failed(reason)
            }
        };
        #[expect(
            clippy::expect_used,
            reason = "only a thread that panicked while holding the lock poisons it, and this \
                      process answers that by stopping: there is no other state to fall back to"
        )]
        let mut state = scan
            .state
            .lock()
            .expect("the graph state lock is not poisoned");
        *state = reached;
    });

    transport::serve(server, tokens, move |request| respond(&served, request))
        .map_err(|error| format!("could not accept a connection: {error}"))
}

/// Refresh on a separate engine handle, never through the request state's mutex.
fn spawn_watcher(
    graph_root: PathBuf,
    index_path: PathBuf,
    interval: Duration,
    scan_state: Arc<ScanState>,
    no_watch: bool,
    stats: Arc<WatchStats>,
) {
    watch::spawn(
        graph_root, index_path, interval, scan_state, no_watch, stats,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorities_require_literal_loopback_and_the_bound_port() {
        for authority in [
            "localhost:8765",
            "LOCALHOST:8765",
            "127.0.0.1:8765",
            "[::1]:8765",
        ] {
            assert!(loopback_authority(authority, 8765), "{authority}");
        }
        for authority in [
            "",
            "localhost",
            "localhost:80",
            "example.com:8765",
            "localhost.example:8765",
            "0.0.0.0:8765",
            "[::]:8765",
            "user@localhost:8765",
            "127.0.0.1:8765/path",
        ] {
            assert!(!loopback_authority(authority, 8765), "{authority}");
        }
        assert!(loopback_authority("localhost", 80));
    }
}
