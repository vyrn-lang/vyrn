//! End-to-end test for the vela-lsp server over the real JSON-RPC wire format.
//!
//! Spawns the `vela-lsp` binary as a subprocess, speaks Content-Length-framed
//! JSON-RPC 2.0 over its stdin/stdout, and asserts the three interactive
//! capabilities work on `examples/enum.vela`:
//!   * `textDocument/hover` over `Circle` at the call site → variant detail.
//!   * `textDocument/definition` over `area` at the call site → a `Location` on
//!     the `fn area` declaration line.
//!   * `textDocument/completion` → top-level items including `Shape`, `Circle`,
//!     `area`, `main`.
//!
//! This guards the whole adapter end to end: capability advertisement, the
//! `analyze` cache populated on didOpen, 1-based↔0-based position conversion,
//! and the typed result shapes.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// One framed JSON-RPC message read from the server's stdout.
struct Message {
    json: serde_json::Value,
}

/// A tiny blocking LSP client over a child process's stdin/stdout.
struct LspClient {
    child: std::process::Child,
}

impl LspClient {
    fn spawn() -> std::io::Result<Self> {
        // `CARGO_BIN_EXE_vela-lsp` points at the built server binary (the
        // `[[bin]] name = "vela-lsp"` in Cargo.toml).
        let bin = env!("CARGO_BIN_EXE_vela-lsp");
        let child = Command::new(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(LspClient { child })
    }

    /// Serialize `v` and write one Content-Length-framed message to stdin.
    fn send(&mut self, v: &serde_json::Value) {
        let body = serde_json::to_vec(v).unwrap();
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let stdin = self.child.stdin.as_mut().expect("stdin open");
        stdin.write_all(header.as_bytes()).unwrap();
        stdin.write_all(&body).unwrap();
        stdin.flush().unwrap();
    }

    /// Read one framed message (headers → Content-Length → body) from stdout.
    /// Returns `None` on EOF.
    fn read(&mut self) -> Option<Message> {
        let stdout = self.child.stdout.as_mut().expect("stdout open");
        // Read headers byte-by-byte until the blank `\r\n` separator. Headers
        // are tiny, so this is fine.
        let mut headers = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            match stdout.read(&mut byte) {
                Ok(0) => return None,
                Ok(_) => {
                    headers.push(byte[0]);
                    // End of headers: `\r\n\r\n`.
                    if headers.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => return None,
            }
        }
        let header_str = String::from_utf8_lossy(&headers);
        let mut content_length: Option<usize> = None;
        for line in header_str.split("\r\n") {
            if let Some(rest) = line.strip_prefix("Content-Length:") {
                content_length = Some(rest.trim().parse().unwrap());
            }
        }
        let len = content_length.expect("Content-Length header present");
        let mut body = vec![0u8; len];
        stdout.read_exact(&mut body).ok()?;
        let json = serde_json::from_slice(&body).unwrap();
        Some(Message { json })
    }

    /// Read messages until one with the given JSON-RPC `id` arrives (a response
    /// to our request). Server-initiated notifications (publishDiagnostics) are
    /// skipped — the server may push them before/after the response.
    fn read_response(&mut self, id: &serde_json::Value) -> serde_json::Value {
        loop {
            let msg = self.read().expect("server closed before responding");
            if msg.json.get("id") == Some(id) {
                return msg.json;
            }
            // else: a notification or someone else's response; keep reading.
        }
    }

    /// Read messages until a notification with `method` arrives, returning its
    /// JSON. Responses to other requests are skipped.
    fn read_notification(&mut self, method: &str) -> serde_json::Value {
        loop {
            let msg = self.read().expect("server closed before notifying");
            if msg.json.get("method").and_then(|m| m.as_str()) == Some(method) {
                return msg.json;
            }
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Best effort: kill the child if it's still running.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The real `examples/enum.vela`, so the test tracks the actual file.
fn enum_vela() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/enum.vela");
    std::fs::read_to_string(path).expect("examples/enum.vela should exist")
}

/// A `file://` URI for the example. The LSP only echoes it back in locations;
/// the exact form doesn't matter as long as it round-trips.
fn enum_uri() -> &'static str {
    "file:///N:/lang/examples/enum.vela"
}

/// Spawn the server, complete the `initialize` handshake, and open
/// `enum.vela`. Asserts the three interactive capabilities are advertised.
/// Returns the live client ready for requests.
fn open_enum() -> LspClient {
    let mut client = LspClient::spawn().expect("spawn vela-lsp");

    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": init_id,
        "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let init_resp = client.read_response(&init_id);
    let caps =
        init_resp.get("result").and_then(|r| r.get("capabilities")).expect("capabilities present");
    assert!(caps.get("hoverProvider").is_some(), "hover advertised");
    assert!(caps.get("definitionProvider").is_some(), "definition advertised");
    assert!(caps.get("completionProvider").is_some(), "completion advertised");

    // initialized notification (the client sends this after initialize).
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    }));

    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": enum_uri(),
                "languageId": "vela",
                "version": 1,
                "text": enum_vela()
            }
        }
    }));
    client
}

/// A one-up request id source for a test (deterministic — `Math.random` etc.
/// are unavailable here, so the test supplies its own counter).
struct Ids(u64);
impl Ids {
    fn new() -> Self { Ids(1) }
    fn next(&mut self) -> serde_json::Value {
        self.0 += 1;
        serde_json::json!(self.0)
    }
}

#[test]
fn hover_definition_completion_on_enum_vela() {
    let mut client = open_enum();
    let mut ids = Ids::new();

    // --- hover over `Circle` at the call site -----------------------------
    // Line 19 1-based, "Circle" cols 18-23 1-based → LSP line 18, char 17.
    let hover_id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": hover_id,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": enum_uri() },
            "position": { "line": 18, "character": 17 }
        }
    }));
    let hover_resp = client.read_response(&hover_id);
    let hover = hover_resp.get("result").expect("hover result");
    let value = hover
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .expect("hover contents.value");
    assert!(
        value.contains("variant of Shape") && value.contains("Circle"),
        "hover detail: {value}"
    );
    assert!(value.contains("Circle(Int64)"), "hover carries the payload: {value}");

    // --- go-to-definition over `area` at the call site --------------------
    // Line 19 1-based, "area" cols 13-16 1-based → LSP line 18, char 12.
    let def_id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": def_id,
        "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": enum_uri() },
            "position": { "line": 18, "character": 12 }
        }
    }));
    let def_resp = client.read_response(&def_id);
    let loc = def_resp.get("result").expect("definition result");
    // `fn area` is declared on line 10 1-based → LSP line 9; name cols 4-7 →
    // chars 3-6.
    let start_line = loc
        .pointer("/range/start/line")
        .and_then(|v| v.as_i64())
        .expect("range.start.line");
    let start_char = loc
        .pointer("/range/start/character")
        .and_then(|v| v.as_i64())
        .expect("range.start.character");
    assert_eq!(start_line, 9, "definition lands on the fn area declaration line");
    assert_eq!(start_char, 3, "definition lands on the name column, not the line start");
    assert_eq!(loc.get("uri").and_then(|u| u.as_str()), Some(enum_uri()));

    // --- completion -------------------------------------------------------
    let comp_id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": comp_id,
        "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": enum_uri() },
            "position": { "line": 18, "character": 1 }
        }
    }));
    let comp_resp = client.read_response(&comp_id);
    let items = comp_resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("completion result is a list");
    let labels: Vec<&str> =
        items.iter().filter_map(|i| i.get("label").and_then(|l| l.as_str())).collect();
    for expected in ["Shape", "Circle", "Rect", "Unit", "area", "main"] {
        assert!(labels.contains(&expected), "completion missing {expected}: {labels:?}");
    }
    // The injected built-in `Value` family must not leak into completions.
    for injected in ["Value", "VInt", "VBool", "VStr"] {
        assert!(!labels.contains(&injected), "injected {injected} leaked: {labels:?}");
    }

    // --- shutdown ---------------------------------------------------------
    let shutdown_id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": shutdown_id,
        "method": "shutdown"
    }));
    let _ = client.read_response(&shutdown_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "exit" }));
    // Let the process exit; ignore its status (it may already be gone). Drop
    // will call kill+wait again, which is harmless on an exited child.
    let _ = client.child.wait();
}

/// Hovering a position that does NOT resolve (whitespace, a keyword, a builtin)
/// must reply with `"result": null` — NOT a message missing both `result` and
/// `error`. The latter is what `Response { result: None, error: None }`
/// serializes to (both fields are `skip_serializing_if = Option::is_none`), and
/// VS Code rejects it as "neither a result nor an error property". This guards
/// the `Response::new_ok(id, Option<T>)` fix against regression.
#[test]
fn hover_off_identifier_returns_null_result() {
    let mut client = open_enum();
    let mut ids = Ids::new();

    // Hover at the start of `fn area` (line 10 1-based → LSP line 9, char 0):
    // the `f` of `fn` is a keyword token, no `TokenInfo` covers it, so `resolve`
    // returns `None` → the server must emit `result: null`.
    let hover_id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": hover_id,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": enum_uri() },
            "position": { "line": 9, "character": 0 }
        }
    }));
    let resp = client.read_response(&hover_id);

    // The response must carry an explicit `result` key, and it must be null —
    // not absent, and not an error.
    assert!(resp.get("error").is_none(), "no error for an off-identifier hover: {resp}");
    assert!(resp.get("result").is_some(), "`result` key must be present (not skipped): {resp}");
    assert!(
        resp.get("result").unwrap().is_null(),
        "off-identifier hover result must be null, not {:?}",
        resp.get("result")
    );

    // Go-to-definition at the same off-identifier position is the same shape.
    let def_id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": def_id,
        "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": enum_uri() },
            "position": { "line": 9, "character": 0 }
        }
    }));
    let dresp = client.read_response(&def_id);
    assert!(dresp.get("error").is_none(), "no error for off-identifier definition: {dresp}");
    assert!(dresp.get("result").is_some(), "`result` key must be present: {dresp}");
    assert!(dresp.get("result").unwrap().is_null(), "off-identifier def result must be null");
}

/// A non-exhaustive `match` diagnostic must squiggle the `match` **keyword**
/// itself — not the whole line (which would include leading spaces and
/// `return`/`{`), and not a zero-length range at column 0 (which VS Code renders
/// as a squiggle on just the first token, e.g. `return`). `analyze` pins these
/// diagnostics to the `Tok::Match` column; this guards that the pinning reaches
/// the wire.
#[test]
fn non_exhaustive_match_squiggles_the_match_keyword() {
    let mut client = LspClient::spawn().expect("spawn vela-lsp");

    // initialize + initialized.
    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": init_id,
        "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let _ = client.read_response(&init_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    // A non-exhaustive `match` (missing variant `B`). The match keyword is on
    // line 3 (1-based) → LSP line 2; `match` is at 1-based cols 13-17 → LSP
    // chars 12-17 (start 12, end 17).
    let uri = "file:///non/exhaustive.vela";
    let src = "\
type T = | A(Int64) | B;
fn f(x: T) -> Int64 {
    let r = match x {
        A(n) => n,
    };
    return r;
}
fn main() -> Int64 { return 0; }
";
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": { "uri": uri, "languageId": "vela", "version": 1, "text": src }
        }
    }));

    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif
        .pointer("/params/diagnostics")
        .and_then(|d| d.as_array())
        .expect("a publishDiagnostics with the match error");
    assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
    let d = &diags[0];
    assert!(
        d.get("message").unwrap().as_str().unwrap().contains("missing variant `B`"),
        "match error: {d}"
    );

    let start = d.pointer("/range/start").unwrap();
    let end = d.pointer("/range/end").unwrap();
    let start_line = start.get("line").unwrap().as_i64().unwrap();
    let start_char = start.get("character").unwrap().as_i64().unwrap();
    let end_line = end.get("line").unwrap().as_i64().unwrap();
    let end_char = end.get("character").unwrap().as_i64().unwrap();
    // The `match` keyword: LSP line 2, chars 12-17 (start 12, end 17).
    assert_eq!(start_line, 2, "diagnostic on the match line: {d}");
    assert_eq!(end_line, 2, "single-line range: {d}");
    assert_eq!(start_char, 12, "squiggle starts at the `match` keyword (char 12): {d}");
    assert_eq!(end_char, 17, "squiggle ends just past `match` (char 17): {d}");
    assert_eq!(end_char - start_char, 5, "squiggle covers exactly `match` (5 chars): {d}");
}

/// `textDocument/completion` at a `.foo` member-access position returns the
/// built-in methods for the receiver's type (here `Array<Int>` → push/at/alen/
/// afree/length), NOT the top-level symbol list. Guards the `is_member_context`
/// routing → `member_completions` → receiver-type resolution path over the wire.
#[test]
fn member_completion_after_dot_lists_array_methods() {
    let mut client = LspClient::spawn().expect("spawn vela-lsp");
    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": init_id, "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let _ = client.read_response(&init_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    let uri = "file:///member/comp.vela";
    let src = "\
fn main() -> Int64 {
    let mut a: Array<Int64> = [];
    a.push(1);
    return a.length;
}
";
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": uri, "languageId": "vela", "version": 1, "text": src } }
    }));
    // Drain the publishDiagnostics from didOpen (the source is clean, so it's
    // an empty list — but it still arrives before our completion response).
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let mut ids = Ids::new();
    let comp_id = ids.next();
    // Line 3 1-based → LSP line 2. `    a.push(1);` — 4 spaces, `a` (col 5),
    // `.` (col 6), then cursor right after the dot at 1-based col 7 → LSP char 6.
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": comp_id, "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 6 }
        }
    }));
    let comp_resp = client.read_response(&comp_id);
    let items = comp_resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("member-completion result is a list");
    let labels: Vec<&str> =
        items.iter().filter_map(|i| i.get("label").and_then(|l| l.as_str())).collect();
    for expected in ["push", "at", "alen", "afree", "length"] {
        assert!(labels.contains(&expected), "array member {expected} missing: {labels:?}");
    }
    // In a `.foo` context the top-level symbols must NOT be offered.
    assert!(!labels.contains(&"main"), "top-level `main` leaked into member completion: {labels:?}");
}