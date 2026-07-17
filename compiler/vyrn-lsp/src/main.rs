//! A minimal, synchronous Language Server Protocol server for Vyrn.
//!
//! Design goals (per the project's "easy maintained" constraint):
//!   * No async runtime — a plain blocking `lsp-server` loop on a single worker
//!     thread (given a large stack for the recursive generator/analysis work).
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

use vyrn_frontend::{
    analyze, completions, member_completions, resolve, string_literal_completions, Analysis,
    SymbolKind,
};

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
/// path (an untitled buffer falls back to single-file [`analyze`]). `overlays`
/// carries every open buffer's live text (path → text) so generator inputs
/// (`.vyx`, …) reflect unsaved edits (RFC-0033); the gen cache re-verifies the
/// overlaid bytes and regenerates when they differ from disk.
fn analyze_doc(uri: &Url, text: &str, overlays: &HashMap<String, String>) -> Analysis {
    let (opts, resolver, path) = match load_context(uri, overlays) {
        Some(ctx) => ctx,
        None => return analyze(text),
    };
    vyrn_frontend::analyze_linked(text, &path, &opts, &resolver)
}

/// Build the load options + overlay-aware resolver + slash path for `uri`, or
/// `None` for an untitled buffer with no filesystem path.
fn load_context(
    uri: &Url,
    overlays: &HashMap<String, String>,
) -> Option<(vyrn_frontend::loader::LoadOptions, EditorResolver, String)> {
    let path = uri.to_file_path().ok()?.to_string_lossy().replace('\\', "/");
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
    let resolver = EditorResolver { manifest_dir, overlays: overlays.clone() };
    Some((opts, resolver, path))
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
    /// Live text of every open buffer (slash path → text), so generator inputs
    /// reflect unsaved edits (RFC-0033). Empty for a plain analysis.
    overlays: HashMap<String, String>,
}

impl vyrn_frontend::loader::ModuleResolver for EditorResolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        if !vyrn_frontend::loader::is_remote(resolved) {
            // Prefer the open buffer's live text over the on-disk file.
            if let Some(text) = self.overlays.get(resolved) {
                return Ok(text.clone());
            }
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

    // Run the whole session on a worker thread with a LARGE stack. Analysis of a
    // document with generator imports (RFC-0021/-0033) runs the comptime
    // interpreter and re-lexes/checks the synthesized module — deeply recursive
    // work that overflows the OS default main-thread stack (≈1 MB on Windows)
    // once the LSP/JSON frames are also on it. 64 MB matches the headroom the CLI
    // and cargo's test threads already enjoy.
    let worker = std::thread::Builder::new()
        .name("vyrn-lsp".into())
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let mut server = Server {
                docs: HashMap::new(),
                analyses: HashMap::new(),
                vyx_owner: HashMap::new(),
            };
            // `initialize` is a special handshake: read it, reply with
            // capabilities, then enter the main loop. EOF here just means the
            // client left.
            if handle_initialize(&connection).is_ok() {
                main_loop(&connection, &mut server);
            }
            connection
        })
        .expect("spawn vyrn-lsp worker thread");
    let connection = worker.join().expect("vyrn-lsp worker thread panicked");
    // Drop the connection BEFORE joining the I/O threads: the writer thread
    // only exits once its sender (owned by `connection`) is dropped, and
    // `IoThreads::join` joins the writer last. Dropping here releases it so the
    // join can complete (a real client additionally closes the pipe, but we
    // shouldn't depend on that).
    drop(connection);
    io_threads.join().expect("LSP io threads panicked");
}

struct Server {
    /// Raw source text per URI (kept so didChange can re-analyze). Holds both
    /// Vyrn documents and generator input buffers (`.vyx`, …).
    docs: HashMap<Url, String>,
    /// Cached [`Analysis`] per URI — diagnostics + a symbol index + identifier
    /// tokens. Built once per open/change; hover/def/completion read from it, so
    /// a request never re-parses. Keyed by the Vyrn (root) document URI only.
    analyses: HashMap<Url, Analysis>,
    /// RFC-0033: a generator input file (slash path) → the Vyrn document whose
    /// analysis synthesized a module from it. Lets a `.vyx` request resolve which
    /// root to map through, and a `.vyx` edit know which root to re-analyze.
    /// Populated whenever a Vyrn document with generator imports is analyzed.
    vyx_owner: HashMap<String, Url>,
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
    let uri = &p.text_document_position_params.text_document.uri;
    let (line, col) = to_frontend(&p.text_document_position_params.position);
    // RFC-0033: a request inside a generator input file (`.vyx`) is answered
    // against the synthesized module at the mapped generated position.
    let r = if is_vyrn_uri(uri) {
        let (analysis, _) = lookup(server, uri)?;
        resolve(analysis, line, col)?
    } else {
        let fwd = vyx_forward(server, uri, line, col)?;
        resolve(&fwd.synth, fwd.line, fwd.col)?
    };
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
    let uri = &p.text_document_position_params.text_document.uri;
    let (line, col) = to_frontend(&p.text_document_position_params.position);
    // RFC-0033: from a `.vyx` template expression, resolve through the
    // synthesized module. Only an IMPORTED declaration (with a real source file)
    // is a useful jump target — a binding local to the synthesized module has no
    // on-disk location, so it yields no definition (v1).
    let (r, home_uri) = if is_vyrn_uri(uri) {
        let (analysis, u) = lookup(server, uri)?;
        (resolve(analysis, line, col)?, Some(u))
    } else {
        let fwd = vyx_forward(server, uri, line, col)?;
        (resolve(&fwd.synth, fwd.line, fwd.col)?, None)
    };
    // A built-in method (e.g. `push`, `info`) resolves for hover but has no source
    // declaration to jump to — return "no definition" rather than a bogus location.
    if !r.definition {
        return None;
    }
    // Cross-file: an imported symbol carries its source module. Local module
    // keys are absolute slash paths (→ a file URI); a remote key (`github:...`)
    // isn't a jumpable file, so it gets hover but no definition.
    let target_uri = match &r.target_file {
        Some(f) => Url::from_file_path(f.replace('/', std::path::MAIN_SEPARATOR_STR)).ok()?,
        // No source file: within the open Vyrn document, jump in place; within a
        // `.vyx` request the target is inside the synthesized module (no file).
        None => home_uri?,
    };
    Some(GotoDefinitionResponse::Scalar(Location {
        uri: target_uri,
        range: lsp_range(r.target_line, r.target_col, r.target_end_col),
    }))
}

fn handle_completion(server: &Server, params: serde_json::Value) -> Option<CompletionResponse> {
    let p: CompletionParams = serde_json::from_value(params).ok()?;
    let uri = &p.text_document_position.text_document.uri;
    let (line, col) = to_frontend(&p.text_document_position.position);
    // RFC-0033: completion inside a `.vyx` template expression runs against the
    // synthesized module at the mapped position (record-field completion after
    // `item.`, top-level symbols otherwise).
    if !is_vyrn_uri(uri) {
        let fwd = vyx_forward(server, uri, line, col)?;
        // `is_member_context` re-extracts line `fwd.line` from the text it's
        // given, so it must receive the WHOLE synthesized source, not one line.
        let items = if is_member_context(Some(&fwd.gen_source), fwd.line, fwd.col) {
            member_completions(&fwd.synth, fwd.line, fwd.col)
        } else {
            completions(&fwd.synth)
        };
        return Some(CompletionResponse::Array(items.into_iter().map(to_completion_item).collect()));
    }
    let (analysis, _uri) = lookup(server, uri)?;
    // A `.foo` member access → context-aware completions for the receiver's type
    // (e.g. `arr.` → push/at/alen/afree/length). Otherwise → all top-level
    // symbols; the client filters by the prefix the user typed.
    let raw = server.docs.get(uri);
    // RFC-0020 M1: inside a string literal whose expected type is a finite
    // string type, offer that type's language (`t("` → every key). Falls back to
    // member / top-level completion otherwise.
    let items = if is_string_literal_context(raw, line, col) {
        raw.map(|src| string_literal_completions(analysis, src, line, col)).unwrap_or_default()
    } else if is_member_context(raw, line, col) {
        member_completions(analysis, line, col)
    } else {
        completions(analysis)
    }
    .into_iter()
    .map(to_completion_item)
    .collect();
    // Always return a list (possibly empty) — the client filters by prefix.
    Some(CompletionResponse::Array(items))
}

/// Map one frontend completion to an LSP `CompletionItem`.
fn to_completion_item(c: vyrn_frontend::Completion) -> CompletionItem {
    CompletionItem {
        label: c.label,
        kind: Some(to_lsp_kind(c.kind)),
        detail: Some(c.detail),
        ..Default::default()
    }
}

/// The synthesized-module analysis and the generated position an RFC-0033
/// forward request maps to.
struct VyxFwd {
    /// Analysis of the synthesized module (linked under the owner's directory).
    synth: Analysis,
    /// The synthesized module's source (for member/string completion context).
    gen_source: String,
    /// 1-based generated line/column the input cursor maps to.
    line: usize,
    col: usize,
}

/// Map a cursor inside a generator input file (`.vyx`) to a position in the
/// synthesized module, and analyze that module so hover/completion/definition
/// can be answered against it (RFC-0033 forward mapping).
///
/// Verbatim regions map column-exactly (the input expression is located inside
/// the governed generated line); derived regions (a `{#for}`/`{#if}` head) map
/// to the region's start. Returns `None` when the cursor is outside any region
/// or the owner can't be re-generated.
fn vyx_forward(server: &Server, vyx_uri: &Url, line: usize, col: usize) -> Option<VyxFwd> {
    let vyx_path = uri_path(vyx_uri)?;
    let owner = server.vyx_owner.get(&vyx_path)?.clone();
    let owner_analysis = server.analyses.get(&owner)?;

    // The innermost region on this input line at or left of the cursor.
    let mut region: Option<vyrn_frontend::origin::Region> = None;
    for r in owner_analysis.origins.regions_for(&vyx_path) {
        if r.origin.line == line && r.origin.col <= col {
            let better = region.as_ref().map(|b| r.origin.col >= b.origin.col).unwrap_or(true);
            if better {
                region = Some(r);
            }
        }
    }
    let region = region?;

    let overlays = overlays_of(server);
    let (opts, resolver, owner_path) = load_context(&owner, &overlays)?;
    let owner_text = server
        .docs
        .get(&owner)
        .cloned()
        .or_else(|| std::fs::read_to_string(&owner_path).ok())?;
    // Re-obtain the synthesized module's source (cheap — the gen cache carries
    // it); find the one this region belongs to.
    let gen_source = vyrn_frontend::loader::generated_modules(&owner_text, &owner_path, &opts, &resolver)
        .ok()?
        .into_iter()
        .find(|(banner, _)| *banner == region.gen_module)
        .map(|(_, src)| src)?;

    let vyx_text = server
        .docs
        .get(vyx_uri)
        .cloned()
        .or_else(|| std::fs::read_to_string(&vyx_path).ok())?;
    let vyx_line = vyx_text.lines().nth(line.saturating_sub(1))?;
    let gen_line = gen_source.lines().nth(region.gen_start_line.saturating_sub(1)).unwrap_or("");
    let (gline, gcol) = map_into_region(vyx_line, region.origin.col, col, gen_line, region.gen_start_line);

    // Analyze the synthesized module as a linked root under the owner's dir, so
    // its imports (std/html, rebased relatives) resolve. Its own diagnostics
    // (e.g. "no main") are ignored — only the symbol index is queried.
    let synth_path = synth_path_for(&owner_path);
    let synth = vyrn_frontend::analyze_linked(&gen_source, &synth_path, &opts, &resolver);
    Some(VyxFwd { synth, gen_source, line: gline, col: gcol })
}

/// Map an input-file cursor into a generated line's verbatim text. `origin_col`
/// is the region's 1-based input start column; the return is a 1-based
/// `(gen_line, gen_col)`. Column-exact when the input expression is found in
/// `gen_line`, else the generated line start (region-level).
fn map_into_region(
    vyx_line: &str,
    origin_col: usize,
    col: usize,
    gen_line: &str,
    gen_start_line: usize,
) -> (usize, usize) {
    let delta = col.saturating_sub(origin_col);
    match align_expr(vyx_line, origin_col, gen_line) {
        Some(gcol) => (gen_start_line, gcol + delta),
        None => (gen_start_line, 1),
    }
}

/// The 1-based column in `gen_line` where the verbatim input expression at
/// `origin_col` begins, found as the longest input-tail prefix that occurs in
/// the generated line (the expression, since the following input bytes — `}`,
/// `>` — diverge from the generated wrapper).
fn align_expr(vyx_line: &str, origin_col: usize, gen_line: &str) -> Option<usize> {
    let tail: Vec<char> = vyx_line.chars().skip(origin_col.saturating_sub(1)).collect();
    let mut len = tail.len();
    while len >= 1 {
        let cand: String = tail[..len].iter().collect();
        if let Some(byte_idx) = gen_line.find(&cand) {
            return Some(gen_line[..byte_idx].chars().count() + 1);
        }
        len -= 1;
    }
    None
}

/// A synthetic root path for a synthesized module, placed in the owner's
/// directory so its relative imports resolve exactly as at generation time.
fn synth_path_for(owner_path: &str) -> String {
    match owner_path.rfind('/') {
        Some(i) => format!("{}/__vyrn_vyx_synth__.vyrn", &owner_path[..i]),
        None => "__vyrn_vyx_synth__.vyrn".to_string(),
    }
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

/// Whether the 1-based `(line, col)` cursor is inside a double-quoted string
/// literal: an odd number of unescaped `"` precede it on the line (RFC-0020
/// string-literal completion). A best-effort per-line scan — good enough to
/// route completion; the frontend re-lexes to pin the exact literal and its
/// expected type.
fn is_string_literal_context(text: Option<&String>, line: usize, col: usize) -> bool {
    let line_text = match text.and_then(|t| t.lines().nth(line.saturating_sub(1))) {
        Some(l) => l,
        None => return false,
    };
    let mut in_str = false;
    let mut escaped = false;
    // Count characters strictly before the cursor (col is 1-based).
    for (idx, ch) in line_text.chars().enumerate() {
        if idx + 1 >= col {
            break;
        }
        if in_str {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_str = false;
            }
        } else if ch == '"' {
            in_str = true;
        }
    }
    in_str
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

/// Whether `uri`'s path is a Vyrn source (`.vyrn`). Anything else the server
/// tracks is a generator INPUT buffer (`.vyx`, …), analyzed only indirectly
/// through the Vyrn document that consumes it (RFC-0033).
fn is_vyrn_uri(uri: &Url) -> bool {
    uri.path().ends_with(".vyrn")
}

/// The slash path of `uri`, or `None` for a non-file URI.
fn uri_path(uri: &Url) -> Option<String> {
    Some(uri.to_file_path().ok()?.to_string_lossy().replace('\\', "/"))
}

/// Every open buffer as `slash-path → text` — the overlay set that makes
/// generation see unsaved edits (RFC-0033).
fn overlays_of(server: &Server) -> HashMap<String, String> {
    server
        .docs
        .iter()
        .filter_map(|(u, t)| uri_path(u).map(|p| (p, t.clone())))
        .collect()
}

/// (Re)analyze the Vyrn document `root_uri` (open buffer, else disk), publish
/// its own diagnostics, and — for every generator input it reads — record the
/// ownership and publish the input's remapped diagnostics against its own URI.
fn reanalyze_root(connection: &Connection, server: &mut Server, root_uri: &Url) {
    let text = match server.docs.get(root_uri) {
        Some(t) => t.clone(),
        None => match uri_path(root_uri).and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(t) => t,
            None => return,
        },
    };
    let overlays = overlays_of(server);
    let analysis = analyze_doc(root_uri, &text, &overlays);
    publish(connection, root_uri, &text, &analysis.diagnostics);
    // Record which inputs this root synthesizes from, and surface each input's
    // remapped diagnostics inside that input's buffer.
    for f in analysis.origins.input_files() {
        server.vyx_owner.insert(f, root_uri.clone());
    }
    publish_remapped(connection, &analysis);
    server.analyses.insert(root_uri.clone(), analysis);
}

/// Publish origin-remapped diagnostics (RFC-0033) grouped by input file, so a
/// template error appears inside its `.vyx` buffer. Every referenced input is
/// republished (empty when clean) so a fixed error clears.
fn publish_remapped(connection: &Connection, analysis: &Analysis) {
    let mut by_file: HashMap<String, Vec<vyrn_frontend::diagnostics::Diagnostic>> = HashMap::new();
    for f in analysis.origins.input_files() {
        by_file.entry(f).or_default();
    }
    for d in &analysis.remapped {
        if let Some(f) = &d.file {
            by_file.entry(f.clone()).or_default().push(d.clone());
        }
    }
    for (file, diags) in by_file {
        // `file` is an absolute slash path; rebuild a native path for the URI.
        if let Ok(uri) = Url::from_file_path(file.replace('/', std::path::MAIN_SEPARATOR_STR)) {
            let src = std::fs::read_to_string(&file).unwrap_or_default();
            publish(connection, &uri, &src, &diags);
        }
    }
}

/// React to an open/change of `uri`: a Vyrn document re-analyzes itself; a
/// generator input buffer (`.vyx`, …) re-analyzes its owning Vyrn document (so
/// its remapped diagnostics refresh from the edited input). An input with no
/// known owner yet is simply stored — opening its consuming `.vyrn` wires it up.
fn refresh_document(connection: &Connection, server: &mut Server, uri: &Url) {
    if is_vyrn_uri(uri) {
        reanalyze_root(connection, server, uri);
    } else if let Some(owner) = uri_path(uri).and_then(|p| server.vyx_owner.get(&p)).cloned() {
        reanalyze_root(connection, server, &owner);
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
            refresh_document(connection, server, &uri);
        }
    } else if DidChangeTextDocument::METHOD == notif.method {
        if let Ok(params) = serde_json::from_value::<DidChangeTextDocumentParams>(notif.params) {
            let uri = params.text_document.uri.clone();
            // Full sync: the last change carries the entire document text.
            if let Some(change) = params.content_changes.into_iter().last() {
                server.docs.insert(uri.clone(), change.text.clone());
                refresh_document(connection, server, &uri);
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