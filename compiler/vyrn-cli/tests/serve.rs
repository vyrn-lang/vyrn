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
        return Response { status: 200, contentType: "text/plain", body: "ok" }
    }
    if req.path == "/boom" {
        let z = hits - hits
        let bad = hits / z
        return Response { status: 200, contentType: "text/plain", body: bad.toString() }
    }
    return Response { status: 200, contentType: "text/plain", body: "hits=\{hits.toString()}" }
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
        return Response { status: 200, contentType: "text/plain", body: fib(20).toString() }
    }
    return Response { status: 200, contentType: "text/plain", body: "echo:\{req.path}" }
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
        return Response { status: 200, contentType: "text/plain", body: (n / z).toString() }
    }
    return Response { status: 200, contentType: "text/plain", body: "ok" }
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
    return Response { status: 200, contentType: "text/plain", body: bump().toString() }
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
