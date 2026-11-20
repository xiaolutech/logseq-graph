//! Bounded, deadline-limited HTTP I/O, isolated from the request-handling slots.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const MAX_CONNECTIONS: usize = 64;
const REQUEST_WORKERS: usize = 16;

#[derive(Clone, Copy)]
struct Limits {
    idle: Duration,
    read: Duration,
    write: Duration,
    head: usize,
    body: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(5),
            read: Duration::from_secs(15),
            write: Duration::from_secs(15),
            head: 64 * 1024,
            body: 16 * 1024 * 1024,
        }
    }
}

pub(super) struct Head {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) headers: Vec<(String, String)>,
}

pub(super) struct Request {
    pub(super) head: Head,
    pub(super) body: Vec<u8>,
}

pub(super) struct Response {
    status: u16,
    body: Vec<u8>,
}

impl Response {
    pub(super) fn json(status: u16, document: Value) -> Self {
        Self {
            status,
            body: document.to_string().into_bytes(),
        }
    }

    /// An answer with no body: the CORS preflight's `204 No Content`.
    pub(super) fn empty(status: u16) -> Self {
        Self {
            status,
            body: Vec::new(),
        }
    }

    fn rejection(status: u16, message: &str) -> Self {
        // The official Fastify error shape plus this surface's `code` and `data`, the same body
        // `RequestError::error_body` builds for a refused call; a 401 is that error verbatim.
        let body = if status == 401 {
            super::error::RequestError::Unauthorized.error_body()
        } else {
            let (code, kind) = if status == 503 {
                (-32003, "unavailable")
            } else {
                (-32600, "invalid-request")
            };
            json!({
                "statusCode": status,
                "error": super::error::reason_phrase(status),
                "message": message,
                "code": code,
                "data": {"kind": kind, "http_status": status}
            })
        };
        Self::json(status, body)
    }

    fn head(&self) -> String {
        // The same table the Fastify `error` member reads, so the status line and the body agree.
        let reason = match super::error::reason_phrase(self.status) {
            "Error" => "Internal Server Error",
            phrase => phrase,
        };
        // The CORS headers the official server states on every answer (`@fastify/cors`,
        // `origin "*"`); a browser client cannot read the response without them.
        format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Access-Control-Allow-Origin: *\r\n\
             Access-Control-Allow-Methods: GET,HEAD,POST\r\n\
             Access-Control-Allow-Headers: authorization,content-type\r\n\
             Vary: Access-Control-Request-Headers\r\n\
             Connection: close\r\n\r\n",
            self.status,
            self.body.len()
        )
    }
}

#[derive(Default)]
struct Slots {
    used: Mutex<usize>,
    available: Condvar,
}

impl Slots {
    fn try_acquire(self: &Arc<Self>, maximum: usize) -> Option<Slot> {
        let mut used = self.used.lock().unwrap_or_else(|error| error.into_inner());
        if *used >= maximum {
            return None;
        }
        *used += 1;
        Some(Slot(Arc::clone(self)))
    }

    fn acquire(self: &Arc<Self>, maximum: usize) -> Slot {
        let mut used = self.used.lock().unwrap_or_else(|error| error.into_inner());
        while *used >= maximum {
            used = self
                .available
                .wait(used)
                .unwrap_or_else(|error| error.into_inner());
        }
        *used += 1;
        Slot(Arc::clone(self))
    }
}

struct Slot(Arc<Slots>);

impl Drop for Slot {
    fn drop(&mut self) {
        let mut used = self
            .0
            .used
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *used -= 1;
        self.0.available.notify_one();
    }
}

pub(super) fn serve(
    listener: TcpListener,
    tokens: Arc<Vec<String>>,
    handler: impl Fn(Request) -> Response + Send + Sync + 'static,
) -> io::Result<()> {
    let port = listener.local_addr()?.port();
    let connections = Arc::new(Slots::default());
    let handlers = Arc::new(Slots::default());
    let handler = Arc::new(handler);
    let mut reported_failure = false;
    for stream in listener.incoming() {
        let stream = match stream {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                if !reported_failure {
                    eprintln!("could not accept a connection: {error}");
                    reported_failure = true;
                }
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            Ok(stream) => stream,
        };
        reported_failure = false;
        let accepted = Instant::now();
        let Some(slot) = connections.try_acquire(MAX_CONNECTIONS) else {
            reject_busy(stream);
            continue;
        };
        let handler = Arc::clone(&handler);
        let handlers = Arc::clone(&handlers);
        let tokens = Arc::clone(&tokens);
        // A failed spawn drops the closure, including its connection slot and socket.
        drop(std::thread::Builder::new().spawn(move || {
            connection(
                stream,
                accepted,
                Limits::default(),
                port,
                slot,
                &handlers,
                &tokens,
                &*handler,
            );
        }));
    }
    Ok(())
}

fn reject_busy(mut stream: TcpStream) {
    let response = Response::rejection(503, "too many concurrent connections");
    best_effort(&mut stream, &response);
    finish_rejection(&mut stream, Duration::ZERO);
}

fn finish_rejection(stream: &mut TcpStream, timeout: Duration) {
    drop(stream.shutdown(Shutdown::Write));
    if timeout.is_zero() {
        if stream.set_nonblocking(true).is_err() {
            return;
        }
    } else if stream.set_nonblocking(false).is_err() {
        return;
    }
    let deadline = Instant::now() + timeout;
    let mut left = 1024 * 1024;
    let mut buffer = [0; 8192];
    while left > 0 {
        if !timeout.is_zero() {
            let Ok(remaining) = remaining(deadline) else {
                break;
            };
            if stream.set_read_timeout(Some(remaining)).is_err() {
                break;
            }
        }
        let count = left.min(buffer.len());
        match stream.read(&mut buffer[..count]) {
            Ok(0) => break,
            Ok(count) => left -= count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

fn best_effort(stream: &mut TcpStream, response: &Response) {
    // A refusal must never stall the accept loop, even when its peer is not reading.
    if stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .is_ok()
        && stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .is_ok()
        && stream.set_nonblocking(true).is_ok()
    {
        let mut pending = [0; 8192];
        drop(stream.read(&mut pending));
        let bytes = [response.head().as_bytes(), &response.body].concat();
        drop(stream.write_all(&bytes));
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "a connection needs its own stream, deadline anchor, bound token, and shared \
              guards; grouping them into a struct would only rename the same eight values"
)]
fn connection(
    mut stream: TcpStream,
    accepted: Instant,
    limits: Limits,
    port: u16,
    _slot: Slot,
    handlers: &Arc<Slots>,
    tokens: &Arc<Vec<String>>,
    handler: &impl Fn(Request) -> Response,
) {
    if stream.set_read_timeout(Some(limits.idle)).is_err()
        || stream.set_write_timeout(Some(limits.write)).is_err()
    {
        return;
    }
    let mut rejected = false;
    let response = match read_request(&mut stream, accepted + limits.read, limits, port, tokens) {
        Ok(request) => {
            let slot = handlers.acquire(REQUEST_WORKERS);
            let response = handler(request);
            drop(slot);
            response
        }
        Err(error) => {
            rejected = true;
            let response = Response::rejection(error.status, error.message);
            if error.status == 408 {
                best_effort(&mut stream, &response);
                finish_rejection(&mut stream, limits.write.min(Duration::from_millis(250)));
                return;
            }
            response
        }
    };
    drop(write_response(&mut stream, &response, limits.write));
    if rejected {
        finish_rejection(&mut stream, limits.write.min(Duration::from_millis(250)));
    } else {
        drop(stream.shutdown(Shutdown::Both));
    }
}

struct Rejection {
    status: u16,
    message: &'static str,
}

impl From<io::Error> for Rejection {
    fn from(error: io::Error) -> Self {
        if matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) {
            Self {
                status: 408,
                message: "request read deadline exceeded",
            }
        } else {
            Self {
                status: 400,
                message: "could not read the complete HTTP request",
            }
        }
    }
}

fn bad_request(message: &'static str) -> Rejection {
    Rejection {
        status: 400,
        message,
    }
}

fn too_large() -> Rejection {
    Rejection {
        status: 413,
        message: "request body exceeds 16 MiB",
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "HTTP deadline exceeded"))
}

struct DeadlineReader<'a> {
    stream: &'a mut TcpStream,
    deadline: Instant,
    idle: Duration,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream
            .set_read_timeout(Some(self.idle.min(remaining(self.deadline)?)))?;
        self.stream.read(buffer)
    }
}

fn line(reader: &mut impl BufRead, maximum: usize) -> Result<Vec<u8>, Rejection> {
    let mut line = Vec::new();
    reader
        .take(maximum as u64 + 1)
        .read_until(b'\n', &mut line)?;
    if line.len() > maximum {
        return Err(Rejection {
            status: 431,
            message: "HTTP head exceeds its size limit",
        });
    }
    if !line.ends_with(b"\r\n") {
        return Err(bad_request("HTTP lines must end with CRLF"));
    }
    Ok(line)
}

fn read_head(reader: &mut impl BufRead, maximum: usize) -> Result<Head, Rejection> {
    let mut bytes = Vec::new();
    loop {
        let line = line(reader, maximum - bytes.len())?;
        let end = line == b"\r\n";
        bytes.extend_from_slice(&line);
        if end {
            break;
        }
    }
    let mut headers = [httparse::EMPTY_HEADER; 128];
    let mut parsed = httparse::Request::new(&mut headers);
    match parsed.parse(&bytes) {
        Ok(httparse::Status::Complete(_)) => {}
        Err(httparse::Error::TooManyHeaders) => {
            return Err(Rejection {
                status: 431,
                message: "too many HTTP headers",
            })
        }
        _ => return Err(bad_request("invalid HTTP request head")),
    }
    let headers = parsed
        .headers
        .iter()
        .map(|header| {
            std::str::from_utf8(header.value)
                .map(|value| {
                    (
                        header.name.to_owned(),
                        value.trim_matches([' ', '\t']).to_owned(),
                    )
                })
                .map_err(|_| bad_request("HTTP headers must be UTF-8"))
        })
        .collect::<Result<_, _>>()?;
    Ok(Head {
        method: parsed
            .method
            .ok_or_else(|| bad_request("missing HTTP method"))?
            .to_owned(),
        path: parsed
            .path
            .ok_or_else(|| bad_request("missing HTTP path"))?
            .to_owned(),
        headers,
    })
}

enum Framing {
    Empty,
    Length(usize),
    Chunked,
}

fn framing(head: &Head) -> Result<Framing, Rejection> {
    let values = |name: &'static str| {
        head.headers
            .iter()
            .filter(move |(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    let lengths: Vec<_> = values("Content-Length").collect();
    let encodings: Vec<_> = values("Transfer-Encoding").collect();
    if lengths.len() > 1 || encodings.len() > 1 || (!lengths.is_empty() && !encodings.is_empty()) {
        return Err(bad_request("ambiguous HTTP body framing"));
    }
    if let Some(length) = lengths.first() {
        if length.is_empty() || !length.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(bad_request("invalid Content-Length"));
        }
        return length
            .parse()
            .map(Framing::Length)
            .map_err(|_| bad_request("invalid Content-Length"));
    }
    if let Some(encoding) = encodings.first() {
        if !encoding.eq_ignore_ascii_case("chunked") {
            return Err(bad_request("Transfer-Encoding must be chunked"));
        }
        return Ok(Framing::Chunked);
    }
    Ok(Framing::Empty)
}

fn read_request(
    stream: &mut TcpStream,
    deadline: Instant,
    limits: Limits,
    port: u16,
    tokens: &Arc<Vec<String>>,
) -> Result<Request, Rejection> {
    let mut reader = BufReader::new(DeadlineReader {
        stream,
        deadline,
        idle: limits.idle,
    });
    let head = read_head(&mut reader, limits.head)?;
    let framing = framing(&head)?;
    // A CORS preflight carries no token and no JSON body: the bearer check and the media-type
    // guard would refuse it before the POST that carries the token could be sent (ADR-0018), so
    // `respond` answers it instead.
    if head.method != "OPTIONS" {
        super::serve::transport_guard(&head, port, tokens)
            .map_err(|(status, message)| Rejection { status, message })?;
    }
    if matches!(framing, Framing::Length(length) if length > limits.body) {
        return Err(too_large());
    }
    let expectations: Vec<_> = head
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("Expect"))
        .map(|(_, value)| value.as_str())
        .collect();
    if !expectations.is_empty() {
        if expectations.len() != 1 || !expectations[0].eq_ignore_ascii_case("100-continue") {
            return Err(bad_request("Expect must be 100-continue"));
        }
        let timeout = limits.write.min(remaining(deadline)?);
        write_bytes(
            reader.get_mut().stream,
            b"HTTP/1.1 100 Continue\r\n\r\n",
            Instant::now() + timeout,
        )?;
    }
    let body = match framing {
        Framing::Empty => Vec::new(),
        Framing::Length(length) => {
            let mut body = Vec::new();
            reader.by_ref().take(length as u64).read_to_end(&mut body)?;
            if body.len() != length {
                return Err(bad_request("could not read the complete HTTP request"));
            }
            body
        }
        Framing::Chunked => read_chunked(&mut reader, limits)?,
    };
    remaining(deadline)?;
    Ok(Request { head, body })
}

fn read_chunked(reader: &mut impl BufRead, limits: Limits) -> Result<Vec<u8>, Rejection> {
    let mut body = Vec::new();
    loop {
        let size_line = line(reader, limits.head)?;
        let size = match httparse::parse_chunk_size(&size_line) {
            Ok(httparse::Status::Complete((used, size))) if used == size_line.len() => size,
            _ => return Err(bad_request("invalid chunk size")),
        };
        if size == 0 {
            let mut trailers = Vec::new();
            loop {
                let line = line(reader, limits.head - trailers.len())?;
                let end = line == b"\r\n";
                trailers.extend_from_slice(&line);
                if end {
                    break;
                }
            }
            let mut headers = [httparse::EMPTY_HEADER; 128];
            if !matches!(
                httparse::parse_headers(&trailers, &mut headers),
                Ok(httparse::Status::Complete(_))
            ) {
                return Err(bad_request("invalid chunk trailers"));
            }
            return Ok(body);
        }
        let available = limits.body - body.len();
        let count = size.min(available as u64 + 1) as usize;
        let end = body.len() + count;
        reader.by_ref().take(count as u64).read_to_end(&mut body)?;
        if body.len() != end {
            return Err(bad_request("could not read the complete HTTP request"));
        }
        if body.len() > limits.body {
            return Err(too_large());
        }
        let mut end = [0; 2];
        reader.read_exact(&mut end)?;
        if end != *b"\r\n" {
            return Err(bad_request("invalid chunk terminator"));
        }
    }
}

fn write_bytes(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "HTTP peer stopped reading",
                ))
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_response(
    stream: &mut TcpStream,
    response: &Response,
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    write_bytes(stream, response.head().as_bytes(), deadline)?;
    write_bytes(stream, &response.body, deadline)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver};

    struct Client {
        stream: TcpStream,
        connections: Arc<Slots>,
        handlers: Arc<Slots>,
        done: Receiver<()>,
        port: u16,
    }

    impl Client {
        fn start(limits: Limits, handler: impl Fn(Request) -> Response + Send + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("address");
            let connections = Arc::new(Slots::default());
            let handlers = Arc::new(Slots::default());
            let slot = connections.try_acquire(1).expect("connection slot");
            let serving = Arc::clone(&handlers);
            let (sender, done) = mpsc::channel();
            std::thread::spawn(move || {
                let (stream, _) = listener.accept().expect("accept");
                connection(
                    stream,
                    Instant::now(),
                    limits,
                    addr.port(),
                    slot,
                    &serving,
                    &Arc::new(vec!["test-token".to_owned()]),
                    &handler,
                );
                sender.send(()).expect("report closed connection");
            });
            let stream = TcpStream::connect(addr).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .expect("write timeout");
            Self {
                stream,
                connections,
                handlers,
                done,
                port: addr.port(),
            }
        }

        fn head(&self, fields: &str) -> String {
            format!("POST /api HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nAuthorization: Bearer test-token\r\n{fields}\r\n", self.port)
        }

        fn response(&mut self) -> (u16, Value) {
            let mut bytes = Vec::new();
            self.stream
                .read_to_end(&mut bytes)
                .expect("read response until EOF");
            let text = String::from_utf8(bytes).expect("UTF-8 response");
            let (head, body) = text.split_once("\r\n\r\n").expect("response head");
            assert!(head.contains("Content-Type: application/json\r\n"));
            assert!(head.ends_with("Connection: close"));
            let status = head
                .split_whitespace()
                .nth(1)
                .expect("status")
                .parse()
                .expect("status number");
            (status, serde_json::from_str(body).expect("JSON response"))
        }

        fn closed(&self) {
            self.done
                .recv_timeout(Duration::from_secs(2))
                .expect("connection thread terminates");
            assert_eq!(*self.connections.used.lock().expect("count"), 0);
            assert_eq!(*self.handlers.used.lock().expect("count"), 0);
        }
    }

    fn tiny_limits() -> Limits {
        Limits {
            idle: Duration::from_millis(80),
            read: Duration::from_millis(200),
            write: Duration::from_millis(80),
            head: 1024,
            body: 64,
        }
    }

    fn echo(request: Request) -> Response {
        Response::json(200, json!({"body": String::from_utf8_lossy(&request.body)}))
    }

    #[test]
    fn stalled_heads_and_bodies_time_out_and_release_their_slots() {
        for body in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let handled = Arc::clone(&calls);
            let mut client = Client::start(tiny_limits(), move |request| {
                handled.fetch_add(1, Ordering::SeqCst);
                echo(request)
            });
            let input = if body {
                client.head("Content-Length: 20\r\n") + "{"
            } else {
                "POST /api HTTP/1.1\r\nHost:".to_owned()
            };
            let start = Instant::now();
            client
                .stream
                .write_all(input.as_bytes())
                .expect("partial request");
            let (status, document) = client.response();
            assert_eq!(status, 408);
            assert_eq!(document["code"], -32600);
            assert_eq!(document["data"]["http_status"], 408);
            assert!(start.elapsed() < Duration::from_secs(1));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            client.closed();
        }
    }

    #[test]
    fn trickling_heads_and_bodies_cannot_extend_the_total_read_deadline() {
        for body in [false, true] {
            let mut limits = tiny_limits();
            limits.idle = Duration::from_millis(300);
            limits.read = Duration::from_millis(120);
            let mut client = Client::start(limits, echo);
            let input = if body {
                client.head("Content-Length: 60\r\n")
            } else {
                "POST /api HTTP/1.1\r\nHost:".to_owned()
            };
            client
                .stream
                .write_all(input.as_bytes())
                .expect("partial request");
            let mut writer = client.stream.try_clone().expect("clone writer");
            let trickle = std::thread::spawn(move || {
                for _ in 0..20 {
                    if writer.write_all(b"x").is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
            let start = Instant::now();
            let (status, _) = client.response();
            assert_eq!(status, 408);
            assert!(
                start.elapsed() < limits.idle,
                "total deadline must beat idle timeout"
            );
            client.closed();
            trickle.join().expect("trickle exits");
        }
    }

    #[test]
    fn a_slow_response_reader_hits_the_write_deadline_and_releases_its_slot() {
        let mut client = Client::start(tiny_limits(), |_| Response {
            status: 200,
            body: vec![b'x'; 32 * 1024 * 1024],
        });
        let head = client.head("Content-Length: 0\r\n");
        let start = Instant::now();
        client
            .stream
            .write_all(head.as_bytes())
            .expect("send request");
        client.closed();
        assert!(start.elapsed() < Duration::from_secs(1));
        let mut buffered = Vec::new();
        client
            .stream
            .read_to_end(&mut buffered)
            .expect("buffered response then EOF");
        assert!(
            buffered.len() < 32 * 1024 * 1024,
            "server must not write the entire body"
        );
    }

    #[test]
    fn ambiguous_or_invalid_body_framing_is_rejected_before_dispatch() {
        for fields in [
            "Content-Length: 1\r\nTransfer-Encoding: chunked\r\n",
            "Content-Length: 1\r\nContent-Length: 1\r\n",
            "Content-Length: 1\r\nContent-Length: 2\r\n",
            "Content-Length: 1, 1\r\n",
            "Content-Length: +1\r\n",
            "Content-Length: -1\r\n",
            "Content-Length: 184467440737095516160\r\n",
            "Transfer-Encoding: gzip\r\n",
            "Transfer-Encoding: gzip, chunked\r\n",
            "Transfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n",
        ] {
            let mut client = Client::start(tiny_limits(), |_| panic!("must not dispatch"));
            let head = client.head(fields);
            client
                .stream
                .write_all(head.as_bytes())
                .expect("send ambiguous head");
            let (status, document) = client.response();
            assert_eq!(status, 400, "{fields}");
            assert_eq!(document["code"], -32600);
            assert_eq!(document["data"]["kind"], "invalid-request");
            client.closed();
        }
    }

    #[test]
    fn empty_length_and_chunked_bodies_are_read_before_dispatch() {
        for (fields, body, expected) in [
            ("", "", ""),
            ("Content-Length: 3\r\n", "abc", "abc"),
            (
                "Transfer-Encoding: chunked\r\n",
                "1;extension=yes\r\na\r\n2\r\nbc\r\n0\r\nX-Trailer: yes\r\n\r\n",
                "abc",
            ),
        ] {
            let mut client = Client::start(tiny_limits(), echo);
            let request = client.head(fields) + body;
            client
                .stream
                .write_all(request.as_bytes())
                .expect("send request");
            let (status, document) = client.response();
            assert_eq!(status, 200);
            assert_eq!(document["body"], expected);
            client.closed();
        }
    }

    #[test]
    fn incomplete_bodies_are_rejected_after_the_peer_closes_its_write_half() {
        for fields in ["Content-Length: 64\r\n", "Transfer-Encoding: chunked\r\n"] {
            let mut client = Client::start(tiny_limits(), |_| panic!("must not dispatch"));
            let input = client.head(fields) + "1";
            client
                .stream
                .write_all(input.as_bytes())
                .expect("partial body");
            client
                .stream
                .shutdown(Shutdown::Write)
                .expect("end partial body");
            assert_eq!(client.response().0, 400);
            client.closed();
        }
    }

    #[test]
    fn early_rejections_preserve_the_json_response_with_unread_body_bytes() {
        for (fields, status) in [
            ("Content-Length: 1000000\r\n", 413),
            (
                "Content-Length: 1000000\r\nTransfer-Encoding: chunked\r\n",
                400,
            ),
            ("Host: attacker.example\r\nContent-Length: 1000000\r\n", 403),
            (
                "Content-Type: text/plain\r\nContent-Length: 1000000\r\n",
                415,
            ),
        ] {
            let mut client = Client::start(tiny_limits(), |_| panic!("must not dispatch"));
            let mut writer = client.stream.try_clone().expect("clone writer");
            let input = client.head(fields) + &"x".repeat(128 * 1024);
            let sending = std::thread::spawn(move || {
                writer
                    .write_all(input.as_bytes())
                    .expect("send unread body");
                writer.shutdown(Shutdown::Write).expect("finish sending");
            });
            let (actual, document) = client.response();
            assert_eq!(actual, status);
            assert_eq!(document["data"]["http_status"], status);
            client.closed();
            sending.join().expect("sender exits");
        }
    }

    #[test]
    fn malformed_heads_chunks_and_oversized_heads_are_json_errors() {
        for (input, status) in [
            ("not HTTP\r\n\r\n".to_owned(), 400),
            (
                "POST /api HTTP/1.1\r\nX: ".to_owned() + &"x".repeat(1024),
                431,
            ),
        ] {
            let mut client = Client::start(tiny_limits(), |_| panic!("must not dispatch"));
            client
                .stream
                .write_all(input.as_bytes())
                .expect("send invalid request");
            let (actual, document) = client.response();
            assert_eq!(actual, status);
            assert_eq!(document["data"]["http_status"], status);
            client.closed();
        }
        for body in ["not-hex\r\n", "1\r\nx!!", "0\r\ninvalid-trailer\r\n\r\n"] {
            let mut client = Client::start(tiny_limits(), |_| panic!("must not dispatch"));
            let input = client.head("Transfer-Encoding: chunked\r\n") + body;
            client
                .stream
                .write_all(input.as_bytes())
                .expect("send invalid chunking");
            assert_eq!(client.response().0, 400);
            client.closed();
        }
    }

    #[test]
    fn connection_slots_and_handler_slots_are_bounded_and_reusable() {
        let slots = Arc::new(Slots::default());
        let held: Vec<_> = (0..MAX_CONNECTIONS)
            .map(|_| slots.try_acquire(MAX_CONNECTIONS).expect("slot"))
            .collect();
        assert!(slots.try_acquire(MAX_CONNECTIONS).is_none());
        drop(held);
        assert_eq!(*slots.used.lock().expect("count"), 0);

        let held: Vec<_> = (0..REQUEST_WORKERS)
            .map(|_| slots.acquire(REQUEST_WORKERS))
            .collect();
        let (sender, receiver) = mpsc::channel();
        let other = Arc::clone(&slots);
        let waiting = std::thread::spawn(move || {
            let slot = other.acquire(REQUEST_WORKERS);
            sender.send(()).expect("acquired");
            drop(slot);
        });
        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        drop(held);
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("slot released");
        waiting.join().expect("waiter exits");
        assert_eq!(*slots.used.lock().expect("count"), 0);
    }
}
