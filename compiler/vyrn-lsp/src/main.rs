//! A synchronous Language Server Protocol server for Vyrn, over stdio.
//!
//! A pure adapter: every answer comes from [`vyrn_frontend::analyze`] and the
//! queries over its result. One blocking `lsp-server` loop runs on a worker
//! thread with a large stack; no async runtime. Diagnostics are pushed on each
//! change; requests are answered in order.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use vyrn_frontend::loader::ModuleResolver;
use vyrn_frontend::session::Session;
use vyrn_genwasm::engine;

mod contracts;
mod cost;
mod rename;
mod templates;

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
    Notification as _, PublishDiagnostics,
};
use lsp_types::request::{RegisterCapability, Request as _};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CompletionItem, CompletionItemKind, CompletionOptions,
    CompletionParams, CompletionResponse, CompletionTextEdit, Diagnostic as LspDiagnostic,
    DiagnosticSeverity, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidChangeWatchedFilesRegistrationOptions, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentFormattingParams, DocumentHighlight, DocumentHighlightKind,
    DocumentHighlightParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse,
    Documentation, FileSystemWatcher, GlobPattern, GotoDefinitionParams, GotoDefinitionResponse,
    Hover, HoverContents, HoverParams, InitializeParams, InitializeResult, InlayHint,
    InlayHintKind, InlayHintLabel, InlayHintParams, InsertTextFormat, Location, MarkupContent,
    MarkupKind, OneOf, Position, PrepareRenameResponse, PublishDiagnosticsParams, Range,
    Registration, RegistrationParams, RelativePattern, RenameOptions, RenameParams, SemanticToken,
    SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams, SemanticTokensRangeParams,
    SemanticTokensRangeResult, SemanticTokensResult, SemanticTokensServerCapabilities,
    ServerCapabilities, ServerInfo, TextDocumentPositionParams, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Url, WorkspaceEdit,
};

use vyrn_frontend::symbolmap::MappedSymbol;
use vyrn_frontend::{
    analyze_judged, class_completions, class_token_hover, completions, member_completions,
    references, resolve, string_literal_completions, Analysis, Completion, LocalKind, RefRange,
    SemKind, SemMods, SymbolKind,
};

use templates::VyxCursor;

/// Whether the client asked for what each line costs ([`cost`]): its `costHints`
/// initialization option, on when it sends none.
static COST_HINTS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn cost_hints() -> bool {
    COST_HINTS.get().copied().unwrap_or(true)
}

/// The pipeline every analysis runs. It computes the cost of each function only when a hint
/// or a lens will show it.
fn judge() -> &'static vyrn_frontend::Judge {
    if cost_hints() {
        &vyrn_lower::JUDGE_COST
    } else {
        &vyrn_lower::JUDGE
    }
}

/// Analyze `text` through the pipeline `vyrn check` runs, with the project
/// the document's path lies in, under the server's `session`; an untitled
/// buffer loads as [`analyze_judged`] says. `overlays` maps every open
/// buffer's path to its live text, read in place of the file.
fn analyze_doc(
    session: &Arc<Session>,
    uri: &Url,
    text: &str,
    overlays: &HashMap<String, String>,
) -> Analysis {
    let (opts, resolver, path, manifest_error) = match load_context(session, uri, overlays) {
        Some(ctx) => ctx,
        None => {
            let opts = vyrn_frontend::loader::LoadOptions {
                std_root: std_root(),
                session: Some(session.clone()),
                ..Default::default()
            };
            let linker = Some(("untitled.vyrn", &opts, &**session as &dyn ModuleResolver));
            return analyze_judged(text, linker, Some(&*engine()), judge());
        }
    };
    let mut analysis = analyze_judged(
        text,
        Some((&path, &opts, &resolver)),
        Some(&*engine()),
        judge(),
    );
    // A manifest that does not parse would drop the import map and audience
    // rules silently, so it is an error on the open document.
    if let Some(e) = manifest_error {
        analysis.diagnostics.insert(
            0,
            vyrn_frontend::diagnostics::Diagnostic::error(1, 0, "parse", e).with_note(
                "note: the project's import map and audience rules cannot be read, \
                 so this file is analyzed without them"
                    .to_string(),
            ),
        );
    }
    // `VYRN_BUILD_PROFILE=1`: the phase table of one analysis, the cost of a
    // keystroke.
    if vyrn_frontend::prof::phases_on() {
        eprint!("{}", vyrn_frontend::prof::phase_table());
    }
    analysis
}

/// Build the load options + overlay-aware resolver + slash path for `uri`
/// under `session`, or `None` for an untitled buffer with no filesystem path.
fn load_context(
    session: &Arc<Session>,
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
        session: Some(session.clone()),
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
        session: session.clone(),
    };
    Some((opts, resolver, path, manifest_error))
}

// The project context is read by the build's own reader, so the editor never
// answers a different question from the build.
use vyrn_frontend::manifest::{find as find_manifest, pinned_blob, std_root, Lock};

/// Read-only, offline module resolver: local paths from open buffers or disk;
/// remote specifiers from `vyrn_vendor/` or the user cache, only when
/// `vyrn.lock` pins them. Never fetches.
struct EditorResolver {
    /// Directory holding `vyrn.json` (and thus `vyrn.lock` / `vyrn_vendor/`),
    /// if the document is inside a project.
    manifest_dir: Option<String>,
    /// Live text of every open buffer (slash path -> text). Empty for a plain
    /// analysis.
    overlays: HashMap<String, String>,
    /// The disk as the server last saw it.
    session: Arc<Session>,
}

impl vyrn_frontend::loader::ModuleResolver for EditorResolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        if !vyrn_frontend::loader::is_remote(resolved) {
            // Overlay keys are normalized; `resolved` keeps the loader's case.
            if let Some(text) = self
                .overlays
                .get(&vyrn_frontend::origin::OriginMaps::norm_path_key(resolved))
            {
                return Ok(text.clone());
            }
            return self.session.read(resolved);
        }
        let dir = self
            .manifest_dir
            .as_deref()
            .ok_or_else(|| "remote import outside a vyrn.json project".to_string())?;
        let (_, sha) = Lock::in_project(dir)?
            .entries
            .get(resolved)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "`{resolved}` is not pinned in vyrn.lock — run `vyrn check` once to fetch it"
                )
            })?;
        // Vendor, then the user cache, hash-verified as the build reads it.
        pinned_blob(Some(dir), &sha).unwrap_or_else(|| {
            Err(format!(
                "`{resolved}` is pinned but not cached — run `vyrn check` once to fetch it"
            ))
        })
    }

    /// The listings and the generator cache are the disk's; only `read` differs
    /// from a build. The shared cache lets a keystroke reuse a build's generation.
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        self.session.list_kinds(resolved)
    }
    fn gen_cache_get(&self, key: &str) -> Option<String> {
        vyrn_frontend::loader::DiskResolver.gen_cache_get(key)
    }
    fn gen_cache_put(&self, key: &str, value: &str) {
        vyrn_frontend::loader::DiskResolver.gen_cache_put(key, value)
    }
}

/// Appends a line to `$VYRN_LSP_LOG`, else `%TEMP%/vyrn-lsp-debug.log`. Always
/// on, so opening a file produces a trace.
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
    let (connection, io_threads) = Connection::stdio();
    dbg_log(&format!(
        "=== vyrn-lsp start pid={} cwd={:?} ===",
        std::process::id(),
        std::env::current_dir().ok()
    ));

    // Generator analysis recurses deeply and overflows the default 1 MB main
    // stack on Windows; 64 MB matches the CLI.
    let worker = std::thread::Builder::new()
        .name("vyrn-lsp".into())
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let mut server = Server {
                // An unchanged body keeps last keystroke's verdict. The server
                // only reads refusals; `vyrn` emits, so it arms no memo.
                // `VYRN_NO_MEMO=1` stands it aside for measurement.
                session: Session::new(std::env::var("VYRN_NO_MEMO").is_err()),
                docs: HashMap::new(),
                analyses: HashMap::new(),
                vyx_owner: HashMap::new(),
                vyx_ownerless: HashSet::new(),
                synth_cache: RefCell::new(HashMap::new()),
                css_cache: RefCell::new(HashMap::new()),
                contract_cache: RefCell::new(HashMap::new()),
                route_facts: RefCell::new(HashMap::new()),
                watched: Vec::new(),
            };
            // An error here means the client left before `initialize`.
            if let Ok(watched) = handle_initialize(&connection) {
                server.watched = watched;
                main_loop(&connection, &mut server);
            }
            connection
        })
        .expect("spawn vyrn-lsp worker thread");
    let connection = worker.join().expect("vyrn-lsp worker thread panicked");
    // Drop before the join: the writer thread exits only once its sender,
    // owned by `connection`, is dropped.
    drop(connection);
    io_threads.join().expect("LSP io threads panicked");
}

struct Server {
    /// Source text per open URI: Vyrn documents and generator inputs (`.vyx`).
    docs: HashMap<Url, String>,
    /// The [`Analysis`] per Vyrn document, built once per change so a request
    /// never re-parses.
    analyses: HashMap<Url, Analysis>,
    /// A generator input (slash path) -> the Vyrn root whose analysis generated
    /// from it: the root a `.vyx` request maps through and a `.vyx` edit
    /// re-analyzes. Filled by analysis and by owner discovery.
    vyx_owner: HashMap<String, Url>,
    /// `.vyx` files (slash path) whose owner discovery found no root. Cleared
    /// whole when a `.vyrn` opens or changes, and per file when a `.vyx` opens.
    vyx_ownerless: HashSet<String>,
    /// Per owner root, its generated modules and their analyses, which `.vyx`
    /// requests reuse. `RefCell` because request handlers hold `&Server`.
    synth_cache: RefCell<HashMap<Url, OwnerSynth>>,
    /// Per app root, its stylesheets for a safelisted-class hover. Re-read only
    /// when the signature (files' len+mtime, app-root and `public/` mtimes) moves.
    css_cache: RefCell<HashMap<std::path::PathBuf, CssIndex>>,
    /// Per app root, its roles and the contracts they resolve to.
    contract_cache: RefCell<HashMap<std::path::PathBuf, contracts::ContractIndex>>,
    /// Per api-module path, the mapped symbols a generating root claims for it.
    /// An empty answer is cached too. [`install_root`] is the only invalidation.
    route_facts: RefCell<HashMap<String, Rc<Vec<MappedSymbol>>>>,
    /// The directories whose file events the client was asked to send. The
    /// session keeps what it read under them once the client accepts.
    watched: Vec<String>,
    /// What the analyses keep between keystrokes. One thread analyses: the
    /// server's own caches are `Rc` and `RefCell`, and each analysis already
    /// runs its typing and placement on every core.
    session: Arc<Session>,
}

/// The id of the one request the server sends: the file-watcher registration.
const WATCH_REQUEST: &str = "vyrn/watchFiles";

/// One app root's stylesheets, with the signature they were read at.
struct CssIndex {
    sig: u64,
    /// `(absolute path, file text)` in discovery order (declared order first).
    files: Vec<(std::path::PathBuf, String)>,
}

/// One owner root's cached generation and per-module analyses.
struct OwnerSynth {
    /// Signature of the inputs; a mismatch invalidates the whole entry.
    sig: u64,
    /// Every generated module reachable from the owner, `(banner, gen_source)`.
    gen_modules: Vec<(String, String)>,
    /// Per banner, the module's analysis, filled on first request.
    analyzed: HashMap<String, Rc<AnalyzedSynth>>,
}

/// A generated module analyzed once and shared.
struct AnalyzedSynth {
    gen_source: String,
    analysis: Analysis,
    tokens: Vec<vyrn_frontend::SemToken>,
}

/// Answers `initialize`, and asks a client that can send file events for them
/// under its workspace folders and the std root. Returns those directories, or
/// none when the client cannot. Reads the `costHints` initialization option.
fn handle_initialize(connection: &Connection) -> Result<Vec<String>, ()> {
    let (id, params) = connection.initialize_start().map_err(|_| ())?;
    let params: InitializeParams = serde_json::from_value(params).unwrap_or_default();
    let off = (params.initialization_options.as_ref()).and_then(|o| o.get("costHints")?.as_bool());
    let _ = COST_HINTS.set(off.unwrap_or(true));

    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(true.into()),
        definition_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            // `.` for members; the rest for `.vyx` template completion.
            trigger_characters: Some(
                [".", "<", "@", ":", "-", " ", "\""]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            ..Default::default()
        }),
        document_symbol_provider: Some(OneOf::Left(true)),
        // The one code action: rename an export a closed contract does not name
        // (`laod` -> `data`).
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        // Scope-aware, so it replaces the client's textual word highlight.
        document_highlight_provider: Some(OneOf::Left(true)),
        // `prepare` refuses a bad cursor before the user types a new name.
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        document_formatting_provider: Some(OneOf::Left(true)),
        // Move hints. No resolve provider: the label is in the cached analysis.
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
    let events =
        (params.capabilities.workspace.as_ref()).and_then(|w| w.did_change_watched_files.as_ref());
    // A base outside the workspace needs a relative pattern.
    if !events.is_some_and(|e| {
        e.dynamic_registration == Some(true) && e.relative_pattern_support == Some(true)
    }) {
        return Ok(Vec::new());
    }
    #[allow(deprecated)]
    let folders = match params.workspace_folders {
        Some(fs) => fs.into_iter().map(|f| f.uri).collect(),
        None => params.root_uri.into_iter().collect::<Vec<_>>(),
    };
    let roots: Vec<String> = (folders.iter())
        .filter_map(|u| u.to_file_path().ok())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .chain(std_root())
        .collect();
    let watchers = (roots.iter())
        .filter_map(|r| Url::from_directory_path(r).ok())
        .map(|base| FileSystemWatcher {
            glob_pattern: GlobPattern::Relative(RelativePattern {
                base_uri: OneOf::Right(base),
                pattern: "**/*".to_string(),
            }),
            kind: None,
        })
        .collect();
    let register = RegistrationParams {
        registrations: vec![Registration {
            id: WATCH_REQUEST.to_string(),
            method: DidChangeWatchedFiles::METHOD.to_string(),
            register_options: serde_json::to_value(DidChangeWatchedFilesRegistrationOptions {
                watchers,
            })
            .ok(),
        }],
    };
    let request = Request::new(
        WATCH_REQUEST.to_string().into(),
        RegisterCapability::METHOD.to_string(),
        register,
    );
    connection
        .sender
        .send(Message::Request(request))
        .map_err(|_| ())?;
    Ok(roots)
}

fn main_loop(connection: &Connection, server: &mut Server) {
    // URIs whose analysis is owed, held only while more messages are queued:
    // a burst collapses, and an idle connection delays nothing.
    let mut owed: Vec<Url> = Vec::new();
    loop {
        let msg = if owed.is_empty() {
            match connection.receiver.recv() {
                Ok(m) => m,
                Err(_) => return,
            }
        } else {
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
                // Return so `main` can drain the io threads.
                if connection.handle_shutdown(&req).unwrap_or(false) {
                    return;
                }
                // A request reads the analysis, so settle what is owed first.
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
            // The loader trusts file events only once the client has agreed to
            // send them.
            Message::Response(resp) => {
                if resp.id == WATCH_REQUEST.to_string().into() && resp.error.is_none() {
                    server.session.watch(&server.watched);
                }
            }
        }
    }
}

/// Dispatch a request to its handler, or answer method-not-found.
fn handle_request(server: &mut Server, req: Request) -> Response {
    // Owner discovery for a `.vyx` also runs here, in case a request precedes
    // the open's discovery. It publishes no diagnostics; didOpen does.
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
        // Use `new_ok(id, Option<T>)`: `None` serializes as `result: null`. A
        // hand-built `Response { result: None, error: None }` omits both fields,
        // which the client rejects.
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
        "textDocument/codeAction" => {
            Response::new_ok(req.id, handle_code_action(server, req.params))
        }
        // An error, not a null, when there is nothing to rename: the client
        // shows the message.
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
        // A `-> f(..)` label at every move.
        "textDocument/inlayHint" => Response::new_ok(req.id, handle_inlay_hint(server, req.params)),
        "textDocument/semanticTokens/full" => {
            Response::new_ok(req.id, handle_semantic_tokens_full(server, req.params))
        }
        "textDocument/semanticTokens/range" => {
            Response::new_ok(req.id, handle_semantic_tokens_range(server, req.params))
        }
        // Whether to show the "Run dev server" CodeLens.
        "vyrn/isDevEntry" => {
            Response::new_ok(req.id, Some(handle_is_dev_entry(server, req.params)))
        }
        "vyrn/costLenses" => Response::new_ok(req.id, Some(handle_cost_lenses(server, req.params))),
        // A custom request, not `code_lens_provider`: `extension.js` builds
        // every lens from answers like this one.
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

/// `vyrn/isDevEntry`: [`is_dev_entry`] on the open buffer, else the file.
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
    let src = doc_text(server, &uri);
    match src {
        Some(text) => is_dev_entry(&text),
        None => false,
    }
}

/// `vyrn/costLenses`: one `{ line, title }` per function that allocates, copies, grows a
/// container or keeps a check ([`cost::lenses`]), `line` 0-based. Empty when the client turned
/// cost hints off, and for a document that does not check.
fn handle_cost_lenses(server: &Server, params: serde_json::Value) -> Vec<serde_json::Value> {
    let uri = (params.pointer("/textDocument/uri"))
        .and_then(|v| v.as_str())
        .and_then(|u| Url::parse(u).ok());
    let Some(uri) = uri.filter(|u| cost_hints() && is_vyrn_uri(u)) else {
        return Vec::new();
    };
    let (Some((analysis, _)), Some(src)) = (lookup(server, &uri), doc_text(server, &uri)) else {
        return Vec::new();
    };
    cost::lenses(analysis, uri_path(&uri).as_deref(), &src)
}

/// `vyrn/routeLenses`: one `{ line, title, method, path, source }` per mounted
/// procedure the document declares, `line` 0-based. Empty for a file no root
/// generates over.
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
        // The client's stub and the server's handler map the same declaration;
        // one lens per declaration.
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

/// Whether `source` is a root `vyrn dev` serves: it imports `std/rpc` and mounts
/// a server surface with `rpc(..)` or `rpcServer(..)`. Parses the root only.
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

/// The hover of a resolved name, its signature fenced as ` ```vyrn ` for
/// highlighting. `symbols.rs` supplies the signature and the prose after it;
/// the fence is presentation, so it lives in the adapter. A hover without a
/// signature is sent as it is.
fn fence_signature(r: &vyrn_frontend::Resolution) -> String {
    match &r.signature {
        Some(sig) if r.doc.is_empty() => format!("```vyrn\n{sig}\n```"),
        Some(sig) => format!("```vyrn\n{sig}\n```\n\n{}", r.doc),
        None => r.hover.clone(),
    }
}

fn handle_hover(server: &Server, params: serde_json::Value) -> Option<Hover> {
    let p: HoverParams = serde_json::from_value(params).ok()?;
    let uri = &p.text_document_position_params.text_document.uri;
    let text = doc_text(server, uri)?;
    let (line, col) = to_frontend(&text, &p.text_document_position_params.position);
    // The contract note and the derived wire facts stand without an ordinary
    // hover: in a `.vyx` script the forward map has nothing for a declaration.
    let note = match (
        contract_hover_note(server, uri, line, col),
        derived_hover_note(server, uri, line, col),
    ) {
        (Some(c), Some(d)) => Some(format!("{c}\n\n{d}")),
        (c, d) => c.or(d),
    };
    // A resolved name, else a class token, whose hover is not fenced.
    let ordinary = if is_vyrn_uri(uri) {
        lookup(server, uri).and_then(|(analysis, _)| match resolve(analysis, line, col) {
            Some(r) => Some(Ok(fence_signature(&r))),
            None => (server.docs.get(uri))
                .and_then(|src| class_token_hover(analysis, src, line, col))
                .map(Err),
        })
    } else {
        vyx_forward(server, uri, line, col).and_then(|fwd| {
            let a = &fwd.synth.analysis;
            match resolve(a, fwd.line, fwd.col) {
                Some(r) => Some(Ok(fence_signature(&r))),
                None => class_token_hover(a, &fwd.synth.gen_source, fwd.line, fwd.col).map(Err),
            }
        })
    };
    let (ordinary, safelisted) = match ordinary {
        Some(Ok(hover)) => (Some(hover), None),
        Some(Err(class)) => (Some(class.text), class.safelisted),
        None => (None, None),
    };
    let value = match (ordinary, note) {
        (Some(o), Some(n)) => format!("{o}\n\n---\n\n{n}"),
        // A safelisted class has no `std/tw` rule; append the app's own rules.
        (Some(o), None) => match safelisted {
            Some(class) => with_app_css(server, uri, o, &class),
            None => o,
        },
        (None, Some(n)) => n,
        (None, None) => return None,
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
    let text = doc_text(server, uri)?;
    let (line, col) = to_frontend(&text, &p.text_document_position_params.position);
    if is_vyrn_uri(uri) {
        if let Some(loc) = import_path_definition(server, uri, line, col) {
            return Some(loc);
        }
    }
    // A contract member's declaration jumps to the contract; ordinary
    // resolution would jump to itself.
    if let Some(loc) = contract_member_definition(server, uri, line, col) {
        return Some(loc);
    }
    let (r, home_uri) = if is_vyrn_uri(uri) {
        let (analysis, u) = lookup(server, uri)?;
        (resolve(analysis, line, col)?, Some(u))
    } else {
        // A component tag is no identifier in the generated module.
        if let Some(loc) = component_tag_definition(server, uri, line, col) {
            return Some(loc);
        }
        let fwd = vyx_forward(server, uri, line, col)?;
        (resolve(&fwd.synth.analysis, fwd.line, fwd.col)?, None)
    };
    // A built-in method resolves for hover but has no declaration.
    if !r.definition {
        return None;
    }
    // A remote module key (`github:...`) is no file, so it has no definition.
    let target_uri = match &r.target_file {
        Some(f) => Url::from_file_path(f.replace('/', std::path::MAIN_SEPARATOR_STR)).ok()?,
        // In place in a Vyrn document; a `.vyx` target in the generated module
        // has no file.
        None => home_uri?,
    };
    let target_text = doc_text(server, &target_uri).unwrap_or_default();
    Some(GotoDefinitionResponse::Scalar(Location {
        uri: target_uri,
        range: lsp_range(&target_text, r.target_line, r.target_col, r.target_end_col),
    }))
}

/// The sibling `CreateForm.vyx` for a cursor on `<CreateForm` or `</CreateForm`,
/// or `None` when there is no PascalCase tag or no such file.
fn component_tag_definition(
    server: &Server,
    uri: &Url,
    line: usize,
    col: usize,
) -> Option<GotoDefinitionResponse> {
    let raw = doc_text(server, uri)?;
    let text = raw.lines().nth(line.saturating_sub(1))?;
    let chars: Vec<char> = text.chars().collect();
    let cur = col.saturating_sub(1).min(chars.len());
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

/// The top of the file an import specifier string under the cursor names,
/// resolved by the linker's loader. `None` for a remote specifier.
fn import_path_definition(
    server: &Server,
    uri: &Url,
    line: usize,
    col: usize,
) -> Option<GotoDefinitionResponse> {
    let src = doc_text(server, uri)?;
    let spec = vyrn_frontend::import_spec_at(&src, line, col)?;
    if vyrn_frontend::loader::is_remote(&spec) {
        return None;
    }
    let overlays = overlays_of(server);
    let (opts, _resolver, importer, _) = load_context(&server.session, uri, &overlays)?;
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

/// The local file an import specifier names. A generator's directory argument
/// resolves to a `.vyrn` guess that does not exist, so it falls back to
/// [`dir_target`]. `None` when nothing on disk matches.
fn import_target_file(
    spec: &str,
    importer: &str,
    opts: &vyrn_frontend::loader::LoadOptions,
) -> Option<String> {
    let resolved = vyrn_frontend::loader::resolve_spec(spec, importer, opts).ok()?;
    if std::path::Path::new(&resolved).is_file() {
        return Some(resolved);
    }
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

/// The scope-aware references of the binding under the cursor; the definition
/// is `Write`, uses `Read`. `Some`, possibly empty, for an open document, so the
/// client does not fall back to word matching.
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

/// One verbatim origin region of a `.vyx`, aligned with its generated line.
struct VyxSpan<'a> {
    region: &'a vyrn_frontend::origin::Region,
    synth: &'a Rc<AnalyzedSynth>,
    vyx_line: &'a str,
    /// 1-based column in the generated line where the verbatim span starts, and
    /// its length in chars.
    gcol: usize,
    span_len: usize,
}

impl VyxSpan<'_> {
    /// Whether a generated range lies on the region's first line, inside the
    /// verbatim span. Only such a range maps back.
    fn within(&self, line: usize, col: usize, end_col: usize) -> bool {
        line == self.region.gen_start_line
            && col >= self.gcol
            && end_col <= self.gcol + self.span_len
    }

    /// The input column of a generated column inside the span.
    fn back(&self, gen_col: usize) -> usize {
        self.region.origin.col + (gen_col - self.gcol)
    }
}

/// Maps a per-feature answer about a `.vyx`'s generated modules back into the
/// `.vyx`: for each region of the file that `wants` (given the input line), the
/// region's module, its aligned span and the input line go to `step`, which
/// pushes mapped items. The result is sorted by `key` and deduplicated on it,
/// because overlapping regions can emit a position twice. Empty when the file
/// has no owner.
fn vyx_back_map<T, K: Ord>(
    server: &Server,
    vyx_uri: &Url,
    wants: impl Fn(&vyrn_frontend::origin::Region, &str) -> bool,
    mut step: impl FnMut(&VyxSpan, &mut Vec<T>),
    key: impl Fn(&T) -> K,
) -> Vec<T> {
    let mut out = Vec::new();
    let Some(vyx_path) = uri_path(vyx_uri) else {
        return out;
    };
    let Some(owner) = server.vyx_owner.get(&vyx_path) else {
        return out;
    };
    let Some(owner_analysis) = server.analyses.get(owner) else {
        return out;
    };
    let Some(vyx_text) = doc_text(server, vyx_uri) else {
        return out;
    };
    // `synth_for` re-hashes the owner on every call, so hold one per banner.
    let mut synths: HashMap<String, Option<Rc<AnalyzedSynth>>> = HashMap::new();
    for region in owner_analysis.origins.regions_for(&vyx_path) {
        let Some(vyx_line) = vyx_text.lines().nth(region.origin.line.saturating_sub(1)) else {
            continue;
        };
        if !wants(&region, vyx_line) {
            continue;
        }
        let Some(synth) = synths
            .entry(region.gen_module.clone())
            .or_insert_with(|| synth_for(server, owner, &region.gen_module))
        else {
            continue;
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
        let span = VyxSpan {
            region: &region,
            synth,
            vyx_line,
            gcol,
            span_len,
        };
        step(&span, &mut out);
    }
    out.sort_by_key(&key);
    out.dedup_by_key(|t| key(t));
    out
}

/// Highlights in a `.vyx`: references in the generated module, mapped back
/// through the verbatim origin regions. References outside a region are dropped.
fn vyx_highlights(server: &Server, vyx_uri: &Url, line: usize, col: usize) -> Vec<RefRange> {
    let Some(fwd) = vyx_forward(server, vyx_uri, line, col) else {
        return Vec::new();
    };
    let refs = references(&fwd.synth.analysis, fwd.line, fwd.col);
    vyx_back_map(
        server,
        vyx_uri,
        |_, _| true,
        |sp, out| {
            // `refs` index the cursor's module only.
            if !Rc::ptr_eq(sp.synth, &fwd.synth) {
                return;
            }
            for r in &refs {
                if sp.within(r.line, r.col, r.end_col) {
                    out.push(RefRange {
                        line: sp.region.origin.line,
                        col: sp.back(r.col),
                        end_col: sp.back(r.end_col),
                        write: r.write,
                    });
                }
            }
        },
        |r| (r.line, r.col),
    )
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
    // In a string literal: the class alphabet for a sequence type (`Tw`), else
    // the language of a finite string type (`t("` -> every key).
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
    // At module scope, the contract's members come first.
    if !is_member_context(Some(&raw), line, col) {
        let mut members = contract_completion_items(server, uri, line, col);
        members.append(&mut items);
        items = members;
    }
    Some(CompletionResponse::Array(items))
}

/// Completion in a `.vyx`: a structural position is answered from the template
/// vocabularies and sibling components; any other goes through the forward map.
fn vyx_completion(
    server: &Server,
    uri: &Url,
    line: usize,
    col: usize,
) -> Option<CompletionResponse> {
    let raw = doc_text(server, uri)?;
    match templates::classify(&raw, line, col) {
        VyxCursor::TagName { prefix, start_col } => Some(tag_name_completion(
            uri, &raw, &prefix, line, start_col, col,
        )),
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
            // The Tw alphabet comes from the generated module; an unthemed `.vyx`
            // gets nothing.
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
            // Contract members must not depend on the forward map: a blank line
            // at module scope has no origin, and that is where they are offered.
            let mut members = contract_completion_items(server, uri, line, col);
            let Some(fwd) = vyx_forward(server, uri, line, col) else {
                return (!members.is_empty()).then_some(CompletionResponse::Array(members));
            };
            let gen = &fwd.synth.gen_source;
            if is_string_literal_context(Some(gen), fwd.line, fwd.col) {
                if let Some(cls) = class_completions(&fwd.synth.analysis, gen, fwd.line, fwd.col) {
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
fn event_name_completion(
    raw: &str,
    line: usize,
    start_col: usize,
    col: usize,
) -> CompletionResponse {
    // Replace from the `@`, which the inserted label carries.
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

/// Class-token completions that replace the typed token.
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

/// Class-token completion with the token's start found on the buffer line.
fn class_completion_response(
    raw: &str,
    line: usize,
    col: usize,
    alphabet: Vec<Completion>,
) -> CompletionResponse {
    let start_col = class_token_start(raw, line, col);
    class_token_response(raw, line, start_col, col, alphabet)
}

/// The 1-based start column of the whitespace- or quote-delimited token at `col`.
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

/// The typed prefix on `line`, from 1-based `start_col` to `col` exclusive.
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

/// The range a completion `textEdit` replaces on 1-based `line`: 1-based char
/// columns `start_col..col`, sent as UTF-16 units.
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

/// A completion item that replaces `range` with `label`, so a token like
/// `md:hover:bg-...` does not repeat the typed prefix.
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

/// The directory of a `.vyx` URI and the component's base name.
fn vyx_dir_and_name(uri: &Url) -> Option<(std::path::PathBuf, String)> {
    let path = uri.to_file_path().ok()?;
    let dir = path.parent()?.to_path_buf();
    let name = path.file_stem()?.to_string_lossy().into_owned();
    Some((dir, name))
}

/// Common HTML element names for a lowercase tag.
const HTML_ELEMENTS: &[&str] = &[
    "div", "span", "p", "a", "ul", "ol", "li", "section", "header", "footer", "nav", "main",
    "article", "aside", "h1", "h2", "h3", "h4", "h5", "h6", "button", "input", "label", "select",
    "option", "textarea", "form", "img", "table", "thead", "tbody", "tr", "td", "th", "pre",
    "code", "strong", "em",
];

fn to_completion_item(c: vyrn_frontend::Completion) -> CompletionItem {
    CompletionItem {
        label: c.label,
        kind: Some(to_lsp_kind(c.kind)),
        detail: Some(c.detail),
        documentation: c.doc.map(|d| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: d,
            })
        }),
        ..Default::default()
    }
}

/// A `.vyx` cursor mapped into its generated module.
struct VyxFwd {
    synth: Rc<AnalyzedSynth>,
    /// 1-based generated line and column.
    line: usize,
    col: usize,
}

/// Map a `.vyx` cursor into its analyzed generated module (forward
/// origin mapping). A verbatim region maps column-exactly; a derived one (a `{#for}`
/// head) maps to the region's start. `None` outside any region or when the
/// owner cannot generate.
fn vyx_forward(server: &Server, vyx_uri: &Url, line: usize, col: usize) -> Option<VyxFwd> {
    let vyx_path = uri_path(vyx_uri)?;
    let owner = server.vyx_owner.get(&vyx_path)?.clone();
    let owner_analysis = server.analyses.get(&owner)?;

    // The rightmost region on this line that starts at or before the cursor.
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

    let synth = synth_for(server, &owner, &region.gen_module)?;

    let vyx_text = doc_text(server, vyx_uri)?;
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

/// The analysis of `owner`'s generated module `banner`, cached until the owner's
/// input signature moves. `None` if the owner cannot be read or does not
/// generate `banner`.
fn synth_for(server: &Server, owner: &Url, banner: &str) -> Option<Rc<AnalyzedSynth>> {
    let overlays = overlays_of(server);
    let (opts, resolver, owner_path, _) = load_context(&server.session, owner, &overlays)?;
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
            let gen_modules = vyrn_frontend::loader::generated_modules(
                &owner_text,
                &owner_path,
                &opts,
                &resolver,
                Some(&*engine()),
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
    // A linked root in the owner's directory, so relative imports resolve. Its
    // diagnostics ("no main") are never read.
    let synth_path = synth_path_for(&owner_path);
    let analysis = analyze_judged(
        &gen_source,
        Some((&synth_path, &opts, &resolver)),
        Some(&*engine()),
        judge(),
    );
    let tokens = vyrn_frontend::semantic_tokens(&analysis);
    let a = Rc::new(AnalyzedSynth {
        gen_source,
        analysis,
        tokens,
    });
    entry.analyzed.insert(banner.to_string(), a.clone());
    Some(a)
}

/// A signature of an owner's generation inputs: its text and every open buffer
/// under its directory. A file that is not open is not tracked.
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

/// Map an input cursor into a generated line, 1-based. `origin_col` is the
/// region's input start column. Column-exact when the input expression is found
/// in `gen_line`, else the line start.
fn map_into_region(
    vyx_line: &str,
    origin_col: usize,
    col: usize,
    gen_line: &str,
    gen_start_line: usize,
) -> (usize, usize) {
    let delta = col.saturating_sub(origin_col);
    match align_expr_span(vyx_line, origin_col, gen_line) {
        Some((gcol, _)) => (gen_start_line, gcol + delta),
        None => (gen_start_line, 1),
    }
}

/// A root path for a generated module in the owner's directory, so relative
/// imports resolve as at generation time.
fn synth_path_for(owner_path: &str) -> String {
    match owner_path.rfind('/') {
        Some(i) => format!("{}/__vyrn_vyx_synth__.vyrn", &owner_path[..i]),
        None => "__vyrn_vyx_synth__.vyrn".to_string(),
    }
}

/// The document's own top-level declarations, as a flat list. Imported symbols
/// carry a `file` and are skipped: their columns index another file.
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

/// One whole-document replace with the formatter's output; empty if already
/// canonical. `null` when the formatter refuses, so format-on-save never corrupts
/// a buffer mid-edit.
fn handle_formatting(server: &Server, params: serde_json::Value) -> Option<Vec<TextEdit>> {
    let p: DocumentFormattingParams = serde_json::from_value(params).ok()?;
    let text = server.docs.get(&p.text_document.uri)?;
    let formatted = vyrn_frontend::fmt(text).ok()?;
    if &formatted == text {
        return Some(vec![]);
    }
    Some(vec![TextEdit {
        range: whole_document_range(text),
        new_text: formatted,
    }])
}

/// A `Range` covering all of `text`.
fn whole_document_range(text: &str) -> Range {
    // The last line's length in UTF-16 units: an astral char is two, and a
    // short end would duplicate the tail on save.
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

/// The semantic token legend. The order of both vecs defines the wire indices;
/// [`sem_type_index`] and [`sem_mods_bits`] must agree with it.
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
            SemanticTokenType::KEYWORD,     // 9 (in the legend for parity; not
                                            //    currently emitted — the grammar
                                            //    owns keywords)
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,     // bit 0
            SemanticTokenModifier::READONLY,        // bit 1
            SemanticTokenModifier::DEFAULT_LIBRARY, // bit 2
            // Where an owning value stops being live. A standard modifier, so
            // every theme has a rule for it.
            SemanticTokenModifier::MODIFICATION, // bit 3
        ],
    }
}

/// The legend index of a [`SemKind`], per [`semantic_tokens_legend`].
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

/// The modifier bitset of [`SemMods`], per [`semantic_tokens_legend`].
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

/// Semantic tokens for the whole document. A `.vyx` maps through the origin map;
/// unmapped and region-level spans get none.
fn handle_semantic_tokens_full(
    server: &Server,
    params: serde_json::Value,
) -> Option<SemanticTokensResult> {
    let p: SemanticTokensParams = serde_json::from_value(params).ok()?;
    let toks = document_sem_tokens(server, &p.text_document.uri)?;
    let text = doc_text(server, &p.text_document.uri).unwrap_or_default();
    Some(SemanticTokensResult::Tokens(encode_tokens(toks, &text)))
}

/// Semantic tokens for a line range: the whole document's, filtered.
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
    Some(SemanticTokensRangeResult::Tokens(encode_tokens(
        toks, &text,
    )))
}

/// The classified tokens for `uri`; a `.vyx` maps them from its generated module.
fn document_sem_tokens(server: &Server, uri: &Url) -> Option<Vec<vyrn_frontend::SemToken>> {
    if is_vyrn_uri(uri) {
        let (analysis, _) = lookup(server, uri)?;
        Some(vyrn_frontend::semantic_tokens(analysis))
    } else {
        Some(vyx_semantic_tokens(server, uri))
    }
}

/// Inlay hints in the requested lines: a label at every move, the type of
/// every binding whose line does not say it, and what the line costs ([`cost::hints`]). A `.vyx` gets type hints only,
/// from [`vyx_type_hints`].
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
        if cost_hints() {
            let path = uri_path(&p.text_document.uri);
            hints.extend(cost::hints(analysis, path.as_deref(), src, from, to));
        }
    }
    Some(hints)
}

/// A `: Type` label after the name of every binding in lines `from..=to` whose
/// source does not already say its type (`let p = o.copy()` -> `p: Outer`). The
/// type is the analysis's, as hover renders it.
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
        let Some(label) = type_hint_label(b, &analysis.spellings, line, b.end_col) else {
            continue;
        };
        out.push(type_hint_at(line, b.line, b.end_col, label));
    }
    out
}

/// The `: Type` label a binding earns on the author's own `line`, or `None`
/// when the analysis gives it no type or the source already says it: an
/// annotation, or an initializer on `line` that shows it.
///
/// `end_col` is the 1-based column just past the name in `line`. The spelling is
/// hover's renderer, [`vyrn_frontend::type_to_string`].
fn type_hint_label(
    b: &vyrn_frontend::LocalBinding,
    spellings: &vyrn_frontend::ast::Spellings,
    line: &str,
    end_col: usize,
) -> Option<String> {
    // Only `let`s and `for` variables can hide a type.
    if !matches!(b.kind, LocalKind::Let { .. } | LocalKind::ForVar) {
        return None;
    }
    let label = vyrn_frontend::type_to_string(b.ty.as_ref()?, spellings);
    if b.annotated || init_shows_type(line, end_col, &label) {
        return None;
    }
    Some(label)
}

/// One type hint just past a name at 1-based `(line, end_col)`, `end_col` a char
/// column of `line_text`.
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

/// Type hints for a `.vyx`: bindings of the generated module mapped back through
/// the verbatim regions, as [`vyx_semantic_tokens`] does. A hint is kept only
/// where the author's own name ends at the mapped column; a misplaced hint is
/// worse than a missing one.
fn vyx_type_hints(server: &Server, vyx_uri: &Url, from: usize, to: usize) -> Vec<InlayHint> {
    vyx_back_map(
        server,
        vyx_uri,
        // A line without a binding keyword carries no hint; most template lines.
        |region, vyx_line| {
            (from..=to).contains(&region.origin.line)
                && (vyx_line.contains("let") || vyx_line.contains("for"))
        },
        |sp, out| {
            for b in &sp.synth.analysis.locals {
                if !sp.within(b.line, b.end_col, b.end_col) {
                    continue;
                }
                let col = sp.back(b.end_col);
                if !name_ends_at(sp.vyx_line, col, &b.name) {
                    continue;
                }
                let Some(label) =
                    type_hint_label(b, &sp.synth.analysis.spellings, sp.vyx_line, col)
                else {
                    continue;
                };
                out.push(type_hint_at(sp.vyx_line, sp.region.origin.line, col, label));
            }
        },
        |h| (h.position.line, h.position.character),
    )
}

/// Whether `name` ends just before 1-based char column `col` of `line`: the
/// proof that a mapped-back position is the author's name, not generator glue.
fn name_ends_at(line: &str, col: usize, name: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let end = col.saturating_sub(1);
    let len = name.chars().count();
    if end > chars.len() || len > end {
        return false;
    }
    chars[end - len..end].iter().collect::<String>() == name
}

/// Whether the initializer after a binding's name already says its type.
/// `end_col` is the 1-based char column just past the name, `ty` the rendered
/// type. True for:
///
/// * a literal, which is its own evidence (`3`, `"s"`, `true`, `[1, 2]`);
/// * an initializer that opens with the type's own name (`Outer { .. }`,
///   `Color.Red`).
///
/// Anything else hides the type. In doubt the answer is false: a hint too many
/// is noise, a hint too few is the feature not working.
fn init_shows_type(line: &str, end_col: usize, ty: &str) -> bool {
    let rest: String = line.chars().skip(end_col.saturating_sub(1)).collect();
    let rest = rest.trim_start();
    // The binding's `=` is the first one after its name, so a comparison inside
    // the initializer (`a == b`) cannot be mistaken for it.
    let Some((_, init)) = rest.split_once('=') else {
        // A `for` variable: its element type is never written.
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

/// Delta-encode tokens into the LSP wire form: sorted, then
/// `[delta line, delta start, len, type, mods]` in 0-based UTF-16 units.
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

/// Semantic tokens of a `.vyx`: the generated module's tokens inside each
/// verbatim origin region, re-anchored at the input columns. Derived regions
/// contribute nothing and stay with the TextMate grammar.
fn vyx_semantic_tokens(server: &Server, vyx_uri: &Url) -> Vec<vyrn_frontend::SemToken> {
    vyx_back_map(
        server,
        vyx_uri,
        |_, _| true,
        |sp, out| {
            for st in sp.synth.tokens.iter() {
                if sp.within(st.line, st.col, st.col + st.len) {
                    out.push(vyrn_frontend::SemToken {
                        line: sp.region.origin.line,
                        col: sp.back(st.col),
                        len: st.len,
                        kind: st.kind,
                        mods: st.mods,
                    });
                }
            }
        },
        |t| (t.line, t.col),
    )
}

/// The `(1-based gen col, char length)` of the longest prefix of the input tail
/// at `origin_col` found in `gen_line`: the verbatim span of a region. The bytes
/// after the expression (`}`, `>`) diverge from the generated wrapper.
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

/// An outline entry for a top-level symbol; `None` for a field, param or local,
/// which the top-level index never holds.
fn to_document_symbol(sym: &vyrn_frontend::Symbol, text: &str) -> Option<DocumentSymbol> {
    let kind = match sym.kind {
        SymbolKind::Function => lsp_types::SymbolKind::FUNCTION,
        SymbolKind::Method => lsp_types::SymbolKind::METHOD,
        SymbolKind::Type => lsp_types::SymbolKind::STRUCT,
        SymbolKind::Variant => lsp_types::SymbolKind::ENUM_MEMBER,
        SymbolKind::Global => lsp_types::SymbolKind::VARIABLE,
        SymbolKind::Field | SymbolKind::Param | SymbolKind::Local => return None,
    };
    let range = lsp_range(text, sym.line, sym.col, sym.end_col);
    let detail = if sym.detail.is_empty() {
        None
    } else {
        Some(sym.detail.clone())
    };
    // `DocumentSymbol` has no `Default`, so the deprecated field must be named.
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

/// Whether the cursor at 1-based `(line, col)` follows a `.`, past the partial
/// member name and spaces.
fn is_member_context(text: Option<&String>, line: usize, col: usize) -> bool {
    let line_text = match text.and_then(|t| t.lines().nth(line.saturating_sub(1))) {
        Some(l) => l,
        None => return false,
    };
    // `col` counts chars, so walk chars, not bytes.
    let chars: Vec<char> = line_text.chars().collect();
    let mut i = col.saturating_sub(2);
    // Skip the partial member name; identifiers are Unicode, as in the lexer.
    while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
        if i == 0 {
            return false;
        }
        i -= 1;
    }
    while i < chars.len() && chars[i] == ' ' {
        if i == 0 {
            return false;
        }
        i -= 1;
    }
    i < chars.len() && chars[i] == '.'
}

/// Whether an odd number of unescaped `"` precede the 1-based `(line, col)` on
/// its line. A per-line scan to route completion; the frontend re-lexes.
fn is_string_literal_context(text: Option<&String>, line: usize, col: usize) -> bool {
    let line_text = match text.and_then(|t| t.lines().nth(line.saturating_sub(1))) {
        Some(l) => l,
        None => return false,
    };
    let mut in_str = false;
    let mut escaped = false;
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

/// The cached [`Analysis`] of an open document.
fn lookup<'s>(server: &'s Server, uri: &Url) -> Option<(&'s Analysis, Url)> {
    Some((server.analyses.get(uri)?, uri.clone()))
}

/// A document seen through the contract that governs it.
struct ContractCtx {
    view: vyrn_frontend::contracts::ContractView,
    /// The buffer for a `.vyrn`, the `<script>` body for a `.vyx`.
    source: String,
    /// Lines to add to a `source` position to reach the buffer (0 for `.vyrn`).
    line_offset: usize,
    /// The members the document's form writes, such as a `.vyx`'s `<template>`.
    /// Completion must not offer one: it would collide with the generated export.
    synthesized: Vec<String>,
}

impl ContractCtx {
    /// A buffer line -> its line in [`Self::source`], or `None` outside a
    /// `.vyx` script.
    fn to_source_line(&self, line: usize) -> Option<usize> {
        let l = line.checked_sub(self.line_offset)?;
        (l > 0 && l <= self.source.lines().count() + 1).then_some(l)
    }
}

/// Resolve `uri` to the contract that governs it, if any.
///
/// App root -> roles (declared in `vyrn.json`, else discovered) -> the role
/// covering this file -> its contract, all by the frontend. `None` for a file in
/// no role or an unreadable contract module.
fn contract_ctx(server: &Server, uri: &Url) -> Option<ContractCtx> {
    let path = uri_path(uri)?;
    let text = doc_text(server, uri)?;
    // `raw` is the whole `.vyx`, whose `<template>` is outside the script body.
    // Empty for a `.vyrn`.
    let (source, line_offset, raw) = if is_vyrn_uri(uri) {
        (text, 0, String::new())
    } else {
        let (body, offset) = contracts::vyx_script(&text)?;
        (body, offset, text)
    };

    let dir = std::path::Path::new(&path).parent()?.to_path_buf();
    let app_dir = vyrn_frontend::manifest::app_root(&dir);
    let overlays = overlays_of(server);
    let (opts, resolver, _, _) = load_context(&server.session, uri, &overlays)?;

    let mut cache = server.contract_cache.borrow_mut();
    // The same two functions `vyrn why --contract` asks, so the two agree.
    // A missing or unreadable manifest is no manifest: the editor never refuses.
    let doc = vyrn_frontend::manifest::find(&app_dir)
        .ok()
        .flatten()
        .map(|m| m.doc);
    let roots = vyrn_frontend::manifest::role_roots(&app_dir, doc.as_ref());
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
        entry.roles =
            vyrn_frontend::contracts::roles_for_project(doc.as_ref(), &roots, &opts, &resolver);
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
    // A role's relative specifier is relative to the manifest, not the page.
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

/// The governing contract's members as full declarations, offered only at module
/// scope of a file some role covers.
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
    // What the document already exports, from its source or its form.
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

/// The contract member named at the cursor, when the cursor is at module scope
/// in a governed file; a local called `data` is no member.
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

/// The `member of contract ...` block appended to a member's hover.
fn contract_hover_note(server: &Server, uri: &Url, line: usize, col: usize) -> Option<String> {
    let (ctx, name) = contract_member_at(server, uri, line, col)?;
    vyrn_frontend::contracts::contract_member_hover(&ctx.view, &name)
}

/// Go-to-definition on a contract member name -> the member in the contract.
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

/// The did-you-mean rename of an export a closed contract does not name.
///
/// Computed from the contract, not by parsing a generator's diagnostic text.
/// The client's diagnostics are attached by range overlap, so the lightbulb
/// appears on the squiggle.
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

/// Whether two LSP ranges share a position; touching ends count.
fn ranges_overlap(a: &Range, b: &Range) -> bool {
    let key = |p: &Position| (p.line, p.character);
    key(&a.start) <= key(&b.end) && key(&b.start) <= key(&a.end)
}

// LSP positions are UTF-16 code units (the default `positionEncoding`); frontend
// columns count chars. Every crossing converts through the line's text.

/// The UTF-16 offset of 0-based char column `char_col` in `line` (clamped to the
/// line's end).
pub(crate) fn char_col_to_utf16(line: &str, char_col: usize) -> u32 {
    line.chars()
        .take(char_col)
        .map(|c| c.len_utf16() as u32)
        .sum()
}

/// The 0-based char column of UTF-16 offset `units` in `line`, clamped to the
/// line's end; inside a surrogate pair, the next char.
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

/// LSP 0-based UTF-16 position -> frontend 1-based (line, char col).
fn to_frontend(text: &str, pos: &Position) -> (usize, usize) {
    let col = utf16_to_char_col(line_of_text(text, pos.line as usize), pos.character);
    ((pos.line + 1) as usize, col + 1)
}

/// Frontend 1-based `(line, col, end_col)` in `text`'s char columns -> LSP
/// `Range`. A col of 0 means an unknown column: a zero-length range at the
/// line start.
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
        SymbolKind::Global => CompletionItemKind::VARIABLE,
        // `completions` never returns these; the match is exhaustive.
        SymbolKind::Param | SymbolKind::Local => CompletionItemKind::VARIABLE,
    }
}

/// Whether `uri` is a `.vyrn` source. Anything else the server tracks is a
/// generator input, analyzed through the Vyrn document that consumes it.
fn is_vyrn_uri(uri: &Url) -> bool {
    uri.path().ends_with(".vyrn")
}

/// The normalized slash path of `uri`, or `None` for a non-file URI. The client
/// sends `file:///n%3A/...` where the loader has `N:/...`, so every path key
/// goes through `norm_path_key`.
fn uri_path(uri: &Url) -> Option<String> {
    let p = uri
        .to_file_path()
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    Some(vyrn_frontend::origin::OriginMaps::norm_path_key(&p))
}

/// The `textDocument.uri` a request targets.
fn request_uri(req: &Request) -> Option<Url> {
    let s = req.params.pointer("/textDocument/uri")?.as_str()?;
    Url::parse(s).ok()
}

/// Every open buffer as slash path -> text, so generation sees unsaved edits.
fn overlays_of(server: &Server) -> HashMap<String, String> {
    server
        .docs
        .iter()
        .filter_map(|(u, t)| uri_path(u).map(|p| (p, t.clone())))
        .collect()
}

/// Re-analyze the Vyrn document `root_uri` (open buffer, else disk) and install
/// it with [`install_root`], publishing.
fn reanalyze_root(connection: &Connection, server: &mut Server, root_uri: &Url) {
    let text = match server.docs.get(root_uri) {
        Some(t) => t.clone(),
        None => match uri_path(root_uri).and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(t) => t,
            None => return,
        },
    };
    let overlays = overlays_of(server);
    let analysis = analyze_doc(&server.session, root_uri, &text, &overlays);
    install_root(Some(connection), server, root_uri, &text, analysis);
}

/// Install a root's `analysis`: own every generator input it reads, refresh the
/// route facts, cache the analysis, and, given a `Connection`, publish the
/// root's and inputs' diagnostics. Discovery passes `None`.
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
    for f in analysis.origins.input_files() {
        let f = vyrn_frontend::origin::OriginMaps::norm_path_key(&f);
        server.vyx_ownerless.remove(&f);
        server.vyx_owner.insert(f, root_uri.clone());
    }
    if let Some(c) = connection {
        publish_remapped(c, server, &analysis);
    }
    // The route facts' only invalidation. A stale entry can only hide a route
    // note, because the hover matches on name and line.
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
    server.synth_cache.borrow_mut().remove(root_uri);
    server.analyses.insert(root_uri.clone(), analysis);
}

// `.vyx` owner discovery: a `.vyx` opened alone has no owner until a `.vyrn`
// that generates from it is analyzed. Discovery ranks the `.vyrn` files under
// the app root and analyzes them in order, within a bound, until one's origins
// claim the `.vyx`.

/// The most `.vyrn` roots discovery analyzes for one `.vyx`.
const MAX_OWNER_CANDIDATES: usize = 48;
/// The deepest directory level `collect_sources` descends to.
const MAX_COLLECT_DEPTH: usize = 8;

/// Wire `vyx_uri`'s owner, discovering it without publishing. A no-op for a
/// `.vyx` already owned or known ownerless.
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

/// Discover and wire `vyx_uri`'s owner, publishing diagnostics. Returns whether
/// an owner was found; a miss is cached as ownerless.
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
            let text = doc_text(server, &owner).unwrap_or_default();
            install_root(Some(connection), server, &owner, &text, analysis);
            server.vyx_owner.contains_key(&path)
        }
        None => {
            server.vyx_ownerless.insert(path);
            false
        }
    }
}

/// The first candidate root whose origins claim `vyx_path`, with its analysis.
/// Mutates nothing; the caller wires the winner.
fn probe_owner(server: &Server, vyx_path: &str) -> Option<(Url, Analysis)> {
    let want = vyrn_frontend::origin::OriginMaps::norm_path_key(vyx_path);
    probe_roots(server, vyx_path, |a| {
        a.origins
            .input_files()
            .iter()
            .any(|f| vyrn_frontend::origin::OriginMaps::norm_path_key(f) == want)
    })
}

/// Analyze the ranked `.vyrn` roots near `path` until one `claims` it.
fn probe_roots(
    server: &Server,
    path: &str,
    claims: impl Fn(&Analysis) -> bool,
) -> Option<(Url, Analysis)> {
    let overlays = overlays_of(server);
    for cand in candidate_owners(path) {
        let text = match doc_text(server, &cand) {
            Some(t) => t,
            None => continue,
        };
        let analysis = analyze_doc(&server.session, &cand, &text, &overlays);
        if claims(&analysis) {
            return Some((cand, analysis));
        }
    }
    None
}

// A declaration's derived wire facts. An api module reaches no generator, so
// only a root that mounts it knows its route. Analyzed roots are asked first;
// a ranked probe of mounting roots is the fallback.

/// Every mapped symbol whose origin is `path`, from a root that generates over
/// it. Cached per file, the empty answer too.
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
        // Only roots that call a map-emitting generator, so a hover in a module
        // nothing mounts analyzes nothing.
        mounting_roots(path, RPC_GENERATORS)
            .into_iter()
            .find_map(|cand| {
                let text = doc_text(server, &cand)?;
                let a = analyze_doc(&server.session, &cand, &text, &overlays_of(server));
                claims(&a).then_some(a)
            })
    };
    // Cache every file these analyses map, and the requested path either way,
    // so an unmounted module is probed once.
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

/// The generators whose maps carry a derived route, for hovers and lenses.
const RPC_GENERATORS: &[&str] = &[
    "rpc(",
    "rpcServer(",
    "client(",
    "rpcClient(",
    "rpcInProcess(",
];

/// Every generator that emits a map. `http()`'s carries no route, but a rename
/// needs it: nothing else records that its re-export is the same declaration.
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
/// A textual filter, so as few roots as possible are analyzed.
fn mounting_roots(path: &str, gens: &[&str]) -> Vec<Url> {
    let file = std::path::Path::new(path);
    let Some(dir) = file.parent() else {
        return Vec::new();
    };
    let app_root = vyrn_frontend::manifest::app_root(dir);
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

/// Every generated symbol standing for a declaration in `path`, from every root
/// that maps the file. [`route_facts_for_file`] stops at the first root; a
/// rename cannot, because `client`, `rpc` and `http` may live in three roots.
fn all_mapped_symbols(server: &Server, path: &str) -> Vec<MappedSymbol> {
    let mut out: Vec<MappedSymbol> = route_facts_for_file(server, path).as_ref().clone();
    let overlays = overlays_of(server);
    for cand in mounting_roots(path, MAP_GENERATORS) {
        let Some(text) = doc_text(server, &cand) else {
            continue;
        };
        let a = analyze_doc(&server.session, &cand, &text, &overlays);
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

/// The mapped symbols for the document `uri`, for route lenses and hovers.
fn route_facts(server: &Server, uri: &Url) -> Rc<Vec<MappedSymbol>> {
    if !is_vyrn_uri(uri) {
        return Rc::new(Vec::new());
    }
    match uri_path(uri) {
        Some(p) => route_facts_for_file(server, &p),
        None => Rc::new(Vec::new()),
    }
}

/// The derived wire facts (`POST /_/pastes/create`) appended to a hover on a
/// procedure declaration's own name; a use of the name gets none.
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

// Rename: the target is resolved here, from the server's caches; `rename.rs`
// does the rest, pure over what it is handed.

/// The rename target at a position, or the reason there is none. A `.vyx` is
/// refused: the file to edit would be a generated module.
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

/// The prepare-rename range and placeholder, or the refusal.
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

/// The rename edit, over source files only.
fn handle_rename(server: &Server, params: serde_json::Value) -> Result<WorkspaceEdit, String> {
    let p: RenameParams = serde_json::from_value(params).map_err(|e| e.to_string())?;
    let (target, uri) = rename_target(server, &p.text_document_position)?;
    let overlays = overlays_of(server);
    // The manifest's aliases, so an import through one resolves as in the linker.
    let opts = load_context(&server.session, &uri, &overlays)
        .map(|(o, _, _, _)| o)
        .unwrap_or_else(|| vyrn_frontend::loader::LoadOptions {
            std_root: std_root(),
            ..Default::default()
        });
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

/// The `.vyrn` files under the app root to try as owners of `vyx_path`, at most
/// [`MAX_OWNER_CANDIDATES`]: a generator importer that names the `.vyx`'s
/// directory first, then any generator importer, then by proximity.
fn candidate_owners(vyx_path: &str) -> Vec<Url> {
    let vyx = std::path::Path::new(vyx_path);
    let Some(vyx_dir) = vyx.parent() else {
        return Vec::new();
    };
    let dir_name = vyx_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let app_root = vyrn_frontend::manifest::app_root(vyx_dir);

    let mut files = Vec::new();
    collect_vyrn(&app_root, 0, &mut files);

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
            let proximity = path_distance(&p, vyx_dir);
            (score, proximity, p)
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(MAX_OWNER_CANDIDATES)
        .filter_map(|(_, _, p)| Url::from_file_path(p).ok())
        .collect()
}

/// Whether a `.vyrn` source mentions a page or component generator, the roots
/// that own `.vyx` files. A textual heuristic that only ranks owner candidates:
/// a comment or a string that spells `pages(` counts.
fn has_generator_import(src: &str) -> bool {
    src.contains("pagesThemed")
        || src.contains("componentsThemed")
        || src.contains("pages(")
        || src.contains("components(")
        || src.contains("pages ")
        || src.contains("components ")
}

/// The `.vyrn` files under `dir`, at most [`MAX_OWNER_CANDIDATES`].
fn collect_vyrn(dir: &std::path::Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
    collect_sources(dir, depth, MAX_OWNER_CANDIDATES, &["vyrn"], out);
}

/// Collect files with one of `exts` under `dir`, at most `cap`, skipping hidden,
/// vendored, build and `public/` directories. Subdirectories are visited in name
/// order, so a capped walk is deterministic.
fn collect_sources(
    dir: &std::path::Path,
    depth: usize,
    cap: usize,
    exts: &[&str],
    out: &mut Vec<std::path::PathBuf>,
) {
    if out.len() >= cap || depth > MAX_COLLECT_DEPTH {
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
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| exts.contains(&x))
        {
            out.push(p);
        }
    }
    subdirs.sort();
    for sub in subdirs {
        collect_sources(&sub, depth + 1, cap, exts, out);
        if out.len() >= cap {
            return;
        }
    }
}

/// The number of path components outside the common prefix of `cand`'s
/// directory and `vyx_dir`.
fn path_distance(cand: &std::path::Path, vyx_dir: &std::path::Path) -> usize {
    let a: Vec<_> = cand.parent().unwrap_or(cand).components().collect();
    let b: Vec<_> = vyx_dir.components().collect();
    let common = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    (a.len() - common) + (b.len() - common)
}

/// Publish remapped diagnostics per generator input, so a template error shows
/// in its `.vyx`. Every input is republished, empty when clean, so a fix clears.
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
        if let Ok(uri) = Url::from_file_path(file.replace('/', std::path::MAIN_SEPARATOR_STR)) {
            // The open buffer: the ranges index the text the user sees.
            let src = server
                .docs
                .get(&uri)
                .cloned()
                .unwrap_or_else(|| std::fs::read_to_string(&file).unwrap_or_default());
            publish(connection, &uri, &src, &diags);
        }
    }
}

/// Analyze after an open or change: a Vyrn document itself, a generator input
/// its owner, and an input without a known owner through discovery.
fn refresh_document(connection: &Connection, server: &mut Server, uri: &Url) {
    if is_vyrn_uri(uri) {
        // A `.vyrn` change may have made an owner.
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

/// Apply a notification to the server state and return the analysis it owes.
/// `main_loop` defers that until the queue drains, so a burst of keystrokes
/// costs one analysis; a `.vyx` edit re-runs generators for seconds.
fn handle_notification(connection: &Connection, server: &mut Server, notif: Notification) -> Owed {
    if DidOpenTextDocument::METHOD == notif.method {
        if let Ok(params) = serde_json::from_value::<DidOpenTextDocumentParams>(notif.params) {
            let uri = params.text_document.uri.clone();
            let text = params.text_document.text;
            server.docs.insert(uri.clone(), text.clone());
            // Opening a `.vyx` retries owner discovery.
            if let Some(p) = uri_path(&uri) {
                server.vyx_ownerless.remove(&p);
            }
            return Owed::Analyze(uri);
        }
    } else if DidChangeTextDocument::METHOD == notif.method {
        if let Ok(params) = serde_json::from_value::<DidChangeTextDocumentParams>(notif.params) {
            let uri = params.text_document.uri.clone();
            // Full sync: the last change is the whole text.
            if let Some(change) = params.content_changes.into_iter().last() {
                server.docs.insert(uri.clone(), change.text.clone());
                return Owed::Analyze(uri);
            }
        }
    } else if DidChangeWatchedFiles::METHOD == notif.method {
        if let Ok(params) = serde_json::from_value::<DidChangeWatchedFilesParams>(notif.params) {
            for change in params.changes {
                if let Ok(p) = change.uri.to_file_path() {
                    server.session.changed(&p.to_string_lossy());
                }
            }
        }
    } else if DidCloseTextDocument::METHOD == notif.method {
        if let Ok(params) = serde_json::from_value::<DidCloseTextDocumentParams>(notif.params) {
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
    Owed::Nothing
}

/// What a notification leaves outstanding. A close cancels a pending analysis,
/// which would read the file from disk and re-publish the cleared diagnostics.
enum Owed {
    Analyze(Url),
    Forget(Url),
    Nothing,
}

/// Push the frontend's diagnostics for `uri` to the client.
///
/// `source` is the text the diagnostics were computed from. A diagnostic with
/// `col == 0` knows only its line and squiggles the whole line; a zero-length
/// range would mark just the first token.
fn publish(
    connection: &Connection,
    uri: &Url,
    source: &str,
    diags: &[vyrn_frontend::diagnostics::Diagnostic],
) {
    let mapped: Vec<LspDiagnostic> = diags
        .iter()
        .map(|d| {
            let line = d.line.saturating_sub(1) as u32;
            // `end_col == 0` is a point.
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
                // The note (a remapped diagnostic's generated location) has no
                // other place in the client, so it joins the message.
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

/// The UTF-16 length of 0-based line `line_idx` in `source` without its line
/// ending, or 0 if out of range.
fn line_utf16_len(source: &str, line_idx: usize) -> u32 {
    source
        .lines()
        .nth(line_idx)
        .map(|l| l.chars().map(char::len_utf16).sum::<usize>() as u32)
        .unwrap_or(0)
}
// A safelisted class has no `std/tw` rule, so its hover appends the app's own
// matching CSS rules verbatim. A textual heuristic: no parser, no cascade.

/// The most matched rules per hover.
const MAX_CSS_RULES: usize = 3;
/// Stop appending rules once the shown CSS reaches this many lines.
const MAX_CSS_LINES: usize = 40;
/// The most `.vyx` files scanned for `stylesheet "..."` declarations.
const MAX_VYX_SCAN: usize = 64;

/// `hover` with the app's CSS rules for the safelisted `class` appended.
fn with_app_css(server: &Server, uri: &Url, hover: String, class: &str) -> String {
    let Some(path) = uri_path(uri) else {
        return hover;
    };
    let file = std::path::PathBuf::from(path.replace('/', std::path::MAIN_SEPARATOR_STR));
    let Some(dir) = file.parent() else {
        return hover;
    };
    let root = vyrn_frontend::manifest::app_root(dir);
    let rules = app_css_rules(server, &root, class);
    if rules.is_empty() {
        return hover;
    }
    let mut out = hover;
    for (rel, line, rule) in rules {
        out.push_str(&format!("\n\n```css\n{rule}\n```\n— {rel}:{line}"));
    }
    out
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

/// The app's stylesheets (path, text), cached per root until [`css_sig`] moves.
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

/// A change signature for a root's stylesheets from `stat` alone: each file's
/// path, length and mtime, plus the mtimes of the root and `public/`, which move
/// when a stylesheet is added or removed.
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
/// 1. Declared: every `stylesheet "..."` URL in the app's `.vyx` files, served
///    from `<root>/public/`, else `<root>/`.
/// 2. Only if none of those exist: every `*.css` directly under `<root>/public/`
///    then `<root>/`.
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
    collect_sources(root, 0, MAX_VYX_SCAN, &["vyx"], &mut vyx);
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

/// The URLs of the `stylesheet "..."` lines in a `.vyx` source, found by text.
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

/// Every rule block in `css` whose selector names `class` as a whole token, as
/// `(1-based line, verbatim block)`, in file order. A brace- and comment-aware
/// scan; at-rule bodies are searched, but the at-rule is not shown.
fn css_rules_for_class(css: &str, class: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    collect_css_rules(css, 0, class, &mut out);
    out
}

/// Scan the top level of `css`, a file or an at-rule body starting at byte
/// `base` of the file, for matching rule blocks.
fn collect_css_rules(css: &str, base: usize, class: &str, out: &mut Vec<(usize, String)>) {
    let b = css.as_bytes();
    let mut i = 0usize;
    let mut sel_start = 0usize;
    while i < b.len() {
        // A comment after selector text keeps it (`.plang /* x */ {`); a
        // comment standing alone resets the scan, so its words never leak.
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            let started = !css[sel_start..i].trim().is_empty();
            match css[i + 2..].find("*/") {
                Some(rel) => i += 2 + rel + 2,
                None => return,
            }
            if !started {
                sel_start = i;
            }
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

/// Whether `selector` has `.class` not followed by a class-name character:
/// `.plang` matches `.plang:hover`, not `.plang-x`.
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

    /// An anonymous enum binding is hinted with hover's spelling, which keeps
    /// the payloads `Type`'s `Display` drops.
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
            annotated: false,
            line: 1,
            col: 5,
            end_col: 6,
            fn_line: 1,
        };
        let label = type_hint_label(&b, &Default::default(), "let e = pick()", 6)
            .expect("the call hides the type");
        assert_eq!(label, "{ A(Int64) | B }", "the arms, as hover writes them");
        assert_eq!(
            label,
            vyrn_frontend::type_to_string(&ty, &Default::default()),
            "one renderer"
        );
        assert_ne!(label, ty.to_string(), "`Display` drops the payloads");
    }

    /// The annotation is the parser's, not the line's: a binding the AST
    /// marks annotated earns no hint, whatever its line spells.
    #[test]
    fn an_annotated_binding_earns_no_hint() {
        let b = vyrn_frontend::LocalBinding {
            name: "n".to_string(),
            kind: LocalKind::Let { mutable: false },
            ty: Some(Type::Int),
            annotated: true,
            line: 1,
            col: 5,
            end_col: 6,
            fn_line: 1,
        };
        assert_eq!(
            type_hint_label(&b, &Default::default(), "let n = pick()", 6),
            None
        );
    }

    /// The emoji is 1 char and 2 UTF-16 units, so every column past it differs
    /// by one.
    const EMOJI_LINE: &str = "show(\"🎉 done\", here)";

    #[test]
    fn utf16_and_char_columns_round_trip_through_an_astral_char() {
        // The emoji sits at char 6, units 6..8.
        for (char_col, units) in [(0usize, 0u32), (5, 5), (6, 6), (7, 8), (15, 16), (20, 21)] {
            assert_eq!(char_col_to_utf16(EMOJI_LINE, char_col), units);
            assert_eq!(utf16_to_char_col(EMOJI_LINE, units), char_col);
        }
        // A unit INSIDE the surrogate pair lands on the next char boundary.
        assert_eq!(utf16_to_char_col(EMOJI_LINE, 7), 7);
        // Past the line end clamps to the line length.
        assert_eq!(
            utf16_to_char_col(EMOJI_LINE, 999),
            EMOJI_LINE.chars().count()
        );
    }

    #[test]
    fn an_inbound_utf16_position_lands_on_the_right_char() {
        // The client sends UTF-16 offset 16 for `here`'s `h`: char col 15,
        // which the frontend reads as 1-based col 16.
        let pos = Position {
            line: 0,
            character: 16,
        };
        assert_eq!(to_frontend(EMOJI_LINE, &pos), (1, 16));
    }

    #[test]
    fn an_outbound_range_leaves_in_utf16_units() {
        // The string literal spans chars 9..11, units 9..12.
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
        // The last line is 9 chars, 10 UTF-16 units.
        assert_eq!(r.end.character, 10);
    }

    #[test]
    fn semantic_token_lengths_are_utf16_units() {
        // One token of 4 chars, 5 units.
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
        let cafedot = String::from("café.");
        assert!(is_member_context(Some(&cafedot), 1, 6));
        let partial = String::from("café.pu");
        assert!(is_member_context(Some(&partial), 1, 8));
        let noreceiver = String::from("café");
        assert!(!is_member_context(Some(&noreceiver), 1, 5));
        let ascii = String::from("arr.pu");
        assert!(is_member_context(Some(&ascii), 1, 7));
        let plain = String::from("let x = 1");
        assert!(!is_member_context(Some(&plain), 1, 10));
    }

    #[test]
    fn a_selector_before_an_inline_comment_still_matches_its_class() {
        let css = ".plang /* legacy */ { color: red }\n.other { color: blue }\n";
        let rules = css_rules_for_class(css, "plang");
        assert_eq!(rules.len(), 1, "{rules:?}");
        assert_eq!(rules[0].0, 1);
        assert!(rules[0].1.contains("color: red"), "{}", rules[0].1);
        // A comment standing alone must not leak into the next selector.
        let spaced = ".a { x: 1 }\n/* .plang mention */\n.b { y: 2 }\n";
        assert!(css_rules_for_class(spaced, "plang").is_empty());
    }
}
