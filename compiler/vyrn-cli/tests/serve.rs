//! Tests `vyrn serve`: spawns the real binary and drives it with raw
//! `TcpStream` requests.
//!
//! The server runs with `--port 0` and names the port it got on stderr; it holds
//! the listener from the moment the OS assigns it, so the harness must never pick
//! a port itself. A `Drop` guard kills the child.

mod serving;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// A running `vyrn serve` child plus drained stdout/stderr buffers. Dropping
/// `server` kills the process, so a panicking test never leaks a listening
/// server.
struct Serve {
    server: serving::Server,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    // Kept alive for the process's lifetime.
    _file: TempFile,
}

/// A temp file that deletes itself on drop.
struct TempFile {
    path: std::path::PathBuf,
}

/// Writes `src` to a path unique per process and call. Not a timestamp: the
/// Windows clock is coarse enough that two tests read the same one and share a file.
fn write_temp_source(src: &str) -> TempFile {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("vyrn-serve-{}-{n}.vyrn", std::process::id()));
    std::fs::write(&path, src).expect("write temp server");
    TempFile { path }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

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

fn start_server_on(src: &str, extra: &[&str]) -> Serve {
    let file = write_temp_source(src);
    let path = file.path.clone();

    let mut child = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("serve")
        .arg(&path)
        .arg("--port")
        .arg("0")
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vyrn serve");

    let stdout = serving::drain(child.stdout.take().unwrap());
    let stderr = serving::drain(child.stderr.take().unwrap());
    let (out, err) = (stdout.0.clone(), stderr.0.clone());
    // The accept loop is live once the banner prints. The wait is long because a
    // saturated machine starts the child slowly; a green run never waits it out.
    let port = serving::wait_for_port(&mut child, stdout, stderr, Duration::from_secs(60))
        .unwrap_or_else(|e| panic!("{e}"));
    Serve {
        server: serving::Server { child, port },
        stdout: out,
        stderr: err,
        _file: file,
    }
}

fn start_server() -> Serve {
    start_server_on(SERVER_SRC, &[])
}

/// Sends a raw request, reads the whole `Connection: close` response, and returns
/// (status line, body).
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

fn get(port: u16, path: &str) -> (String, String) {
    request(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"),
    )
}

#[test]
fn module_state_persists_across_requests() {
    let s = start_server();
    let (_, b1) = get(s.server.port, "/");
    let (_, b2) = get(s.server.port, "/");
    let (_, b3) = get(s.server.port, "/");
    assert_eq!(b1, "hits=1", "first request");
    assert_eq!(b2, "hits=2", "second request (state persisted)");
    assert_eq!(b3, "hits=3", "third request (state persisted)");
}

#[test]
fn handler_trap_yields_500_and_server_survives() {
    let s = start_server();
    let (status, body) = get(s.server.port, "/boom");
    assert_eq!(
        status, "HTTP/1.1 500 Internal Server Error",
        "trap -> 500 status"
    );
    assert_eq!(body, "internal error", "trap -> generic 500 body");

    let err = wait_for(&s.stderr, "division by zero", Duration::from_secs(5));
    assert!(
        err.contains("error: division by zero"),
        "trap logged to stderr:\n{err}"
    );

    let (status, body) = get(s.server.port, "/health");
    assert_eq!(status, "HTTP/1.1 200 OK", "server survived the trap");
    assert_eq!(body, "ok");
}

/// A trap abandons the handler's frames and open regions. Each `/trap` holds
/// about 300 of the 1,000 calls and one of the 64 regions when it traps, so a
/// leak fails by the fourth request, or by the 65th.
#[test]
fn trapped_requests_give_back_their_call_depth_and_regions() {
    let s = start_server_on(
        r#"
fn down(n: Int64, boom: Bool) -> Int64 {
    if n == 0 {
        region {
            if boom { panic("boom") }
        }
        return 0
    }
    return down(n - 1, boom) + 1
}

fn handle(req: Request) -> Response {
    let got = down(300, req.path == "/trap")
    return Response { status: 200, contentType: "text/plain", body: got.toString(), vary: "", headers: [:] }
}
"#,
        &[],
    );
    for _ in 0..65 {
        let (status, _) = get(s.server.port, "/trap");
        assert_eq!(status, "HTTP/1.1 500 Internal Server Error");
    }
    let (status, body) = get(s.server.port, "/deep");
    let err = s.stderr.lock().unwrap().clone();
    assert_eq!(status, "HTTP/1.1 200 OK", "stderr:\n{err}");
    assert_eq!(body, "300");
}

#[test]
fn garbage_request_yields_400_without_reaching_vyrn() {
    let s = start_server();
    let (status, body) = request(s.server.port, "this is not http\r\n\r\n");
    assert_eq!(status, "HTTP/1.1 400 Bad Request", "garbage -> 400");
    assert_eq!(body, "bad request");
    let (status, _) = get(s.server.port, "/health");
    assert_eq!(
        status, "HTTP/1.1 200 OK",
        "server survived the garbage request"
    );
}

#[test]
fn chunked_body_yields_501() {
    let s = start_server();
    let (status, _) = request(
        s.server.port,
        "POST / HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 501 Not Implemented", "chunked -> 501");
}

#[test]
fn post_body_reaches_handle() {
    let s = start_server();
    let (status, body) = request(
        s.server.port,
        "POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(body, "hits=1");
}

#[test]
fn main_startup_print_precedes_first_request() {
    let s = start_server();
    // The banner `start_server` waited for prints after `main` runs. Wait rather
    // than read what the drain thread holds, so only a missing line times out.
    wait_for(&s.stdout, "server up", Duration::from_secs(10));
}

/// A `handle` free of module state, the shape the `--workers` isolation gate
/// admits. `main` may print: it runs once, before any worker starts.
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
    // Wait for the words: the port has arrived, but the rest of the line need not.
    wait_for(&s.stderr, "with 4 workers", Duration::from_secs(10));

    let port = s.server.port;
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

    let out = wait_for(&s.stdout, "server up", Duration::from_secs(10));
    assert_eq!(
        out.matches("server up").count(),
        1,
        "main's print appears exactly once:\n{out}"
    );
}

#[test]
fn workers_survive_a_trap_and_keep_serving() {
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
    let (status, _) = get(s.server.port, "/boom");
    assert_eq!(status, "HTTP/1.1 500 Internal Server Error", "trap -> 500");
    let err = wait_for(&s.stderr, "division by zero", Duration::from_secs(5));
    assert!(
        err.contains("error: division by zero"),
        "canonical wording logged:\n{err}"
    );
    let (status, body) = get(s.server.port, "/health");
    assert_eq!(status, "HTTP/1.1 200 OK", "pool survived the trap");
    assert_eq!(body, "ok");
}

#[test]
fn workers_are_refused_when_handle_touches_module_state() {
    // `bump` puts a hop in the call path the refusal names.
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
    let file = write_temp_source(src);

    let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("serve")
        .arg(&file.path)
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
    let s = start_server();
    let err = s.stderr.lock().unwrap().clone();
    assert!(
        !err.contains("workers"),
        "default banner has no pool:\n{err}"
    );
    let (_, b1) = get(s.server.port, "/");
    assert_eq!(b1, "hits=1");
}

/// The host end of the response header map: what the writer puts on
/// the wire. `std/http`'s projections that produce these are in `tests/http.rs`.
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
        s.server.port,
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
        s.server.port,
        "GET /cond HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 304 Not Modified", "{raw}");
    // RFC 9110 15.4.5: the metadata the 200 would have sent, and no content.
    assert!(raw.contains("\r\nETag: \"abc\"\r\n"), "{raw}");
    assert!(raw.contains("\r\nCache-Control: max-age=60\r\n"), "{raw}");
    assert!(raw.contains("\r\nVary: Accept\r\n"), "{raw}");
    // An empty content type writes no field, rather than a malformed one.
    assert!(
        !raw.contains("Content-Type"),
        "a 304 declares no media type:\n{raw}"
    );
    assert!(
        raw.ends_with("\r\n\r\n"),
        "nothing after the header block:\n{raw}"
    );
}

/// Returns (status line, whole response text), header block included.
fn request_raw(port: u16, raw: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.write_all(raw.as_bytes()).expect("write request");
    stream.flush().ok();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read response");
    let status = resp.lines().next().unwrap_or("").to_string();
    (status, resp)
}

// The streaming response and the disconnect signal: a
// client that vanishes runs the producer's release before the next event. The
// source calls `serveStream` directly, below `std/http`, to pin the mechanism.
// Two witnesses, because "production stopped" and "the release ran" differ:
// `/steps` counts the step function's runs and must stop moving; `/probe` reads
// the stream's cursor cell through the `Ref` the step parked, and traps
// ("slots: handle is not alive") once the release has freed it.
const SSE_SRC: &str = r#"
import { Cursor, cursorGet, cursorSet, unfold, map } from "std/stream"

let mut steps: Int64 = 0
let mut saved: Cursor = Cursor { slot: 0 - 1, gen: 0 }

/// An endless feed: it never answers `None`, so only the client going away can
/// end it.
fn tick(c: Cursor) -> Option<String> {
    steps = steps + 1
    saved = c
    let n = cursorGet(c)
    cursorSet(c, n + 1)
    return Some("id: \{n}\ndata: e\{n}\n\n")
}

/// The same feed, unencoded, plus the encoder — which is what a `map` over a
/// live feed is for. A live feed maps only because the combinators are
/// lazy: an eager `map` would pull the endless feed and
/// never return.
fn nums(c: Cursor) -> Option<Int64> {
    steps = steps + 1
    saved = c
    let n = cursorGet(c)
    cursorSet(c, n + 1)
    return Some(n)
}

fn frame(n: Int64) -> String {
    return "id: \{n}\ndata: e\{n}\n\n"
}

/// A feed with nothing to say — the 204 path.
fn silent(c: Cursor) -> Option<String> {
    steps = steps + 1
    return None
}

fn handle(req: Request) -> Response {
    if req.path == "/live" {
        serveStream(unfold(0, tick))
        return Response { status: 200, contentType: "text/event-stream", body: "retry: 500\n\n", vary: "", headers: [:] }
    }
    if req.path == "/mapped" {
        serveStream(map(unfold(0, nums), frame))
        return Response { status: 200, contentType: "text/event-stream", body: "retry: 500\n\n", vary: "", headers: [:] }
    }
    if req.path == "/empty" {
        serveStream(unfold(0, silent))
        return Response { status: 200, contentType: "text/event-stream", body: "retry: 500\n\n", vary: "", headers: [:] }
    }
    if req.path == "/steps" {
        return Response { status: 200, contentType: "text/plain", body: "\{steps}", vary: "", headers: [:] }
    }
    if req.path == "/probe" {
        return Response { status: 200, contentType: "text/plain", body: "\{cursorGet(saved)}", vary: "", headers: [:] }
    }
    return Response { status: 404, contentType: "text/plain", body: "no", vary: "", headers: [:] }
}
"#;

/// Opens `path`, reads until `want` bytes have arrived or the read deadline
/// passes, and returns the connection still open. The deadline keeps an endless
/// stream from hanging the suite.
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
    let (live, text) = open_stream(s.server.port, "/live", 200);
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
    assert!(
        text.contains("\r\nContent-Type: text/event-stream\r\n"),
        "{text}"
    );
    // A stream's body ends when the connection does, so it has no length.
    assert!(
        !text.contains("Content-Length"),
        "a stream declares no length:\n{text}"
    );
    assert!(text.contains("\r\nCache-Control: no-store\r\n"), "{text}");
    // `Response.body` is the stream's prologue, written once before the first
    // frame: where SSE's reconnect hint belongs.
    let (_, after) = text.split_once("\r\n\r\n").expect("header block");
    assert!(
        after.starts_with("retry: 500\n\n"),
        "prologue first:\n{after}"
    );
    assert!(
        after.contains("id: 0\ndata: e0\n\n"),
        "the first frame:\n{after}"
    );
    assert!(
        after.contains("id: 1\ndata: e1\n\n"),
        "and the second:\n{after}"
    );
    drop(live);
}

#[test]
fn a_producer_with_nothing_to_say_answers_204_rather_than_an_empty_stream() {
    let s = start_server_on(SSE_SRC, &[]);
    // 204 is the one status a plain `EventSource` reads as "do not reconnect"
    // (WHATWG HTML 9.2.5), so a finished feed does not start a reconnect loop.
    let (status, raw) = request_raw(
        s.server.port,
        "GET /empty HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 204 No Content", "{raw}");
    assert!(
        !raw.contains("text/event-stream"),
        "no stream was opened:\n{raw}"
    );
    let (status, body) = get(s.server.port, "/steps");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(
        body, "1",
        "the silent step ran once and was not asked again"
    );
}

#[test]
fn a_client_that_vanishes_runs_the_producers_release_before_the_next_event() {
    let s = start_server_on(SSE_SRC, &[]);
    let (live, text) = open_stream(s.server.port, "/live", 200);
    assert!(
        text.contains("id: 0\ndata: e0\n\n"),
        "events were flowing:\n{text}"
    );

    // The server learns of the disconnect only by writing to the dead socket.
    drop(live);

    // The first `/steps` blocks until the pump notices, so its answer is already
    // the final count.
    let (status, settled) = get(s.server.port, "/steps");
    assert_eq!(
        status, "HTTP/1.1 200 OK",
        "the server survived the disconnect"
    );
    std::thread::sleep(Duration::from_millis(300));
    let (_, later) = get(s.server.port, "/steps");
    assert_eq!(
        settled, later,
        "the producer kept running after the client went away ({settled} -> {later})"
    );

    // Stopped production could be a stuck pump; a released cell could only have
    // come from `close`.
    let (status, body) = get(s.server.port, "/probe");
    assert_eq!(
        status, "HTTP/1.1 500 Internal Server Error",
        "cursor still live: {body}"
    );
    let err = wait_for(
        &s.stderr,
        "slots: handle is not alive",
        Duration::from_secs(5),
    );
    assert!(err.contains("error: slots: handle is not alive"), "{err}");

    let (status, _) = get(s.server.port, "/steps");
    assert_eq!(status, "HTTP/1.1 200 OK");
}

#[test]
fn a_mapped_feed_streams_and_its_release_walks_the_chain() {
    // The host holds only the `map` wrapper, so its release must walk to the
    // inner feed: `saved`, which `/probe` reads, is the inner producer's cursor.
    let s = start_server_on(SSE_SRC, &[]);
    let (live, text) = open_stream(s.server.port, "/mapped", 200);
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
    let (_, after) = text.split_once("\r\n\r\n").expect("header block");
    assert!(
        after.contains("id: 0\ndata: e0\n\n"),
        "a mapped frame:\n{after}"
    );
    assert!(
        after.contains("id: 1\ndata: e1\n\n"),
        "and the next:\n{after}"
    );

    drop(live);
    let (status, settled) = get(s.server.port, "/steps");
    assert_eq!(
        status, "HTTP/1.1 200 OK",
        "the server survived the disconnect"
    );
    std::thread::sleep(Duration::from_millis(300));
    let (_, later) = get(s.server.port, "/steps");
    assert_eq!(
        settled, later,
        "the feed kept running behind the map after the client went away ({settled} -> {later})"
    );

    let (status, body) = get(s.server.port, "/probe");
    assert_eq!(
        status, "HTTP/1.1 500 Internal Server Error",
        "cursor still live: {body}"
    );
    let err = wait_for(
        &s.stderr,
        "slots: handle is not alive",
        Duration::from_secs(5),
    );
    assert!(err.contains("error: slots: handle is not alive"), "{err}");
}

// The same mechanism through WebSocket framing, with `SSE_SRC`'s two witnesses
// (`/steps`, `/probe`), also below `std/http`.
const WS_SRC: &str = r#"
import { Cursor, cursorGet, cursorSet, unfold } from "std/stream"
import { sha1 } from "std/hash"
import { base64EncodeBytes } from "std/codecs"

let mut steps: Int64 = 0
let mut saved: Cursor = Cursor { slot: 0 - 1, gen: 0 }

/// An endless feed of PAYLOADS, not frames: the rule is that Vyrn owns
/// what the user chooses and the host owns what the protocol fixes, and there is
/// no choice in an opcode, a length or a mask.
fn tick(c: Cursor) -> Option<String> {
    steps = steps + 1
    saved = c
    let n = cursorGet(c)
    cursorSet(c, n + 1)
    return Some("e\{n}")
}

/// One 100-byte message, then the end — the fragmentation and close-code case.
fn once(c: Cursor) -> Option<String> {
    let n = cursorGet(c)
    if n > 0 {
        return None
    }
    cursorSet(c, n + 1)
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
        Some(v) => v.copy(),
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
        body: params.copy(),
        vary: "",
        headers: ["Upgrade": "websocket", "Connection": "Upgrade", "Sec-WebSocket-Accept": accept(req)],
    }
}

fn handle(req: Request) -> Response {
    if req.path == "/socket" {
        serveStream(unfold(0, tick))
        return upgrade(req, "1000 0")
    }
    if req.path == "/split" {
        serveStream(unfold(0, once))
        return upgrade(req, "1001 40")
    }
    if req.path == "/steps" {
        return Response { status: 200, contentType: "text/plain", body: "\{steps}", vary: "", headers: [:] }
    }
    if req.path == "/probe" {
        return Response { status: 200, contentType: "text/plain", body: "\{cursorGet(saved)}", vary: "", headers: [:] }
    }
    return Response { status: 404, contentType: "text/plain", body: "no", vary: "", headers: [:] }
}
"#;

/// RFC 6455 1.3's worked example: this key must produce this accept value, which
/// pins `std/hash`'s SHA-1 on the wire.
const WS_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
const WS_ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

/// Upgrades and returns the open socket plus the handshake head.
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

/// Reads one server frame. Panics on a masked one: RFC 6455 5.1 forbids a server
/// to mask.
fn read_frame(s: &mut TcpStream) -> Option<Frame> {
    let mut h = [0u8; 2];
    s.read_exact(&mut h).ok()?;
    assert_eq!(
        h[1] & 0x80,
        0,
        "a server frame must not be masked (RFC 6455 5.1)"
    );
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
    Some(Frame {
        fin: h[0] & 0x80 != 0,
        opcode: h[0] & 0x0f,
        payload,
    })
}

/// Reads frames until a close arrives and returns its code. Bounded, so a server
/// that never closes fails rather than hangs. The bound is large because the pump
/// sees a client's close only at a frame boundary, and a fast runner pushes
/// hundreds of frames of the endless producer first.
fn read_until_close(s: &mut TcpStream) -> Option<u16> {
    for _ in 0..8192 {
        let f = read_frame(s)?;
        if f.opcode == 8 {
            return Some(u16::from_be_bytes([f.payload[0], f.payload[1]]));
        }
    }
    None
}

/// `mask` false is the protocol violation RFC 6455 5.1 names.
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
    let (mut sock, head) = open_socket(s.server.port, "/socket");
    assert!(
        head.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
        "{head}"
    );
    assert!(head.contains("\r\nUpgrade: websocket\r\n"), "{head}");
    assert!(head.contains("\r\nConnection: Upgrade\r\n"), "{head}");
    assert!(
        head.contains(&format!("\r\nSec-WebSocket-Accept: {WS_ACCEPT}\r\n")),
        "{head}"
    );
    // The head's `body` is the host's framing parameters and is written nowhere.
    assert!(
        head.ends_with("\r\n\r\n"),
        "nothing after the header block:\n{head}"
    );
    assert!(
        !head.contains("1000 0"),
        "the framing parameters are not written:\n{head}"
    );

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
    let s = start_server_on(WS_SRC, &[]);
    let (mut sock, head) = open_socket(s.server.port, "/socket");
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    assert_eq!(
        read_frame(&mut sock).expect("a frame").payload,
        b"e0",
        "frames were flowing"
    );
    read_frame(&mut sock).expect("a second frame");

    drop(sock);

    // Blocks until the pump notices, so this answer is already the final count.
    let (status, settled) = get(s.server.port, "/steps");
    assert_eq!(
        status, "HTTP/1.1 200 OK",
        "the server survived the disconnect"
    );
    std::thread::sleep(Duration::from_millis(300));
    let (_, later) = get(s.server.port, "/steps");
    assert_eq!(
        settled, later,
        "the producer kept running after the client went away ({settled} -> {later})"
    );

    let (status, body) = get(s.server.port, "/probe");
    assert_eq!(
        status, "HTTP/1.1 500 Internal Server Error",
        "cursor still live: {body}"
    );
    let err = wait_for(
        &s.stderr,
        "slots: handle is not alive",
        Duration::from_secs(5),
    );
    assert!(err.contains("error: slots: handle is not alive"), "{err}");
}

#[test]
fn a_client_close_is_answered_with_a_close_frame() {
    // RFC 6455 5.5.1. The pump sees the close only at the next frame boundary,
    // because it blocks in the producer between frames.
    let s = start_server_on(WS_SRC, &[]);
    let (mut sock, _) = open_socket(s.server.port, "/socket");
    read_frame(&mut sock).expect("a frame");
    write_client_frame(&mut sock, 8, &1000u16.to_be_bytes(), true);
    assert_eq!(
        read_until_close(&mut sock),
        Some(1000),
        "the close is answered"
    );
    let (status, _) = get(s.server.port, "/probe");
    assert_eq!(
        status, "HTTP/1.1 500 Internal Server Error",
        "the cursor was released"
    );
}

#[test]
fn an_unmasked_client_frame_closes_with_1002() {
    // RFC 6455 5.1: every client frame is masked. Server-push ignores client
    // messages, but a frame that breaks the framing rules is still an error.
    let s = start_server_on(WS_SRC, &[]);
    let (mut sock, _) = open_socket(s.server.port, "/socket");
    read_frame(&mut sock).expect("a frame");
    write_client_frame(&mut sock, 1, b"hello", false);
    assert_eq!(read_until_close(&mut sock), Some(1002), "a protocol error");
}

#[test]
fn max_frame_splits_a_message_and_the_feeds_end_carries_the_programs_close_code() {
    // `/split` yields one 100-byte message under a 40-byte limit, then ends.
    // RFC 6455 5.4: the first fragment carries the data opcode, the rest carry
    // the continuation opcode, and only the last sets FIN.
    let s = start_server_on(WS_SRC, &[]);
    let (mut sock, head) = open_socket(s.server.port, "/split");
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    let a = read_frame(&mut sock).expect("first fragment");
    let b = read_frame(&mut sock).expect("second fragment");
    let c = read_frame(&mut sock).expect("last fragment");
    assert_eq!((a.opcode, a.fin, a.payload.len()), (1, false, 40));
    assert_eq!((b.opcode, b.fin, b.payload.len()), (0, false, 40));
    assert_eq!((c.opcode, c.fin, c.payload.len()), (0, true, 20));
    let whole: Vec<u8> = [a.payload, b.payload, c.payload].concat();
    assert_eq!(whole.len(), 100, "the message reassembles");
    assert_eq!(
        read_until_close(&mut sock),
        Some(1001),
        "closeCode reached the wire"
    );
}

/// `std/http`'s `ws`, mounted, writing its own handshake: the projection over the
/// mechanism `WS_SRC` pins. It lives here because `tests/http.rs` has no
/// `vyrn serve` harness.
const WS_MOUNTED_SRC: &str = r#"
import { Frames, Live, Route, Socket, mount, ws } from "std/http"
import * as stream from "std/stream"

fn step(c: stream.Cursor) -> Option<String> {
    let n = stream.cursorGet(c)
    if n > 1 {
        return None
    }
    stream.cursorSet(c, n + 1)
    return Some("m\{n}")
}

fn feed(req: Request, ps: Map<String, String>, since: Int64) -> Stream<String> {
    return stream.unfold(since, step)
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
    let mut sock = TcpStream::connect(("127.0.0.1", s.server.port)).expect("connect");
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
    assert!(
        head.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
        "{head}"
    );
    assert!(
        head.contains(&format!("\r\nSec-WebSocket-Accept: {WS_ACCEPT}\r\n")),
        "{head}"
    );
    assert!(
        head.contains("\r\nSec-WebSocket-Protocol: vyrn.v1\r\n"),
        "{head}"
    );
    assert_eq!(read_frame(&mut sock).expect("m0").payload, b"m0");
    assert_eq!(read_frame(&mut sock).expect("m1").payload, b"m1");
    assert_eq!(read_until_close(&mut sock), Some(1001), "closeCode");
}

#[test]
fn a_subprotocol_the_client_did_not_offer_is_not_echoed() {
    let s = start_server_on(WS_MOUNTED_SRC, &[]);
    let (_sock, head) = open_socket(s.server.port, "/chat");
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    // RFC 6455 4.2.2: the server must not select a subprotocol the client did
    // not send.
    assert!(!head.contains("Sec-WebSocket-Protocol"), "{head}");
}

#[test]
fn an_upgrade_the_projection_refuses_never_opens_a_stream() {
    let s = start_server_on(WS_MOUNTED_SRC, &[]);
    // RFC 6455 4.4: an unknown version is answered 426, naming the one spoken.
    let (status, raw) = request_raw(
        s.server.port,
        &format!(
            "GET /chat HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: {WS_KEY}\r\n\
             Sec-WebSocket-Version: 8\r\nConnection: close\r\n\r\n"
        ),
    );
    assert_eq!(status, "HTTP/1.1 426 ", "{raw}");
    assert!(raw.contains("\r\nSec-WebSocket-Version: 13\r\n"), "{raw}");
    let (status, body) = request(
        s.server.port,
        "GET /chat HTTP/1.1\r\nHost: localhost\r\nSec-WebSocket-Version: 13\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 400 Bad Request", "{body}");
    assert_eq!(body, "not a WebSocket upgrade");
    let (status, _) = get(s.server.port, "/elsewhere");
    assert_eq!(status, "HTTP/1.1 404 Not Found");
}

#[test]
fn many_opened_and_dropped_streams_leave_the_cursor_slab_alone() {
    // Cursor cells come from a slab of 65536, so a release that did not run shows up
    // as a trap, not as growth (`examples/streamunfold.vyrn` checks the same
    // in-process).
    let s = start_server_on(SSE_SRC, &[]);
    for _ in 0..200 {
        let (live, text) = open_stream(s.server.port, "/live", 120);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
        drop(live);
    }
    let (status, _) = get(s.server.port, "/steps");
    assert_eq!(
        status, "HTTP/1.1 200 OK",
        "200 opened-and-dropped streams later"
    );
}
