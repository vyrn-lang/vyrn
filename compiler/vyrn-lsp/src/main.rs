//! A minimal, synchronous Language Server Protocol server for Vyrn.
//!
//! Design goals (per the project's "easy maintained" constraint):
//!   * No async runtime — a plain blocking `lsp-server` loop on the main thread.
//!   * No duplication of the compiler. The only compiler calls are
//!     [`vyrn_frontend::analyze`] (diagnostics + a symbol index, in one pass) and
//!     the [`vyrn_frontend::resolve`] / [`vyrn_frontend::completions`] /
//!     [`vyrn_frontend::member_completions`] queries over its result. This server
//!     is a pure adapter: text in, LSP diagnostics / hover / go-to-definition /
//!     completion out.
//!   * Hover, go-to-definition, and completion cover top-level functions, types,
//!     and variants; locals/params (with inferred `let` types) for hover + def;
//!     and built-in method calls (`arr.push`, `log.info`) for hover +
//!     `.foo` member completion. The checker now resolves user `protocol`/`impl`
//!     method calls (RFC-0002 §5); surfacing those in hover/`.foo` completion for
//!     protocol-typed receivers is a remaining enhancement to the query layer.
//!
//! Wire format: the server reads Content-Length-framed JSON-RPC messages from
//! stdin and writes them to stdout. Diagnostics are pushed via
//! `textDocument/publishDiagnostics` whenever a document changes; hover/def/
//! completion are answered synchronously to each request.

use std::collections::HashMap;

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    Diagnostic as LspDiagnostic, DiagnosticSeverity, DocumentFormattingParams, DocumentSymbol,
    DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse,
    Hover, HoverContents, HoverParams, InitializeParams, InitializeResult, Location, MarkupContent,
    MarkupKind, OneOf, Position, PublishDiagnosticsParams, Range, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Url,
};

use vyrn_frontend::{analyze, completions, member_completions, resolve, Analysis, SymbolKind};

// ---------------------------------------------------------------------------
// Multi-file analysis (RFC-0010). A document with `import`s is analyzed via
// `analyze_linked`, which resolves the imports through the module loader so
// imported names stop showing as "unknown" in the editor. The resolver below
// is deliberately READ-ONLY and offline: local files come from disk; remote
// modules come from `./vyrn_vendor` or `~/.vyrn/cache` *only if* `vyrn.lock`
// already pins them (the editor never touches the network — fetching and
// pinning stay `vyrn`'s job).
// ---------------------------------------------------------------------------

/// Analyze `text`, linking imports when the document has a real filesystem
/// path (an untitled buffer falls back to single-file [`analyze`]).
fn analyze_doc(uri: &Url, text: &str) -> Analysis {
    let path = match uri.to_file_path() {
        Ok(p) => p.to_string_lossy().replace('\\', "/"),
        Err(()) => return analyze(text),
    };
    let mut opts =
        vyrn_frontend::loader::LoadOptions { std_root: std_root(), ..Default::default() };
    let manifest_dir = std::path::Path::new(&path)
        .parent()
        .and_then(|d| find_manifest(d))
        .map(|(dir, deps)| {
            opts.aliases = deps.into_iter().collect();
            opts.alias_base = dir.clone();
            dir
        });
    let resolver = EditorResolver { manifest_dir };
    vyrn_frontend::analyze_linked(text, &path, &opts, &resolver)
}

/// The std-library root: `$VYRN_STD`, or `std/` found by walking up from the
/// executable (the bundled server lives at `<repo>/editor/vscode/server/`,
/// dev builds under `<repo>/compiler/vyrn-lsp/target/<profile>/` — both are
/// within five levels of the repo's `std/`). Mirrors `vyrn`'s discovery.
fn std_root() -> Option<String> {
    if let Ok(p) = std::env::var("VYRN_STD") {
        if std::path::Path::new(&p).exists() {
            return Some(p.replace('\\', "/"));
        }
    }
    let mut dir = std::env::current_exe().ok()?;
    for _ in 0..5 {
        dir = dir.parent()?.to_path_buf();
        let cand = dir.join("std");
        if cand.is_dir() {
            return Some(cand.to_string_lossy().replace('\\', "/"));
        }
    }
    None
}

/// Find `vyrn.json` by walking up from `start`; returns the manifest's
/// directory (slash-separated) and its `dependencies` import map. A compact
/// duplicate of `vyrn`'s reader (the CLI is a binary crate, not linkable).
fn find_manifest(start: &std::path::Path) -> Option<(String, Vec<(String, String)>)> {
    use vyrn_frontend::schema::Json;
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("vyrn.json");
        if candidate.is_file() {
            let text = std::fs::read_to_string(&candidate).ok()?;
            let doc = vyrn_frontend::schema::parse_json(&text).ok()?;
            let deps = match doc.get("dependencies") {
                Some(Json::Obj(entries)) => entries
                    .iter()
                    .filter_map(|(k, v)| match v {
                        Json::Str(s) => Some((k.clone(), s.clone())),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            return Some((dir.to_string_lossy().replace('\\', "/"), deps));
        }
        dir = dir.parent()?.to_path_buf();
    }
}

/// Read-only module resolver for the editor: local paths from disk; remote
/// specifiers served from the project's `vyrn_vendor/` or the user cache — but
/// only when `vyrn.lock` pins them. Never fetches.
struct EditorResolver {
    /// Directory holding `vyrn.json` (and thus `vyrn.lock` / `vyrn_vendor/`),
    /// if the document is inside a project.
    manifest_dir: Option<String>,
}

impl vyrn_frontend::loader::ModuleResolver for EditorResolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        if !vyrn_frontend::loader::is_remote(resolved) {
            return std::fs::read_to_string(resolved).map_err(|e| e.to_string());
        }
        let dir = self
            .manifest_dir
            .as_deref()
            .ok_or_else(|| "remote import outside a vyrn.json project".to_string())?;
        let lock = std::fs::read_to_string(std::path::Path::new(dir).join("vyrn.lock"))
            .map_err(|_| format!("`{resolved}` is not pinned yet — run `vyrn check` once to fetch it"))?;
        // vyrn.lock is TSV: `specifier ⇥ resolved-url ⇥ sha256`, keyed by the
        // exact specifier string the loader hands us.
        let sha = lock
            .lines()
            .filter_map(|l| {
                let mut parts = l.split('\t');
                Some((parts.next()?, parts.nth(1)?))
            })
            .find(|(spec, _)| *spec == resolved)
            .map(|(_, sha)| sha.to_string())
            .ok_or_else(|| {
                format!("`{resolved}` is not pinned in vyrn.lock — run `vyrn check` once to fetch it")
            })?;
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| ".".to_string());
        for blob_dir in [
            std::path::Path::new(dir).join("vyrn_vendor/sha256"),
            std::path::Path::new(&home).join(".vyrn/cache/sha256"),
        ] {
            if let Ok(text) = std::fs::read_to_string(blob_dir.join(&sha)) {
                return Ok(text);
            }
        }
        Err(format!("`{resolved}` is pinned but not cached — run `vyrn check` once to fetch it"))
    }

    /// Generation-time `listDir` (RFC-0021): read the local directory. The
    /// generator's inputs are local files, so this is a plain read-only listing.
    fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
        let entries = std::fs::read_dir(resolved).map_err(|_| format!("cannot list `{resolved}`"))?;
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        Ok(names)
    }

    /// Participate in the shared generator cache (RFC-0021) so per-keystroke
    /// re-analysis reuses a build's generation instead of re-running it. Same
    /// `~/.vyrn/cache/gen` the CLI writes (honors `VYRN_GEN_CACHE_DIR`).
    fn gen_cache_get(&self, key: &str) -> Option<String> {
        std::fs::read_to_string(gen_cache_dir().join(key)).ok()
    }
    fn gen_cache_put(&self, key: &str, value: &str) {
        let dir = gen_cache_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(key), value);
    }
}

/// The shared generator cache directory (`~/.vyrn/cache/gen`, overridable with
/// `VYRN_GEN_CACHE_DIR`) — kept byte-identical to the CLI's so a build and the
/// editor reuse each other's generation.
fn gen_cache_dir() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("VYRN_GEN_CACHE_DIR") {
        return std::path::PathBuf::from(d);
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    std::path::Path::new(&home).join(".vyrn/cache/gen")
}

fn main() {
    // `Connection::stdio` sets up the stdin/stdout channels. The server is
    // single-threaded and blocking — no tokio, no I/O threads.
    let (connection, io_threads) = Connection::stdio();

    let mut server = Server { docs: HashMap::new(), analyses: HashMap::new() };

    // `initialize` is a special handshake: read it, reply with capabilities,
    // then enter the main loop. EOF here just means the client left.
    if handle_initialize(&connection).is_err() {
        return;
    }

    main_loop(&connection, &mut server);
    // Drop the connection BEFORE joining the I/O threads: the writer thread
    // only exits once its sender (owned by `connection`) is dropped, and
    // `IoThreads::join` joins the writer last. Dropping here releases it so the
    // join can complete (a real client additionally closes the pipe, but we
    // shouldn't depend on that).
    drop(connection);
    io_threads.join().expect("LSP io threads panicked");
}

struct Server {
    /// Raw source text per URI (kept so didChange can re-analyze).
    docs: HashMap<Url, String>,
    /// Cached [`Analysis`] per URI — diagnostics + a symbol index + identifier
    /// tokens. Built once per open/change; hover/def/completion read from it, so
    /// a request never re-parses.
    analyses: HashMap<Url, Analysis>,
}

fn handle_initialize(connection: &Connection) -> Result<(), ()> {
    // lsp-server 0.7: `initialize_start` reads the first `initialize` request
    // and returns its id + raw params; `initialize_finish` sends the reply.
    let (id, params) = connection.initialize_start().map_err(|_| ())?;
    let _params: InitializeParams = serde_json::from_value(params).unwrap_or_default();

    let capabilities = ServerCapabilities {
        // Full document sync: the client sends the whole text on every edit.
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(true.into()),
        definition_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".into()]),
            ..Default::default()
        }),
        document_symbol_provider: Some(OneOf::Left(true)),
        // Whole-document formatting (RFC-0017): the handler runs `vyrn_frontend::fmt`
        // and returns one full-range replace. VS Code format-on-save then works.
        document_formatting_provider: Some(OneOf::Left(true)),
        ..Default::default()
    };
    let result = InitializeResult {
        capabilities,
        server_info: Some(ServerInfo { name: "vyrn-lsp".into(), version: Some("0.1.0".into()) }),
    };
    let value = serde_json::to_value(result).unwrap();
    connection.initialize_finish(id, value).map_err(|_| ())?;
    Ok(())
}

fn main_loop(connection: &Connection, server: &mut Server) {
    while let Ok(msg) = connection.receiver.recv() {
        match msg {
            Message::Request(req) => {
                // `handle_shutdown` replies to `shutdown` and returns true; it
                // does not exit the process — we return so `main` can finish
                // and the io threads can drain.
                if connection.handle_shutdown(&req).unwrap_or(false) {
                    return;
                }
                let resp = handle_request(server, req);
                let _ = connection.sender.send(Message::Response(resp));
            }
            Message::Notification(notif) => {
                handle_notification(connection, server, notif);
            }
            Message::Response(_) => {} // we sent no requests; ignore responses
        }
    }
}

/// Dispatch a request to a hover/definition/completion handler, or the
/// method-not-found fallback. Always produces a `Response` (never leaves the
/// client waiting on a reply).
fn handle_request(server: &Server, req: Request) -> Response {
    match req.method.as_str() {
        // `Response::new_ok(id, Option<T>)` is the correct shape for "maybe a
        // result": serde serializes `Some(x)` as the object and `None` as `null`.
        // We must NOT hand-build `Response { result: None, error: None }` — both
        // fields are `skip_serializing_if = Option::is_none`, so that would emit a
        // message with NEITHER `result` nor `error`, which the JSON-RPC client
        // rejects ("neither a result nor an error property"). A null `result` is
        // the spec-correct "nothing to hover / no definition".
        "textDocument/hover" => Response::new_ok(req.id, handle_hover(server, req.params)),
        "textDocument/definition" => Response::new_ok(req.id, handle_definition(server, req.params)),
        "textDocument/completion" => Response::new_ok(req.id, handle_completion(server, req.params)),
        "textDocument/documentSymbol" => {
            Response::new_ok(req.id, handle_document_symbol(server, req.params))
        }
        "textDocument/formatting" => Response::new_ok(req.id, handle_formatting(server, req.params)),
        _ => Response {
            id: req.id,
            result: None,
            error: Some(lsp_server::ResponseError {
                code: -32601, // Method not found
                message: format!("unsupported request: {}", req.method),
                data: None,
            }),
        },
    }
}

fn handle_hover(server: &Server, params: serde_json::Value) -> Option<Hover> {
    let p: HoverParams = serde_json::from_value(params).ok()?;
    let (analysis, _uri) = lookup(server, &p.text_document_position_params.text_document.uri)?;
    let (line, col) = to_frontend(&p.text_document_position_params.position);
    let r = resolve(analysis, line, col)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: r.hover,
        }),
        range: None,
    })
}

fn handle_definition(server: &Server, params: serde_json::Value) -> Option<GotoDefinitionResponse> {
    let p: GotoDefinitionParams = serde_json::from_value(params).ok()?;
    let (analysis, uri) = lookup(server, &p.text_document_position_params.text_document.uri)?;
    let (line, col) = to_frontend(&p.text_document_position_params.position);
    let r = resolve(analysis, line, col)?;
    // A built-in method (e.g. `push`, `info`) resolves for hover but has no source
    // declaration to jump to — return "no definition" rather than a bogus location.
    if !r.definition {
        return None;
    }
    // Cross-file: an imported symbol carries its source module. Local module
    // keys are absolute slash paths (→ a file URI); a remote key (`github:...`)
    // isn't a jumpable file, so it gets hover but no definition.
    let uri = match &r.target_file {
        Some(f) => Url::from_file_path(f).ok()?,
        None => uri,
    };
    Some(GotoDefinitionResponse::Scalar(Location {
        uri,
        range: lsp_range(r.target_line, r.target_col, r.target_end_col),
    }))
}

fn handle_completion(server: &Server, params: serde_json::Value) -> Option<CompletionResponse> {
    let p: CompletionParams = serde_json::from_value(params).ok()?;
    let uri = &p.text_document_position.text_document.uri;
    let (analysis, _uri) = lookup(server, uri)?;
    let (line, col) = to_frontend(&p.text_document_position.position);
    // A `.foo` member access → context-aware completions for the receiver's type
    // (e.g. `arr.` → push/at/alen/afree/length). Otherwise → all top-level
    // symbols; the client filters by the prefix the user typed.
    let raw = server.docs.get(uri);
    let items = if is_member_context(raw, line, col) {
        member_completions(analysis, line, col)
    } else {
        completions(analysis)
    }
    .into_iter()
    .map(|c| CompletionItem {
        label: c.label,
        kind: Some(to_lsp_kind(c.kind)),
        detail: Some(c.detail),
        ..Default::default()
    })
    .collect();
    // Always return a list (possibly empty) — the client filters by prefix.
    Some(CompletionResponse::Array(items))
}

/// Answer `textDocument/documentSymbol` from the cached symbol index: the
/// document's own top-level declarations (functions, methods, types, variants),
/// as a FLAT list. Imported cross-file symbols carry a `file` and are skipped —
/// they are not declared in this document (and their columns index the other
/// file's token stream, so they have no valid range here).
fn handle_document_symbol(
    server: &Server,
    params: serde_json::Value,
) -> Option<DocumentSymbolResponse> {
    let p: DocumentSymbolParams = serde_json::from_value(params).ok()?;
    let (analysis, _uri) = lookup(server, &p.text_document.uri)?;
    let symbols: Vec<DocumentSymbol> = analysis
        .symbols
        .iter()
        .filter(|s| s.file.is_none())
        .filter_map(to_document_symbol)
        .collect();
    Some(DocumentSymbolResponse::Nested(symbols))
}

/// Answer `textDocument/formatting` (RFC-0017): run the canonical formatter on
/// the cached document and return one whole-document replace. A document that
/// fails to lex returns `null` (no edit) — format-on-save must never corrupt a
/// buffer the user is mid-edit in. An already-canonical document returns an empty
/// edit list.
fn handle_formatting(server: &Server, params: serde_json::Value) -> Option<Vec<TextEdit>> {
    let p: DocumentFormattingParams = serde_json::from_value(params).ok()?;
    let text = server.docs.get(&p.text_document.uri)?;
    // A lex error (or the internal safety tripwire) → `None` → null result.
    let formatted = vyrn_frontend::fmt(text).ok()?;
    if &formatted == text {
        return Some(vec![]);
    }
    Some(vec![TextEdit { range: whole_document_range(text), new_text: formatted }])
}

/// A `Range` covering the entire `text` (start of the document to just past its
/// last character), so a single edit replaces everything.
fn whole_document_range(text: &str) -> Range {
    // LSP lines are 0-based; the end position is the line/character just after the
    // last content. Counting `\n`s gives the last line index; the final line's
    // length is its char count.
    let mut last_line = 0u32;
    let mut last_line_len = 0u32;
    for ch in text.chars() {
        if ch == '\n' {
            last_line += 1;
            last_line_len = 0;
        } else {
            last_line_len += 1;
        }
    }
    Range {
        start: Position { line: 0, character: 0 },
        end: Position { line: last_line, character: last_line_len },
    }
}

/// Map one frontend [`Symbol`](vyrn_frontend::Symbol) to an LSP `DocumentSymbol`.
/// Field/Param/Local never appear in the top-level index; they are dropped
/// defensively (the match must stay exhaustive). `col == 0` means "whole line"
/// and `lsp_range` maps it to character 0.
fn to_document_symbol(sym: &vyrn_frontend::Symbol) -> Option<DocumentSymbol> {
    let kind = match sym.kind {
        SymbolKind::Function => lsp_types::SymbolKind::FUNCTION,
        SymbolKind::Method => lsp_types::SymbolKind::METHOD,
        SymbolKind::Type => lsp_types::SymbolKind::STRUCT,
        SymbolKind::Variant => lsp_types::SymbolKind::ENUM_MEMBER,
        // Module state (RFC-0013) shows as a variable in the outline.
        SymbolKind::Global => lsp_types::SymbolKind::VARIABLE,
        SymbolKind::Field | SymbolKind::Param | SymbolKind::Local => return None,
    };
    let range = lsp_range(sym.line, sym.col, sym.end_col);
    let detail = if sym.detail.is_empty() { None } else { Some(sym.detail.clone()) };
    // `deprecated` is a deprecated field of `DocumentSymbol` but the struct has
    // no `Default`, so it must be named; silence the lint locally.
    #[allow(deprecated)]
    Some(DocumentSymbol {
        name: sym.name.clone(),
        detail,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: None,
    })
}

/// Whether the cursor at 1-based `(line, col)` is in a `.foo` member-access
/// context: the nearest non-space character to the left (skipping the partial
/// member name being typed) is a `.`. Used to route completion to
/// [`member_completions`] instead of top-level [`completions`].
fn is_member_context(text: Option<&String>, line: usize, col: usize) -> bool {
    let line_text = match text.and_then(|t| t.lines().nth(line.saturating_sub(1))) {
        Some(l) => l,
        None => return false,
    };
    // `col` is 1-based; the char just before the cursor is at 0-based index
    // `col - 2` in the line. Walk left, skipping the partial identifier the user
    // is typing (alnum/underscore), then any spaces.
    let bytes = line_text.as_bytes();
    let mut i = col.saturating_sub(2);
    // Skip the partial member name (e.g. the `pu` in `arr.pu`).
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i = i.wrapping_sub(1);
        if i == usize::MAX {
            return false;
        }
    }
    // Skip spaces between the dot and the partial name.
    while i < bytes.len() && bytes[i] == b' ' {
        i = i.wrapping_sub(1);
        if i == usize::MAX {
            return false;
        }
    }
    i < bytes.len() && bytes[i] == b'.'
}

/// Look up the cached [`Analysis`] for a document. Returns `None` (→ a null
/// result, i.e. "nothing to report") if the document isn't open or failed to
/// parse (no symbols were indexed).
fn lookup<'s>(server: &'s Server, uri: &Url) -> Option<(&'s Analysis, Url)> {
    Some((server.analyses.get(uri)?, uri.clone()))
}

/// LSP 0-based position → frontend 1-based (line, col).
fn to_frontend(pos: &Position) -> (usize, usize) {
    ((pos.line + 1) as usize, (pos.character + 1) as usize)
}

/// Frontend 1-based (line, col, end_col) → LSP 0-based `Range`. A col of 0 means
/// "whole line, unknown column" → a zero-length range at the line start (mirrors
/// `publish()`).
fn lsp_range(line: usize, col: usize, end_col: usize) -> Range {
    let l = line.saturating_sub(1) as u32;
    let c = if col == 0 { 0 } else { col.saturating_sub(1) as u32 };
    let ec = if end_col == 0 { c } else { end_col.saturating_sub(1) as u32 };
    Range {
        start: Position { line: l, character: c },
        end: Position { line: l, character: ec },
    }
}

fn to_lsp_kind(kind: SymbolKind) -> CompletionItemKind {
    match kind {
        SymbolKind::Function | SymbolKind::Method => CompletionItemKind::FUNCTION,
        SymbolKind::Type => CompletionItemKind::CLASS,
        SymbolKind::Variant => CompletionItemKind::ENUM_MEMBER,
        SymbolKind::Field => CompletionItemKind::FIELD,
        // Module state (RFC-0013) completes as a variable.
        SymbolKind::Global => CompletionItemKind::VARIABLE,
        // Locals are never returned by `completions` (top-level only), but the
        // match must be exhaustive — map them to VARIABLE for safety.
        SymbolKind::Param | SymbolKind::Local => CompletionItemKind::VARIABLE,
    }
}

fn handle_notification(connection: &Connection, server: &mut Server, notif: Notification) {
    // Dispatch on the notification method. `lsp-types` gives typed params per
    // known method; unknown notifications are ignored.
    if DidOpenTextDocument::METHOD == notif.method {
        if let Ok(params) = serde_json::from_value::<DidOpenTextDocumentParams>(notif.params) {
            let uri = params.text_document.uri.clone();
            let text = params.text_document.text;
            server.docs.insert(uri.clone(), text.clone());
            let analysis = analyze_doc(&uri, &text);
            publish(connection, &uri, &text, &analysis.diagnostics);
            server.analyses.insert(uri, analysis);
        }
    } else if DidChangeTextDocument::METHOD == notif.method {
        if let Ok(params) = serde_json::from_value::<DidChangeTextDocumentParams>(notif.params) {
            let uri = params.text_document.uri.clone();
            // Full sync: the last change carries the entire document text.
            if let Some(change) = params.content_changes.into_iter().last() {
                server.docs.insert(uri.clone(), change.text.clone());
                let analysis = analyze_doc(&uri, &change.text);
                publish(connection, &uri, &change.text, &analysis.diagnostics);
                server.analyses.insert(uri, analysis);
            }
        }
    } else if DidCloseTextDocument::METHOD == notif.method {
        if let Ok(params) = serde_json::from_value::<DidCloseTextDocumentParams>(notif.params) {
            // Drop the document and clear its diagnostics.
            server.docs.remove(&params.text_document.uri);
            server.analyses.remove(&params.text_document.uri);
            let _ = connection.sender.send(Message::Notification(Notification::new(
                PublishDiagnostics::METHOD.to_string(),
                PublishDiagnosticsParams {
                    uri: params.text_document.uri,
                    diagnostics: vec![],
                    version: None,
                },
            )));
        }
    }
    // Other notifications (didSave, etc.) are ignored.
}

/// Push the frontend's diagnostics for `uri` to the client.
///
/// `source` is the document text the diagnostics were computed from, used to
/// turn a "whole line" diagnostic (`col == 0`, i.e. the stage knew only the
/// line) into a squiggle over the *entire* line. Rendering such a diagnostic as
/// a zero-length range at column 0 makes VS Code squiggle just the first token
/// on the line (e.g. `return` on a `return match s {` line), which is misleading
/// — the error is about the `match`, not `return`. The whole line covers the
/// relevant keyword and reads as "this line has a problem".
fn publish(
    connection: &Connection,
    uri: &Url,
    source: &str,
    diags: &[vyrn_frontend::diagnostics::Diagnostic],
) {
    let mapped: Vec<LspDiagnostic> = diags
        .iter()
        .map(|d| {
            // 1-based frontend line → 0-based LSP line.
            let line = d.line.saturating_sub(1) as u32;
            // col == 0 means "whole line / unknown column" → squiggle the whole
            // line (start 0 .. line length). Otherwise a precise token range
            // (end_col == 0 → a single character/point).
            let (start_char, end_char) = if d.col == 0 {
                (0, line_char_len(source, d.line.saturating_sub(1)))
            } else {
                let s = d.col.saturating_sub(1) as u32;
                let e = if d.end_col == 0 { s } else { d.end_col.saturating_sub(1) as u32 };
                (s, e)
            };
            LspDiagnostic {
                range: Range {
                    start: Position { line, character: start_char },
                    end: Position { line, character: end_char },
                },
                severity: Some(match d.severity {
                    vyrn_frontend::diagnostics::Severity::Error => DiagnosticSeverity::ERROR,
                    vyrn_frontend::diagnostics::Severity::Warning => DiagnosticSeverity::WARNING,
                }),
                code: None,
                code_description: None,
                source: Some("vyrn".into()),
                message: d.message.clone(),
                related_information: None,
                tags: None,
                data: None,
            }
        })
        .collect();
    let _ = connection.sender.send(Message::Notification(Notification::new(
        PublishDiagnostics::METHOD.to_string(),
        PublishDiagnosticsParams { uri: uri.clone(), diagnostics: mapped, version: None },
    )));
}

/// The character length of line `line_idx` (0-based) in `source`, or 0 if out
/// of range. Uses `str::lines`, so a trailing `\r`/`\n` is not counted — this is
/// the visible line length. (LSP positions are UTF-16 code units; for the
/// *end* of a whole-line squiggle this is a cosmetic detail the client clamps
/// to the line end, and Vyrn sources are overwhelmingly ASCII.)
fn line_char_len(source: &str, line_idx: usize) -> u32 {
    source.lines().nth(line_idx).map(|l| l.chars().count() as u32).unwrap_or(0)
}