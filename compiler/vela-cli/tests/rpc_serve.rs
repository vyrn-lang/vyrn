//! Integration tests for the typed-procedure HTTP mounts (RFC-0019): spawn the
//! real `velac serve` on a file with `rpc fn`s and drive `/rpc/*` with raw
//! `std::net::TcpStream` requests.
//!
//! Asserted: a 200 typed round trip, a 422 with the EXACT `{"issues":[...]}`
//! bytes, 404 on an unknown procedure, 415 on a non-JSON body, 204 on a
//! `Unit`-returning procedure, and the `GET /rpc/$schema` registry shape.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A served source with two procedures: `getUser` (validated request) and
/// `ping` (parameterless, `Unit` return). No `handle` — this is an RPC-only
/// server, which `velac serve` now supports.
const SERVER_SRC: &str = r#"
type UserId = Int64 where value >= 1

type GetUserReq = { id: UserId }
type User = { name: String, active: Bool }

export rpc fn getUser(req: GetUserReq) -> User {
    return User { name: "user#\{req.id}", active: true }
}

export rpc fn ping() -> Unit {
    return
}
"#;

struct Serve {
    child: Child,
    port: u16,
    #[allow(dead_code)]
    stderr: Arc<Mutex<String>>,
    _file: TempFile,
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct TempFile {
    path: std::path::PathBuf,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

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

fn start_server() -> Serve {
    let unique = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    );
    let path = std::env::temp_dir().join(format!("vela-rpc-{unique}.vela"));
    std::fs::write(&path, SERVER_SRC).expect("write temp server");
    let file = TempFile { path: path.clone() };

    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_velac"))
        .arg("serve")
        .arg(&path)
        .arg("--port")
        .arg(port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn velac serve");

    let _ = drain(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());
    let server = Serve { child, port, stderr, _file: file };
    wait_for(&server.stderr, "serving", Duration::from_secs(10));
    server
}

/// Send a raw request, read the whole `Connection: close` response, split into
/// (status line, body).
fn request(port: u16, raw: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.write_all(raw.as_bytes()).expect("write request");
    stream.flush().ok();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read response");
    let (head, body) = resp.split_once("\r\n\r\n").unwrap_or((resp.as_str(), ""));
    (head.lines().next().unwrap_or("").to_string(), body.to_string())
}

/// A `POST /rpc/<name>` with the given content type and body.
fn post(port: u16, name: &str, content_type: &str, body: &str) -> (String, String) {
    request(
        port,
        &format!(
            "POST /rpc/{name} HTTP/1.1\r\nHost: localhost\r\nContent-Type: {content_type}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

#[test]
fn typed_round_trip_returns_200_json() {
    let s = start_server();
    let (status, body) = post(s.port, "getUser", "application/json", "{\"id\":7}");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(body, "{\"name\":\"user#7\",\"active\":true}");
}

#[test]
fn content_type_negotiation_via_charset_param() {
    let s = start_server();
    // A `; charset=utf-8` parameter is stripped — still `application/json`.
    let (status, _) = post(s.port, "getUser", "application/json; charset=utf-8", "{\"id\":7}");
    assert_eq!(status, "HTTP/1.1 200 OK");
}

#[test]
fn invalid_request_returns_422_with_exact_issues() {
    let s = start_server();
    // `id` is a string, not an integer → one decode Issue.
    let (status, body) = post(s.port, "getUser", "application/json", "{\"id\":\"x\"}");
    assert_eq!(status, "HTTP/1.1 422 Unprocessable Entity");
    assert_eq!(
        body,
        "{\"issues\":[{\"key\":\"json.type\",\"path\":\"id\",\"message\":\"expected integer, found string\"}]}"
    );
}

#[test]
fn where_clause_violation_returns_422() {
    let s = start_server();
    // `id` is 0, violating `UserId = Int64 where value >= 1`.
    let (status, body) = post(s.port, "getUser", "application/json", "{\"id\":0}");
    assert_eq!(status, "HTTP/1.1 422 Unprocessable Entity");
    assert!(body.contains("\"issues\":["), "422 body is an issues array: {body}");
    assert!(body.contains("validate"), "carries a validation issue: {body}");
}

#[test]
fn unknown_procedure_returns_404() {
    let s = start_server();
    let (status, _) = post(s.port, "nope", "application/json", "{}");
    assert_eq!(status, "HTTP/1.1 404 Not Found");
}

#[test]
fn non_json_content_type_returns_415() {
    let s = start_server();
    let (status, _) = post(s.port, "getUser", "text/plain", "{\"id\":7}");
    assert_eq!(status, "HTTP/1.1 415 Unsupported Media Type");
}

#[test]
fn unit_returning_procedure_returns_204() {
    let s = start_server();
    let (status, body) = post(s.port, "ping", "application/json", "{}");
    assert_eq!(status, "HTTP/1.1 204 No Content");
    assert_eq!(body, "", "204 has no body");
}

#[test]
fn get_on_procedure_returns_405() {
    let s = start_server();
    let (status, _) = request(
        s.port,
        "GET /rpc/getUser HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 405 Method Not Allowed");
}

#[test]
fn schema_endpoint_lists_procedures() {
    let s = start_server();
    let (status, body) = request(
        s.port,
        "GET /rpc/$schema HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    // The registry names both procedures; `getUser`'s request schema is emitted
    // and `ping`'s request/response are null (parameterless, Unit return).
    assert!(body.starts_with("{\"procedures\":["), "registry shape: {body}");
    assert!(body.contains("\"name\":\"getUser\""), "lists getUser: {body}");
    assert!(body.contains("\"name\":\"ping\",\"request\":null,\"response\":null"), "ping nulls: {body}");
    assert!(body.contains("\"type\":\"object\""), "getUser request is a JSON Schema object: {body}");
}
