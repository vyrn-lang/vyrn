//! End-to-end test for the vyrn-lsp server over the real JSON-RPC wire format.
//!
//! Spawns the `vyrn-lsp` binary as a subprocess, speaks Content-Length-framed
//! JSON-RPC 2.0 over its stdin/stdout, and asserts the three interactive
//! capabilities work on `examples/enum.vyrn`:
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

/// How long `read()` waits for the next server message before failing the
/// test WITH EVIDENCE. A pipe read has no portable timeout, so frames are
/// parsed on a reader thread and handed over a channel; without this, a
/// message that never arrives hangs the test forever (observed on the first
/// Linux CI run of the `.vyx` suite — cargo's "over 60 seconds" notes with
/// zero diagnostics). Generous: CI runs debug builds on few cores.
const READ_TIMEOUT_SECS: u64 = 120;

/// One framed JSON-RPC message read from the server's stdout.
struct Message {
    json: serde_json::Value,
}

/// Read one framed message (headers → Content-Length → body). `None` on EOF.
/// Runs on the reader thread, where blocking is fine.
fn read_frame(stdout: &mut impl Read) -> Option<serde_json::Value> {
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
    Some(serde_json::from_slice(&body).unwrap())
}

/// One line per message for the timeout dump: id/method plus the URI (for
/// publishDiagnostics) so a missing-publish hang names what DID arrive.
fn summarize(json: &serde_json::Value) -> String {
    let method = json.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = json.get("id").map(|i| i.to_string()).unwrap_or_default();
    let uri = json
        .pointer("/params/uri")
        .and_then(|u| u.as_str())
        .unwrap_or("");
    format!("  id={id} method={method} uri={uri}")
}

/// A tiny blocking LSP client over a child process's stdin/stdout.
struct LspClient {
    child: std::process::Child,
    /// Frames parsed by the reader thread; `None` is pushed once on EOF.
    rx: std::sync::mpsc::Receiver<serde_json::Value>,
    /// One-line summaries of everything received, for the timeout dump.
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl LspClient {
    fn spawn() -> std::io::Result<Self> {
        // `CARGO_BIN_EXE_vyrn-lsp` points at the built server binary (the
        // `[[bin]] name = "vyrn-lsp"` in Cargo.toml).
        let bin = env!("CARGO_BIN_EXE_vyrn-lsp");
        let mut child = Command::new(bin)
            // Disable the shared generator cache so RFC-0033 fixtures never hit a
            // stale synthesized module from another run.
            .env("VYRN_NO_GEN_CACHE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdout = child.stdout.take().expect("stdout piped");
        let (tx, rx) = std::sync::mpsc::channel();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_w = seen.clone();
        std::thread::spawn(move || {
            while let Some(json) = read_frame(&mut stdout) {
                seen_w.lock().unwrap().push(summarize(&json));
                if tx.send(json).is_err() {
                    break; // client dropped
                }
            }
        });
        Ok(LspClient { child, rx, seen })
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

    /// Next framed message from the reader thread. Returns `None` on EOF.
    /// Panics after `READ_TIMEOUT_SECS` with a dump of every message received
    /// so far — a missing message must fail with evidence, never hang.
    fn read(&mut self) -> Option<Message> {
        match self
            .rx
            .recv_timeout(std::time::Duration::from_secs(READ_TIMEOUT_SECS))
        {
            Ok(json) => Some(Message { json }),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let seen = self.seen.lock().unwrap();
                panic!(
                    "no server message within {READ_TIMEOUT_SECS}s; {} received so far:\n{}",
                    seen.len(),
                    seen.join("\n")
                );
            }
        }
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

/// The real `examples/enum.vyrn`, so the test tracks the actual file.
fn enum_vyrn() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/enum.vyrn");
    std::fs::read_to_string(path).expect("examples/enum.vyrn should exist")
}

/// A `file://` URI for the example. The LSP only echoes it back in locations;
/// the exact form doesn't matter as long as it round-trips.
fn enum_uri() -> &'static str {
    "file:///N:/lang/examples/enum.vyrn"
}

/// Spawn the server, complete the `initialize` handshake, and open
/// `enum.vyrn`. Asserts the three interactive capabilities are advertised.
/// Returns the live client ready for requests.
fn open_enum() -> LspClient {
    let mut client = LspClient::spawn().expect("spawn vyrn-lsp");

    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": init_id,
        "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let init_resp = client.read_response(&init_id);
    let caps = init_resp
        .get("result")
        .and_then(|r| r.get("capabilities"))
        .expect("capabilities present");
    assert!(caps.get("hoverProvider").is_some(), "hover advertised");
    assert!(
        caps.get("definitionProvider").is_some(),
        "definition advertised"
    );
    assert!(
        caps.get("completionProvider").is_some(),
        "completion advertised"
    );
    assert!(
        caps.get("documentSymbolProvider").is_some(),
        "document symbols advertised"
    );

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
                "languageId": "vyrn",
                "version": 1,
                "text": enum_vyrn()
            }
        }
    }));
    client
}

/// A one-up request id source for a test (deterministic — `Math.random` etc.
/// are unavailable here, so the test supplies its own counter).
struct Ids(u64);
impl Ids {
    fn new() -> Self {
        Ids(1)
    }
    fn next(&mut self) -> serde_json::Value {
        self.0 += 1;
        serde_json::json!(self.0)
    }
}

#[test]
fn hover_definition_completion_on_enum_vyrn() {
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
    assert!(
        value.contains("Circle(Int64)"),
        "hover carries the payload: {value}"
    );

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
    assert_eq!(
        start_line, 9,
        "definition lands on the fn area declaration line"
    );
    assert_eq!(
        start_char, 3,
        "definition lands on the name column, not the line start"
    );
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
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("label").and_then(|l| l.as_str()))
        .collect();
    for expected in ["Shape", "Circle", "Rect", "Unit", "area", "main"] {
        assert!(
            labels.contains(&expected),
            "completion missing {expected}: {labels:?}"
        );
    }
    // The injected built-in `Value` family must not leak into completions.
    for injected in ["Value", "IntVal", "StrVal", "BoolVal"] {
        assert!(
            !labels.contains(&injected),
            "injected {injected} leaked: {labels:?}"
        );
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

/// `textDocument/documentSymbol` returns this document's own top-level
/// declarations as a flat list: the `Shape` type, its `Circle`/`Rect`/`Unit`
/// variants, and the `area`/`main` functions — each with the correct 0-based
/// declaration line and LSP `SymbolKind`. Guards capability advertisement, the
/// `file.is_none()` filter (no imported symbols here to skip), and the kind
/// mapping (Type→Struct, Variant→EnumMember, Function→Function).
#[test]
fn document_symbol_lists_top_level_declarations() {
    let mut client = open_enum();
    let mut ids = Ids::new();

    // `open_enum` advertised hover/def/completion; documentSymbol must be too.
    // (Re-initialize handshake already happened; assert via the earlier caps is
    // covered by open_enum. Here we exercise the request itself.)
    let sym_id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": sym_id,
        "method": "textDocument/documentSymbol",
        "params": { "textDocument": { "uri": enum_uri() } }
    }));
    let resp = client.read_response(&sym_id);
    let items = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("documentSymbol result is a list");

    // Build name → (0-based line, kind) from the flat DocumentSymbol list.
    // LSP SymbolKind numbers: Method=6, Function=12, EnumMember=22, Struct=23.
    let mut by_name: std::collections::HashMap<&str, (i64, i64)> = std::collections::HashMap::new();
    for it in items {
        let name = it
            .get("name")
            .and_then(|n| n.as_str())
            .expect("symbol name");
        let line = it
            .pointer("/range/start/line")
            .and_then(|l| l.as_i64())
            .expect("range line");
        let kind = it
            .get("kind")
            .and_then(|k| k.as_i64())
            .expect("symbol kind");
        by_name.insert(name, (line, kind));
    }

    for expected in ["Shape", "Circle", "Rect", "Unit", "area", "main"] {
        assert!(
            by_name.contains_key(expected),
            "documentSymbol missing {expected}: {by_name:?}"
        );
    }
    // Declaration lines (0-based): `type Shape` on file line 4 → 3; variants on
    // 5/6/7 → 4/5/6; `fn area` on 10 → 9; `fn main` on 18 → 17.
    assert_eq!(by_name["Shape"].0, 3, "Shape declared on 0-based line 3");
    assert_eq!(by_name["Circle"].0, 4, "Circle variant on 0-based line 4");
    assert_eq!(by_name["area"].0, 9, "area declared on 0-based line 9");
    assert_eq!(by_name["main"].0, 17, "main declared on 0-based line 17");
    // Kind mapping.
    assert_eq!(by_name["Shape"].1, 23, "type → Struct(23)");
    assert_eq!(by_name["Circle"].1, 22, "variant → EnumMember(22)");
    assert_eq!(by_name["area"].1, 12, "function → Function(12)");
    assert_eq!(by_name["main"].1, 12, "function → Function(12)");

    // shutdown.
    let shutdown_id = ids.next();
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "id": shutdown_id, "method": "shutdown" }));
    let _ = client.read_response(&shutdown_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "exit" }));
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
    assert!(
        resp.get("error").is_none(),
        "no error for an off-identifier hover: {resp}"
    );
    assert!(
        resp.get("result").is_some(),
        "`result` key must be present (not skipped): {resp}"
    );
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
    assert!(
        dresp.get("error").is_none(),
        "no error for off-identifier definition: {dresp}"
    );
    assert!(
        dresp.get("result").is_some(),
        "`result` key must be present: {dresp}"
    );
    assert!(
        dresp.get("result").unwrap().is_null(),
        "off-identifier def result must be null"
    );
}

/// A non-exhaustive `match` diagnostic must squiggle the `match` **keyword**
/// itself — not the whole line (which would include leading spaces and
/// `return`/`{`), and not a zero-length range at column 0 (which VS Code renders
/// as a squiggle on just the first token, e.g. `return`). `analyze` pins these
/// diagnostics to the `Tok::Match` column; this guards that the pinning reaches
/// the wire.
#[test]
fn non_exhaustive_match_squiggles_the_match_keyword() {
    let mut client = LspClient::spawn().expect("spawn vyrn-lsp");

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
    let uri = "file:///non/exhaustive.vyrn";
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
            "textDocument": { "uri": uri, "languageId": "vyrn", "version": 1, "text": src }
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
        d.get("message")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("missing variant `B`"),
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
    assert_eq!(
        start_char, 12,
        "squiggle starts at the `match` keyword (char 12): {d}"
    );
    assert_eq!(
        end_char, 17,
        "squiggle ends just past `match` (char 17): {d}"
    );
    assert_eq!(
        end_char - start_char,
        5,
        "squiggle covers exactly `match` (5 chars): {d}"
    );
}

/// `textDocument/completion` at a `.foo` member-access position returns the
/// built-in methods for the receiver's type (here `Array<Int>` → push/at/pop/
/// length), NOT the top-level symbol list. Guards the `is_member_context`
/// routing → `member_completions` → receiver-type resolution path over the wire.
#[test]
fn member_completion_after_dot_lists_array_methods() {
    let mut client = LspClient::spawn().expect("spawn vyrn-lsp");
    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": init_id, "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let _ = client.read_response(&init_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    let uri = "file:///member/comp.vyrn";
    let src = "\
fn main() -> Int64 {
    let mut a: Array<Int64> = [];
    a.push(1);
    return a.length;
}
";
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": uri, "languageId": "vyrn", "version": 1, "text": src } }
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
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("label").and_then(|l| l.as_str()))
        .collect();
    for expected in ["push", "at", "pop", "length"] {
        assert!(
            labels.contains(&expected),
            "array member {expected} missing: {labels:?}"
        );
    }
    assert!(
        !labels.contains(&"afree"),
        "`afree` was removed: {labels:?}"
    );
    // In a `.foo` context the top-level symbols must NOT be offered.
    assert!(
        !labels.contains(&"main"),
        "top-level `main` leaked into member completion: {labels:?}"
    );
}

/// A `modify self` method is offered after a dot and hovers with the receiver's
/// capability written out. A reader deciding whether to call `t.record(..)`
/// wants to know it will mutate `t`, and the word is the only place that says
/// so.
#[test]
fn member_completion_and_hover_show_a_modify_self_receiver() {
    let mut client = LspClient::spawn().expect("spawn vyrn-lsp");
    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": init_id, "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let _ = client.read_response(&init_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    let uri = "file:///member/recv.vyrn";
    let src = "\
type Tally = { running: Int64 }
protocol Counting {
    fn record(modify self, n: Int64) -> Unit
}
impl Counting for Tally {
    fn record(modify self, n: Int64) -> Unit { self.running = self.running + n }
}
fn main() -> Int64 {
    let mut t = Tally { running: 0 }
    t.record(2)
    return t.running
}
";
    did_open(&mut client, uri, "vyrn", src);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    // Right after the dot in `t.record(2)`.
    let (line, ch) = pos_after(src, "    t.");
    let labels = completion_labels(&mut client, uri, line, ch);
    assert!(
        labels.contains(&"record".to_string()),
        "method missing: {labels:?}"
    );

    // Hover over the protocol's own declaration of it.
    let (hline, hch) = pos_after(src, "    fn r");
    let hover = hover_value(&mut client, uri, hline, hch).expect("hover on the declaration");
    assert!(
        hover.contains("fn record(modify self: Tally, n: Int64)"),
        "hover hides the receiver's capability: {hover}"
    );
}

/// RFC-0020 M1: `textDocument/completion` inside a string literal whose expected
/// type is a finite string type offers that type's whole language (`t("` → every
/// key), NOT the top-level symbol list.
#[test]
fn string_literal_completion_offers_finite_keys() {
    let mut client = LspClient::spawn().expect("spawn vyrn-lsp");
    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": init_id, "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let _ = client.read_response(&init_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    let uri = "file:///finite/keys.vyrn";
    let src = "\
type TransKey = String where value =~ \"nav\\\\.(home|about)\\\\.label\"
fn t(key: TransKey) -> Int64 { return 0 }
fn main() -> Int64 {
    return t(\"\")
}
";
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": uri, "languageId": "vyrn", "version": 1, "text": src } }
    }));
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let mut ids = Ids::new();
    let comp_id = ids.next();
    // Line 4 (1-based) `    return t("")` → LSP line 3. The opening `"` is at
    // 1-based col 14; the cursor sits between the quotes at 0-based char 14.
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": comp_id, "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": 3, "character": 14 }
        }
    }));
    let comp_resp = client.read_response(&comp_id);
    let items = comp_resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("string-literal completion result is a list");
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("label").and_then(|l| l.as_str()))
        .collect();
    assert!(
        labels.contains(&"nav.home.label"),
        "missing key: {labels:?}"
    );
    assert!(
        labels.contains(&"nav.about.label"),
        "missing key: {labels:?}"
    );
    // The top-level symbols must not leak into the string-literal context.
    assert!(
        !labels.contains(&"main"),
        "top-level `main` leaked: {labels:?}"
    );
}

/// Multi-file awareness (RFC-0010): a document importing from a sibling file
/// gets CLEAN diagnostics (the loader resolves the import), and an import of a
/// nonexistent module produces a diagnostic instead of silent breakage.
/// Before `analyze_linked`, every imported name squiggled as unknown.
#[test]
fn imports_resolve_across_files() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lib.vyrn"),
        "export fn double(x: Int64) -> Int64 {\n    return x * 2\n}\n",
    )
    .unwrap();
    let root_path = dir.join("main.vyrn");
    let root_text =
        "import { double } from \"./lib\"\n\nfn main() -> Int64 {\n    return double(21)\n}\n";
    std::fs::write(&root_path, root_text).unwrap();
    let uri = format!("file:///{}", root_path.to_string_lossy().replace('\\', "/"));

    let mut client = LspClient::spawn().expect("spawn vyrn-lsp");
    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": init_id, "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let _ = client.read_response(&init_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": {
            "uri": uri.clone(), "languageId": "vyrn", "version": 1, "text": root_text
        } }
    }));
    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif["params"]["diagnostics"].as_array().unwrap();
    assert!(
        diags.is_empty(),
        "import resolved via loader, expected no diagnostics: {diags:?}"
    );

    // Cross-file go-to-definition: `double` at the call site (0-based line 3,
    // `    return double(21)`, char 12 is inside the name) → a Location in
    // lib.vyrn on its declaration line.
    let def_id = serde_json::json!(2);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": def_id, "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": uri.clone() },
            "position": { "line": 3, "character": 12 }
        }
    }));
    let resp = client.read_response(&def_id);
    let loc = &resp["result"];
    let target = loc["uri"].as_str().expect("definition returns a Location");
    assert!(
        target.ends_with("lib.vyrn"),
        "definition jumps into the imported file: {target}"
    );
    assert_eq!(
        loc["range"]["start"]["line"], 0,
        "lands on `export fn double`"
    );

    // Cross-file hover: the same position shows the imported signature.
    let hover_id = serde_json::json!(3);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": hover_id, "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": uri.clone() },
            "position": { "line": 3, "character": 12 }
        }
    }));
    let resp = client.read_response(&hover_id);
    let hover = resp["result"]["contents"]["value"]
        .as_str()
        .expect("hover has content");
    assert!(
        hover.contains("double"),
        "hover shows the imported signature: {hover}"
    );

    // Edit the import to a nonexistent module → a load diagnostic appears.
    let bad_text = root_text.replace("./lib", "./gone");
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [ { "text": bad_text } ]
        }
    }));
    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif["params"]["diagnostics"].as_array().unwrap();
    assert!(
        !diags.is_empty(),
        "unresolvable import must produce a diagnostic"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Whole-document formatting (RFC-0017): the server advertises
/// `documentFormattingProvider`, and `textDocument/formatting` on a deliberately
/// mis-spaced (but lexable) buffer returns one full-range `TextEdit` whose
/// `newText` is the canonical form — semicolons dropped, spacing normalized,
/// 4-space indent, one trailing newline.
#[test]
fn document_formatting_returns_canonical_edit() {
    let mut client = LspClient::spawn().expect("spawn vyrn-lsp");
    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": init_id, "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let init_resp = client.read_response(&init_id);
    assert!(
        init_resp
            .pointer("/result/capabilities/documentFormattingProvider")
            .is_some(),
        "documentFormatting advertised: {init_resp}"
    );
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    // Messy input: no indent, cramped operators, trailing semicolons.
    let uri = "file:///fmt/messy.vyrn";
    let src = "fn main()->Int64{\nlet  x=1+2*3;\nreturn x;\n}\n";
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": uri, "languageId": "vyrn", "version": 1, "text": src } }
    }));
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let mut ids = Ids::new();
    let fmt_id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": fmt_id, "method": "textDocument/formatting",
        "params": {
            "textDocument": { "uri": uri },
            "options": { "tabSize": 4, "insertSpaces": true }
        }
    }));
    let resp = client.read_response(&fmt_id);
    let edits = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("formatting returns edits");
    assert_eq!(edits.len(), 1, "one whole-document edit: {resp}");
    let new_text = edits[0]
        .get("newText")
        .and_then(|t| t.as_str())
        .expect("newText");
    assert_eq!(
        new_text, "fn main() -> Int64 {\n    let x = 1 + 2 * 3\n    return x\n}\n",
        "formatting yields the canonical form"
    );

    // A document that fails to lex returns a null result (no edit) — never a
    // corrupting change while the user is mid-edit.
    let bad_uri = "file:///fmt/broken.vyrn";
    let bad_src = "fn main() -> Int64 { let s = \"unterminated }\n";
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": bad_uri, "languageId": "vyrn", "version": 1, "text": bad_src } }
    }));
    let _ = client.read_notification("textDocument/publishDiagnostics");
    let bad_id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": bad_id, "method": "textDocument/formatting",
        "params": { "textDocument": { "uri": bad_uri }, "options": { "tabSize": 4, "insertSpaces": true } }
    }));
    let bad_resp = client.read_response(&bad_id);
    assert!(
        bad_resp.get("error").is_none(),
        "no error for an unlexable buffer: {bad_resp}"
    );
    assert!(
        bad_resp.get("result").is_some(),
        "`result` key present (not skipped): {bad_resp}"
    );
    assert!(
        bad_resp["result"].is_null(),
        "unlexable buffer formats to null (no edit)"
    );
}

// ===========================================================================
// RFC-0033 — origin maps: diagnostics and editor requests inside `.vyx` inputs.
// A tiny fixture project (app.vyrn + comp/Widget.vyx) is written to a scratch
// dir; the server discovers `std/` by walking up from its own binary.
// ===========================================================================

use std::sync::atomic::{AtomicUsize, Ordering};
static RFC33_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A `file://` URI for `path` (drive-letter absolute → `file:///C:/…`).
fn file_uri(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// The one-line app that imports a view function generated over `./comp`.
const RFC33_APP: &str = "import { components } from \"std/vyx\"\n\
    import { widget } from components(\"./comp\")\n\
    fn main() -> Int64 { return 0 }\n";

/// A `.vyx` whose template mistypes `title` as `titel` — a type error that
/// RFC-0033 remaps to line 6, column 8 of the `.vyx` (RFC-0039 v2 grammar:
/// `<li>{{ ` is 7 chars, so `item` starts at col 8).
const RFC33_VYX: &str = "<script>\n\
    type Row = { title: String }\n\
    props { item: Row }\n\
    </script>\n\
    <template>\n\
    <li>{{ item.titel }}</li>\n\
    </template>\n";

/// A well-typed variant (`item.title`), for hover/completion where the module
/// must link cleanly.
const RFC33_VYX_OK: &str = "<script>\n\
    type Row = { title: String }\n\
    props { item: Row }\n\
    </script>\n\
    <template>\n\
    <li>{{ item.title }}</li>\n\
    </template>\n";

fn rfc33_scratch(tag: &str, vyx_body: &str) -> std::path::PathBuf {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rfc33_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("comp")).unwrap();
    std::fs::write(dir.join("comp/Widget.vyx"), vyx_body).unwrap();
    std::fs::write(dir.join("app.vyrn"), RFC33_APP).unwrap();
    dir
}

/// Spawn + initialize + initialized, ready for didOpen.
fn rfc33_client() -> LspClient {
    let mut client = LspClient::spawn().expect("spawn vyrn-lsp");
    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": init_id, "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let _ = client.read_response(&init_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
    client
}

fn did_open(client: &mut LspClient, uri: &str, lang: &str, text: &str) {
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": uri, "languageId": lang, "version": 1, "text": text } }
    }));
}

/// Read publishDiagnostics notifications until one whose URI contains `needle`.
fn read_diags_for(client: &mut LspClient, needle: &str) -> serde_json::Value {
    loop {
        let msg = client.read().expect("server closed before publishing");
        if msg.json.get("method").and_then(|m| m.as_str())
            == Some("textDocument/publishDiagnostics")
        {
            let uri = msg
                .json
                .pointer("/params/uri")
                .and_then(|u| u.as_str())
                .unwrap_or("");
            if uri.contains(needle) {
                return msg.json;
            }
        }
    }
}

/// A template type error is published INTO the `.vyx` buffer at the exact
/// source line/column, not against the synthesized module.
#[test]
fn rfc33_vyx_type_error_publishes_into_the_vyx_buffer() {
    let dir = rfc33_scratch("diag", RFC33_VYX);
    let mut client = rfc33_client();
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        RFC33_APP,
    );

    let note = read_diags_for(&mut client, "Widget.vyx");
    let diags = note
        .pointer("/params/diagnostics")
        .and_then(|d| d.as_array())
        .expect("diags array");
    assert!(
        !diags.is_empty(),
        "the .vyx buffer carries the remapped error: {note}"
    );
    let d0 = &diags[0];
    let msg = d0.get("message").and_then(|m| m.as_str()).unwrap_or("");
    assert!(msg.contains("titel"), "carries the checker message: {msg}");
    // `.vyx` line 6 (0-based 5), column 8 (0-based 7) — where `item` starts
    // inside `{{ item.titel }}` (RFC-0039 v2).
    assert_eq!(
        d0.pointer("/range/start/line").and_then(|l| l.as_i64()),
        Some(5),
        "line: {note}"
    );
    assert_eq!(
        d0.pointer("/range/start/character")
            .and_then(|c| c.as_i64()),
        Some(7),
        "col: {note}"
    );
}

/// A `.vyx` whose DYNAMIC attribute expression mistypes `title` as `titel`. The
/// emitter hoists a `:attr` value onto its own origin-governed `let … = A("href",
/// item.titel)` line (RFC-0039 §1); after RFC-0054 M4b that hoist is emitted
/// through `rawAt` (not a hand-written `//@origin`), so this pins that the
/// hoisted-region origins stay column-exact through the migration.
const RFC33_VYX_ATTR_BAD: &str = "<script>\n\
    type Row = { title: String }\n\
    props { item: Row }\n\
    </script>\n\
    <template>\n\
    <a :href=\"item.titel\">x</a>\n\
    </template>\n";

/// RFC-0054 M4b: a CHECK (type) error inside a HOISTED `:attr` expression is
/// published into the `.vyx` buffer at the expression's exact line/column — the
/// `rawAt`-emitted origin of the hoisted `let` binding round-trips through
/// `OriginMaps` exactly as the former hand-concatenated directive did. `<a :href="`
/// is 10 chars, so `item.titel` starts at column 11 (0-based char 10) of line 6.
#[test]
fn rfc54_m4b_vyx_dyn_attr_check_error_maps_column_exact() {
    let dir = rfc33_scratch("attr", RFC33_VYX_ATTR_BAD);
    let mut client = rfc33_client();
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        RFC33_APP,
    );

    let note = read_nonempty_diags_for(&mut client, "Widget.vyx");
    let diags = note
        .pointer("/params/diagnostics")
        .and_then(|d| d.as_array())
        .expect("diags array");
    let d0 = &diags[0];
    let msg = d0.get("message").and_then(|m| m.as_str()).unwrap_or("");
    assert!(msg.contains("titel"), "carries the checker message: {msg}");
    assert!(
        msg.contains("in generated code"),
        "keeps the generated breadcrumb: {msg}"
    );
    assert_eq!(
        d0.pointer("/range/start/line").and_then(|l| l.as_i64()),
        Some(5),
        "line: {note}"
    );
    assert_eq!(
        d0.pointer("/range/start/character")
            .and_then(|c| c.as_i64()),
        Some(10),
        "col: {note}"
    );
}

// ---- RFC-0053: lex errors in generated code reach the `.vyx` buffer -------

/// A `.vyx` whose template expression carries a stray `\` — the character the
/// LEXER rejects, so the synthesized module never parses at all.
const RFC53_VYX_BAD: &str = "<script>\n\
    type Row = { title: String }\n\
    props { item: Row }\n\
    </script>\n\
    <template>\n\
    <li>{{ item.title\\ }}</li>\n\
    </template>\n";

/// Like [`read_diags_for`], but skips the empty publishes (a clean file is
/// republished every analysis so a fixed error clears).
fn read_nonempty_diags_for(client: &mut LspClient, needle: &str) -> serde_json::Value {
    loop {
        let note = read_diags_for(client, needle);
        let n = note
            .pointer("/params/diagnostics")
            .and_then(|d| d.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if n > 0 {
            return note;
        }
    }
}

/// RFC-0053 §1: a LEX error inside a template expression — which leaves the
/// synthesized module unparseable — is still published into the `.vyx` buffer at
/// the expression's line/column, with the generated location kept in the message
/// as the `emit-gen` breadcrumb. Before RFC-0053 this class of error was adopted
/// into the ROOT document at line 0 as a dead-end banner string.
#[test]
fn rfc53_vyx_lex_error_publishes_into_the_vyx_buffer() {
    let dir = rfc33_scratch("lex", RFC53_VYX_BAD);
    let mut client = rfc33_client();
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        RFC33_APP,
    );

    let note = read_nonempty_diags_for(&mut client, "Widget.vyx");
    let diags = note
        .pointer("/params/diagnostics")
        .and_then(|d| d.as_array())
        .expect("diags array");
    let d0 = &diags[0];
    let msg = d0.get("message").and_then(|m| m.as_str()).unwrap_or("");
    assert!(
        msg.contains("unexpected character"),
        "carries the lexer message: {msg}"
    );
    assert!(
        msg.contains("in generated code"),
        "keeps the generated breadcrumb: {msg}"
    );
    // Line 6 (0-based 5), column 8 (0-based 7) — the start of the expression.
    assert_eq!(
        d0.pointer("/range/start/line").and_then(|l| l.as_i64()),
        Some(5),
        "line: {note}"
    );
    assert_eq!(
        d0.pointer("/range/start/character")
            .and_then(|c| c.as_i64()),
        Some(7),
        "col: {note}"
    );
}

/// RFC-0053 §2: an UNSAVED `.vyx` edit re-generates. The fixture on disk is
/// well-formed and stays untouched; the break is introduced by a `didChange`
/// overlay only, and the diagnostic must still appear in the `.vyx` buffer —
/// proving the RFC-0021 gen cache re-verifies its recorded inputs through the
/// overlay-aware resolver (a keystroke, not a save, drives the squiggle).
#[test]
fn rfc53_unsaved_vyx_edit_regenerates_and_squiggles() {
    let dir = rfc33_scratch("overlay", RFC33_VYX_OK);
    let vyx_path = dir.join("comp/Widget.vyx");
    let vyx_uri = file_uri(&vyx_path);
    let mut client = rfc33_client();
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        RFC33_APP,
    );
    // The clean analysis wires up `.vyx` ownership.
    let _ = read_diags_for(&mut client, "Widget.vyx");
    did_open(&mut client, &vyx_uri, "vyx", RFC33_VYX_OK);

    // Break it in the BUFFER only — disk keeps the good text (asserted below).
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": vyx_uri, "version": 2 },
            "contentChanges": [{ "text": RFC53_VYX_BAD }]
        }
    }));

    let note = read_nonempty_diags_for(&mut client, "Widget.vyx");
    let diags = note
        .pointer("/params/diagnostics")
        .and_then(|d| d.as_array())
        .expect("diags array");
    let msg = diags[0]
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("");
    assert!(
        msg.contains("unexpected character"),
        "the unsaved edit re-generated: {msg}"
    );
    // The file on disk was never written — the diagnostic tracks the buffer.
    let on_disk = std::fs::read_to_string(&vyx_path).unwrap();
    assert_eq!(on_disk, RFC33_VYX_OK, "the test must never touch disk");
}

/// Hover inside a template `{expr}` resolves against the synthesized module:
/// hovering `item` reports its prop type.
#[test]
fn rfc33_hover_in_vyx_template_resolves_the_prop() {
    let dir = rfc33_scratch("hover", RFC33_VYX_OK);
    let mut client = rfc33_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &app_uri, "vyrn", RFC33_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx"); // ownership wired up
    did_open(&mut client, &vyx_uri, "vyx", RFC33_VYX_OK);

    // `item` on `.vyx` line 6 (0-based 5), char 7 (the `i` of `item` inside
    // `{{ item.title }}`, RFC-0039 v2).
    let hover_id = serde_json::json!(100);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": hover_id, "method": "textDocument/hover",
        "params": { "textDocument": { "uri": vyx_uri }, "position": { "line": 5, "character": 7 } }
    }));
    let resp = client.read_response(&hover_id);
    let value = resp
        .pointer("/result/contents/value")
        .and_then(|v| v.as_str())
        .expect("hover contents: no result");
    assert!(value.contains("Row"), "hover names the prop type: {value}");
}

/// Completion after `item.` inside a template offers the record's fields.
#[test]
fn rfc33_completion_in_vyx_template_offers_record_fields() {
    let dir = rfc33_scratch("comp", RFC33_VYX_OK);
    let mut client = rfc33_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &app_uri, "vyrn", RFC33_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx");
    did_open(&mut client, &vyx_uri, "vyx", RFC33_VYX_OK);

    // Cursor just past `item.` on line 6 (0-based 5): `<li>{{ ` is 7 chars, so
    // `.` sits at char 11 and the cursor lands at char 12 (RFC-0039 v2).
    let comp_id = serde_json::json!(101);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": comp_id, "method": "textDocument/completion",
        "params": { "textDocument": { "uri": vyx_uri }, "position": { "line": 5, "character": 12 } }
    }));
    let resp = client.read_response(&comp_id);
    let items = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("completion list");
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("label").and_then(|l| l.as_str()))
        .collect();
    assert!(
        labels.contains(&"title"),
        "offers the record field `title`: {labels:?}"
    );
}

// ===========================================================================
// RFC-0042 — template editor intelligence: class / attribute / component /
// TransKey completion inside `.vyx` (and `theme.cls("…")` in `.vyrn`).
// ===========================================================================

/// 0-based (line, char) just past the first occurrence of `needle` in `text`.
fn pos_after(text: &str, needle: &str) -> (u32, u32) {
    let idx = text
        .find(needle)
        .unwrap_or_else(|| panic!("needle {needle:?} not found"))
        + needle.len();
    let pre = &text[..idx];
    let line = pre.matches('\n').count() as u32;
    let col = (pre.len() - pre.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32;
    (line, col)
}

/// A tiny theme: two brand shades, two spacings, an `md` breakpoint (so `md:`
/// variants exist), and a `book-card` safelist entry.
const RFC42_THEME: &str =
    "{ \"colors\": { \"brand\": { \"500\": \"#4f46e5\", \"600\": \"#4338ca\" } },\n\
  \"spacing\": { \"2\": \"0.5rem\", \"4\": \"1rem\" },\n\
  \"breakpoints\": { \"md\": \"768px\" },\n\
  \"safelist\": [\"book-card\"] }";

/// The app importing a themed component view over `./comp`.
const RFC42_APP: &str = "import { componentsThemed } from \"std/vyx\"\n\
    import { widget } from componentsThemed(\"./comp\", \"./theme.json\")\n\
    fn main() -> Int64 { return 0 }\n";

/// A themed widget: a valid utility class, a safelisted class, a base + variant
/// utility, and a finite `TransKey` interpolation (`{{ t("home") }}`).
const RFC42_VYX: &str = "<script>\n\
    type TransKey = String where value =~ \"(home|about)\"\n\
    fn t(k: TransKey) -> String { return k }\n\
    props { x: String }\n\
    </script>\n\
    <template>\n\
    <div class=\"flex p-4\"><span class=\"book-card\">{{ x }}</span><p class=\"bg-brand-500 md:hover:bg-brand-600\">{{ t(\"home\") }}</p></div>\n\
    </template>\n";

fn rfc42_scratch() -> std::path::PathBuf {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rfc42_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("comp")).unwrap();
    std::fs::write(dir.join("comp/Widget.vyx"), RFC42_VYX).unwrap();
    std::fs::write(dir.join("app.vyrn"), RFC42_APP).unwrap();
    std::fs::write(dir.join("theme.json"), RFC42_THEME).unwrap();
    dir
}

/// Open the themed app (wires ownership) then the widget; returns (client, vyx_uri).
fn rfc42_open() -> (LspClient, String) {
    let dir = rfc42_scratch();
    let mut client = rfc33_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    did_open(&mut client, &app_uri, "vyrn", RFC42_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx"); // ownership wired
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &vyx_uri, "vyx", RFC42_VYX);
    (client, vyx_uri)
}

fn completion_labels(client: &mut LspClient, uri: &str, line: u32, ch: u32) -> Vec<String> {
    let id = serde_json::json!(format!("c{line}_{ch}"));
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/completion",
        "params": { "textDocument": { "uri": uri }, "position": { "line": line, "character": ch } }
    }));
    let resp = client.read_response(&id);
    let items = resp
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|i| i.get("label").and_then(|l| l.as_str()).map(String::from))
        .collect()
}

fn hover_value(client: &mut LspClient, uri: &str, line: u32, ch: u32) -> Option<String> {
    let id = serde_json::json!(format!("h{line}_{ch}"));
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/hover",
        "params": { "textDocument": { "uri": uri }, "position": { "line": line, "character": ch } }
    }));
    let resp = client.read_response(&id);
    resp.pointer("/result/contents/value")
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// The target URI of a `textDocument/definition` (a scalar `Location`), or
/// `None` when the server returns no definition.
fn definition_target(client: &mut LspClient, uri: &str, line: u32, ch: u32) -> Option<String> {
    let id = serde_json::json!(format!("d{line}_{ch}"));
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/definition",
        "params": { "textDocument": { "uri": uri }, "position": { "line": line, "character": ch } }
    }));
    let resp = client.read_response(&id);
    resp.pointer("/result/uri")
        .and_then(|u| u.as_str())
        .map(String::from)
}

/// Phase A: a `class="…"` token inside a themed `.vyx` offers the `Tw` alphabet
/// filtered by the token under the cursor — utilities, a safelisted name, and
/// `md:`/`hover:` variants.
#[test]
fn rfc42_class_token_completion_offers_tw_alphabet() {
    let (mut client, uri) = rfc42_open();
    // `class="flex p-4"` — cursor right after the `p` of `p-4`.
    let (l, c) = pos_after(RFC42_VYX, "flex p");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.contains(&"p-4".to_string()),
        "utility p-4 offered: {labels:?}"
    );
    assert!(
        labels.contains(&"px-2".to_string()),
        "utility px-2 offered: {labels:?}"
    );
    // Top-level symbols must NOT leak into a class value.
    assert!(
        !labels.contains(&"widget".to_string()),
        "no top-level leak: {labels:?}"
    );

    // Safelisted name: `class="book-card"` — cursor after `book`.
    let (l, c) = pos_after(RFC42_VYX, "\"book");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.contains(&"book-card".to_string()),
        "safelisted offered: {labels:?}"
    );

    // Variant: cursor after `md:h` in `md:hover:bg-brand-600`.
    let (l, c) = pos_after(RFC42_VYX, "md:h");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.iter().any(|s| s.starts_with("md:hover:")),
        "md:hover: variants offered: {labels:?}"
    );
}

/// Phase A: hover on a class token shows the CSS rule for a utility, or
/// "safelisted (app-styled)" for a safelist entry.
#[test]
fn rfc42_hover_on_class_shows_css_or_safelisted() {
    let (mut client, uri) = rfc42_open();
    // Hover inside `bg-brand-500`.
    let (l, c) = pos_after(RFC42_VYX, "bg-brand-5");
    let v = hover_value(&mut client, &uri, l, c).expect("hover on utility class");
    assert!(
        v.contains("background-color:#4f46e5"),
        "utility CSS rule: {v}"
    );

    // Hover inside the safelisted `book-card`.
    let (l, c) = pos_after(RFC42_VYX, "book-car");
    let v = hover_value(&mut client, &uri, l, c).expect("hover on safelisted class");
    assert!(v.contains("safelisted"), "safelisted note: {v}");
}

/// Phase D: a finite `TransKey` string inside `{{ t("…") }}` completes the key
/// domain — the RFC-0033 forward map now routes string-literal contexts.
#[test]
fn rfc42_transkey_completion_inside_mustache() {
    let (mut client, uri) = rfc42_open();
    // `{{ t("home") }}` — cursor just inside the opening quote (after `t("`).
    let (l, c) = pos_after(RFC42_VYX, "t(\"");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.contains(&"home".to_string()),
        "TransKey home: {labels:?}"
    );
    assert!(
        labels.contains(&"about".to_string()),
        "TransKey about: {labels:?}"
    );
}

// --- Phase B/C: structural completion on the raw `.vyx` (no owner needed) ----

const RFC42_STRUCT_PANEL: &str = "<template>\n\
    <Book></Book>\n\
    <BookCard t></BookCard>\n\
    <a v-i></a>\n\
    <button @cl></button>\n\
    </template>\n";

const RFC42_STRUCT_CARD: &str = "<script>\nprops { title: String, url: String }\n</script>\n\
    <template>\n<div>{{ title }}</div>\n</template>\n";

fn rfc42_struct_dir() -> std::path::PathBuf {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rfc42s_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Panel.vyx"), RFC42_STRUCT_PANEL).unwrap();
    std::fs::write(dir.join("BookCard.vyx"), RFC42_STRUCT_CARD).unwrap();
    dir
}

/// Phase C: `<Boo…` offers sibling PascalCase components.
#[test]
fn rfc42_component_tag_completion() {
    let dir = rfc42_struct_dir();
    let mut client = rfc33_client();
    let uri = file_uri(&dir.join("Panel.vyx"));
    did_open(&mut client, &uri, "vyx", RFC42_STRUCT_PANEL);
    let (l, c) = pos_after(RFC42_STRUCT_PANEL, "<Book");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.contains(&"BookCard".to_string()),
        "sibling component offered: {labels:?}"
    );
}

/// Phase C: an attribute position inside a component tag offers its props.
#[test]
fn rfc42_component_prop_completion() {
    let dir = rfc42_struct_dir();
    let mut client = rfc33_client();
    let uri = file_uri(&dir.join("Panel.vyx"));
    did_open(&mut client, &uri, "vyx", RFC42_STRUCT_PANEL);
    // `<BookCard t` — cursor after the `t`.
    let (l, c) = pos_after(RFC42_STRUCT_PANEL, "<BookCard t");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.contains(&"title".to_string()),
        "prop title offered: {labels:?}"
    );
    assert!(
        labels.contains(&":url".to_string()),
        "dynamic-bound prop offered: {labels:?}"
    );
}

/// Phase B: an attribute-name position on an element offers HTML attributes and
/// `v-*` directives; an `@…` position offers DOM events.
#[test]
fn rfc42_attribute_and_event_completion() {
    let dir = rfc42_struct_dir();
    let mut client = rfc33_client();
    let uri = file_uri(&dir.join("Panel.vyx"));
    did_open(&mut client, &uri, "vyx", RFC42_STRUCT_PANEL);

    // `<a v-i` — attribute name position.
    let (l, c) = pos_after(RFC42_STRUCT_PANEL, "<a v-i");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.contains(&"v-if".to_string()),
        "directive v-if: {labels:?}"
    );
    assert!(
        labels.contains(&"href".to_string()),
        "element attr href on <a>: {labels:?}"
    );

    // `<button @cl` — event name position.
    let (l, c) = pos_after(RFC42_STRUCT_PANEL, "@cl");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.contains(&"@click".to_string()),
        "event @click: {labels:?}"
    );
}

// ===========================================================================
// RFC-0047 — semantic tokens + import hover.
// ===========================================================================

/// The token-type legend order the server registers (see `semantic_tokens_legend`).
const SEM_TYPES: &[&str] = &[
    "namespace",
    "type",
    "enumMember",
    "parameter",
    "variable",
    "property",
    "function",
    "method",
    "macro",
    "keyword",
];

/// One decoded semantic token: absolute 0-based (line, char), length, type name,
/// modifier bitset.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `len`/`mods` decoded for completeness; asserts use line/ch/ty
struct SemTok {
    line: u32,
    ch: u32,
    len: u32,
    ty: String,
    mods: u32,
}

/// Request `textDocument/semanticTokens/full` for `uri` and decode the delta
/// stream into absolute tokens.
fn semantic_tokens_full(client: &mut LspClient, uri: &str) -> Vec<SemTok> {
    let id = serde_json::json!("sem_full");
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/semanticTokens/full",
        "params": { "textDocument": { "uri": uri } }
    }));
    let resp = client.read_response(&id);
    let raw: Vec<u32> = resp
        .pointer("/result/data")
        .and_then(|d| d.as_array())
        .expect("semanticTokens result.data")
        .iter()
        .map(|v| v.as_u64().unwrap() as u32)
        .collect();
    let mut out = Vec::new();
    let (mut line, mut ch) = (0u32, 0u32);
    for chunk in raw.chunks(5) {
        let (dl, ds) = (chunk[0], chunk[1]);
        if dl == 0 {
            ch += ds;
        } else {
            line += dl;
            ch = ds;
        }
        out.push(SemTok {
            line,
            ch,
            len: chunk[2],
            ty: SEM_TYPES[chunk[3] as usize].to_string(),
            mods: chunk[4],
        });
    }
    out
}

/// The 0-based (line, char) of the first occurrence of `name` on 1-based `line`.
fn at(src: &str, line: usize, name: &str) -> (u32, u32) {
    let text = src
        .lines()
        .nth(line - 1)
        .unwrap_or_else(|| panic!("no line {line}"));
    let col = text
        .find(name)
        .unwrap_or_else(|| panic!("`{name}` not on line {line}: {text:?}"));
    ((line - 1) as u32, col as u32)
}

/// The type name of the token starting exactly at (line, ch), if classified.
fn kind_at(toks: &[SemTok], line: u32, ch: u32) -> Option<&str> {
    toks.iter()
        .find(|t| t.line == line && t.ch == ch)
        .map(|t| t.ty.as_str())
}

/// RFC-0047 §1: `semanticTokens/full` classifies each identifier by KIND — the
/// headline being that an import specifier gets its real kind (`greet`→function,
/// `Color`→type), which TextMate cannot do. Also covers function / type /
/// parameter / property / variable classification and go-to-def-consistency.
#[test]
fn semantic_tokens_classify_by_kind() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-sem-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lib.vyrn"),
        "export fn greet(name: String) -> String {\n    return name\n}\n\
         export type Color = | Red | Green | Blue\n",
    )
    .unwrap();
    let main_src = "import { greet, Color } from \"./lib\"\n\
\n\
type Point = { x: Int64, y: Int64 }\n\
\n\
fn dist(p: Point) -> Int64 {\n\
    return p.x + p.y\n\
}\n\
\n\
fn pick() -> Color {\n\
    return Green\n\
}\n\
\n\
fn main() -> Int64 {\n\
    let msg = greet(\"hi\")\n\
    return dist(Point { x: 1, y: 2 })\n\
}\n";
    let root_path = dir.join("main.vyrn");
    std::fs::write(&root_path, main_src).unwrap();
    let uri = format!("file:///{}", root_path.to_string_lossy().replace('\\', "/"));

    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", main_src);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let toks = semantic_tokens_full(&mut client, &uri);
    assert!(!toks.is_empty(), "semantic tokens returned");

    // THE headline: import specifiers get their real kind.
    let (l, c) = at(main_src, 1, "greet");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("function"),
        "import specifier `greet` → function"
    );
    let (l, c) = at(main_src, 1, "Color");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("type"),
        "import specifier `Color` → type"
    );

    // Declarations and uses.
    let (l, c) = at(main_src, 5, "dist");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("function"),
        "`dist` decl → function"
    );
    let (l, c) = at(main_src, 5, "p"); // the parameter `p`
    assert_eq!(
        kind_at(&toks, l, c),
        Some("parameter"),
        "param `p` → parameter"
    );
    let (l, c) = at(main_src, 5, "Point"); // annotation
    assert_eq!(
        kind_at(&toks, l, c),
        Some("type"),
        "`Point` annotation → type"
    );

    // A record-field member access → property (the `x` in `p.x`).
    let (l, c) = at(main_src, 6, "p.x");
    assert_eq!(
        kind_at(&toks, l, c + 2),
        Some("property"),
        "`p.x` field → property"
    );

    // A `let` binding → variable; the call target is a function.
    let (l, c) = at(main_src, 14, "msg");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("variable"),
        "`let msg` → variable"
    );
    let (l, c) = at(main_src, 14, "greet");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("function"),
        "call `greet` → function"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0047 §2: hover fires at the IMPORT SITE of a specifier (not just the use
/// site), showing the imported declaration's signature — one resolver, both
/// positions.
#[test]
fn hover_on_import_specifier_shows_signature() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-imphover-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lib.vyrn"),
        "export fn greet(name: String) -> String {\n    return name\n}\n",
    )
    .unwrap();
    let main_src = "import { greet } from \"./lib\"\n\nfn main() -> Int64 {\n    return 0\n}\n";
    let root_path = dir.join("main.vyrn");
    std::fs::write(&root_path, main_src).unwrap();
    let uri = format!("file:///{}", root_path.to_string_lossy().replace('\\', "/"));

    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", main_src);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    // `greet` inside the import list: line 1, char at "greet".
    let (l, c) = at(main_src, 1, "greet");
    let v = hover_value(&mut client, &uri, l, c).expect("hover at import specifier");
    assert!(
        v.contains("greet"),
        "import-site hover shows the signature: {v}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0047 §1 (`.vyx`): a template `{{ item.title }}` classifies through the
/// origin map — `item` (a prop) → parameter, `title` (a field) → property.
#[test]
fn semantic_tokens_in_vyx_template() {
    let dir = rfc33_scratch("sem", RFC33_VYX_OK);
    let mut client = rfc33_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &app_uri, "vyrn", RFC33_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx"); // ownership wired
    did_open(&mut client, &vyx_uri, "vyx", RFC33_VYX_OK);

    let toks = semantic_tokens_full(&mut client, &vyx_uri);
    // `<li>{{ item.title }}` on `.vyx` line 6 (0-based 5): `item` at char 7.
    assert_eq!(
        kind_at(&toks, 5, 7),
        Some("parameter"),
        "template `item` (prop) → parameter: {toks:?}"
    );
    // `.title` — the `title` member at char 12.
    assert_eq!(
        kind_at(&toks, 5, 12),
        Some("property"),
        "template `title` field → property: {toks:?}"
    );
}

// ===========================================================================
// RFC-0048 — complete `.vyx` origins: script sections + real-file pages.
// The `<script>` section now carries `//@origin` for its import + helper lines,
// and pages/layouts/errors compile against the REAL route file, so the LSP lights
// up import-specifier hover/classification in a `.vyx` script AND semantic tokens
// + class completion on a real `routes/*.vyx` (both were dead pre-RFC-0048).
// ===========================================================================

/// A component whose `<script>` imports std/time (selective + namespace) and
/// declares helper fns — the RFC-0048 §1 surface.
const RFC48_APP: &str = "import { components } from \"std/vyx\"\n\
    import { widget } from components(\"./comp\")\n\
    fn main() -> Int64 { return 0 }\n";
const RFC48_VYX: &str = "<script>\n\
    import { format, fromMillis } from \"std/time\"\n\
    import * as clk from \"std/time\"\n\
    fn shown() -> String { return format(fromMillis(0)) }\n\
    fn now() -> String { return clk.format(clk.fromMillis(0)) }\n\
    props { x: String }\n\
    </script>\n\
    <template>\n\
    <li>{{ shown() }} {{ x }} {{ now() }}</li>\n\
    </template>\n";

/// RFC-0048 §1: a `.vyx` `<script>` import specifier hovers with its signature
/// and classifies by kind — `format`/`fromMillis`→function, `clk`→namespace — and
/// a pass-through helper `fn` classifies too, all via the new script-region
/// `//@origin` directives (dead before this RFC).
#[test]
fn rfc48_vyx_script_import_hover_and_classification() {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rfc48c_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("comp")).unwrap();
    std::fs::write(dir.join("comp/Widget.vyx"), RFC48_VYX).unwrap();
    std::fs::write(dir.join("app.vyrn"), RFC48_APP).unwrap();
    let mut client = rfc33_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &app_uri, "vyrn", RFC48_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx"); // ownership wired
    did_open(&mut client, &vyx_uri, "vyx", RFC48_VYX);

    // Hover the import specifier `format` → its std/time signature.
    let (hl, hc) = at(RFC48_VYX, 2, "format");
    let v = hover_value(&mut client, &vyx_uri, hl, hc)
        .expect("hover on `.vyx` script import specifier `format`");
    assert!(
        v.contains("format"),
        "import-specifier hover shows the signature: {v}"
    );

    let toks = semantic_tokens_full(&mut client, &vyx_uri);
    let (fl, fc) = at(RFC48_VYX, 2, "format");
    assert_eq!(
        kind_at(&toks, fl, fc),
        Some("function"),
        "import `format` → function: {toks:?}"
    );
    let (ml, mc) = at(RFC48_VYX, 2, "fromMillis");
    assert_eq!(
        kind_at(&toks, ml, mc),
        Some("function"),
        "import `fromMillis` → function: {toks:?}"
    );
    let (cl, cc) = at(RFC48_VYX, 3, "clk");
    assert_eq!(
        kind_at(&toks, cl, cc),
        Some("namespace"),
        "`import * as clk` → namespace: {toks:?}"
    );
    let (sl, sc) = at(RFC48_VYX, 4, "shown");
    assert_eq!(
        kind_at(&toks, sl, sc),
        Some("function"),
        "helper `shown` def → function: {toks:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0062 §1: `Ok`/`Err`/`Some`/`None` — and user-defined enum variant
/// constructors — classify as `enumMember` in EVERY position: call (`Ok(x)`),
/// pattern (`Ok(v) =>`), bare (`None`), and nested (`Some(Ok(x))`). The headline
/// is the CALL-SITE `return Ok(...)` shape from `examples/bin/store.vyrn` that
/// drove the RFC — earlier semantic-token coverage only pinned patterns.
#[test]
fn semantic_tokens_classify_constructors_as_enum_member() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-ctor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let main_src = "type Shape = | Circle(Int64) | Dot\n\
fn make(n: Int64) -> Shape {\n\
    return Circle(n)\n\
}\n\
fn find(x: Int64) -> Result<Int64, String> {\n\
    if x > 0 {\n\
        return Ok(x)\n\
    }\n\
    return Err(\"neg\")\n\
}\n\
fn opt(x: Bool) -> Option<Result<Int64, String>> {\n\
    if x {\n\
        return Some(Ok(1))\n\
    }\n\
    return None\n\
}\n\
fn classify(s: Shape) -> Int64 {\n\
    return match s {\n\
        Circle(r) => r,\n\
        Dot => 0,\n\
    }\n\
}\n\
fn main() -> Int64 { return 0 }\n";
    let root_path = dir.join("main.vyrn");
    std::fs::write(&root_path, main_src).unwrap();
    let uri = file_uri(&root_path);

    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", main_src);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let toks = semantic_tokens_full(&mut client, &uri);
    let expect = |name: &str, line: usize| {
        let (l, c) = at(main_src, line, name);
        assert_eq!(
            kind_at(&toks, l, c),
            Some("enumMember"),
            "`{name}` on line {line} → enumMember: {toks:?}"
        );
    };
    // User-defined variant, call position (the RFC's "verify + pin" clause).
    expect("Circle", 3);
    // Builtin constructors, call position — THE headline (`return Ok(...)`).
    expect("Ok", 7);
    expect("Err", 9);
    // Nested: `Some(Ok(1))` — both the outer and inner constructor.
    expect("Some", 13);
    expect("Ok", 13);
    // Bare (no call): `None`.
    expect("None", 15);
    // Match patterns still classify (regression guard).
    expect("Circle", 19);
    expect("Dot", 20);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A `.vyx` component whose `<script>` helper returns a `Result` built with the
/// ambient `Ok` constructor — RFC-0062 §1 must classify it `enumMember` through
/// the RFC-0048 forward map, exactly as in a plain `.vyrn`.
const RFC62_APP: &str = "import { components } from \"std/vyx\"\n\
    import { widget } from components(\"./comp\")\n\
    fn main() -> Int64 { return 0 }\n";
const RFC62_VYX: &str = "<script>\n\
    fn wrap(n: Int64) -> Result<Int64, String> { return Ok(n) }\n\
    props { x: String }\n\
    </script>\n\
    <template>\n\
    <li>{{ x }}</li>\n\
    </template>\n";

#[test]
fn semantic_tokens_classify_constructor_in_vyx_script() {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rfc62_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("comp")).unwrap();
    std::fs::write(dir.join("comp/Widget.vyx"), RFC62_VYX).unwrap();
    std::fs::write(dir.join("app.vyrn"), RFC62_APP).unwrap();
    let mut client = rfc33_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &app_uri, "vyrn", RFC62_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx"); // ownership wired
    did_open(&mut client, &vyx_uri, "vyx", RFC62_VYX);

    let toks = semantic_tokens_full(&mut client, &vyx_uri);
    // `Ok` inside the script helper body on `.vyx` line 2 (1-based).
    let (l, c) = at(RFC62_VYX, 2, "Ok(n)");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("enumMember"),
        "`.vyx` script `Ok` → enumMember: {toks:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A `pagesThemed` app + a single themed route file (`routes/index.vyx`).
const RFC48_PAGE_APP: &str = "import { pagesThemed } from \"std/ui\"\n\
    import { route } from pagesThemed(\"./routes\", \"./theme.json\")\n\
    fn handle(req: Request) -> Response { return route(req) }\n";
const RFC48_INDEX: &str = "<script>\n\
    import { format, fromMillis } from \"std/time\"\n\
    import * as clk from \"std/time\"\n\
    fn shown() -> String { return format(fromMillis(0)) }\n\
    fn now() -> String { return clk.format(clk.fromMillis(0)) }\n\
    </script>\n\
    <template>\n\
    <main class=\"flex p-4\"><a class=\"mr-2 hover:bg-brand-600\">{{ shown() }}{{ now() }}</a></main>\n\
    </template>\n";

/// RFC-0048 §2: a real `routes/*.vyx` page (compiled through `pagesThemed`) now
/// has semantic tokens (was ZERO — origins pointed at the synthetic
/// `UiPageBody.vyx`) and offers `Tw` class completion in its template, exactly as
/// a `componentsThemed` component does.
#[test]
fn rfc48_page_semantic_tokens_and_class_completion() {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rfc48p_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("routes")).unwrap();
    std::fs::write(dir.join("routes/index.vyx"), RFC48_INDEX).unwrap();
    std::fs::write(dir.join("app.vyrn"), RFC48_PAGE_APP).unwrap();
    std::fs::write(dir.join("theme.json"), RFC42_THEME).unwrap();
    let mut client = rfc33_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let index_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(&mut client, &app_uri, "vyrn", RFC48_PAGE_APP);
    let _ = read_diags_for(&mut client, "index.vyx"); // page ownership wired to the real file
    did_open(&mut client, &index_uri, "vyx", RFC48_INDEX);

    // Semantic tokens now exist on the page route file, and an import specifier
    // classifies by kind.
    let toks = semantic_tokens_full(&mut client, &index_uri);
    assert!(
        !toks.is_empty(),
        "the page route file now has semantic tokens (was 0)"
    );
    let (fl, fc) = at(RFC48_INDEX, 2, "format");
    assert_eq!(
        kind_at(&toks, fl, fc),
        Some("function"),
        "page import `format` → function: {toks:?}"
    );

    // Tw class completion fires in the page template (RFC-0042 reachable in pages).
    let (l, c) = pos_after(RFC48_INDEX, "flex p");
    let labels = completion_labels(&mut client, &index_uri, l, c);
    assert!(
        labels.contains(&"p-4".to_string()),
        "Tw class completion on the page: {labels:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0048 LIVE TRANSCRIPT: a `pagesThemed` route page mirroring the user's
/// money-shot content (import specifiers, `import * as` namespace, a `mr-2
/// hover:bg-brand-600` variant class). Runs a real spawn→initialize→didOpen→
/// hover/semanticTokens/completion session and PRINTS the results (run with
/// `--nocapture` to read the transcript). Also asserts the headline outcomes so
/// it guards against regression. A lean scratch app is used (not `examples/bin`)
/// because every forward-map request re-runs the owner's generators — the full
/// bin app's rpc+openapi+tw+pages stack makes that impractically slow.
#[test]
fn rfc48_live_transcript_pagesthemed() {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rfc48t_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("routes")).unwrap();
    std::fs::write(dir.join("routes/index.vyx"), RFC48_INDEX).unwrap();
    std::fs::write(dir.join("app.vyrn"), RFC48_PAGE_APP).unwrap();
    std::fs::write(dir.join("theme.json"), RFC42_THEME).unwrap();
    let mut client = rfc33_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let index_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(&mut client, &app_uri, "vyrn", RFC48_PAGE_APP);
    let _ = read_diags_for(&mut client, "index.vyx");
    did_open(&mut client, &index_uri, "vyx", RFC48_INDEX);

    println!("\n===== RFC-0048 LIVE TRANSCRIPT (pagesThemed route: routes/index.vyx) =====");

    // Script import-specifier + namespace hovers.
    for (label, line, name) in [
        ("format", 2u32, "format"),
        ("fromMillis", 2, "fromMillis"),
        ("clk (ns)", 3, "clk"),
    ] {
        let (l, c) = at(RFC48_INDEX, line as usize, name);
        let h = hover_value(&mut client, &index_uri, l, c).unwrap_or("<none>".into());
        println!(
            "  hover {label:>11} @{}:{} -> {}",
            l + 1,
            c + 1,
            h.replace('\n', " ⏎ ")
        );
    }

    // Semantic tokens (0 → non-empty) + kinds.
    let toks = semantic_tokens_full(&mut client, &index_uri);
    println!(
        "  semanticTokens/full: {} tokens (pre-RFC-0048: 0)",
        toks.len()
    );
    for (label, line, name) in [
        ("format", 2u32, "format"),
        ("fromMillis", 2, "fromMillis"),
        ("clk", 3, "clk"),
        ("shown(def)", 4, "shown"),
    ] {
        let (l, c) = at(RFC48_INDEX, line as usize, name);
        println!(
            "  token {label:>11} @{}:{} -> {:?}",
            l + 1,
            c + 1,
            kind_at(&toks, l, c)
        );
    }

    // Tw class completion + CSS hover in the page template.
    let (cl, cc) = pos_after(RFC48_INDEX, "mr-");
    let labels = completion_labels(&mut client, &index_uri, cl, cc);
    let mr: Vec<&String> = labels
        .iter()
        .filter(|s| s.starts_with("mr-"))
        .take(4)
        .collect();
    println!(
        "  class-completion @{}:{} (in \"mr-2 hover:bg-brand-600\") -> {:?}",
        cl + 1,
        cc + 1,
        mr
    );
    let (hl, hc) = pos_after(RFC48_INDEX, "hover:bg-brand-6");
    let css = hover_value(&mut client, &index_uri, hl, hc).unwrap_or("<none>".into());
    println!(
        "  class-hover @{}:{} (hover:bg-brand-600) -> {}",
        hl + 1,
        hc + 1,
        css.replace('\n', " ⏎ ")
    );
    println!("=========================================================================\n");

    // Headline assertions (guard against regression).
    assert!(!toks.is_empty(), "page semantic tokens non-empty");
    let (l, c) = at(RFC48_INDEX, 2, "format");
    assert_eq!(kind_at(&toks, l, c), Some("function"), "format -> function");
    let (l, c) = at(RFC48_INDEX, 3, "clk");
    assert_eq!(kind_at(&toks, l, c), Some("namespace"), "clk -> namespace");
    assert!(
        mr.iter().any(|s| *s == "mr-2"),
        "mr-2 offered in page class completion"
    );
    assert!(
        css.contains("margin") || css.contains("brand") || css.contains("#43"),
        "CSS hover on variant class: {css}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// RFC-0049 — `.vyx` owner discovery + cached forward-mapping. THE tests every
// prior `.vyx` test skipped: they open ONLY the `.vyx` (never its owning
// `.vyrn` first), the normal user action. Before this RFC these returned
// NOTHING (no owner ⇒ no analysis). Now discovery finds the owner on didOpen.
// ===========================================================================

/// RFC-0049 §1/§2/§3 on a PAGE `.vyx` (owned via `pagesThemed`): open ONLY
/// `routes/index.vyx` — its owner `app.vyrn` is never opened — and assert hover,
/// semantic tokens (functions classified as `function`), go-to-definition (into
/// the imported `std/time`), and Tw class completion all work. App root found via
/// `vyrn.json`.
#[test]
fn rfc49_open_only_page_vyx_is_fully_analyzed() {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rfc49page_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("routes")).unwrap();
    std::fs::write(dir.join("routes/index.vyx"), RFC48_INDEX).unwrap();
    std::fs::write(dir.join("app.vyrn"), RFC48_PAGE_APP).unwrap();
    std::fs::write(dir.join("theme.json"), RFC42_THEME).unwrap();
    std::fs::write(dir.join("vyrn.json"), "{ \"name\": \"p\" }").unwrap();

    let mut client = rfc33_client();
    let index_uri = file_uri(&dir.join("routes/index.vyx"));
    // OPEN ONLY THE `.vyx` — the owner `app.vyrn` is never opened.
    did_open(&mut client, &index_uri, "vyx", RFC48_INDEX);

    // Hover on a script import specifier → its std/time signature (owner
    // discovered, forward map + synth analysis served from the §2 cache).
    let (hl, hc) = at(RFC48_INDEX, 2, "format");
    let v = hover_value(&mut client, &index_uri, hl, hc)
        .expect("hover works on a standalone-opened page .vyx (owner discovered)");
    assert!(
        v.contains("format"),
        "hover shows the imported signature: {v}"
    );

    // Semantic tokens non-empty and functions classify as `function` (not the
    // TextMate `variable` fallback the user reported).
    let toks = semantic_tokens_full(&mut client, &index_uri);
    assert!(
        !toks.is_empty(),
        "standalone page .vyx has semantic tokens (was 0)"
    );
    let (fl, fc) = at(RFC48_INDEX, 2, "format");
    assert_eq!(
        kind_at(&toks, fl, fc),
        Some("function"),
        "`format` → function: {toks:?}"
    );
    let (ml, mc) = at(RFC48_INDEX, 2, "fromMillis");
    assert_eq!(
        kind_at(&toks, ml, mc),
        Some("function"),
        "`fromMillis` → function: {toks:?}"
    );

    // Go-to-definition on a `format` call → the imported std/time source.
    let (dl, dc) = at(RFC48_INDEX, 4, "format");
    let target = definition_target(&mut client, &index_uri, dl, dc)
        .expect("definition jumps from a standalone page .vyx");
    assert!(
        target.contains("time"),
        "definition lands in std/time: {target}"
    );

    // Tw class completion in the template.
    let (cl, cc) = pos_after(RFC48_INDEX, "flex p");
    let labels = completion_labels(&mut client, &index_uri, cl, cc);
    assert!(
        labels.contains(&"p-4".to_string()),
        "class completion on a standalone page: {labels:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A `componentsThemed` app + a themed widget importing std/time — used by the
/// component standalone test. NO `vyrn.json`: discovery finds the app root via the
/// generator-importing `.vyrn` signal instead.
const RFC49_COMP_APP: &str = "import { componentsThemed } from \"std/vyx\"\n\
    import { widget } from componentsThemed(\"./widgets\", \"./theme.json\")\n\
    fn main() -> Int64 { return 0 }\n";
const RFC49_WIDGET: &str = "<script>\n\
    import { format, fromMillis } from \"std/time\"\n\
    props { label: String }\n\
    fn shown() -> String { return format(fromMillis(0)) }\n\
    </script>\n\
    <template>\n\
    <section class=\"flex p-4\">{{ shown() }} {{ label }}</section>\n\
    </template>\n";

/// RFC-0049 §1/§2/§3 on a COMPONENT `.vyx` (owned via `componentsThemed`): open
/// ONLY `widgets/Widget.vyx`; its owner is discovered via the generator-import
/// app-root signal (no `vyrn.json`). Same four capabilities asserted.
#[test]
fn rfc49_open_only_component_vyx_is_fully_analyzed() {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rfc49comp_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("widgets")).unwrap();
    std::fs::write(dir.join("widgets/Widget.vyx"), RFC49_WIDGET).unwrap();
    std::fs::write(dir.join("app.vyrn"), RFC49_COMP_APP).unwrap();
    std::fs::write(dir.join("theme.json"), RFC42_THEME).unwrap();

    let mut client = rfc33_client();
    let widget_uri = file_uri(&dir.join("widgets/Widget.vyx"));
    // OPEN ONLY THE `.vyx` — the owner `app.vyrn` is never opened.
    did_open(&mut client, &widget_uri, "vyx", RFC49_WIDGET);

    let (hl, hc) = at(RFC49_WIDGET, 2, "format");
    let v = hover_value(&mut client, &widget_uri, hl, hc)
        .expect("hover works on a standalone-opened component .vyx (owner discovered)");
    assert!(
        v.contains("format"),
        "hover shows the imported signature: {v}"
    );

    let toks = semantic_tokens_full(&mut client, &widget_uri);
    assert!(
        !toks.is_empty(),
        "standalone component .vyx has semantic tokens (was 0)"
    );
    let (fl, fc) = at(RFC49_WIDGET, 2, "format");
    assert_eq!(
        kind_at(&toks, fl, fc),
        Some("function"),
        "`format` → function: {toks:?}"
    );
    let (sl, sc) = at(RFC49_WIDGET, 4, "shown");
    assert_eq!(
        kind_at(&toks, sl, sc),
        Some("function"),
        "helper `shown` def → function: {toks:?}"
    );

    let (dl, dc) = at(RFC49_WIDGET, 4, "format");
    let target = definition_target(&mut client, &widget_uri, dl, dc)
        .expect("definition jumps from a standalone component .vyx");
    assert!(
        target.contains("time"),
        "definition lands in std/time: {target}"
    );

    let (cl, cc) = pos_after(RFC49_WIDGET, "flex p");
    let labels = completion_labels(&mut client, &widget_uri, cl, cc);
    assert!(
        labels.contains(&"p-4".to_string()),
        "class completion on a standalone component: {labels:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0049 LIVE TRANSCRIPT over the REAL `examples/bin` app — the user's exact
/// scenario. Opens ONLY `routes/index.vyx` (page, owned by `server.vyrn` via
/// `pagesThemed`) and ONLY `widgets/CreateForm.vyx` (component, owned by
/// `client.vyrn` via `componentsThemed`); NEITHER owner is opened. Prints the
/// hover / token-kind / definition / completion results at the user's positions,
/// and a cold-vs-warm hover-latency number proving the §2 cache makes it
/// interactive.
///
/// `#[ignore]` — an ON-DEMAND live money-shot over the whole `examples/bin`
/// stack: the harness disables the on-disk gen cache (`VYRN_NO_GEN_CACHE`), so
/// the FIRST generation of bin's `rpc`+`openapi`+`pages`+`tw`+`i18n` stack is
/// very slow (minutes) — in a real editor the gen cache is warm and this is
/// seconds. The two always-on scratch tests above are the fast regression
/// guards; run this explicitly for the transcript:
///   `cargo test -p vyrn-lsp rfc49_live -- --ignored --nocapture`
#[test]
#[ignore]
fn rfc49_live_transcript_examples_bin() {
    // The repo root is two levels above this crate's manifest dir. Build the path
    // directly (no `canonicalize`, which on Windows yields a `\\?\` verbatim path
    // that does not round-trip through a `file://` URI).
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root");
    let bin = repo.join("examples").join("bin");
    assert!(
        bin.join("vyrn.json").is_file(),
        "examples/bin exists at {bin:?}"
    );
    let index_path = bin.join("routes/index.vyx");
    let form_path = bin.join("widgets/CreateForm.vyx");
    let index_src = std::fs::read_to_string(&index_path).expect("routes/index.vyx");
    let form_src = std::fs::read_to_string(&form_path).expect("widgets/CreateForm.vyx");
    let index_uri = file_uri(&index_path);
    let form_uri = file_uri(&form_path);

    let mut client = rfc33_client();

    // ---- PAGE: open ONLY routes/index.vyx (server.vyrn never opened) ----------
    let t_open = std::time::Instant::now();
    did_open(&mut client, &index_uri, "vyx", &index_src);
    // First hover: cold (owner discovery already generated once on didOpen; this
    // is the first synth-analysis of the mapped module).
    let (fl, fc) = at(&index_src, 7, "format");
    let cold = std::time::Instant::now();
    let hf = hover_value(&mut client, &index_uri, fl, fc).unwrap_or("<none>".into());
    let cold_ms = cold.elapsed().as_millis();
    // Second identical hover: warm (§2 cache hit — no generator/analyze rerun).
    let warm = std::time::Instant::now();
    let _ = hover_value(&mut client, &index_uri, fl, fc);
    let warm_ms = warm.elapsed().as_millis();

    println!("\n===== RFC-0049 LIVE TRANSCRIPT — examples/bin, opening ONLY the .vyx =====");
    println!("[PAGE] routes/index.vyx (owner server.vyrn via pagesThemed; NOT opened)");
    println!("  didOpen+discovery: {} ms", t_open.elapsed().as_millis());
    println!("  hover format @7 -> {}", hf.replace('\n', " ⏎ "));
    let (ml, mc) = at(&index_src, 7, "fromMillis");
    let hm = hover_value(&mut client, &index_uri, ml, mc).unwrap_or("<none>".into());
    println!("  hover fromMillis @7 -> {}", hm.replace('\n', " ⏎ "));
    let (pl, pc) = at(&index_src, 18, "pasteTally");
    let hp = hover_value(&mut client, &index_uri, pl, pc).unwrap_or("<none>".into());
    println!("  hover pasteTally(def) @18 -> {}", hp.replace('\n', " ⏎ "));

    let itoks = semantic_tokens_full(&mut client, &index_uri);
    println!(
        "  semanticTokens/full: {} tokens (pre-RFC-0049: 0 — owner-less)",
        itoks.len()
    );
    for (label, line, name) in [
        ("format", 7usize, "format"),
        ("fromMillis", 7, "fromMillis"),
        ("pasteTally", 18, "pasteTally"),
        ("recent", 14, "recent"),
    ] {
        let (l, c) = at(&index_src, line, name);
        println!("  token {label:>11} @{line} -> {:?}", kind_at(&itoks, l, c));
    }

    let (dl, dc) = at(&index_src, 42, "format");
    let idef = definition_target(&mut client, &index_uri, dl, dc).unwrap_or("<none>".into());
    println!("  definition format@42 -> {}", idef);

    let (icl, icc) = pos_after(&index_src, "class=\"");
    let ilabels = completion_labels(&mut client, &index_uri, icl, icc);
    let ip: Vec<&String> = ilabels
        .iter()
        .filter(|s| s.starts_with("p-"))
        .take(4)
        .collect();
    println!(
        "  class-completion at class=\"| -> {:?} (of {} labels)",
        ip,
        ilabels.len()
    );
    println!("  HOVER LATENCY: cold {cold_ms} ms, warm {warm_ms} ms (§2 synth cache)");

    // ---- COMPONENT: open ONLY widgets/CreateForm.vyx (client.vyrn not opened) --
    did_open(&mut client, &form_uri, "vyx", &form_src);
    println!(
        "[COMPONENT] widgets/CreateForm.vyx (owner client.vyrn via componentsThemed; NOT opened)"
    );
    let (cl, cc) = at(&form_src, 3, "tBinCreate");
    let hc0 = hover_value(&mut client, &form_uri, cl, cc).unwrap_or("<none>".into());
    println!("  hover tBinCreate @3 -> {}", hc0.replace('\n', " ⏎ "));
    let (sl, sc) = at(&form_src, 3, "tBinSave");
    let hs = hover_value(&mut client, &form_uri, sl, sc).unwrap_or("<none>".into());
    println!("  hover tBinSave @3 -> {}", hs.replace('\n', " ⏎ "));
    let ftoks = semantic_tokens_full(&mut client, &form_uri);
    println!("  semanticTokens/full: {} tokens", ftoks.len());
    let (tl, tc) = at(&form_src, 3, "tBinCreate");
    println!("  token tBinCreate @3 -> {:?}", kind_at(&ftoks, tl, tc));
    let (fcl, fcc) = pos_after(&form_src, "class=\"");
    let flabels = completion_labels(&mut client, &form_uri, fcl, fcc);
    let fp: Vec<&String> = flabels
        .iter()
        .filter(|s| s.starts_with("p-"))
        .take(4)
        .collect();
    println!(
        "  class-completion at class=\"| -> {:?} (of {} labels)",
        fp,
        flabels.len()
    );
    println!("=========================================================================\n");

    // ---- Headline assertions (guard the user's exact scenario) ----------------
    assert!(
        hf.contains("format"),
        "page: hover on format resolves: {hf}"
    );
    assert!(
        !itoks.is_empty(),
        "page: standalone .vyx has semantic tokens"
    );
    let (l, c) = at(&index_src, 7, "format");
    assert_eq!(
        kind_at(&itoks, l, c),
        Some("function"),
        "page: format → function (not variable)"
    );
    let (l, c) = at(&index_src, 18, "pasteTally");
    assert_eq!(
        kind_at(&itoks, l, c),
        Some("function"),
        "page: pasteTally def → function"
    );
    assert!(
        idef.contains("time"),
        "page: definition on format → std/time: {idef}"
    );
    assert!(
        ilabels.iter().any(|s| s.starts_with("p-")),
        "page: Tw class completion fires"
    );

    assert!(
        hc0.contains("tBinCreate"),
        "component: hover on tBinCreate resolves: {hc0}"
    );
    assert!(
        !ftoks.is_empty(),
        "component: standalone .vyx has semantic tokens"
    );
    assert_eq!(
        kind_at(&ftoks, tl, tc),
        Some("function"),
        "component: tBinCreate → function"
    );
    assert!(
        flabels.iter().any(|s| s.starts_with("p-")),
        "component: Tw class completion fires"
    );
    // Warm hover must not be dramatically slower than cold — the cache holds.
    assert!(
        warm_ms <= cold_ms + 50,
        "warm hover ({warm_ms}ms) ≤ cold ({cold_ms}ms)+slack"
    );
}

// ---------------------------------------------------------------------------
// RFC-0050 — scope-aware highlight, import-path definition, namespace colour
// ---------------------------------------------------------------------------

/// One `textDocument/documentHighlight` occurrence: 1-based line, 0-based start
/// char, and the LSP `DocumentHighlightKind` (2 = Read, 3 = Write). `ch` is
/// asserted by no test yet but stays in the Debug dumps assertions print.
#[derive(Debug)]
struct Highlight {
    line: u32,
    #[allow(dead_code)]
    ch: u32,
    kind: u64,
}

/// Request `textDocument/documentHighlight` at 0-based `(line, ch)`. Returns the
/// occurrences (empty when the cursor resolves to no binding — a `Some([])`
/// result the server sends so VS Code does not word-match). Panics on a null
/// result (which would be a missing-provider regression).
fn document_highlight(client: &mut LspClient, uri: &str, line: u32, ch: u32) -> Vec<Highlight> {
    let id = serde_json::json!(format!("hl{line}_{ch}"));
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/documentHighlight",
        "params": { "textDocument": { "uri": uri }, "position": { "line": line, "character": ch } }
    }));
    let resp = client.read_response(&id);
    resp.pointer("/result")
        .and_then(|r| r.as_array())
        .expect("documentHighlight must return an array (never null)")
        .iter()
        .map(|h| Highlight {
            line: h
                .pointer("/range/start/line")
                .and_then(|v| v.as_u64())
                .unwrap() as u32
                + 1,
            ch: h
                .pointer("/range/start/character")
                .and_then(|v| v.as_u64())
                .unwrap() as u32,
            kind: h.get("kind").and_then(|k| k.as_u64()).unwrap_or(0),
        })
        .collect()
}

/// A `.vyrn` where `count` is a local in `tally`, a word in a comment, AND an
/// out-of-scope binding in `other`. Highlighting the `tally` `count` must return
/// ONLY its in-scope occurrences — never the comment word, never `other`'s
/// binding (the exact user complaint against VS Code's word-match).
const RFC50_HL: &str = "\
fn tally() -> Int64 {
    let mut count = 0
    for i in [1, 2, 3] {
        count = count + i
    }
    return count
}

fn other() -> Int64 {
    // this comment mentions count but must never be highlighted
    let count = 99
    return count
}

fn main() -> Int64 { return tally() }
";

/// RFC-0050 §1: `documentHighlight` resolves the binding under the cursor and
/// returns its ACTUAL, scope-aware references — the declaration as `Write`, uses
/// as `Read` — excluding a same-named token in a comment and an out-of-scope
/// same-named binding. The provider must be advertised so VS Code stops
/// word-matching.
#[test]
fn rfc0050_document_highlight_is_scope_aware() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-hl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("hl.vyrn");
    std::fs::write(&path, RFC50_HL).unwrap();
    let uri = file_uri(&path);

    // Advertisement.
    let mut client = LspClient::spawn().expect("spawn vyrn-lsp");
    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": init_id, "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let caps = client.read_response(&init_id);
    assert!(
        caps.pointer("/result/capabilities/documentHighlightProvider")
            .is_some(),
        "documentHighlight capability advertised"
    );
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
    did_open(&mut client, &uri, "vyrn", RFC50_HL);

    // Cursor on the `count` declaration in `tally` (1-based line 2).
    let (l, c) = at(RFC50_HL, 2, "count");
    let hls = document_highlight(&mut client, &uri, l, c);
    let lines: Vec<u32> = hls.iter().map(|h| h.line).collect();

    // In-scope occurrences only: decl on line 2, uses on line 4 (×2) and 6.
    assert!(lines.contains(&2), "decl line 2 highlighted: {hls:?}");
    assert!(lines.contains(&4), "use line 4 highlighted: {hls:?}");
    assert!(lines.contains(&6), "use line 6 highlighted: {hls:?}");
    // The declaration occurrence is a Write; the uses are Reads.
    let decl = hls.iter().find(|h| h.line == 2).unwrap();
    assert_eq!(decl.kind, 3, "declaration is Write: {decl:?}");
    assert!(
        hls.iter().filter(|h| h.line != 2).all(|h| h.kind == 2),
        "uses are Read: {hls:?}"
    );

    // Never the comment word (line 11) nor the out-of-scope binding in `other`
    // (lines 12/13) — the whole point of resolving instead of word-matching.
    assert!(
        !lines.contains(&11),
        "comment `count` NOT highlighted: {hls:?}"
    );
    assert!(
        !lines.contains(&12),
        "other()'s `count` decl NOT highlighted: {hls:?}"
    );
    assert!(
        !lines.contains(&13),
        "other()'s `count` use NOT highlighted: {hls:?}"
    );

    // An unresolved cursor (a keyword) returns an EMPTY list, not null — so VS
    // Code does not fall back to word-match.
    let hls2 = document_highlight(&mut client, &uri, 0, 0); // the `f` of `fn`
    assert!(
        hls2.is_empty(),
        "unresolved cursor yields empty (not word-match): {hls2:?}"
    );

    let _ = client.child.kill();
}

/// RFC-0050 §2: `definition` on an import SOURCE STRING resolves it through the
/// loader to the imported file's `Location` — a relative spec (`"./store"`) to
/// the sibling file, and a `std/` spec (`"std/time"`) to the std module.
#[test]
fn rfc0050_definition_on_import_path() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-imp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // NOT `get`: that is a RESERVED name (the slot table's generation-checked
    // read), so `export fn get()` is a program no valid module can contain. This
    // fixture used it and passed only because the loader registered reserved
    // names into its flat namespace — the same defect that made one `fn at`
    // produce 53 diagnostics pointing into `std/num.vyrn`. The name is
    // incidental to what this test checks, so it is a legal one now.
    std::fs::write(
        dir.join("store.vyrn"),
        "export fn stored() -> Int64 { return 1 }\n",
    )
    .unwrap();
    let app = "\
import { stored } from \"./store\"
import { now } from \"std/time\"
fn main() -> Int64 { return stored() + now() }
";
    let app_path = dir.join("app.vyrn");
    std::fs::write(&app_path, app).unwrap();
    let uri = file_uri(&app_path);

    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", app);

    // Cursor inside `"./store"` (line 1, 0-based) → store.vyrn.
    let (l, c) = at(app, 1, "./store");
    let store = definition_target(&mut client, &uri, l, c).expect("definition on ./store");
    assert!(
        store.ends_with("/store.vyrn"),
        "./store → sibling store.vyrn: {store}"
    );

    // Cursor inside `"std/time"` (line 2) → the std time module.
    let (l, c) = at(app, 2, "std/time");
    let stdt = definition_target(&mut client, &uri, l, c).expect("definition on std/time");
    assert!(
        stdt.replace('\\', "/").ends_with("std/time.vyrn"),
        "std/time → std file: {stdt}"
    );

    // A cursor NOT on an import string (the `stored` call) still does identifier
    // go-to-definition — the import-path path is additive, not a hijack.
    let (l, c) = at(app, 3, "stored");
    let g = definition_target(&mut client, &uri, l, c).expect("definition on stored() call");
    assert!(
        g.ends_with("/store.vyrn"),
        "stored() → its imported decl in store.vyrn: {g}"
    );

    let _ = client.child.kill();
}

/// RFC-0050 §3: the `import * as ns` binding token AND every `ns.` qualifier
/// classify as `namespace` (legend index 0), NOT `type` — so a namespace is
/// never mis-coloured as a type by the server. (When a user still sees it green,
/// that is their theme colouring `namespace` like `type` — standard, not a
/// server bug.)
#[test]
fn rfc0050_namespace_binding_classifies_as_namespace() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-ns-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lib.vyrn"),
        "export fn ping() -> Int64 { return 1 }\n",
    )
    .unwrap();
    let app = "\
import * as store from \"./lib\"
fn main() -> Int64 { return store.ping() }
";
    let app_path = dir.join("app.vyrn");
    std::fs::write(&app_path, app).unwrap();
    let uri = file_uri(&app_path);

    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", app);
    let toks = semantic_tokens_full(&mut client, &uri);

    // The binding `store` in `import * as store` (line 1) → namespace.
    let (l, c) = at(app, 1, "store");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("namespace"),
        "import binding → namespace: {toks:?}"
    );
    // The `store` qualifier in `store.ping()` (line 2) → namespace.
    let (l, c) = at(app, 2, "store");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("namespace"),
        "ns qualifier → namespace: {toks:?}"
    );

    let _ = client.child.kill();
}

// ===========================================================================
// RFC-0051 — hover quality: `///` docs, member hover, record structure, and
// class-token precision. Every assertion here is a symptom the user measured
// against the deployed server before the fix (docs never rendered anywhere;
// `x.field` / `ns.member` hovered null inside a `.vyx`).
// ===========================================================================

/// §1: a `///` doc renders beneath the signature — for a declaration in the
/// open document AND for one imported from another module.
#[test]
fn rfc51_doc_comment_renders_in_hover() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-51doc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lib.vyrn"),
        "/// Adds one. A parity citizen: pure.\nexport fn bump(n: Int64) -> Int64 { return n + 1 }\n",
    )
    .unwrap();
    let app = "\
import { bump } from \"./lib\"
/// The entry point, documented.
fn main() -> Int64 { return bump(1) }
";
    let app_path = dir.join("app.vyrn");
    std::fs::write(&app_path, app).unwrap();
    let uri = file_uri(&app_path);

    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", app);

    // Imported: hover `bump` at its use site.
    let (l, c) = at(app, 3, "bump(1)");
    let h = hover_value(&mut client, &uri, l, c).expect("hover on imported fn");
    // The signature comes first, fenced as ```vyrn so the editor highlights it
    // with the grammar the extension ships (`fence_signature`).
    assert!(
        h.starts_with("```vyrn\nfn bump(n: Int64) -> Int64\n```"),
        "fenced signature first: {h}"
    );
    assert!(
        h.contains("Adds one. A parity citizen: pure."),
        "imported doc rendered: {h}"
    );

    // Own document: hover `main` at its declaration.
    let (l, c) = at(app, 3, "main");
    let h = hover_value(&mut client, &uri, l, c).expect("hover on own fn");
    assert!(
        h.contains("The entry point, documented."),
        "own-module doc rendered: {h}"
    );

    let _ = client.child.kill();
}

/// §1 + §2: a namespace over a GENERATED module (`i18n(..)`) resolves its
/// members, and the generated `///` — the translation itself — is the doc.
/// This is RFC-0020's "hover a key, see the translation" working for the first
/// time: before, `namespace_members` tried to `read` the module's banner key,
/// failed, and the namespace had zero members (so `t.x` hovered null).
#[test]
fn rfc51_generated_namespace_member_hover_shows_translation() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-51i18n-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("strings")).unwrap();
    std::fs::write(
        dir.join("strings/en.json"),
        "{ \"app\": { \"tagline\": \"Paste text, get a short link.\" } }",
    )
    .unwrap();
    let app = "\
import { i18n } from \"std/i18n\"
import * as t from i18n(\"./strings\")
fn main() -> Int64 { let s = t.appTagline() return 0 }
";
    let app_path = dir.join("app.vyrn");
    std::fs::write(&app_path, app).unwrap();
    let uri = file_uri(&app_path);

    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", app);

    let (l, c) = at(app, 3, "appTagline");
    let h = hover_value(&mut client, &uri, l, c).expect("hover on generated ns member");
    assert!(
        h.contains("fn appTagline() -> String"),
        "member signature: {h}"
    );
    assert!(
        h.contains("Paste text, get a short link."),
        "translation as doc: {h}"
    );
    assert!(h.contains("via namespace `t`"), "notes the namespace: {h}");

    let _ = client.child.kill();
}

/// §2 + §3 inside a `.vyx` template: hovering the segment after the `.`
/// resolves the record FIELD (it was null — `resolve` had no member path at
/// all), and hovering the receiver shows the record's SHAPE, not just its name.
#[test]
fn rfc51_member_and_structure_hover_in_vyx_template() {
    let dir = rfc33_scratch("hover51", RFC33_VYX_OK);
    let mut client = rfc33_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &app_uri, "vyrn", RFC33_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx");
    did_open(&mut client, &vyx_uri, "vyx", RFC33_VYX_OK);

    // `<li>{{ item.title }}` on line 6 (0-based 5): `item` at char 7, the `.`
    // at 11, `title` from 12.
    let h = hover_value(&mut client, &vyx_uri, 5, 13).expect("member hover in a .vyx template");
    assert!(h.contains("title: String"), "the field's type: {h}");

    let h = hover_value(&mut client, &vyx_uri, 5, 7).expect("receiver hover");
    assert!(h.contains("item: Row"), "the value's type: {h}");
    assert!(
        h.contains("type Row = { title: String }"),
        "the record's shape: {h}"
    );

    let _ = client.child.kill();
}

/// §4: with two classes in one `class="…"`, hover reports the token actually
/// under the cursor — and class COMPLETION replaces exactly that token's range
/// (hover and completion must agree on where a token starts).
#[test]
fn rfc51_class_hover_and_completion_agree_on_the_token_under_the_cursor() {
    let (mut client, uri) = rfc42_open();

    // `class="bg-brand-500 md:hover:bg-brand-600"` — the first token.
    let (l, c) = pos_after(RFC42_VYX, "bg-brand-5");
    let h = hover_value(&mut client, &uri, l, c).expect("hover on the first class");
    assert!(
        h.contains("`bg-brand-500`"),
        "the token under the cursor: {h}"
    );
    assert!(
        !h.contains("md:hover:"),
        "not the LAST token on the line: {h}"
    );

    // The second token, on the same attribute.
    let (l2, c2) = pos_after(RFC42_VYX, "md:hover:bg-brand-6");
    let h2 = hover_value(&mut client, &uri, l2, c2).expect("hover on the second class");
    assert!(
        h2.contains("`md:hover:bg-brand-600`"),
        "the variant token: {h2}"
    );

    // Completion at the same cursor replaces the whole token, from its start.
    let id = serde_json::json!("rfc51-cls");
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/completion",
        "params": { "textDocument": { "uri": uri }, "position": { "line": l2, "character": c2 } }
    }));
    let resp = client.read_response(&id);
    let items = resp
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let item = items
        .iter()
        .find(|i| i.get("label").and_then(|s| s.as_str()) == Some("md:hover:bg-brand-600"))
        .expect("the variant class is offered");
    let start = item
        .pointer("/textEdit/range/start/character")
        .and_then(|v| v.as_u64())
        .expect("a replace range");
    let end = item
        .pointer("/textEdit/range/end/character")
        .and_then(|v| v.as_u64())
        .unwrap();
    // The token starts right after the space that ends `bg-brand-500`.
    let tok_start = (c2 as u64) - ("md:hover:bg-brand-6".len() as u64);
    assert_eq!(
        start, tok_start,
        "completion replaces from the token start: {item}"
    );
    assert_eq!(end, c2 as u64, "…up to the cursor: {item}");

    let _ = client.child.kill();
}

// ===========================================================================
// RFC-0052 — a safelisted class hovers with the app's OWN CSS.
// ===========================================================================

/// Theme with two safelisted names: `book-card` (the app styles it) and
/// `no-style` (it doesn't).
const RFC52_THEME: &str = "{ \"colors\": { \"brand\": { \"500\": \"#4f46e5\" } },\n\
  \"spacing\": { \"2\": \"0.5rem\" },\n\
  \"safelist\": [\"book-card\", \"no-style\"] }";

const RFC52_APP: &str = "import { componentsThemed } from \"std/vyx\"\n\
    import { widget } from componentsThemed(\"./comp\", \"./theme.json\")\n\
    fn main() -> Int64 { return 0 }\n";

const RFC52_VYX: &str = "<script>\n\
    props { x: String }\n\
    </script>\n\
    <template>\n\
    <div class=\"book-card\"><span class=\"no-style\">{{ x }}</span><p class=\"bg-brand-500\">{{ x }}</p></div>\n\
    </template>\n";

/// A layout declaring the app's stylesheet (RFC-0041 `head`). Only its
/// `stylesheet "…"` text matters to discovery.
const RFC52_LAYOUT: &str = "<script>\nhead {\n    stylesheet \"/app.css\"\n}\n</script>\n\
    <template>\n<div><slot/></div>\n</template>\n";

/// The declared stylesheet. `.book-card-x` / `.book-cards` must NOT match
/// `book-card` (whole-token rule); the descendant and `:hover` rules must.
const RFC52_CSS: &str = ".book-card-x { color: red; }\n\
.book-cards { color: blue; }\n\
li.list .book-card {\n  padding: 2px;\n}\n\
.book-card:hover { color: green; }\n";

/// A second stylesheet nobody declares — it must be ignored while a declared one
/// exists (discovery order), so its marker never shows up in a hover.
const RFC52_DECOY_CSS: &str = ".book-card { content: \"DECOYRULE\"; }\n";

fn rfc52_scratch() -> std::path::PathBuf {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rfc52_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("comp")).unwrap();
    std::fs::create_dir_all(dir.join("public")).unwrap();
    std::fs::create_dir_all(dir.join("routes")).unwrap();
    std::fs::write(dir.join("comp/Widget.vyx"), RFC52_VYX).unwrap();
    std::fs::write(dir.join("routes/layout.vyx"), RFC52_LAYOUT).unwrap();
    std::fs::write(dir.join("app.vyrn"), RFC52_APP).unwrap();
    std::fs::write(dir.join("theme.json"), RFC52_THEME).unwrap();
    std::fs::write(dir.join("public/app.css"), RFC52_CSS).unwrap();
    std::fs::write(dir.join("public/decoy.css"), RFC52_DECOY_CSS).unwrap();
    dir
}

fn rfc52_open() -> (LspClient, String) {
    let dir = rfc52_scratch();
    let mut client = rfc33_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    did_open(&mut client, &app_uri, "vyrn", RFC52_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx"); // ownership wired
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &vyx_uri, "vyx", RFC52_VYX);
    (client, vyx_uri)
}

/// A safelisted class with app CSS: the honest "safelisted (app-styled)" line
/// STAYS and the app's own rule(s) are appended with `file:line`. The whole-token
/// rule keeps `.book-card-x` / `.book-cards` out, and the undeclared decoy
/// stylesheet is not consulted while a declared one exists.
#[test]
fn rfc52_safelisted_hover_shows_the_apps_own_css() {
    let (mut client, uri) = rfc52_open();
    let (l, c) = pos_after(RFC52_VYX, "book-car");
    let v = hover_value(&mut client, &uri, l, c).expect("hover on safelisted class");
    assert!(
        v.contains("safelisted (app-styled)"),
        "keeps the safelisted line: {v}"
    );
    assert!(
        v.contains("li.list .book-card"),
        "descendant rule shown: {v}"
    );
    assert!(v.contains("padding: 2px"), "rule body verbatim: {v}");
    assert!(v.contains(".book-card:hover"), "the :hover rule too: {v}");
    assert!(
        v.contains("public/app.css:3"),
        "declared sheet + 1-based line: {v}"
    );
    // Whole-token matching: neither the longer class nor the plural one match.
    assert!(
        !v.contains("book-card-x"),
        "`.book-card-x` must not match: {v}"
    );
    assert!(
        !v.contains("book-cards"),
        "`.book-cards` must not match: {v}"
    );
    // Discovery order: the declared sheet wins; the undeclared decoy is unused.
    assert!(
        !v.contains("DECOYRULE"),
        "undeclared stylesheet not consulted: {v}"
    );
    let _ = client.child.kill();
}

/// A safelisted class the app never styles: today's text, unchanged.
#[test]
fn rfc52_safelisted_without_a_rule_is_unchanged() {
    let (mut client, uri) = rfc52_open();
    let (l, c) = pos_after(RFC52_VYX, "no-sty");
    let v = hover_value(&mut client, &uri, l, c).expect("hover on safelisted class");
    assert_eq!(
        v, "**`no-style`** — safelisted (app-styled)",
        "unchanged: {v}"
    );
    let _ = client.child.kill();
}

/// No regression: a utility class still hovers with its GENERATED rule and gains
/// no app-CSS block.
#[test]
fn rfc52_utility_hover_is_unchanged() {
    let (mut client, uri) = rfc52_open();
    let (l, c) = pos_after(RFC52_VYX, "bg-brand-5");
    let v = hover_value(&mut client, &uri, l, c).expect("hover on utility class");
    assert!(v.contains("`Tw` utility class"), "utility hover: {v}");
    assert!(v.contains("background-color:#4f46e5"), "generated CSS: {v}");
    assert!(!v.contains("app.css"), "no app-CSS block appended: {v}");
    let _ = client.child.kill();
}

// ---- RFC-0054: code quotes in the editor -----------------------------------

/// A broken `vyrn"…"` skeleton is an ordinary parse diagnostic IN THE GENERATOR'S
/// FILE at the literal's line — published against the `.vyrn` URI opened over
/// stdio in the VS Code URI form (`file:///c%3A/…`, drive lower-cased and
/// percent-encoded). It never becomes a runtime "unexpected character in
/// generated code".
#[test]
fn rfc54_broken_skeleton_publishes_in_the_generator_file() {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rfc54_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Line 2 of the literal is `type Query {` — GraphQL, not Vyrn, so the skeleton
    // parses in no mode.
    let gen = "export gen fn mk(name: String) -> String {\n\
               let body = vyrn\"\"\"\n\
               type Query {\n\
               }\"\"\"\n\
               return render(body)\n\
               }\n";
    let gen_path = dir.join("gen.vyrn");
    std::fs::write(&gen_path, gen).unwrap();

    // Build the VS Code percent-encoded URI form: lower-case drive + `%3A`.
    let raw = gen_path.to_string_lossy().replace('\\', "/");
    let uri = if raw.len() > 2 && raw.as_bytes()[1] == b':' {
        let drive = raw[..1].to_lowercase();
        format!("file:///{drive}%3A{}", &raw[2..])
    } else {
        format!("file://{raw}")
    };

    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", gen);
    let note = read_nonempty_diags_for(&mut client, "gen.vyrn");
    let diags = note
        .pointer("/params/diagnostics")
        .and_then(|d| d.as_array())
        .expect("diags");
    let msg = diags[0]
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("");
    assert!(
        msg.contains("skeleton does not parse"),
        "skeleton message: {msg}"
    );
    // The literal opens on line 2 (`vyrn"""`); its content starts on line 3
    // (`type Query {`), 1-based → LSP line 2 (0-based).
    let line = diags[0]
        .pointer("/range/start/line")
        .and_then(|l| l.as_i64());
    assert_eq!(line, Some(2), "reported at the literal's line: {note}");
    let _ = client.child.kill();
}

/// Semantic tokens must not crash on the new `vyrn"…"` / `vyrn"""…"""` tag — it is
/// classified like any tagged template (the tag ident + the string token).
#[test]
fn rfc54_semantic_tokens_do_not_crash_on_a_code_quote() {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rfc54sem_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = "export gen fn mk(name: String) -> String {\n\
               let body = vyrn\"\"\"export fn greet\\{name}() -> String {\n\
               return \"hi\"\n\
               }\"\"\"\n\
               return render(body)\n\
               }\n";
    let path = dir.join("q.vyrn");
    std::fs::write(&path, src).unwrap();
    let uri = file_uri(&path);
    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", src);

    let req_id = serde_json::json!(700);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": req_id, "method": "textDocument/semanticTokens/full",
        "params": { "textDocument": { "uri": uri } }
    }));
    let resp = client.read_response(&req_id);
    // A valid (non-error) response with a data array — proves no panic on the tag.
    assert!(
        resp.get("error").is_none(),
        "semantic tokens errored: {resp}"
    );
    assert!(
        resp.pointer("/result/data")
            .and_then(|d| d.as_array())
            .is_some(),
        "semantic tokens returned data: {resp}"
    );
    let _ = client.child.kill();
}

// ===========================================================================
// RFC-0064: the `vyrn/isDevEntry` predicate — is a document a dev-server entry
// (imports std/rpc AND has an rpcServer(...) call site)? The extension queries
// this to render the "▶ Run dev server" CodeLens.
// ===========================================================================

/// Send `vyrn/isDevEntry` for `uri` and return the boolean result.
fn query_is_dev_entry(client: &mut LspClient, uri: &str, id: u64) -> bool {
    let req_id = serde_json::json!(id);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": req_id, "method": "vyrn/isDevEntry",
        "params": { "textDocument": { "uri": uri } }
    }));
    let resp = client.read_response(&req_id);
    assert!(resp.get("error").is_none(), "isDevEntry errored: {resp}");
    resp.get("result")
        .and_then(|r| r.as_bool())
        .expect("isDevEntry result is a bool")
}

#[test]
fn is_dev_entry_positive_and_negative() {
    let mut client = rfc33_client();

    // --- integration via an open buffer (didOpen → server.docs → predicate) ---
    // A minimal serve-calling root: imports std/rpc AND an rpcServer(...) call site.
    let server_src = "import { rpcServer } from \"std/rpc\"\n\
        import { rpcHandle } from rpcServer(\"./contract\")\n\
        fn handle(req: Request) -> Response { return rpcHandle(req) }\n";
    let server_uri = "file:///n%3A/lang/scratch/server.vyrn";
    did_open(&mut client, server_uri, "vyrn", server_src);
    assert!(
        query_is_dev_entry(&mut client, server_uri, 900),
        "a serve-calling root is a dev entry"
    );

    // An rpc CLIENT: imports std/rpc but calls rpcClient(, not a server — excluded.
    let client_src = "import { rpcClient } from \"std/rpc\"\n\
        import * as api from rpcClient(\"./contract\")\n\
        fn main() -> Int64 { return 0 }\n";
    let client_uri = "file:///n%3A/lang/scratch/client.vyrn";
    did_open(&mut client, client_uri, "vyrn", client_src);
    assert!(
        !query_is_dev_entry(&mut client, client_uri, 901),
        "an rpc client is NOT a dev entry"
    );

    // A plain library / CLI module: no std/rpc at all.
    let lib_src = "fn main() -> Int64 { print(\"hi\") return 0 }\n";
    let lib_uri = "file:///n%3A/lang/scratch/lib.vyrn";
    did_open(&mut client, lib_uri, "vyrn", lib_src);
    assert!(
        !query_is_dev_entry(&mut client, lib_uri, 902),
        "a plain module is NOT a dev entry"
    );

    // --- the exact files the RFC names, resolved from DISK (the handler's
    //     no-buffer fallback; no heavy didOpen analysis needed). ----------------
    for (rel, expect) in [
        ("bin/server.vyrn", true),
        ("shelf/server.vyrn", true),
        ("vlog.vyrn", false),
        ("bin/client/boot.vyrn", false),
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples")
            .join(rel);
        let uri = file_uri(&path);
        let got = query_is_dev_entry(&mut client, &uri, 910);
        assert_eq!(got, expect, "isDevEntry({rel}) should be {expect}");
    }

    let _ = client.child.kill();
}

// ===========================================================================
// RFC-0071 M2b — the warning channel reaches the editor.
//
// A generator's non-fatal notice rides its output as a `//@warning` directive
// carrying an origin position. The loader lifts it into a `Severity::Warning` on
// the SUCCESS path, and the LSP publishes it against the file the position
// names, as `DiagnosticSeverity.Warning` (2) — a squiggle the author can act on,
// in a buffer they actually have open, on a project that compiles.
//
// M2c deleted the deprecation notices that were this channel's only real
// producer, so the fixture is a synthetic generator: the channel is retained on
// its own merits, and this is the proof it still reaches an editor.
// ===========================================================================

/// A generator emitting one warning positioned at `legacy.vyrn:2:1`.
const WARN_GEN: &str = "export gen fn legacy(path: String) -> String {\n    \
    return \"//@warning \" + path + \":2:1 `fn old` is deprecated — write `fn new` instead\n\" +\n        \
    \"export fn greeting() -> String {\n    return \\\"hi\\\"\n}\n\"\n}\n";

const WARN_LEGACY: &str = "// A module on the old form.\nfn old() -> Int64 {\n    return 1\n}\n";

const WARN_APP: &str = "import { legacy } from \"./gen\"\n\
    import { greeting } from legacy(\"./legacy.vyrn\")\n\
    fn main() -> Int64 { print(greeting()) return 0 }\n";

/// A warning is published INTO the buffer its origin position names, at that
/// line/column, with WARNING severity — and the project still compiles, which is
/// the whole difference between a warning and an error.
#[test]
fn rfc71_a_generator_warning_publishes_into_the_input_buffer() {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rfc71_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("gen.vyrn"), WARN_GEN).unwrap();
    std::fs::write(dir.join("legacy.vyrn"), WARN_LEGACY).unwrap();
    std::fs::write(dir.join("app.vyrn"), WARN_APP).unwrap();

    let mut client = rfc33_client();
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        WARN_APP,
    );

    let note = read_diags_for(&mut client, "legacy.vyrn");
    let diags = note
        .pointer("/params/diagnostics")
        .and_then(|d| d.as_array())
        .expect("diags array");
    assert_eq!(diags.len(), 1, "one deprecation: {note}");
    let d0 = &diags[0];
    // 2 == DiagnosticSeverity.Warning. An ERROR here would mean the editor is
    // telling the author their working project is broken.
    assert_eq!(
        d0.get("severity").and_then(|s| s.as_i64()),
        Some(2),
        "warning severity: {note}"
    );
    let msg = d0.get("message").and_then(|m| m.as_str()).unwrap_or("");
    assert!(
        msg.contains("`fn old` is deprecated"),
        "carries the notice: {msg}"
    );
    // `legacy.vyrn` line 2 (0-based 1), column 1 (0-based 0).
    assert_eq!(
        d0.pointer("/range/start/line").and_then(|l| l.as_i64()),
        Some(1),
        "line: {note}"
    );

    let _ = client.child.kill();
}

// ===========================================================================
// RFC-0071 M4 — contract members in the editor.
//
// The milestone the whole RFC was justified by: `head` and `data` became
// declarations in M1/M2 so the editor could SEE them. These drive the real
// server over stdio against real page fixtures and assert on the payloads —
// this repo has twice shipped editor behaviour that passed unit tests and did
// nothing in the editor.
// ===========================================================================

/// A project whose `routes/` directory is consumed by `std/ui`'s pages
/// generator. NOTHING here says "routes are pages": the LSP learns it from this
/// import, the way the RFC's fallback describes.
const M4_APP: &str = "import { pages } from \"std/ui\"\n\
    import { components } from \"std/vyx\"\n\
    import { route } from pages(\"./routes\")\n\
    import { widget } from components(\"./widgets\")\n\
    fn main() -> Int64 { return 0 }\n";

const M4_MANIFEST: &str = "{ \"name\": \"m4\", \"main\": \"app.vyrn\" }\n";

/// A page that declares `head` and is missing `data` — so completion has
/// something to offer and something to have already dropped.
const M4_PAGE: &str = "<script>\n\
    import { Head, noHead } from \"std/ui\"\n\
    export fn head() -> Head {\n    return noHead()\n}\n\
    \n\
    </script>\n\
    <template>\n<h1>home</h1>\n</template>\n";

/// A layout beside it. `head { … }` is a LAYOUT form (RFC-0071 M2b/M2c): a
/// layout has no contract to be a member of, so the editor must offer it
/// nothing.
const M4_LAYOUT: &str = "<script>\n\
    head {\n    stylesheet \"/s.css\"\n}\n\
    \n\
    </script>\n\
    <template>\n<div><slot/></div>\n</template>\n";

/// A `.vyrn` page with a near-miss export — the did-you-mean case.
const M4_TYPO_PAGE: &str = "import { Query, query } from \"std/ui\"\n\
    \n\
    export fn dta() -> Query<Int64> {\n    return query(one)\n}\n\
    \n\
    fn one() -> Int64 {\n    return 1\n}\n";

fn m4_scratch(tag: &str) -> std::path::PathBuf {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_m4_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("routes")).unwrap();
    std::fs::create_dir_all(dir.join("widgets")).unwrap();
    std::fs::write(dir.join("vyrn.json"), M4_MANIFEST).unwrap();
    std::fs::write(dir.join("app.vyrn"), M4_APP).unwrap();
    std::fs::write(dir.join("routes/index.vyx"), M4_PAGE).unwrap();
    std::fs::write(dir.join("routes/layout.vyx"), M4_LAYOUT).unwrap();
    std::fs::write(dir.join("routes/typo.vyrn"), M4_TYPO_PAGE).unwrap();
    dir
}

/// Every completion item at `(line, ch)`, as raw JSON, so an assertion can look
/// at whatever field it needs (insertText, insertTextFormat, sortText, …).
fn completion_items(
    client: &mut LspClient,
    uri: &str,
    line: u32,
    ch: u32,
) -> Vec<serde_json::Value> {
    let mut ids = Ids::new();
    let id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": ch }
        }
    }));
    let resp = client.read_response(&id);
    resp.get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default()
}

fn code_actions(
    client: &mut LspClient,
    uri: &str,
    line: u32,
    start: u32,
    end: u32,
) -> Vec<serde_json::Value> {
    let mut ids = Ids::new();
    let id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/codeAction",
        "params": {
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": line, "character": start },
                "end": { "line": line, "character": end }
            },
            "context": { "diagnostics": [] }
        }
    }));
    let resp = client.read_response(&id);
    resp.get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default()
}

/// The capability has to be advertised or VS Code never asks.
#[test]
fn m4_code_action_capability_is_advertised() {
    let mut client = LspClient::spawn().expect("spawn");
    let id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let resp = client.read_response(&id);
    let caps = resp.pointer("/result/capabilities").expect("capabilities");
    assert_eq!(
        caps.get("codeActionProvider"),
        Some(&serde_json::json!(true)),
        "codeActionProvider advertised: {caps}"
    );
    let _ = client.child.kill();
}

/// **Completion.** At module scope in a `.vyx` page, the contract's members are
/// offered as snippets, each inserting the full declaration, each carrying the
/// member's `///` doc — and the one the page already wrote is gone.
#[test]
fn m4_completion_offers_contract_members_in_a_vyx_page() {
    let dir = m4_scratch("comp");
    let mut client = rfc33_client();
    let page_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        M4_APP,
    );
    did_open(&mut client, &page_uri, "vyx", M4_PAGE);

    // 0-based line 5: the blank line after head's body, inside <script> — module
    // scope.
    let items = completion_items(&mut client, &page_uri, 5, 0);
    let snippets: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("insertText").and_then(|t| t.as_str()))
        .collect();
    assert!(
        snippets.contains(&"export fn data() -> Query<T> {\n    return $0\n}"),
        "the blocking data shape inserts its whole declaration: {snippets:?}"
    );
    assert!(
        snippets.contains(&"export fn data() -> Lazy<T> {\n    return $0\n}"),
        "and so does the lazy one — a multi-shape member offers its shapes: {snippets:?}"
    );
    assert!(
        snippets.iter().any(|s| s.contains("ParamQuery<P, T>"))
            && snippets.iter().any(|s| s.contains("ParamLazy<P, T>")),
        "all four `data` shapes: {snippets:?}"
    );
    assert!(
        !snippets.iter().any(|s| s.starts_with("export fn head")),
        "`head` is already written, so it is not offered again: {snippets:?}"
    );
    // Nor is `page`: the `<template>` below is the page, so accepting the
    // snippet would insert a declaration that collides with the generated
    // export. What a file's FORM writes counts as already written.
    assert!(
        !snippets.iter().any(|s| s.starts_with("export fn page")),
        "the template already wrote the page: {snippets:?}"
    );
    assert!(
        snippets.iter().any(|s| s.starts_with("export fn respond")),
        "`respond` is the alternative to a view and no template writes one: {snippets:?}"
    );

    let data = items
        .iter()
        .find(|i| i.get("label").and_then(|l| l.as_str()) == Some("data"))
        .expect("a `data` item");
    // 2 == InsertTextFormat.Snippet. Without it VS Code inserts the tabstops as
    // literal text.
    assert_eq!(
        data.get("insertTextFormat").and_then(|f| f.as_i64()),
        Some(2)
    );
    let detail = data.get("detail").and_then(|d| d.as_str()).unwrap_or("");
    assert!(
        detail.contains("contract `Page` (std/ui)"),
        "detail names the contract: {detail}"
    );
    let doc = data
        .pointer("/documentation/value")
        .and_then(|d| d.as_str())
        .unwrap_or("");
    assert!(
        doc.contains("data, resolved before render"),
        "the /// doc rides along: {doc:?}"
    );

    // Contract members sort above the document's own symbols.
    let ordinary = items
        .iter()
        .find(|i| i.get("insertText").is_none())
        .and_then(|i| {
            i.get("sortText")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
        });
    if let Some(o) = ordinary {
        let s = data.get("sortText").and_then(|s| s.as_str()).unwrap_or("z");
        assert!(s < o.as_str(), "contract members sort first: {s} vs {o}");
    }
    let _ = client.child.kill();
}

/// Completion must NOT offer page members inside a layout: `head { … }` is a
/// layout form and a layout has no contract to be a member of.
#[test]
fn m4_completion_offers_nothing_in_a_layout() {
    let dir = m4_scratch("layout");
    let mut client = rfc33_client();
    let layout_uri = file_uri(&dir.join("routes/layout.vyx"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        M4_APP,
    );
    did_open(&mut client, &layout_uri, "vyx", M4_LAYOUT);

    // 0-based line 4: the blank line after the head block, inside <script>.
    let items = completion_items(&mut client, &layout_uri, 4, 0);
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("label").and_then(|l| l.as_str()))
        .collect();
    assert!(
        !labels.contains(&"data"),
        "a layout is not a page: {labels:?}"
    );
    assert!(
        items.iter().all(|i| i.get("insertText").is_none()),
        "no contract snippets in a layout: {items:?}"
    );
    let _ = client.child.kill();
}

/// Completion must not fire inside a function BODY — a contract member is a
/// declaration, and `export fn` is not valid there.
#[test]
fn m4_completion_does_not_fire_inside_a_body() {
    let dir = m4_scratch("body");
    let mut client = rfc33_client();
    let page_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        M4_APP,
    );
    did_open(&mut client, &page_uri, "vyx", M4_PAGE);

    // 0-based line 3 = `    return noHead()` — inside head's body.
    let items = completion_items(&mut client, &page_uri, 3, 4);
    assert!(
        items.iter().all(|i| i.get("insertText").is_none()),
        "no declaration snippets inside a body: {items:?}"
    );
    let _ = client.child.kill();
}

/// **Hover.** Hovering the member's name in a page shows its shapes, its doc,
/// and the contract it belongs to.
#[test]
fn m4_hover_names_the_contract() {
    let dir = m4_scratch("hover");
    let mut client = rfc33_client();
    let page_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        M4_APP,
    );
    did_open(&mut client, &page_uri, "vyx", M4_PAGE);

    // 0-based line 2 = `export fn head() -> Head {`; `head` starts at char 10.
    let h = hover_value(&mut client, &page_uri, 2, 11).expect("hover on `head`");
    assert!(
        h.contains("member of contract `Page` (std/ui)"),
        "names the contract:\n{h}"
    );
    assert!(h.contains("fn() -> Head"), "names the type:\n{h}");
    assert!(
        h.contains("head takes what the view takes"),
        "carries the /// doc:\n{h}"
    );
    let _ = client.child.kill();
}

/// **Go-to-def.** From the member's name in a page, jump into the contract
/// declaration in `std/ui.vyrn` — landing on the member, not the file top.
#[test]
fn m4_definition_jumps_into_the_contract() {
    let dir = m4_scratch("def");
    let mut client = rfc33_client();
    let page_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        M4_APP,
    );
    did_open(&mut client, &page_uri, "vyx", M4_PAGE);

    let mut ids = Ids::new();
    let id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": page_uri },
            "position": { "line": 2, "character": 11 }
        }
    }));
    let resp = client.read_response(&id);
    let loc = resp.get("result").expect("a definition");
    let target = loc.get("uri").and_then(|u| u.as_str()).unwrap_or("");
    assert!(
        target.ends_with("std/ui.vyrn"),
        "jumps into std/ui: {target}"
    );

    // The range must cover the literal member name in the real std/ui source.
    let line = loc
        .pointer("/range/start/line")
        .and_then(|l| l.as_i64())
        .unwrap() as usize;
    let start = loc
        .pointer("/range/start/character")
        .and_then(|c| c.as_i64())
        .unwrap() as usize;
    let end = loc
        .pointer("/range/end/character")
        .and_then(|c| c.as_i64())
        .unwrap() as usize;
    let std_ui = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../std/ui.vyrn"))
        .expect("std/ui.vyrn");
    let text = std_ui.lines().nth(line).expect("the declaration line");
    let name: String = text.chars().skip(start).take(end - start).collect();
    assert_eq!(name, "head", "lands on the member name, on {text:?}");
    let _ = client.child.kill();
}

/// **Quick-fix.** A near-miss export in a `.vyrn` page offers the rename, and
/// the edit replaces exactly the misspelled name.
#[test]
fn m4_code_action_renames_a_near_miss() {
    let dir = m4_scratch("fix");
    let mut client = rfc33_client();
    let typo_uri = file_uri(&dir.join("routes/typo.vyrn"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        M4_APP,
    );
    did_open(&mut client, &typo_uri, "vyrn", M4_TYPO_PAGE);

    // 0-based line 2 = `export fn dta() -> Query<Int64> {`; `dta` at chars 10-13.
    let actions = code_actions(&mut client, &typo_uri, 2, 10, 13);
    assert_eq!(actions.len(), 1, "one quick-fix: {actions:?}");
    let a = &actions[0];
    let title = a.get("title").and_then(|t| t.as_str()).unwrap_or("");
    assert!(
        title.contains("Rename `dta` to `data`") && title.contains("contract `Page` (std/ui)"),
        "the title says what and why: {title}"
    );
    assert_eq!(a.get("kind").and_then(|k| k.as_str()), Some("quickfix"));
    let edits = a
        .pointer("/edit/changes")
        .and_then(|c| c.get(&typo_uri))
        .and_then(|e| e.as_array())
        .expect("an edit for this document");
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0].get("newText").and_then(|t| t.as_str()),
        Some("data")
    );
    assert_eq!(
        edits[0]
            .pointer("/range/start/line")
            .and_then(|l| l.as_i64()),
        Some(2)
    );
    assert_eq!(
        edits[0]
            .pointer("/range/start/character")
            .and_then(|c| c.as_i64()),
        Some(10)
    );
    assert_eq!(
        edits[0]
            .pointer("/range/end/character")
            .and_then(|c| c.as_i64()),
        Some(13)
    );

    // Away from the offending name, nothing is offered.
    assert!(code_actions(&mut client, &typo_uri, 5, 0, 0).is_empty());
    let _ = client.child.kill();
}

/// A `.vyrn` page behaves sanely: completion offers the members at module
/// scope, and hover does not claim a non-member is one.
#[test]
fn m4_a_vyrn_page_completes_without_misfiring() {
    let dir = m4_scratch("vyrnpage");
    let mut client = rfc33_client();
    let typo_uri = file_uri(&dir.join("routes/typo.vyrn"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        M4_APP,
    );
    did_open(&mut client, &typo_uri, "vyrn", M4_TYPO_PAGE);

    // 0-based line 1: the blank line after the import — module scope.
    let items = completion_items(&mut client, &typo_uri, 1, 0);
    let snippets: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("insertText").and_then(|t| t.as_str()))
        .collect();
    assert!(
        snippets.iter().any(|s| s.starts_with("export fn head()")),
        "head is offered in a .vyrn page too: {snippets:?}"
    );
    assert!(
        snippets.iter().any(|s| s.starts_with("export fn data()")),
        "and data — `dta` is not it: {snippets:?}"
    );
    // Hovering the misspelled export must NOT claim it is a contract member.
    let h = hover_value(&mut client, &typo_uri, 2, 11).unwrap_or_default();
    assert!(
        !h.contains("member of contract"),
        "`dta` is not a member: {h}"
    );
    let _ = client.child.kill();
}

/// A module in no role gets nothing — the whole feature is scoped by the role,
/// so a store or a helper module is untouched.
#[test]
fn m4_a_module_outside_every_role_is_untouched() {
    let dir = m4_scratch("outside");
    let store = dir.join("store.vyrn");
    let src = "fn helper() -> Int64 {\n    return 1\n}\n\n";
    std::fs::write(&store, src).unwrap();
    let mut client = rfc33_client();
    let uri = file_uri(&store);
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        M4_APP,
    );
    did_open(&mut client, &uri, "vyrn", src);

    let items = completion_items(&mut client, &uri, 3, 0);
    assert!(
        items.iter().all(|i| i.get("insertText").is_none()),
        "no contract snippets outside a role: {items:?}"
    );
    assert!(code_actions(&mut client, &uri, 0, 0, 10).is_empty());
    let _ = client.child.kill();
}

/// The `vyrn.json` `roles` key wins over discovery — the form RFC-0071
/// specifies and RFC-0072 inherits, proved by pointing a role at a directory
/// the generator call sites do NOT name.
#[test]
fn m4_manifest_roles_override_discovery() {
    let dir = m4_scratch("roles");
    std::fs::create_dir_all(dir.join("screens")).unwrap();
    std::fs::write(
        dir.join("vyrn.json"),
        "{ \"name\": \"m4\", \"main\": \"app.vyrn\",\n  \"roles\": { \"screens\": \"std/ui:Page\" } }\n",
    )
    .unwrap();
    let src = "import { Query } from \"std/ui\"\n\n";
    let screen = dir.join("screens/home.vyrn");
    std::fs::write(&screen, src).unwrap();

    let mut client = rfc33_client();
    let uri = file_uri(&screen);
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        M4_APP,
    );
    did_open(&mut client, &uri, "vyrn", src);
    let items = completion_items(&mut client, &uri, 1, 0);
    let snippets: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("insertText").and_then(|t| t.as_str()))
        .collect();
    assert!(
        snippets.iter().any(|s| s.starts_with("export fn data()")),
        "the declared role governs `screens/`: {snippets:?}"
    );

    // …and `routes/`, which discovery WOULD have found, is now outside every
    // declared role: an explicit `roles` key is the whole mapping.
    let page_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(&mut client, &page_uri, "vyx", M4_PAGE);
    let items = completion_items(&mut client, &page_uri, 5, 0);
    assert!(
        items.iter().all(|i| i.get("insertText").is_none()),
        "declared roles replace discovery entirely: {items:?}"
    );
    let _ = client.child.kill();
}

/// RFC-0071's headline claim, and the reason none of `Page`, `Component`,
/// `head` or `data` appears anywhere in the server: **a user-authored generator
/// with its own contract gets identical completion, hover, go-to-definition and
/// quick-fix with zero LSP changes.**
///
/// Nothing in this fixture is std. The contract is declared in the app's own
/// `gen.vyrn`, attached to `./screens` by the app's own generator call, and the
/// editor treats it exactly like `std/ui:Page` — because it never knew the
/// difference.
#[test]
fn m4_a_user_authored_contract_gets_the_same_editor_support() {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_m4_user_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("screens")).unwrap();

    let gen = "/// What a screen module may export.\n\
        export contract Screen {\n\
        \x20   /// The screen's title bar.\n\
        \x20   fn title() -> String = untitled()\n\
        \x20   /// The screen's pixel budget.\n\
        \x20   fn budget() -> Int64\n\
        }\n\
        \n\
        fn untitled() -> String {\n    return \"untitled\"\n}\n\
        \n\
        export gen fn screens(dir: String) -> String {\n\
        \x20   return \"export fn mounted() -> Int64 {\\n    return 1\\n}\\n\"\n\
        }\n";
    let app = "import { screens } from \"./gen\"\n\
        import { mounted } from screens(\"./screens\")\n\
        fn main() -> Int64 { return mounted() }\n";
    // A screen that wrote `titel` — one transposition from `title`.
    let screen = "export fn titel() -> String {\n    return \"home\"\n}\n\n";

    std::fs::write(
        dir.join("vyrn.json"),
        "{ \"name\": \"u\", \"main\": \"app.vyrn\" }\n",
    )
    .unwrap();
    std::fs::write(dir.join("gen.vyrn"), gen).unwrap();
    std::fs::write(dir.join("app.vyrn"), app).unwrap();
    std::fs::write(dir.join("screens/home.vyrn"), screen).unwrap();

    let mut client = rfc33_client();
    let uri = file_uri(&dir.join("screens/home.vyrn"));
    did_open(&mut client, &file_uri(&dir.join("app.vyrn")), "vyrn", app);
    did_open(&mut client, &uri, "vyrn", screen);

    // Completion: both members, required first, each a full declaration.
    let items = completion_items(&mut client, &uri, 3, 0);
    let snippets: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("insertText").and_then(|t| t.as_str()))
        .collect();
    assert!(
        snippets.contains(&"export fn budget() -> Int64 {\n    return $0\n}"),
        "the required member: {snippets:?}"
    );
    assert!(
        snippets.contains(&"export fn title() -> String {\n    return $0\n}"),
        "the optional member: {snippets:?}"
    );
    let budget = items
        .iter()
        .find(|i| i.get("label").and_then(|l| l.as_str()) == Some("budget"))
        .expect("budget");
    let title = items
        .iter()
        .find(|i| i.get("label").and_then(|l| l.as_str()) == Some("title"))
        .expect("title");
    assert!(
        budget.get("sortText").and_then(|s| s.as_str())
            < title.get("sortText").and_then(|s| s.as_str()),
        "required sorts before optional: {items:?}"
    );
    assert!(budget
        .get("detail")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .contains("contract `Screen` (./gen)"));

    // Quick-fix: the near-miss rename, on a contract the server has never heard
    // of.
    let actions = code_actions(&mut client, &uri, 0, 10, 15);
    assert_eq!(actions.len(), 1, "one quick-fix: {actions:?}");
    let title_text = actions[0]
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("");
    assert!(
        title_text.contains("Rename `titel` to `title`"),
        "the user's own did-you-mean: {title_text}"
    );

    // Hover + go-to-def on the member the screen DID get right, once renamed.
    let fixed = "export fn title() -> String {\n    return \"home\"\n}\n\n";
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [ { "text": fixed } ]
        }
    }));
    let h = hover_value(&mut client, &uri, 0, 11).expect("hover on `title`");
    assert!(
        h.contains("member of contract `Screen` (./gen)"),
        "names the user's contract:\n{h}"
    );
    assert!(
        h.contains("The screen's title bar."),
        "carries the user's /// doc:\n{h}"
    );

    let mut ids = Ids::new();
    let id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 11 }
        }
    }));
    let resp = client.read_response(&id);
    let loc = resp.get("result").expect("a definition");
    assert!(
        loc.get("uri")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .ends_with("gen.vyrn"),
        "jumps into the user's own module: {loc}"
    );
    // `fn title()` is on line 4 (1-based) of gen.vyrn → 0-based 3.
    assert_eq!(
        loc.pointer("/range/start/line").and_then(|l| l.as_i64()),
        Some(3),
        "{loc}"
    );

    let _ = client.child.kill();
}

/// The repo's OWN page, opened the way an editor opens it — no fixture, no
/// scratch directory, the real `examples/bin` with the real `vyrn.json` that has
/// no `roles` key at all.
///
/// `#[ignore]`d because opening a real `.vyx` triggers owner discovery, which
/// runs the themed pages+components generators over the whole app: correct, but
/// seconds rather than milliseconds, and the fixture tests above already cover
/// every assertion. Run it with `cargo test --release -- --ignored` when the
/// resolution chain changes.
#[test]
#[ignore]
fn m4_the_repos_own_page_gets_contract_completion() {
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let page = root.join("examples/bin/app/routes/index.vyx");
    let server = root.join("examples/bin/server.vyrn");
    let page_src = std::fs::read_to_string(&page).expect("the real page");
    let server_src = std::fs::read_to_string(&server).expect("the real server root");

    let mut client = rfc33_client();
    did_open(&mut client, &file_uri(&server), "vyrn", &server_src);
    did_open(&mut client, &file_uri(&page), "vyx", &page_src);
    let uri = file_uri(&page);

    // The blank line just before `fn isLoading` — module scope in the real page.
    let (line, _) = pos_after(&page_src, "fn isLoading");
    let items = completion_items(&mut client, &uri, line - 1, 0);
    let snippets: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("insertText").and_then(|t| t.as_str()))
        .collect();
    // `index.vyx` already exports `head` AND `data`, so neither is offered
    // again — the contract has nothing left to add, which is itself the right
    // answer and proves the resolution ran.
    assert!(
        snippets.is_empty(),
        "a page that satisfies its contract is offered nothing more: {snippets:?}"
    );

    // Hover on the real `export fn data()` names the real contract.
    let (dline, dcol) = pos_after(&page_src, "export fn data");
    let h = hover_value(&mut client, &uri, dline, dcol - 2).expect("hover on `data`");
    assert!(h.contains("member of contract `Page` (std/ui)"), "{h}");
    let _ = client.child.kill();
}

/// RFC-0072 M1: the audience rule reaches the EDITOR.
///
/// A rule you only discover at the shell is half a rule — the whole complaint
/// against Nuxt's split is that a server import reaching a component is found
/// later than it should be. The server carries the project's declared audience
/// vocabulary into its load options, so a widening import squiggles as you type
/// it, with the same message and the same note the compiler prints.
#[test]
fn a_widening_import_squiggles_in_the_editor() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-aud-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("server")).unwrap();
    std::fs::create_dir_all(dir.join("app")).unwrap();
    std::fs::write(
        dir.join("vyrn.json"),
        "{\n  \"name\": \"aud\",\n  \"main\": \"main.vyrn\",\n  \"audience\": { \"server\": [\"server\"], \"client\": [\"client\"], \"universal\": [\"app\"] }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("server/store.vyrn"),
        "export fn secret() -> Int64 {\n    return 7\n}\n",
    )
    .unwrap();
    let page = dir.join("app/view.vyrn");
    let text = "import { secret } from \"../server/store\"\n\nexport fn view() -> Int64 {\n    return secret()\n}\n";
    std::fs::write(&page, text).unwrap();
    let uri = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));

    let mut client = LspClient::spawn().expect("spawn vyrn-lsp");
    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": init_id, "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let _ = client.read_response(&init_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": {
            "uri": uri.clone(), "languageId": "vyrn", "version": 1, "text": text
        } }
    }));
    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif["params"]["diagnostics"].as_array().unwrap();
    let msg = diags
        .iter()
        .filter_map(|d| d["message"].as_str())
        .find(|m| m.contains("cannot import"))
        .unwrap_or_else(|| panic!("expected an audience diagnostic, got {diags:?}"));
    assert!(msg.contains("app/view.vyrn` is universal"), "{msg}");
    assert!(
        msg.contains("`server/store.vyrn`, which is server-only"),
        "{msg}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// A `///` above a method inside a `protocol` body reaches hover — on the
// signature itself and on a call through an `impl` (whose body repeats no
// prose). `std/http`'s `Policy` is the module this was found on.
// ===========================================================================

const PROTOCOL_DOC_SRC: &str = "type Route = { ok: Bool }\n\
protocol Policy {\n\
    /// `Cache-Control: max-age=N` on a successful answer.\n\
    fn cacheFor(self, seconds: Int64) -> Route\n\
}\n\
impl Policy for Route {\n\
    fn cacheFor(self, seconds: Int64) -> Route { return self }\n\
}\n\
fn main() -> Int64 {\n\
    let r = Route { ok: true }\n\
    let out = r.cacheFor(60)\n\
    return 0\n\
}\n";

#[test]
fn hover_shows_a_protocol_methods_doc_comment() {
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_protodoc_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("p.vyrn"), PROTOCOL_DOC_SRC).unwrap();
    let uri = file_uri(&dir.join("p.vyrn"));
    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", PROTOCOL_DOC_SRC);

    // The signature inside the protocol body.
    let (l, c) = pos_after(PROTOCOL_DOC_SRC, "fn cacheFor");
    let hover = hover_value(&mut client, &uri, l, c - 1).expect("hover on the signature");
    assert!(
        hover.contains("max-age=N"),
        "signature hover carries the doc: {hover}"
    );

    // The call site, which resolves through the impl.
    let (l, c) = pos_after(PROTOCOL_DOC_SRC, "r.cacheFor");
    let hover = hover_value(&mut client, &uri, l, c - 1).expect("hover on the call");
    assert!(
        hover.contains("max-age=N"),
        "call-site hover carries the doc: {hover}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// RFC-0073 M3 — the LSP reads the symbol map a generator baked in.
//
// A generated stub has no source file and no `///` of its own, so before this a
// `client()` export hovered as its own signature and go-to-definition returned
// NOTHING: the "file" a generated declaration carries is a banner, not a path.
// The map supplies both halves, and the derived route is the fact the buffer
// never states.
// ===========================================================================

const M3_WIRE: &str = "/// One note, as it crosses the wire.
export type Note = { text: String }
export type NoteReq = { id: Int64 }
";

const M3_API: &str = "import { Note, NoteReq } from \"../../shared/wire\"

/// Fetch one note by id. The doc a generated stub has to borrow.
export fn byId(req: NoteReq) -> Note {
    return Note { text: req.id.toString() }
}
";

const M3_BOOT: &str = "import { client } from \"std/rpc\"
import * as api from client(\"./server/api\")

fn onNote(r: api.RpcReply<api.Note>) {
}

fn main() -> Int64 {
    api.notesById(api.NoteReq { id: 1 }, onNote)
    return 0
}
";

/// A scratch full-stack project: a wire module, one api procedure, and a client
/// root that generates stubs over the api directory.
fn m3_scratch() -> std::path::PathBuf {
    let n = RFC33_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_m3_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("server/api")).unwrap();
    std::fs::create_dir_all(dir.join("shared")).unwrap();
    std::fs::write(
        dir.join("vyrn.json"),
        "{ \"name\": \"m3\", \"client\": \"boot.vyrn\" }
",
    )
    .unwrap();
    std::fs::write(dir.join("shared/wire.vyrn"), M3_WIRE).unwrap();
    std::fs::write(dir.join("server/api/notes.vyrn"), M3_API).unwrap();
    std::fs::write(dir.join("boot.vyrn"), M3_BOOT).unwrap();
    dir
}

#[test]
fn a_generated_stub_borrows_its_declarations_doc_and_shows_its_derived_route() {
    let dir = m3_scratch();
    let uri = file_uri(&dir.join("boot.vyrn"));
    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", M3_BOOT);

    let (l, c) = pos_after(M3_BOOT, "api.notesById");
    let hover = hover_value(&mut client, &uri, l, c - 1).expect("hover on the generated stub");
    // The stub's own signature — what you actually call, callback and all.
    assert!(hover.contains("fn notesById(req: NoteReq"), "{hover}");
    // The DECLARATION's doc, which the stub does not carry.
    assert!(
        hover.contains("Fetch one note by id"),
        "the origin's doc is borrowed: {hover}"
    );
    // The derived wire fact, and where the declaration is written.
    assert!(
        hover.contains("`POST /_/notes/byId` · convention"),
        "{hover}"
    );
    assert!(hover.contains("generated from `byId` in"), "{hover}");
    assert!(hover.contains("server/api/notes.vyrn:4"), "{hover}");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = client.child.kill();
}

#[test]
fn go_to_definition_on_a_generated_stub_lands_on_the_declaration() {
    let dir = m3_scratch();
    let uri = file_uri(&dir.join("boot.vyrn"));
    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", M3_BOOT);

    let (l, c) = pos_after(M3_BOOT, "api.notesById");
    let id = serde_json::json!("m3def");
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/definition",
        "params": { "textDocument": { "uri": uri }, "position": { "line": l, "character": c - 1 } }
    }));
    let resp = client.read_response(&id);
    let loc = resp
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let target = loc
        .pointer("/uri")
        .and_then(|u| u.as_str())
        .unwrap_or_default();
    assert!(
        target.ends_with("server/api/notes.vyrn"),
        "lands on the api module: {loc}"
    );
    // `export fn byId` is line 4 (1-based) → 0-based 3, and `byId` starts at
    // column 11 (1-based) → 0-based 10. The map is the only thing that knows
    // the column; the AST carries a line and nothing else.
    assert_eq!(
        loc.pointer("/range/start/line").and_then(|l| l.as_i64()),
        Some(3),
        "{loc}"
    );
    assert_eq!(
        loc.pointer("/range/start/character")
            .and_then(|c| c.as_i64()),
        Some(10),
        "{loc}"
    );

    // A re-emitted TYPE has lost its file in the generated source; the map is
    // the only place that still says it came from `shared/wire.vyrn`.
    let (tl, tc) = pos_after(M3_BOOT, "api.NoteReq {");
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "m3ty", "method": "textDocument/definition",
        "params": { "textDocument": { "uri": uri }, "position": { "line": tl, "character": tc - 3 } }
    }));
    let resp = client.read_response(&serde_json::json!("m3ty"));
    let t = resp
        .pointer("/result/uri")
        .and_then(|u| u.as_str())
        .unwrap_or_default();
    assert!(
        t.ends_with("shared/wire.vyrn"),
        "a re-emitted type jumps home: {resp}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = client.child.kill();
}

#[test]
fn a_procedure_declaration_hovers_with_the_route_it_is_mounted_at() {
    let dir = m3_scratch();
    let api_uri = file_uri(&dir.join("server/api/notes.vyrn"));
    let mut client = rfc33_client();
    // Only the api module is open. It reaches no generator, so the facts can
    // come only from the ranked probe over the roots that mount a surface.
    did_open(&mut client, &api_uri, "vyrn", M3_API);

    let (l, c) = pos_after(M3_API, "export fn byId");
    let hover = hover_value(&mut client, &api_uri, l, c - 1).expect("hover on the declaration");
    assert!(hover.contains("fn byId(req: NoteReq) -> Note"), "{hover}");
    assert!(hover.contains("Fetch one note by id"), "{hover}");
    assert!(
        hover.contains("`POST /_/notes/byId` · convention"),
        "the derived route: {hover}"
    );

    // The lens the extension renders reads the same facts.
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "m3lens", "method": "vyrn/routeLenses",
        "params": { "textDocument": { "uri": api_uri } }
    }));
    let resp = client.read_response(&serde_json::json!("m3lens"));
    let lenses = resp["result"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        lenses.len(),
        1,
        "one lens per DECLARATION, not per generated symbol: {lenses:?}"
    );
    assert_eq!(lenses[0]["line"].as_i64(), Some(3), "{lenses:?}");
    assert_eq!(
        lenses[0]["title"].as_str(),
        Some("POST /_/notes/byId · convention")
    );

    // A module nothing mounts gets no lenses and no note — and is not an error.
    let wire_uri = file_uri(&dir.join("shared/wire.vyrn"));
    did_open(&mut client, &wire_uri, "vyrn", M3_WIRE);
    let (wl, wc) = pos_after(M3_WIRE, "export type Note ");
    let h = hover_value(&mut client, &wire_uri, wl, wc - 6).unwrap_or_default();
    assert!(
        !h.contains("POST"),
        "a wire type is mounted at nothing: {h}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = client.child.kill();
}

// ===========================================================================
// RFC-0073 M4 — rename across the generated boundary.
//
// `byId` in the api module is `api.notesById` in the client, and the two
// spellings share no substring the editor could match on. The symbol map is the
// only thing that relates them, so this is the milestone the RFC was written
// for: one cursor, one new name, and every call site through the generated
// module follows.
//
// The edit spans SOURCES only. The generated module is a build artifact and is
// never in the result — it is regenerated from the renamed declaration.
// ===========================================================================

/// `textDocument/rename` at `(line, ch)`, as `(edits by file suffix, error)`.
fn rename_at(
    client: &mut LspClient,
    uri: &str,
    line: u32,
    ch: u32,
    new_name: &str,
) -> (Vec<(String, Vec<String>)>, Option<String>) {
    let id = serde_json::json!(format!("rn{line}_{ch}"));
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/rename",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": ch },
            "newName": new_name
        }
    }));
    let resp = client.read_response(&id);
    if let Some(msg) = resp.pointer("/error/message").and_then(|m| m.as_str()) {
        return (Vec::new(), Some(msg.to_string()));
    }
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(changes) = resp.pointer("/result/changes").and_then(|c| c.as_object()) {
        for (file, edits) in changes {
            let spans: Vec<String> = edits
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|e| {
                    format!(
                        "{}:{}={}",
                        e.pointer("/range/start/line")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(-1),
                        e.pointer("/range/start/character")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(-1),
                        e.get("newText").and_then(|v| v.as_str()).unwrap_or("")
                    )
                })
                .collect();
            out.push((file.clone(), spans));
        }
    }
    out.sort();
    (out, None)
}

/// The edits for the file whose URI ends with `suffix`.
fn edits_for<'a>(changes: &'a [(String, Vec<String>)], suffix: &str) -> &'a [String] {
    changes
        .iter()
        .find(|(f, _)| f.ends_with(suffix))
        .map(|(_, e)| e.as_slice())
        .unwrap_or_else(|| panic!("no edits for {suffix} in {changes:#?}"))
}

#[test]
fn renaming_a_procedure_follows_the_symbol_map_into_the_generated_call_site() {
    let dir = m3_scratch();
    let api_uri = file_uri(&dir.join("server/api/notes.vyrn"));
    let mut client = rfc33_client();
    did_open(&mut client, &api_uri, "vyrn", M3_API);

    let (l, c) = pos_after(M3_API, "export fn byId");

    // The pre-flight seeds the box with the current name and the exact span.
    let id = serde_json::json!("m4prep");
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/prepareRename",
        "params": { "textDocument": { "uri": api_uri }, "position": { "line": l, "character": c - 1 } }
    }));
    let prep = client.read_response(&id);
    assert_eq!(
        prep.pointer("/result/placeholder").and_then(|p| p.as_str()),
        Some("byId"),
        "{prep}"
    );
    assert_eq!(
        prep.pointer("/result/range/start/line")
            .and_then(|v| v.as_i64()),
        Some(3),
        "{prep}"
    );
    assert_eq!(
        prep.pointer("/result/range/start/character")
            .and_then(|v| v.as_i64()),
        Some(10),
        "{prep}"
    );

    let (changes, err) = rename_at(&mut client, &api_uri, l, c - 1, "fetch");
    assert!(err.is_none(), "{err:?}");

    // The declaration itself.
    assert_eq!(edits_for(&changes, "server/api/notes.vyrn"), ["3:10=fetch"]);
    // And the call site, under the name the GENERATOR derives — which shares no
    // substring with `byId` that a textual rename could have found.
    assert_eq!(edits_for(&changes, "boot.vyrn"), ["7:8=notesFetch"]);
    // The generated module is not a file, and is not edited.
    assert_eq!(changes.len(), 2, "sources only: {changes:#?}");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = client.child.kill();
}

#[test]
fn renaming_a_wire_type_follows_both_the_direct_import_and_the_re_emitted_copy() {
    let dir = m3_scratch();
    let wire_uri = file_uri(&dir.join("shared/wire.vyrn"));
    let mut client = rfc33_client();
    did_open(&mut client, &wire_uri, "vyrn", M3_WIRE);

    let (l, c) = pos_after(M3_WIRE, "export type Note");
    let (changes, err) = rename_at(&mut client, &wire_uri, l, c - 1, "Memo");
    assert!(err.is_none(), "{err:?}");

    // Its own declaration.
    assert_eq!(edits_for(&changes, "shared/wire.vyrn"), ["1:12=Memo"]);
    // The api module imports it DIRECTLY — an ordinary cross-file reference,
    // found because that module's import resolves to this file. `NoteReq` is a
    // different token and does not move.
    assert_eq!(
        edits_for(&changes, "server/api/notes.vyrn"),
        ["0:9=Memo", "3:32=Memo", "4:11=Memo"]
    );
    // The client never imports it: `client()` re-emits the declaration, and the
    // map is the only record that the copy came from here.
    assert_eq!(edits_for(&changes, "boot.vyrn"), ["3:30=Memo"]);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = client.child.kill();
}

#[test]
fn rename_refuses_what_it_cannot_carry_through_rather_than_doing_half_of_it() {
    let dir = m3_scratch();
    let api_uri = file_uri(&dir.join("server/api/notes.vyrn"));
    let boot_uri = file_uri(&dir.join("boot.vyrn"));
    let mut client = rfc33_client();
    did_open(&mut client, &api_uri, "vyrn", M3_API);
    did_open(&mut client, &boot_uri, "vyrn", M3_BOOT);

    // A new name that is not an identifier would not fail to compile, it would
    // fail to LEX, in every file the rename touched.
    let (l, c) = pos_after(M3_API, "export fn byId");
    let (_, err) = rename_at(&mut client, &api_uri, l, c - 1, "by Id");
    assert!(err.unwrap_or_default().contains("not a valid identifier"));

    // A cursor on a local binding: nothing crosses a file boundary, and there is
    // nothing here this milestone knows how to rename.
    let (bl, bc) = pos_after(M3_API, "Note { text: req");
    let (_, err) = rename_at(&mut client, &api_uri, bl, bc - 1, "x");
    assert!(err.unwrap_or_default().contains("no declaration here"));

    // A cursor on an IMPORTED name: renameable, but not from here.
    let (il, ic) = pos_after(M3_API, "    return Note");
    let (_, err) = rename_at(&mut client, &api_uri, il, ic - 1, "x");
    assert!(err
        .unwrap_or_default()
        .contains("not declared in this file"));

    // A GENERATED name at its call site: the thing to rename is the declaration
    // it stands for, and the message says so instead of silently doing nothing.
    let (gl, gc) = pos_after(M3_BOOT, "api.notesById");
    let (_, err) = rename_at(&mut client, &boot_uri, gl, gc - 1, "x");
    assert!(
        err.is_some(),
        "a generated symbol is not renameable in place"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = client.child.kill();
}

/// The RFC's acceptance line, against the corpus rather than a fixture: renaming
/// a procedure in `examples/bin` updates every call site across `app/`,
/// `client/` and the REST projection in one edit.
///
/// `create` is mapped by three generators living in three different roots —
/// `client()` exports it as `pastesCreate`, `rpc()` dispatches to
/// `rpcHandlePastesCreate`, `http()` re-exports it as `create` — and `create`
/// appears in none of the calling files under a name a textual rename could have
/// found. Asserted by the new NAMES and the files they land in rather than by
/// line numbers, so editing the example does not falsify the property.
#[test]
fn renaming_a_procedure_in_the_corpus_reaches_every_generated_call_site() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/bin");
    let api = root.join("server/api/pastes.vyrn");
    let text = std::fs::read_to_string(&api).expect("examples/bin/server/api/pastes.vyrn");
    let uri = file_uri(&api);
    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", &text);

    let (l, c) = pos_after(&text, "export mut fn create");
    let (changes, err) = rename_at(&mut client, &uri, l, c - 1, "add");
    assert!(err.is_none(), "{err:?}");

    // The declaration.
    assert_eq!(edits_for(&changes, "server/api/pastes.vyrn"), ["27:14=add"]);
    // The browser client, through `client()`'s stub — the one the RFC is about.
    let boot: Vec<&str> = edits_for(&changes, "client/boot.vyrn")
        .iter()
        .map(|s| s.as_str())
        .collect();
    assert_eq!(boot.len(), 1, "{boot:?}");
    assert!(
        boot[0].ends_with("=pastesAdd"),
        "the derived stub name moves too: {boot:?}"
    );
    // The REST projection, through `http()`'s same-named re-export: the import
    // list AND the `POST(create("/"))` that names it.
    let http: Vec<&str> = edits_for(&changes, "pastes.http.vyrn")
        .iter()
        .map(|s| s.as_str())
        .collect();
    assert_eq!(http.len(), 2, "{http:?}");
    assert!(http.iter().all(|e| e.ends_with("=add")), "{http:?}");
    // Sources only: the generated modules are build artifacts.
    assert_eq!(changes.len(), 3, "{changes:#?}");

    let _ = client.child.kill();
}

/// The same rename seen from a `.vyx`: a page imports `recent` DIRECTLY, and its
/// reference lives in a `<script>` body whose lines are offset from the file's.
#[test]
fn renaming_a_procedure_in_the_corpus_reaches_a_pages_script_body() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/bin");
    let api = root.join("server/api/pastes.vyrn");
    let text = std::fs::read_to_string(&api).expect("examples/bin/server/api/pastes.vyrn");
    let uri = file_uri(&api);
    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", &text);

    let (l, c) = pos_after(&text, "export fn recent");
    let (changes, err) = rename_at(&mut client, &uri, l, c - 1, "latest");
    assert!(err.is_none(), "{err:?}");

    let page: Vec<&str> = edits_for(&changes, "app/routes/index.vyx")
        .iter()
        .map(|s| s.as_str())
        .collect();
    // The import and the one call. `recentRows` is a different token and the
    // four prose mentions of "recent" in the comments are not tokens at all.
    assert_eq!(page.len(), 2, "{page:?}");
    assert!(page.iter().all(|e| e.ends_with("=latest")), "{page:?}");
    // The `.vyx` line numbers are the FILE's, not the script body's: the import
    // is on file line 15 (0-based 14).
    assert!(
        page[0].starts_with("14:"),
        "script-body lines map back to the file: {page:?}"
    );

    let _ = client.child.kill();
}

// ===========================================================================
// RFC-0091 M1/M3 — the container protocols in the editor.
//
// A `Copy` impl is an ordinary impl method, so it was already indexed. The two
// new shapes are a `place nth` — which is not a function and never reaches
// `Program::functions` — and a loop variable whose type comes from it. The
// buffer must check clean and the loop variable must hover as its element type.
// ===========================================================================

const CONTAINER_SRC: &str = "type Ring = { data: Array<Int64> }

impl Iterate for Ring {
    fn size(self) -> Int64 {
        return self.data.length
    }
    place nth(read self, i: Int64) -> Int64 {
        yield self.data[i]
    }
}

impl Copy for Ring {
    fn copy(self) -> Ring {
        return Ring { data: [] }
    }
}

fn main() -> Int64 {
    let r = Ring { data: [] }
    let q = r.copy()
    let mut t = 0
    for x in q {
        t = t + x
    }
    return t
}
";

#[test]
fn a_user_container_checks_and_hovers_in_the_editor() {
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_container_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("c.vyrn"), CONTAINER_SRC).unwrap();
    let uri = file_uri(&dir.join("c.vyrn"));
    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", CONTAINER_SRC);

    // The buffer is clean: a `place` member and an `Iterate` loop are not
    // errors, and neither is a declared `copy`.
    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif["params"]["diagnostics"].as_array().unwrap();
    assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");

    // The loop variable takes what `place nth` yields.
    let (l, c) = pos_after(CONTAINER_SRC, "for x");
    let hover = hover_value(&mut client, &uri, l, c - 1).expect("hover on the loop variable");
    assert!(
        hover.contains("Int64"),
        "the loop variable hovers as its element type: {hover}"
    );

    // The declared `copy` gives the binding the receiver's type.
    let (l, c) = pos_after(CONTAINER_SRC, "let q");
    let hover = hover_value(&mut client, &uri, l, c - 1).expect("hover on the copy");
    assert!(
        hover.contains("Ring"),
        "a declared copy hovers as its type: {hover}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0087 U1 over the wire: the memory model in the editor.
///
/// One document, three surfaces, one table behind them. The point of driving it
/// through the real server rather than the frontend API is the wire shapes: the
/// modifier bit has to match the legend the server advertised, and an inlay hint
/// has to arrive at a 0-based position the client can draw at.
#[test]
fn the_memory_model_reaches_hover_tokens_and_inlay_hints() {
    let mut client = LspClient::spawn().expect("spawn vyrn-lsp");
    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": init_id,
        "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let init = client.read_response(&init_id);
    let caps = init.pointer("/result/capabilities").expect("capabilities");
    assert!(
        caps.get("inlayHintProvider").is_some(),
        "inlay hints advertised: {caps}"
    );
    // The legend the modifier bit is an index into. `modification` must be bit 3.
    let mods = caps
        .pointer("/semanticTokensProvider/legend/tokenModifiers")
        .and_then(|m| m.as_array())
        .expect("the token legend");
    assert_eq!(mods[3].as_str(), Some("modification"), "legend: {mods:?}");
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    let uri = "file:///memory/model.vyrn";
    let src = "\
fn take(s: consume String) -> Int64 { return 1 }
fn main() -> Int64 {
    let a = \"x\" + \"y\"
    let n = take(a)
    let b = \"p\" + \"q\"
    print(b)
    return n
}
";
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": { "uri": uri, "languageId": "vyrn", "version": 1, "text": src }
        }
    }));
    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif
        .pointer("/params/diagnostics")
        .and_then(|d| d.as_array())
        .unwrap();
    assert!(diags.is_empty(), "clean source: {diags:?}");

    let mut ids = Ids::new();

    // 1. Hover on `a` in `let a = ..` (LSP line 2, char 8).
    let id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/hover",
        "params": { "textDocument": { "uri": uri }, "position": { "line": 2, "character": 8 } }
    }));
    let hover = client.read_response(&id);
    let text = hover
        .pointer("/result/contents/value")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(
        text.contains("memory: moved at line 4 into `take(..)`"),
        "hover: {text}"
    );

    // 2. Hover on `b`, which lives to block exit — the other answer.
    let id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/hover",
        "params": { "textDocument": { "uri": uri }, "position": { "line": 4, "character": 8 } }
    }));
    let hover = client.read_response(&id);
    let text = hover
        .pointer("/result/contents/value")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(
        text.contains("memory: reclaimed at block exit"),
        "hover: {text}"
    );

    // 3. The move gets an inlay hint at the end of `a` inside `take(a)`.
    let id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/inlayHint",
        "params": {
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 20, "character": 0 }
            }
        }
    }));
    let hints = client.read_response(&id);
    let list = hints
        .get("result")
        .and_then(|r| r.as_array())
        .expect("hints: {hints}");
    assert_eq!(list.len(), 1, "one move, one hint: {list:?}");
    assert_eq!(list[0].pointer("/position/line").unwrap().as_i64(), Some(3));
    assert_eq!(list[0].get("label").unwrap().as_str(), Some("→ take(..)"));

    // 4. The last use carries the modifier. Tokens are delta-encoded
    //    `[Δline, Δstart, len, type, mods]`; bit 3 is `modification`.
    let id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/semanticTokens/full",
        "params": { "textDocument": { "uri": uri } }
    }));
    let toks = client.read_response(&id);
    let data: Vec<i64> = toks
        .pointer("/result/data")
        .and_then(|d| d.as_array())
        .expect("token data")
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    let mut line = 0i64;
    let mut marked = Vec::new();
    for q in data.chunks(5) {
        line += q[0];
        if q[4] & (1 << 3) != 0 {
            marked.push(line);
        }
    }
    // Exactly the `a` handed to `take`, on LSP line 3. The declaration is never
    // a last use, and `b` has no such point.
    assert_eq!(marked, vec![3], "marked last uses: {marked:?}");
}

/// A container that exports `remove` gets the method form, in the editor as
/// well as in the compiler. The parser bound `.remove(..)` to the `Map` builtin
/// before any type was known, so `people.remove(h)` was refused where `people`
/// is the container and `remove` is its own export.
///
/// The claim here is that the NAME resolves to the declaration: go-to-definition
/// on the method-form call lands in the container module, not on a builtin, and
/// the buffer has no diagnostic. A `Map` receiver in the same server still
/// completes `remove` as the builtin, which is the other half of "the right one".
#[test]
fn a_containers_own_remove_takes_the_method_form() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-shadow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("bag.vyrn"),
        "export type Bag = { items: Array<Int64> }\n\
         export fn add(b: modify Bag, v: Int64) -> Int64 { b.items.push(v)\n return b.items.length }\n\
         export fn remove(b: modify Bag, i: Int64) -> Bool { let x = b.items.swapRemove(i)\n return true }\n",
    )
    .unwrap();
    let app = "\
import { Bag, add, remove } from \"./bag\"
fn main() -> Int64 {
    let mut b = Bag { items: [] }
    let n = b.add(7)
    let gone = b.remove(0)
    return n
}
";
    let app_path = dir.join("app.vyrn");
    std::fs::write(&app_path, app).unwrap();
    let uri = file_uri(&app_path);

    let mut client = rfc33_client();
    did_open(&mut client, &uri, "vyrn", app);
    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif["params"]["diagnostics"].as_array().unwrap();
    assert!(
        diags.is_empty(),
        "method-form `remove` on a container: {diags:?}"
    );

    // `b.remove(0)` resolves to the container's declaration, in its own file.
    let (l, c) = at(app, 5, "b.remove");
    let target = definition_target(&mut client, &uri, l, c + 2).expect("definition on b.remove");
    assert!(
        target.ends_with("/bag.vyrn"),
        "b.remove → bag.vyrn: {target}"
    );

    // A module that does NOT claim the name keeps the builtin: `m.remove(k)`
    // checks, and completion after `m.` still offers it.
    let maps = "\
fn main() -> Int64 {
    let mut m: Map<String, Int64> = [:]
    let had = m.remove(\"k\")
    return m.length
}
";
    let maps_path = dir.join("maps.vyrn");
    std::fs::write(&maps_path, maps).unwrap();
    let maps_uri = file_uri(&maps_path);
    did_open(&mut client, &maps_uri, "vyrn", maps);
    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif["params"]["diagnostics"].as_array().unwrap();
    assert!(diags.is_empty(), "builtin `remove` on a Map: {diags:?}");
    let (l, c) = at(maps, 3, "m.remove");
    let labels = completion_labels(&mut client, &maps_uri, l, c + 2);
    assert!(
        labels.contains(&"remove".to_string()),
        "map completes `remove`: {labels:?}"
    );

    // The trade-off, pinned: scope answers, not the receiver's type, so one
    // module cannot mean both. It is refused at the call and says which type it
    // wanted — a compile error, never two meanings for one name.
    let clash = "\
import { Bag, remove } from \"./bag\"
fn main() -> Int64 {
    let mut m: Map<String, Int64> = [:]
    let had = m.remove(\"k\")
    return 0
}
";
    let clash_path = dir.join("clash.vyrn");
    std::fs::write(&clash_path, clash).unwrap();
    let clash_uri = file_uri(&clash_path);
    did_open(&mut client, &clash_uri, "vyrn", clash);
    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif["params"]["diagnostics"].as_array().unwrap();
    let msg = diags
        .first()
        .and_then(|d| d["message"].as_str())
        .unwrap_or("");
    assert!(
        msg.contains("expects Bag"),
        "the clash names the type it wanted: {diags:?}"
    );

    let _ = client.child.kill();
    let _ = std::fs::remove_dir_all(&dir);
}
