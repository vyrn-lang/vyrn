//! End-to-end tests of the `vyrn-lsp` binary over Content-Length-framed
//! JSON-RPC on its stdin and stdout.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// How long `read()` waits for the next server message before failing with a
/// dump of what arrived. A pipe read has no portable timeout, so frames are
/// parsed on a reader thread and handed over a channel. Generous: CI runs
/// debug builds on few cores.
const READ_TIMEOUT_SECS: u64 = 120;

struct Message {
    json: serde_json::Value,
}

/// Reads one framed message; `None` on EOF. Runs on the reader thread.
fn read_frame(stdout: &mut impl Read) -> Option<serde_json::Value> {
    let mut headers = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match stdout.read(&mut byte) {
            Ok(0) => return None,
            Ok(_) => {
                headers.push(byte[0]);
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

/// One line per message for the timeout dump; the URI names which
/// publishDiagnostics arrived.
fn summarize(json: &serde_json::Value) -> String {
    let method = json.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = json.get("id").map(|i| i.to_string()).unwrap_or_default();
    let uri = json
        .pointer("/params/uri")
        .and_then(|u| u.as_str())
        .unwrap_or("");
    format!("  id={id} method={method} uri={uri}")
}

struct LspClient {
    child: std::process::Child,
    /// The sender drops on EOF.
    rx: std::sync::mpsc::Receiver<serde_json::Value>,
    /// One-line summaries of everything received, for the timeout dump.
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl LspClient {
    /// A server with the generator cache off: a fixture is written and
    /// rewritten under the same paths, and a synthesized module from another
    /// run would answer for the one this run wrote.
    fn spawn() -> std::io::Result<Self> {
        let bin = env!("CARGO_BIN_EXE_vyrn-lsp");
        let mut cmd = Command::new(bin);
        cmd.env("VYRN_NO_GEN_CACHE", "1");
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // `VYRN_BUILD_PROFILE=1` makes the server print a phase table per
            // analysis; let it through.
            .stderr(if std::env::var("VYRN_BUILD_PROFILE").is_ok() {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
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

    fn send(&mut self, v: &serde_json::Value) {
        let body = serde_json::to_vec(v).unwrap();
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let stdin = self.child.stdin.as_mut().expect("stdin open");
        stdin.write_all(header.as_bytes()).unwrap();
        stdin.write_all(&body).unwrap();
        stdin.flush().unwrap();
    }

    /// Returns the next message, or `None` on EOF.
    ///
    /// # Panics
    ///
    /// After `READ_TIMEOUT_SECS`, with a dump of every message received.
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

    /// Reads until the response with `id`, skipping everything else.
    fn read_response(&mut self, id: &serde_json::Value) -> serde_json::Value {
        loop {
            let msg = self.read().expect("server closed before responding");
            if msg.json.get("id") == Some(id) {
                return msg.json;
            }
        }
    }

    /// Reads until a notification with `method`, skipping everything else.
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
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn enum_vyrn() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/enum.vyrn");
    std::fs::read_to_string(path).expect("examples/enum.vyrn should exist")
}

/// The server only echoes the URI back, so its form need only round-trip.
fn enum_uri() -> &'static str {
    "file:///N:/lang/examples/enum.vyrn"
}

/// Spawns the server, initializes it, asserts the capabilities it advertises,
/// and opens `enum.vyrn`.
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

/// Request ids from 2 up; 1 is `initialize`'s.
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

    // `Circle` at the call site: 1-based line 19, cols 18-23.
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

    // `area` at the call site: 1-based line 19, cols 13-16.
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
    // `fn area`: 1-based line 10, name cols 4-7.
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

    let shutdown_id = ids.next();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": shutdown_id,
        "method": "shutdown"
    }));
    let _ = client.read_response(&shutdown_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "exit" }));
    let _ = client.child.wait();
}

/// Each symbol carries its 0-based declaration line and LSP `SymbolKind`.
#[test]
fn document_symbol_lists_top_level_declarations() {
    let mut client = open_enum();
    let mut ids = Ids::new();

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

    // LSP SymbolKind: Method=6, Function=12, EnumMember=22, Struct=23.
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
    assert_eq!(by_name["Shape"].0, 3, "Shape declared on 0-based line 3");
    assert_eq!(by_name["Circle"].0, 4, "Circle variant on 0-based line 4");
    assert_eq!(by_name["area"].0, 9, "area declared on 0-based line 9");
    assert_eq!(by_name["main"].0, 17, "main declared on 0-based line 17");
    assert_eq!(by_name["Shape"].1, 23, "type → Struct(23)");
    assert_eq!(by_name["Circle"].1, 22, "variant → EnumMember(22)");
    assert_eq!(by_name["area"].1, 12, "function → Function(12)");
    assert_eq!(by_name["main"].1, 12, "function → Function(12)");

    let shutdown_id = ids.next();
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "id": shutdown_id, "method": "shutdown" }));
    let _ = client.read_response(&shutdown_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "exit" }));
    let _ = client.child.wait();
}

/// A reply with neither `result` nor `error` (what a `Response` with both
/// `None` serializes to) is rejected by VS Code, so `result` must be present.
#[test]
fn hover_off_identifier_returns_null_result() {
    let mut client = open_enum();
    let mut ids = Ids::new();

    // The `fn` keyword of `fn area`: no `TokenInfo` covers it.
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

/// Not the whole line, and not a zero-length range at column 0, which VS Code
/// renders on the line's first token.
#[test]
fn non_exhaustive_match_squiggles_the_match_keyword() {
    let mut client = LspClient::spawn().expect("spawn vyrn-lsp");

    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": init_id,
        "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let _ = client.read_response(&init_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    // `match` is at 1-based line 3, cols 13-17.
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

/// The prose-pinning fallback in `analyze` would squiggle `Age`, the first
/// backtick-quoted token it finds on the line; the column comes from
/// [`vyrn_frontend::ast::ImplBlock::col`] instead.
#[test]
fn an_impl_head_diagnostic_squiggles_the_impl_keyword() {
    let mut client = LspClient::spawn().expect("spawn vyrn-lsp");

    let init_id = serde_json::json!(1);
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": init_id,
        "method": "initialize",
        "params": { "capabilities": {}, "processId": null }
    }));
    let _ = client.read_response(&init_id);
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    // `impl` for a validated scalar is refused. The head is at
    // 1-based line 3, cols 1-4.
    let uri = "file:///impl/head.vyrn";
    let src = "\
type Age = Int64 where value >= 0
protocol Show { fn show(self) -> String }
impl Show for Age {
    fn show(self) -> String { return \"age\" }
}
fn main() -> Int64 { return 0 }
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
        .expect("a publishDiagnostics with the impl error");
    let d = diags
        .iter()
        .find(|d| {
            d.get("message")
                .and_then(|m| m.as_str())
                .is_some_and(|m| m.contains("is not supported"))
        })
        .unwrap_or_else(|| panic!("expected the impl-head refusal: {diags:?}"));

    let start = d.pointer("/range/start").unwrap();
    let end = d.pointer("/range/end").unwrap();
    assert_eq!(
        start.get("line").unwrap().as_i64().unwrap(),
        2,
        "diagnostic on the impl line: {d}"
    );
    assert_eq!(
        start.get("character").unwrap().as_i64().unwrap(),
        0,
        "squiggle starts at `impl` (char 0): {d}"
    );
    // A whole-line fallback would end at the line's length, 18.
    assert_eq!(
        end.get("character").unwrap().as_i64().unwrap(),
        4,
        "squiggle covers exactly `impl` (4 chars), not the whole line: {d}"
    );
}

/// Offers the receiver type's methods, not the top-level symbols.
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
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let mut ids = Ids::new();
    let comp_id = ids.next();
    // Right after the dot of `    a.push(1);`: 1-based line 3, col 7.
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
    assert!(
        !labels.contains(&"main"),
        "top-level `main` leaked into member completion: {labels:?}"
    );
}

/// The hover's `modify` is the only place that says `t.record(..)` mutates `t`.
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

    let (line, ch) = pos_after(src, "    t.");
    let labels = completion_labels(&mut client, uri, line, ch);
    assert!(
        labels.contains(&"record".to_string()),
        "method missing: {labels:?}"
    );

    // The protocol's declaration of `record`.
    let (hline, hch) = pos_after(src, "    fn r");
    let hover = hover_value(&mut client, uri, hline, hch).expect("hover on the declaration");
    assert!(
        hover.contains("fn record(modify self: Tally, n: Int64)"),
        "hover hides the receiver's capability: {hover}"
    );
}

/// Inside a string literal whose expected type is a finite string type, the
/// type's whole language is offered, not the top-level symbols.
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
    // Between the quotes of `    return t("")`: 1-based line 4, col 15.
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
    assert!(
        !labels.contains(&"main"),
        "top-level `main` leaked: {labels:?}"
    );
}

/// An import of a sibling file is clean and navigable; an import of a missing
/// module is a diagnostic.
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

    // Inside `double` of `    return double(21)`.
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

/// One full-range `TextEdit` holding the canonical form.
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

    // A document that fails to lex gets no edit, so a mid-edit buffer is never
    // corrupted.
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

// The origin-map tests write a fixture project to a scratch dir; the
// server finds `std/` by walking up from its own binary.

use std::sync::atomic::{AtomicUsize, Ordering};
static SCRATCH_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn file_uri(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

const VYX_APP: &str = "import { components } from \"std/vyx\"\n\
    import { widget } from components(\"./comp\")\n\
    fn main() -> Int64 { return 0 }\n";

/// Mistypes `title` as `titel`: a type error at line 6, column 8 of the `.vyx`
/// (`<li>{{ ` is 7 chars).
const VYX_TYPE_ERROR: &str = "<script>\n\
    type Row = { title: String }\n\
    props { item: Row }\n\
    </script>\n\
    <template>\n\
    <li>{{ item.titel }}</li>\n\
    </template>\n";

/// Well-typed, for requests where the module must link cleanly.
const VYX_OK: &str = "<script>\n\
    type Row = { title: String }\n\
    props { item: Row }\n\
    </script>\n\
    <template>\n\
    <li>{{ item.title }}</li>\n\
    </template>\n";

fn vyx_scratch(tag: &str, vyx_body: &str) -> std::path::PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_vyx_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("comp")).unwrap();
    std::fs::write(dir.join("comp/Widget.vyx"), vyx_body).unwrap();
    std::fs::write(dir.join("app.vyrn"), VYX_APP).unwrap();
    dir
}

fn spawn_client() -> LspClient {
    handshake(LspClient::spawn().expect("spawn vyrn-lsp"))
}

fn handshake(mut client: LspClient) -> LspClient {
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

/// Reads until a publishDiagnostics whose URI contains `needle`.
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

/// At the `.vyx` line and column, not against the synthesized module.
#[test]
fn vyx_type_error_publishes_into_the_vyx_buffer() {
    let dir = vyx_scratch("diag", VYX_TYPE_ERROR);
    let mut client = spawn_client();
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        VYX_APP,
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

/// Mistypes `title` as `titel` in a dynamic attribute, which the emitter hoists
/// onto its own `let` line with a `rawAt` origin.
const VYX_ATTR_BAD: &str = "<script>\n\
    type Row = { title: String }\n\
    props { item: Row }\n\
    </script>\n\
    <template>\n\
    <a :href=\"item.titel\">x</a>\n\
    </template>\n";

/// `<a :href="` is 10 chars, so `item.titel` starts at line 6, column 11.
#[test]
fn vyx_dyn_attr_check_error_maps_column_exact() {
    let dir = vyx_scratch("attr", VYX_ATTR_BAD);
    let mut client = spawn_client();
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        VYX_APP,
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

/// A stray `\` in a template expression: the lexer rejects it, so the
/// synthesized module never parses.
const VYX_LEX_ERROR: &str = "<script>\n\
    type Row = { title: String }\n\
    props { item: Row }\n\
    </script>\n\
    <template>\n\
    <li>{{ item.title\\ }}</li>\n\
    </template>\n";

/// Like [`read_diags_for`], but skips empty publishes: a clean file is
/// republished every analysis so a fixed error clears.
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

/// At the expression's line and column, with the generated location kept in
/// the message.
#[test]
fn vyx_lex_error_publishes_into_the_vyx_buffer() {
    let dir = vyx_scratch("lex", VYX_LEX_ERROR);
    let mut client = spawn_client();
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        VYX_APP,
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

/// The break exists only in a `didChange` overlay, so the generator cache must
/// re-verify its inputs through the overlay-aware resolver.
#[test]
fn unsaved_vyx_edit_regenerates_and_squiggles() {
    let dir = vyx_scratch("overlay", VYX_OK);
    let vyx_path = dir.join("comp/Widget.vyx");
    let vyx_uri = file_uri(&vyx_path);
    let mut client = spawn_client();
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        VYX_APP,
    );
    // The first analysis records which root owns the `.vyx`.
    let _ = read_diags_for(&mut client, "Widget.vyx");
    did_open(&mut client, &vyx_uri, "vyx", VYX_OK);

    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": vyx_uri, "version": 2 },
            "contentChanges": [{ "text": VYX_LEX_ERROR }]
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
    let on_disk = std::fs::read_to_string(&vyx_path).unwrap();
    assert_eq!(on_disk, VYX_OK, "the test must never touch disk");
}

#[test]
fn completion_in_vyx_template_offers_record_fields() {
    let dir = vyx_scratch("comp", VYX_OK);
    let mut client = spawn_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &app_uri, "vyrn", VYX_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx");
    did_open(&mut client, &vyx_uri, "vyx", VYX_OK);

    // Just past `item.` on line 6: `<li>{{ ` is 7 chars.
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

/// The `md` breakpoint makes `md:` variants exist.
const THEMED_THEME: &str =
    "{ \"colors\": { \"brand\": { \"500\": \"#4f46e5\", \"600\": \"#4338ca\" } },\n\
  \"spacing\": { \"2\": \"0.5rem\", \"4\": \"1rem\" },\n\
  \"breakpoints\": { \"md\": \"768px\" },\n\
  \"safelist\": [\"book-card\"] }";

const THEMED_APP: &str = "import { componentsThemed } from \"std/vyx\"\n\
    import { widget } from componentsThemed(\"./comp\", \"./theme.json\")\n\
    fn main() -> Int64 { return 0 }\n";

const THEMED_VYX: &str = "<script>\n\
    type TransKey = String where value =~ \"(home|about)\"\n\
    fn t(k: TransKey) -> String { return k }\n\
    props { x: String }\n\
    </script>\n\
    <template>\n\
    <div class=\"flex p-4\"><span class=\"book-card\">{{ x }}</span><p class=\"bg-brand-500 md:hover:bg-brand-600\">{{ t(\"home\") }}</p></div>\n\
    </template>\n";

fn themed_scratch() -> std::path::PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_themed_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("comp")).unwrap();
    std::fs::write(dir.join("comp/Widget.vyx"), THEMED_VYX).unwrap();
    std::fs::write(dir.join("app.vyrn"), THEMED_APP).unwrap();
    std::fs::write(dir.join("theme.json"), THEMED_THEME).unwrap();
    dir
}

/// Opens the themed app, which records the widget's owner, then the widget.
/// Returns the client and the widget's URI.
fn open_themed() -> (LspClient, String) {
    let dir = themed_scratch();
    let mut client = spawn_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    did_open(&mut client, &app_uri, "vyrn", THEMED_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx");
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &vyx_uri, "vyx", THEMED_VYX);
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

/// The target URI of a scalar `Location` definition; `None` for no definition.
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

/// Utilities, a safelisted name and variants, filtered by the token under the
/// cursor.
#[test]
fn class_token_completion_offers_tw_alphabet() {
    let (mut client, uri) = open_themed();
    let (l, c) = pos_after(THEMED_VYX, "flex p");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.contains(&"p-4".to_string()),
        "utility p-4 offered: {labels:?}"
    );
    assert!(
        labels.contains(&"px-2".to_string()),
        "utility px-2 offered: {labels:?}"
    );
    assert!(
        !labels.contains(&"widget".to_string()),
        "no top-level leak: {labels:?}"
    );

    let (l, c) = pos_after(THEMED_VYX, "\"book");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.contains(&"book-card".to_string()),
        "safelisted offered: {labels:?}"
    );

    let (l, c) = pos_after(THEMED_VYX, "md:h");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.iter().any(|s| s.starts_with("md:hover:")),
        "md:hover: variants offered: {labels:?}"
    );
}

#[test]
fn hover_on_class_shows_css_or_safelisted() {
    let (mut client, uri) = open_themed();
    let (l, c) = pos_after(THEMED_VYX, "bg-brand-5");
    let v = hover_value(&mut client, &uri, l, c).expect("hover on utility class");
    assert!(
        v.contains("background-color:#4f46e5"),
        "utility CSS rule: {v}"
    );

    let (l, c) = pos_after(THEMED_VYX, "book-car");
    let v = hover_value(&mut client, &uri, l, c).expect("hover on safelisted class");
    assert!(v.contains("safelisted"), "safelisted note: {v}");
}

/// The origin map's forward direction routes string-literal contexts too.
#[test]
fn transkey_completion_inside_mustache() {
    let (mut client, uri) = open_themed();
    let (l, c) = pos_after(THEMED_VYX, "t(\"");
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

// Structural completion works on a raw `.vyx` with no owning root.
const STRUCT_PANEL: &str = "<template>\n\
    <Book></Book>\n\
    <BookCard t></BookCard>\n\
    <a v-i></a>\n\
    <button @cl></button>\n\
    </template>\n";

const STRUCT_CARD: &str = "<script>\nprops { title: String, url: String }\n</script>\n\
    <template>\n<div>{{ title }}</div>\n</template>\n";

fn struct_dir() -> std::path::PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_struct_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Panel.vyx"), STRUCT_PANEL).unwrap();
    std::fs::write(dir.join("BookCard.vyx"), STRUCT_CARD).unwrap();
    dir
}

#[test]
fn component_tag_completion() {
    let dir = struct_dir();
    let mut client = spawn_client();
    let uri = file_uri(&dir.join("Panel.vyx"));
    did_open(&mut client, &uri, "vyx", STRUCT_PANEL);
    let (l, c) = pos_after(STRUCT_PANEL, "<Book");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.contains(&"BookCard".to_string()),
        "sibling component offered: {labels:?}"
    );
}

#[test]
fn component_prop_completion() {
    let dir = struct_dir();
    let mut client = spawn_client();
    let uri = file_uri(&dir.join("Panel.vyx"));
    did_open(&mut client, &uri, "vyx", STRUCT_PANEL);
    let (l, c) = pos_after(STRUCT_PANEL, "<BookCard t");
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

#[test]
fn attribute_and_event_completion() {
    let dir = struct_dir();
    let mut client = spawn_client();
    let uri = file_uri(&dir.join("Panel.vyx"));
    did_open(&mut client, &uri, "vyx", STRUCT_PANEL);

    let (l, c) = pos_after(STRUCT_PANEL, "<a v-i");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.contains(&"v-if".to_string()),
        "directive v-if: {labels:?}"
    );
    assert!(
        labels.contains(&"href".to_string()),
        "element attr href on <a>: {labels:?}"
    );

    let (l, c) = pos_after(STRUCT_PANEL, "@cl");
    let labels = completion_labels(&mut client, &uri, l, c);
    assert!(
        labels.contains(&"@click".to_string()),
        "event @click: {labels:?}"
    );
}

/// The server's token-type legend, in order; keep equal to
/// `semantic_tokens_legend`.
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

/// One decoded semantic token at an absolute 0-based position; `mods` is the
/// modifier bitset.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `len` and `mods` show in failure output only
struct SemTok {
    line: u32,
    ch: u32,
    len: u32,
    ty: String,
    mods: u32,
}

/// Requests `semanticTokens/full` and decodes the delta stream to absolute
/// positions.
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

fn kind_at(toks: &[SemTok], line: u32, ch: u32) -> Option<&str> {
    toks.iter()
        .find(|t| t.line == line && t.ch == ch)
        .map(|t| t.ty.as_str())
}

/// An import specifier gets its real kind, which a TextMate grammar cannot
/// give.
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

    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", main_src);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let toks = semantic_tokens_full(&mut client, &uri);
    assert!(!toks.is_empty(), "semantic tokens returned");

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

    let (l, c) = at(main_src, 5, "dist");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("function"),
        "`dist` decl → function"
    );
    let (l, c) = at(main_src, 5, "p");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("parameter"),
        "param `p` → parameter"
    );
    let (l, c) = at(main_src, 5, "Point");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("type"),
        "`Point` annotation → type"
    );

    let (l, c) = at(main_src, 6, "p.x");
    assert_eq!(
        kind_at(&toks, l, c + 2),
        Some("property"),
        "`p.x` field → property"
    );

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

/// At the import site, not only the use site.
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

    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", main_src);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let (l, c) = at(main_src, 1, "greet");
    let v = hover_value(&mut client, &uri, l, c).expect("hover at import specifier");
    assert!(
        v.contains("greet"),
        "import-site hover shows the signature: {v}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Template tokens classify through the origin map.
#[test]
fn semantic_tokens_in_vyx_template() {
    let dir = vyx_scratch("sem", VYX_OK);
    let mut client = spawn_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &app_uri, "vyrn", VYX_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx");
    did_open(&mut client, &vyx_uri, "vyx", VYX_OK);

    let toks = semantic_tokens_full(&mut client, &vyx_uri);
    // `<li>{{ item.title }}` on line 6: `item` at char 7, `title` at char 12.
    assert_eq!(
        kind_at(&toks, 5, 7),
        Some("parameter"),
        "template `item` (prop) → parameter: {toks:?}"
    );
    assert_eq!(
        kind_at(&toks, 5, 12),
        Some("property"),
        "template `title` field → property: {toks:?}"
    );
}

// The `<script>` section carries origins for its import and helper lines, and
// pages, layouts and errors compile against the real route file.

/// The component's `<script>` imports `std/time` selectively and as a
/// namespace, and declares helper fns.
const PAGES_APP: &str = "import { components } from \"std/vyx\"\n\
    import { widget } from components(\"./comp\")\n\
    fn main() -> Int64 { return 0 }\n";
const PAGES_VYX: &str = "<script>\n\
    import { format, fromMillis } from \"std/time\"\n\
    import * as clk from \"std/time\"\n\
    fn shown() -> String { return format(fromMillis(0)) }\n\
    fn now() -> String { return clk.format(clk.fromMillis(0)) }\n\
    props { x: String }\n\
    </script>\n\
    <template>\n\
    <li>{{ shown() }} {{ x }} {{ now() }}</li>\n\
    </template>\n";

#[test]
fn vyx_script_import_hover_and_classification() {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_pages_c_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("comp")).unwrap();
    std::fs::write(dir.join("comp/Widget.vyx"), PAGES_VYX).unwrap();
    std::fs::write(dir.join("app.vyrn"), PAGES_APP).unwrap();
    let mut client = spawn_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &app_uri, "vyrn", PAGES_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx");
    did_open(&mut client, &vyx_uri, "vyx", PAGES_VYX);

    let (hl, hc) = at(PAGES_VYX, 2, "format");
    let v = hover_value(&mut client, &vyx_uri, hl, hc)
        .expect("hover on `.vyx` script import specifier `format`");
    assert!(
        v.contains("format"),
        "import-specifier hover shows the signature: {v}"
    );

    let toks = semantic_tokens_full(&mut client, &vyx_uri);
    let (fl, fc) = at(PAGES_VYX, 2, "format");
    assert_eq!(
        kind_at(&toks, fl, fc),
        Some("function"),
        "import `format` → function: {toks:?}"
    );
    let (ml, mc) = at(PAGES_VYX, 2, "fromMillis");
    assert_eq!(
        kind_at(&toks, ml, mc),
        Some("function"),
        "import `fromMillis` → function: {toks:?}"
    );
    let (cl, cc) = at(PAGES_VYX, 3, "clk");
    assert_eq!(
        kind_at(&toks, cl, cc),
        Some("namespace"),
        "`import * as clk` → namespace: {toks:?}"
    );
    let (sl, sc) = at(PAGES_VYX, 4, "shown");
    assert_eq!(
        kind_at(&toks, sl, sc),
        Some("function"),
        "helper `shown` def → function: {toks:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// In every position: call, pattern, bare and nested.
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

    let mut client = spawn_client();
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
    expect("Circle", 3);
    expect("Ok", 7);
    expect("Err", 9);
    expect("Some", 13);
    expect("Ok", 13);
    expect("None", 15);
    expect("Circle", 19);
    expect("Dot", 20);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A `<script>` helper builds a `Result` with `Ok`, which classifies through the
/// origin map's forward direction as in a plain `.vyrn`.
const CTOR_APP: &str = "import { components } from \"std/vyx\"\n\
    import { widget } from components(\"./comp\")\n\
    fn main() -> Int64 { return 0 }\n";
const CTOR_VYX: &str = "<script>\n\
    fn wrap(n: Int64) -> Result<Int64, String> { return Ok(n) }\n\
    props { x: String }\n\
    </script>\n\
    <template>\n\
    <li>{{ x }}</li>\n\
    </template>\n";

#[test]
fn semantic_tokens_classify_constructor_in_vyx_script() {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_ctor_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("comp")).unwrap();
    std::fs::write(dir.join("comp/Widget.vyx"), CTOR_VYX).unwrap();
    std::fs::write(dir.join("app.vyrn"), CTOR_APP).unwrap();
    let mut client = spawn_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &app_uri, "vyrn", CTOR_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx");
    did_open(&mut client, &vyx_uri, "vyx", CTOR_VYX);

    let toks = semantic_tokens_full(&mut client, &vyx_uri);
    let (l, c) = at(CTOR_VYX, 2, "Ok(n)");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("enumMember"),
        "`.vyx` script `Ok` → enumMember: {toks:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A `pagesThemed` app with one themed route, `routes/index.vyx`.
const PAGES_PAGE_APP: &str = "import { pagesThemed } from \"std/ui\"\n\
    import { route } from pagesThemed(\"./routes\", \"./theme.json\")\n\
    fn handle(req: Request) -> Response { return route(req) }\n";
const PAGES_INDEX: &str = "<script>\n\
    import { format, fromMillis } from \"std/time\"\n\
    import * as clk from \"std/time\"\n\
    fn shown() -> String { return format(fromMillis(0)) }\n\
    fn now() -> String { return clk.format(clk.fromMillis(0)) }\n\
    </script>\n\
    <template>\n\
    <main class=\"flex p-4\"><a class=\"mr-2 hover:bg-brand-600\">{{ shown() }}{{ now() }}</a></main>\n\
    </template>\n";

/// A page's origins point at the real route file, not a synthetic one.
#[test]
fn page_semantic_tokens_and_class_completion() {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_pages_p_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("routes")).unwrap();
    std::fs::write(dir.join("routes/index.vyx"), PAGES_INDEX).unwrap();
    std::fs::write(dir.join("app.vyrn"), PAGES_PAGE_APP).unwrap();
    std::fs::write(dir.join("theme.json"), THEMED_THEME).unwrap();
    let mut client = spawn_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let index_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(&mut client, &app_uri, "vyrn", PAGES_PAGE_APP);
    let _ = read_diags_for(&mut client, "index.vyx");
    did_open(&mut client, &index_uri, "vyx", PAGES_INDEX);

    let toks = semantic_tokens_full(&mut client, &index_uri);
    assert!(
        !toks.is_empty(),
        "the page route file now has semantic tokens (was 0)"
    );
    let (fl, fc) = at(PAGES_INDEX, 2, "format");
    assert_eq!(
        kind_at(&toks, fl, fc),
        Some("function"),
        "page import `format` → function: {toks:?}"
    );

    let (l, c) = pos_after(PAGES_INDEX, "flex p");
    let labels = completion_labels(&mut client, &index_uri, l, c);
    assert!(
        labels.contains(&"p-4".to_string()),
        "Tw class completion on the page: {labels:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Prints a hover, semantic-token and completion session on a route page; read
/// it with `--nocapture`. A lean scratch app, not `examples/bin`, because every
/// forward-map request re-runs the owner's generators.
#[test]
fn live_transcript_pagesthemed() {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_pages_t_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("routes")).unwrap();
    std::fs::write(dir.join("routes/index.vyx"), PAGES_INDEX).unwrap();
    std::fs::write(dir.join("app.vyrn"), PAGES_PAGE_APP).unwrap();
    std::fs::write(dir.join("theme.json"), THEMED_THEME).unwrap();
    let mut client = spawn_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let index_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(&mut client, &app_uri, "vyrn", PAGES_PAGE_APP);
    let _ = read_diags_for(&mut client, "index.vyx");
    did_open(&mut client, &index_uri, "vyx", PAGES_INDEX);

    println!("\n===== LIVE TRANSCRIPT (pagesThemed route: routes/index.vyx) =====");

    for (label, line, name) in [
        ("format", 2u32, "format"),
        ("fromMillis", 2, "fromMillis"),
        ("clk (ns)", 3, "clk"),
    ] {
        let (l, c) = at(PAGES_INDEX, line as usize, name);
        let h = hover_value(&mut client, &index_uri, l, c).unwrap_or("<none>".into());
        println!(
            "  hover {label:>11} @{}:{} -> {}",
            l + 1,
            c + 1,
            h.replace('\n', " ⏎ ")
        );
    }

    let toks = semantic_tokens_full(&mut client, &index_uri);
    println!("  semanticTokens/full: {} tokens", toks.len());
    for (label, line, name) in [
        ("format", 2u32, "format"),
        ("fromMillis", 2, "fromMillis"),
        ("clk", 3, "clk"),
        ("shown(def)", 4, "shown"),
    ] {
        let (l, c) = at(PAGES_INDEX, line as usize, name);
        println!(
            "  token {label:>11} @{}:{} -> {:?}",
            l + 1,
            c + 1,
            kind_at(&toks, l, c)
        );
    }

    let (cl, cc) = pos_after(PAGES_INDEX, "mr-");
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
    let (hl, hc) = pos_after(PAGES_INDEX, "hover:bg-brand-6");
    let css = hover_value(&mut client, &index_uri, hl, hc).unwrap_or("<none>".into());
    println!(
        "  class-hover @{}:{} (hover:bg-brand-600) -> {}",
        hl + 1,
        hc + 1,
        css.replace('\n', " ⏎ ")
    );
    println!("=========================================================================\n");

    assert!(!toks.is_empty(), "page semantic tokens non-empty");
    let (l, c) = at(PAGES_INDEX, 2, "format");
    assert_eq!(kind_at(&toks, l, c), Some("function"), "format -> function");
    let (l, c) = at(PAGES_INDEX, 3, "clk");
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

// The owner-discovery tests open only the `.vyx`, never its owning
// `.vyrn`, as a user does; the server discovers the owner on didOpen.

/// The app root is found through `vyrn.json`.
#[test]
fn open_only_page_vyx_is_fully_analyzed() {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("vyrn_lsp_openonly_page_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("routes")).unwrap();
    std::fs::write(dir.join("routes/index.vyx"), PAGES_INDEX).unwrap();
    std::fs::write(dir.join("app.vyrn"), PAGES_PAGE_APP).unwrap();
    std::fs::write(dir.join("theme.json"), THEMED_THEME).unwrap();
    std::fs::write(dir.join("vyrn.json"), "{ \"name\": \"p\" }").unwrap();

    let mut client = spawn_client();
    let index_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(&mut client, &index_uri, "vyx", PAGES_INDEX);

    let (hl, hc) = at(PAGES_INDEX, 2, "format");
    let v = hover_value(&mut client, &index_uri, hl, hc)
        .expect("hover works on a standalone-opened page .vyx (owner discovered)");
    assert!(
        v.contains("format"),
        "hover shows the imported signature: {v}"
    );

    let toks = semantic_tokens_full(&mut client, &index_uri);
    assert!(
        !toks.is_empty(),
        "standalone page .vyx has semantic tokens (was 0)"
    );
    let (fl, fc) = at(PAGES_INDEX, 2, "format");
    assert_eq!(
        kind_at(&toks, fl, fc),
        Some("function"),
        "`format` → function: {toks:?}"
    );
    let (ml, mc) = at(PAGES_INDEX, 2, "fromMillis");
    assert_eq!(
        kind_at(&toks, ml, mc),
        Some("function"),
        "`fromMillis` → function: {toks:?}"
    );

    let (dl, dc) = at(PAGES_INDEX, 4, "format");
    let target = definition_target(&mut client, &index_uri, dl, dc)
        .expect("definition jumps from a standalone page .vyx");
    assert!(
        target.contains("time"),
        "definition lands in std/time: {target}"
    );

    let (cl, cc) = pos_after(PAGES_INDEX, "flex p");
    let labels = completion_labels(&mut client, &index_uri, cl, cc);
    assert!(
        labels.contains(&"p-4".to_string()),
        "class completion on a standalone page: {labels:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// No `vyrn.json`: discovery finds the app root as the `.vyrn` that imports a
/// generator.
const OPEN_ONLY_COMP_APP: &str = "import { componentsThemed } from \"std/vyx\"\n\
    import { widget } from componentsThemed(\"./widgets\", \"./theme.json\")\n\
    fn main() -> Int64 { return 0 }\n";
const OPEN_ONLY_WIDGET: &str = "<script>\n\
    import { format, fromMillis } from \"std/time\"\n\
    props { label: String }\n\
    fn shown() -> String { return format(fromMillis(0)) }\n\
    </script>\n\
    <template>\n\
    <section class=\"flex p-4\">{{ shown() }} {{ label }}</section>\n\
    </template>\n";

#[test]
fn open_only_component_vyx_is_fully_analyzed() {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("vyrn_lsp_openonly_comp_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("widgets")).unwrap();
    std::fs::write(dir.join("widgets/Widget.vyx"), OPEN_ONLY_WIDGET).unwrap();
    std::fs::write(dir.join("app.vyrn"), OPEN_ONLY_COMP_APP).unwrap();
    std::fs::write(dir.join("theme.json"), THEMED_THEME).unwrap();

    let mut client = spawn_client();
    let widget_uri = file_uri(&dir.join("widgets/Widget.vyx"));
    did_open(&mut client, &widget_uri, "vyx", OPEN_ONLY_WIDGET);

    let (hl, hc) = at(OPEN_ONLY_WIDGET, 2, "format");
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
    let (fl, fc) = at(OPEN_ONLY_WIDGET, 2, "format");
    assert_eq!(
        kind_at(&toks, fl, fc),
        Some("function"),
        "`format` → function: {toks:?}"
    );
    let (sl, sc) = at(OPEN_ONLY_WIDGET, 4, "shown");
    assert_eq!(
        kind_at(&toks, sl, sc),
        Some("function"),
        "helper `shown` def → function: {toks:?}"
    );

    let (dl, dc) = at(OPEN_ONLY_WIDGET, 4, "format");
    let target = definition_target(&mut client, &widget_uri, dl, dc)
        .expect("definition jumps from a standalone component .vyx");
    assert!(
        target.contains("time"),
        "definition lands in std/time: {target}"
    );

    let (cl, cc) = pos_after(OPEN_ONLY_WIDGET, "flex p");
    let labels = completion_labels(&mut client, &widget_uri, cl, cc);
    assert!(
        labels.contains(&"p-4".to_string()),
        "class completion on a standalone component: {labels:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// One `documentHighlight` occurrence: 1-based `line`, 0-based `ch`, and the
/// `DocumentHighlightKind` (2 = Read, 3 = Write). `ch` shows in failure output.
#[derive(Debug)]
struct Highlight {
    line: u32,
    #[allow(dead_code)]
    ch: u32,
    kind: u64,
}

/// Returns the occurrences at 0-based `(line, ch)`; empty when the cursor
/// resolves to no binding, so VS Code does not fall back to word-matching.
///
/// # Panics
///
/// On a null result.
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

/// `count` is a local in `tally`, a word in a comment, and a binding in `other`.
const HIGHLIGHT_SRC: &str = "\
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

/// The declaration is a `Write`, the uses `Read`s; a same-named comment word
/// and an out-of-scope binding are excluded. The provider must be
/// advertised or VS Code word-matches.
#[test]
fn document_highlight_is_scope_aware() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-hl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("hl.vyrn");
    std::fs::write(&path, HIGHLIGHT_SRC).unwrap();
    let uri = file_uri(&path);

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
    did_open(&mut client, &uri, "vyrn", HIGHLIGHT_SRC);

    let (l, c) = at(HIGHLIGHT_SRC, 2, "count");
    let hls = document_highlight(&mut client, &uri, l, c);
    let lines: Vec<u32> = hls.iter().map(|h| h.line).collect();

    assert!(lines.contains(&2), "decl line 2 highlighted: {hls:?}");
    assert!(lines.contains(&4), "use line 4 highlighted: {hls:?}");
    assert!(lines.contains(&6), "use line 6 highlighted: {hls:?}");
    let decl = hls.iter().find(|h| h.line == 2).unwrap();
    assert_eq!(decl.kind, 3, "declaration is Write: {decl:?}");
    assert!(
        hls.iter().filter(|h| h.line != 2).all(|h| h.kind == 2),
        "uses are Read: {hls:?}"
    );

    // Line 11 is the comment; lines 12 and 13 are `other`'s binding.
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

    let hls2 = document_highlight(&mut client, &uri, 0, 0); // the `f` of `fn`
    assert!(
        hls2.is_empty(),
        "unresolved cursor yields empty (not word-match): {hls2:?}"
    );

    let _ = client.child.kill();
}

/// The loader resolves both a relative and a `std/` import string.
#[test]
fn definition_on_import_path() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-imp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Not `get`: that name is reserved (the slot table's checked read).
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

    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", app);

    let (l, c) = at(app, 1, "./store");
    let store = definition_target(&mut client, &uri, l, c).expect("definition on ./store");
    assert!(
        store.ends_with("/store.vyrn"),
        "./store → sibling store.vyrn: {store}"
    );

    let (l, c) = at(app, 2, "std/time");
    let stdt = definition_target(&mut client, &uri, l, c).expect("definition on std/time");
    assert!(
        stdt.replace('\\', "/").ends_with("std/time.vyrn"),
        "std/time → std file: {stdt}"
    );

    // Off an import string, identifier definition still answers.
    let (l, c) = at(app, 3, "stored");
    let g = definition_target(&mut client, &uri, l, c).expect("definition on stored() call");
    assert!(
        g.ends_with("/store.vyrn"),
        "stored() → its imported decl in store.vyrn: {g}"
    );

    let _ = client.child.kill();
}

/// Both the binding and every `ns.` qualifier, never `type`. A theme
/// may still colour `namespace` like `type`.
#[test]
fn namespace_binding_classifies_as_namespace() {
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

    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", app);
    let toks = semantic_tokens_full(&mut client, &uri);

    let (l, c) = at(app, 1, "store");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("namespace"),
        "import binding → namespace: {toks:?}"
    );
    let (l, c) = at(app, 2, "store");
    assert_eq!(
        kind_at(&toks, l, c),
        Some("namespace"),
        "ns qualifier → namespace: {toks:?}"
    );

    let _ = client.child.kill();
}

/// Beneath the signature, for a local and for an imported declaration.
#[test]
fn doc_comment_renders_in_hover() {
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

    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", app);

    let (l, c) = at(app, 3, "bump(1)");
    let h = hover_value(&mut client, &uri, l, c).expect("hover on imported fn");
    // Fenced as ```vyrn so the editor highlights it with the extension's
    // grammar (`fence_signature`).
    assert!(
        h.starts_with("```vyrn\nfn bump(n: Int64) -> Int64\n```"),
        "fenced signature first: {h}"
    );
    assert!(
        h.contains("Adds one. A parity citizen: pure."),
        "imported doc rendered: {h}"
    );

    let (l, c) = at(app, 3, "main");
    let h = hover_value(&mut client, &uri, l, c).expect("hover on own fn");
    assert!(
        h.contains("The entry point, documented."),
        "own-module doc rendered: {h}"
    );

    let _ = client.child.kill();
}

/// A namespace over a generated module resolves its members, and the generated
/// `///` doc is the translation.
#[test]
fn generated_namespace_member_hover_shows_translation() {
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

    let mut client = spawn_client();
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

/// After the `.` a hover shows the field; on the receiver it shows the record's
/// shape, not only its name.
#[test]
fn member_and_structure_hover_in_vyx_template() {
    let dir = vyx_scratch("hover51", VYX_OK);
    let mut client = spawn_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &app_uri, "vyrn", VYX_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx");
    did_open(&mut client, &vyx_uri, "vyx", VYX_OK);

    // `<li>{{ item.title }}` on line 6: `item` at char 7, `title` from 12.
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

#[test]
fn class_hover_and_completion_agree_on_the_token_under_the_cursor() {
    let (mut client, uri) = open_themed();

    // `class="bg-brand-500 md:hover:bg-brand-600"`.
    let (l, c) = pos_after(THEMED_VYX, "bg-brand-5");
    let h = hover_value(&mut client, &uri, l, c).expect("hover on the first class");
    assert!(
        h.contains("`bg-brand-500`"),
        "the token under the cursor: {h}"
    );
    assert!(
        !h.contains("md:hover:"),
        "not the LAST token on the line: {h}"
    );

    let (l2, c2) = pos_after(THEMED_VYX, "md:hover:bg-brand-6");
    let h2 = hover_value(&mut client, &uri, l2, c2).expect("hover on the second class");
    assert!(
        h2.contains("`md:hover:bg-brand-600`"),
        "the variant token: {h2}"
    );

    let id = serde_json::json!("cls");
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
    let tok_start = (c2 as u64) - ("md:hover:bg-brand-6".len() as u64);
    assert_eq!(
        start, tok_start,
        "completion replaces from the token start: {item}"
    );
    assert_eq!(end, c2 as u64, "…up to the cursor: {item}");

    let _ = client.child.kill();
}

/// `book-card` is safelisted and app-styled; `no-style` is safelisted only.
const STYLED_THEME: &str = "{ \"colors\": { \"brand\": { \"500\": \"#4f46e5\" } },\n\
  \"spacing\": { \"2\": \"0.5rem\" },\n\
  \"safelist\": [\"book-card\", \"no-style\"] }";

const STYLED_APP: &str = "import { componentsThemed } from \"std/vyx\"\n\
    import { widget } from componentsThemed(\"./comp\", \"./theme.json\")\n\
    fn main() -> Int64 { return 0 }\n";

const STYLED_VYX: &str = "<script>\n\
    props { x: String }\n\
    </script>\n\
    <template>\n\
    <div class=\"book-card\"><span class=\"no-style\">{{ x }}</span><p class=\"bg-brand-500\">{{ x }}</p></div>\n\
    </template>\n";

/// Declares the app's stylesheet; discovery reads only its
/// `stylesheet` line.
const STYLED_LAYOUT: &str = "<script>\nhead {\n    stylesheet \"/app.css\"\n}\n</script>\n\
    <template>\n<div><slot/></div>\n</template>\n";

/// `.book-card-x` and `.book-cards` must not match `book-card`; the descendant
/// and `:hover` rules must.
const STYLED_CSS: &str = ".book-card-x { color: red; }\n\
.book-cards { color: blue; }\n\
li.list .book-card {\n  padding: 2px;\n}\n\
.book-card:hover { color: green; }\n";

/// Undeclared, so ignored while a declared stylesheet exists.
const STYLED_DECOY_CSS: &str = ".book-card { content: \"DECOYRULE\"; }\n";

fn styled_scratch() -> std::path::PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_styled_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("comp")).unwrap();
    std::fs::create_dir_all(dir.join("public")).unwrap();
    std::fs::create_dir_all(dir.join("routes")).unwrap();
    std::fs::write(dir.join("comp/Widget.vyx"), STYLED_VYX).unwrap();
    std::fs::write(dir.join("routes/layout.vyx"), STYLED_LAYOUT).unwrap();
    std::fs::write(dir.join("app.vyrn"), STYLED_APP).unwrap();
    std::fs::write(dir.join("theme.json"), STYLED_THEME).unwrap();
    std::fs::write(dir.join("public/app.css"), STYLED_CSS).unwrap();
    std::fs::write(dir.join("public/decoy.css"), STYLED_DECOY_CSS).unwrap();
    dir
}

fn open_styled() -> (LspClient, String) {
    let dir = styled_scratch();
    let mut client = spawn_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    did_open(&mut client, &app_uri, "vyrn", STYLED_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx");
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &vyx_uri, "vyx", STYLED_VYX);
    (client, vyx_uri)
}

/// The "safelisted (app-styled)" line stays and the app's rules follow with
/// `file:line`.
#[test]
fn safelisted_hover_shows_the_apps_own_css() {
    let (mut client, uri) = open_styled();
    let (l, c) = pos_after(STYLED_VYX, "book-car");
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
    assert!(
        !v.contains("book-card-x"),
        "`.book-card-x` must not match: {v}"
    );
    assert!(
        !v.contains("book-cards"),
        "`.book-cards` must not match: {v}"
    );
    assert!(
        !v.contains("DECOYRULE"),
        "undeclared stylesheet not consulted: {v}"
    );
    let _ = client.child.kill();
}

#[test]
fn safelisted_without_a_rule_is_unchanged() {
    let (mut client, uri) = open_styled();
    let (l, c) = pos_after(STYLED_VYX, "no-sty");
    let v = hover_value(&mut client, &uri, l, c).expect("hover on safelisted class");
    assert_eq!(
        v, "**`no-style`** — safelisted (app-styled)",
        "unchanged: {v}"
    );
    let _ = client.child.kill();
}

/// A utility class hovers with its generated rule and no app-CSS block.
#[test]
fn utility_hover_is_unchanged() {
    let (mut client, uri) = open_styled();
    let (l, c) = pos_after(STYLED_VYX, "bg-brand-5");
    let v = hover_value(&mut client, &uri, l, c).expect("hover on utility class");
    assert!(v.contains("`Tw` utility class"), "utility hover: {v}");
    assert!(v.contains("background-color:#4f46e5"), "generated CSS: {v}");
    assert!(!v.contains("app.css"), "no app-CSS block appended: {v}");
    let _ = client.child.kill();
}

/// A broken `vyrn"..."` skeleton is a parse diagnostic in the generator's file
/// at the literal's line, opened under the URI form VS Code sends
/// (`file:///c%3A/...`, drive lower-cased and percent-encoded).
#[test]
fn broken_skeleton_publishes_in_the_generator_file() {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_skeleton_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // `type Query {` is GraphQL, not Vyrn, so the skeleton parses in no mode.
    let gen = "export gen fn mk(name: String) -> String {\n\
               let body = vyrn\"\"\"\n\
               type Query {\n\
               }\"\"\"\n\
               return render(body)\n\
               }\n";
    let gen_path = dir.join("gen.vyrn");
    std::fs::write(&gen_path, gen).unwrap();

    let raw = gen_path.to_string_lossy().replace('\\', "/");
    let uri = if raw.len() > 2 && raw.as_bytes()[1] == b':' {
        let drive = raw[..1].to_lowercase();
        format!("file:///{drive}%3A{}", &raw[2..])
    } else {
        format!("file://{raw}")
    };

    let mut client = spawn_client();
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
    // The literal's content starts on 1-based line 3.
    let line = diags[0]
        .pointer("/range/start/line")
        .and_then(|l| l.as_i64());
    assert_eq!(line, Some(2), "reported at the literal's line: {note}");
    let _ = client.child.kill();
}

/// Asks `vyrn/isDevEntry`: does the document import `std/rpc` and call
/// `rpcServer(...)`? The extension shows its "Run dev server" CodeLens on a yes.
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
    let mut client = spawn_client();

    let server_src = "import { rpcServer } from \"std/rpc\"\n\
        import { rpcHandle } from rpcServer(\"./contract\")\n\
        fn handle(req: Request) -> Response { return rpcHandle(req) }\n";
    let server_uri = "file:///n%3A/lang/scratch/server.vyrn";
    did_open(&mut client, server_uri, "vyrn", server_src);
    assert!(
        query_is_dev_entry(&mut client, server_uri, 900),
        "a serve-calling root is a dev entry"
    );

    let client_src = "import { rpcClient } from \"std/rpc\"\n\
        import * as api from rpcClient(\"./contract\")\n\
        fn main() -> Int64 { return 0 }\n";
    let client_uri = "file:///n%3A/lang/scratch/client.vyrn";
    did_open(&mut client, client_uri, "vyrn", client_src);
    assert!(
        !query_is_dev_entry(&mut client, client_uri, 901),
        "an rpc client is NOT a dev entry"
    );

    let lib_src = "fn main() -> Int64 { print(\"hi\") return 0 }\n";
    let lib_uri = "file:///n%3A/lang/scratch/lib.vyrn";
    did_open(&mut client, lib_uri, "vyrn", lib_src);
    assert!(
        !query_is_dev_entry(&mut client, lib_uri, 902),
        "a plain module is NOT a dev entry"
    );

    // Unopened files, which the handler reads from disk.
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

/// Emits one `//@warning` directive positioned at `legacy.vyrn:2:1`. No std
/// generator emits warnings, so the channel's fixture is synthetic.
const WARN_GEN: &str = "export gen fn legacy(path: String) -> String {\n    \
    return \"//@warning \" + path + \":2:1 `fn old` is deprecated — write `fn new` instead\n\" +\n        \
    \"export fn greeting() -> String {\n    return \\\"hi\\\"\n}\n\"\n}\n";

const WARN_LEGACY: &str = "// A module on the old form.\nfn old() -> Int64 {\n    return 1\n}\n";

const WARN_APP: &str = "import { legacy } from \"./gen\"\n\
    import { greeting } from legacy(\"./legacy.vyrn\")\n\
    fn main() -> Int64 { print(greeting()) return 0 }\n";

/// Into the buffer and line its origin names, with warning severity, on a
/// project that compiles.
#[test]
fn a_generator_warning_publishes_into_the_input_buffer() {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_genwarn_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("gen.vyrn"), WARN_GEN).unwrap();
    std::fs::write(dir.join("legacy.vyrn"), WARN_LEGACY).unwrap();
    std::fs::write(dir.join("app.vyrn"), WARN_APP).unwrap();

    let mut client = spawn_client();
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
    // 2 is DiagnosticSeverity.Warning.
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
    assert_eq!(
        d0.pointer("/range/start/line").and_then(|l| l.as_i64()),
        Some(1),
        "line: {note}"
    );

    let _ = client.child.kill();
}

// Contract members in the editor. These drive the real server over
// stdio because editor behaviour that passed unit tests has twice done nothing
// in the editor.

/// Nothing says `routes/` holds pages: the LSP learns it from the `pages`
/// import.
const CONTRACT_APP: &str = "import { pages } from \"std/ui\"\n\
    import { components } from \"std/vyx\"\n\
    import { route } from pages(\"./routes\")\n\
    import { widget } from components(\"./widgets\")\n\
    fn main() -> Int64 { return 0 }\n";

const CONTRACT_MANIFEST: &str = "{ \"name\": \"m4\", \"main\": \"app.vyrn\" }\n";

/// Declares `head` and lacks `data`, so completion has one member to offer and
/// one to drop.
const CONTRACT_PAGE: &str = "<script>\n\
    import { Head, noHead } from \"std/ui\"\n\
    export fn head() -> Head {\n    return noHead()\n}\n\
    \n\
    </script>\n\
    <template>\n<h1>home</h1>\n</template>\n";

/// `head { ... }` is a layout form; a layout has no contract, so the editor
/// offers it no members.
const CONTRACT_LAYOUT: &str = "<script>\n\
    head {\n    stylesheet \"/s.css\"\n}\n\
    \n\
    </script>\n\
    <template>\n<div><slot/></div>\n</template>\n";

/// A `.vyrn` page whose `dta` is a near miss of `data`.
const CONTRACT_TYPO_PAGE: &str = "import { Query, query } from \"std/ui\"\n\
    \n\
    export fn dta() -> Query<Int64> {\n    return query(one)\n}\n\
    \n\
    fn one() -> Int64 {\n    return 1\n}\n";

fn contract_scratch(tag: &str) -> std::path::PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_m4_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("routes")).unwrap();
    std::fs::create_dir_all(dir.join("widgets")).unwrap();
    std::fs::write(dir.join("vyrn.json"), CONTRACT_MANIFEST).unwrap();
    std::fs::write(dir.join("app.vyrn"), CONTRACT_APP).unwrap();
    std::fs::write(dir.join("routes/index.vyx"), CONTRACT_PAGE).unwrap();
    std::fs::write(dir.join("routes/layout.vyx"), CONTRACT_LAYOUT).unwrap();
    std::fs::write(dir.join("routes/typo.vyrn"), CONTRACT_TYPO_PAGE).unwrap();
    dir
}

/// Every completion item at `(line, ch)`, as raw JSON.
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
fn code_action_capability_is_advertised() {
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

/// At module scope, as snippets that insert the full declaration and carry the
/// member's doc; a member the page already wrote is not offered.
#[test]
fn completion_offers_contract_members_in_a_vyx_page() {
    let dir = contract_scratch("comp");
    let mut client = spawn_client();
    let page_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        CONTRACT_APP,
    );
    did_open(&mut client, &page_uri, "vyx", CONTRACT_PAGE);

    // 0-based line 5: the blank line after head's body, at module scope.
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
    // The `<template>` writes `page`; a snippet would collide with the
    // generated export.
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
    // 2 is InsertTextFormat.Snippet; without it VS Code inserts the tabstops
    // as text.
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

/// A layout has no contract.
#[test]
fn completion_offers_nothing_in_a_layout() {
    let dir = contract_scratch("layout");
    let mut client = spawn_client();
    let layout_uri = file_uri(&dir.join("routes/layout.vyx"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        CONTRACT_APP,
    );
    did_open(&mut client, &layout_uri, "vyx", CONTRACT_LAYOUT);

    // 0-based line 4: the blank line after the head block.
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

/// A contract member is a declaration, and `export fn` is invalid in a body.
#[test]
fn completion_does_not_fire_inside_a_body() {
    let dir = contract_scratch("body");
    let mut client = spawn_client();
    let page_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        CONTRACT_APP,
    );
    did_open(&mut client, &page_uri, "vyx", CONTRACT_PAGE);

    // 0-based line 3 is `    return noHead()`.
    let items = completion_items(&mut client, &page_uri, 3, 4);
    assert!(
        items.iter().all(|i| i.get("insertText").is_none()),
        "no declaration snippets inside a body: {items:?}"
    );
    let _ = client.child.kill();
}

#[test]
fn hover_names_the_contract() {
    let dir = contract_scratch("hover");
    let mut client = spawn_client();
    let page_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        CONTRACT_APP,
    );
    did_open(&mut client, &page_uri, "vyx", CONTRACT_PAGE);

    // 0-based line 2 is `export fn head() -> Head {`; `head` starts at char 10.
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

/// Lands on the member in `std/ui.vyrn`, not the file top.
#[test]
fn definition_jumps_into_the_contract() {
    let dir = contract_scratch("def");
    let mut client = spawn_client();
    let page_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        CONTRACT_APP,
    );
    did_open(&mut client, &page_uri, "vyx", CONTRACT_PAGE);

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

/// The edit replaces exactly the misspelled name.
#[test]
fn code_action_renames_a_near_miss() {
    let dir = contract_scratch("fix");
    let mut client = spawn_client();
    let typo_uri = file_uri(&dir.join("routes/typo.vyrn"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        CONTRACT_APP,
    );
    did_open(&mut client, &typo_uri, "vyrn", CONTRACT_TYPO_PAGE);

    // 0-based line 2 is `export fn dta() -> Query<Int64> {`; `dta` is 10-13.
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

    assert!(code_actions(&mut client, &typo_uri, 5, 0, 0).is_empty());
    let _ = client.child.kill();
}

/// Completion offers the members at module scope; hover does not call a
/// non-member a member.
#[test]
fn a_vyrn_page_completes_without_misfiring() {
    let dir = contract_scratch("vyrnpage");
    let mut client = spawn_client();
    let typo_uri = file_uri(&dir.join("routes/typo.vyrn"));
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        CONTRACT_APP,
    );
    did_open(&mut client, &typo_uri, "vyrn", CONTRACT_TYPO_PAGE);

    // 0-based line 1: the blank line after the import.
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
    let h = hover_value(&mut client, &typo_uri, 2, 11).unwrap_or_default();
    assert!(
        !h.contains("member of contract"),
        "`dta` is not a member: {h}"
    );
    let _ = client.child.kill();
}

#[test]
fn a_module_outside_every_role_is_untouched() {
    let dir = contract_scratch("outside");
    let store = dir.join("store.vyrn");
    let src = "fn helper() -> Int64 {\n    return 1\n}\n\n";
    std::fs::write(&store, src).unwrap();
    let mut client = spawn_client();
    let uri = file_uri(&store);
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        CONTRACT_APP,
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

/// The `vyrn.json` `roles` key names a directory no generator call site names.
#[test]
fn manifest_roles_override_discovery() {
    let dir = contract_scratch("roles");
    std::fs::create_dir_all(dir.join("screens")).unwrap();
    std::fs::write(
        dir.join("vyrn.json"),
        "{ \"name\": \"m4\", \"main\": \"app.vyrn\",\n  \"roles\": { \"screens\": \"std/ui:Page\" } }\n",
    )
    .unwrap();
    let src = "import { Query } from \"std/ui\"\n\n";
    let screen = dir.join("screens/home.vyrn");
    std::fs::write(&screen, src).unwrap();

    let mut client = spawn_client();
    let uri = file_uri(&screen);
    did_open(
        &mut client,
        &file_uri(&dir.join("app.vyrn")),
        "vyrn",
        CONTRACT_APP,
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

    // An explicit `roles` key is the whole mapping, so discovery's `routes/`
    // is outside every role.
    let page_uri = file_uri(&dir.join("routes/index.vyx"));
    did_open(&mut client, &page_uri, "vyx", CONTRACT_PAGE);
    let items = completion_items(&mut client, &page_uri, 5, 0);
    assert!(
        items.iter().all(|i| i.get("insertText").is_none()),
        "declared roles replace discovery entirely: {items:?}"
    );
    let _ = client.child.kill();
}

/// Completion, hover, definition and quick-fix for a contract the app declares
/// itself: the server names no std contract.
#[test]
fn a_user_authored_contract_gets_the_same_editor_support() {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
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
    // `titel` is one transposition from `title`.
    let screen = "export fn titel() -> String {\n    return \"home\"\n}\n\n";

    std::fs::write(
        dir.join("vyrn.json"),
        "{ \"name\": \"u\", \"main\": \"app.vyrn\" }\n",
    )
    .unwrap();
    std::fs::write(dir.join("gen.vyrn"), gen).unwrap();
    std::fs::write(dir.join("app.vyrn"), app).unwrap();
    std::fs::write(dir.join("screens/home.vyrn"), screen).unwrap();

    let mut client = spawn_client();
    let uri = file_uri(&dir.join("screens/home.vyrn"));
    did_open(&mut client, &file_uri(&dir.join("app.vyrn")), "vyrn", app);
    did_open(&mut client, &uri, "vyrn", screen);

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
    // `fn title()` is on 1-based line 4 of gen.vyrn.
    assert_eq!(
        loc.pointer("/range/start/line").and_then(|l| l.as_i64()),
        Some(3),
        "{loc}"
    );

    let _ = client.child.kill();
}

/// The server loads with the project's declared audiences, so the editor shows
/// the compiler's message as the import is typed.
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

/// The `impl` repeats no doc, so a call through it must show the protocol's.
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
    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", PROTOCOL_DOC_SRC);

    let (l, c) = pos_after(PROTOCOL_DOC_SRC, "fn cacheFor");
    let hover = hover_value(&mut client, &uri, l, c - 1).expect("hover on the signature");
    assert!(
        hover.contains("max-age=N"),
        "signature hover carries the doc: {hover}"
    );

    let (l, c) = pos_after(PROTOCOL_DOC_SRC, "r.cacheFor");
    let hover = hover_value(&mut client, &uri, l, c - 1).expect("hover on the call");
    assert!(
        hover.contains("max-age=N"),
        "call-site hover carries the doc: {hover}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// A generated stub has no source file and no doc of its own; the symbol map a
// generator bakes in supplies both, and the derived route.

const RPC_WIRE: &str = "/// One note, as it crosses the wire.
export type Note = { text: String }
export type NoteReq = { id: Int64 }
";

const RPC_API: &str = "import { Note, NoteReq } from \"../../shared/wire\"

/// Fetch one note by id. The doc a generated stub has to borrow.
export fn byId(req: NoteReq) -> Note {
    return Note { text: req.id.toString() }
}
";

const RPC_BOOT: &str = "import { client } from \"std/rpc\"
import * as api from client(\"./server/api\")

fn onNote(r: api.RpcReply<api.Note>) {
}

fn main() -> Int64 {
    api.notesById(api.NoteReq { id: 1 }, onNote)
    return 0
}
";

/// Writes a wire module, one api procedure, and a client root that generates
/// stubs over the api directory.
fn rpc_scratch() -> std::path::PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
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
    std::fs::write(dir.join("shared/wire.vyrn"), RPC_WIRE).unwrap();
    std::fs::write(dir.join("server/api/notes.vyrn"), RPC_API).unwrap();
    std::fs::write(dir.join("boot.vyrn"), RPC_BOOT).unwrap();
    dir
}

#[test]
fn a_generated_stub_borrows_its_declarations_doc_and_shows_its_derived_route() {
    let dir = rpc_scratch();
    let uri = file_uri(&dir.join("boot.vyrn"));
    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", RPC_BOOT);

    let (l, c) = pos_after(RPC_BOOT, "api.notesById");
    let hover = hover_value(&mut client, &uri, l, c - 1).expect("hover on the generated stub");
    assert!(hover.contains("fn notesById(req: NoteReq"), "{hover}");
    assert!(
        hover.contains("Fetch one note by id"),
        "the origin's doc is borrowed: {hover}"
    );
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
    let dir = rpc_scratch();
    let uri = file_uri(&dir.join("boot.vyrn"));
    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", RPC_BOOT);

    let (l, c) = pos_after(RPC_BOOT, "api.notesById");
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
    // `byId` is at 1-based line 4, column 11. Only the map knows the column;
    // the AST carries a line alone.
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

    // A re-emitted type has lost its file in the generated source; only the map
    // says it came from `shared/wire.vyrn`.
    let (tl, tc) = pos_after(RPC_BOOT, "api.NoteReq {");
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
    let dir = rpc_scratch();
    let api_uri = file_uri(&dir.join("server/api/notes.vyrn"));
    let mut client = spawn_client();
    // Only the api module is open and it reaches no generator, so the facts
    // come from the ranked probe over the roots that mount a surface.
    did_open(&mut client, &api_uri, "vyrn", RPC_API);

    let (l, c) = pos_after(RPC_API, "export fn byId");
    let hover = hover_value(&mut client, &api_uri, l, c - 1).expect("hover on the declaration");
    assert!(hover.contains("fn byId(req: NoteReq) -> Note"), "{hover}");
    assert!(hover.contains("Fetch one note by id"), "{hover}");
    assert!(
        hover.contains("`POST /_/notes/byId` · convention"),
        "the derived route: {hover}"
    );

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

    let wire_uri = file_uri(&dir.join("shared/wire.vyrn"));
    did_open(&mut client, &wire_uri, "vyrn", RPC_WIRE);
    let (wl, wc) = pos_after(RPC_WIRE, "export type Note ");
    let h = hover_value(&mut client, &wire_uri, wl, wc - 6).unwrap_or_default();
    assert!(
        !h.contains("POST"),
        "a wire type is mounted at nothing: {h}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = client.child.kill();
}

// Rename across the generated boundary: `byId` is `api.notesById` in
// the client, and only the symbol map relates the two. The edit spans sources
// only; a generated module is regenerated from the renamed declaration.

/// Renames at `(line, ch)`; returns `line:char=newText` edits per file URI, or
/// the error message.
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

fn edits_for<'a>(changes: &'a [(String, Vec<String>)], suffix: &str) -> &'a [String] {
    changes
        .iter()
        .find(|(f, _)| f.ends_with(suffix))
        .map(|(_, e)| e.as_slice())
        .unwrap_or_else(|| panic!("no edits for {suffix} in {changes:#?}"))
}

#[test]
fn renaming_a_procedure_follows_the_symbol_map_into_the_generated_call_site() {
    let dir = rpc_scratch();
    let api_uri = file_uri(&dir.join("server/api/notes.vyrn"));
    let mut client = spawn_client();
    did_open(&mut client, &api_uri, "vyrn", RPC_API);

    let (l, c) = pos_after(RPC_API, "export fn byId");

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

    assert_eq!(edits_for(&changes, "server/api/notes.vyrn"), ["3:10=fetch"]);
    // The call site takes the name the generator derives.
    assert_eq!(edits_for(&changes, "boot.vyrn"), ["7:8=notesFetch"]);
    assert_eq!(changes.len(), 2, "sources only: {changes:#?}");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = client.child.kill();
}

#[test]
fn renaming_a_wire_type_follows_both_the_direct_import_and_the_re_emitted_copy() {
    let dir = rpc_scratch();
    let wire_uri = file_uri(&dir.join("shared/wire.vyrn"));
    let mut client = spawn_client();
    did_open(&mut client, &wire_uri, "vyrn", RPC_WIRE);

    let (l, c) = pos_after(RPC_WIRE, "export type Note");
    let (changes, err) = rename_at(&mut client, &wire_uri, l, c - 1, "Memo");
    assert!(err.is_none(), "{err:?}");

    assert_eq!(edits_for(&changes, "shared/wire.vyrn"), ["1:12=Memo"]);
    // The api module imports it directly; `NoteReq` is a different token.
    assert_eq!(
        edits_for(&changes, "server/api/notes.vyrn"),
        ["0:9=Memo", "3:32=Memo", "4:11=Memo"]
    );
    // The client never imports it: `client()` re-emits the declaration, and
    // only the map records where the copy came from.
    assert_eq!(edits_for(&changes, "boot.vyrn"), ["3:30=Memo"]);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = client.child.kill();
}

#[test]
fn rename_refuses_what_it_cannot_carry_through_rather_than_doing_half_of_it() {
    let dir = rpc_scratch();
    let api_uri = file_uri(&dir.join("server/api/notes.vyrn"));
    let boot_uri = file_uri(&dir.join("boot.vyrn"));
    let mut client = spawn_client();
    did_open(&mut client, &api_uri, "vyrn", RPC_API);
    did_open(&mut client, &boot_uri, "vyrn", RPC_BOOT);

    // A non-identifier would break lexing in every file the rename touched.
    let (l, c) = pos_after(RPC_API, "export fn byId");
    let (_, err) = rename_at(&mut client, &api_uri, l, c - 1, "by Id");
    assert!(err.unwrap_or_default().contains("not a valid identifier"));

    // A local binding.
    let (bl, bc) = pos_after(RPC_API, "Note { text: req");
    let (_, err) = rename_at(&mut client, &api_uri, bl, bc - 1, "x");
    assert!(err.unwrap_or_default().contains("no declaration here"));

    // An imported name: renameable, but not from here.
    let (il, ic) = pos_after(RPC_API, "    return Note");
    let (_, err) = rename_at(&mut client, &api_uri, il, ic - 1, "x");
    assert!(err
        .unwrap_or_default()
        .contains("not declared in this file"));

    // A generated name: rename the declaration it stands for instead.
    let (gl, gc) = pos_after(RPC_BOOT, "api.notesById");
    let (_, err) = rename_at(&mut client, &boot_uri, gl, gc - 1, "x");
    assert!(
        err.is_some(),
        "a generated symbol is not renameable in place"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = client.child.kill();
}

/// In `examples/bin`, three generators in three roots map `create`: `client()`
/// as `pastesCreate`, `rpc()` as `rpcHandlePastesCreate`, `http()` as `create`.
/// Asserted by names and files, not line numbers, so editing the example does
/// not break the test.
#[test]
fn renaming_a_procedure_in_the_corpus_reaches_every_generated_call_site() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/bin");
    let api = root.join("server/api/pastes.vyrn");
    let text = std::fs::read_to_string(&api).expect("examples/bin/server/api/pastes.vyrn");
    let uri = file_uri(&api);
    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", &text);

    let (l, c) = pos_after(&text, "export mut fn create");
    let (changes, err) = rename_at(&mut client, &uri, l, c - 1, "add");
    assert!(err.is_none(), "{err:?}");

    let decl = format!("{l}:{}=add", c - "create".len() as u32);
    assert_eq!(edits_for(&changes, "server/api/pastes.vyrn"), [decl]);
    let boot: Vec<&str> = edits_for(&changes, "client/boot.vyrn")
        .iter()
        .map(|s| s.as_str())
        .collect();
    assert_eq!(boot.len(), 1, "{boot:?}");
    assert!(
        boot[0].ends_with("=pastesAdd"),
        "the derived stub name moves too: {boot:?}"
    );
    // `http()`'s re-export: the import list and `POST(create("/"))`.
    let http: Vec<&str> = edits_for(&changes, "pastes.http.vyrn")
        .iter()
        .map(|s| s.as_str())
        .collect();
    assert_eq!(http.len(), 2, "{http:?}");
    assert!(http.iter().all(|e| e.ends_with("=add")), "{http:?}");
    assert_eq!(changes.len(), 3, "{changes:#?}");

    let _ = client.child.kill();
}

/// A page imports `recent` directly, in a `<script>` body whose lines are
/// offset from the file's.
#[test]
fn renaming_a_procedure_in_the_corpus_reaches_a_pages_script_body() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/bin");
    let api = root.join("server/api/pastes.vyrn");
    let text = std::fs::read_to_string(&api).expect("examples/bin/server/api/pastes.vyrn");
    let uri = file_uri(&api);
    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", &text);

    let (l, c) = pos_after(&text, "export fn recent");
    let (changes, err) = rename_at(&mut client, &uri, l, c - 1, "latest");
    assert!(err.is_none(), "{err:?}");

    let page: Vec<&str> = edits_for(&changes, "app/routes/index.vyx")
        .iter()
        .map(|s| s.as_str())
        .collect();
    // The import and the one call; `recentRows` and comment prose do not move.
    assert_eq!(page.len(), 2, "{page:?}");
    assert!(page.iter().all(|e| e.ends_with("=latest")), "{page:?}");
    // The import is on 1-based line 15 of the file.
    assert!(
        page[0].starts_with("14:"),
        "script-body lines map back to the file: {page:?}"
    );

    let _ = client.child.kill();
}

/// An `Iterate` impl whose `nth` projects an element, and a declared `Copy`.
const CONTAINER_SRC: &str = "type Ring = { data: Array<Int64> }

impl Iterate for Ring {
    fn size(self) -> Int64 {
        return self.data.length
    }
    fn nth(read self, i: Int64) -> read Int64 {
        return self.data[i]
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
    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", CONTAINER_SRC);

    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif["params"]["diagnostics"].as_array().unwrap();
    assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");

    let (l, c) = pos_after(CONTAINER_SRC, "for x");
    let hover = hover_value(&mut client, &uri, l, c - 1).expect("hover on the loop variable");
    assert!(
        hover.contains("Int64"),
        "the loop variable hovers as its element type: {hover}"
    );

    let (l, c) = pos_after(CONTAINER_SRC, "let q");
    let hover = hover_value(&mut client, &uri, l, c - 1).expect("hover on the copy");
    assert!(
        hover.contains("Ring"),
        "a declared copy hovers as its type: {hover}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Driven over the wire for the wire shapes: the modifier bit must match the
/// advertised legend, and an inlay hint must arrive at a 0-based position.
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

    // `a` in `let a = ..`.
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

    // `b` lives to block exit.
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
    // Type hints share the request; a move hint's label starts with an arrow.
    let moves: Vec<&serde_json::Value> = list
        .iter()
        .filter(|h| h["label"].as_str().is_some_and(|l| l.starts_with('→')))
        .collect();
    assert_eq!(moves.len(), 1, "one move, one hint: {list:?}");
    assert_eq!(
        moves[0].pointer("/position/line").unwrap().as_i64(),
        Some(3)
    );
    assert_eq!(moves[0].get("label").unwrap().as_str(), Some("→ take(..)"));

    // The last use carries the `modification` modifier, bit 3. Tokens are
    // delta-encoded `[dline, dstart, len, type, mods]`.
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
    // Only the `a` handed to `take`: a declaration is never a last use, and
    // `b` has none.
    assert_eq!(marked, vec![3], "marked last uses: {marked:?}");
}

/// The method-form call resolves to the container's own export, not the `Map`
/// builtin, while a `Map` receiver elsewhere keeps the builtin.
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

    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", app);
    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif["params"]["diagnostics"].as_array().unwrap();
    assert!(
        diags.is_empty(),
        "method-form `remove` on a container: {diags:?}"
    );

    let (l, c) = at(app, 5, "b.remove");
    let target = definition_target(&mut client, &uri, l, c + 2).expect("definition on b.remove");
    assert!(
        target.ends_with("/bag.vyrn"),
        "b.remove → bag.vyrn: {target}"
    );

    // A module that does not import a `remove` keeps the builtin.
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

    // Scope decides, not the receiver's type, so one module cannot mean both:
    // the call is refused and names the type it wanted.
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

/// Every inlay hint for the whole document, as `(line, character, label)`.
fn inlay_hints(client: &mut LspClient, uri: &str) -> Vec<(u32, u32, String)> {
    let id = serde_json::json!(format!("inlay-{uri}"));
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/inlayHint",
        "params": {
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 100000, "character": 0 }
            }
        }
    }));
    let resp = client.read_response(&id);
    resp.get("result")
        .and_then(|r| r.as_array())
        .expect("an inlay hint list")
        .iter()
        .map(|h| {
            (
                h.pointer("/position/line").unwrap().as_u64().unwrap() as u32,
                h.pointer("/position/character").unwrap().as_u64().unwrap() as u32,
                h["label"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The type hints only; a type hint's label starts with `: `.
fn type_hints(client: &mut LspClient, uri: &str) -> Vec<(u32, u32, String)> {
    inlay_hints(client, uri)
        .into_iter()
        .filter(|(_, _, l)| l.starts_with(": "))
        .collect()
}

/// Over `examples/copy.vyrn`: `let p = o.copy()` hides `p`'s type, while the
/// record literal above names `Outer`.
#[test]
fn a_binding_gets_a_type_hint_when_its_line_hides_the_type() {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/copy.vyrn"
    ));
    let src = std::fs::read_to_string(path).expect("examples/copy.vyrn should exist");
    let uri = file_uri(path);
    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", &src);
    let diags = read_diags_for(&mut client, "copy.vyrn");
    let list = diags["params"]["diagnostics"].as_array().unwrap();
    assert!(list.is_empty(), "the example is clean: {list:?}");

    let hints = type_hints(&mut client, &uri);

    let (pline, pcol) = pos_after(&src, "    let p");
    let shown: Vec<&(u32, u32, String)> = hints.iter().filter(|(l, _, _)| *l == pline).collect();
    assert_eq!(
        shown.len(),
        1,
        "one hint on the copy line, at {pline}: {shown:?}"
    );
    assert_eq!(shown[0].2, ": Outer", "the copy takes the receiver's type");
    assert_eq!(shown[0].1, pcol, "the hint sits after the name `p`");

    let (oline, _) = pos_after(&src, "    let mut o = Outer {");
    let on_o: Vec<&(u32, u32, String)> = hints.iter().filter(|(l, _, _)| *l == oline).collect();
    assert!(on_o.is_empty(), "a record literal names its type: {on_o:?}");

    let _ = client.child.kill();
}

/// Both read the same row of the cached analysis.
#[test]
fn a_type_hint_agrees_with_the_hover_at_the_same_binding() {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/copy.vyrn"
    ));
    let src = std::fs::read_to_string(path).expect("examples/copy.vyrn should exist");
    let uri = file_uri(path);
    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", &src);
    let _ = read_diags_for(&mut client, "copy.vyrn");

    let hints = type_hints(&mut client, &uri);
    assert!(hints.len() > 8, "the file binds plenty: {}", hints.len());
    for (line, ch, label) in &hints {
        // The hint is drawn just past the name; hover wants a column inside it.
        let hover = hover_value(&mut client, &uri, *line, ch - 1)
            .unwrap_or_else(|| panic!("hover at the binding on line {line}"));
        let sig = hover
            .lines()
            .find(|l| l.starts_with("let ") || l.starts_with("for "))
            .unwrap_or_else(|| panic!("a binding signature in: {hover}"));
        let ty = sig
            .split_once(": ")
            .unwrap_or_else(|| panic!("a type in the signature: {sig}"))
            .1;
        assert_eq!(
            format!(": {ty}"),
            *label,
            "hint and hover disagree on line {line}"
        );
    }
    let _ = client.child.kill();
}

/// An annotation, a record literal and a literal value name the type on the
/// line; only the call earns a hint.
#[test]
fn a_line_that_already_names_the_type_gets_no_hint() {
    let dir = std::env::temp_dir().join(format!("vyrn-lsp-inlay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = "\
type Point = { x: Int64, y: Int64 }
fn made() -> Point { return Point { x: 1, y: 2 } }
fn main() -> Int64 {
    let annotated: Int64 = 7
    let record = Point { x: 3, y: 4 }
    let number = 41
    let text = \"hello\"
    let flag = true
    let list = [1, 2, 3]
    let called = made()
    print(text)
    if flag { print(\"y\") }
    return annotated + record.x + number + list[0] + called.y
}
";
    let path = dir.join("hints.vyrn");
    std::fs::write(&path, src).unwrap();
    let uri = file_uri(&path);
    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", src);
    let diags = read_diags_for(&mut client, "hints.vyrn");
    let list = diags["params"]["diagnostics"].as_array().unwrap();
    assert!(list.is_empty(), "the fixture is clean: {list:?}");

    let hints = type_hints(&mut client, &uri);
    let labels: Vec<&str> = hints.iter().map(|(_, _, l)| l.as_str()).collect();
    assert_eq!(
        labels,
        vec![": Point"],
        "only the call hides its type: {hints:?}"
    );
    let (cline, _) = pos_after(src, "    let called");
    assert_eq!(hints[0].0, cline, "the hint is on the call line");

    let _ = client.child.kill();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The `<script>` binds by a call, an annotation and a literal; only the call
/// hides its type.
const VYX_HINTS: &str = "<script>\n\
    type Row = { title: String }\n\
    props { item: Row, words: Array<String> }\n\
    fn made() -> Row {\n\
    return Row { title: \"t\" }\n\
    }\n\
    fn label(r: Row) -> String {\n\
    let built = made()\n\
    let named: String = \"x\"\n\
    let plain = \"y\"\n\
    return built.title + named + plain + r.title\n\
    }\n\
    </script>\n\
    <template>\n\
    <ul>\n\
    <li>{{ label(item) }}</li>\n\
    <li v-for=\"w in words\" :key=\"w\">{{ w }}</li>\n\
    </ul>\n\
    </template>\n";

/// The script line is copied verbatim into the synthesized module, so the hint
/// maps back column-exactly.
#[test]
fn a_vyx_script_binding_gets_a_type_hint_at_its_own_position() {
    let dir = vyx_scratch("inlay", VYX_HINTS);
    let mut client = spawn_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &app_uri, "vyrn", VYX_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx");
    did_open(&mut client, &vyx_uri, "vyx", VYX_HINTS);

    let hints = type_hints(&mut client, &vyx_uri);
    assert_eq!(
        hints.len(),
        1,
        "only the call hides its type in the script: {hints:?}"
    );
    let (bline, bcol) = pos_after(VYX_HINTS, "let built");
    assert_eq!(hints[0].0, bline, "the hint is on the `built` line");
    assert_eq!(hints[0].1, bcol, "the hint sits just past the name");
    assert_eq!(hints[0].2, ": Row", "the call returns the prop's type");

    // Template lines hold no binding of the author's. The generated module
    // binds on the `v-for` line under an invented name, which must not show.
    let (tline, _) = pos_after(VYX_HINTS, "<li>{{ label(item) }}");
    let (fline, _) = pos_after(VYX_HINTS, "<li v-for=\"w in words\"");
    assert!(
        !hints.iter().any(|(l, _, _)| *l == tline || *l == fline),
        "no hint in the template: {hints:?}"
    );

    let _ = client.child.kill();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_vyx_type_hint_agrees_with_the_hover_at_the_same_binding() {
    let dir = vyx_scratch("inlayhover", VYX_HINTS);
    let mut client = spawn_client();
    let app_uri = file_uri(&dir.join("app.vyrn"));
    let vyx_uri = file_uri(&dir.join("comp/Widget.vyx"));
    did_open(&mut client, &app_uri, "vyrn", VYX_APP);
    let _ = read_diags_for(&mut client, "Widget.vyx");
    did_open(&mut client, &vyx_uri, "vyx", VYX_HINTS);

    let hints = type_hints(&mut client, &vyx_uri);
    assert!(!hints.is_empty(), "the script binds something");
    for (line, ch, label) in &hints {
        let hover = hover_value(&mut client, &vyx_uri, *line, ch - 1)
            .unwrap_or_else(|| panic!("hover at the binding on line {line}"));
        let sig = hover
            .lines()
            .find(|l| l.starts_with("let ") || l.starts_with("for "))
            .unwrap_or_else(|| panic!("a binding signature in: {hover}"));
        let ty = sig
            .split_once(": ")
            .unwrap_or_else(|| panic!("a type in the signature: {sig}"))
            .1;
        assert_eq!(
            format!(": {ty}"),
            *label,
            "hint and hover disagree on line {line}"
        );
    }

    let _ = client.child.kill();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The editor reads a pinned module through the build's reader, so it refuses
/// a tampered vendored blob and a specifier pinned twice.
#[test]
fn a_pin_the_build_refuses_is_refused_in_the_editor() {
    let root = std::env::temp_dir().join(format!("vyrn-lsp-pin-{}", std::process::id()));
    let spec = "https://x.invalid/dep.vyrn";
    let good = "export fn dep() -> Int64 { return 1 }\n";
    let sha = vyrn_frontend::hash::sha256_hex(good.as_bytes());
    let app = format!("import {{ dep }} from \"{spec}\"\nfn main() -> Int64 {{ return dep() }}\n");

    // The blob under the pinned name holds other bytes.
    let dir = root.join("tampered");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("vyrn_vendor/sha256")).unwrap();
    std::fs::write(dir.join("vyrn.json"), "{}").unwrap();
    std::fs::write(dir.join("vyrn.lock"), format!("{spec}\t{spec}\t{sha}\n")).unwrap();
    std::fs::write(
        dir.join("vyrn_vendor/sha256").join(&sha),
        "export fn dep() -> Int64 { return 99 }\n",
    )
    .unwrap();
    let app_path = dir.join("app.vyrn");
    std::fs::write(&app_path, &app).unwrap();

    let mut client = spawn_client();
    did_open(&mut client, &file_uri(&app_path), "vyrn", &app);
    let text = read_diags_for(&mut client, "app.vyrn").to_string();
    assert!(
        text.contains("does not match its recorded sha256"),
        "a tampered vendored blob must be refused, not analyzed: {text}"
    );
    let _ = client.child.kill();

    // One specifier, two pins: what a merge that appends produces.
    let dir = root.join("dupe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("vyrn_vendor/sha256")).unwrap();
    std::fs::write(dir.join("vyrn.json"), "{}").unwrap();
    std::fs::write(
        dir.join("vyrn.lock"),
        format!("{spec}\t{spec}\t{sha}\n{spec}\t{spec}\tdeadbeef\n"),
    )
    .unwrap();
    std::fs::write(dir.join("vyrn_vendor/sha256").join(&sha), good).unwrap();
    let app_path = dir.join("app.vyrn");
    std::fs::write(&app_path, &app).unwrap();

    let mut client = spawn_client();
    did_open(&mut client, &file_uri(&app_path), "vyrn", &app);
    let text = read_diags_for(&mut client, "app.vyrn").to_string();
    assert!(
        text.contains("pinned twice"),
        "a lock the build refuses must not analyze clean: {text}"
    );
    let _ = client.child.kill();
}

/// A rule the kernel (`vyrn-lower`) states alone reaches the editor in the
/// words `vyrn check` uses; a sentence in other words is a second rule. The
/// witness is row 12 of the refusals census.
#[test]
fn a_rule_that_left_the_checker_is_still_shown_in_the_editor() {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../vyrn-cli/tests/unlicensed/u12_module_state_to_a_consume_parameter_heapless.vyrn"
    ));
    let src = std::fs::read_to_string(path).expect("row 12's program should exist");

    let mut client = spawn_client();
    did_open(&mut client, &file_uri(&path), "vyrn", &src);
    let diags = read_diags_for(&mut client, "u12_module_state");
    let list = diags["params"]["diagnostics"].as_array().unwrap();
    let said: Vec<&str> = list.iter().filter_map(|d| d["message"].as_str()).collect();
    assert_eq!(
        said,
        vec![
            "module state `g` may not be passed to a `consume` parameter via `take(..)` \
             — nothing may take ownership of module state (it lives for the whole module \
             and is never dropped)"
        ],
        "the editor says what `vyrn check` says"
    );
    assert_eq!(list[0]["range"]["start"]["line"].as_u64(), Some(10));
    let _ = client.child.kill();
}

/// Writes `files` under a new scratch directory and returns it.
fn scratch_project(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

/// Returns each diagnostic of a publishDiagnostics note as its 0-based line
/// and the first line of its message, the sentence `vyrn check` prints.
fn said(notif: &serde_json::Value) -> Vec<(u64, String)> {
    notif["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            let line = d["range"]["start"]["line"].as_u64().unwrap();
            let message = d["message"].as_str().unwrap();
            (line, message.lines().next().unwrap().to_string())
        })
        .collect()
}

/// Writes `files` under a scratch directory, opens `root` in a new server, and
/// returns what it publishes for `root` ([`said`]).
fn project_diagnostics(tag: &str, files: &[(&str, &str)], root: &str) -> Vec<(u64, String)> {
    let dir = scratch_project(tag, files);
    let root_path = dir.join(root);
    let text = std::fs::read_to_string(&root_path).unwrap();
    let mut client = spawn_client();
    did_open(&mut client, &file_uri(&root_path), "vyrn", &text);
    let notif = read_diags_for(&mut client, root.rsplit('/').next().unwrap());
    let _ = client.child.kill();
    let _ = std::fs::remove_dir_all(&dir);
    said(&notif)
}

/// A `derive` generator `bad` whose every function returns `ret`, declared
/// `-> String`.
fn derive_gen(ret: &str) -> String {
    format!(
        "export gen fn bad(t: TypeArg) -> String {{\n    let mut out = \"\"\n    \
         for r in t.roots {{\n        let n = t.nodes[r]\n        \
         out = out + \"fn \" + n.name + \"(v: \" + n.spelling + \") -> String {{\n    \
         return {ret}\n}}\n\"\n    }}\n    return out\n}}\n"
    )
}

const DERIVE_MAIN: &str = "import { bad } from \"./gen\"\n\ntype P = { x: Int64 }\n\n\
                           fn main() -> Int64 {\n    print(derive(bad, P { x: 1 }))\n    \
                           return 0\n}\n";

/// A function a `derive` generator writes is checked as `vyrn check` checks
/// it. The error is in no file of the project, so it shows at line 0.
#[test]
fn the_editor_checks_what_a_derive_writes() {
    let said = project_diagnostics(
        "derive",
        &[("gen.vyrn", &derive_gen("1")), ("main.vyrn", DERIVE_MAIN)],
        "main.vyrn",
    );
    assert_eq!(
        said,
        vec![(
            0,
            "in generated by derive(bad, ..): return type mismatch: expected String, found Int64"
                .to_string()
        )]
    );
}

/// The server keeps what a `derive` generator wrote for the next edit. After
/// the generator's file changes on disk, an edit of the root shows what a new
/// server shows, so the kept output was not served for the changed program.
#[test]
fn an_edit_after_the_generator_changes_derives_again() {
    let dir = scratch_project(
        "rederive",
        &[("gen.vyrn", &derive_gen("1")), ("main.vyrn", DERIVE_MAIN)],
    );
    let uri = file_uri(&dir.join("main.vyrn"));
    let edited = format!("{DERIVE_MAIN}// edit\n");
    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", DERIVE_MAIN);
    let before = said(&read_diags_for(&mut client, "main.vyrn"));
    assert_eq!(
        before.len(),
        1,
        "the first generator is refused: {before:?}"
    );
    std::fs::write(dir.join("gen.vyrn"), derive_gen("\\\"v\\\"")).unwrap();
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": edited }]
        }
    }));
    let after = said(&read_diags_for(&mut client, "main.vyrn"));
    let _ = client.child.kill();
    let mut fresh = spawn_client();
    did_open(&mut fresh, &uri, "vyrn", &edited);
    let expected = said(&read_diags_for(&mut fresh, "main.vyrn"));
    let _ = fresh.child.kill();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(after, expected);
    assert!(
        expected.is_empty(),
        "the second generator is accepted: {expected:?}"
    );
}

/// Beside a type error, the typed judgment still refuses the bodies the error
/// does not reach, as `vyrn check` does.
#[test]
fn the_editor_shows_a_typed_refusal_beside_a_type_error() {
    let main = "fn f() -> Int64 {\n    let x = 1\n    x = 2\n    return x\n}\n\n\
                fn main() -> Int64 {\n    return nope()\n}\n";
    let said = project_diagnostics("typed", &[("main.vyrn", main)], "main.vyrn");
    assert_eq!(
        said,
        vec![
            (
                2,
                "cannot assign to `x` (declared without `mut`)".to_string()
            ),
            (7, "call to unknown function `nope`".to_string()),
        ]
    );
}

/// The floor decision the load leaves to the effect judgment is made, so a
/// browser artifact that reads the command line is refused.
#[test]
fn the_editor_makes_the_floor_decision() {
    let manifest = "{ \"name\": \"p\", \"artifacts\": { \"app\": \
                    { \"entry\": \"client/boot.vyrn\", \"target\": \"browser\" } } }\n";
    let boot = "fn main() -> Int64 {\n    let a = args()\n    print(a[0])\n    return 0\n}\n";
    let said = project_diagnostics(
        "floor",
        &[("vyrn.json", manifest), ("client/boot.vyrn", boot)],
        "client/boot.vyrn",
    );
    assert_eq!(
        said,
        vec![(
            1,
            "artifact `app` (browser) cannot include `client/boot.vyrn`: it reads the \
             command line"
                .to_string()
        )]
    );
}

/// The loader renames a type two modules declare (`Cfg` becomes `Cfg__from0`).
/// Hover, type hints and completion details name it as its module wrote it.
#[test]
fn hover_and_hints_name_a_renamed_type_as_its_module_wrote_it() {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_names_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("cfg.vyrn"),
        "export type Cfg = { m: String }\n\
         export type Wrap = { inner: Cfg }\n\
         export fn make() -> Cfg { return Cfg { m: \"a\" } }\n\
         export fn read(c: Cfg) -> Int64 { return 1 }\n\
         export fn wrap() -> Wrap { return Wrap { inner: make() } }\n",
    )
    .unwrap();
    let root = "import { make, read, wrap, Wrap } from \"./cfg.vyrn\"\n\
                type Cfg = { n: Int64 }\n\
                fn main() -> Int64 {\n    let c = make()\n    let d = read(c)\n    let mine = Cfg { n: 1 }\n    let w = wrap()\n    let z = w.inner\n    print(d)\n    return mine.n\n}\n";
    let path = dir.join("main.vyrn");
    std::fs::write(&path, root).unwrap();
    let uri = file_uri(&path);
    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", root);
    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif["params"]["diagnostics"].as_array().unwrap();
    assert!(diags.is_empty(), "{notif}");

    let make = hover_value(&mut client, &uri, 3, 13).expect("hover on make");
    let read = hover_value(&mut client, &uri, 4, 13).expect("hover on read");
    let c = hover_value(&mut client, &uri, 3, 8).expect("hover on c");
    let wrap = hover_value(&mut client, &uri, 0, 28).expect("hover on the imported type");
    let field = hover_value(&mut client, &uri, 7, 16).expect("hover on the field");
    let hints: Vec<String> = type_hints(&mut client, &uri)
        .into_iter()
        .map(|(_, _, l)| l)
        .collect();
    let details = completion_details(&mut client, &uri, 7, 14);
    let shown = hints.iter().chain([&make, &read, &c, &wrap, &field]);
    for text in shown.chain(details.iter().map(|(_, d)| d)) {
        assert!(!text.contains("__from"), "a linked name shows: {text}");
    }
    assert!(make.contains("fn make() -> Cfg"), "hover: {make}");
    assert!(read.contains("fn read(c: Cfg) -> Int64"), "hover: {read}");
    assert!(c.contains("let c: Cfg"), "hover: {c}");
    assert!(wrap.contains("type Wrap = { inner: Cfg }"), "hover: {wrap}");
    assert!(field.contains("inner: Cfg"), "hover: {field}");
    assert_eq!(
        hints.iter().filter(|h| *h == ": Cfg").count(),
        2,
        "{hints:?}"
    );
    assert!(
        details.contains(&("inner".to_string(), "inner: Cfg".to_string())),
        "completion: {details:?}"
    );

    let _ = client.child.kill();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The loader gives every declaration and variant of a runtime module a reserved
/// spelling (`json$Json`, `json$JNull`). Hover and hints name them as `std/json`
/// wrote them.
#[test]
fn hover_and_hints_name_a_runtime_module_type_as_its_module_wrote_it() {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_rt_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let root = "import { Json } from \"std/json\"\n\
                fn main() -> Int64 {\n    let j: Json = JNull\n    let k = j\n    return 0\n}\n";
    let path = dir.join("main.vyrn");
    std::fs::write(&path, root).unwrap();
    let uri = file_uri(&path);
    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", root);
    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif["params"]["diagnostics"].as_array().unwrap();
    assert!(diags.is_empty(), "{notif}");

    let k = hover_value(&mut client, &uri, 3, 8).expect("hover on k");
    let hints: Vec<String> = type_hints(&mut client, &uri)
        .into_iter()
        .map(|(_, _, l)| l)
        .collect();
    for text in hints.iter().chain([&k]) {
        assert!(!text.contains('$'), "a reserved name shows: {text}");
    }
    assert!(k.contains("let k: Json"), "hover: {k}");
    assert_eq!(hints, [": Json"], "{hints:?}");

    let _ = client.child.kill();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `(label, detail)` of every completion item at the position.
fn completion_details(
    client: &mut LspClient,
    uri: &str,
    line: u32,
    ch: u32,
) -> Vec<(String, String)> {
    let id = serde_json::json!(format!("cd{line}_{ch}"));
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/completion",
        "params": { "textDocument": { "uri": uri }, "position": { "line": line, "character": ch } }
    }));
    let resp = client.read_response(&id);
    let items = resp["result"].as_array().cloned().unwrap_or_default();
    items
        .iter()
        .map(|i| {
            let s = |k: &str| i[k].as_str().unwrap_or_default().to_string();
            (s("label"), s("detail"))
        })
        .collect()
}

/// A hover that shows two declarations of one spelling names each with the
/// module it comes from, as a refusal does.
#[test]
fn hover_tells_two_types_of_one_spelling_apart() {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_lsp_pair_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("b.vyrn"), "export type Cfg = { m: String }\n").unwrap();
    std::fs::write(
        dir.join("lib.vyrn"),
        "import { Cfg as Remote } from \"./b.vyrn\"\n\
         export type Cfg = { n: Int64 }\n\
         export fn pair(a: Cfg, b: Remote) -> Int64 { return a.n }\n",
    )
    .unwrap();
    let root = "import { pair } from \"./lib.vyrn\"\nfn main() -> Int64 { return 0 }\n";
    let path = dir.join("main.vyrn");
    std::fs::write(&path, root).unwrap();
    let uri = file_uri(&path);
    let mut client = spawn_client();
    did_open(&mut client, &uri, "vyrn", root);
    let notif = client.read_notification("textDocument/publishDiagnostics");
    let diags = notif["params"]["diagnostics"].as_array().unwrap();
    assert!(diags.is_empty(), "{notif}");

    let pair = hover_value(&mut client, &uri, 0, 10).expect("hover on pair");
    assert!(!pair.contains("__from"), "a linked name shows: {pair}");
    assert!(
        pair.contains("fn pair(a: Cfg from \"./lib.vyrn\", b: Cfg from \""),
        "hover: {pair}"
    );

    let _ = client.child.kill();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A client that sends file events is asked for them under its workspace
/// folder, and the server reads an edit on disk once the event names the file.
/// A client that cannot send them has every edit on disk read.
#[test]
fn a_disk_edit_is_read_after_its_event_or_always_without_events() {
    for events in [true, false] {
        let dir =
            std::env::temp_dir().join(format!("vyrn-lsp-watch-{events}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let slash = |p: &std::path::Path| p.to_string_lossy().replace('\\', "/");
        let lib = dir.join("lib.vyrn");
        std::fs::write(
            &lib,
            "export fn double(x: Int64) -> Int64 {\n    return x * 2\n}\n",
        )
        .unwrap();
        let root_text =
            "import { double } from \"./lib\"\n\nfn main() -> Int64 {\n    return double(21)\n}\n";
        let uri = format!("file:///{}", slash(&dir.join("main.vyrn")));
        std::fs::write(dir.join("main.vyrn"), root_text).unwrap();

        let mut client = LspClient::spawn().expect("spawn vyrn-lsp");
        let caps = match events {
            true => serde_json::json!({ "workspace": { "didChangeWatchedFiles": {
                "dynamicRegistration": true, "relativePatternSupport": true
            } } }),
            false => serde_json::json!({}),
        };
        let init_id = serde_json::json!(1);
        client.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": init_id, "method": "initialize",
            "params": { "capabilities": caps, "processId": null, "workspaceFolders": [
                { "uri": format!("file:///{}", slash(&dir)), "name": "w" }
            ] }
        }));
        let _ = client.read_response(&init_id);
        client
            .send(&serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
        if events {
            let register = client.read_notification("client/registerCapability");
            let pattern = &register["params"]["registrations"][0]["registerOptions"]["watchers"][0]
                ["globPattern"];
            assert_eq!(pattern["pattern"], "**/*", "{register}");
            client.send(&serde_json::json!({
                "jsonrpc": "2.0", "id": register["id"], "result": null
            }));
        }
        let mut version = 1;
        let mut analyze = |client: &mut LspClient| {
            let method = if version == 1 { "didOpen" } else { "didChange" };
            client.send(&serde_json::json!({
                "jsonrpc": "2.0", "method": format!("textDocument/{method}"),
                "params": {
                    "textDocument": {
                        "uri": uri.clone(), "languageId": "vyrn", "version": version,
                        "text": root_text
                    },
                    "contentChanges": [ { "text": root_text } ]
                }
            }));
            version += 1;
            let notif = client.read_notification("textDocument/publishDiagnostics");
            notif["params"]["diagnostics"].as_array().unwrap().len()
        };
        assert_eq!(analyze(&mut client), 0, "the program starts clean");

        let text = "export fn double(x: Int64) -> String {\n    return \"a\"\n}\n";
        std::fs::write(&lib, text).unwrap();
        let without_event = analyze(&mut client);
        if events {
            assert_eq!(
                without_event, 0,
                "no event, so the server keeps what it read"
            );
            client.send(&serde_json::json!({
                "jsonrpc": "2.0", "method": "workspace/didChangeWatchedFiles",
                "params": { "changes": [ { "uri": format!("file:///{}", slash(&lib)), "type": 2 } ] }
            }));
            assert_ne!(analyze(&mut client), 0, "the event names the edited file");
        } else {
            assert_ne!(
                without_event, 0,
                "a client without events has the disk read"
            );
        }
        drop(client);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
