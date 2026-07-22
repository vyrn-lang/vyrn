//! Integration tests for typed RPC (RFC-0019) — the `std/rpc` generators driven
//! through the real `vyrn` binary against the `examples/fullstack` contract.
//!
//! Three surfaces:
//!   * `serve` the fullstack server root and assert the wire behavior of the
//!     synthesized `rpcHandle` — 200 / 422-with-exact-issue-bytes / 405 / 404 /
//!     the `$schema` registry;
//!   * `emit-gen` the client and assert the synthesized stubs + dispatchers;
//!   * `test` the in-process flavor (`examples/rpc.vyrn`) runs green.
//!
//! `serve`/`emit-gen`/`test` are interpreter-only (no clang), so these run in
//! the default `cargo test`. Generation runs with the cache disabled so a stale
//! entry from another run can never mask a regression.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn repo_file(rel: &str) -> PathBuf {
    // vyrn-cli/ -> compiler/ -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel).canonicalize().unwrap()
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c
}

// ---- serve harness (mirrors tests/serve.rs) --------------------------------

struct Serve {
    child: Child,
    port: u16,
    stderr: Arc<Mutex<String>>,
}
impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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

/// Spawn `vyrn serve examples/fullstack/server.vyrn` on a free port.
fn start_server() -> Serve {
    let server = repo_file("examples/fullstack/server.vyrn");
    let port = free_port();
    let mut child = vyrn()
        .arg("serve")
        .arg(&server)
        .arg("--port")
        .arg(port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vyrn serve");
    let _ = drain(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());
    let s = Serve { child, port, stderr };
    wait_for(&s.stderr, "serving", Duration::from_secs(20));
    s
}

/// Send a raw request, read the whole `Connection: close` response, split into
/// (status_line, body).
fn request(port: u16, raw: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.write_all(raw.as_bytes()).expect("write");
    stream.flush().ok();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read");
    let (head, body) = resp.split_once("\r\n\r\n").unwrap_or((resp.as_str(), ""));
    (head.lines().next().unwrap_or("").to_string(), body.to_string())
}

fn post(port: u16, path: &str, body: &str) -> (String, String) {
    request(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}
fn get(port: u16, path: &str) -> (String, String) {
    request(port, &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"))
}

#[test]
fn rpc_ok_encodes_the_typed_result() {
    let s = start_server();
    let (status, body) = post(s.port, "/rpc/getUser", "{\"id\":7}");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(body, "{\"id\":7,\"name\":\"user7\",\"age\":30}");
}

#[test]
fn rpc_invalid_payload_is_422_with_exact_issue_bytes() {
    let s = start_server();
    // age 200 fails `Age = Int64 where value <= 130` during decode.
    let (status, body) = post(s.port, "/rpc/createUser", "{\"name\":\"Bob\",\"age\":200}");
    assert_eq!(status, "HTTP/1.1 422 Unprocessable Entity");
    assert_eq!(
        body,
        "{\"issues\":[{\"key\":\"validate\",\"path\":\"age\",\"message\":\"validation failed for `Age`\"}]}"
    );
}

#[test]
fn rpc_result_ok_is_200_with_tagged_ok() {
    // A `Result`-returning procedure (RFC-0024): an application-level success is
    // a 200 carrying the externally-tagged `{"Ok":true}` — never a 422.
    let s = start_server();
    let (status, body) = post(s.port, "/rpc/deleteUser", "{\"id\":5}");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(body, "{\"Ok\":true}");
}

#[test]
fn rpc_result_err_is_200_with_tagged_err() {
    // An application-level refusal is ALSO a 200, carrying `{"Err":".."}` — the
    // status distinguishes transport/validation from application outcome.
    let s = start_server();
    let (status, body) = post(s.port, "/rpc/deleteUser", "{\"id\":0}");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(body, "{\"Err\":\"cannot delete the root user\"}");
}

#[test]
fn rpc_result_invalid_request_is_still_422() {
    // A malformed REQUEST (id is not an integer) is a decode failure: 422 with
    // the server's issues — 422 stays reserved for request validation.
    let s = start_server();
    let (status, body) = post(s.port, "/rpc/deleteUser", "{\"id\":\"nope\"}");
    assert_eq!(status, "HTTP/1.1 422 Unprocessable Entity");
    assert_eq!(
        body,
        "{\"issues\":[{\"key\":\"json.type\",\"path\":\"id\",\"message\":\"expected integer, found string\"}]}"
    );
}

#[test]
fn rpc_schema_registry_reflects_result_oneof() {
    let s = start_server();
    let (_status, body) = get(s.port, "/rpc/$schema");
    // deleteUser's response schema is the RFC-0024 Ok/Err oneOf.
    assert!(body.contains("\"name\":\"deleteUser\""), "deleteUser in registry:\n{body}");
    assert!(
        body.contains("\"properties\":{\"Ok\":{\"type\":\"boolean\"}}"),
        "Ok arm reflected:\n{body}"
    );
    assert!(
        body.contains("\"properties\":{\"Err\":{\"type\":\"string\"}}"),
        "Err arm reflected:\n{body}"
    );
}

#[test]
fn rpc_wrong_method_is_405() {
    let s = start_server();
    let (status, _) = get(s.port, "/rpc/getUser");
    assert_eq!(status, "HTTP/1.1 405 Method Not Allowed");
}

#[test]
fn rpc_unknown_procedure_is_404() {
    let s = start_server();
    let (status, _) = post(s.port, "/rpc/nope", "{}");
    assert_eq!(status, "HTTP/1.1 404 Not Found");
}

#[test]
fn rpc_non_rpc_path_falls_through_to_pages() {
    let s = start_server();
    let (status, body) = get(s.port, "/");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(body.contains("vyrn fullstack"), "page fallback body:\n{body}");
}

#[test]
fn rpc_schema_registry_lists_every_procedure() {
    let s = start_server();
    let (status, body) = get(s.port, "/rpc/$schema");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(body.starts_with("{\"procedures\":["), "registry shape:\n{body}");
    assert!(body.contains("\"name\":\"getUser\""), "getUser in registry");
    assert!(body.contains("\"name\":\"createUser\""), "createUser in registry");
    // getUser's request carries the id property; createUser's request carries a
    // `$ref` to the validated Age (its `where` becomes a JSON Schema bound).
    assert!(body.contains("\"required\":[\"id\"]"), "getUser request schema");
    assert!(body.contains("\"maximum\":130"), "Age bound reflected into the schema");
}

// ---- emit-gen: the client's synthesized surface ----------------------------

#[test]
fn emit_gen_client_shows_stubs_and_dispatchers() {
    let client = repo_file("examples/fullstack/client.vyrn");
    let out = vyrn().arg("emit-gen").arg(&client).output().expect("emit-gen");
    assert!(out.status.success(), "emit-gen failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let src = String::from_utf8_lossy(&out.stdout);
    // The single shared transport, declared once.
    assert!(src.contains("extern fn vyrnRpcCall(name: String, body: String) -> Int64"), "{src}");
    // The RFC-0068 structured-reply surface: the wire issue shape and the reply
    // enum (a `Rejected` arm, since the prelude `Validation` already owns `Invalid`).
    assert!(
        src.contains("export type RpcIssue = { key: String, path: String, message: String }"),
        "RpcIssue type:\n{src}"
    );
    assert!(src.contains("export type RpcReply<T> ="), "RpcReply type:\n{src}");
    assert!(src.contains("| Rejected(Array<RpcIssue>)"), "RpcReply Rejected arm:\n{src}");
    assert!(src.contains("export fn rpcIssuesFrom(from: Array<Issue>) -> Array<RpcIssue>"), "issue mapper:\n{src}");
    // Each stub takes (req, cb: fn(RpcReply<Ret>)) and records the callback under
    // the call id (RFC-0040 §2); the completion dispatcher routes the reply to it.
    assert!(
        src.contains("export fn getUser(req: GetUserReq, cb: fn(RpcReply<User>))"),
        "getUser stub:\n{src}"
    );
    assert!(
        src.contains("let mut rpcPendingGetUser: Map<String, fn(RpcReply<User>)> = [:]"),
        "getUser pending map:\n{src}"
    );
    assert!(
        src.contains("export extern fn vyrnRpcDoneGetUser(id: Int64, status: Int64, body: String)"),
        "getUser dispatcher:\n{src}"
    );
    assert!(
        src.contains("Some(cb) => rpcDeliverGetUser(key, cb, rpcUnifyGetUser(status, body))"),
        "getUser dispatch routes to the pending callback:\n{src}"
    );
    // 2xx decodes to `Done`; 422 parses issues to `Rejected`; a decode/transport
    // fault is `Failed` — the locked transport wording rides the `Failed` arm.
    assert!(src.contains("Valid(v) => Done(v),"), "2xx -> Done:\n{src}");
    assert!(src.contains("Valid(bag) => Rejected(bag.issues),"), "422 -> Rejected:\n{src}");
    assert!(
        src.contains("Failed(\"procedure `getUser` is unreachable\")"),
        "unreachable wording on the Failed arm:\n{src}"
    );
    // No `on<Proc>` convention survives (clean break).
    assert!(!src.contains("onGetUser"), "no on<Proc> emission:\n{src}");
    assert!(src.contains("export extern fn vyrnRpcDoneCreateUser("), "createUser dispatcher");
    // The contract types are re-emitted verbatim (not imported).
    assert!(src.contains("export type User = { id: Int64, name: Username, age: Age }"), "{src}");
}

// ---- the in-process flavor under `vyrn test` -------------------------------

#[test]
fn in_process_flavor_runs_green_under_vyrn_test() {
    let example = repo_file("examples/rpc.vyrn");
    let out = vyrn().arg("test").arg(&example).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "in-process tests failed:\n{combined}");
    assert!(combined.contains("3 passed, 0 failed"), "expected 3 green tests:\n{combined}");
}
