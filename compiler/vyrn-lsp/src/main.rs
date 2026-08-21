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

mod contracts;
mod rename;
mod templates;

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CompletionItem, CompletionItemKind, CompletionOptions,
    CompletionParams, CompletionResponse, CompletionTextEdit, Diagnostic as LspDiagnostic,
    DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentFormattingParams, DocumentHighlight, DocumentHighlightKind,
    DocumentHighlightParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse,
    Documentation, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
    InitializeParams, InitializeResult, InlayHint, InlayHintKind, InlayHintLabel, InlayHintParams,
    InsertTextFormat, Location, MarkupContent, MarkupKind, OneOf, Position, PrepareRenameResponse,
    PublishDiagnosticsParams, Range, RenameOptions, RenameParams, SemanticToken,
    SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams, SemanticTokensRangeParams,
    SemanticTokensRangeResult, SemanticTokensResult, SemanticTokensServerCapabilities,
    ServerCapabilities, ServerInfo, TextDocumentPositionParams, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Url, WorkspaceEdit,
};

use vyrn_frontend::symbolmap::MappedSymbol;
use vyrn_frontend::{
    analyze, class_completions, class_token_hover, completions, member_completions, references,
    resolve, string_literal_completions, Analysis, Completion, LocalKind, RefRange, SemKind,
    SemMods, SymbolKind,
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
    let (opts, resolver, path, manifest_error) = match load_context(uri, overlays) {
        Some(ctx) => ctx,
        None => return analyze(text),
    };
    let mut analysis = vyrn_frontend::analyze_linked(text, &path, &opts, &resolver);
    // A manifest that exists and does not parse is the one state the editor may
    // not answer from: the import map and the audience vocabulary both come out
    // of that file, and dropping them silently makes the editor agree with a
    // build that is about to refuse. Reported against the open document, the way
    // an error in an imported file is.
    if let Some(e) = manifest_error {
        analysis.diagnostics.insert(
            0,
            vyrn_frontend::diagnostics::Diagnostic {
                file: None,
                line: 1,
                col: 0,
                end_col: 0,
                severity: vyrn_frontend::diagnostics::Severity::Error,
                stage: "parse",
                message: e,
                note: Some(
                    "note: the project's import map and audience rules cannot be read, \
                     so this file is analyzed without them"
                        .to_string(),
                ),
                from_generated: false,
            },
        );
    }
    analysis
}

/// Build the load options + overlay-aware resolver + slash path for `uri`, or
/// `None` for an untitled buffer with no filesystem path.
fn load_context(
    uri: &Url,
    overlays: &HashMap<String, String>,
) -> Option<(
    vyrn_frontend::loader::LoadOptions,
    EditorResolver,
    String,
    Option<String>,
)> {
    let path = uri
        .to_file_path()
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let mut opts = vyrn_frontend::loader::LoadOptions {
        std_root: std_root(),
        ..Default::default()
    };
    let found = match std::path::Path::new(&path).parent() {
        Some(d) => find_manifest(d),
        None => Ok(None),
    };
    let mut manifest_error = None;
    let manifest_dir = match found {
        Ok(m) => m.map(|m| {
            opts.aliases = m.dependencies.into_iter().collect();
            opts.alias_base = m.dir.clone();
            opts.audience = m.audience;
            opts.artifacts = m.artifacts;
            m.dir
        }),
        Err(e) => {
            manifest_error = Some(e);
            None
        }
    };
    let resolver = EditorResolver {
        manifest_dir,
        overlays: overlays.clone(),
    };
    Some((opts, resolver, path, manifest_error))
}

/// The project context — `vyrn.json`, `vyrn.lock`, the content-addressed caches
/// and the `std/` root — is read by [`vyrn_frontend::manifest`], the same
/// reader `vyrn` uses.
///
/// It used to be a compact duplicate here, justified by "the CLI is a binary
/// crate, not linkable". That is true and it was never a reason to have two
/// readers, only a reason the reader could not stay in the CLI. The duplicate
/// drifted exactly where a duplicate does: it served a cached module without
/// checking that its bytes still hash to the pin, and it accepted a `vyrn.lock`
/// the build refuses. An editor that answers a different question from the
/// build is worse than an editor that answers none.
use vyrn_frontend::manifest::{find as find_manifest, pinned_blob, std_root, Lock};

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
            // Overlay keys come from `uri_path` (normalized); `resolved` comes
            // from the loader (original case) — normalize or unsaved edits miss.
            if let Some(text) = self
                .overlays
                .get(&vyrn_frontend::origin::OriginMaps::norm_path_key(resolved))
            {
                return Ok(text.clone());
            }
            return std::fs::read_to_string(resolved).map_err(|e| e.to_string());
        }
        let dir = self
            .manifest_dir
            .as_deref()
            .ok_or_else(|| "remote import outside a vyrn.json project".to_string())?;
        // The lock through the reader that refuses a damaged one. A lock the
        // build will not accept must not analyze as if it were fine: this file
        // used to split the TSV inline, which took the FIRST of two pins for one
        // specifier and read a spaces-for-tabs line as "never pinned".
        let (_, sha) = Lock::in_project(dir)?
            .entries
            .get(resolved)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "`{resolved}` is not pinned in vyrn.lock — run `vyrn check` once to fetch it"
                )
            })?;
        // Vendor, then the user cache, hash-verified — the same read the build
        // does. A cached blob whose bytes no longer hash to the pin is the one
        // case content-addressing exists to catch, and the editor used to serve
        // it without looking.
        pinned_blob(Some(dir), &sha).unwrap_or_else(|| {
            Err(format!(
                "`{resolved}` is pinned but not cached — run `vyrn check` once to fetch it"
            ))
        })
    }

    /// Generation-time `listDir` (RFC-0021): read the local directory. The
    /// generator's inputs are local files, so this is a plain read-only listing.
    fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
        let entries = std::fs::read_dir(resolved)
            .map_err(|_| vyrn_frontend::trap::io_at("listerr", resolved))?;
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
        vyrn_frontend::manifest::gen_cache_get(key)
    }
    fn gen_cache_put(&self, key: &str, value: &str) {
        vyrn_frontend::manifest::gen_cache_put(key, value)
    }
}

/// Diagnostic trace: append a line to `$VYRN_LSP_LOG`, else
/// `%TEMP%/vyrn-lsp-debug.log`. Always on and configuration-free so a user can
/// produce a trace by just opening a file — the editor-integration bugs of
/// RFC-0047..0050 were all invisible from outside the running editor.
fn dbg_log(msg: &str) {
    use std::io::Write;
    let path = std::env::var("VYRN_LSP_LOG").unwrap_or_else(|_| {
        let tmp = std::env::var("TEMP")
            .or_else(|_| std::env::var("TMPDIR"))
            .unwrap_or_else(|_| ".".into());
        format!("{tmp}/vyrn-lsp-debug.log")
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{msg}");
    }
}

fn main() {
    // RFC-0076 M4. Choosing an engine, not adding analysis: the generator still
    // runs the same Vyrn program and still returns the same source. This is the
    // process the engine exists for — a compiled artifact is argument-independent
    // and cached for the session, so the clang is paid once per generator instead
    // of once per keystroke. Installed before any document can be opened, since
    // generation happens deep inside the load, and it declines to the interpreter
    // when there is no clang or no wasi sysroot, so an editor on a machine
    // without a toolchain is slower and never broken. `VYRN_NO_WASM_GEN=1`
    // forces the interpreter, spelled exactly as the CLI spells it.
    #[cfg(feature = "wasm-gen")]
    if std::env::var("VYRN_NO_WASM_GEN").is_err() {
        vyrn_genwasm::install();
    }

    // `Connection::stdio` sets up the stdin/stdout channels. The server is
    // single-threaded and blocking — no tokio, no I/O threads.
    let (connection, io_threads) = Connection::stdio();
    dbg_log(&format!(
        "=== vyrn-lsp start pid={} cwd={:?} ===",
        std::process::id(),
        std::env::current_dir().ok()
    ));

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
                css_cache: RefCell::new(HashMap::new()),
                contract_cache: RefCell::new(HashMap::new()),
                route_facts: RefCell::new(HashMap::new()),
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
    /// RFC-0052: per app root, the app's own stylesheets (path + text) used to
    /// answer a safelisted-class hover. Keyed by app root; refreshed only when the
    /// cheap signature (each file's len+mtime, plus the app-root and `public/`
    /// directory mtimes) changes, so a tooltip never re-walks/re-reads the disk.
    css_cache: RefCell<HashMap<std::path::PathBuf, CssIndex>>,
    /// RFC-0071 M4: per app root, the roles the project declares (or the ones
    /// discovered from its generator call sites) and the contracts they resolve
    /// to. Re-derived only when `vyrn.json` or a root module changes, and each
    /// cached contract re-checks its own declaring file — so editing
    /// `std/ui.vyrn` is picked up without restarting the server, and a keystroke
    /// in a page re-reads nothing.
    contract_cache: RefCell<HashMap<std::path::PathBuf, contracts::ContractIndex>>,
    /// RFC-0073 M3: per api-module path, the mapped symbols a generating root
    /// claims for it — the answer to "what is this procedure mounted at" for a
    /// file that reaches no generator itself. The empty answer is cached too, so
    /// a module nothing mounts costs one probe rather than one per hover.
    /// Refreshed by [`install_root`] whenever a root that mounts a surface is
    /// analyzed — the authority on the answer, and the only invalidation this
    /// needs (see the note there).
    route_facts: RefCell<HashMap<String, Rc<Vec<MappedSymbol>>>>,
}

/// RFC-0052: one app root's discovered stylesheets, with the signature they were
/// read at.
struct CssIndex {
    sig: u64,
    /// `(absolute path, file text)` in discovery order (declared order first).
    files: Vec<(std::path::PathBuf, String)>,
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
                [".", "<", "@", ":", "-", " ", "\""]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            ..Default::default()
        }),
        document_symbol_provider: Some(OneOf::Left(true)),
        // RFC-0071 M4: the did-you-mean rename on an export a closed contract
        // does not name (`laod` → `data`). The only code action the server
        // offers, so the capability is advertised plainly rather than filtered
        // by kind.
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        // Scope-aware highlight (RFC-0050 §1): the handler resolves the binding
        // under the cursor and returns its ACTUAL references (not a word-match),
        // so registering this overrides VS Code's dumb textual occurrence
        // highlighting — comments and out-of-scope same-named bindings are excluded.
        document_highlight_provider: Some(OneOf::Left(true)),
        // RFC-0073 M4: cross-boundary rename. `prepare` is advertised because the
        // refusal is the interesting half — a cursor that is not on a top-level
        // declaration, or on a generated name at a call site, is told so BEFORE
        // the user types a replacement, instead of after.
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        // Whole-document formatting (RFC-0017): the handler runs `vyrn_frontend::fmt`
        // and returns one full-range replace. VS Code format-on-save then works.
        document_formatting_provider: Some(OneOf::Left(true)),
        // Semantic tokens (RFC-0047 §1): the server classifies every identifier
        // from the cached `Analysis` (function vs type vs variable vs …), which
        // TextMate cannot distinguish. `full` + `range` are both served.
        // RFC-0087 U1: move hints. A move is the one memory event with a source
        // position of its own, and the census's root gap is that nothing in the
        // source says where a value went. No resolve provider — the label is
        // already in the cached analysis, so there is nothing to resolve.
        inlay_hint_provider: Some(OneOf::Left(true)),
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
        server_info: Some(ServerInfo {
            name: "vyrn-lsp".into(),
            version: Some("0.1.0".into()),
        }),
    };
    let value = serde_json::to_value(result).unwrap();
    connection.initialize_finish(id, value).map_err(|_| ())?;
    Ok(())
}

fn main_loop(connection: &Connection, server: &mut Server) {
    // URIs whose analysis is owed. Held only while more messages are already
    // waiting — the moment the queue is empty they are analyzed, so nothing is
    // delayed on an idle connection and no timer is involved.
    let mut owed: Vec<Url> = Vec::new();
    loop {
        let msg = if owed.is_empty() {
            match connection.receiver.recv() {
                Ok(m) => m,
                Err(_) => return,
            }
        } else {
            // Something is owed: take whatever is ALREADY queued first, so a
            // burst collapses. When nothing is left, settle the debt.
            match connection.receiver.try_recv() {
                Ok(m) => m,
                Err(_) => {
                    for uri in std::mem::take(&mut owed) {
                        refresh_document(connection, server, &uri);
                    }
                    continue;
                }
            }
        };
        match msg {
            Message::Request(req) => {
                // `handle_shutdown` replies to `shutdown` and returns true; it
                // does not exit the process — we return so `main` can finish
                // and the io threads can drain.
                if connection.handle_shutdown(&req).unwrap_or(false) {
                    return;
                }
                // A request reads the analysis, so settle anything owed first.
                for uri in std::mem::take(&mut owed) {
                    refresh_document(connection, server, &uri);
                }
                let m = req.method.clone();
                let u = request_uri(&req).map(|u| u.to_string()).unwrap_or_default();
                dbg_log(&format!("REQ  {m} uri={u}"));
                let resp = handle_request(server, req);
                let empty = match &resp.result {
                    None => true,
                    Some(serde_json::Value::Null) => true,
                    Some(serde_json::Value::Array(a)) => a.is_empty(),
                    _ => false,
                };
                dbg_log(&format!(
                    "RESP {m} -> {}{}",
                    if empty { "EMPTY/null" } else { "ok" },
                    resp.error
                        .as_ref()
                        .map(|e| format!(" ERROR {}", e.message))
                        .unwrap_or_default()
                ));
                let _ = connection.sender.send(Message::Response(resp));
            }
            Message::Notification(notif) => {
                dbg_log(&format!(
                    "NOTIF {} {}",
                    notif.method,
                    notif
                        .params
                        .get("textDocument")
                        .map(|d| format!(
                            "uri={} languageId={}",
                            d.get("uri").and_then(|v| v.as_str()).unwrap_or("?"),
                            d.get("languageId").and_then(|v| v.as_str()).unwrap_or("-")
                        ))
                        .unwrap_or_default()
                ));
                match handle_notification(connection, server, notif) {
                    // Newest wins: one entry per document, refreshed once.
                    Owed::Analyze(uri) => {
                        owed.retain(|u| u != &uri);
                        owed.push(uri);
                    }
                    // A close cancels the analysis it would have raced.
                    Owed::Forget(uri) => owed.retain(|u| u != &uri),
                    Owed::Nothing => {}
                }
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
            dbg_log(&format!(
                "  vyx owner for {} => {:?} (ownerless={})",
                uri.path(),
                uri_path(&uri)
                    .and_then(|p| server.vyx_owner.get(&p))
                    .map(|u| u.to_string()),
                uri_path(&uri)
                    .map(|p| server.vyx_ownerless.contains(&p))
                    .unwrap_or(false)
            ));
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
        "textDocument/definition" => {
            Response::new_ok(req.id, handle_definition(server, req.params))
        }
        "textDocument/completion" => {
            Response::new_ok(req.id, handle_completion(server, req.params))
        }
        "textDocument/documentSymbol" => {
            Response::new_ok(req.id, handle_document_symbol(server, req.params))
        }
        "textDocument/documentHighlight" => {
            Response::new_ok(req.id, handle_document_highlight(server, req.params))
        }
        // RFC-0071 M4: the contract's did-you-mean rename.
        "textDocument/codeAction" => {
            Response::new_ok(req.id, handle_code_action(server, req.params))
        }
        // RFC-0073 M4: rename, and the pre-flight that refuses early. Both
        // answer an ERROR rather than a null when there is nothing to rename —
        // the client shows the message, which is the whole point of saying why.
        "textDocument/prepareRename" => match handle_prepare_rename(server, req.params) {
            Ok(r) => Response::new_ok(req.id, Some(r)),
            Err(msg) => Response::new_err(req.id, -32803 /* RequestFailed */, msg),
        },
        "textDocument/rename" => match handle_rename(server, req.params) {
            Ok(e) => Response::new_ok(req.id, Some(e)),
            Err(msg) => Response::new_err(req.id, -32803, msg),
        },
        "textDocument/formatting" => {
            Response::new_ok(req.id, handle_formatting(server, req.params))
        }
        // RFC-0087 U1: a `-> f(..)` label at every move.
        "textDocument/inlayHint" => Response::new_ok(req.id, handle_inlay_hint(server, req.params)),
        "textDocument/semanticTokens/full" => {
            Response::new_ok(req.id, handle_semantic_tokens_full(server, req.params))
        }
        "textDocument/semanticTokens/range" => {
            Response::new_ok(req.id, handle_semantic_tokens_range(server, req.params))
        }
        // RFC-0064: a cheap predicate the extension queries to decide whether to
        // render the "▶ Run dev server" CodeLens. Always answers a bool (never
        // leaves the client waiting).
        "vyrn/isDevEntry" => {
            Response::new_ok(req.id, Some(handle_is_dev_entry(server, req.params)))
        }
        // RFC-0073 M3: the derived route of every procedure this document
        // declares, for the CodeLens above each one. A custom request and not a
        // `code_lens_provider`, because every lens this editor shows — the
        // RFC-0064 dev entry, the RFC-0055 bench lenses, the run lens — is built
        // in `extension.js` from an answer like this one, and one lens source is
        // worth more than the capability.
        "vyrn/routeLenses" => {
            Response::new_ok(req.id, Some(handle_route_lenses(server, req.params)))
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

/// RFC-0064: `vyrn/isDevEntry` — is `params.textDocument.uri` a **dev-server
/// entry**, i.e. the exact kind of file `vyrn dev` is meant for? Answers a plain
/// bool. Reads the open buffer (else disk) and runs [`is_dev_entry`] on it.
fn handle_is_dev_entry(server: &Server, params: serde_json::Value) -> bool {
    let Some(uri) = params.pointer("/textDocument/uri").and_then(|v| v.as_str()) else {
        return false;
    };
    let Ok(uri) = Url::parse(uri) else {
        return false;
    };
    if !is_vyrn_uri(&uri) {
        return false;
    }
    let src = server
        .docs
        .get(&uri)
        .cloned()
        .or_else(|| uri_path(&uri).and_then(|p| std::fs::read_to_string(p).ok()));
    match src {
        Some(text) => is_dev_entry(&text),
        None => false,
    }
}

/// RFC-0073 M3: `vyrn/routeLenses` — one entry per procedure `params
/// .textDocument.uri` declares that a generator mounts, as
/// `{ line, title, method, path, source }` with a 0-based `line` (the editor's
/// own convention). Empty for a file no root generates over.
fn handle_route_lenses(server: &Server, params: serde_json::Value) -> Vec<serde_json::Value> {
    let Some(uri) = params
        .pointer("/textDocument/uri")
        .and_then(|v| v.as_str())
        .and_then(|u| Url::parse(u).ok())
    else {
        return Vec::new();
    };
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut seen: Vec<(usize, String)> = Vec::new();
    for m in route_facts(server, &uri).iter() {
        let (Some(path), Some(title)) = (m.derived("path"), m.route_line()) else {
            continue;
        };
        // A procedure is mapped twice — once by the client's stub, once by the
        // server's handler — and they name the same declaration at the same
        // place. One lens per declaration, not one per generated symbol.
        let key = (m.line, path.to_string());
        if m.line == 0 || seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(serde_json::json!({
            "line": m.line - 1,
            "title": title.replace('`', ""),
            "method": m.derived("method").unwrap_or("POST"),
            "path": path,
            "source": m.derived("source").unwrap_or("convention"),
        }));
    }
    out.sort_by_key(|v| v.get("line").and_then(|l| l.as_u64()).unwrap_or(0));
    out
}

/// The dev-entry predicate (RFC-0064): the root module imports `std/rpc` **and**
/// mounts a SERVER surface from it — `rpc("./server/api")` (RFC-0072 M3) or the
/// single-module `rpcServer(…)`. That is precisely the set of server roots
/// `vyrn dev` builds+serves — a client (`client` / `rpcClient`), an in-process
/// module (`clientInProcess` / `rpcInProcess`), a library, and a CLI example are
/// all excluded.
///
/// Deviation from the RFC letter (documented in RFC-0064 "As landed"): the RFC
/// wrote the predicate as "calls `serve(` from std/rpc", but `std/rpc` exposes no
/// `serve` — a server root is composed by importing from the mounting GENERATOR
/// (`import { rpcHandle } from rpc("./server/api")`). The generator import IS the
/// "import present + call site" the RFC describes, so the mounting generator is
/// the real spelling of `serve`.
///
/// Cheap on purpose: a lex+parse of the ROOT source only (no linking, no
/// generation), so `program.imports` is exactly the root module's imports.
fn is_dev_entry(source: &str) -> bool {
    use vyrn_frontend::ast::ImportSource;
    let Ok(tokens) = vyrn_frontend::lexer::lex(source) else {
        return false;
    };
    let Ok(program) = vyrn_frontend::parser::parse(tokens) else {
        return false;
    };
    let imports_rpc = program
        .imports
        .iter()
        .any(|i| matches!(&i.source, ImportSource::Path(p) if p == "std/rpc"));
    let calls_server = program
        .imports
        .iter()
        .any(|i| matches!(&i.source, ImportSource::Generator { name, .. } if name == "rpc" || name == "rpcServer"));
    imports_rpc && calls_server
}

/// Fence a hover's signature line as ` ```vyrn `, so the editor highlights it
/// with the grammar the extension already ships. `symbols.rs` builds hover text
/// as PLAIN prose — its own tests pin those strings exactly, so the fence is
/// presentation and lives here, at the adapter, like every other LSP concern.
///
/// The hover convention is "signature, blank line, docs". Two shapes carry a
/// signature: a first paragraph that starts with a declaration keyword
/// (`fn tally(..) -> Int64\n\nwhat it does`), and a builtin method's one-liner
/// whose prose follows an em dash (`x.copy() -> T — a deep copy ..`). Anything
/// else — a Tw class note, a contract note, plain prose — is left alone.
fn fence_signature(hover: &str) -> String {
    let (head, rest) = match hover.find("\n\n") {
        Some(i) => (&hover[..i], &hover[i..]),
        None => (hover, ""),
    };
    const DECL: &[&str] = &[
        "fn ",
        "gen fn ",
        "mut fn ",
        "type ",
        "let ",
        "protocol ",
        "impl ",
        "place ",
        "contract ",
    ];
    if DECL.iter().any(|d| head.starts_with(d)) {
        return format!("```vyrn\n{head}\n```{rest}");
    }
    // A builtin method detail: `x.copy() -> T — a deep copy of ..`. The dash
    // separates signature from prose, so the fence takes the left half only.
    if let Some((sig, doc)) = head.split_once(" — ") {
        if !sig.contains('\n') && sig.contains('(') && sig.contains("->") {
            return format!("```vyrn\n{sig}\n```\n\n{doc}{rest}");
        }
    }
    hover.to_string()
}

fn handle_hover(server: &Server, params: serde_json::Value) -> Option<Hover> {
    let p: HoverParams = serde_json::from_value(params).ok()?;
    let uri = &p.text_document_position_params.text_document.uri;
    let text = doc_text(server, uri)?;
    let (line, col) = to_frontend(&text, &p.text_document_position_params.position);
    // RFC-0033: a request inside a generator input file (`.vyx`) is answered
    // against the synthesized module at the mapped generated position. RFC-0042:
    // when nothing resolves (the cursor is on a class token inside a string, not an
    // identifier), fall back to `Tw` class hover — the CSS rule `css()` emits, or
    // "safelisted (app-styled)".
    // RFC-0071 M4: the contract note for a member declaration under the cursor.
    // Computed first because in a `.vyx` there may be no ordinary hover to
    // attach it to — a `<script>`'s own declarations are not template
    // expressions, so the forward map has nothing for them.
    // RFC-0073 M3: the derived wire facts of a procedure DECLARATION, which only
    // a root that generates over its module can supply. Computed after the
    // contract note and joined with it, so a declaration that is both a contract
    // member and a mounted procedure says both.
    let note = match (
        contract_hover_note(server, uri, line, col),
        derived_hover_note(server, uri, line, col),
    ) {
        (Some(c), Some(d)) => Some(format!("{c}\n\n{d}")),
        (c, d) => c.or(d),
    };
    let ordinary = if is_vyrn_uri(uri) {
        lookup(server, uri).and_then(|(analysis, _)| match resolve(analysis, line, col) {
            Some(r) => Some(r.hover),
            None => server
                .docs
                .get(uri)
                .and_then(|src| class_token_hover(analysis, src, line, col)),
        })
    } else {
        vyx_forward(server, uri, line, col).and_then(|fwd| {
            match resolve(&fwd.synth.analysis, fwd.line, fwd.col) {
                Some(r) => Some(r.hover),
                None => class_token_hover(
                    &fwd.synth.analysis,
                    &fwd.synth.gen_source,
                    fwd.line,
                    fwd.col,
                ),
            }
        })
    };
    let ordinary = ordinary.map(|o| fence_signature(&o));
    let value = match (ordinary, note) {
        (Some(o), Some(n)) => format!("{o}\n\n---\n\n{n}"),
        (Some(o), None) => o,
        (None, Some(n)) => n,
        (None, None) => return None,
    };
    // RFC-0052: a safelisted class generates no `std/tw` rule, but the app itself
    // styles it — append the matching rule(s) from the app's own stylesheet(s).
    let value = with_app_css(server, uri, value);
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
    let text = doc_text(server, uri)?;
    let (line, col) = to_frontend(&text, &p.text_document_position_params.position);
    // RFC-0033: from a `.vyx` template expression, resolve through the
    // synthesized module. Only an IMPORTED declaration (with a real source file)
    // is a useful jump target — a binding local to the synthesized module has no
    // on-disk location, so it yields no definition (v1).
    // RFC-0050 §2: a cursor inside an import SOURCE STRING (a plain spec like
    // `"./store"` / `"std/time"`, or a generator-call argument) jumps to the
    // resolved file, resolved through the very loader the linker uses.
    if is_vyrn_uri(uri) {
        if let Some(loc) = import_path_definition(server, uri, line, col) {
            return Some(loc);
        }
    }
    // RFC-0071 M4: on a contract member's name at module scope, jump to the
    // MEMBER in the contract. This wins over the ordinary resolution because
    // that one would be a self-jump — the cursor is on the declaration it
    // resolves to — and the contract is the thing the reader actually wants to
    // see. A use of the same name inside a body is not at module scope, so it
    // keeps resolving to the page's own declaration.
    if let Some(loc) = contract_member_definition(server, uri, line, col) {
        return Some(loc);
    }
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
    let target_text = doc_text(server, &target_uri).unwrap_or_default();
    Some(GotoDefinitionResponse::Scalar(Location {
        uri: target_uri,
        range: lsp_range(&target_text, r.target_line, r.target_col, r.target_end_col),
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
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 0,
            },
        },
    }))
}

/// RFC-0050 §2: resolve a `GotoDefinition` on the import SOURCE STRING under the
/// cursor to the imported file's `Location` (top of file). The specifier is
/// identified by the frontend ([`vyrn_frontend::import_spec_at`]) and resolved by
/// the very loader the linker uses ([`vyrn_frontend::loader::resolve_spec`]) —
/// no second copy of the path logic. Remote/uncached specifiers yield nothing,
/// quietly.
fn import_path_definition(
    server: &Server,
    uri: &Url,
    line: usize,
    col: usize,
) -> Option<GotoDefinitionResponse> {
    let src = server
        .docs
        .get(uri)
        .cloned()
        .or_else(|| uri_path(uri).and_then(|p| std::fs::read_to_string(p).ok()))?;
    let spec = vyrn_frontend::import_spec_at(&src, line, col)?;
    // A remote specifier is not a jumpable local file.
    if vyrn_frontend::loader::is_remote(&spec) {
        return None;
    }
    let overlays = overlays_of(server);
    let (opts, _resolver, importer, _) = load_context(uri, &overlays)?;
    let target = import_target_file(&spec, &importer, &opts)?;
    let url = Url::from_file_path(target.replace('/', std::path::MAIN_SEPARATOR_STR)).ok()?;
    Some(GotoDefinitionResponse::Scalar(Location {
        uri: url,
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 0,
            },
        },
    }))
}

/// The local file a resolved import specifier names. A plain spec resolves to a
/// `.vyrn`/`.json` file directly; a generator DIRECTORY argument (`"./widgets"`)
/// resolves to the `.vyrn` guess that doesn't exist, so it falls back to the
/// directory the spec names — an entry file inside it, else the directory itself
/// (RFC-0050 §2). `None` when nothing on disk matches.
fn import_target_file(
    spec: &str,
    importer: &str,
    opts: &vyrn_frontend::loader::LoadOptions,
) -> Option<String> {
    let resolved = vyrn_frontend::loader::resolve_spec(spec, importer, opts).ok()?;
    if std::path::Path::new(&resolved).is_file() {
        return Some(resolved);
    }
    // The extension-less `.vyrn` guess didn't exist: the spec may name a
    // directory a generator consumes.
    let dir = resolved.strip_suffix(".vyrn").unwrap_or(&resolved);
    dir_target(dir)
}

/// A jump target for a directory a generator consumes: a same-named or `index`
/// entry file, else the first `.vyx`/`.vyrn` inside, else the directory itself.
/// `None` when `dir` is not a directory.
fn dir_target(dir: &str) -> Option<String> {
    let p = std::path::Path::new(dir);
    if !p.is_dir() {
        return None;
    }
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for cand in [
        format!("{name}.vyx"),
        format!("{name}.vyrn"),
        "index.vyx".into(),
        "index.vyrn".into(),
    ] {
        let f = p.join(&cand);
        if f.is_file() {
            return Some(f.to_string_lossy().replace('\\', "/"));
        }
    }
    if let Ok(rd) = std::fs::read_dir(p) {
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path().to_string_lossy().replace('\\', "/"))
            .filter(|s| s.ends_with(".vyx") || s.ends_with(".vyrn"))
            .collect();
        names.sort();
        if let Some(f) = names.into_iter().next() {
            return Some(f);
        }
    }
    Some(dir.to_string())
}

/// RFC-0050 §1: `textDocument/documentHighlight`. Resolves the binding under the
/// cursor and returns its ACTUAL references (scope-aware) — not a textual
/// word-match. The defining occurrence is `Write`, uses are `Read`. Always
/// returns `Some` (possibly empty) for an open document so VS Code does not fall
/// back to word-matching; a `.vyx` maps the synthesized module's references back
/// through the origin map.
fn handle_document_highlight(
    server: &Server,
    params: serde_json::Value,
) -> Option<Vec<DocumentHighlight>> {
    let p: DocumentHighlightParams = serde_json::from_value(params).ok()?;
    let uri = &p.text_document_position_params.text_document.uri;
    let text = doc_text(server, uri).unwrap_or_default();
    let (line, col) = to_frontend(&text, &p.text_document_position_params.position);
    let refs = if is_vyrn_uri(uri) {
        let (analysis, _) = lookup(server, uri)?;
        references(analysis, line, col)
    } else {
        vyx_highlights(server, uri, line, col)
    };
    Some(
        refs.into_iter()
            .map(|r| DocumentHighlight {
                range: lsp_range(&text, r.line, r.col, r.end_col),
                kind: Some(if r.write {
                    DocumentHighlightKind::WRITE
                } else {
                    DocumentHighlightKind::READ
                }),
            })
            .collect(),
    )
}

/// RFC-0050 §1 in a `.vyx`: forward the cursor into the synthesized module,
/// compute references there, and map each back into the input buffer's
/// coordinates through the verbatim origin regions (the inverse of
/// [`vyx_semantic_tokens`]). Best-effort: only references inside a verbatim
/// region map cleanly.
fn vyx_highlights(server: &Server, vyx_uri: &Url, line: usize, col: usize) -> Vec<RefRange> {
    let Some(fwd) = vyx_forward(server, vyx_uri, line, col) else {
        return Vec::new();
    };
    let refs = references(&fwd.synth.analysis, fwd.line, fwd.col);
    let Some(vyx_path) = uri_path(vyx_uri) else {
        return Vec::new();
    };
    let Some(owner) = server.vyx_owner.get(&vyx_path).cloned() else {
        return Vec::new();
    };
    let Some(owner_analysis) = server.analyses.get(&owner) else {
        return Vec::new();
    };
    let Some(vyx_text) = server
        .docs
        .get(vyx_uri)
        .cloned()
        .or_else(|| std::fs::read_to_string(&vyx_path).ok())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for region in owner_analysis.origins.regions_for(&vyx_path) {
        let Some(vyx_line) = vyx_text.lines().nth(region.origin.line.saturating_sub(1)) else {
            continue;
        };
        let Some(gen_line) = fwd
            .synth
            .gen_source
            .lines()
            .nth(region.gen_start_line.saturating_sub(1))
        else {
            continue;
        };
        let Some((gcol, span_len)) = align_expr_span(vyx_line, region.origin.col, gen_line) else {
            continue;
        };
        for r in &refs {
            if r.line != region.gen_start_line || r.col < gcol {
                continue;
            }
            if r.end_col > gcol + span_len {
                continue;
            }
            out.push(RefRange {
                line: region.origin.line,
                col: region.origin.col + (r.col - gcol),
                end_col: region.origin.col + (r.end_col - gcol),
                write: r.write,
            });
        }
    }
    out.sort_by_key(|r| (r.line, r.col));
    out.dedup_by_key(|r| (r.line, r.col));
    out
}

fn handle_completion(server: &Server, params: serde_json::Value) -> Option<CompletionResponse> {
    let p: CompletionParams = serde_json::from_value(params).ok()?;
    let uri = &p.text_document_position.text_document.uri;
    let raw = doc_text(server, uri)?;
    let (line, col) = to_frontend(&raw, &p.text_document_position.position);
    if !is_vyrn_uri(uri) {
        return vyx_completion(server, uri, line, col);
    }
    let (analysis, _uri) = lookup(server, uri)?;
    // A `.foo` member access → context-aware completions for the receiver's type
    // (e.g. `arr.` → push/at/pop/length). Otherwise → all top-level
    // symbols; the client filters by the prefix the user typed.
    // RFC-0020 M1 / RFC-0042: inside a string literal whose expected type is a
    // finite string type, offer its language (`t("` → every key); whose expected
    // type is a sequence type (`theme.cls("…")` → `Tw`), offer the class alphabet
    // as token-in-sequence replacements. Falls back to member / top-level.
    if is_string_literal_context(Some(&raw), line, col) {
        if let Some(cls) = class_completions(analysis, &raw, line, col) {
            return Some(class_completion_response(&raw, line, col, cls));
        }
        let items = string_literal_completions(analysis, &raw, line, col)
            .into_iter()
            .map(to_completion_item)
            .collect();
        return Some(CompletionResponse::Array(items));
    }
    let mut items: Vec<CompletionItem> = if is_member_context(Some(&raw), line, col) {
        member_completions(analysis, line, col)
    } else {
        completions(analysis)
    }
    .into_iter()
    .map(to_completion_item)
    .collect();
    // RFC-0071 M4: at module scope in a page (or any file a role attaches a
    // contract to), the contract's members come first — they are the thing you
    // are there to write, and each inserts its full declaration.
    if !is_member_context(Some(&raw), line, col) {
        let mut members = contract_completion_items(server, uri, line, col);
        members.append(&mut items);
        items = members;
    }
    // Always return a list (possibly empty) — the client filters by prefix.
    Some(CompletionResponse::Array(items))
}

/// Completion inside a `.vyx` template (RFC-0042). First a structural scan of the
/// raw `.vyx` classifies the cursor (attribute name / event / tag / component
/// prop / class value); anything structural is answered from the discovery
/// vocabularies or sibling components. A non-structural position (`{{ expr }}`,
/// script) falls through to the RFC-0033 forward-map path, which now also serves
/// finite/sequence string-literal completion (TransKey keys, `Tw` classes).
fn vyx_completion(
    server: &Server,
    uri: &Url,
    line: usize,
    col: usize,
) -> Option<CompletionResponse> {
    let raw = server
        .docs
        .get(uri)
        .cloned()
        .or_else(|| uri_path(uri).and_then(|p| std::fs::read_to_string(p).ok()))?;
    match templates::classify(&raw, line, col) {
        VyxCursor::TagName { prefix, start_col } => {
            Some(tag_name_completion(uri, &raw, &prefix, line, start_col, col))
        }
        VyxCursor::AttrName {
            tag,
            prefix: _,
            is_component,
            start_col,
        } => Some(attr_name_completion(
            uri,
            &raw,
            &tag,
            is_component,
            line,
            start_col,
            col,
        )),
        VyxCursor::EventName {
            prefix: _,
            start_col,
        } => Some(event_name_completion(&raw, line, start_col, col)),
        VyxCursor::ClassValue {
            token: _,
            start_col,
        } => {
            // The Tw alphabet comes from the synthesized (themed) module via the
            // forward map; a non-themed `.vyx` has no domain and gets nothing.
            let fwd = vyx_forward(server, uri, line, col)?;
            let cls = class_completions(
                &fwd.synth.analysis,
                &fwd.synth.gen_source,
                fwd.line,
                fwd.col,
            )?;
            Some(class_token_response(&raw, line, start_col, col, cls))
        }
        VyxCursor::Other => {
            // RFC-0071 M4: the governing contract's members, computed BEFORE the
            // forward map is consulted. At module scope in a `<script>` there is
            // usually nothing generated to map to (a blank line between
            // declarations has no origin), and that is precisely where `head`
            // and `data` have to be offered — so the contract path must not
            // depend on the map succeeding.
            let mut members = contract_completion_items(server, uri, line, col);
            let Some(fwd) = vyx_forward(server, uri, line, col) else {
                return (!members.is_empty()).then_some(CompletionResponse::Array(members));
            };
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
            let mut out: Vec<CompletionItem> = items.into_iter().map(to_completion_item).collect();
            members.append(&mut out);
            Some(CompletionResponse::Array(members))
        }
    }
}

/// Component tags (sibling PascalCase `.vyx`) plus, for a lowercase prefix, the
/// document's plain symbols. Each item replaces the partial tag name.
fn tag_name_completion(
    uri: &Url,
    raw: &str,
    prefix: &str,
    line: usize,
    start_col: usize,
    col: usize,
) -> CompletionResponse {
    let range = replace_range(raw, line, start_col, col);
    let mut items: Vec<CompletionItem> = Vec::new();
    if let Some((dir, self_name)) = vyx_dir_and_name(uri) {
        for name in templates::sibling_components(&dir, &self_name) {
            items.push(edit_item(
                &name,
                CompletionItemKind::CLASS,
                "component",
                range,
            ));
        }
    }
    // Common HTML elements, for a lowercase tag start.
    if !prefix
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase())
    {
        for el in HTML_ELEMENTS {
            items.push(edit_item(
                el,
                CompletionItemKind::KEYWORD,
                "html element",
                range,
            ));
        }
    }
    CompletionResponse::Array(items)
}

/// Attribute-name completion: a component tag offers its declared props; an
/// element offers global + per-element HTML attributes and the `v-*` directives.
fn attr_name_completion(
    uri: &Url,
    raw: &str,
    tag: &str,
    is_component: bool,
    line: usize,
    start_col: usize,
    col: usize,
) -> CompletionResponse {
    let range = replace_range(raw, line, start_col, col);
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
        items.push(edit_item(
            a,
            CompletionItemKind::PROPERTY,
            "html attribute",
            range,
        ));
    }
    for a in templates::element_attrs(tag) {
        items.push(edit_item(
            a,
            CompletionItemKind::PROPERTY,
            "html attribute",
            range,
        ));
    }
    for (d, detail) in templates::DIRECTIVES {
        items.push(edit_item(d, CompletionItemKind::KEYWORD, detail, range));
    }
    CompletionResponse::Array(items)
}

/// `@event` completion: the DOM events the runtime dispatches.
fn event_name_completion(raw: &str, line: usize, start_col: usize, col: usize) -> CompletionResponse {
    // Replace from the `@` (start_col) so the inserted `@click` keeps the sigil.
    let range = replace_range(raw, line, start_col, col);
    let items = templates::EVENTS
        .iter()
        .map(|e| {
            edit_item(
                &format!("@{e}"),
                CompletionItemKind::EVENT,
                "dom event",
                range,
            )
        })
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
    let range = replace_range(raw, line, start_col, col);
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
/// (1-based char columns in `raw`, exclusive) — the span a completion `textEdit`
/// replaces, sent as UTF-16 units.
fn replace_range(raw: &str, line: usize, start_col: usize, col: usize) -> Range {
    let l = line.saturating_sub(1) as u32;
    let line_text = line_of_text(raw, l as usize);
    Range {
        start: Position {
            line: l,
            character: char_col_to_utf16(line_text, start_col.saturating_sub(1)),
        },
        end: Position {
            line: l,
            character: char_col_to_utf16(line_text, col.saturating_sub(1)),
        },
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
    "div", "span", "p", "a", "ul", "ol", "li", "section", "header", "footer", "nav", "main",
    "article", "aside", "h1", "h2", "h3", "h4", "h5", "h6", "button", "input", "label", "select",
    "option", "textarea", "form", "img", "table", "thead", "tbody", "tr", "td", "th", "pre",
    "code", "strong", "em",
];

/// Map one frontend completion to an LSP `CompletionItem`.
fn to_completion_item(c: vyrn_frontend::Completion) -> CompletionItem {
    CompletionItem {
        label: c.label,
        kind: Some(to_lsp_kind(c.kind)),
        detail: Some(c.detail),
        // RFC-0051 §1: the declaration's `///` doc, shown in the completion
        // item's detail pane (markdown, verbatim).
        documentation: c.doc.map(|d| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: d,
            })
        }),
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
            let better = region
                .as_ref()
                .map(|b| r.origin.col >= b.origin.col)
                .unwrap_or(true);
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
    let gen_line = synth
        .gen_source
        .lines()
        .nth(region.gen_start_line.saturating_sub(1))
        .unwrap_or("");
    let (gline, gcol) = map_into_region(
        vyx_line,
        region.origin.col,
        col,
        gen_line,
        region.gen_start_line,
    );
    Some(VyxFwd {
        synth,
        line: gline,
        col: gcol,
    })
}

/// The analyzed synthesized module (RFC-0049 §2) for `owner`'s generated module
/// `banner`, from the cache when the owner's input signature is unchanged, else
/// generated + analyzed and cached. Returns `None` if the owner can't be read or
/// the banner isn't among its generated modules.
fn synth_for(server: &Server, owner: &Url, banner: &str) -> Option<Rc<AnalyzedSynth>> {
    let overlays = overlays_of(server);
    let (opts, resolver, owner_path, _) = load_context(owner, &overlays)?;
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
            let gen_modules = vyrn_frontend::loader::generated_modules(
                &owner_text,
                &owner_path,
                &opts,
                &resolver,
            )
            .ok()?;
            cache.insert(
                owner.clone(),
                OwnerSynth {
                    sig,
                    gen_modules,
                    analyzed: HashMap::new(),
                },
            );
            cache.get_mut(owner).unwrap()
        }
    };

    if let Some(a) = entry.analyzed.get(banner) {
        return Some(a.clone());
    }
    let gen_source = entry
        .gen_modules
        .iter()
        .find(|(b, _)| b == banner)
        .map(|(_, s)| s.clone())?;
    // Analyze the synthesized module as a linked root under the owner's dir, so
    // its imports (std/html, rebased relatives) resolve. Its own diagnostics
    // (e.g. "no main") are ignored — only the symbol index / tokens are queried.
    let synth_path = synth_path_for(&owner_path);
    let analysis = vyrn_frontend::analyze_linked(&gen_source, &synth_path, &opts, &resolver);
    let tokens = vyrn_frontend::semantic_tokens(&analysis);
    let a = Rc::new(AnalyzedSynth {
        gen_source,
        analysis,
        tokens,
    });
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
    let tail: Vec<char> = vyx_line
        .chars()
        .skip(origin_col.saturating_sub(1))
        .collect();
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
    let text = doc_text(server, &p.text_document.uri).unwrap_or_default();
    let symbols: Vec<DocumentSymbol> = analysis
        .symbols
        .iter()
        .filter(|s| s.file.is_none())
        .filter_map(|s| to_document_symbol(s, &text))
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
    Some(vec![TextEdit {
        range: whole_document_range(text),
        new_text: formatted,
    }])
}

/// A `Range` covering the entire `text` (start of the document to just past its
/// last character), so a single edit replaces everything.
fn whole_document_range(text: &str) -> Range {
    // LSP lines are 0-based; the end position is the line/character just after
    // the last content. Counting `\n`s gives the last line index; the final
    // line's length is its UTF-16 code-unit length — what the client reads. An
    // astral-plane char (emoji, CJK ext-B) is 2 units and 1 char; counting chars
    // here used to end the format edit short and duplicate the tail on save.
    let mut last_line = 0u32;
    let mut last_line_len = 0u32;
    for ch in text.chars() {
        if ch == '\n' {
            last_line += 1;
            last_line_len = 0;
        } else {
            last_line_len += ch.len_utf16() as u32;
        }
    }
    Range {
        start: Position {
            line: 0,
            character: 0,
        },
        end: Position {
            line: last_line,
            character: last_line_len,
        },
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
            // RFC-0087 U1: where an owning value stops being live. `MODIFICATION`
            // is a standard modifier, so a theme that never heard of Vyrn still
            // has a rule for it, and the extension gives it its own colour.
            SemanticTokenModifier::MODIFICATION, // bit 3
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
    if m.last_use {
        b |= 1 << 3;
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
    let text = doc_text(server, &p.text_document.uri).unwrap_or_default();
    Some(SemanticTokensResult::Tokens(encode_tokens(toks, &text)))
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
    let text = doc_text(server, &p.text_document.uri).unwrap_or_default();
    Some(SemanticTokensRangeResult::Tokens(encode_tokens(toks, &text)))
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

/// `textDocument/inlayHint` — a label at every move (RFC-0087 U1), and the type
/// of every binding whose line does not say it.
///
/// Pure over the cached analysis, filtered here to the requested line range.
/// A `.vyx` has no analysis of its own — its script is a synthesized module —
/// so its type hints are mapped back out of that module by [`vyx_type_hints`].
fn handle_inlay_hint(server: &Server, params: serde_json::Value) -> Option<Vec<InlayHint>> {
    let p: InlayHintParams = serde_json::from_value(params).ok()?;
    let (from, to) = (
        p.range.start.line as usize + 1,
        p.range.end.line as usize + 1,
    );
    if !is_vyrn_uri(&p.text_document.uri) {
        return Some(vyx_type_hints(server, &p.text_document.uri, from, to));
    }
    let (analysis, _) = lookup(server, &p.text_document.uri)?;
    let src = doc_text(server, &p.text_document.uri);
    let mut hints: Vec<InlayHint> = vyrn_frontend::inlay_hints(analysis)
        .into_iter()
        .filter(|h| h.line >= from && h.line <= to)
        .map(|h| {
            // The move label's column leaves the frontend as a char column and
            // goes on the wire as UTF-16 units.
            let line_text = src
                .as_deref()
                .map(|t| line_of_text(t, h.line.saturating_sub(1)))
                .unwrap_or("");
            InlayHint {
                position: Position {
                    line: h.line.saturating_sub(1) as u32,
                    character: char_col_to_utf16(line_text, h.col.saturating_sub(1)),
                },
                label: InlayHintLabel::String(h.label),
                kind: None,
                text_edits: None,
                tooltip: None,
                padding_left: Some(true),
                padding_right: None,
                data: None,
            }
        })
        .collect();
    if let Some(src) = src.as_deref() {
        hints.extend(type_hints(analysis, src, from, to));
    }
    Some(hints)
}

/// A `: Type` label after the name of every binding the source does not already
/// type (`let p = o.copy()` → `p: Outer`).
///
/// The type is the one the cached [`Analysis`] holds for that binding — the row
/// hover renders — so a hint and a hover cannot disagree. A binding the analysis
/// gives no type gets no hint.
///
/// Whether the type is ALREADY VISIBLE is a question about the text, not about
/// types, so [`spells_type`] answers it from the document's own line. The server
/// re-analyzes nothing.
fn type_hints(analysis: &Analysis, src: &str, from: usize, to: usize) -> Vec<InlayHint> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    for b in &analysis.locals {
        if b.line < from || b.line > to || b.end_col == 0 {
            continue;
        }
        let Some(line) = lines.get(b.line - 1) else {
            continue;
        };
        let Some(label) = type_hint_label(b, line, b.end_col) else {
            continue;
        };
        out.push(type_hint_at(line, b.line, b.end_col, label));
    }
    out
}

/// The `: Type` label a binding earns on the author's own `line`, or `None`
/// when the analysis gives it no type or the line already says it.
///
/// `end_col` is the 1-based column just past the name IN THAT LINE — the
/// binding's own for a `.vyrn`, the mapped-back one for a `.vyx`.
///
/// The type spelling is [`vyrn_frontend::type_to_string`], the renderer hover
/// uses, so an anonymous enum reads as its variant arms (`{ A(Int64) | B }`) in
/// both surfaces and there is no second renderer.
fn type_hint_label(b: &vyrn_frontend::LocalBinding, line: &str, end_col: usize) -> Option<String> {
    // A parameter always writes its type; only `let`s and `for` variables can
    // hide one.
    if !matches!(b.kind, LocalKind::Let { .. } | LocalKind::ForVar) {
        return None;
    }
    let label = vyrn_frontend::type_to_string(b.ty.as_ref()?);
    if spells_type(line, end_col, &label) {
        return None;
    }
    Some(label)
}

/// One type hint, drawn just past a name at 1-based `(line, end_col)` — char
/// columns in `line_text`, sent as UTF-16 units.
fn type_hint_at(line_text: &str, line: usize, end_col: usize, label: String) -> InlayHint {
    InlayHint {
        position: Position {
            line: (line - 1) as u32,
            character: char_col_to_utf16(line_text, end_col - 1),
        },
        label: InlayHintLabel::String(format!(": {label}")),
        kind: Some(InlayHintKind::TYPE),
        text_edits: None,
        tooltip: None,
        padding_left: Some(false),
        padding_right: None,
        data: None,
    }
}

/// Type hints for a `.vyx` input: the bindings its `<script>` declares, mapped
/// out of the synthesized module back into the author's own coordinates
/// (RFC-0033 regions, the same inversion [`vyx_semantic_tokens`] does).
///
/// A `<script>` line is copied verbatim into the generated module under its own
/// origin directive, so its bindings map column-exactly. A hint is emitted only
/// when the mapped position lands on the author's own name: the binding's name
/// must end exactly there in the `.vyx` line. Everything else — a binding the
/// generator invented, a derived region that does not align verbatim — is
/// dropped. A misplaced hint is worse than a missing one.
fn vyx_type_hints(server: &Server, vyx_uri: &Url, from: usize, to: usize) -> Vec<InlayHint> {
    let mut out = Vec::new();
    let Some(vyx_path) = uri_path(vyx_uri) else {
        return out;
    };
    let Some(owner) = server.vyx_owner.get(&vyx_path).cloned() else {
        return out;
    };
    let Some(owner_analysis) = server.analyses.get(&owner) else {
        return out;
    };
    let Some(vyx_text) = server
        .docs
        .get(vyx_uri)
        .cloned()
        .or_else(|| std::fs::read_to_string(&vyx_path).ok())
    else {
        return out;
    };

    // One synthesized module per banner, held for the whole request: a `.vyx`
    // has one generated module and many regions, and `synth_for` re-hashes the
    // owner on every call.
    let mut synths: HashMap<String, Rc<AnalyzedSynth>> = HashMap::new();
    for region in owner_analysis.origins.regions_for(&vyx_path) {
        if region.origin.line < from || region.origin.line > to {
            continue;
        }
        let Some(vyx_line) = vyx_text.lines().nth(region.origin.line.saturating_sub(1)) else {
            continue;
        };
        // A hint always sits on a binding's own declaration line, so a line with
        // no binding keyword can carry none — and the whole `<template>` is
        // usually that, at the price of one substring scan.
        if !vyx_line.contains("let") && !vyx_line.contains("for") {
            continue;
        }
        let synth = match synths.get(&region.gen_module) {
            Some(s) => s.clone(),
            None => {
                let Some(s) = synth_for(server, &owner, &region.gen_module) else {
                    continue;
                };
                synths.insert(region.gen_module.clone(), s.clone());
                s
            }
        };
        let Some(gen_line) = synth
            .gen_source
            .lines()
            .nth(region.gen_start_line.saturating_sub(1))
        else {
            continue;
        };
        let Some((gcol, span_len)) = align_expr_span(vyx_line, region.origin.col, gen_line) else {
            continue;
        };
        for b in &synth.analysis.locals {
            // Only a binding on the region's first generated line, wholly inside
            // the verbatim span, maps cleanly back.
            if b.line != region.gen_start_line || b.end_col < gcol {
                continue;
            }
            if b.end_col > gcol + span_len {
                continue;
            }
            let col = region.origin.col + (b.end_col - gcol);
            if !name_ends_at(vyx_line, col, &b.name) {
                continue;
            }
            let Some(label) = type_hint_label(b, vyx_line, col) else {
                continue;
            };
            out.push(type_hint_at(vyx_line, region.origin.line, col, label));
        }
    }
    // Overlapping regions (rare) could double-emit a position; keep one per spot.
    out.sort_by_key(|h| (h.position.line, h.position.character));
    out.dedup_by_key(|h| (h.position.line, h.position.character));
    out
}

/// Does `name` end exactly at 1-based character column `col` of `line`?
///
/// The proof that a mapped-back position is the author's own name and not
/// generator glue that happened to align.
fn name_ends_at(line: &str, col: usize, name: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let end = col.saturating_sub(1);
    let len = name.chars().count();
    if end > chars.len() || len > end {
        return false;
    }
    chars[end - len..end].iter().collect::<String>() == name
}

/// Does the text after a binding's name already say its type?
///
/// `line` is the declaration line, `end_col` the 1-based column just past the
/// name (character columns, the lexer's convention), `ty` the rendered type.
/// True — and so no hint — for:
///
/// * a written annotation (`let x: Int64 = ..`);
/// * a literal, which is its own evidence (`3`, `"s"`, `true`, `[1, 2]`);
/// * an initializer that opens with the type's own name (`Outer { .. }`,
///   `Color.Red`).
///
/// Everything else — a call, a `match`, a `spawn`, a field or element read —
/// hides the type, and gets the hint. In doubt the answer is false: a hint too
/// many is noise, a hint too few is the feature not working.
fn spells_type(line: &str, end_col: usize, ty: &str) -> bool {
    let rest: String = line.chars().skip(end_col.saturating_sub(1)).collect();
    let rest = rest.trim_start();
    if rest.starts_with(':') {
        return true;
    }
    // The binding's `=` is the first one after its name, so a comparison inside
    // the initializer (`a == b`) cannot be mistaken for it.
    let Some((_, init)) = rest.split_once('=') else {
        // A `for` variable has no initializer, and its element type is never
        // written on the line.
        return false;
    };
    let init = init.trim_start();
    let mut chars = init.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first.is_ascii_digit() || matches!(first, '"' | '\'' | '[' | '`') {
        return true;
    }
    if first == '-' && chars.next().is_some_and(|c| c.is_ascii_digit()) {
        return true;
    }
    let word: String = init
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    // `Slots<Int64>` is spelled by a `Slots { .. }`; the head is what a source
    // line can carry.
    let head = ty.split(['<', ' ']).next().unwrap_or(ty);
    word == "true" || word == "false" || word == head
}

/// Delta-encode classified tokens into the LSP wire form. Tokens are sorted by
/// (line, col) and encoded as the required `[Δline, Δstart, len, type, mods]`
/// quintuples (0-based UTF-16 positions, converted from the frontend's char
/// columns through `text`'s lines).
fn encode_tokens(mut toks: Vec<vyrn_frontend::SemToken>, text: &str) -> SemanticTokens {
    toks.sort_by_key(|t| (t.line, t.col));
    let mut data = Vec::with_capacity(toks.len());
    let mut prev_line = 0u32;
    let mut prev_col = 0u32;
    for t in toks {
        let line = t.line.saturating_sub(1) as u32;
        let line_text = line_of_text(text, line as usize);
        let start = t.col.saturating_sub(1);
        let col = char_col_to_utf16(line_text, start);
        let len = char_col_to_utf16(line_text, start + t.len) - col;
        let delta_line = line.saturating_sub(prev_line);
        let delta_start = if delta_line == 0 {
            col.saturating_sub(prev_col)
        } else {
            col
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length: len,
            token_type: sem_type_index(t.kind),
            token_modifiers_bitset: sem_mods_bits(t.mods),
        });
        prev_line = line;
        prev_col = col;
    }
    SemanticTokens {
        result_id: None,
        data,
    }
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
    let Some(vyx_path) = uri_path(vyx_uri) else {
        return out;
    };
    let Some(owner) = server.vyx_owner.get(&vyx_path).cloned() else {
        return out;
    };
    let Some(owner_analysis) = server.analyses.get(&owner) else {
        return out;
    };
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
        let Some(gen_line) = gen_source
            .lines()
            .nth(region.gen_start_line.saturating_sub(1))
        else {
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
    let tail: Vec<char> = vyx_line
        .chars()
        .skip(origin_col.saturating_sub(1))
        .collect();
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
fn to_document_symbol(sym: &vyrn_frontend::Symbol, text: &str) -> Option<DocumentSymbol> {
    let kind = match sym.kind {
        SymbolKind::Function => lsp_types::SymbolKind::FUNCTION,
        SymbolKind::Method => lsp_types::SymbolKind::METHOD,
        SymbolKind::Type => lsp_types::SymbolKind::STRUCT,
        SymbolKind::Variant => lsp_types::SymbolKind::ENUM_MEMBER,
        // Module state (RFC-0013) shows as a variable in the outline.
        SymbolKind::Global => lsp_types::SymbolKind::VARIABLE,
        SymbolKind::Field | SymbolKind::Param | SymbolKind::Local => return None,
    };
    let range = lsp_range(text, sym.line, sym.col, sym.end_col);
    let detail = if sym.detail.is_empty() {
        None
    } else {
        Some(sym.detail.clone())
    };
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
    // `col` is 1-based in CHARS (the frontend convention), so walk left over
    // chars — indexing bytes with a char column lands mid-UTF-8 after any
    // non-ASCII receiver (`café.`) and silently drops member completions.
    let chars: Vec<char> = line_text.chars().collect();
    let mut i = col.saturating_sub(2);
    // Skip the partial member name (e.g. the `pu` in `arr.pu`). Vyrn identifiers
    // are unicode (the lexer takes any alphabetic char), so skip them as chars.
    while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
        if i == 0 {
            return false;
        }
        i -= 1;
    }
    // Skip spaces between the dot and the partial name.
    while i < chars.len() && chars[i] == ' ' {
        if i == 0 {
            return false;
        }
        i -= 1;
    }
    i < chars.len() && chars[i] == '.'
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

// ---------------------------------------------------------------------------
// RFC-0071 M4: contract members in the editor
// ---------------------------------------------------------------------------

/// A document seen through the contract that governs it.
struct ContractCtx {
    view: vyrn_frontend::contracts::ContractView,
    /// The document's Vyrn source — the buffer itself for a `.vyrn`, the
    /// `<script>` body for a `.vyx`.
    source: String,
    /// Lines to add to a `source` position to reach the buffer (0 for `.vyrn`).
    line_offset: usize,
    /// The members the document's FORM writes rather than its `source` — a
    /// `.vyx`'s `<template>`. Completion must not offer one: the declaration it
    /// would insert collides with the generated export.
    synthesized: Vec<String>,
}

impl ContractCtx {
    /// A buffer line → the corresponding line in [`Self::source`], or `None`
    /// when the cursor is outside the script region of a `.vyx`.
    fn to_source_line(&self, line: usize) -> Option<usize> {
        let l = line.checked_sub(self.line_offset)?;
        (l > 0 && l <= self.source.lines().count() + 1).then_some(l)
    }
}

/// Resolve `uri` to the contract that governs it, if any.
///
/// The chain is entirely the frontend's: app root → roles (`vyrn.json`'s
/// `roles` key, else discovery from the generator call sites) → the role whose
/// scope covers this file → the contract that role names. A file in no role, or
/// a role whose contract module cannot be read, simply has no contract, and
/// every capability below falls back to what it did before this milestone.
fn contract_ctx(server: &Server, uri: &Url) -> Option<ContractCtx> {
    let path = uri_path(uri)?;
    let text = server
        .docs
        .get(uri)
        .cloned()
        .or_else(|| std::fs::read_to_string(&path).ok())?;
    // `raw` is the whole `.vyx` buffer, kept because its `<template>` is not in
    // the `<script>` body and is where a page's view comes from. Empty for a
    // `.vyrn`, which has no form beyond its text.
    let (source, line_offset, raw) = if is_vyrn_uri(uri) {
        (text, 0, String::new())
    } else {
        // A generator input: only `.vyx` carries a Vyrn `<script>`, and a cursor
        // outside it is not at module scope in any module.
        let (body, offset) = contracts::vyx_script(&text)?;
        (body, offset, text)
    };

    let dir = std::path::Path::new(&path).parent()?.to_path_buf();
    let app_dir = app_root_for(&dir);
    let overlays = overlays_of(server);
    let (opts, resolver, _, _) = load_context(uri, &overlays)?;

    let mut cache = server.contract_cache.borrow_mut();
    let roots = contracts::role_roots(&app_dir);
    let sig = contracts::roles_sig(&app_dir, &roots);
    let entry = cache
        .entry(app_dir.clone())
        .or_insert_with(|| contracts::ContractIndex {
            sig,
            derived: false,
            roles: Vec::new(),
            views: HashMap::new(),
        });
    if entry.sig != sig || !entry.derived {
        entry.sig = sig;
        entry.derived = true;
        entry.roles = contracts::roles_of(&app_dir, &roots, &opts, &resolver);
        entry.views.clear();
    }
    let role = vyrn_frontend::contracts::role_for(&path, &entry.roles)?.clone();
    let key = format!("{}:{}", role.module, role.contract);
    // A cached view is trusted only while its own declaring file is unchanged.
    if let Some((was, view)) = entry.views.get(&key) {
        if *was == contracts::file_sig(std::path::Path::new(&view.file)) {
            let synthesized = vyrn_frontend::contracts::synthesized_members(view, &path, &raw);
            return Some(ContractCtx {
                view: view.clone(),
                source,
                line_offset,
                synthesized,
            });
        }
    }
    // A manifest role's relative specifier is relative to the MANIFEST, so the
    // importer is the manifest — not the page, which may live several
    // directories down.
    let manifest = app_dir
        .join("vyrn.json")
        .to_string_lossy()
        .replace('\\', "/");
    let view = vyrn_frontend::contracts::load_role_contract(&role, &manifest, &opts, &resolver)?;
    let file_sig = contracts::file_sig(std::path::Path::new(&view.file));
    entry.views.insert(key, (file_sig, view.clone()));
    let synthesized = vyrn_frontend::contracts::synthesized_members(&view, &path, &raw);
    Some(ContractCtx {
        view,
        source,
        line_offset,
        synthesized,
    })
}

/// RFC-0071 M4 completion: the governing contract's members, offered at module
/// scope as full declarations.
///
/// The two gates are what keep this from misfiring. **Module scope** — a
/// contract member is a declaration, so it is offered where a declaration can
/// go and nowhere else (never inside a function body, never inside a record
/// type). **Role** — a `layout.vyx` beside a page is chrome with no contract, so
/// it is not in the role and gets nothing.
fn contract_completion_items(
    server: &Server,
    uri: &Url,
    line: usize,
    col: usize,
) -> Vec<CompletionItem> {
    let Some(ctx) = contract_ctx(server, uri) else {
        return Vec::new();
    };
    let Some(src_line) = ctx.to_source_line(line) else {
        return Vec::new();
    };
    if !vyrn_frontend::at_module_scope(&ctx.source, src_line, col) {
        return Vec::new();
    }
    // What the document already has: what its `<script>` exports, plus what its
    // FORM exports for it — a `.vyx`'s `<template>` is its view, so offering the
    // view member would insert a declaration that collides with the generated one.
    let mut already = contracts::exported_names(&ctx.source);
    already.extend(ctx.synthesized.iter().cloned());
    vyrn_frontend::contracts::contract_completions(&ctx.view, &already)
        .into_iter()
        .map(|c| CompletionItem {
            label: c.label,
            kind: Some(CompletionItemKind::SNIPPET),
            detail: Some(c.detail),
            documentation: c.doc.map(|d| {
                Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: d,
                })
            }),
            insert_text: Some(c.snippet),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            // Contract members sort ABOVE the document's own symbols: at module
            // scope in a page they are the thing you are there to write.
            sort_text: Some(format!("0{}", c.sort)),
            ..Default::default()
        })
        .collect()
}

/// RFC-0071 M4 hover / go-to-definition: the contract member named at the
/// cursor, when the cursor is at module scope in a governed file.
///
/// Module scope is the discriminator that keeps a local variable called `data`
/// inside a function body from claiming to be a contract member.
fn contract_member_at(
    server: &Server,
    uri: &Url,
    line: usize,
    col: usize,
) -> Option<(ContractCtx, String)> {
    let ctx = contract_ctx(server, uri)?;
    let src_line = ctx.to_source_line(line)?;
    if !vyrn_frontend::at_module_scope(&ctx.source, src_line, col) {
        return None;
    }
    let text = server.docs.get(uri)?;
    let (name, _, _) = contracts::ident_at(text, line, col)?;
    ctx.view.member(&name)?;
    Some((ctx, name))
}

/// The `member of contract …` block appended to a member's hover.
fn contract_hover_note(server: &Server, uri: &Url, line: usize, col: usize) -> Option<String> {
    let (ctx, name) = contract_member_at(server, uri, line, col)?;
    vyrn_frontend::contracts::contract_member_hover(&ctx.view, &name)
}

/// Go-to-definition on a contract member name → the member's declaration inside
/// the contract.
fn contract_member_definition(
    server: &Server,
    uri: &Url,
    line: usize,
    col: usize,
) -> Option<GotoDefinitionResponse> {
    let (ctx, name) = contract_member_at(server, uri, line, col)?;
    let m = ctx.view.member(&name)?;
    let target =
        Url::from_file_path(ctx.view.file.replace('/', std::path::MAIN_SEPARATOR_STR)).ok()?;
    let target_text = doc_text(server, &target).unwrap_or_default();
    Some(GotoDefinitionResponse::Scalar(Location {
        uri: target,
        range: lsp_range(&target_text, m.line, m.col, m.end_col),
    }))
}

/// `textDocument/codeAction` — the did-you-mean rename RFC-0071 asks for.
///
/// The offer is computed from the CONTRACT (an export the closed contract does
/// not name, within the same Damerau-Levenshtein threshold `std/contract` uses),
/// not by re-parsing a diagnostic message: a generator's diagnostic text is its
/// own business, and reading it here would make the server know a generator.
/// The diagnostics the client sent are attached by RANGE overlap, so the
/// lightbulb still appears on the squiggle.
fn handle_code_action(
    server: &Server,
    params: serde_json::Value,
) -> Option<Vec<CodeActionOrCommand>> {
    let p: CodeActionParams = serde_json::from_value(params).ok()?;
    let uri = &p.text_document.uri;
    let ctx = contract_ctx(server, uri)?;
    let mut out = Vec::new();
    let text = doc_text(server, uri).unwrap_or_default();
    for fix in vyrn_frontend::contracts::contract_fixes(&ctx.view, &ctx.source) {
        let range = lsp_range(&text, fix.line + ctx.line_offset, fix.col, fix.end_col);
        if !ranges_overlap(&range, &p.range) {
            continue;
        }
        let diagnostics: Vec<LspDiagnostic> = p
            .context
            .diagnostics
            .iter()
            .filter(|d| ranges_overlap(&d.range, &range) || d.message.contains(&fix.from))
            .cloned()
            .collect();
        let mut changes = std::collections::HashMap::new();
        changes.insert(
            uri.clone(),
            vec![TextEdit {
                range,
                new_text: fix.to.clone(),
            }],
        );
        out.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: format!(
                "Rename `{}` to `{}` ({})",
                fix.from,
                fix.to,
                ctx.view.site()
            ),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: (!diagnostics.is_empty()).then_some(diagnostics),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            }),
            is_preferred: Some(true),
            ..Default::default()
        }));
    }
    Some(out)
}

/// Whether two LSP ranges share at least a position (a zero-width cursor at the
/// start or end of a range counts as touching it).
fn ranges_overlap(a: &Range, b: &Range) -> bool {
    let key = |p: &Position| (p.line, p.character);
    key(&a.start) <= key(&b.end) && key(&b.start) <= key(&a.end)
}

/// LSP positions are UTF-16 code units (the default `positionEncoding`, which
/// this server does not renegotiate), while the frontend's columns count Unicode
/// chars. The two diverge on any line carrying an astral-plane character (emoji,
/// CJK ext-B — 2 UTF-16 units, 1 char), so every crossing of the LSP/frontend
/// boundary converts through the target line's text.

/// The UTF-16 offset of 0-based char column `char_col` in `line` (clamped to the
/// line's end).
pub(crate) fn char_col_to_utf16(line: &str, char_col: usize) -> u32 {
    line.chars()
        .take(char_col)
        .map(|c| c.len_utf16() as u32)
        .sum()
}

/// The 0-based char column of UTF-16 offset `units` in `line` (clamped: past the
/// end, or inside a surrogate pair, lands on the nearest char boundary).
pub(crate) fn utf16_to_char_col(line: &str, units: u32) -> usize {
    let mut seen = 0u32;
    for (idx, c) in line.chars().enumerate() {
        if seen >= units {
            return idx;
        }
        seen += c.len_utf16() as u32;
    }
    line.chars().count()
}

/// The 0-based `line`-th line of `text`, or "" when out of range.
pub(crate) fn line_of_text(text: &str, line: usize) -> &str {
    text.lines().nth(line).unwrap_or("")
}

/// The live text of `uri`: the open buffer, else the file on disk.
fn doc_text(server: &Server, uri: &Url) -> Option<String> {
    server
        .docs
        .get(uri)
        .cloned()
        .or_else(|| uri_path(uri).and_then(|p| std::fs::read_to_string(p).ok()))
}

/// LSP 0-based UTF-16 position → frontend 1-based (line, col in chars).
fn to_frontend(text: &str, pos: &Position) -> (usize, usize) {
    let col = utf16_to_char_col(line_of_text(text, pos.line as usize), pos.character);
    ((pos.line + 1) as usize, col + 1)
}

/// Frontend 1-based (line, col, end_col) IN `text`'s char columns → LSP 0-based
/// UTF-16 `Range`. A col of 0 means "whole line, unknown column" → a zero-length
/// range at the line start (mirrors [`publish`]).
fn lsp_range(text: &str, line: usize, col: usize, end_col: usize) -> Range {
    let l = line.saturating_sub(1) as u32;
    let line_text = line_of_text(text, l as usize);
    let c = if col == 0 {
        0
    } else {
        char_col_to_utf16(line_text, col - 1)
    };
    let ec = if end_col == 0 {
        c
    } else {
        char_col_to_utf16(line_text, end_col - 1)
    };
    Range {
        start: Position {
            line: l,
            character: c,
        },
        end: Position {
            line: l,
            character: ec,
        },
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
    // Normalized: VS Code sends a Windows drive letter percent-encoded AND
    // lower-cased (`file:///n%3A/…`), while origin directives and the loader
    // carry `N:/…`. Windows paths are case-insensitive but `String` equality is
    // not, so an un-normalized key made every `.vyx` lookup miss — the bug
    // behind "hover/Ctrl+Click/colour do nothing in .vyx".
    let p = uri
        .to_file_path()
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    Some(vyrn_frontend::origin::OriginMaps::norm_path_key(&p))
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
        // Normalized to match `uri_path` — see its note (Windows drive-letter case).
        let f = vyrn_frontend::origin::OriginMaps::norm_path_key(&f);
        server.vyx_ownerless.remove(&f);
        server.vyx_owner.insert(f, root_uri.clone());
    }
    if let Some(c) = connection {
        publish_remapped(c, server, &analysis);
    }
    // RFC-0073 M3: a root that mounts a surface is the authority on where its
    // procedures live, so installing one REFRESHES the route facts for every
    // module it maps. This is also the only invalidation the cache has, and it
    // is enough: an entry can go stale only by its declaration moving, and the
    // hover matches on name AND line, so a stale entry makes a route note
    // disappear until the root is re-analyzed — never appear on the wrong
    // declaration.
    if !analysis.symbol_maps.is_empty() {
        let mut grouped: HashMap<String, Vec<MappedSymbol>> = HashMap::new();
        for m in &analysis.symbol_maps {
            grouped
                .entry(vyrn_frontend::origin::OriginMaps::norm_path_key(&m.file))
                .or_default()
                .push(m.clone());
        }
        let mut cache = server.route_facts.borrow_mut();
        for (k, v) in grouped {
            cache.insert(k, Rc::new(v));
        }
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
    let Some(path) = uri_path(vyx_uri) else {
        return;
    };
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
    let Some(path) = uri_path(vyx_uri) else {
        return false;
    };
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
    // Compare normalized: `vyx_path` came from a URI (lower-cased drive on
    // Windows), the origin paths from the loader (`N:/…`).
    let want = vyrn_frontend::origin::OriginMaps::norm_path_key(vyx_path);
    probe_roots(server, vyx_path, |a| {
        a.origins
            .input_files()
            .iter()
            .any(|f| vyrn_frontend::origin::OriginMaps::norm_path_key(f) == want)
    })
}

/// Analyze the ranked `.vyrn` roots near `path` until one `claims` it. Pure: it
/// mutates nothing on the server.
fn probe_roots(
    server: &Server,
    path: &str,
    claims: impl Fn(&Analysis) -> bool,
) -> Option<(Url, Analysis)> {
    let overlays = overlays_of(server);
    for cand in candidate_owners(path) {
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
        if claims(&analysis) {
            return Some((cand, analysis));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// RFC-0073 M3 — a declaration's derived wire facts.
//
// `server/api/pastes.vyrn` reaches no generator: it is what a generator reads,
// not what reads one. So the file open in the editor can never say what `create`
// is mounted at — only a ROOT that calls `rpc(..)` or `client(..)` can, and its
// answer is M1's symbol map. This is the `.vyx` owner bargain pointed the other
// way: find a root whose maps name this file, and remember the answer.
//
// Roots already analyzed are consulted first, and they usually suffice — in a
// full-stack project the server root or the client boot is open. The ranked
// probe is the fallback, and it is what makes the facts appear in a window that
// has only the api module open.
// ---------------------------------------------------------------------------

/// Every mapped symbol whose origin is `path`, from a root that generates over
/// it. Cached per file — including the empty answer, so a module nothing mounts
/// is probed once rather than once per hover.
fn route_facts_for_file(server: &Server, path: &str) -> Rc<Vec<MappedSymbol>> {
    if let Some(hit) = server.route_facts.borrow().get(path) {
        return hit.clone();
    }
    let claims = |a: &Analysis| {
        a.symbol_maps
            .iter()
            .any(|m| vyrn_frontend::symbolmap::same_file(&m.file, path))
    };
    let probed = if server.analyses.values().any(claims) {
        None
    } else {
        // Nothing open claims this file. Analyze the roots near it that CALL a
        // map-emitting generator — a handful in a real project, and none at all
        // in a directory that mounts nothing, which is what keeps a hover in an
        // ordinary module from paying for this at all.
        mounting_roots(path, RPC_GENERATORS)
            .into_iter()
            .find_map(|cand| {
                let text =
                    server.docs.get(&cand).cloned().or_else(|| {
                        uri_path(&cand).and_then(|p| std::fs::read_to_string(p).ok())
                    })?;
                let a = analyze_doc(&cand, &text, &overlays_of(server));
                claims(&a).then_some(a)
            })
    };
    // Cache what these analyses say about EVERY file they map, not just the one
    // asked for: a client's map covers the whole api directory, so the second
    // procedure hovered is free even in another module. The requested path is
    // inserted either way, so an unmounted module is probed once and not again.
    let mut grouped: HashMap<String, Vec<MappedSymbol>> = HashMap::new();
    grouped.insert(path.to_string(), Vec::new());
    for a in probed.iter().chain(server.analyses.values()) {
        for m in &a.symbol_maps {
            grouped
                .entry(vyrn_frontend::origin::OriginMaps::norm_path_key(&m.file))
                .or_default()
                .push(m.clone());
        }
    }
    let mut cache = server.route_facts.borrow_mut();
    for (k, v) in grouped {
        cache.insert(k, Rc::new(v));
    }
    cache
        .get(path)
        .cloned()
        .unwrap_or_else(|| Rc::new(Vec::new()))
}

/// The generators whose maps carry a DERIVED ROUTE — what a hover and a lens are
/// asking about.
const RPC_GENERATORS: &[&str] = &[
    "rpc(",
    "rpcServer(",
    "client(",
    "rpcClient(",
    "rpcInProcess(",
];

/// Every generator that emits a map at all. The REST projection's is route-less
/// (its paths are written in the projection file, not derived), so it is useless
/// to a hover and indispensable to a RENAME — `http("./pastes")` re-exports each
/// procedure under its own name, and nothing else records that the two `create`s
/// are the same declaration.
const MAP_GENERATORS: &[&str] = &[
    "rpc(",
    "rpcServer(",
    "client(",
    "rpcClient(",
    "rpcInProcess(",
    "http(",
];

/// The `.vyrn` roots near `path` that call one of `gens`, nearest first.
///
/// A textual filter and not an analysis: the point is to analyze as few roots as
/// possible, and a root that never calls one of these cannot map anything.
fn mounting_roots(path: &str, gens: &[&str]) -> Vec<Url> {
    let file = std::path::Path::new(path);
    let Some(dir) = file.parent() else {
        return Vec::new();
    };
    let app_root = app_root_for(dir);
    let mut files = Vec::new();
    collect_vyrn(&app_root, 0, &mut files);
    let mut scored: Vec<(usize, std::path::PathBuf)> = files
        .into_iter()
        .filter(|p| {
            let src = std::fs::read_to_string(p).unwrap_or_default();
            gens.iter().any(|g| src.contains(g))
        })
        .map(|p| (path_distance(&p, dir), p))
        .collect();
    scored.sort_by_key(|(d, _)| *d);
    scored
        .into_iter()
        .filter_map(|(_, p)| Url::from_file_path(p).ok())
        .collect()
}

/// RFC-0073 M4: every generated symbol standing for a declaration in `path`,
/// from EVERY generator that maps the file — not just the first one found.
///
/// This is where a rename's needs diverge from a hover's. A hover asks "what is
/// this mounted at" and one answer settles it, so [`route_facts_for_file`] stops
/// at the first root that claims the file. A rename asks "what else is named
/// after this", and stopping early is how you rewrite the client's call sites and
/// leave the server's — a procedure in `examples/bin` is mapped by three
/// generators (`client`, `rpc`, `http`) living in three different roots, and all
/// three names have to move together or none of them should.
fn all_mapped_symbols(server: &Server, path: &str) -> Vec<MappedSymbol> {
    let mut out: Vec<MappedSymbol> = route_facts_for_file(server, path).as_ref().clone();
    let overlays = overlays_of(server);
    for cand in mounting_roots(path, MAP_GENERATORS) {
        let Some(text) = server
            .docs
            .get(&cand)
            .cloned()
            .or_else(|| uri_path(&cand).and_then(|p| std::fs::read_to_string(p).ok()))
        else {
            continue;
        };
        let a = analyze_doc(&cand, &text, &overlays);
        for m in &a.symbol_maps {
            if vyrn_frontend::symbolmap::same_file(&m.file, path)
                && !out.iter().any(|o| o.name == m.name && o.line == m.line)
            {
                out.push(m.clone());
            }
        }
    }
    out
}

/// The mapped symbols for the document `uri`, in declaration order — what a
/// route CodeLens renders and what the declaration hover reads.
fn route_facts(server: &Server, uri: &Url) -> Rc<Vec<MappedSymbol>> {
    if !is_vyrn_uri(uri) {
        return Rc::new(Vec::new());
    }
    match uri_path(uri) {
        Some(p) => route_facts_for_file(server, &p),
        None => Rc::new(Vec::new()),
    }
}

/// RFC-0073 M3: the derived wire facts appended to a procedure DECLARATION's
/// hover — `POST /_/pastes/create · convention`, the thing a reader otherwise
/// has to remember the convention to know.
///
/// Gated on the cursor sitting on the declaration's own name, so hovering a call
/// to `create` inside the same module is unaffected: the note is about where a
/// declaration is MOUNTED, which is a fact about the declaration and not about
/// every use of the name.
fn derived_hover_note(server: &Server, uri: &Url, line: usize, col: usize) -> Option<String> {
    if !is_vyrn_uri(uri) {
        return None;
    }
    let (analysis, _) = lookup(server, uri)?;
    let decl = analysis.symbols.iter().find(|s| {
        s.file.is_none() && s.line == line && s.col > 0 && col >= s.col && col <= s.end_col
    })?;
    let name = decl.name.clone();
    let facts = route_facts(server, uri);
    let m = facts
        .iter()
        .find(|m| m.decl == name && m.line == line && m.route_line().is_some())?;
    m.route_line()
}

// ---------------------------------------------------------------------------
// RFC-0073 M4 — cross-boundary rename.
//
// The declaration is resolved here (it needs the server's cached analysis and
// the M3 route-facts cache); everything the rename then DOES lives in
// `rename.rs`, which is pure over what these two hand it.
// ---------------------------------------------------------------------------

/// The rename target under a `textDocument/{prepareRename,rename}` position, or
/// the reason there is none.
///
/// A generator input (`.vyx`) is refused outright, and says why: its declarations
/// are not what a generated symbol maps back to, and the file this server would
/// have to edit is the synthesized module — a build artifact.
fn rename_target(
    server: &Server,
    pos: &TextDocumentPositionParams,
) -> Result<(rename::Target, Url), String> {
    let uri = &pos.text_document.uri;
    if !is_vyrn_uri(uri) {
        return Err("rename works on a `.vyrn` declaration; a `.vyx` is a generator input".into());
    }
    let text = doc_text(server, uri).ok_or_else(|| "this document is not open".to_string())?;
    let (line, col) = to_frontend(&text, &pos.position);
    let path = uri_path(uri).ok_or_else(|| "this document has no file path".to_string())?;
    let (analysis, _) = lookup(server, uri).ok_or_else(|| {
        "this document has not been analyzed yet — save it once and try again".to_string()
    })?;
    let target = rename::target_at(analysis, &path, line, col)?;
    Ok((target, uri.clone()))
}

/// `textDocument/prepareRename` — the range and placeholder, or the refusal.
fn handle_prepare_rename(
    server: &Server,
    params: serde_json::Value,
) -> Result<PrepareRenameResponse, String> {
    let p: TextDocumentPositionParams =
        serde_json::from_value(params).map_err(|e| e.to_string())?;
    let (target, _) = rename_target(server, &p)?;
    let text = doc_text(server, &p.text_document.uri).unwrap_or_default();
    Ok(rename::prepare(&target, &text))
}

/// `textDocument/rename` — the edit, spanning SOURCE files only.
fn handle_rename(server: &Server, params: serde_json::Value) -> Result<WorkspaceEdit, String> {
    let p: RenameParams = serde_json::from_value(params).map_err(|e| e.to_string())?;
    let (target, uri) = rename_target(server, &p.text_document_position)?;
    let overlays = overlays_of(server);
    // The load options carry the manifest's aliases, so an importing module that
    // reaches the declaration through a dependency alias resolves the same way
    // the linker resolves it.
    let opts = load_context(&uri, &overlays)
        .map(|(o, _, _, _)| o)
        .unwrap_or_else(|| vyrn_frontend::loader::LoadOptions {
            std_root: std_root(),
            ..Default::default()
        });
    // M1's map, read in the other direction: every symbol a generator maps ONTO
    // this file, from which the declaration's own are selected.
    let path = uri_path(&uri).ok_or_else(|| "this document has no file path".to_string())?;
    let maps = all_mapped_symbols(server, &path);
    let (analysis, _) = lookup(server, &uri).ok_or_else(|| "no analysis".to_string())?;
    let decl_text = doc_text(server, &uri).unwrap_or_default();
    rename::workspace_edit(
        &target,
        &decl_text,
        &p.new_name,
        &maps,
        analysis,
        &uri,
        &overlays,
        &opts,
    )
}

/// The `.vyrn` roots to try as owners of `vyx_path`, most-likely first. Finds the
/// app root (nearest ancestor with `vyrn.json`, else the nearest ancestor holding
/// a generator-importing `.vyrn`, else the `.vyx`'s own directory), collects the
/// `.vyrn` files under it (bounded), and ranks them: a root that imports a page/
/// component generator AND names this `.vyx`'s directory first, then any
/// generator-importing root, then by path proximity.
fn candidate_owners(vyx_path: &str) -> Vec<Url> {
    let vyx = std::path::Path::new(vyx_path);
    let Some(vyx_dir) = vyx.parent() else {
        return Vec::new();
    };
    let dir_name = vyx_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let app_root = app_root_for(vyx_dir);

    let mut files = Vec::new();
    collect_vyrn(&app_root, 0, &mut files);

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
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
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
fn collect_vyrn(dir: &std::path::Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
    collect_sources(dir, depth, MAX_OWNER_CANDIDATES, &["vyrn"], out);
}

/// The walk itself, over any extension set and any cap — RFC-0073 M4's rename
/// wants `.vyx` too, and a much higher cap, because a file it fails to visit is
/// a call site it fails to rename.
fn collect_sources(
    dir: &std::path::Path,
    depth: usize,
    cap: usize,
    exts: &[&str],
    out: &mut Vec<std::path::PathBuf>,
) {
    if out.len() >= cap || depth > MAX_WALK_UP {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
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
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| exts.contains(&x))
        {
            out.push(p);
        }
    }
    for sub in subdirs {
        collect_sources(&sub, depth + 1, cap, exts, out);
        if out.len() >= cap {
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
fn publish_remapped(connection: &Connection, server: &Server, analysis: &Analysis) {
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
            // Prefer the open buffer. Reading from disk on every didChange both
            // costs a syscall per generator input and reports against text the
            // user has already edited away — the diagnostic ranges would be
            // computed from a stale file.
            let src = server
                .docs
                .get(&uri)
                .cloned()
                .unwrap_or_else(|| std::fs::read_to_string(&file).unwrap_or_default());
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
    } else if let Some(owner) = uri_path(uri)
        .and_then(|p| server.vyx_owner.get(&p))
        .cloned()
    {
        reanalyze_root(connection, server, &owner);
    } else {
        discover_vyx_owner(connection, server, uri);
    }
}

/// Apply a notification's effect on server state and return the URI that now
/// OWES an analysis, if any. The analysis itself is the caller's business —
/// `main_loop` defers it until the message queue is drained, so a burst of
/// keystrokes costs one analysis of the newest text rather than one per
/// character. That matters most where analysis is slowest: editing a `.vyx`
/// re-runs its owner's generators, which is seconds, and five queued keystrokes
/// used to mean five of them back to back.
fn handle_notification(connection: &Connection, server: &mut Server, notif: Notification) -> Owed {
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
            return Owed::Analyze(uri);
        }
    } else if DidChangeTextDocument::METHOD == notif.method {
        if let Ok(params) = serde_json::from_value::<DidChangeTextDocumentParams>(notif.params) {
            let uri = params.text_document.uri.clone();
            // Full sync: the last change carries the entire document text.
            if let Some(change) = params.content_changes.into_iter().last() {
                server.docs.insert(uri.clone(), change.text.clone());
                return Owed::Analyze(uri);
            }
        }
    } else if DidCloseTextDocument::METHOD == notif.method {
        if let Ok(params) = serde_json::from_value::<DidCloseTextDocumentParams>(notif.params) {
            // Drop the document and clear its diagnostics.
            server.docs.remove(&params.text_document.uri);
            server.analyses.remove(&params.text_document.uri);
            let closed = params.text_document.uri.clone();
            let _ = connection
                .sender
                .send(Message::Notification(Notification::new(
                    PublishDiagnostics::METHOD.to_string(),
                    PublishDiagnosticsParams {
                        uri: params.text_document.uri,
                        diagnostics: vec![],
                        version: None,
                    },
                )));
            return Owed::Forget(closed);
        }
    }
    // Other notifications (didSave, etc.) are ignored.
    Owed::Nothing
}

/// What a notification leaves outstanding. A close must be able to CANCEL a
/// pending analysis: `reanalyze_root` falls back to reading the file from disk
/// when a document is not open, so an analysis still owed when the client closes
/// the document would re-publish diagnostics the close just cleared.
enum Owed {
    Analyze(Url),
    Forget(Url),
    Nothing,
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
            // (end_col == 0 → a single character/point). Columns leave the
            // frontend as chars and go on the wire as UTF-16 units.
            let line_text = line_of_text(source, line as usize);
            let (start_char, end_char) = if d.col == 0 {
                (0, line_utf16_len(source, d.line.saturating_sub(1)))
            } else {
                let s = char_col_to_utf16(line_text, d.col.saturating_sub(1));
                let e = if d.end_col == 0 {
                    s
                } else {
                    char_col_to_utf16(line_text, d.end_col.saturating_sub(1))
                };
                (s, e)
            };
            LspDiagnostic {
                range: Range {
                    start: Position {
                        line,
                        character: start_char,
                    },
                    end: Position {
                        line,
                        character: end_char,
                    },
                },
                severity: Some(match d.severity {
                    vyrn_frontend::diagnostics::Severity::Error => DiagnosticSeverity::ERROR,
                    vyrn_frontend::diagnostics::Severity::Warning => DiagnosticSeverity::WARNING,
                }),
                code: None,
                code_description: None,
                source: Some("vyrn".into()),
                // RFC-0033/0053: a remapped diagnostic keeps the generated
                // location as a note. VS Code renders a diagnostic's message
                // verbatim and has nowhere else to put it, so it is appended —
                // without it the squiggle in a `.vyx` gives no way to find the
                // generated line it actually fired on.
                message: match &d.note {
                    Some(n) => format!("{}\n{n}", d.message),
                    None => d.message.clone(),
                },
                related_information: None,
                tags: None,
                data: None,
            }
        })
        .collect();
    let _ = connection
        .sender
        .send(Message::Notification(Notification::new(
            PublishDiagnostics::METHOD.to_string(),
            PublishDiagnosticsParams {
                uri: uri.clone(),
                diagnostics: mapped,
                version: None,
            },
        )));
}

/// The UTF-16 length of line `line_idx` (0-based) in `source`, or 0 if out of
/// range. Uses `str::lines`, so a trailing `\r`/`\n` is not counted — this is
/// the visible line length (an LSP client clamps an end-of-line position to it).
fn line_utf16_len(source: &str, line_idx: usize) -> u32 {
    source
        .lines()
        .nth(line_idx)
        .map(|l| l.chars().map(char::len_utf16).sum::<usize>() as u32)
        .unwrap_or(0)
}
// ---------------------------------------------------------------------------
// RFC-0052 — a safelisted class hovers with the app's OWN CSS.
//
// `std/tw` generates nothing for a safelist entry, so the RFC-0042 hover can only
// say "safelisted (app-styled)". The rule is nevertheless right there in the
// app's stylesheet (the one its `head { stylesheet "…" }` declares), so we find
// it and append it verbatim. This is a heuristic tooltip, not a CSS model: no
// parser, no cascade, no "which rule wins" — it either finds whole-token
// selector matches or degrades to today's text.
// ---------------------------------------------------------------------------

/// Cap: at most this many matched rules per hover (a tooltip, not a stylesheet).
const MAX_CSS_RULES: usize = 3;
/// Cap: stop appending rules once the shown CSS reaches this many lines.
const MAX_CSS_LINES: usize = 40;
/// Bound on the `.vyx` files scanned for `stylesheet "…"` declarations.
const MAX_VYX_SCAN: usize = 64;

/// If `hover` is the RFC-0042 "safelisted (app-styled)" text, append the app's
/// own matching CSS rule(s); otherwise (a utility's generated rule, or any other
/// hover) return it untouched.
fn with_app_css(server: &Server, uri: &Url, hover: String) -> String {
    let Some(class) = safelisted_class_of(&hover) else {
        return hover;
    };
    let Some(path) = uri_path(uri) else {
        return hover;
    };
    let file = std::path::PathBuf::from(path.replace('/', std::path::MAIN_SEPARATOR_STR));
    let Some(dir) = file.parent() else {
        return hover;
    };
    let root = app_root_for(dir);
    let rules = app_css_rules(server, &root, &class);
    if rules.is_empty() {
        return hover;
    }
    let mut out = hover;
    for (rel, line, rule) in rules {
        out.push_str(&format!("\n\n```css\n{rule}\n```\n— {rel}:{line}"));
    }
    out
}

/// The class name of an RFC-0042 safelisted hover (`` **`plang`** — safelisted
/// (app-styled)``), or `None` for any other hover text.
fn safelisted_class_of(hover: &str) -> Option<String> {
    if !hover.ends_with("— safelisted (app-styled)") {
        return None;
    }
    let rest = hover.strip_prefix("**`")?;
    let end = rest.find("`**")?;
    Some(rest[..end].to_string())
}

/// The app's own rules matching `class`, as `(path relative to the app root,
/// 1-based line, rule text)`, in stylesheet then file order, capped.
fn app_css_rules(
    server: &Server,
    root: &std::path::Path,
    class: &str,
) -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    let mut lines = 0usize;
    for (path, text) in app_stylesheets(server, root) {
        for (line, rule) in css_rules_for_class(&text, class) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            lines += rule.lines().count();
            out.push((rel, line, rule));
            if out.len() >= MAX_CSS_RULES || lines >= MAX_CSS_LINES {
                return out;
            }
        }
    }
    out
}

/// The app's stylesheets (path + text), from the per-root cache when the cheap
/// signature is unchanged, else re-discovered and re-read.
fn app_stylesheets(server: &Server, root: &std::path::Path) -> Vec<(std::path::PathBuf, String)> {
    let mut cache = server.css_cache.borrow_mut();
    if let Some(idx) = cache.get(root) {
        if idx.sig == css_sig(root, idx.files.iter().map(|(p, _)| p.as_path())) {
            return idx.files.clone();
        }
    }
    let files: Vec<(std::path::PathBuf, String)> = discover_stylesheets(root)
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|t| (p, t)))
        .collect();
    let sig = css_sig(root, files.iter().map(|(p, _)| p.as_path()));
    cache.insert(
        root.to_path_buf(),
        CssIndex {
            sig,
            files: files.clone(),
        },
    );
    files
}

/// Cheap change signature for a root's stylesheets: every file's path, length and
/// mtime, plus the app root's and its `public/`'s directory mtimes (a directory
/// mtime moves when a stylesheet is added or removed, so a NEW stylesheet is
/// picked up without re-walking on every hover). Only `stat`s — never reads.
fn css_sig<'a>(root: &std::path::Path, files: impl Iterator<Item = &'a std::path::Path>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    fn stamp(p: &std::path::Path, h: &mut std::collections::hash_map::DefaultHasher) {
        p.to_string_lossy().hash(h);
        if let Ok(m) = std::fs::metadata(p) {
            m.len().hash(h);
            if let Ok(t) = m.modified() {
                if let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) {
                    d.as_nanos().hash(h);
                }
            }
        }
    }
    stamp(root, &mut h);
    stamp(&root.join("public"), &mut h);
    for f in files {
        stamp(f, &mut h);
    }
    h.finish()
}

/// Stylesheet files for an app root, in priority order:
/// 1. **Declared** — every `head { stylesheet "…" }` URL found in the app's `.vyx`
///    files, mapped by the serve convention (`/style.css` →
///    `<root>/public/style.css`, falling back to `<root>/style.css`). This is what
///    the browser actually loads.
/// 2. **Fallback** (only when nothing was declared or none of the declared files
///    exist) — every `*.css` directly under `<root>/public/` then `<root>/`.
///
/// Deduplicated; existing files only.
fn discover_stylesheets(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    fn push(p: std::path::PathBuf, out: &mut Vec<std::path::PathBuf>) {
        if p.is_file() && !out.contains(&p) {
            out.push(p);
        }
    }
    let mut vyx = Vec::new();
    collect_vyx(root, 0, &mut vyx);
    vyx.sort();
    for v in &vyx {
        let Ok(src) = std::fs::read_to_string(v) else {
            continue;
        };
        for url in stylesheet_urls(&src) {
            let rel = url.trim_start_matches('/');
            if rel.is_empty() {
                continue;
            }
            let rel = rel.replace('/', std::path::MAIN_SEPARATOR_STR);
            let in_public = root.join("public").join(&rel);
            if in_public.is_file() {
                push(in_public, &mut out);
            } else {
                push(root.join(&rel), &mut out);
            }
        }
    }
    if !out.is_empty() {
        return out;
    }
    for dir in [root.join("public"), root.to_path_buf()] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut css: Vec<std::path::PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("css"))
            .collect();
        css.sort();
        for p in css {
            push(p, &mut out);
        }
    }
    out
}

/// The URLs of `stylesheet "…"` declarations in a `.vyx` source (the RFC-0041
/// `head` block). Textual — a `stylesheet` keyword followed by a string literal.
fn stylesheet_urls(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in src.lines() {
        let Some(rest) = line.trim_start().strip_prefix("stylesheet") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('"') else {
            continue;
        };
        if let Some(end) = rest.find('"') {
            out.push(rest[..end].to_string());
        }
    }
    out
}

/// Recursively collect `.vyx` files under `root` (skipping vendored/hidden/build
/// and `public/` dirs), bounded like the RFC-0049 owner walk.
fn collect_vyx(dir: &std::path::Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
    if depth > MAX_WALK_UP || out.len() >= MAX_VYX_SCAN {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.starts_with('.')
                || name == "vyrn_vendor"
                || name == "target"
                || name == "node_modules"
                || name == "public"
            {
                continue;
            }
            subdirs.push(p);
        } else if p.extension().and_then(|x| x.to_str()) == Some("vyx") {
            out.push(p);
        }
    }
    subdirs.sort();
    for d in subdirs {
        collect_vyx(&d, depth + 1, out);
    }
}

/// Every rule block in `css` whose selector mentions `class` as a WHOLE token, as
/// `(1-based line of the block, verbatim block text)`.
///
/// Deliberately heuristic (RFC-0052): brace- and `/* … */`-aware scanning, no
/// parser. `.plang` matches `li.paste .plang`, `.plang:hover` and `a.plang`, but
/// NOT `.plangs` or `.plang-x`. At-rule bodies (`@media { … }`) are descended into
/// so inner rules are found; the at-rule context itself is not shown. No cascade
/// resolution — every textual match is reported, in file order.
fn css_rules_for_class(css: &str, class: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    collect_css_rules(css, 0, class, &mut out);
    out
}

/// Scan the top level of `css` (a whole file, or an at-rule's inner text starting
/// at byte `base` within the file) for matching rule blocks.
fn collect_css_rules(css: &str, base: usize, class: &str, out: &mut Vec<(usize, String)>) {
    let b = css.as_bytes();
    let mut i = 0usize;
    let mut sel_start = 0usize;
    while i < b.len() {
        // Skip comments wholesale (they must not open or close a block).
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            match css[i + 2..].find("*/") {
                Some(rel) => i += 2 + rel + 2,
                None => return,
            }
            sel_start = i;
            continue;
        }
        if b[i] == b'{' {
            let selector = css[sel_start..i].trim();
            let Some(close) = matching_brace(css, i) else {
                return;
            };
            if selector.starts_with('@') {
                collect_css_rules(&css[i + 1..close], base + i + 1, class, out);
            } else if selector_has_class(selector, class) {
                let lead = css[sel_start..].len() - css[sel_start..].trim_start().len();
                let start = sel_start + lead;
                out.push((
                    line_of(css, base + start),
                    css[start..=close].trim().to_string(),
                ));
            }
            i = close + 1;
            sel_start = i;
            continue;
        }
        if b[i] == b'}' {
            i += 1;
            sel_start = i;
            continue;
        }
        i += 1;
    }
}

/// Byte index of the `}` closing the `{` at `open`, comment-aware.
fn matching_brace(css: &str, open: usize) -> Option<usize> {
    let b = css.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2 + css[i + 2..].find("*/")? + 2;
            continue;
        }
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whether a selector mentions `class` as a whole class token: a `.` immediately
/// before it and no class-name character (alphanumeric, `-`, `_`) after it — so
/// `.plang` matches `li.paste .plang` and `.plang:hover`, but not `.plang-x`.
fn selector_has_class(selector: &str, class: &str) -> bool {
    let needle = format!(".{class}");
    let mut from = 0usize;
    while let Some(rel) = selector[from..].find(&needle) {
        let at = from + rel;
        let after = selector[at + needle.len()..].chars().next();
        if !matches!(after, Some(c) if c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return true;
        }
        from = at + needle.len();
    }
    false
}

/// 1-based line number of byte offset `at` in `css`.
fn line_of(css: &str, at: usize) -> usize {
    css[..at.min(css.len())].matches('\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyrn_frontend::ast::{EnumVariant, Type};

    /// A binding whose type is an anonymous enum is hinted, and hinted with the
    /// arm spelling hover writes.
    ///
    /// `Type`'s own `Display` writes `enum { A | B }` — payloads dropped. Hover
    /// writes the arms, and so does the hint, because both call
    /// [`vyrn_frontend::type_to_string`]. This pins the one string, so the two
    /// surfaces cannot drift apart.
    #[test]
    fn an_anonymous_enum_binding_is_hinted_with_the_hover_spelling() {
        let ty = Type::Enum(vec![
            EnumVariant {
                name: "A".to_string(),
                payload: vec![Type::Int],
            },
            EnumVariant {
                name: "B".to_string(),
                payload: vec![],
            },
        ]);
        let b = vyrn_frontend::LocalBinding {
            name: "e".to_string(),
            kind: LocalKind::Let { mutable: false },
            ty: Some(ty.clone()),
            line: 1,
            col: 5,
            end_col: 6,
            fn_line: 1,
        };
        let label = type_hint_label(&b, "let e = pick()", 6).expect("the call hides the type");
        assert_eq!(label, "{ A(Int64) | B }", "the arms, as hover writes them");
        assert_eq!(label, vyrn_frontend::type_to_string(&ty), "one renderer");
        assert_ne!(label, ty.to_string(), "`Display` drops the payloads");
    }

    // ------------------------------------------------------------------
    // UTF-16 ↔ char column conversion (LSP positions are UTF-16 code units;
    // the frontend counts chars — they diverge on every astral-plane char).
    // ------------------------------------------------------------------

    /// `show("🎉 done", here)` — the emoji is 1 char and 2 UTF-16 units, so
    /// every column past it differs by one between the two conventions.
    const EMOJI_LINE: &str = "show(\"🎉 done\", here)";

    #[test]
    fn utf16_and_char_columns_round_trip_through_an_astral_char() {
        for (char_col, units) in [(0usize, 0u32), (5, 5), (6, 7), (15, 16), (20, 21)] {
            assert_eq!(char_col_to_utf16(EMOJI_LINE, char_col), units);
            assert_eq!(utf16_to_char_col(EMOJI_LINE, units), char_col);
        }
        // A unit INSIDE the surrogate pair clamps to the pair's own char.
        assert_eq!(utf16_to_char_col(EMOJI_LINE, 6), 5);
        // Past the line end clamps to the line length.
        assert_eq!(
            utf16_to_char_col(EMOJI_LINE, 999),
            EMOJI_LINE.chars().count()
        );
    }

    #[test]
    fn an_inbound_utf16_position_lands_on_the_right_char() {
        // The client sends UTF-16 offset 16 for `here`'s `h` — char col 15,
        // which the frontend reads as 1-based col 16.
        let pos = Position {
            line: 0,
            character: 16,
        };
        assert_eq!(to_frontend(EMOJI_LINE, &pos), (1, 16));
    }

    #[test]
    fn an_outbound_range_leaves_in_utf16_units() {
        // `let s = "🎉"` — the string literal spans chars 9..11, units 9..12.
        let text = "let s = \"🎉\"";
        let r = lsp_range(text, 1, 10, 12);
        assert_eq!(r.start.character, 9);
        assert_eq!(r.end.character, 12, "the emoji counts as two units");
    }

    #[test]
    fn whole_document_range_ends_at_the_utf16_end_of_the_last_line() {
        let text = "let a = 1\nshow(\"🎉\")";
        let r = whole_document_range(text);
        assert_eq!(r.end.line, 1);
        // Last line is `show("🎉")`: 9 chars, 10 UTF-16 units. Counting chars
        // used to end the format edit one unit short per emoji and duplicate
        // the tail on save.
        assert_eq!(r.end.character, 10);
    }

    #[test]
    fn semantic_token_lengths_are_utf16_units() {
        // One token covering `"🎉x"` on its line: 4 chars, 5 units.
        let text = "let s = \"🎉x\"";
        let toks = vec![vyrn_frontend::SemToken {
            line: 1,
            col: 9,
            len: 4,
            kind: SemKind::Variable,
            mods: SemMods::default(),
        }];
        let encoded = encode_tokens(toks, text);
        assert_eq!(encoded.data.len(), 1);
        assert_eq!(encoded.data[0].delta_start, 8);
        assert_eq!(encoded.data[0].length, 5);
    }

    #[test]
    fn member_context_survives_a_non_ascii_receiver() {
        // `café.` — the old byte-indexed walk landed on a continuation byte of
        // `é` and silently withheld member completions.
        let cafedot = String::from("café.");
        assert!(is_member_context(Some(&cafedot), 1, 6));
        let partial = String::from("café.pu");
        assert!(is_member_context(Some(&partial), 1, 8));
        let noreceiver = String::from("café");
        assert!(!is_member_context(Some(&noreceiver), 1, 5));
        // The ASCII cases behave exactly as before.
        let ascii = String::from("arr.pu");
        assert!(is_member_context(Some(&ascii), 1, 7));
        let plain = String::from("let x = 1");
        assert!(!is_member_context(Some(&plain), 1, 10));
    }
}
