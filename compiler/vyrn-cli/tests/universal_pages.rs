//! Integration tests for RFC-0069 universal pages — the pastebin's `handle`
//! driven through a real `vyrn serve`, exercising both channels of the page
//! router:
//!
//!   * an UNMARKED request is served as HTML exactly as before (the soft-nav
//!     document channel is byte-for-byte unchanged);
//!   * a MARKED request (`?__vyrn=data`) is answered with the
//!     `{page, title, props[, params]}` JSON payload, running `load()` exactly
//!     as SSR would — the home list, a paste's `load()` props round-trip, the
//!     static `/about` payload, the `@error` payload on a miss, and the
//!     non-client `/raw/*` route falling back to its real (non-JSON) response.
//!
//! The store is file-backed (`data/pastes.json` relative to the process cwd), so
//! the server runs in a fresh temp dir — an empty store the test seeds through
//! the RPC surface, isolated from the repo's `examples/bin/data`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel).canonicalize().unwrap()
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c
}

struct Serve {
    #[allow(dead_code)]
    child: Child,
    port: u16,
    stderr: Arc<Mutex<String>>,
    _dir: PathBuf,
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

fn drain_into<R: Read + Send + 'static>(mut r: R, acc: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => acc.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
    });
}

fn wait_for(acc: &Arc<Mutex<String>>, needle: &str, timeout: Duration) {
    let start = Instant::now();
    loop {
        if acc.lock().unwrap().contains(needle) {
            return;
        }
        if start.elapsed() > timeout {
            panic!("timed out waiting for {needle:?}; got:\n{}", acc.lock().unwrap());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The port of a shared `vyrn serve examples/bin/server.vyrn`, started ONCE for the
/// whole suite in a fresh temp cwd (an empty file store the tests seed). Generation
/// is expensive (~10s, cache disabled); sharing one server keeps the suite fast and
/// dodges the readiness-timeout that N parallel generations would blow. The child is
/// intentionally leaked — it lives until the test process exits.
fn bin_port() -> u16 {
    static PORT: OnceLock<u16> = OnceLock::new();
    *PORT.get_or_init(|| {
        let server = repo_file("examples/bin/server.vyrn");
        let dir = std::env::temp_dir().join(format!("vyrn_upages_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let port = free_port();
        let mut child = vyrn()
            .arg("serve")
            .arg(&server)
            .arg("--port")
            .arg(port.to_string())
            .current_dir(&dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn vyrn serve");
        // The `serving` banner goes to stdout, generation errors to stderr — combine
        // both so the wait sees the banner and a failure surfaces its cause.
        let out = Arc::new(Mutex::new(String::new()));
        drain_into(child.stdout.take().unwrap(), out.clone());
        drain_into(child.stderr.take().unwrap(), out.clone());
        let s = Serve { child, port, stderr: out, _dir: dir };
        wait_for(&s.stderr, "serving", Duration::from_secs(60));
        std::mem::forget(s); // keep the server alive for the whole run
        port
    })
}

/// Send a raw request, read the whole `Connection: close` response, split into
/// (status_line, headers, body).
fn request(port: u16, raw: &str) -> (String, String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.write_all(raw.as_bytes()).expect("write");
    stream.flush().ok();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read");
    let (head, body) = resp.split_once("\r\n\r\n").unwrap_or((resp.as_str(), ""));
    let status = head.lines().next().unwrap_or("").to_string();
    (status, head.to_string(), body.to_string())
}

fn get(port: u16, path: &str) -> (String, String, String) {
    request(port, &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"))
}

fn post(port: u16, path: &str, body: &str) -> (String, String, String) {
    request(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

fn content_type(headers: &str) -> String {
    for line in headers.lines() {
        if let Some(v) = line.strip_prefix("Content-Type:").or_else(|| line.strip_prefix("content-type:")) {
            return v.trim().to_string();
        }
    }
    String::new()
}

/// Seed one paste through the RPC surface; return its server-assigned id.
fn create_paste(port: u16, title: &str, body: &str, lang: &str) -> String {
    let req = format!("{{\"title\":\"{title}\",\"body\":\"{body}\",\"lang\":\"{lang}\"}}");
    let (status, _h, resp) = post(port, "/rpc/createPaste", &req);
    assert_eq!(status, "HTTP/1.1 200 OK", "createPaste failed: {resp}");
    // Result procedure → 200 `{"Ok":{...paste...}}`. Pull the id field.
    let key = "\"id\":\"";
    let i = resp.find(key).expect("paste id in create response") + key.len();
    let j = resp[i..].find('"').unwrap() + i;
    resp[i..j].to_string()
}

// ---- the document channel is unchanged -------------------------------------

#[test]
fn unmarked_about_is_html() {
    let port = bin_port();
    let (status, headers, body) = get(port, "/about");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(content_type(&headers), "text/html");
    // The full themed page (shell + body), not a JSON payload.
    assert!(body.contains("<!doctype html>") || body.contains("<html"), "expected an HTML document, got:\n{body}");
    assert!(body.contains("About"));
}

#[test]
fn unmarked_missing_paste_is_404_html() {
    let port = bin_port();
    let (status, headers, _body) = get(port, "/p/nope404");
    assert_eq!(status, "HTTP/1.1 404 Not Found");
    assert_eq!(content_type(&headers), "text/html");
}

// ---- the data channel (RFC-0069 §2) ----------------------------------------

#[test]
fn marked_about_is_the_exact_static_payload() {
    let port = bin_port();
    let (status, headers, body) = get(port, "/about?__vyrn=data");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(content_type(&headers), "application/json");
    // A static page: empty props, empty params, the url-pattern title/id.
    assert_eq!(body, "{\"page\":\"/about\",\"title\":\"/about\",\"props\":null,\"params\":null}");
}

#[test]
fn marked_home_payload_carries_the_loaded_list() {
    let port = bin_port();
    let id = create_paste(port, "hello", "world", "text");
    let (status, headers, body) = get(port, "/?__vyrn=data");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(content_type(&headers), "application/json");
    assert!(body.starts_with("{\"page\":\"/\",\"title\":"), "unexpected payload:\n{body}");
    // props is the load() result — the paste array, carrying the seeded paste.
    assert!(body.contains("\"props\":["));
    assert!(body.contains(&format!("\"id\":\"{id}\"")));
    assert!(body.contains("\"title\":\"hello\""));
}

#[test]
fn marked_paste_props_round_trip_through_the_wire_codec() {
    let port = bin_port();
    let id = create_paste(port, "deep title", "the body text", "text");
    let (status, headers, body) = get(port, &format!("/p/{id}?__vyrn=data"));
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(content_type(&headers), "application/json");
    assert!(body.starts_with("{\"page\":\"/p/:id\","), "unexpected payload:\n{body}");
    // The rendered title travels in the payload (the paste title, via head{}).
    assert!(body.contains("\"title\":\"deep title\""), "payload:\n{body}");
    // props is the loaded Paste; params carries the matched route id.
    assert!(body.contains(&format!("\"props\":{{\"id\":\"{id}\"")));
    assert!(body.contains("\"body\":\"the body text\""));
    assert!(body.contains(&format!("\"params\":{{\"id\":\"{id}\"}}")));
}

#[test]
fn marked_missing_paste_is_the_error_payload() {
    let port = bin_port();
    let (status, headers, body) = get(port, "/p/ghost?__vyrn=data");
    // A miss on the DATA channel is a 200 carrying the @error payload (the client
    // renders the themed error page); the document channel still 404s.
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(content_type(&headers), "application/json");
    assert!(body.starts_with("{\"page\":\"@error\",\"status\":404,"), "unexpected payload:\n{body}");
    assert!(body.contains("\"props\":{\"status\":404,"));
}

#[test]
fn marked_non_client_route_falls_back_to_its_real_response() {
    let port = bin_port();
    let id = create_paste(port, "raw", "raw body content", "text");
    // /raw/[id] is a `.vyrn` respond page — NOT in the client bundle. A marked
    // request must NOT be answered as JSON, so the client hard-navs to it.
    let (status, headers, body) = get(port, &format!("/raw/{id}?__vyrn=data"));
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(!content_type(&headers).contains("application/json"), "raw route must not be JSON: {}", content_type(&headers));
    assert!(body.contains("raw body content"));
}
