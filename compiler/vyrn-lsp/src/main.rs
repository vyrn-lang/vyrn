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

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

mod templates;

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    CompletionTextEdit, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, Diagnostic as LspDiagnostic, DiagnosticSeverity,
    DocumentFormattingParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
    InitializeParams, InitializeResult, Location, MarkupContent, MarkupKind, OneOf, Position,
    PublishDiagnosticsParams, Range, SemanticToken, SemanticTokenModifier, SemanticTokenType,
    SemanticTokens, SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions,
    SemanticTokensParams, SemanticTokensRangeParams, SemanticTokensRangeResult,
    SemanticTokensResult, SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Url,
};

use vyrn_frontend::{
    analyze, class_completions, class_token_hover, completions, member_completions, resolve,
    string_literal_completions, Analysis, Completion, SemKind, SemMods, SymbolKind,
};

use templates::VyxCursor;

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
                vyx_ownerless: HashSet::new(),
                synth_cache: RefCell::new(HashMap::new()),
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
    /// Populated whenever a Vyrn document with generator imports is analyzed, and
    /// by RFC-0049 owner discovery when a `.vyx` is opened without its owner.
    vyx_owner: HashMap<String, Url>,
    /// RFC-0049 §1: `.vyx` files (slash path) for which owner discovery ran and
    /// found no consuming root (a scratch file). Cached so discovery — which
    /// analyzes candidate roots — does not re-run on every keystroke/hover. Cleared
    /// wholesale whenever a `.vyrn` is opened/changed (the project may have gained
    /// an owner) and per-file on a `.vyx` (re)open (an explicit retry).
    vyx_ownerless: HashSet<String>,
    /// RFC-0049 §2: the synthesized-module analysis cache, per owner root. Keyed by
    /// a content signature (owner text + open inputs under its dir); hover / tokens
    /// / definition / completion for a `.vyx` reuse it instead of re-running the
    /// owner's generators and re-analyzing the synthesized module on every request.
    /// `RefCell` because request handlers hold `&Server` (the server is
    /// single-threaded, so a borrow never races).
    synth_cache: RefCell<HashMap<Url, OwnerSynth>>,
}

/// RFC-0049 §2: one owner root's cached generation + per-module analyses.
struct OwnerSynth {
    /// Content signature of the inputs this generation was produced from. A
    /// mismatch (owner edit, `.vyx`/theme edit) invalidates the whole entry.
    sig: u64,
    /// Every synthesized module reachable from the owner, `(banner, gen_source)`
    /// — the result of one `generated_modules` run, reused across requests.
    gen_modules: Vec<(String, String)>,
    /// Per generated-module banner: its analyzed synthesized module + classified
    /// tokens, filled lazily the first time a request touches that module.
    analyzed: HashMap<String, Rc<AnalyzedSynth>>,
}

/// A synthesized module analyzed once and shared (RFC-0049 §2): the source, its
/// [`Analysis`] (for hover/def/completion) and its semantic tokens.
struct AnalyzedSynth {
    gen_source: String,
    analysis: Analysis,
    tokens: Vec<vyrn_frontend::SemToken>,
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
            // `.` for member access; `<`/`@`/`:`/`-`/space for `.vyx` template
            // structural + class-token completion (RFC-0042).
            trigger_characters: Some(
                [".", "<", "@", ":", "-", " ", "\""].iter().map(|s| s.to_string()).collect(),
            ),
            ..Default::default()
        }),
        document_symbol_provider: Some(OneOf::Left(true)),
        // Whole-document formatting (RFC-0017): the handler runs `vyrn_frontend::fmt`
        // and returns one full-range replace. VS Code format-on-save then works.
        document_formatting_provider: Some(OneOf::Left(true)),
        // Semantic tokens (RFC-0047 §1): the server classifies every identifier
        // from the cached `Analysis` (function vs type vs variable vs …), which
        // TextMate cannot distinguish. `full` + `range` are both served.
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                work_done_progress_options: Default::default(),
                legend: semantic_tokens_legend(),
                range: Some(true),
                full: Some(SemanticTokensFullOptions::Bool(true)),
            },
        )),
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
fn handle_request(server: &mut Server, req: Request) -> Response {
    // RFC-0049 §1: a `.vyx` request whose owner is not wired yet triggers owner
    // discovery here too — not only on didOpen — so the first interaction works
    // even if a request somehow precedes the open's discovery. This path never
    // publishes diagnostics (no `Connection`); the didOpen path does.
    if let Some(uri) = request_uri(&req) {
        if !is_vyrn_uri(&uri) {
            ensure_vyx_owner(server, &uri);
        }
    }
    let server: &Server = server;
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
        "textDocument/semanticTokens/full" => {
            Response::new_ok(req.id, handle_semantic_tokens_full(server, req.params))
        }
        "textDocument/semanticTokens/range" => {
            Response::new_ok(req.id, handle_semantic_tokens_range(server, req.params))
        }
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
    // against the synthesized module at the mapped generated position. RFC-0042:
    // when nothing resolves (the cursor is on a class token inside a string, not an
    // identifier), fall back to `Tw` class hover — the CSS rule `css()` emits, or
    // "safelisted (app-styled)".
    let value = if is_vyrn_uri(uri) {
        let (analysis, _) = lookup(server, uri)?;
        match resolve(analysis, line, col) {
            Some(r) => r.hover,
            None => server
                .docs
                .get(uri)
                .and_then(|src| class_token_hover(analysis, src, line, col))?,
        }
    } else {
        let fwd = vyx_forward(server, uri, line, col)?;
        match resolve(&fwd.synth.analysis, fwd.line, fwd.col) {
            Some(r) => r.hover,
            None => class_token_hover(&fwd.synth.analysis, &fwd.synth.gen_source, fwd.line, fwd.col)?,
        }
    };
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
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
        // RFC-0049 §3: a component tag `<CreateForm>` jumps to the sibling
        // `CreateForm.vyx` — resolved structurally, before the forward map (the tag
        // is not an identifier inside the synthesized module).
        if let Some(loc) = component_tag_definition(server, uri, line, col) {
            return Some(loc);
        }
        let fwd = vyx_forward(server, uri, line, col)?;
        (resolve(&fwd.synth.analysis, fwd.line, fwd.col)?, None)
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

/// RFC-0049 §3: if the `.vyx` cursor sits on a component tag (`<CreateForm …>` or
/// `</CreateForm>`), a `GotoDefinition` to the sibling `CreateForm.vyx`. Returns
/// `None` when the cursor is not on a PascalCase tag or no sibling file exists.
fn component_tag_definition(
    server: &Server,
    uri: &Url,
    line: usize,
    col: usize,
) -> Option<GotoDefinitionResponse> {
    let raw = server
        .docs
        .get(uri)
        .cloned()
        .or_else(|| uri_path(uri).and_then(|p| std::fs::read_to_string(p).ok()))?;
    let text = raw.lines().nth(line.saturating_sub(1))?;
    let chars: Vec<char> = text.chars().collect();
    // 0-based cursor index within the line.
    let cur = col.saturating_sub(1).min(chars.len());
    // Walk left to the start of the identifier under the cursor.
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut start = cur;
    while start > 0 && chars.get(start - 1).is_some_and(|&c| is_ident(c)) {
        start -= 1;
    }
    let mut end = cur;
    while end < chars.len() && chars.get(end).is_some_and(|&c| is_ident(c)) {
        end += 1;
    }
    if start >= end {
        return None;
    }
    // The token must be a PascalCase tag opened by `<` or `</` (skipping a `/`).
    let before = {
        let mut i = start;
        while i > 0 && chars[i - 1] == '/' {
            i -= 1;
        }
        i.checked_sub(1).and_then(|j| chars.get(j)).copied()
    };
    if before != Some('<') {
        return None;
    }
    let name: String = chars[start..end].iter().collect();
    if !name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let (dir, _self_name) = vyx_dir_and_name(uri)?;
    let sibling = dir.join(format!("{name}.vyx"));
    if !sibling.is_file() {
        return None;
    }
    let target = Url::from_file_path(&sibling).ok()?;
    Some(GotoDefinitionResponse::Scalar(Location {
        uri: target,
        range: Range {
            start: Position { line: 0, character: 0 },
            end: Position { line: 0, character: 0 },
        },
    }))
}

fn handle_completion(server: &Server, params: serde_json::Value) -> Option<CompletionResponse> {
    let p: CompletionParams = serde_json::from_value(params).ok()?;
    let uri = &p.text_document_position.text_document.uri;
    let (line, col) = to_frontend(&p.text_document_position.position);
    if !is_vyrn_uri(uri) {
        return vyx_completion(server, uri, line, col);
    }
    let (analysis, _uri) = lookup(server, uri)?;
    // A `.foo` member access → context-aware completions for the receiver's type
    // (e.g. `arr.` → push/at/alen/afree/length). Otherwise → all top-level
    // symbols; the client filters by the prefix the user typed.
    let raw = server.docs.get(uri);
    // RFC-0020 M1 / RFC-0042: inside a string literal whose expected type is a
    // finite string type, offer its language (`t("` → every key); whose expected
    // type is a sequence type (`theme.cls("…")` → `Tw`), offer the class alphabet
    // as token-in-sequence replacements. Falls back to member / top-level.
    if is_string_literal_context(raw, line, col) {
        if let (Some(src), Some(cls)) =
            (raw, raw.and_then(|s| class_completions(analysis, s, line, col)))
        {
            return Some(class_completion_response(src, line, col, cls));
        }
        let items = raw
            .map(|src| string_literal_completions(analysis, src, line, col))
            .unwrap_or_default()
            .into_iter()
            .map(to_completion_item)
            .collect();
        return Some(CompletionResponse::Array(items));
    }
    let items = if is_member_context(raw, line, col) {
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

/// Completion inside a `.vyx` template (RFC-0042). First a structural scan of the
/// raw `.vyx` classifies the cursor (attribute name / event / tag / component
/// prop / class value); anything structural is answered from the discovery
/// vocabularies or sibling components. A non-structural position (`{{ expr }}`,
/// script) falls through to the RFC-0033 forward-map path, which now also serves
/// finite/sequence string-literal completion (TransKey keys, `Tw` classes).
fn vyx_completion(server: &Server, uri: &Url, line: usize, col: usize) -> Option<CompletionResponse> {
    let raw = server
        .docs
        .get(uri)
        .cloned()
        .or_else(|| uri_path(uri).and_then(|p| std::fs::read_to_string(p).ok()))?;
    match templates::classify(&raw, line, col) {
        VyxCursor::TagName { prefix, start_col } => {
            Some(tag_name_completion(uri, &prefix, line, start_col, col))
        }
        VyxCursor::AttrName { tag, prefix: _, is_component, start_col } => {
            Some(attr_name_completion(uri, &tag, is_component, line, start_col, col))
        }
        VyxCursor::EventName { prefix: _, start_col } => {
            Some(event_name_completion(line, start_col, col))
        }
        VyxCursor::ClassValue { token: _, start_col } => {
            // The Tw alphabet comes from the synthesized (themed) module via the
            // forward map; a non-themed `.vyx` has no domain and gets nothing.
            let fwd = vyx_forward(server, uri, line, col)?;
            let cls =
                class_completions(&fwd.synth.analysis, &fwd.synth.gen_source, fwd.line, fwd.col)?;
            Some(class_token_response(&raw, line, start_col, col, cls))
        }
        VyxCursor::Other => {
            let fwd = vyx_forward(server, uri, line, col)?;
            let gen = &fwd.synth.gen_source;
            // A string literal in the generated code → finite keys or `Tw` classes.
            if is_string_literal_context(Some(gen), fwd.line, fwd.col) {
                if let Some(cls) = class_completions(&fwd.synth.analysis, gen, fwd.line, fwd.col) {
                    // A generated class string reached via `{{ }}` is rare, but map
                    // the token in the .vyx line if present.
                    return Some(class_completion_response(&raw, line, col, cls));
                }
                let items = string_literal_completions(&fwd.synth.analysis, gen, fwd.line, fwd.col)
                    .into_iter()
                    .map(to_completion_item)
                    .collect();
                return Some(CompletionResponse::Array(items));
            }
            let items = if is_member_context(Some(gen), fwd.line, fwd.col) {
                member_completions(&fwd.synth.analysis, fwd.line, fwd.col)
            } else {
                completions(&fwd.synth.analysis)
            };
            Some(CompletionResponse::Array(items.into_iter().map(to_completion_item).collect()))
        }
    }
}

/// Component tags (sibling PascalCase `.vyx`) plus, for a lowercase prefix, the
/// document's plain symbols. Each item replaces the partial tag name.
fn tag_name_completion(
    uri: &Url,
    prefix: &str,
    line: usize,
    start_col: usize,
    col: usize,
) -> CompletionResponse {
    let range = replace_range(line, start_col, col);
    let mut items: Vec<CompletionItem> = Vec::new();
    if let Some((dir, self_name)) = vyx_dir_and_name(uri) {
        for name in templates::sibling_components(&dir, &self_name) {
            items.push(edit_item(&name, CompletionItemKind::CLASS, "component", range));
        }
    }
    // Common HTML elements, for a lowercase tag start.
    if !prefix.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        for el in HTML_ELEMENTS {
            items.push(edit_item(el, CompletionItemKind::KEYWORD, "html element", range));
        }
    }
    CompletionResponse::Array(items)
}

/// Attribute-name completion: a component tag offers its declared props; an
/// element offers global + per-element HTML attributes and the `v-*` directives.
fn attr_name_completion(
    uri: &Url,
    tag: &str,
    is_component: bool,
    line: usize,
    start_col: usize,
    col: usize,
) -> CompletionResponse {
    let range = replace_range(line, start_col, col);
    let mut items: Vec<CompletionItem> = Vec::new();
    if is_component {
        if let Some((dir, _)) = vyx_dir_and_name(uri) {
            let path = dir.join(format!("{tag}.vyx"));
            for prop in templates::component_props(&path) {
                let label = prop.name.clone();
                let detail = format!("prop: {}", prop.ty);
                items.push(edit_item(&label, CompletionItemKind::FIELD, &detail, range));
                // Also offer the dynamic-bound form `:prop`.
                items.push(edit_item(
                    &format!(":{label}"),
                    CompletionItemKind::FIELD,
                    &detail,
                    range,
                ));
            }
        }
        return CompletionResponse::Array(items);
    }
    for a in templates::GLOBAL_ATTRS {
        items.push(edit_item(a, CompletionItemKind::PROPERTY, "html attribute", range));
    }
    for a in templates::element_attrs(tag) {
        items.push(edit_item(a, CompletionItemKind::PROPERTY, "html attribute", range));
    }
    for (d, detail) in templates::DIRECTIVES {
        items.push(edit_item(d, CompletionItemKind::KEYWORD, detail, range));
    }
    CompletionResponse::Array(items)
}

/// `@event` completion: the DOM events the runtime dispatches.
fn event_name_completion(line: usize, start_col: usize, col: usize) -> CompletionResponse {
    // Replace from the `@` (start_col) so the inserted `@click` keeps the sigil.
    let range = replace_range(line, start_col, col);
    let items = templates::EVENTS
        .iter()
        .map(|e| edit_item(&format!("@{e}"), CompletionItemKind::EVENT, "dom event", range))
        .collect();
    CompletionResponse::Array(items)
}

/// Build class-token completions replacing the current token in a `.vyx` line.
fn class_token_response(
    raw: &str,
    line: usize,
    start_col: usize,
    col: usize,
    alphabet: Vec<Completion>,
) -> CompletionResponse {
    let prefix = line_slice(raw, line, start_col, col);
    let range = replace_range(line, start_col, col);
    let items = alphabet
        .into_iter()
        .filter(|c| c.label.starts_with(&prefix))
        .map(|c| edit_item(&c.label, CompletionItemKind::CONSTANT, &c.detail, range))
        .collect();
    CompletionResponse::Array(items)
}

/// Class-token completion where the token span is computed from the buffer line
/// directly (the `.vyrn` `theme.cls("…")` path and generated-string fallback).
fn class_completion_response(
    raw: &str,
    line: usize,
    col: usize,
    alphabet: Vec<Completion>,
) -> CompletionResponse {
    let start_col = class_token_start(raw, line, col);
    class_token_response(raw, line, start_col, col, alphabet)
}

/// The 1-based start column of the whitespace/quote-delimited token containing the
/// 1-based cursor `col` on `line`.
fn class_token_start(raw: &str, line: usize, col: usize) -> usize {
    let Some(text) = raw.lines().nth(line.saturating_sub(1)) else {
        return col;
    };
    let chars: Vec<char> = text.chars().collect();
    let mut lo = col.saturating_sub(1).min(chars.len());
    while lo > 0 {
        let c = chars[lo - 1];
        if c.is_whitespace() || c == '"' || c == '\'' {
            break;
        }
        lo -= 1;
    }
    lo + 1
}

/// The substring of `line` from 1-based `start_col` up to (excluding) 1-based
/// `col` — the token prefix already typed.
fn line_slice(raw: &str, line: usize, start_col: usize, col: usize) -> String {
    let Some(text) = raw.lines().nth(line.saturating_sub(1)) else {
        return String::new();
    };
    let chars: Vec<char> = text.chars().collect();
    let lo = start_col.saturating_sub(1).min(chars.len());
    let hi = col.saturating_sub(1).min(chars.len());
    if lo >= hi {
        return String::new();
    }
    chars[lo..hi].iter().collect()
}

/// A zero-width-safe LSP range on `line` (1-based) from `start_col`..`col`
/// (1-based, exclusive) — the span a completion `textEdit` replaces.
fn replace_range(line: usize, start_col: usize, col: usize) -> Range {
    let l = line.saturating_sub(1) as u32;
    Range {
        start: Position { line: l, character: start_col.saturating_sub(1) as u32 },
        end: Position { line: l, character: col.saturating_sub(1) as u32 },
    }
}

/// A completion item that replaces `range` with `label` (token-in-sequence /
/// prefix-replace insertion, so multi-char tokens like `md:hover:bg-…` don't
/// duplicate the already-typed prefix).
fn edit_item(label: &str, kind: CompletionItemKind, detail: &str, range: Range) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        detail: Some(detail.to_string()),
        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
            range,
            new_text: label.to_string(),
        })),
        ..Default::default()
    }
}

/// The directory of a `.vyx` URI and the component's own base name (no `.vyx`).
fn vyx_dir_and_name(uri: &Url) -> Option<(std::path::PathBuf, String)> {
    let path = uri.to_file_path().ok()?;
    let dir = path.parent()?.to_path_buf();
    let name = path.file_stem()?.to_string_lossy().into_owned();
    Some((dir, name))
}

/// A small set of common HTML element names for lowercase tag completion.
const HTML_ELEMENTS: &[&str] = &[
    "div", "span", "p", "a", "ul", "ol", "li", "section", "header", "footer",
    "nav", "main", "article", "aside", "h1", "h2", "h3", "h4", "h5", "h6",
    "button", "input", "label", "select", "option", "textarea", "form", "img",
    "table", "thead", "tbody", "tr", "td", "th", "pre", "code", "strong", "em",
];

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
    /// The cached analyzed synthesized module (RFC-0049 §2): analysis + source +
    /// tokens, shared across requests until the owner or an input changes.
    synth: Rc<AnalyzedSynth>,
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

    // The synthesized module for this region — analyzed once and reused (§2).
    let synth = synth_for(server, &owner, &region.gen_module)?;

    let vyx_text = server
        .docs
        .get(vyx_uri)
        .cloned()
        .or_else(|| std::fs::read_to_string(&vyx_path).ok())?;
    let vyx_line = vyx_text.lines().nth(line.saturating_sub(1))?;
    let gen_line =
        synth.gen_source.lines().nth(region.gen_start_line.saturating_sub(1)).unwrap_or("");
    let (gline, gcol) = map_into_region(vyx_line, region.origin.col, col, gen_line, region.gen_start_line);
    Some(VyxFwd { synth, line: gline, col: gcol })
}

/// The analyzed synthesized module (RFC-0049 §2) for `owner`'s generated module
/// `banner`, from the cache when the owner's input signature is unchanged, else
/// generated + analyzed and cached. Returns `None` if the owner can't be read or
/// the banner isn't among its generated modules.
fn synth_for(server: &Server, owner: &Url, banner: &str) -> Option<Rc<AnalyzedSynth>> {
    let overlays = overlays_of(server);
    let (opts, resolver, owner_path) = load_context(owner, &overlays)?;
    let owner_text = server
        .docs
        .get(owner)
        .cloned()
        .or_else(|| std::fs::read_to_string(&owner_path).ok())?;
    let sig = owner_sig(&owner_text, &owner_path, &overlays);

    let mut cache = server.synth_cache.borrow_mut();
    let entry = match cache.get(owner) {
        Some(e) if e.sig == sig => cache.get_mut(owner).unwrap(),
        _ => {
            // (Re)generate: the owner or an input changed (or first touch).
            let gen_modules =
                vyrn_frontend::loader::generated_modules(&owner_text, &owner_path, &opts, &resolver)
                    .ok()?;
            cache.insert(
                owner.clone(),
                OwnerSynth { sig, gen_modules, analyzed: HashMap::new() },
            );
            cache.get_mut(owner).unwrap()
        }
    };

    if let Some(a) = entry.analyzed.get(banner) {
        return Some(a.clone());
    }
    let gen_source =
        entry.gen_modules.iter().find(|(b, _)| b == banner).map(|(_, s)| s.clone())?;
    // Analyze the synthesized module as a linked root under the owner's dir, so
    // its imports (std/html, rebased relatives) resolve. Its own diagnostics
    // (e.g. "no main") are ignored — only the symbol index / tokens are queried.
    let synth_path = synth_path_for(&owner_path);
    let analysis = vyrn_frontend::analyze_linked(&gen_source, &synth_path, &opts, &resolver);
    let tokens = vyrn_frontend::semantic_tokens(&analysis);
    let a = Rc::new(AnalyzedSynth { gen_source, analysis, tokens });
    entry.analyzed.insert(banner.to_string(), a.clone());
    Some(a)
}

/// A content signature for an owner's generation inputs: the owner's own text
/// plus every open buffer under its directory (the `.vyx`/theme inputs a
/// generator reads). Any edit to one changes the signature, invalidating the
/// cached generation (RFC-0049 §2). Files not open are read from disk at
/// generation time; the editor only tracks open buffers, so this captures every
/// input it can influence.
fn owner_sig(owner_text: &str, owner_path: &str, overlays: &HashMap<String, String>) -> u64 {
    let dir = match owner_path.rfind('/') {
        Some(i) => &owner_path[..=i], // keep the trailing slash
        None => "",
    };
    let mut under: Vec<(&String, &String)> = overlays
        .iter()
        .filter(|(p, _)| p.as_str() != owner_path && (dir.is_empty() || p.starts_with(dir)))
        .collect();
    under.sort_by(|a, b| a.0.cmp(b.0));
    let mut h = std::collections::hash_map::DefaultHasher::new();
    owner_text.hash(&mut h);
    for (p, t) in under {
        p.hash(&mut h);
        t.hash(&mut h);
    }
    h.finish()
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

// ---------------------------------------------------------------------------
// Semantic tokens (RFC-0047 §1)
// ---------------------------------------------------------------------------

/// The token legend advertised in server capabilities. The ORDER of both vecs is
/// load-bearing: it defines the integer indices the wire encoding uses, so
/// [`sem_type_index`] / [`sem_mods_bits`] must agree with it.
fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::NAMESPACE,   // 0
            SemanticTokenType::TYPE,        // 1
            SemanticTokenType::ENUM_MEMBER, // 2
            SemanticTokenType::PARAMETER,   // 3
            SemanticTokenType::VARIABLE,    // 4
            SemanticTokenType::PROPERTY,    // 5
            SemanticTokenType::FUNCTION,    // 6
            SemanticTokenType::METHOD,      // 7
            SemanticTokenType::MACRO,       // 8
            SemanticTokenType::KEYWORD,     // 9 (in the legend for §3 parity; not
                                            //    currently emitted — the grammar
                                            //    owns keywords)
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,     // bit 0
            SemanticTokenModifier::READONLY,        // bit 1
            SemanticTokenModifier::DEFAULT_LIBRARY, // bit 2
        ],
    }
}

/// The legend index of a frontend [`SemKind`]. Must match [`semantic_tokens_legend`].
fn sem_type_index(k: SemKind) -> u32 {
    match k {
        SemKind::Namespace => 0,
        SemKind::Type => 1,
        SemKind::EnumMember => 2,
        SemKind::Parameter => 3,
        SemKind::Variable => 4,
        SemKind::Property => 5,
        SemKind::Function => 6,
        SemKind::Method => 7,
        SemKind::Macro => 8,
    }
}

/// The modifier bitset for a frontend [`SemMods`]. Must match [`semantic_tokens_legend`].
fn sem_mods_bits(m: SemMods) -> u32 {
    let mut b = 0;
    if m.declaration {
        b |= 1 << 0;
    }
    if m.readonly {
        b |= 1 << 1;
    }
    if m.default_library {
        b |= 1 << 2;
    }
    b
}

/// `textDocument/semanticTokens/full` (RFC-0047 §1): classify every identifier in
/// the document. `.vyrn` classifies directly from the cached analysis; `.vyx`
/// classifies its template/script tokens by mapping through the origin map into
/// the synthesized module (region-level/unmapped spans stay TextMate-only).
fn handle_semantic_tokens_full(
    server: &Server,
    params: serde_json::Value,
) -> Option<SemanticTokensResult> {
    let p: SemanticTokensParams = serde_json::from_value(params).ok()?;
    let toks = document_sem_tokens(server, &p.text_document.uri)?;
    Some(SemanticTokensResult::Tokens(encode_tokens(toks)))
}

/// `textDocument/semanticTokens/range`: the same classification, filtered to the
/// requested line range (v1 computes the whole document then filters — the
/// documents are small and the analysis is already cached).
fn handle_semantic_tokens_range(
    server: &Server,
    params: serde_json::Value,
) -> Option<SemanticTokensRangeResult> {
    let p: SemanticTokensRangeParams = serde_json::from_value(params).ok()?;
    let mut toks = document_sem_tokens(server, &p.text_document.uri)?;
    let start = (p.range.start.line + 1) as usize;
    let end = (p.range.end.line + 1) as usize;
    toks.retain(|t| t.line >= start && t.line <= end);
    Some(SemanticTokensRangeResult::Tokens(encode_tokens(toks)))
}

/// The classified tokens for `uri`: from the cached analysis for a `.vyrn`
/// document, or origin-mapped from the synthesized module for a `.vyx` input.
fn document_sem_tokens(server: &Server, uri: &Url) -> Option<Vec<vyrn_frontend::SemToken>> {
    if is_vyrn_uri(uri) {
        let (analysis, _) = lookup(server, uri)?;
        Some(vyrn_frontend::semantic_tokens(analysis))
    } else {
        Some(vyx_semantic_tokens(server, uri))
    }
}

/// Delta-encode classified tokens into the LSP wire form. Tokens are sorted by
/// (line, col) and encoded as the required `[Δline, Δstart, len, type, mods]`
/// quintuples (0-based positions).
fn encode_tokens(mut toks: Vec<vyrn_frontend::SemToken>) -> SemanticTokens {
    toks.sort_by_key(|t| (t.line, t.col));
    let mut data = Vec::with_capacity(toks.len());
    let mut prev_line = 0u32;
    let mut prev_col = 0u32;
    for t in toks {
        let line = t.line.saturating_sub(1) as u32;
        let col = t.col.saturating_sub(1) as u32;
        let delta_line = line.saturating_sub(prev_line);
        let delta_start = if delta_line == 0 { col.saturating_sub(prev_col) } else { col };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length: t.len as u32,
            token_type: sem_type_index(t.kind),
            token_modifiers_bitset: sem_mods_bits(t.mods),
        });
        prev_line = line;
        prev_col = col;
    }
    SemanticTokens { result_id: None, data }
}

/// Classify a `.vyx` input's identifiers by mapping each verbatim origin region
/// (RFC-0033) back from the synthesized module's classification into the input
/// file's coordinates (RFC-0047 §1). The synthesized module is analyzed once per
/// generated module (banner); each region's generated line is scanned for the
/// tokens that fall inside its verbatim span, which are re-anchored at the
/// corresponding input columns. Regions that don't align verbatim (derived
/// spans) contribute nothing, leaving them to the TextMate grammar.
fn vyx_semantic_tokens(server: &Server, vyx_uri: &Url) -> Vec<vyrn_frontend::SemToken> {
    let mut out = Vec::new();
    let Some(vyx_path) = uri_path(vyx_uri) else { return out };
    let Some(owner) = server.vyx_owner.get(&vyx_path).cloned() else { return out };
    let Some(owner_analysis) = server.analyses.get(&owner) else { return out };
    let regions = owner_analysis.origins.regions_for(&vyx_path);
    if regions.is_empty() {
        return out;
    }

    let Some(vyx_text) = server
        .docs
        .get(vyx_uri)
        .cloned()
        .or_else(|| std::fs::read_to_string(&vyx_path).ok())
    else {
        return out;
    };

    // Each region's synthesized module is fetched from the shared §2 cache, so a
    // module is generated + analyzed + classified at most once per owner state,
    // reused across regions AND across hover/def/completion requests.
    for region in &regions {
        let Some(synth) = synth_for(server, &owner, &region.gen_module) else {
            continue;
        };
        let gen_source = &synth.gen_source;
        let synth_toks = &synth.tokens;

        let Some(vyx_line) = vyx_text.lines().nth(region.origin.line.saturating_sub(1)) else {
            continue;
        };
        let Some(gen_line) = gen_source.lines().nth(region.gen_start_line.saturating_sub(1)) else {
            continue;
        };
        // Where the verbatim input expression lands in the generated line, and how
        // long (in chars) the verbatim run is.
        let Some((gcol, span_len)) = align_expr_span(vyx_line, region.origin.col, gen_line) else {
            continue;
        };
        for st in synth_toks.iter() {
            // Only tokens on the region's first generated line, wholly inside the
            // verbatim span, map cleanly back to the input.
            if st.line != region.gen_start_line || st.col < gcol {
                continue;
            }
            if st.col + st.len > gcol + span_len {
                continue;
            }
            out.push(vyrn_frontend::SemToken {
                line: region.origin.line,
                col: region.origin.col + (st.col - gcol),
                len: st.len,
                kind: st.kind,
                mods: st.mods,
            });
        }
    }
    // Overlapping regions (rare) could double-emit a position; keep one per spot.
    out.sort_by_key(|t| (t.line, t.col));
    out.dedup_by_key(|t| (t.line, t.col));
    out
}

/// Like [`align_expr`], but also returns the char length of the matched verbatim
/// run — the longest input tail (from `origin_col`) that occurs in `gen_line`.
/// `(1-based gen col, matched char length)`, or `None` when nothing aligns.
fn align_expr_span(vyx_line: &str, origin_col: usize, gen_line: &str) -> Option<(usize, usize)> {
    let tail: Vec<char> = vyx_line.chars().skip(origin_col.saturating_sub(1)).collect();
    let mut len = tail.len();
    while len >= 1 {
        let cand: String = tail[..len].iter().collect();
        if let Some(byte_idx) = gen_line.find(&cand) {
            return Some((gen_line[..byte_idx].chars().count() + 1, len));
        }
        len -= 1;
    }
    None
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

/// The document URI a `textDocument/*` request targets (all such requests carry
/// `textDocument.uri`). Used to trigger lazy `.vyx` owner discovery before the
/// request is answered (RFC-0049 §1).
fn request_uri(req: &Request) -> Option<Url> {
    let s = req.params.pointer("/textDocument/uri")?.as_str()?;
    Url::parse(s).ok()
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
    install_root(Some(connection), server, root_uri, &text, analysis);
}

/// Wire an owner root's freshly built `analysis` into the server: record the
/// ownership of every generator input it reads, cache the analysis, and — when a
/// `Connection` is given — publish the root's and inputs' diagnostics. Discovery
/// (RFC-0049) reuses this with `None` to wire an owner without publishing (it did
/// not originate from an open/change of that root).
fn install_root(
    connection: Option<&Connection>,
    server: &mut Server,
    root_uri: &Url,
    text: &str,
    analysis: Analysis,
) {
    if let Some(c) = connection {
        publish(c, root_uri, text, &analysis.diagnostics);
    }
    // Record which inputs this root synthesizes from; a discovered owner clears the
    // negative cache for its inputs (they are owned after all).
    for f in analysis.origins.input_files() {
        server.vyx_ownerless.remove(&f);
        server.vyx_owner.insert(f, root_uri.clone());
    }
    if let Some(c) = connection {
        publish_remapped(c, &analysis);
    }
    // A re-analysis of this owner invalidates any cached generation for it.
    server.synth_cache.borrow_mut().remove(root_uri);
    server.analyses.insert(root_uri.clone(), analysis);
}

// ---------------------------------------------------------------------------
// RFC-0049 §1 — `.vyx` owner discovery.
//
// A `.vyx` opened directly (the normal user action) has no `vyx_owner` entry
// until its owning `.vyrn` is analyzed. Discovery finds that owner from the
// `.vyx`'s path: locate the app root, rank the `.vyrn` files under it
// (generator-importing, directory-referencing ones first), analyze them
// nearest-first within a bound, and the owner is the one whose synthesized
// origins claim this `.vyx`. A genuine scratch `.vyx` is remembered as
// owner-less so discovery does not re-run per keystroke.
// ---------------------------------------------------------------------------

/// The most `.vyrn` roots discovery will analyze for one `.vyx` (a sane cap so a
/// large repo never triggers an unbounded scan).
const MAX_OWNER_CANDIDATES: usize = 48;
/// The most directory levels discovery walks up looking for an app root.
const MAX_WALK_UP: usize = 8;

/// Ensure `vyx_uri`'s owner is wired, discovering it if needed (no publishing —
/// the request path). A `.vyx` already owned, or already known owner-less, is a
/// cheap no-op.
fn ensure_vyx_owner(server: &mut Server, vyx_uri: &Url) {
    let Some(path) = uri_path(vyx_uri) else { return };
    if !path.ends_with(".vyx") {
        return;
    }
    if server.vyx_owner.contains_key(&path) || server.vyx_ownerless.contains(&path) {
        return;
    }
    match probe_owner(server, &path) {
        Some((owner, analysis)) => install_root(None, server, &owner, "", analysis),
        None => {
            server.vyx_ownerless.insert(path);
        }
    }
}

/// Discover and wire `vyx_uri`'s owner *with* diagnostics published (the didOpen
/// path). Returns whether an owner was found. A genuine scratch `.vyx` is cached
/// owner-less so a subsequent keystroke does not re-scan.
fn discover_vyx_owner(connection: &Connection, server: &mut Server, vyx_uri: &Url) -> bool {
    let Some(path) = uri_path(vyx_uri) else { return false };
    if server.vyx_owner.contains_key(&path) {
        return true;
    }
    if server.vyx_ownerless.contains(&path) {
        return false;
    }
    match probe_owner(server, &path) {
        Some((owner, analysis)) => {
            // Reuse the analysis probe_owner already built — publish its and the
            // inputs' diagnostics and wire ownership without generating a second
            // time (owner generation is the expensive step).
            let text = server
                .docs
                .get(&owner)
                .cloned()
                .or_else(|| uri_path(&owner).and_then(|p| std::fs::read_to_string(p).ok()))
                .unwrap_or_default();
            install_root(Some(connection), server, &owner, &text, analysis);
            server.vyx_owner.contains_key(&path)
        }
        None => {
            server.vyx_ownerless.insert(path);
            false
        }
    }
}

/// Analyze candidate `.vyrn` roots for `vyx_path` (ranked, bounded) and return
/// the first whose synthesized origins claim it, with its analysis. Pure: it
/// mutates nothing on the server (the caller wires the winner).
fn probe_owner(server: &Server, vyx_path: &str) -> Option<(Url, Analysis)> {
    let overlays = overlays_of(server);
    for cand in candidate_owners(vyx_path) {
        let text = match server
            .docs
            .get(&cand)
            .cloned()
            .or_else(|| uri_path(&cand).and_then(|p| std::fs::read_to_string(p).ok()))
        {
            Some(t) => t,
            None => continue,
        };
        let analysis = analyze_doc(&cand, &text, &overlays);
        if analysis.origins.input_files().iter().any(|f| f == vyx_path) {
            return Some((cand, analysis));
        }
    }
    None
}

/// The `.vyrn` roots to try as owners of `vyx_path`, most-likely first. Finds the
/// app root (nearest ancestor with `vyrn.json`, else the nearest ancestor holding
/// a generator-importing `.vyrn`, else the `.vyx`'s own directory), collects the
/// `.vyrn` files under it (bounded), and ranks them: a root that imports a page/
/// component generator AND names this `.vyx`'s directory first, then any
/// generator-importing root, then by path proximity.
fn candidate_owners(vyx_path: &str) -> Vec<Url> {
    let vyx = std::path::Path::new(vyx_path);
    let Some(vyx_dir) = vyx.parent() else { return Vec::new() };
    let dir_name = vyx_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let app_root = app_root_for(vyx_dir);

    let mut files = Vec::new();
    collect_vyrn(&app_root, &app_root, 0, &mut files);

    // Score each candidate from a cheap textual read (no analysis yet).
    let mut scored: Vec<(i32, usize, std::path::PathBuf)> = files
        .into_iter()
        .map(|p| {
            let src = std::fs::read_to_string(&p).unwrap_or_default();
            let generator = has_generator_import(&src);
            let names_dir = !dir_name.is_empty() && src.contains(&dir_name);
            let mut score = 0;
            if generator {
                score += 2;
            }
            if generator && names_dir {
                score += 4;
            }
            // Proximity: prefer a root in the `.vyx`'s directory or a near ancestor.
            let proximity = path_distance(&p, vyx_dir);
            (score, proximity, p)
        })
        .collect();
    // Higher score first; then nearer (smaller distance) first.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(MAX_OWNER_CANDIDATES)
        .filter_map(|(_, _, p)| Url::from_file_path(p).ok())
        .collect()
}

/// The app root for a `.vyx`'s directory: the nearest ancestor (within
/// [`MAX_WALK_UP`]) containing `vyrn.json`, else the nearest ancestor that holds
/// a generator-importing `.vyrn`, else `vyx_dir` itself.
fn app_root_for(vyx_dir: &std::path::Path) -> std::path::PathBuf {
    let mut fallback: Option<std::path::PathBuf> = None;
    let mut dir = vyx_dir.to_path_buf();
    for _ in 0..MAX_WALK_UP {
        if dir.join("vyrn.json").is_file() {
            return dir;
        }
        if fallback.is_none() && dir_has_generator_root(&dir) {
            fallback = Some(dir.clone());
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
    fallback.unwrap_or_else(|| vyx_dir.to_path_buf())
}

/// Whether `dir` directly contains a `.vyrn` file importing a page/component
/// generator — the "app root" signal when there is no `vyrn.json`.
fn dir_has_generator_root(dir: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) == Some("vyrn") {
            if let Ok(src) = std::fs::read_to_string(&p) {
                if has_generator_import(&src) {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether a `.vyrn` source imports one of the directory-consuming generators
/// (`pages`/`pagesThemed`/`components`/`componentsThemed`) — the roots that own
/// `.vyx` files.
fn has_generator_import(src: &str) -> bool {
    src.contains("pagesThemed")
        || src.contains("componentsThemed")
        || src.contains("pages(")
        || src.contains("components(")
        || src.contains("pages ")
        || src.contains("components ")
}

/// Recursively collect `.vyrn` files under `root` (skipping vendored/hidden and
/// build dirs), stopping once [`MAX_OWNER_CANDIDATES`] are gathered.
fn collect_vyrn(
    root: &std::path::Path,
    dir: &std::path::Path,
    depth: usize,
    out: &mut Vec<std::path::PathBuf>,
) {
    if out.len() >= MAX_OWNER_CANDIDATES || depth > MAX_WALK_UP {
        return;
    }
    let _ = root;
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            // Skip noise that never holds an owner root.
            if name.starts_with('.')
                || name == "vyrn_vendor"
                || name == "target"
                || name == "node_modules"
                || name == "public"
            {
                continue;
            }
            subdirs.push(p);
        } else if p.extension().and_then(|x| x.to_str()) == Some("vyrn") {
            out.push(p);
        }
    }
    for sub in subdirs {
        collect_vyrn(root, &sub, depth + 1, out);
        if out.len() >= MAX_OWNER_CANDIDATES {
            return;
        }
    }
}

/// A rough directory distance between a candidate `.vyrn` and the `.vyx`'s dir:
/// the number of path components not in their common prefix (nearer = smaller).
fn path_distance(cand: &std::path::Path, vyx_dir: &std::path::Path) -> usize {
    let a: Vec<_> = cand.parent().unwrap_or(cand).components().collect();
    let b: Vec<_> = vyx_dir.components().collect();
    let common = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    (a.len() - common) + (b.len() - common)
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
/// its remapped diagnostics refresh from the edited input). RFC-0049: a `.vyx`
/// with no known owner triggers owner discovery (opening it standalone is the
/// normal action) rather than being silently stored.
fn refresh_document(connection: &Connection, server: &mut Server, uri: &Url) {
    if is_vyrn_uri(uri) {
        // A `.vyrn` open/change may have introduced (or fixed) an owner — allow a
        // previously owner-less `.vyx` to be re-discovered.
        server.vyx_ownerless.clear();
        reanalyze_root(connection, server, uri);
    } else if let Some(owner) = uri_path(uri).and_then(|p| server.vyx_owner.get(&p)).cloned() {
        reanalyze_root(connection, server, &owner);
    } else {
        discover_vyx_owner(connection, server, uri);
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
            // An explicit (re)open of a `.vyx` retries owner discovery even if a
            // prior attempt cached it owner-less (RFC-0049 §1).
            if let Some(p) = uri_path(&uri) {
                server.vyx_ownerless.remove(&p);
            }
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