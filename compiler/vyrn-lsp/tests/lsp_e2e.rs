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
        // `CARGO_BIN_EXE_vyrn-lsp` points at the built server binary (the
        // `[[bin]] name = "vyrn-lsp"` in Cargo.toml).
        let bin = env!("CARGO_BIN_EXE_vyrn-lsp");
        let child = Command::new(bin)
            // Disable the shared generator cache so RFC-0033 fixtures never hit a
            // stale synthesized module from another run.
            .env("VYRN_NO_GEN_CACHE", "1")
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
    let caps =
        init_resp.get("result").and_then(|r| r.get("capabilities")).expect("capabilities present");
    assert!(caps.get("hoverProvider").is_some(), "hover advertised");
    assert!(caps.get("definitionProvider").is_some(), "definition advertised");
    assert!(caps.get("completionProvider").is_some(), "completion advertised");
    assert!(caps.get("documentSymbolProvider").is_some(), "document symbols advertised");

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
    fn new() -> Self { Ids(1) }
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
    for injected in ["Value", "IntVal", "StrVal", "BoolVal"] {
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
        let name = it.get("name").and_then(|n| n.as_str()).expect("symbol name");
        let line = it.pointer("/range/start/line").and_then(|l| l.as_i64()).expect("range line");
        let kind = it.get("kind").and_then(|k| k.as_i64()).expect("symbol kind");
        by_name.insert(name, (line, kind));
    }

    for expected in ["Shape", "Circle", "Rect", "Unit", "area", "main"] {
        assert!(by_name.contains_key(expected), "documentSymbol missing {expected}: {by_name:?}");
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
    let labels: Vec<&str> =
        items.iter().filter_map(|i| i.get("label").and_then(|l| l.as_str())).collect();
    for expected in ["push", "at", "alen", "afree", "length"] {
        assert!(labels.contains(&expected), "array member {expected} missing: {labels:?}");
    }
    // In a `.foo` context the top-level symbols must NOT be offered.
    assert!(!labels.contains(&"main"), "top-level `main` leaked into member completion: {labels:?}");
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
    let labels: Vec<&str> =
        items.iter().filter_map(|i| i.get("label").and_then(|l| l.as_str())).collect();
    assert!(labels.contains(&"nav.home.label"), "missing key: {labels:?}");
    assert!(labels.contains(&"nav.about.label"), "missing key: {labels:?}");
    // The top-level symbols must not leak into the string-literal context.
    assert!(!labels.contains(&"main"), "top-level `main` leaked: {labels:?}");
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
    let root_text = "import { double } from \"./lib\"\n\nfn main() -> Int64 {\n    return double(21)\n}\n";
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
    assert!(diags.is_empty(), "import resolved via loader, expected no diagnostics: {diags:?}");

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
    assert!(target.ends_with("lib.vyrn"), "definition jumps into the imported file: {target}");
    assert_eq!(loc["range"]["start"]["line"], 0, "lands on `export fn double`");

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
    let hover = resp["result"]["contents"]["value"].as_str().expect("hover has content");
    assert!(hover.contains("double"), "hover shows the imported signature: {hover}");

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
    assert!(!diags.is_empty(), "unresolvable import must produce a diagnostic");

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
    let edits = resp.get("result").and_then(|r| r.as_array()).expect("formatting returns edits");
    assert_eq!(edits.len(), 1, "one whole-document edit: {resp}");
    let new_text = edits[0].get("newText").and_then(|t| t.as_str()).expect("newText");
    assert_eq!(
        new_text,
        "fn main() -> Int64 {\n    let x = 1 + 2 * 3\n    return x\n}\n",
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
    assert!(bad_resp.get("error").is_none(), "no error for an unlexable buffer: {bad_resp}");
    assert!(bad_resp.get("result").is_some(), "`result` key present (not skipped): {bad_resp}");
    assert!(bad_resp["result"].is_null(), "unlexable buffer formats to null (no edit)");
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
    if s.starts_with('/') { format!("file://{s}") } else { format!("file:///{s}") }
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
        if msg.json.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics") {
            let uri = msg.json.pointer("/params/uri").and_then(|u| u.as_str()).unwrap_or("");
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
    did_open(&mut client, &file_uri(&dir.join("app.vyrn")), "vyrn", RFC33_APP);

    let note = read_diags_for(&mut client, "Widget.vyx");
    let diags = note.pointer("/params/diagnostics").and_then(|d| d.as_array()).expect("diags array");
    assert!(!diags.is_empty(), "the .vyx buffer carries the remapped error: {note}");
    let d0 = &diags[0];
    let msg = d0.get("message").and_then(|m| m.as_str()).unwrap_or("");
    assert!(msg.contains("titel"), "carries the checker message: {msg}");
    // `.vyx` line 6 (0-based 5), column 8 (0-based 7) — where `item` starts
    // inside `{{ item.titel }}` (RFC-0039 v2).
    assert_eq!(d0.pointer("/range/start/line").and_then(|l| l.as_i64()), Some(5), "line: {note}");
    assert_eq!(d0.pointer("/range/start/character").and_then(|c| c.as_i64()), Some(7), "col: {note}");
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
    let value = resp.pointer("/result/contents/value").and_then(|v| v.as_str())
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
    let items = resp.get("result").and_then(|r| r.as_array()).expect("completion list");
    let labels: Vec<&str> = items.iter().filter_map(|i| i.get("label").and_then(|l| l.as_str())).collect();
    assert!(labels.contains(&"title"), "offers the record field `title`: {labels:?}");
}

// ===========================================================================
// RFC-0042 — template editor intelligence: class / attribute / component /
// TransKey completion inside `.vyx` (and `theme.cls("…")` in `.vyrn`).
// ===========================================================================

/// 0-based (line, char) just past the first occurrence of `needle` in `text`.
fn pos_after(text: &str, needle: &str) -> (u32, u32) {
    let idx =
        text.find(needle).unwrap_or_else(|| panic!("needle {needle:?} not found")) + needle.len();
    let pre = &text[..idx];
    let line = pre.matches('\n').count() as u32;
    let col = (pre.len() - pre.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32;
    (line, col)
}

/// A tiny theme: two brand shades, two spacings, an `md` breakpoint (so `md:`
/// variants exist), and a `book-card` safelist entry.
const RFC42_THEME: &str = "{ \"colors\": { \"brand\": { \"500\": \"#4f46e5\", \"600\": \"#4338ca\" } },\n\
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
    let items = resp.get("result").and_then(|r| r.as_array()).cloned().unwrap_or_default();
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
    resp.pointer("/result/contents/value").and_then(|v| v.as_str()).map(String::from)
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
    assert!(labels.contains(&"p-4".to_string()), "utility p-4 offered: {labels:?}");
    assert!(labels.contains(&"px-2".to_string()), "utility px-2 offered: {labels:?}");
    // Top-level symbols must NOT leak into a class value.
    assert!(!labels.contains(&"widget".to_string()), "no top-level leak: {labels:?}");

    // Safelisted name: `class="book-card"` — cursor after `book`.
    let (l, c) = pos_after(RFC42_VYX, "\"book");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(labels.contains(&"book-card".to_string()), "safelisted offered: {labels:?}");

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
    assert!(v.contains("background-color:#4f46e5"), "utility CSS rule: {v}");

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
    assert!(labels.contains(&"home".to_string()), "TransKey home: {labels:?}");
    assert!(labels.contains(&"about".to_string()), "TransKey about: {labels:?}");
}

// --- Phase B/C: structural completion on the raw `.vyx` (no owner needed) ----

const RFC42_STRUCT_PANEL: &str = "<template>\n\
    <Book></Book>\n\
    <BookCard t></BookCard>\n\
    <a v-i></a>\n\
    <button @cl></button>\n\
    </template>\n";

const RFC42_STRUCT_CARD: &str =
    "<script>\nprops { title: String, url: String }\n</script>\n\
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
    assert!(labels.contains(&"BookCard".to_string()), "sibling component offered: {labels:?}");
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
    assert!(labels.contains(&"title".to_string()), "prop title offered: {labels:?}");
    assert!(labels.contains(&":url".to_string()), "dynamic-bound prop offered: {labels:?}");
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
    assert!(labels.contains(&"v-if".to_string()), "directive v-if: {labels:?}");
    assert!(labels.contains(&"href".to_string()), "element attr href on <a>: {labels:?}");

    // `<button @cl` — event name position.
    let (l, c) = pos_after(RFC42_STRUCT_PANEL, "@cl");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(labels.contains(&"@click".to_string()), "event @click: {labels:?}");
}
