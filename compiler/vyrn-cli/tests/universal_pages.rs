//! Universal pages: the pastebin's `handle` through a real `vyrn serve`, with
//! both representations the page router negotiates at one URL. A document
//! request gets the same HTML bytes whether it sends `Accept: text/html`, nothing, or a
//! browser's navigation `Accept`. An `Accept: application/json` request gets the
//! `{page, title, props[, params]}` payload with `Vary: Accept`, running `load()` as SSR
//! would; the non-client `/raw/*` route keeps its real response. No response names the
//! language or the framework.
//!
//! The store is file-backed (`data/pastes.json` under the cwd), so the server runs in a
//! fresh temp dir the test seeds through the RPC surface. The server takes `--port 0`
//! and prints the port it holds; the harness reads it and never picks a port itself.

mod serving;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

fn repo_file(rel: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap();
    // `std::fs::canonicalize` on Windows returns a `\\?\` verbatim path, which wedges the
    // pages generator's relative-import resolution under `vyrn serve`. Strip the prefix.
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(stripped) => PathBuf::from(stripped),
        None => p,
    }
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c
}

/// The shared `vyrn serve examples/bin/server.vyrn` in a fresh temp cwd. Generation is
/// expensive, so concurrent tests share one server: each holds the handle, the static
/// holds only a `Weak`, and the last test to finish kills it. A failure to start is
/// recorded once, so later tests fail fast on the cause.
fn bin_server() -> Arc<serving::Server> {
    static SHARED: Mutex<Weak<serving::Server>> = Mutex::new(Weak::new());
    static FAILED: OnceLock<String> = OnceLock::new();
    let mut shared = SHARED.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(e) = FAILED.get() {
        panic!("shared bin server failed to start: {e}");
    }
    if let Some(s) = shared.upgrade() {
        return s;
    }
    match spawn_bin_server() {
        Ok(s) => {
            let s = Arc::new(s);
            *shared = Arc::downgrade(&s);
            s
        }
        Err(e) => {
            let _ = FAILED.set(e.clone());
            drop(shared);
            panic!("shared bin server failed to start: {e}");
        }
    }
}

fn spawn_bin_server() -> Result<serving::Server, String> {
    let server = repo_file("examples/bin/server.vyrn");
    let dir = std::env::temp_dir().join(format!("vyrn_upages_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).unwrap();
    let mut child = vyrn()
        .arg("serve")
        .arg(&server)
        .arg("--port")
        .arg("0")
        .current_dir(&dir)
        // Set every stdio explicitly, `stdin` included: a child that inherits the
        // console holds the runner's stdout pipe open, so `cargo test ... | tail` hangs
        // on a suite that passed. Do not drop any of these three.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn vyrn serve: {e}"))?;
    let stdout = serving::drain(child.stdout.take().unwrap());
    let stderr = serving::drain(child.stderr.take().unwrap());
    // Cold, cache-disabled generation of the whole bin app in a debug build takes
    // minutes; 600s is the ceiling.
    let port = serving::wait_for_port(&mut child, stdout, stderr, Duration::from_secs(600))?;
    Ok(serving::Server { child, port })
}

/// Send a raw request, read the whole `Connection: close` response, split into
/// (status_line, headers, body).
fn request(port: u16, raw: &str) -> (String, String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    // A response path that fails to close the socket must fail the test, not hang the
    // suite on an unbounded read_to_string.
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.write_all(raw.as_bytes()).expect("write");
    stream.flush().ok();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read");
    let (head, body) = resp.split_once("\r\n\r\n").unwrap_or((resp.as_str(), ""));
    let status = head.lines().next().unwrap_or("").to_string();
    (status, head.to_string(), body.to_string())
}

fn get(port: u16, path: &str) -> (String, String, String) {
    request(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"),
    )
}

/// A GET stating the representation it wants: the whole wire difference
/// between a page's document and its data payload.
fn get_accept(port: u16, path: &str, accept: &str) -> (String, String, String) {
    request(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nAccept: {accept}\r\nConnection: close\r\n\r\n"),
    )
}

/// The JSON representation of a page.
fn get_data(port: u16, path: &str) -> (String, String, String) {
    get_accept(port, path, "application/json")
}

fn header(headers: &str, name: &str) -> String {
    for line in headers.lines() {
        let (n, v) = match line.split_once(':') {
            Some(p) => p,
            None => continue,
        };
        if n.trim().eq_ignore_ascii_case(name) {
            return v.trim().to_string();
        }
    }
    String::new()
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
    header(headers, "content-type")
}

/// Seeds one paste through the RPC surface; returns its server-assigned id.
fn create_paste(port: u16, title: &str, body: &str, lang: &str) -> String {
    let req = format!("{{\"title\":\"{title}\",\"body\":\"{body}\",\"lang\":\"{lang}\"}}");
    let (status, _h, resp) = post(port, "/_/pastes/create", &req);
    assert_eq!(status, "HTTP/1.1 200 OK", "pastes/create failed: {resp}");
    // A Result procedure answers 200 `{"Ok":{...paste...}}`. Pull the id field.
    let key = "\"id\":\"";
    let i = resp.find(key).expect("paste id in create response") + key.len();
    let j = resp[i..].find('"').unwrap() + i;
    resp[i..j].to_string()
}

/// Content negotiation must not move a byte of the document channel: `Accept: text/html`,
/// no `Accept`, and a browser's navigation `Accept` get the same status, headers and
/// body, and no `Vary` header, since the document is what this URL answers by default.
#[test]
#[ignore = "generates the full bin app cold (minutes in a debug build) - run with the parity tier: cargo test --test universal_pages -- --ignored"]
fn every_document_accept_is_byte_identical() {
    let server = bin_server();
    let port = server.port;
    const BROWSER: &str =
        "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";
    for path in ["/about", "/", "/p/nope404"] {
        let bare = get(port, path);
        for accept in ["text/html", BROWSER, "*/*", "text/html,application/json"] {
            let stated = get_accept(port, path, accept);
            assert_eq!(
                bare.0, stated.0,
                "{path} status moved under Accept: {accept}"
            );
            assert_eq!(bare.2, stated.2, "{path} body moved under Accept: {accept}");
            assert_eq!(content_type(&stated.1), "text/html", "{path} @ {accept}");
            assert_eq!(
                header(&stated.1, "vary"),
                "",
                "a document must not Vary: {path} @ {accept}"
            );
        }
    }
}

#[test]
#[ignore = "generates the full bin app cold (minutes in a debug build) - run with the parity tier: cargo test --test universal_pages -- --ignored"]
fn document_about_is_html() {
    let server = bin_server();
    let port = server.port;
    let (status, headers, body) = get_accept(port, "/about", "text/html");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(content_type(&headers), "text/html");
    // The full themed page (shell + body), not a JSON payload.
    assert!(
        body.contains("<!doctype html>") || body.contains("<html"),
        "expected an HTML document, got:\n{body}"
    );
    assert!(body.contains("About"));
}

#[test]
#[ignore = "generates the full bin app cold (minutes in a debug build) - run with the parity tier: cargo test --test universal_pages -- --ignored"]
fn unmarked_lazy_home_is_byte_identical_and_never_renders_the_skeleton() {
    // The home loader is `lazy`, but the first load is full SSR: the server
    // has the data, renders the `Ready` arm, and never shows the `Loading` skeleton.
    let server = bin_server();
    let port = server.port;
    let (status, headers, body) = get(port, "/");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(content_type(&headers), "text/html");
    // The shell prefix (create-island mount and headings) is fixed, whether or not the
    // shared store has pastes by the time this runs.
    assert!(
        body.contains(
            "<main><p class=\"sub\">Paste text, get a short link. Persisted to disk.</p><div id=\"app\"></div><h2>Recent pastes</h2>"
        ),
        "home shell changed:\n{body}"
    );
    // The lazy skeleton must not leak into SSR: no spinner, no loading label.
    assert!(
        !body.contains("spinner"),
        "lazy skeleton leaked into SSR:\n{body}"
    );
    assert!(
        !body.contains("Loading recent pastes"),
        "lazy loading label leaked into SSR:\n{body}"
    );
}

#[test]
#[ignore = "generates the full bin app cold (minutes in a debug build) - run with the parity tier: cargo test --test universal_pages -- --ignored"]
fn unmarked_missing_paste_is_404_html() {
    let server = bin_server();
    let port = server.port;
    let (status, headers, _body) = get(port, "/p/nope404");
    assert_eq!(status, "HTTP/1.1 404 Not Found");
    assert_eq!(content_type(&headers), "text/html");
}

#[test]
#[ignore = "generates the full bin app cold (minutes in a debug build) - run with the parity tier: cargo test --test universal_pages -- --ignored"]
fn marked_about_is_the_exact_static_payload() {
    let server = bin_server();
    let port = server.port;
    let (status, headers, body) = get_data(port, "/about");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(content_type(&headers), "application/json");
    // A static page: empty props, empty params, the url-pattern title/id.
    assert_eq!(
        body,
        "{\"page\":\"/about\",\"title\":\"/about\",\"props\":null,\"params\":null}"
    );
}

#[test]
#[ignore = "generates the full bin app cold (minutes in a debug build) - run with the parity tier: cargo test --test universal_pages -- --ignored"]
fn marked_home_payload_carries_the_loaded_list() {
    let server = bin_server();
    let port = server.port;
    let id = create_paste(port, "hello", "world", "text");
    let (status, headers, body) = get_data(port, "/");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(content_type(&headers), "application/json");
    assert!(
        body.starts_with("{\"page\":\"/\",\"title\":"),
        "unexpected payload:\n{body}"
    );
    // props is the load() result: the paste array, carrying the seeded paste.
    assert!(body.contains("\"props\":["));
    assert!(body.contains(&format!("\"id\":\"{id}\"")));
    assert!(body.contains("\"title\":\"hello\""));
}

#[test]
#[ignore = "generates the full bin app cold (minutes in a debug build) - run with the parity tier: cargo test --test universal_pages -- --ignored"]
fn marked_paste_props_round_trip_through_the_wire_codec() {
    let server = bin_server();
    let port = server.port;
    let id = create_paste(port, "deep title", "the body text", "text");
    let (status, headers, body) = get_data(port, &format!("/p/{id}"));
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(content_type(&headers), "application/json");
    assert!(
        body.starts_with("{\"page\":\"/p/:id\","),
        "unexpected payload:\n{body}"
    );
    // The rendered title travels in the payload (the paste title, via head{}).
    assert!(
        body.contains("\"title\":\"deep title\""),
        "payload:\n{body}"
    );
    // props is the loaded Paste; params carries the matched route id.
    assert!(body.contains(&format!("\"props\":{{\"id\":\"{id}\"")));
    assert!(body.contains("\"body\":\"the body text\""));
    assert!(body.contains(&format!("\"params\":{{\"id\":\"{id}\"}}")));
}

#[test]
#[ignore = "generates the full bin app cold (minutes in a debug build) - run with the parity tier: cargo test --test universal_pages -- --ignored"]
fn marked_missing_paste_is_the_error_payload() {
    let server = bin_server();
    let port = server.port;
    let (status, headers, body) = get_data(port, "/p/ghost");
    // A miss on the DATA channel is a 200 carrying the @error payload (the client
    // renders the themed error page); the document channel still 404s.
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(content_type(&headers), "application/json");
    assert!(
        body.starts_with("{\"page\":\"@error\",\"status\":404,"),
        "unexpected payload:\n{body}"
    );
    assert!(body.contains("\"props\":{\"status\":404,"));
}

#[test]
#[ignore = "generates the full bin app cold (minutes in a debug build) - run with the parity tier: cargo test --test universal_pages -- --ignored"]
fn marked_non_client_route_falls_back_to_its_real_response() {
    let server = bin_server();
    let port = server.port;
    let id = create_paste(port, "raw", "raw body content", "text");
    // /raw/[id] is a `.vyrn` respond page, not in the client bundle. A marked request
    // must not be answered as JSON, so the client hard-navs to it.
    let (status, headers, body) = get_data(port, &format!("/raw/{id}"));
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(
        !content_type(&headers).contains("application/json"),
        "raw route must not be JSON: {}",
        content_type(&headers)
    );
    assert!(body.contains("raw body content"));
}
