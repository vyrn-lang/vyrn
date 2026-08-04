//! Integration tests for `vyrn serve` (RFC-0016): spawn the real `vyrn`
//! binary as an HTTP host and drive it with raw `std::net::TcpStream` requests.
//!
//! Each test picks a free port by binding an ephemeral listener, reading its
//! port, and dropping it (accepting the small bind race), then spawns the
//! server on that port and waits for the `serving ...` line on stderr before
//! connecting. A `Drop` guard kills the child at the end.
//!
//! Asserted: `/health` → 200 ok; module state (the hit counter) persisting and
//! incrementing across sequential requests; a handler trap → 500 with the
//! server surviving the next request; garbage → 400; and `main`'s startup
//! `print` reaching stdout before the first request is served.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One served source used by every test: module state (`hits`), a `main` that
/// prints a startup banner, and a `handle` covering `/health`, a trap path
/// (`/boom`, runtime division by zero), and a default that echoes the counter.
const SERVER_SRC: &str = r#"
let mut hits: Int64 = 0

fn main() -> Int64 {
    print("server up")
    return 0
}

fn handle(req: Request) -> Response {
    hits = hits + 1
    if req.path == "/health" {
        return Response { status: 200, contentType: "text/plain", body: "ok", vary: "", headers: [:] }
    }
    if req.path == "/boom" {
        let z = hits - hits
        let bad = hits / z
        return Response { status: 200, contentType: "text/plain", body: bad.toString(), vary: "", headers: [:] }
    }
    return Response { status: 200, contentType: "text/plain", body: "hits=\{hits.toString()}", vary: "", headers: [:] }
}
"#;

/// A running `vyrn serve` child plus drained stdout/stderr buffers. The `Drop`
/// impl kills the process so a panicking test never leaks a listening server.
struct Serve {
    child: Child,
    port: u16,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    // Keep the temp file alive for the process's lifetime.
    _file: TempFile,
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A temp file that deletes itself on drop.
struct TempFile {
    path: std::path::PathBuf,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Bind an ephemeral port, read it, drop the listener, and return the port.
/// The tiny window before `vyrn serve` re-binds is an accepted race.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// Continuously read `r` into `acc` on a background thread (so the child never
/// blocks on a full pipe).
fn drain<R: Read + Send + 'static>(mut r: R) -> Arc<Mutex<String>> {
    let acc = Arc::new(Mutex::new(String::new()));
    let a = acc.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => a.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
    });
    acc
}

/// Poll `acc` until it contains `needle` (or panic after `timeout`).
fn wait_for(acc: &Arc<Mutex<String>>, needle: &str, timeout: Duration) -> String {
    let start = Instant::now();
    loop {
        {
            let s = acc.lock().unwrap();
            if s.contains(needle) {
                return s.clone();
            }
        }
        if start.elapsed() > timeout {
            let s = acc.lock().unwrap();
            panic!("timed out waiting for {needle:?}; captured so far:\n{}", *s);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Spawn `vyrn serve <tmp> --port <free> [extra args]` on `src` and wait for
/// the startup line before returning.
fn start_server_on(src: &str, extra: &[&str]) -> Serve {
    let unique = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    );
    let path = std::env::temp_dir().join(format!("vyrn-serve-{unique}.vyrn"));
    std::fs::write(&path, src).expect("write temp server");
    let file = TempFile { path: path.clone() };

    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("serve")
        .arg(&path)
        .arg("--port")
        .arg(port.to_string())
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vyrn serve");

    let stdout = drain(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());
    let server = Serve { child, port, stdout, stderr, _file: file };
    // The accept loop is live once the banner prints.
    wait_for(&server.stderr, "serving", Duration::from_secs(10));
    server
}

/// Spawn `vyrn serve <tmp> --port <free>` on `SERVER_SRC` and wait for the
/// startup line before returning.
fn start_server() -> Serve {
    start_server_on(SERVER_SRC, &[])
}

/// Send a raw request line + headers, read the whole `Connection: close`
/// response, and split it into (status_line, body).
fn request(port: u16, raw: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.write_all(raw.as_bytes()).expect("write request");
    stream.flush().ok();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read response");
    let (head, body) = resp.split_once("\r\n\r\n").unwrap_or((resp.as_str(), ""));
    let status = head.lines().next().unwrap_or("").to_string();
    (status, body.to_string())
}

/// A well-formed GET for `path`.
fn get(port: u16, path: &str) -> (String, String) {
    request(port, &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"))
}

#[test]
fn health_returns_200_ok() {
    let s = start_server();
    let (status, body) = get(s.port, "/health");
    assert_eq!(status, "HTTP/1.1 200 OK", "status line");
    assert_eq!(body, "ok", "health body");
}

#[test]
fn module_state_persists_across_requests() {
    let s = start_server();
    // A fresh server: the counter starts at 0 and each request bumps it.
    let (_, b1) = get(s.port, "/");
    let (_, b2) = get(s.port, "/");
    let (_, b3) = get(s.port, "/");
    assert_eq!(b1, "hits=1", "first request");
    assert_eq!(b2, "hits=2", "second request (state persisted)");
    assert_eq!(b3, "hits=3", "third request (state persisted)");
}

#[test]
fn handler_trap_yields_500_and_server_survives() {
    let s = start_server();
    // The trap path: division by zero inside `handle`.
    let (status, body) = get(s.port, "/boom");
    assert_eq!(status, "HTTP/1.1 500 Internal Server Error", "trap -> 500 status");
    assert_eq!(body, "internal error", "trap -> generic 500 body");

    // The canonical trap wording is logged to the server's stderr.
    let err = wait_for(&s.stderr, "division by zero", Duration::from_secs(5));
    assert!(err.contains("error: division by zero"), "trap logged to stderr:\n{err}");

    // A subsequent request still works — one bad request did not kill the server.
    let (status, body) = get(s.port, "/health");
    assert_eq!(status, "HTTP/1.1 200 OK", "server survived the trap");
    assert_eq!(body, "ok");
}

#[test]
fn garbage_request_yields_400_without_reaching_vyrn() {
    let s = start_server();
    let (status, body) = request(s.port, "this is not http\r\n\r\n");
    assert_eq!(status, "HTTP/1.1 400 Bad Request", "garbage -> 400");
    assert_eq!(body, "bad request");
    // And the server is still alive for a real request afterward.
    let (status, _) = get(s.port, "/health");
    assert_eq!(status, "HTTP/1.1 200 OK", "server survived the garbage request");
}

#[test]
fn chunked_body_yields_501() {
    let s = start_server();
    let (status, _) = request(
        s.port,
        "POST / HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 501 Not Implemented", "chunked -> 501");
}

#[test]
fn post_body_reaches_handle() {
    let s = start_server();
    // A Content-Length body is read exactly and the request is served.
    let (status, body) = request(
        s.port,
        "POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(body, "hits=1");
}

#[test]
fn main_startup_print_precedes_first_request() {
    let s = start_server();
    // `start_server` already waited for the `serving` banner on stderr, which
    // is printed AFTER `main` runs — so `main`'s stdout is present before any
    // request is served.
    let out = s.stdout.lock().unwrap().clone();
    assert!(out.contains("server up"), "main's startup print reached stdout first:\n{out}");
}

// ---- worker threads (RFC-0025): `vyrn serve --workers N` -------------------

/// A module-state-free server: `handle` computes (fib) and echoes — the shape
/// the isolation gate admits. `main` still prints a banner (allowed: `main`
/// runs once, on the setup interpreter, before any worker starts).
const PURE_SERVER_SRC: &str = r#"
fn fib(n: Int64) -> Int64 {
    if n < 2 { return n }
    return fib(n - 1) + fib(n - 2)
}

fn main() -> Int64 {
    print("server up")
    return 0
}

fn handle(req: Request) -> Response {
    if req.path == "/fib" {
        return Response { status: 200, contentType: "text/plain", body: fib(20).toString(), vary: "", headers: [:] }
    }
    return Response { status: 200, contentType: "text/plain", body: "echo:\{req.path}", vary: "", headers: [:] }
}
"#;

#[test]
fn workers_answer_concurrent_requests_correctly() {
    let s = start_server_on(PURE_SERVER_SRC, &["--workers", "4"]);
    // The banner names the pool.
    let err = s.stderr.lock().unwrap().clone();
    assert!(err.contains("with 4 workers"), "banner should name the pool:\n{err}");

    // Eight concurrent client threads; every response must be correct.
    let port = s.port;
    let handles: Vec<_> = (0..8)
        .map(|i| {
            std::thread::spawn(move || {
                if i % 2 == 0 {
                    get(port, "/fib")
                } else {
                    get(port, &format!("/req{i}"))
                }
            })
        })
        .collect();
    for (i, h) in handles.into_iter().enumerate() {
        let (status, body) = h.join().expect("client thread");
        assert_eq!(status, "HTTP/1.1 200 OK", "request {i} status");
        if i % 2 == 0 {
            assert_eq!(body, "6765", "request {i} computed fib(20)");
        } else {
            assert_eq!(body, format!("echo:/req{i}"), "request {i} echoed its path");
        }
    }

    // `main` ran ONCE (on the setup interpreter), not once per worker.
    let out = s.stdout.lock().unwrap().clone();
    assert_eq!(out.matches("server up").count(), 1, "main's print appears exactly once:\n{out}");
}

#[test]
fn workers_survive_a_trap_and_keep_serving() {
    // A trap inside one worker's `handle` answers 500 and the pool lives on.
    let s = start_server_on(
        r#"
fn handle(req: Request) -> Response {
    if req.path == "/boom" {
        let n = req.body.byteLength
        let z = n - n
        return Response { status: 200, contentType: "text/plain", body: (n / z).toString(), vary: "", headers: [:] }
    }
    return Response { status: 200, contentType: "text/plain", body: "ok", vary: "", headers: [:] }
}
"#,
        &["--workers", "2"],
    );
    let (status, _) = get(s.port, "/boom");
    assert_eq!(status, "HTTP/1.1 500 Internal Server Error", "trap -> 500");
    let err = wait_for(&s.stderr, "division by zero", Duration::from_secs(5));
    assert!(err.contains("error: division by zero"), "canonical wording logged:\n{err}");
    let (status, body) = get(s.port, "/health");
    assert_eq!(status, "HTTP/1.1 200 OK", "pool survived the trap");
    assert_eq!(body, "ok");
}

#[test]
fn workers_are_refused_when_handle_touches_module_state() {
    // SERVER_SRC's `handle` writes `hits` — the isolation gate must refuse the
    // pool at startup, naming the offending call path, and exit nonzero. Use a
    // helper in the chain so the path has a hop in it.
    let src = r#"
let mut hits: Int64 = 0

fn bump() -> Int64 {
    hits = hits + 1
    return hits
}

fn handle(req: Request) -> Response {
    return Response { status: 200, contentType: "text/plain", body: bump().toString(), vary: "", headers: [:] }
}
"#;
    let unique = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    );
    let path = std::env::temp_dir().join(format!("vyrn-serve-{unique}.vyrn"));
    std::fs::write(&path, src).expect("write temp server");
    let _file = TempFile { path: path.clone() };

    let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("serve")
        .arg(&path)
        .arg("--port")
        .arg("0")
        .arg("--workers")
        .arg("2")
        .output()
        .expect("run vyrn serve");
    assert!(!out.status.success(), "the gate must refuse --workers");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(
            "error: `--workers` needs a module-state-free `handle`: `handle` -> `bump` \
             reads or writes module state `hits` (shared by definition) — run without \
             `--workers` for the sequential loop"
        ),
        "refusal names the call path:\n{err}"
    );
}

#[test]
fn sequential_default_is_unchanged_for_stateful_handles() {
    // No `--workers` = today's sequential loop, module state and all — the
    // stateful counter still works (also covered by
    // `module_state_persists_across_requests`; this pins that the RFC-0025
    // machinery did not alter the default path's banner or behavior).
    let s = start_server();
    let err = s.stderr.lock().unwrap().clone();
    assert!(!err.contains("workers"), "default banner has no pool:\n{err}");
    let (_, b1) = get(s.port, "/");
    assert_eq!(b1, "hits=1");
}

/// The response header map and the bodyless answer it exists for (RFC-0074 M2).
/// A projection is what actually produces these (`etag`/`cacheFor` in
/// `std/http`, covered in `tests/http.rs`); this is the host end — what the
/// writer puts on the wire for a `Response` that carries them.
const HEADER_SRC: &str = r#"
fn handle(req: Request) -> Response {
    if req.path == "/cond" {
        // What `mount` hands back for a matched `If-None-Match`: the validators
        // stay, the body and the media type go.
        return Response { status: 304, contentType: "", body: "", vary: "Accept", headers: ["ETag": "\"abc\"", "Cache-Control": "max-age=60"] }
    }
    return Response { status: 200, contentType: "text/plain", body: "ok", vary: "", headers: ["ETag": "\"abc\"", "Cache-Control": "max-age=60"] }
}
"#;

#[test]
fn the_response_header_map_reaches_the_wire() {
    let s = start_server_on(HEADER_SRC, &[]);
    let (status, raw) = request_raw(
        s.port,
        "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 200 OK", "{raw}");
    assert!(raw.contains("\r\nETag: \"abc\"\r\n"), "{raw}");
    assert!(raw.contains("\r\nCache-Control: max-age=60\r\n"), "{raw}");
    assert!(raw.contains("\r\nContent-Type: text/plain\r\n"), "{raw}");
}

#[test]
fn a_304_carries_its_validators_and_neither_body_nor_content_type() {
    let s = start_server_on(HEADER_SRC, &[]);
    let (status, raw) = request_raw(
        s.port,
        "GET /cond HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 304 Not Modified", "{raw}");
    // RFC 9110 15.4.5: the metadata the 200 would have sent, and no content.
    assert!(raw.contains("\r\nETag: \"abc\"\r\n"), "{raw}");
    assert!(raw.contains("\r\nCache-Control: max-age=60\r\n"), "{raw}");
    assert!(raw.contains("\r\nVary: Accept\r\n"), "{raw}");
    // An empty content type writes no field at all, rather than a malformed one.
    assert!(!raw.contains("Content-Type"), "a 304 declares no media type:\n{raw}");
    assert!(raw.ends_with("\r\n\r\n"), "nothing after the header block:\n{raw}");
}

/// The whole response text, not just (status, body) — these tests are about the
/// header block itself.
fn request_raw(port: u16, raw: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.write_all(raw.as_bytes()).expect("write request");
    stream.flush().ok();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read response");
    let status = resp.lines().next().unwrap_or("").to_string();
    (status, resp)
}

// ---- RFC-0074 M3a: the streaming response, and the disconnect signal --------
//
// The one conformance row RFC-0075 left open ("client disconnects mid-stream →
// producer release runs") could not be tested before there was a transport to
// disconnect from. It is tested here, in its stronger form: **the release runs
// before the next event would be produced**.
//
// This source is deliberately below `std/http` — it calls `serveStream` and
// `fromStep` directly, so the pin is on the MECHANISM rather than on the
// projection that spells it. `std/http`'s `sse` values are pinned in
// `tests/http.rs`, and `examples/bin` is the live one.
//
// Two witnesses, because "the release ran" and "production stopped" are
// different claims:
//
// - `/steps` counts how many times the producer's step function ran. After the
//   client vanishes it must stop moving — that is the row's own wording.
// - `/probe` reads the cursor cell the stream owns, through a `Ref` the step
//   parked in module state. Releasing a stream releases that cell and bumps its
//   generation, so a later read is the canonical "reference used after release"
//   trap. A 500 there is the release itself being observed, not inferred.
const SSE_SRC: &str = r#"
import { unfold, map } from "std/stream"

let mut steps: Int64 = 0
let mut saved: Ref<Int64> = cell(0)

/// An endless feed: it never answers `None`, so only the client going away can
/// end it.
fn tick(c: Ref<Int64>) -> Option<String> {
    steps = steps + 1
    saved = c
    let n = get(c)
    set(c, n + 1)
    return Some("id: \{n}\ndata: e\{n}\n\n")
}

/// The same feed, unencoded, plus the encoder — which is what a `map` over a
/// live feed is for, and what RFC-0075 M2c made possible: mapping one was a hang
/// until the combinators became lazy, and M3a's element is an encoded frame
/// partly because of it.
fn nums(c: Ref<Int64>) -> Option<Int64> {
    steps = steps + 1
    saved = c
    let n = get(c)
    set(c, n + 1)
    return Some(n)
}

fn frame(n: Int64) -> String {
    return "id: \{n}\ndata: e\{n}\n\n"
}

/// A feed with nothing to say — the 204 path.
fn silent(c: Ref<Int64>) -> Option<String> {
    steps = steps + 1
    return None
}

fn handle(req: Request) -> Response {
    if req.path == "/live" {
        serveStream(fromStep(0, tick))
        return Response { status: 200, contentType: "text/event-stream", body: "retry: 500\n\n", vary: "", headers: [:] }
    }
    if req.path == "/mapped" {
        serveStream(map(unfold(0, nums), frame))
        return Response { status: 200, contentType: "text/event-stream", body: "retry: 500\n\n", vary: "", headers: [:] }
    }
    if req.path == "/empty" {
        serveStream(fromStep(0, silent))
        return Response { status: 200, contentType: "text/event-stream", body: "retry: 500\n\n", vary: "", headers: [:] }
    }
    if req.path == "/steps" {
        return Response { status: 200, contentType: "text/plain", body: "\{steps}", vary: "", headers: [:] }
    }
    if req.path == "/probe" {
        return Response { status: 200, contentType: "text/plain", body: "\{get(saved)}", vary: "", headers: [:] }
    }
    return Response { status: 404, contentType: "text/plain", body: "no", vary: "", headers: [:] }
}
"#;

/// Open `path`, read until at least `want` bytes have arrived (or the read
/// deadline passes), and hand back the connection still open. Bounded, because a
/// stream that never ends must not be able to hang the suite.
fn open_stream(port: u16, path: &str, want: usize) -> (TcpStream, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n\r\n")
                .as_bytes(),
        )
        .expect("write request");
    stream.flush().ok();
    let mut got = Vec::new();
    let mut buf = [0u8; 512];
    while got.len() < want {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
        }
    }
    let text = String::from_utf8_lossy(&got).to_string();
    (stream, text)
}

#[test]
fn a_stream_answers_with_frames_and_no_content_length() {
    let s = start_server_on(SSE_SRC, &[]);
    let (live, text) = open_stream(s.port, "/live", 200);
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
    assert!(text.contains("\r\nContent-Type: text/event-stream\r\n"), "{text}");
    // A stream's body ends when the connection does; declaring a length would be
    // declaring a length nobody knows.
    assert!(!text.contains("Content-Length"), "a stream declares no length:\n{text}");
    assert!(text.contains("\r\nCache-Control: no-store\r\n"), "{text}");
    // The `Response.body` is the stream's prologue, written once before the
    // first frame — SSE's reconnect hint belongs exactly there.
    let (_, after) = text.split_once("\r\n\r\n").expect("header block");
    assert!(after.starts_with("retry: 500\n\n"), "prologue first:\n{after}");
    assert!(after.contains("id: 0\ndata: e0\n\n"), "the first frame:\n{after}");
    assert!(after.contains("id: 1\ndata: e1\n\n"), "and the second:\n{after}");
    drop(live);
}

#[test]
fn a_producer_with_nothing_to_say_answers_204_rather_than_an_empty_stream() {
    let s = start_server_on(SSE_SRC, &[]);
    // 204 is the one status a plain `EventSource` reads as "stop, do not
    // reconnect" (WHATWG HTML 9.2.5), so a normally-completed feed costs the
    // client exactly one more request rather than an endless reconnect loop.
    let (status, raw) = request_raw(
        s.port,
        "GET /empty HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 204 No Content", "{raw}");
    assert!(!raw.contains("text/event-stream"), "no stream was opened:\n{raw}");
    // The server is still there: the empty producer was released, not leaked.
    let (status, body) = get(s.port, "/steps");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(body, "1", "the silent step ran once and was not asked again");
}

#[test]
fn a_client_that_vanishes_runs_the_producers_release_before_the_next_event() {
    let s = start_server_on(SSE_SRC, &[]);
    let (live, text) = open_stream(s.port, "/live", 200);
    assert!(text.contains("id: 0\ndata: e0\n\n"), "events were flowing:\n{text}");

    // The disconnect. Nothing tells the server about it; it finds out by
    // writing to a socket that is no longer there.
    drop(live);

    // The first `/steps` blocks until the pump notices — which is the write
    // after the drop — so its answer is already the final count.
    let (status, settled) = get(s.port, "/steps");
    assert_eq!(status, "HTTP/1.1 200 OK", "the server survived the disconnect");
    std::thread::sleep(Duration::from_millis(300));
    let (_, later) = get(s.port, "/steps");
    assert_eq!(
        settled, later,
        "the producer kept running after the client went away ({settled} -> {later})"
    );

    // And the release itself: the stream's cursor cell is gone, so reading the
    // `Ref` the step parked traps. This is the row's evidence rather than its
    // symptom — production stopping could be a stuck pump; a released cell
    // could only have come from `close`.
    let (status, body) = get(s.port, "/probe");
    assert_eq!(status, "HTTP/1.1 500 Internal Server Error", "cursor still live: {body}");
    let err = wait_for(&s.stderr, "reference used after release", Duration::from_secs(5));
    assert!(err.contains("error: reference used after release"), "{err}");

    // One dropped client did not cost the server anything else.
    let (status, _) = get(s.port, "/steps");
    assert_eq!(status, "HTTP/1.1 200 OK");
}

#[test]
fn a_mapped_feed_streams_and_its_release_walks_the_chain() {
    // RFC-0075 M2c, at the transport this milestone exists to unblock. Two
    // streams are alive here — the feed and the `map` that wraps it — and only
    // the wrapper has a name the host can hold, so the release has to walk from
    // it to the feed's cursor cell. The `/probe` trap is that walk being
    // observed: `saved` is the INNER producer's cursor, one the host never
    // touched.
    let s = start_server_on(SSE_SRC, &[]);
    let (live, text) = open_stream(s.port, "/mapped", 200);
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
    let (_, after) = text.split_once("\r\n\r\n").expect("header block");
    assert!(after.contains("id: 0\ndata: e0\n\n"), "a mapped frame:\n{after}");
    assert!(after.contains("id: 1\ndata: e1\n\n"), "and the next:\n{after}");

    drop(live);
    let (status, settled) = get(s.port, "/steps");
    assert_eq!(status, "HTTP/1.1 200 OK", "the server survived the disconnect");
    std::thread::sleep(Duration::from_millis(300));
    let (_, later) = get(s.port, "/steps");
    assert_eq!(
        settled, later,
        "the feed kept running behind the map after the client went away ({settled} -> {later})"
    );

    let (status, body) = get(s.port, "/probe");
    assert_eq!(status, "HTTP/1.1 500 Internal Server Error", "cursor still live: {body}");
    let err = wait_for(&s.stderr, "reference used after release", Duration::from_secs(5));
    assert!(err.contains("error: reference used after release"), "{err}");
}

// ---- RFC-0074 M3b: the same signal, a second transport --------------------
//
// The milestone's pin is that everything ABOVE this comment passes unchanged.
// M3a wrote its conformance tests below `std/http`, against `serveStream` and
// `fromStep` directly, precisely so a second adapter would have nothing to
// rewrite — and nothing was rewritten: `SSE_SRC`, `open_stream` and the four
// tests over them are as M3a left them.
//
// What follows is the same mechanism through WebSocket framing. The witnesses
// are `SSE_SRC`'s: `/steps` stops moving, and `/probe` traps on a cursor cell
// only a `close` could have released. The source is below `std/http` for M3a's
// reason — this is about the mechanism, not about the projection that spells it.
// `std/http`'s `ws` values are pinned in `tests/http.rs`, and `examples/bin`
// serves the same paste tail over both transports.
const WS_SRC: &str = r#"
import { sha1 } from "std/hash"
import { base64EncodeBytes } from "std/codecs"

let mut steps: Int64 = 0
let mut saved: Ref<Int64> = cell(0)

/// An endless feed of PAYLOADS, not frames: RFC-0074's rule is that Vyrn owns
/// what the user chooses and the host owns what the protocol fixes, and there is
/// no choice in an opcode, a length or a mask.
fn tick(c: Ref<Int64>) -> Option<String> {
    steps = steps + 1
    saved = c
    let n = get(c)
    set(c, n + 1)
    return Some("e\{n}")
}

/// One 100-byte message, then the end — the fragmentation and close-code case.
fn once(c: Ref<Int64>) -> Option<String> {
    let n = get(c)
    if n > 0 {
        return None
    }
    set(c, n + 1)
    let mut s = ""
    let mut i = 0
    while i < 10 {
        s = s + "0123456789"
        i = i + 1
    }
    return Some(s)
}

fn headerOf(req: Request, name: String) -> String {
    return match req.headers[name] {
        Some(v) => v,
        None => "",
    }
}

/// RFC 6455 4.2.2's nonce transform, in ordinary Vyrn: base64(SHA-1(key + GUID)).
fn accept(req: Request) -> String {
    let key = headerOf(req, "sec-websocket-key")
    return base64EncodeBytes(sha1(bytes(key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11")))
}

/// The handshake head. `body` is not a prologue — a socket has nothing between
/// the handshake and the first frame — so it carries the two numbers the host
/// frames with: the close code and the fragment limit.
fn upgrade(req: Request, params: String) -> Response {
    return Response {
        status: 101,
        contentType: "",
        body: params,
        vary: "",
        headers: ["Upgrade": "websocket", "Connection": "Upgrade", "Sec-WebSocket-Accept": accept(req)],
    }
}

fn handle(req: Request) -> Response {
    if req.path == "/socket" {
        serveStream(fromStep(0, tick))
        return upgrade(req, "1000 0")
    }
    if req.path == "/split" {
        serveStream(fromStep(0, once))
        return upgrade(req, "1001 40")
    }
    if req.path == "/steps" {
        return Response { status: 200, contentType: "text/plain", body: "\{steps}", vary: "", headers: [:] }
    }
    if req.path == "/probe" {
        return Response { status: 200, contentType: "text/plain", body: "\{get(saved)}", vary: "", headers: [:] }
    }
    return Response { status: 404, contentType: "text/plain", body: "no", vary: "", headers: [:] }
}
"#;

/// RFC 6455 §1.3's worked example: this key must produce this accept value. It
/// pins the SHA-1 written in `std/hash` all the way to the wire.
const WS_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
const WS_ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

/// One frame read off the wire.
struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

/// Do the upgrade and hand back the still-open socket plus the handshake head.
fn open_socket(port: u16, path: &str) -> (TcpStream, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
                 Connection: keep-alive, Upgrade\r\nSec-WebSocket-Key: {WS_KEY}\r\n\
                 Sec-WebSocket-Version: 13\r\n\r\n"
            )
            .as_bytes(),
        )
        .expect("write upgrade");
    stream.flush().ok();
    // Read exactly the header block, byte at a time, so no frame bytes are eaten.
    let mut head = Vec::new();
    let mut one = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut one) {
            Ok(0) | Err(_) => break,
            Ok(_) => head.push(one[0]),
        }
    }
    (stream, String::from_utf8_lossy(&head).to_string())
}

/// Read one server frame. Panics on a masked one: §5.1 forbids a server to mask.
fn read_frame(s: &mut TcpStream) -> Option<Frame> {
    let mut h = [0u8; 2];
    s.read_exact(&mut h).ok()?;
    assert_eq!(h[1] & 0x80, 0, "a server frame must not be masked (RFC 6455 5.1)");
    let len = match h[1] & 0x7f {
        126 => {
            let mut e = [0u8; 2];
            s.read_exact(&mut e).ok()?;
            u16::from_be_bytes(e) as usize
        }
        127 => {
            let mut e = [0u8; 8];
            s.read_exact(&mut e).ok()?;
            u64::from_be_bytes(e) as usize
        }
        n => n as usize,
    };
    let mut payload = vec![0u8; len];
    s.read_exact(&mut payload).ok()?;
    Some(Frame { fin: h[0] & 0x80 != 0, opcode: h[0] & 0x0f, payload })
}

/// Read frames until a close arrives, and answer with its code. Bounded, so a
/// server that never closes fails the test rather than hanging the suite.
fn read_until_close(s: &mut TcpStream) -> Option<u16> {
    for _ in 0..64 {
        let f = read_frame(s)?;
        if f.opcode == 8 {
            return Some(u16::from_be_bytes([f.payload[0], f.payload[1]]));
        }
    }
    None
}

/// A client frame. `mask` false is the protocol violation §5.1 names.
fn write_client_frame(s: &mut TcpStream, opcode: u8, payload: &[u8], mask: bool) {
    let mut f = vec![0x80 | opcode];
    let n = payload.len() as u8;
    f.push(if mask { 0x80 | n } else { n });
    if mask {
        let key = [0x37u8, 0xfa, 0x21, 0x3d];
        f.extend_from_slice(&key);
        for (i, b) in payload.iter().enumerate() {
            f.push(b ^ key[i % 4]);
        }
    } else {
        f.extend_from_slice(payload);
    }
    let _ = s.write_all(&f);
    let _ = s.flush();
}

#[test]
fn a_socket_handshake_answers_101_and_the_frames_carry_the_payload() {
    let s = start_server_on(WS_SRC, &[]);
    let (mut sock, head) = open_socket(s.port, "/socket");
    assert!(head.starts_with("HTTP/1.1 101 Switching Protocols\r\n"), "{head}");
    assert!(head.contains("\r\nUpgrade: websocket\r\n"), "{head}");
    assert!(head.contains("\r\nConnection: Upgrade\r\n"), "{head}");
    // The SHA-1 in `std/hash`, on the wire, against RFC 6455's own example.
    assert!(head.contains(&format!("\r\nSec-WebSocket-Accept: {WS_ACCEPT}\r\n")), "{head}");
    // The head's `body` is the host's framing parameters and is written nowhere.
    assert!(head.ends_with("\r\n\r\n"), "nothing after the header block:\n{head}");
    assert!(!head.contains("1000 0"), "the framing parameters are not written:\n{head}");

    for i in 0..3 {
        let f = read_frame(&mut sock).expect("a frame");
        assert_eq!(f.opcode, 1, "a text frame");
        assert!(f.fin, "unfragmented");
        assert_eq!(String::from_utf8_lossy(&f.payload), format!("e{i}"));
    }
    drop(sock);
}

#[test]
fn a_socket_client_that_vanishes_runs_the_producers_release_before_the_next_event() {
    // RFC-0075's disconnect row, through the second adapter, with the SAME two
    // witnesses M3a's SSE test uses — which is the whole of what "the signal
    // generalises" claims. Nothing here asks the host about the client.
    let s = start_server_on(WS_SRC, &[]);
    let (mut sock, head) = open_socket(s.port, "/socket");
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    assert_eq!(read_frame(&mut sock).expect("a frame").payload, b"e0", "frames were flowing");
    read_frame(&mut sock).expect("a second frame");

    drop(sock);

    // Blocks until the pump notices — which is the write after the drop — so this
    // answer is already the final count.
    let (status, settled) = get(s.port, "/steps");
    assert_eq!(status, "HTTP/1.1 200 OK", "the server survived the disconnect");
    std::thread::sleep(Duration::from_millis(300));
    let (_, later) = get(s.port, "/steps");
    assert_eq!(
        settled, later,
        "the producer kept running after the client went away ({settled} -> {later})"
    );

    let (status, body) = get(s.port, "/probe");
    assert_eq!(status, "HTTP/1.1 500 Internal Server Error", "cursor still live: {body}");
    let err = wait_for(&s.stderr, "reference used after release", Duration::from_secs(5));
    assert!(err.contains("error: reference used after release"), "{err}");
}

#[test]
fn a_client_close_is_answered_with_a_close_frame() {
    // §5.5.1. The pump learns about it at the next frame boundary rather than
    // instantly, because it spends the time in between blocked in the producer —
    // which is the same fact that refuses `heartbeat`.
    let s = start_server_on(WS_SRC, &[]);
    let (mut sock, _) = open_socket(s.port, "/socket");
    read_frame(&mut sock).expect("a frame");
    write_client_frame(&mut sock, 8, &1000u16.to_be_bytes(), true);
    assert_eq!(read_until_close(&mut sock), Some(1000), "the close is answered");
    // And the producer behind it was released.
    let (status, _) = get(s.port, "/probe");
    assert_eq!(status, "HTTP/1.1 500 Internal Server Error", "the cursor was released");
}

#[test]
fn an_unmasked_client_frame_closes_with_1002() {
    // §5.1: every client frame is masked. Server-push has nothing to do with a
    // client's message, but a frame that breaks the framing rules is a protocol
    // error rather than something to skip past.
    let s = start_server_on(WS_SRC, &[]);
    let (mut sock, _) = open_socket(s.port, "/socket");
    read_frame(&mut sock).expect("a frame");
    write_client_frame(&mut sock, 1, b"hello", false);
    assert_eq!(read_until_close(&mut sock), Some(1002), "a protocol error");
}

#[test]
fn max_frame_splits_a_message_and_the_feeds_end_carries_the_programs_close_code() {
    // `/split` yields one 100-byte message under a 40-byte limit, then ends.
    // §5.4: the first fragment carries the data opcode with FIN clear, the rest
    // carry the continuation opcode, and only the last sets FIN.
    let s = start_server_on(WS_SRC, &[]);
    let (mut sock, head) = open_socket(s.port, "/split");
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    let a = read_frame(&mut sock).expect("first fragment");
    let b = read_frame(&mut sock).expect("second fragment");
    let c = read_frame(&mut sock).expect("last fragment");
    assert_eq!((a.opcode, a.fin, a.payload.len()), (1, false, 40));
    assert_eq!((b.opcode, b.fin, b.payload.len()), (0, false, 40));
    assert_eq!((c.opcode, c.fin, c.payload.len()), (0, true, 20));
    let whole: Vec<u8> = [a.payload, b.payload, c.payload].concat();
    assert_eq!(whole.len(), 100, "the message reassembles");
    // The feed ended, so the host closes with the code the program chose.
    assert_eq!(read_until_close(&mut sock), Some(1001), "closeCode reached the wire");
}

#[test]
fn a_socket_upgrade_the_program_refuses_is_an_ordinary_response() {
    // Not every 101 path is a stream: a request that is not an upgrade never
    // reaches `serveStream`, so it answers as a buffered response and the
    // connection ends. (`std/http`'s `ws` writes the 400/426; here the source
    // simply has no such path, so the default 404 proves the same thing — a
    // non-upgrading GET at a socket's URL is not framed.)
    let s = start_server_on(WS_SRC, &[]);
    let (status, body) = get(s.port, "/nope");
    assert_eq!(status, "HTTP/1.1 404 Not Found");
    assert_eq!(body, "no");
}

/// The same milestone one level up: `std/http`'s `ws`, mounted, writing its own
/// handshake. `WS_SRC` above proves the MECHANISM; this proves the projection
/// that spells it — the handshake `std/http` computes, the subprotocol rule of
/// §4.2.2 (a server may only select what the client offered), and the two ways an
/// upgrade is refused without ever opening a stream. It lives here rather than in
/// `tests/http.rs` for the reason M2 recorded: a `vyrn serve` harness works in
/// this file and not in that one.
const WS_MOUNTED_SRC: &str = r#"
import { Frames, Live, Route, Socket, mount, ws } from "std/http"

fn step(c: Ref<Int64>) -> Option<String> {
    let n = get(c)
    if n > 1 {
        return None
    }
    set(c, n + 1)
    return Some("m\{n}")
}

fn feed(req: Request, ps: Map<String, String>, since: Int64) -> Stream<String> {
    return fromStep(since, step)
}

fn sockets() -> Array<Socket> {
    return [ws("/chat", feed).closeCode(1001).subprotocol("vyrn.v1")]
}

fn handle(req: Request) -> Response {
    let groups: Array<Array<Route>> = []
    let live: Array<Live> = []
    return match mount(req, groups, live, sockets()) {
        Some(r) => r,
        None => Response { status: 404, contentType: "text/plain", body: "no route", vary: "", headers: [:] },
    }
}
"#;

#[test]
fn the_ws_projection_writes_its_own_handshake() {
    let s = start_server_on(WS_MOUNTED_SRC, &[]);
    let mut sock = TcpStream::connect(("127.0.0.1", s.port)).expect("connect");
    sock.set_read_timeout(Some(Duration::from_secs(5))).ok();
    sock.write_all(
        format!(
            "GET /chat HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
             Connection: keep-alive, Upgrade\r\nSec-WebSocket-Key: {WS_KEY}\r\n\
             Sec-WebSocket-Protocol: chat, vyrn.v1\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .as_bytes(),
    )
    .expect("write upgrade");
    sock.flush().ok();
    let mut head = Vec::new();
    let mut one = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match sock.read(&mut one) {
            Ok(0) | Err(_) => break,
            Ok(_) => head.push(one[0]),
        }
    }
    let head = String::from_utf8_lossy(&head).to_string();
    assert!(head.starts_with("HTTP/1.1 101 Switching Protocols\r\n"), "{head}");
    assert!(head.contains(&format!("\r\nSec-WebSocket-Accept: {WS_ACCEPT}\r\n")), "{head}");
    // Offered by the client and chosen by the route, so it is echoed.
    assert!(head.contains("\r\nSec-WebSocket-Protocol: vyrn.v1\r\n"), "{head}");
    assert_eq!(read_frame(&mut sock).expect("m0").payload, b"m0");
    assert_eq!(read_frame(&mut sock).expect("m1").payload, b"m1");
    // The feed ended, so the close carries the code the projection chose.
    assert_eq!(read_until_close(&mut sock), Some(1001), "closeCode");
}

#[test]
fn a_subprotocol_the_client_did_not_offer_is_not_echoed() {
    let s = start_server_on(WS_MOUNTED_SRC, &[]);
    let (_sock, head) = open_socket(s.port, "/chat");
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    // §4.2.2: the server must not select a subprotocol the client did not send.
    assert!(!head.contains("Sec-WebSocket-Protocol"), "{head}");
}

#[test]
fn an_upgrade_the_projection_refuses_never_opens_a_stream() {
    let s = start_server_on(WS_MOUNTED_SRC, &[]);
    // §4.4: a version we do not speak is answered 426 naming the one we do.
    let (status, raw) = request_raw(
        s.port,
        &format!(
            "GET /chat HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: {WS_KEY}\r\n\
             Sec-WebSocket-Version: 8\r\nConnection: close\r\n\r\n"
        ),
    );
    assert_eq!(status, "HTTP/1.1 426 ", "{raw}");
    assert!(raw.contains("\r\nSec-WebSocket-Version: 13\r\n"), "{raw}");
    // A plain GET at the socket's path is not an upgrade at all.
    let (status, body) = request(
        s.port,
        "GET /chat HTTP/1.1\r\nHost: localhost\r\nSec-WebSocket-Version: 13\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 400 Bad Request", "{body}");
    assert_eq!(body, "not a WebSocket upgrade");
    // And the server is still there: no producer was opened and abandoned.
    let (status, _) = get(s.port, "/elsewhere");
    assert_eq!(status, "HTTP/1.1 404 Not Found");
}

#[test]
fn many_opened_and_dropped_streams_leave_the_cursor_slab_alone() {
    // RFC-0075's `#6156` row at transport scale: every one of these opens a
    // producer and abandons it. The cursor cells come from a slab of 65536, so a
    // release that did not run would show up as a trap rather than as memory
    // growth — the same property `examples/streamunfold.vyrn` measures in-process.
    let s = start_server_on(SSE_SRC, &[]);
    for _ in 0..200 {
        let (live, text) = open_stream(s.port, "/live", 120);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
        drop(live);
    }
    let (status, _) = get(s.port, "/steps");
    assert_eq!(status, "HTTP/1.1 200 OK", "200 opened-and-dropped streams later");
}
