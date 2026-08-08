//! Module loader/linker (RFC-0010).
//!
//! Sits IN FRONT of the existing pipeline: each file is lexed/parsed with the
//! ordinary parser, imports are resolved recursively, and everything is linked
//! into **one** [`Program`] — so the checker, interpreter, code generator,
//! monomorphization, and the three-way parity harness are completely unaware
//! that modules exist.
//!
//! I/O lives behind [`ModuleResolver`]: the CLI provides a filesystem (and,
//! in later milestones, a network/cache) implementation; tests use in-memory
//! maps; the frontend itself never touches the filesystem or network.
//!
//! Rules enforced here:
//!   * a specifier resolves relative to the importing file (`./`, `../`),
//!     or against the std root for `std/...`; `.vyrn` is appended when the
//!     specifier has no extension;
//!   * import cycles are errors (named in full);
//!   * an imported name must exist in the target module and be `export`ed;
//!   * top-level names must be unique across the whole program (a collision
//!     names both files);
//!   * a module may only reference foreign names it imported (visibility) —
//!     including enum variants (importing the enum brings its constructors)
//!     and protocol methods (importing the protocol brings its methods);
//!   * only the root module may carry a `logging { .. }` config block;
//!   * `impl` blocks travel with their module and apply program-wide
//!     (coherence: duplicate `(protocol, type)` impls are a link error).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::{lexer, parser};

/// Provides module source text for a **resolved** specifier (a normalized,
/// slash-separated path — see [`resolve_spec`]). Implementations: the CLI's
/// filesystem resolver, in-memory maps in tests, cache/network in M4.
pub trait ModuleResolver {
    fn read(&self, resolved: &str) -> Result<String, String>;
    /// List the entry names directly under the directory `resolved` (no `.`/`..`;
    /// bare names, not paths). Default: unsupported. Filesystem-backed resolvers
    /// override it; the in-memory [`MapResolver`] scans its keys. Used by
    /// generation-time `listDir` (RFC-0021); ordinary module loading never calls
    /// it.
    fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
        Err(format!("cannot list `{resolved}`"))
    }
    /// Fetch a cached generator output by content-address key (RFC-0021). The
    /// frontend stays filesystem-free: the CLI/LSP back this with
    /// `~/.vyrn/cache/gen`; tests use an in-memory map. Default: no cache (a
    /// permanent miss), so generation always re-runs.
    fn gen_cache_get(&self, _key: &str) -> Option<String> {
        None
    }
    /// Store a generator output under its content-address key (RFC-0021). Default:
    /// a no-op (no cache). Failures are swallowed — the cache is an optimization,
    /// never a correctness dependency.
    fn gen_cache_put(&self, _key: &str, _value: &str) {}
}

thread_local! {
    /// Count of generator bodies actually *run* (cache misses) on this thread.
    /// Thread-local so the parallel test runner sees each test's own count (a
    /// generation runs inline on the calling thread). Test-observable evidence
    /// that the cache short-circuits re-runs (RFC-0021).
    static GEN_RUNS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn bump_gen_runs() {
    GEN_RUNS.with(|c| c.set(c.get() + 1));
}

/// The number of generator runs so far on this thread (cache misses).
pub fn gen_run_count() -> u64 {
    GEN_RUNS.with(|c| c.get())
}

thread_local! {
    /// Test-only guardrail overrides (thread-local ⇒ no parallel-test
    /// interference). `None` ⇒ the production defaults.
    static GEN_FUEL_OVERRIDE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    static GEN_MAX_OUTPUT_OVERRIDE: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// A resolver over an in-memory map — used by tests and always available.
pub struct MapResolver(pub HashMap<String, String>);

impl ModuleResolver for MapResolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        self.0
            .get(resolved)
            .cloned()
            .ok_or_else(|| format!("module not found: {resolved}"))
    }
    fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
        // Every key directly under `resolved/` contributes its next path segment.
        let prefix = format!("{}/", resolved.trim_end_matches('/'));
        let mut names: std::collections::BTreeSet<String> = Default::default();
        let mut any_under = false;
        for key in self.0.keys() {
            if let Some(rest) = key.strip_prefix(&prefix) {
                any_under = true;
                if let Some(seg) = rest.split('/').next() {
                    if !seg.is_empty() {
                        names.insert(seg.to_string());
                    }
                }
            }
        }
        if !any_under {
            return Err(format!("cannot list `{resolved}`"));
        }
        Ok(names.into_iter().collect())
    }
}

/// A resolver that forwards every call to an inner resolver while recording each
/// successful `read` as `(resolved key, content)`. Used by `moduleInterface`
/// (RFC-0031): reflecting a module's reachable type closure links its imports, so
/// every module the link reads — not just the reflected file — must join the
/// generator's recorded cache inputs. `list`/`gen_cache_*` pass straight through.
pub struct RecordingResolver<'a> {
    inner: &'a dyn ModuleResolver,
    reads: std::cell::RefCell<Vec<(String, String)>>,
}

impl<'a> RecordingResolver<'a> {
    pub fn new(inner: &'a dyn ModuleResolver) -> Self {
        Self {
            inner,
            reads: std::cell::RefCell::new(Vec::new()),
        }
    }
    /// The recorded reads, in first-read order, deduplicated by path.
    pub fn into_reads(self) -> Vec<(String, String)> {
        let mut seen = HashSet::new();
        self.reads
            .into_inner()
            .into_iter()
            .filter(|(p, _)| seen.insert(p.clone()))
            .collect()
    }
}

impl ModuleResolver for RecordingResolver<'_> {
    fn read(&self, resolved: &str) -> Result<String, String> {
        let r = self.inner.read(resolved);
        if let Ok(s) = &r {
            self.reads
                .borrow_mut()
                .push((resolved.to_string(), s.clone()));
        }
        r
    }
    fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
        self.inner.list(resolved)
    }
    fn gen_cache_get(&self, key: &str) -> Option<String> {
        self.inner.gen_cache_get(key)
    }
    fn gen_cache_put(&self, key: &str, value: &str) {
        self.inner.gen_cache_put(key, value)
    }
}

/// Lexically normalize a slash-separated path: resolve `.` and `..`, collapse
/// duplicate separators. Purely textual (works for in-memory resolvers too).
pub(crate) fn normalize(path: &str) -> String {
    let slashed = path.replace('\\', "/");
    let mut out: Vec<&str> = Vec::new();
    for seg in slashed.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|s| *s != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    // Preserve absolute paths / drive letters ("N:/..", "/..").
    if path.starts_with('/') && !joined.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

/// The import specifier a module reached at resolved key `key` should be written
/// as from a file whose directory is `importer_dir` (RFC-0031). A `std/` module
/// keeps its `std/`-rooted specifier, a remote module its remote key; a local
/// module becomes a relative path (`./name`, `../name`) with the `.vyrn`
/// extension dropped. The inverse of [`resolve_spec`] for the common cases a
/// closure type's declaring module can take.
pub fn import_specifier(importer_dir: &str, key: &str, std_root: Option<&str>) -> String {
    let strip = |s: &str| s.strip_suffix(".vyrn").unwrap_or(s).to_string();
    if is_remote(key) {
        return strip(key);
    }
    if let Some(root) = std_root {
        let root = normalize(root);
        if let Some(rest) = normalize(key).strip_prefix(&format!("{root}/")) {
            return format!("std/{}", strip(rest));
        }
    }
    let keyn = normalize(key);
    let from: Vec<String> = if importer_dir.is_empty() {
        Vec::new()
    } else {
        normalize(importer_dir)
            .split('/')
            .map(str::to_string)
            .collect()
    };
    let to: Vec<String> = keyn.split('/').map(str::to_string).collect();
    let mut i = 0;
    while i < from.len() && i < to.len() && from[i] == to[i] {
        i += 1;
    }
    let mut segs: Vec<String> = Vec::new();
    for _ in i..from.len() {
        segs.push("..".to_string());
    }
    for s in &to[i..] {
        segs.push(s.clone());
    }
    let joined = strip(&segs.join("/"));
    if segs.first().map(|s| s == "..").unwrap_or(false) {
        joined
    } else {
        format!("./{joined}")
    }
}

/// The file name a `panic` in this module reports (census U5).
///
/// **Not the module key.** A key is a resolved path, so it is absolute whenever
/// the root was given as one — which the parity harness always does, and which a
/// shipped wasm module would then carry the build machine's directory layout in.
/// The name here is derived instead: the root module by its own base name, every
/// other module by the specifier an import would spell it with, plus `.vyrn`.
/// `std/slots.vyrn`, `sub/lib.vyrn`, `../shared/lib.vyrn`. It depends on the
/// project's shape and on nothing outside it, so two machines building one
/// program bake the same bytes.
fn site_file(key: &str, root_key: &str, std_root: Option<&str>) -> String {
    if key == root_key {
        return key.rsplit('/').next().unwrap_or(key).to_string();
    }
    // A generated module (RFC-0021) has no path — its key is a banner ending in
    // the importer's resolved path, which is absolute for the same reason. Name
    // the generator and the file it was synthesized for, each stably.
    if let Some(importer) = generated_importer(key) {
        let head = key.strip_suffix(importer).unwrap_or(key).trim_end();
        return format!("{head} {}", site_file(importer, root_key, std_root));
    }
    let spec = import_specifier(dir_of(root_key), key, std_root);
    format!("{}.vyrn", spec.strip_prefix("./").unwrap_or(&spec))
}

/// Rewrite every `panic(msg)` in `program` to [`PANIC_AT`]`(msg, "file:line")`.
///
/// One walk over every body a module can hold — [`crate::project::walk_program`],
/// which is where the list of those bodies lives.
fn stamp_panic_sites(program: &mut Program, file: &str) {
    let mut stamp = |e: &mut Expr| {
        if let Expr::Call { name, args, line } = e {
            if name == "panic" && args.len() == 1 {
                *name = PANIC_AT.to_string();
                args.push(Expr::Str(format!("{file}:{line}")));
            }
        }
    };
    crate::project::walk_program(program, &mut stamp);
}

/// The directory part of a resolved module path ("" when it has none).
fn dir_of(resolved: &str) -> &str {
    match resolved.rfind('/') {
        Some(i) => &resolved[..i],
        None => "",
    }
}

/// If `key` is a generated module's banner (`generated by <fn>(<args>) at
/// <importer>`, RFC-0021), the real importer file it was synthesized for;
/// otherwise `None`. A generated module has no path of its own, so its
/// relative/bare imports — and its visibility into the surrounding program —
/// resolve against this real importer, not the banner text.
///
/// Public since RFC-0072: a generated module's AUDIENCE is the audience of the
/// file it was synthesized for, so [`crate::audience`] asks the same question.
pub fn generated_importer(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("generated by ")?;
    let idx = rest.rfind(" at ")?;
    Some(&rest[idx + 4..])
}

/// Whether a specifier/key is remote (`github:`, `gist:`, `https:`).
pub fn is_remote(spec: &str) -> bool {
    spec.starts_with("github:") || spec.starts_with("gist:") || spec.starts_with("https://")
}

/// The immutable base of a remote key (`github:o/r@ref`, `gist:u/id[@rev]`,
/// or `https://host`). Relative imports inside a remote module must stay
/// under it — a remote file can never read your disk or climb out of its
/// pinned tree.
fn remote_base(key: &str) -> Option<String> {
    if let Some(rest) = key.strip_prefix("github:") {
        let at = rest.find('@')?;
        let slash = rest[at + 1..].find('/')?;
        return Some(format!("github:{}", &rest[..at + 1 + slash]));
    }
    if let Some(rest) = key.strip_prefix("gist:") {
        // gist:user/id[@rev]/file — base is user/id[@rev].
        let mut segs = rest.splitn(3, '/');
        let user = segs.next()?;
        let id = segs.next()?;
        return Some(format!("gist:{user}/{id}"));
    }
    if let Some(rest) = key.strip_prefix("https://") {
        let host = rest.split('/').next()?;
        return Some(format!("https://{host}"));
    }
    None
}

/// Normalize the path part of a remote key (the scheme/anchor is left alone).
fn normalize_remote(key: &str) -> String {
    let Some(base) = remote_base(key) else {
        return key.to_string();
    };
    let rest = &key[base.len()..];
    let rest = rest.trim_start_matches('/');
    format!("{base}/{}", normalize(rest))
}

/// RFC-0062: the two std modules whose ONLY job is to name the ambient builtins
/// (`Ok`/`Err`/`Result`, `Some`/`None`/`Option`). Importing one is a validated
/// no-op — the loader recognizes the specifier before any file resolution,
/// checks the imported names against this fixed export list, and binds nothing
/// (the names keep resolving to the builtins they already were). Returns the
/// module's fixed export list, or `None` for any other specifier. Public so the
/// editor can offer completion/hover for these names.
pub fn builtin_alias_exports(spec: &str) -> Option<&'static [&'static str]> {
    match spec {
        "std/result" => Some(&["Result", "Ok", "Err"]),
        "std/option" => Some(&["Option", "Some", "None"]),
        _ => None,
    }
}

/// Resolve an import specifier written inside `importer` to a module key.
///
/// Public so the editor can reuse the loader's exact resolution for
/// go-to-definition on an import path string (RFC-0050 §2) — no second,
/// drifting copy of the path logic. The returned key is a local slash path for
/// relative / `std/` specifiers (with `.vyrn` appended when extension-less) or a
/// remote key (`github:` / `gist:` / `https://`) the editor treats as
/// un-jumpable. Read-only: it touches no filesystem.
pub fn resolve_spec(spec: &str, importer: &str, opts: &LoadOptions) -> Result<String, String> {
    // A generated module (RFC-0021) has no path of its own — its imports resolve
    // against the real file that triggered generation, encoded in its banner key.
    let importer = generated_importer(importer).unwrap_or(importer);
    let with_ext = |p: String| {
        if p.ends_with(".vyrn") || p.ends_with(".json") {
            p
        } else {
            format!("{p}.vyrn")
        }
    };
    if let Some(rest) = spec.strip_prefix("std/") {
        let root = opts
            .std_root
            .as_deref()
            .ok_or_else(|| "std library not available (no std root configured)".to_string())?;
        return Ok(normalize(&with_ext(format!("{root}/{rest}"))));
    }
    if spec.starts_with("http://") {
        return Err(format!("insecure `http:` import `{spec}` — use https"));
    }
    // Remote specifiers are their own keys; the resolver (vyrn-cli) turns them
    // into content via the lockfile/cache/network.
    if is_remote(spec) {
        let key = normalize_remote(&with_ext(spec.to_string()));
        remote_base(&key).ok_or_else(|| format!("malformed remote specifier `{spec}`"))?;
        return Ok(key);
    }
    if spec.starts_with("./") || spec.starts_with("../") {
        // Inside a remote module, relative imports stay within the pinned
        // base — never onto the local disk, never above the anchor.
        if let Some(base) = remote_base(importer) {
            let dir = dir_of(importer);
            let key = normalize_remote(&with_ext(format!("{dir}/{spec}")));
            let escaped = !key.starts_with(&format!("{base}/"))
                || key[base.len()..].split('/').any(|seg| seg == "..");
            if escaped {
                return Err(format!(
                    "`{spec}` escapes its remote module's base `{base}`"
                ));
            }
            return Ok(key);
        }
        let base = dir_of(importer);
        let joined = if base.is_empty() {
            spec.to_string()
        } else {
            format!("{base}/{spec}")
        };
        return Ok(normalize(&with_ext(joined)));
    }
    // A bare specifier resolves through the manifest's dependency map; the
    // mapped target is itself a specifier, rooted at the manifest's directory.
    // Remote modules have no manifest — their bare specifiers are errors.
    if remote_base(importer).is_none() {
        if let Some(target) = opts.aliases.get(spec) {
            if target.starts_with("./") || target.starts_with("../") {
                let joined = if opts.alias_base.is_empty() {
                    target.clone()
                } else {
                    format!("{}/{target}", opts.alias_base)
                };
                return Ok(normalize(&with_ext(joined)));
            }
            if target.starts_with("std/") || is_remote(target) {
                return resolve_spec(target, importer, opts);
            }
            return Err(format!(
                "manifest maps `{spec}` to `{target}`, which is not a supported specifier"
            ));
        }
    }
    Err(format!(
        "cannot resolve import `{spec}`: use a relative path (`./name`), `std/name`, \
         a remote specifier (github:/gist:/https:), or declare it in vyrn.json's \
         `dependencies`"
    ))
}

/// Options for a load: the std root plus the project manifest's dependency
/// aliases (RFC-0010 M3). `aliases` maps bare specifiers (`"pad"`) to real
/// specifiers; relative mapped values resolve against `alias_base` (the
/// manifest's directory), NOT the importing file.
#[derive(Default)]
pub struct LoadOptions {
    pub std_root: Option<String>,
    pub aliases: std::collections::HashMap<String, String>,
    /// Directory the manifest lives in (slash-separated); base for relative
    /// alias targets. Empty = current directory.
    pub alias_base: String,
    /// RFC-0072 M1: the project's declared audience vocabulary, or `None` when
    /// `vyrn.json` has no `audience` key. `None` disables the whole mechanism —
    /// every module universal, no import rejected — which is what makes this
    /// opt-in per project and unable to break anything that compiles today.
    pub audience: Option<crate::audience::AudienceMap>,
}

/// RFC-0072 M1: the objection, if any, to `importer` importing `imported`.
///
/// The rule is a property of two paths and a declared vocabulary — no file
/// content, no parse, no order dependence — so it is one function and the loader
/// calls it once per edge. `None` whenever the project declared no `audience`
/// key, which is every project that has not opted in.
///
/// The message names BOTH files, because the objection is about the edge and
/// naming one end of it would leave the reader hunting for the other. The note
/// cites the `vyrn.json` key that decided it, so the answer to "says who?" is in
/// the diagnostic rather than in the documentation.
fn audience_objection(
    importer: &str,
    imported: &str,
    line: usize,
    opts: &LoadOptions,
) -> Option<Diagnostic> {
    use crate::audience;
    let map = opts.audience.as_ref()?;
    let from = audience::audience_of(importer, map);
    let to = audience::audience_of(imported, map);
    if !audience::widens(from.audience, to.audience) {
        return None;
    }
    let mut d = Diagnostic::error(
        line,
        0,
        "audience",
        format!(
            "`{}` is {} and cannot import `{}`, which is {}",
            audience::display_path(importer, map),
            from.audience.phrase(),
            audience::display_path(imported, map),
            to.audience.phrase()
        ),
    );
    d.note = Some(format!(
        "audience `{}` is declared by vyrn.json:{} — {}; the importer's own audience comes from {}",
        to.audience,
        to.audience.key(),
        audience::remedy(to.audience),
        from.because()
    ));
    Some(d)
}

/// One parsed module awaiting linking.
struct Module {
    key: String,
    program: Program,
    /// The resolved key each import points at, in `program.imports` order.
    import_targets: Vec<String>,
    /// The synthesized source text, for a module produced by a generator
    /// (RFC-0021); `None` for a module read from disk. Powers `vyrn emit-gen`.
    gen_source: Option<String>,
    /// RFC-0078 M2b: this module entered the load by INJECTION — nothing imported
    /// it; a builtin's desugar or its routed call needs it. `Some(prefix)` is the
    /// reserved spelling its declarations are renamed to (see [`RT_PREFIX`]), so
    /// they can neither collide with a user's names nor be captured by them.
    injected: Option<&'static str>,
}

/// The prefix every declaration of an INJECTED runtime module is renamed to
/// (RFC-0078 M2b). `$` is not an identifier character in Vyrn — the lexer takes
/// `is_alphanumeric() || '_'` — so no source can spell one of these names. That
/// is the whole defence: a builtin desugar that calls `json$emit` cannot be
/// captured by a program's own `fn emit`, and `link`'s program-wide uniqueness
/// check cannot report a collision against a module the user never imported.
pub const RT_PREFIX: &str = "json$";

/// The module the `toJson` desugar links (RFC-0078 M2b): `std/json`'s value tree
/// and its writer, which after M2a's split imports nothing.
pub const RT_JSON_SPEC: &str = "std/json";

/// One Vyrn module a builtin's implementation lives in, and the reserved prefix
/// its declarations are renamed to.
///
/// RFC-0078 M2b injected exactly one (`std/json`, for `toJson`) and M4c made the
/// mechanism a table rather than a second copy of itself. Adding a builtin to the
/// runtime is now an ENTRY here plus a deletion in each engine.
pub struct RtModule {
    /// The import spec, resolved against the std root like any other.
    pub spec: &'static str,
    /// The prefix every declaration of the module is renamed to. `$` is not an
    /// identifier character, which is the whole defence — see [`RT_PREFIX`].
    pub prefix: &'static str,
    /// Builtins whose mention links the module but whose lowering is still a
    /// compiler part: `toJson` needs the static type of its argument, so each
    /// engine builds an AST through `jsonenc` and only the SERIALIZER is here.
    pub desugared: &'static [&'static str],
    /// Builtins that ARE one of the module's exported functions (RFC-0078 M4c):
    /// `(builtin, the RESERVED spelling of the exported Vyrn function)`. Nothing
    /// type-directed is involved — the argument types are fixed — so each engine's
    /// whole implementation is a call to the second name.
    ///
    /// The prefix is written out rather than composed, so [`routed_builtin`] is a
    /// scan returning a `&'static str` with no allocation on a path every call
    /// expression takes. `every_route_is_spelled_with_its_modules_prefix` is what
    /// keeps the redundancy honest.
    pub routes: &'static [(&'static str, &'static str)],
}

/// Every runtime module, in load order.
pub const RT_MODULES: &[RtModule] = &[
    // `fromJson` links this one too, and for the same reason `toJson` does: the
    // `Json` tree its decoders walk is declared here. It is listed as desugared
    // rather than routed because both builtins need the argument's static type,
    // which is a compiler part no table entry can express.
    RtModule {
        spec: RT_JSON_SPEC,
        prefix: RT_PREFIX,
        desugared: &["toJson", "fromJson"],
        routes: &[],
    },
    // RFC-0078 M3: `fromJson`'s untyped half — the reader (via `std/jsonread`),
    // the RFC-0018 Issue vocabulary, the path arithmetic and the scalar decoders.
    // The typed half is generated per target type by `jsondec` and calls in here,
    // so the 32 `__vyrn_vj_*` C functions and the interpreter's own walk are gone
    // rather than duplicated.
    RtModule {
        spec: "std/jsondec",
        prefix: "jsondec$",
        desugared: &["fromJson"],
        routes: &[],
    },
    // RFC-0078 M4b(1)/M4c: the six codecs. They had no C shim at all — ~520 lines
    // of hand-written LLVM IR in the textual emitter and 159 lines of Rust in the
    // interpreter — and needed no primitive to write in Vyrn.
    RtModule {
        spec: "std/codecs",
        prefix: "codecs$",
        desugared: &[],
        routes: &[
            ("hexEncode", "codecs$hexEncodeV"),
            ("hexDecode", "codecs$hexDecodeV"),
            ("base64Encode", "codecs$base64EncodeV"),
            ("base64Decode", "codecs$base64DecodeV"),
            ("urlEncode", "codecs$urlEncodeV"),
            ("urlDecode", "codecs$urlDecodeV"),
        ],
    },
    // RFC-0078 M4b(2)/M4c: `chars`, and `@charCount` — the census's one builtin with
    // no justification for being one, added on an existing row rather than a new
    // one. It is spelled with the `@` because that is what the parser produces:
    // `s.charCount()` is method-only, so the AST call name is `@charCount` and that
    // is the string every engine looks up. `lineAt`/`colAt` are deliberately NOT
    // here — see the M4c note in RFC-0078: the interpreter memoizes a line-start
    // table that a Vyrn library cannot, and retiring them is a decision about that
    // cache (M5) rather than about capability.
    RtModule {
        spec: "std/text",
        prefix: "text$",
        desugared: &[],
        routes: &[("chars", "text$charsV"), ("@charCount", "text$charCountV")],
    },
    // RFC-0078 M4b(3)/M4c: the three string predicates — and `slice`, which M4c
    // refused and RFC-0079 M3 took. The refusal was "it TRAPS, and Vyrn has no
    // expression that aborts, so `sliceV` can only answer `None` where the builtin
    // ends the process". M3 did not add the abort to `slice`; it made the failure a
    // VALUE (`Result<String, SliceError>`) and let the caller choose, which deleted
    // an interpreter arm, ~50 lines of emitted IR and a wasm runtime function
    // instead of adding a fourth copy of the range check.
    //
    // `byteLength` is still not here: it is a VIEW (`strlen`), it folds at compile
    // time inside refinement predicates, and the byte view is what this module is
    // built on.
    RtModule {
        spec: "std/strpred",
        prefix: "strpred$",
        desugared: &[],
        routes: &[
            ("contains", "strpred$containsV"),
            ("startsWith", "strpred$startsWithV"),
            ("endsWith", "strpred$endsWithV"),
            ("slice", "strpred$sliceV"),
        ],
    },
    // RFC-0081 M2: the six decimal places. Listed as DESUGARED rather than routed
    // for the same reason `toJson` is — `@str` is type-directed and only its float
    // case is a call; an `Int64` still renders with `%lld` and a `Bool` with a
    // `select`, neither of which a `(builtin, function)` row can express. `print`
    // is here because it formats a float without going through `@str`, and a
    // program that says `print(1.5)` and never interpolates would otherwise reach
    // a formatter that is not in its link.
    //
    // Which means nearly every program in the repo now links this module, and the
    // measured price of that is the reason it is affordable: the direct backend
    // sweeps unreached functions (`Module::sweep`), so a program that formats no
    // float carries none of it, and a `vyrn check` pays ~2 ms for the parse the
    // loader memoizes anyway.
    RtModule {
        spec: "std/num",
        prefix: "num$",
        desugared: &["@str", "print"],
        routes: &[],
    },
];

/// The reserved spelling of `std/num`'s float formatter — the ONE float formatter
/// the two compiled backends have (RFC-0081 M2), reached from `@str` and from
/// `print`. Spelled here rather than in each backend for the reason a route is:
/// the prefix and the name have to agree with the table above, and
/// `the_float_formatter_is_std_nums` is what keeps that honest.
///
/// Not a `routes` row, because a route is a whole-builtin rename and this is one
/// CASE of one: `@str` on an `Int64` still renders with `%lld`, on a `Bool` with a
/// `select`, and only a float becomes a call. That is the same reason `toJson` is
/// a desugar (see [`RtModule::desugared`]).
pub const F64_STR: &str = "num$f64Str";

/// The reserved spelling a routed builtin's call becomes, or `None` for a name no
/// runtime module implements.
///
/// The one function every engine calls, and the reason M4c is a rename rather
/// than three lowerings: the interpreter, the textual emitter and the direct wasm
/// backend each replace their implementation of `hexEncode` with
/// `self.call(routed_builtin("hexEncode")?)`.
pub fn routed_builtin(name: &str) -> Option<&'static str> {
    RT_MODULES
        .iter()
        .flat_map(|rt| rt.routes)
        .find(|(builtin, _)| *builtin == name)
        .map(|(_, reserved)| *reserved)
}

/// The reserved spelling of an injected runtime declaration (`std/json`'s, the
/// only prefix the `toJson` desugar spells).
pub fn rt_name(name: &str) -> String {
    format!("{RT_PREFIX}{name}")
}

/// The synthesized source of every generator-produced module reachable from the
/// root (RFC-0021), as `(banner, source)` pairs in load order — the data behind
/// `vyrn emit-gen`. Runs the whole load (generators fire, cache included) but
/// discards the link.
pub fn generated_modules(
    root_source: &str,
    root_path: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Result<Vec<(String, String)>, Vec<Diagnostic>> {
    let (modules, _, _, _) =
        load_modules(root_source, root_path, opts, resolver).map_err(|(d, _)| d)?;
    Ok(modules
        .into_iter()
        .filter_map(|m| m.gen_source.map(|s| (m.key, s)))
        .collect())
}

/// Load `root_source` (already read; its path is `root_path`) and every module
/// it transitively imports, then link them into one [`Program`].
///
/// On any problem, returns all diagnostics found so far — parse errors carry
/// the file they occurred in via [`Diagnostic::file`].
pub fn load(
    root_source: &str,
    root_path: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Result<Program, Vec<Diagnostic>> {
    load_with_origins(root_source, root_path, opts, resolver).0
}

/// The success-path warnings a load produced, in module-entry order (RFC-0071
/// M2b). Empty for a load that reached no generator emitting a deprecation.
pub type Warnings = Vec<Diagnostic>;

/// Like [`load`], but also returns the RFC-0033 origin maps built from every
/// synthesized generator module reachable from the root. The maps drive
/// diagnostic remapping (CLI + LSP) and the LSP's forward hover/completion/
/// go-to-definition inside generator input files; they are empty when no
/// reachable generator emitted `//@origin` directives.
///
/// RFC-0053: the maps are returned **whether or not the load succeeded** — they
/// are a line-scan over each synthesized module's text and need no successful
/// parse, so a `.vyx` whose template fails to lex still maps its generated lines
/// back to the file the user is editing. The returned diagnostics have already
/// been remapped through them.
/// RFC-0071 M2b: the third element is the load's WARNINGS — diagnostics that do
/// not fail it. They are returned on the error path too (empty, since a failed
/// load has nothing to advise about), so callers have one shape to destructure.
/// The fourth element is the module GRAPH this load already built — the same
/// `(key, import targets, synthesized source)` triples [`module_graph_with_sources`]
/// derives. It is returned rather than dropped because the symbol indexer needs
/// it to resolve `import * as ns`, and recomputing it there meant a SECOND
/// complete load — generators, cache lookups and all — on every keystroke.
pub fn load_with_origins(
    root_source: &str,
    root_path: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> (
    Result<Program, Vec<Diagnostic>>,
    crate::origin::OriginMaps,
    Warnings,
    ModuleGraph,
) {
    // A fresh epoch for the outermost load only — see `current_input_hash`.
    let depth = LOAD_DEPTH.with(|d| {
        d.set(d.get() + 1);
        d.get()
    });
    if depth == 1 {
        LOAD_EPOCH.with(|e| e.set(e.get().wrapping_add(1)));
        MODULE_HASHES.with(|m| m.borrow_mut().clear());
    }
    let out = load_with_origins_inner(root_source, root_path, opts, resolver);
    LOAD_DEPTH.with(|d| d.set(d.get() - 1));
    out
}

fn load_with_origins_inner(
    root_source: &str,
    root_path: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> (
    Result<Program, Vec<Diagnostic>>,
    crate::origin::OriginMaps,
    Warnings,
    ModuleGraph,
) {
    match load_modules(root_source, root_path, opts, resolver) {
        Err((diags, origins)) => (Err(diags), origins, Vec::new(), Vec::new()),
        Ok((modules, root_key, origins, warnings)) => {
            let graph = graph_of(&modules);
            (link(modules, &root_key), origins, warnings, graph)
        }
    }
}

/// `(module key, resolved import targets, synthesized source)` per loaded module.
pub type ModuleGraph = Vec<(String, Vec<String>, Option<String>)>;

/// `module key -> content hash` for the modules the last outermost load visited.
/// The checker uses it to reuse diagnostics for modules that did not change
/// (RFC-free: see `check_accum_reusing`). Valid until the next load begins.
pub fn last_module_hashes() -> HashMap<String, String> {
    MODULE_HASHES.with(|m| m.borrow().clone())
}

fn graph_of(modules: &[Module]) -> ModuleGraph {
    modules
        .iter()
        .map(|m| {
            (
                m.key.clone(),
                m.import_targets.clone(),
                m.gen_source.clone(),
            )
        })
        .collect()
}

/// The module dependency graph: every (module key, resolved import targets)
/// pair reachable from the root — powers `vyrn deps`.
pub fn module_graph(
    root_source: &str,
    root_path: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Result<Vec<(String, Vec<String>)>, Vec<Diagnostic>> {
    let (modules, _, _, _) =
        load_modules(root_source, root_path, opts, resolver).map_err(|(d, _)| d)?;
    Ok(modules
        .into_iter()
        .map(|m| (m.key, m.import_targets))
        .collect())
}

/// Like [`module_graph`], but each entry also carries the module's SYNTHESIZED
/// source when it was produced by a generator (RFC-0021) — `None` for a module
/// read from disk. RFC-0051 §2: the symbol indexer needs it to list the exports
/// of an `import * as ns from gen(..)` namespace, whose "file" is a banner key
/// no resolver can read.
#[allow(clippy::type_complexity)]
pub fn module_graph_with_sources(
    root_source: &str,
    root_path: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Result<Vec<(String, Vec<String>, Option<String>)>, Vec<Diagnostic>> {
    let (modules, _, _, _) =
        load_modules(root_source, root_path, opts, resolver).map_err(|(d, _)| d)?;
    Ok(modules
        .into_iter()
        .map(|m| (m.key, m.import_targets, m.gen_source))
        .collect())
}

/// Load every module reachable from the root, returning them with the root key
/// and the RFC-0033 origin maps.
///
/// RFC-0053: the maps are accumulated **as each synthesized module is entered**,
/// from its source text alone — a line-scan that needs no successful parse — so a
/// module that fails to lex or parse still has a map. On the error path the
/// diagnostics are routed through the very same [`crate::origin::OriginMaps::remap`]
/// the checker's use, so a stray character inside a `.vyx` template expression is
/// reported at that `.vyx` line:col instead of at a dead-end banner key. The
/// never-lose guarantee is unchanged: an error on a line with no governing
/// directive (generator glue) keeps its generated location plus the note.
#[allow(clippy::type_complexity)]
fn load_modules(
    root_source: &str,
    root_path: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Result<
    (
        Vec<Module>,
        String,
        crate::origin::OriginMaps,
        Vec<Diagnostic>,
    ),
    (Vec<Diagnostic>, crate::origin::OriginMaps),
> {
    let root_key = normalize(root_path);
    let mut modules: Vec<Module> = Vec::new();
    let mut states: HashMap<String, bool> = HashMap::new(); // false = loading
    let mut stack: Vec<String> = Vec::new();
    // Generator-import identity (RFC-0040 §1): a resolved-inputs key
    // (`name\0resolved-arg\0…`) mapped to the banner of the FIRST module
    // synthesized for it. Two imports whose path args RESOLVE identically —
    // however they are spelled (`./strings` vs `../strings` from a rebased `.vyx`
    // import) — reuse that one module: one instance, shared state, no collision.
    let mut identities: HashMap<String, String> = HashMap::new();
    let mut origins = crate::origin::OriginMaps::new();
    // RFC-0071 M2b: success-path warnings accumulated as modules are entered.
    // They travel BESIDE the program, never in place of it — a warning must not
    // change an exit code or a byte of program output.
    let mut warnings: Vec<Diagnostic> = Vec::new();

    #[allow(clippy::too_many_arguments)]
    fn visit(
        key: &str,
        source: Option<&str>,
        opts: &LoadOptions,
        resolver: &dyn ModuleResolver,
        modules: &mut Vec<Module>,
        states: &mut HashMap<String, bool>,
        identities: &mut HashMap<String, String>,
        origins: &mut crate::origin::OriginMaps,
        warnings: &mut Vec<Diagnostic>,
        stack: &mut Vec<String>,
        root_key: &str,
    ) -> Result<(), Vec<Diagnostic>> {
        match states.get(key) {
            Some(true) => return Ok(()), // already loaded
            Some(false) => {
                let cycle: Vec<&str> = stack.iter().map(|s| s.as_str()).collect();
                return Err(vec![Diagnostic::error(
                    0,
                    0,
                    "load",
                    format!("import cycle: {} -> {key}", cycle.join(" -> ")),
                )]);
            }
            None => {}
        }
        states.insert(key.to_string(), false);
        stack.push(key.to_string());

        let text = match source {
            Some(t) => t.to_string(),
            None => resolver.read(key).map_err(|e| {
                vec![Diagnostic::error(
                    0,
                    0,
                    "load",
                    format!("cannot load `{key}`: {e}"),
                )]
            })?,
        };
        let is_root = key == root_key;

        // RFC-0053: register a synthesized module's `//@origin` table NOW, before
        // it is lexed — the table is a pure line-scan over the text, so a module
        // that never lexes or parses still gets one, and its lex/parse errors can
        // be remapped onto the `.vyx` (or other input) the text came from.
        // `generated_importer` uses the LAST ` at `, so a nested generator's banner
        // still yields the real on-disk file in one step.
        if key.starts_with("generated by ") {
            let importer = generated_importer(key).unwrap_or(key);
            origins.add_module(key, &text, dir_of(importer));
            // RFC-0071 M2b: the same line-scan lifts `//@warning` directives
            // into success-path WARNINGS. A page is generated twice (server +
            // client bundle), and a generator may be re-entered, so the same
            // notice arrives more than once for one authored line — de-duplicate
            // on what the user sees (file, line, message), never on the banner.
            for d in crate::origin::warnings(key, &text, dir_of(importer)) {
                if !warnings.iter().any(|w: &Diagnostic| {
                    w.file == d.file && w.line == d.line && w.message == d.message
                }) {
                    warnings.push(d);
                }
            }
        }

        // A `.json` module is a JSON Schema document: synthesize validated
        // type declarations from it (RFC-0010 M2) instead of parsing Vyrn.
        // Schema modules import nothing themselves.
        if key.ends_with(".json") {
            let decls = crate::schema::synthesize(&text, None, key)
                .map_err(|e| vec![Diagnostic::error(0, 0, "load", e)])?;
            modules.push(Module {
                key: key.to_string(),
                program: Program {
                    imports: Vec::new(),
                    type_decls: decls,
                    functions: Vec::new(),
                    protocols: Vec::new(),
                    contracts: Vec::new(),
                    impls: Vec::new(),
                    globals: Vec::new(),
                    tests: Vec::new(),
                    benches: Vec::new(),
                    log_level: DEFAULT_LOG_LEVEL,
                    log_sink: LogSink::Stderr,
                },
                import_targets: Vec::new(),
                gen_source: None,
                injected: None,
            });
            stack.pop();
            states.insert(key.to_string(), true);
            return Ok(());
        }
        // Lex + parse, memoized on the module's TEXT.
        //
        // A keystroke changes one module, but the loader re-parsed every module
        // reachable from the root: 32 modules and 719 KB for examples/bin, all
        // but one of them byte-identical to the previous keystroke. The text is
        // the whole input to lexing and parsing, so its hash is the whole key.
        //
        // Cached BEFORE the per-module attribution below, which depends on `key`
        // and `is_root` rather than on the text, and so must still run — the same
        // source loaded under two keys yields two different modules from one
        // parse.
        //
        // Only successes are cached. A parse error's diagnostics are rewritten
        // with the module key on the way out, and a failed module is not worth
        // remembering.
        let mut program = {
            // Cheap, non-cryptographic: this key never leaves the process.
            let hash = {
                let mut h: u64 = 0xcbf29ce484222325;
                for b in text.as_bytes() {
                    h ^= *b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                format!("{h:x}:{}", text.len())
            };
            MODULE_HASHES.with(|m| m.borrow_mut().insert(key.to_string(), hash.clone()));
            if let Some(hit) = PARSE_CACHE.with(|c| c.borrow().get(&hash).cloned()) {
                hit
            } else {
                let tokens = lexer::lex(&text).map_err(|mut d| {
                    if !is_root {
                        d.file = Some(key.to_string());
                    }
                    vec![d]
                })?;
                let (parsed, errors) = parser::parse_accum(tokens);
                if !errors.is_empty() {
                    return Err(errors
                        .into_iter()
                        .map(|mut d| {
                            if !is_root {
                                d.file = Some(key.to_string());
                            }
                            d
                        })
                        .collect());
                }
                PARSE_CACHE.with(|c| {
                    let mut c = c.borrow_mut();
                    // ponytail: a keystroke replaces one entry and leaves the old
                    // text's behind. Bounded crudely so a long editing session
                    // cannot grow without limit; a proper LRU if it ever matters.
                    if c.len() > 512 {
                        c.clear();
                    }
                    c.insert(hash, parsed.clone());
                });
                parsed
            }
        };

        // Only the root configures logging (defaults are indistinguishable
        // from "unset", which is fine — they are the same behavior).
        if !is_root
            && (program.log_level != DEFAULT_LOG_LEVEL || program.log_sink != LogSink::Stderr)
        {
            return Err(vec![Diagnostic::error(
                0,
                0,
                "load",
                format!("`{key}`: only the root module may configure `logging {{ .. }}`"),
            )]);
        }

        // Module state is legal in ANY module (RFC-0029, lifting RFC-0013's
        // root-only rule and generalizing RFC-0021's synthesized-module
        // carve-out to the whole language). State stays module-private — one
        // instance per process, initialized in linker order (see the merge
        // below) — and is never exportable (`export let` is a parse error).

        // Attribute decls to this module (root stays `None` so single-file
        // diagnostics render exactly as before).
        if !is_root {
            for f in &mut program.functions {
                f.module = Some(key.to_string());
            }
            for t in &mut program.type_decls {
                t.module = Some(key.to_string());
            }
            for p in &mut program.protocols {
                p.module = Some(key.to_string());
            }
            // Contracts (RFC-0071) carry their module too: `ContractInfo.module`
            // is what lets a diagnostic say which library declared the contract
            // (`contract `Page`, std/ui`).
            for c in &mut program.contracts {
                c.module = Some(key.to_string());
            }
            // Module state (RFC-0029) carries its owning module too, for
            // diagnostics and the same-module initializer-call rule.
            for g in &mut program.globals {
                g.module = Some(key.to_string());
            }
            // Tag tests with their module too (RFC-0015): they still type-check,
            // but `vyrn test <root>` runs only the root's (`None`-module) tests.
            for t in &mut program.tests {
                t.module = Some(key.to_string());
            }
            // Tag benches with their module too (RFC-0055): they still type-check,
            // but `vyrn bench <root>` runs only the root's (`None`-module) benches.
            for b in &mut program.benches {
                b.module = Some(key.to_string());
            }
        }

        // Census U5: stamp every `panic` in this module with the file and line
        // it is written at, HERE, because this is the only pass that knows both.
        // The parser knows the line and not the file; every stage after this one
        // knows neither, because a `place` projection is cloned into its access
        // site and a monomorphized generic is cloned per instantiation.
        //
        // After the parse cache, deliberately: the cache is keyed by content
        // hash, so two files with identical text share one parse and would
        // otherwise share one file name.
        stamp_panic_sites(
            &mut program,
            &site_file(key, root_key, opts.std_root.as_deref()),
        );

        // RFC-0062: `std/result` / `std/option` are validated NO-OP imports —
        // their only job is to spell the ambient builtins (`Ok`/`Err`/`Result`,
        // `Some`/`None`/`Option`) as explicit imports. Recognize the specifier
        // BEFORE any file resolution (the `std/` root never holds real files for
        // these, so the builtins can never be shadowed or diverge), validate the
        // imported names against the fixed export list, reject `import * as`
        // (namespacing a builtin would create a second spelling — `r.Ok`), then
        // DROP the import so nothing is loaded or linked. The names stay the
        // builtins they already were; ambient use without the import is unaffected.
        let mut idx = 0;
        while idx < program.imports.len() {
            let hit = {
                let imp = &program.imports[idx];
                match &imp.source {
                    ImportSource::Path(spec) => builtin_alias_exports(spec).map(|exports| {
                        (
                            spec.clone(),
                            imp.namespace.is_some(),
                            imp.names.clone(),
                            imp.line,
                            exports,
                        )
                    }),
                    _ => None,
                }
            };
            let Some((spec, is_namespace, names, line, exports)) = hit else {
                idx += 1;
                continue;
            };
            let load_err = |msg: String| -> Vec<Diagnostic> {
                let mut d = Diagnostic::error(line, 0, "load", msg);
                if !is_root {
                    d.file = Some(key.to_string());
                }
                vec![d]
            };
            if is_namespace {
                return Err(load_err(format!(
                    "`{spec}` cannot be imported as a namespace (`import * as`) — its names \
                     are builtins; import them by name or use them directly"
                )));
            }
            for n in &names {
                if !exports.contains(&n.original.as_str()) {
                    return Err(load_err(format!("{spec} has no export `{}`", n.original)));
                }
            }
            program.imports.remove(idx);
        }

        // Resolve and load imports depth-first. `ImportSource::Path` resolves +
        // visits the target module here; `ImportSource::Generator` is handled in a
        // second pass (below), once every path-imported module — including the one
        // defining the generator — is loaded and available to run.
        let mut import_targets: Vec<Option<String>> = vec![None; program.imports.len()];
        for (i, imp) in program.imports.iter().enumerate() {
            if let ImportSource::Path(path) = &imp.source {
                let target = resolve_spec(path, key, opts).map_err(|e| {
                    let mut d = Diagnostic::error(imp.line, 0, "load", e);
                    if !is_root {
                        d.file = Some(key.to_string());
                    }
                    vec![d]
                })?;
                // RFC-0072 M1: an import may not WIDEN audience. Checked here,
                // before the target is visited, so the first illegal edge is the
                // one reported rather than whatever its subtree fails at.
                if let Some(mut d) = audience_objection(key, &target, imp.line, opts) {
                    if !is_root {
                        d.file = Some(key.to_string());
                    }
                    return Err(vec![d]);
                }
                visit(
                    &target, None, opts, resolver, modules, states, identities, origins, warnings,
                    stack, root_key,
                )?;
                import_targets[i] = Some(target);
            }
        }
        // Generator-call imports (RFC-0021): run each generator now that its
        // module is loaded, synthesize the module source, and visit it. Calls whose
        // path args RESOLVE identically dedup on the resolved-inputs identity
        // (RFC-0040 §1) — one module, shared state; an exact repeat dedups on
        // `gen_key` (already-loaded ⇒ no source, no re-run).
        for (i, imp) in program.imports.iter().enumerate() {
            if let ImportSource::Generator { name, args, line } = &imp.source {
                let (gen_key, gen_source) = run_generator(
                    key, is_root, name, args, *line, opts, resolver, modules, states, identities,
                    root_key,
                )?;
                if let Some(src) = gen_source {
                    visit(
                        &gen_key,
                        Some(&src),
                        opts,
                        resolver,
                        modules,
                        states,
                        identities,
                        origins,
                        warnings,
                        stack,
                        root_key,
                    )?;
                }
                import_targets[i] = Some(gen_key);
            }
        }
        let import_targets: Vec<String> = import_targets
            .into_iter()
            .map(|t| t.expect("every import resolved"))
            .collect();

        stack.pop();
        states.insert(key.to_string(), true);
        // A module synthesized by a generator (RFC-0021) keeps its source text
        // (its key is the generator banner) so `vyrn emit-gen` can print it.
        let gen_source = key.starts_with("generated by ").then(|| text.clone());
        modules.push(Module {
            key: key.to_string(),
            program,
            import_targets,
            gen_source,
            injected: None,
        });
        Ok(())
    }

    if let Err(mut diags) = visit(
        &root_key,
        Some(root_source),
        opts,
        resolver,
        &mut modules,
        &mut states,
        &mut identities,
        &mut origins,
        &mut warnings,
        &mut stack,
        &root_key,
    ) {
        // RFC-0053: lex/parse/load failures inside a synthesized module are
        // remapped onto their originating input file, exactly as check/movecheck
        // diagnostics are on the success path (`lib::load`). Ungoverned lines keep
        // their generated location.
        if !origins.is_empty() {
            for d in &mut diags {
                origins.remap(d);
            }
        }
        return Err((diags, origins));
    }

    // RFC-0078 M2b: the INJECTED imports. `toJson` compiles into a call to
    // `std/json`'s writer and `hexEncode` into a call to `std/codecs`, so a program
    // that mentions one of those builtins links the module even though no line of
    // it says so. The loader walks imports from the root, and this is the one thing
    // that enters that worklist without one.
    //
    // Conditional on the mention: injecting unconditionally would put every
    // runtime module's functions into every binary in the repo for builtins most
    // programs never touch. `program_ref_names` is the same scan `resolve_aliases`
    // uses, so "mentions `toJson`" means the same thing here as everywhere else —
    // module-scope `let` initializers are outside it, and outside what a global may
    // legally call anyway (no user calls, RFC-0013).
    //
    // M4c made this a loop over `RT_MODULES` rather than a second copy of itself,
    // which is the whole reason the codecs cost no new mechanism.
    let mentioned: HashSet<String> = modules
        .iter()
        .flat_map(|m| program_ref_names(&m.program))
        .collect();
    for rt in RT_MODULES {
        let wanted = rt
            .desugared
            .iter()
            .chain(rt.routes.iter().map(|(b, _)| b))
            .any(|b| mentioned.contains(*b));
        if !wanted {
            continue;
        }
        // A missing std root is not an error HERE: the diagnostic for a program
        // that needs the runtime and cannot find it belongs to whoever needs it,
        // not to a scan. Each engine refuses loudly at the call instead.
        let Ok(target) = resolve_spec(rt.spec, &root_key, opts) else {
            continue;
        };
        // The same rule one step further in, and RFC-0081 M2 is what made it
        // necessary: a spec can RESOLVE against a root that has no such file, and
        // `@str`/`print` mean nearly every program now reaches this loop. A
        // resolver serving a partial std tree (an in-memory one, an editor's) would
        // otherwise fail to load programs that never format a float. A module that
        // is present but broken still fails the load below — this skips only what
        // cannot be read at all.
        if !states.contains_key(&target) && resolver.read(&target).is_err() {
            continue;
        }
        if !states.contains_key(&target) {
            if let Err(mut diags) = visit(
                &target,
                None,
                opts,
                resolver,
                &mut modules,
                &mut states,
                &mut identities,
                &mut origins,
                &mut warnings,
                &mut stack,
                &root_key,
            ) {
                if !origins.is_empty() {
                    for d in &mut diags {
                        origins.remap(d);
                    }
                }
                return Err((diags, origins));
            }
        }
        // Set AFTER the visit, and whether or not this load performed it: the
        // program may ALSO import the module by hand, in which case it is already
        // there and the reserved spellings apply to it either way. (They are
        // transparent to a hand importer — `resolve_aliases` rewrites its
        // references along with everything else.)
        if let Some(m) = modules.iter_mut().find(|m| m.key == target) {
            m.injected = Some(rt.prefix);
        }
    }

    Ok((modules, root_key, origins, warnings))
}

/// Guardrails (RFC-0021): a generator's step budget and output-size cap.
const GEN_FUEL: u64 = 20_000_000;
const GEN_MAX_OUTPUT: usize = 4 * 1024 * 1024;

thread_local! {
    /// `module key -> content hash` for the load in progress. Handed to the
    /// checker so it can tell which modules are byte-identical to last time.
    static MODULE_HASHES: std::cell::RefCell<HashMap<String, String>> =
        std::cell::RefCell::new(HashMap::new());
}

thread_local! {
    /// Bumped once per outermost load; stamps [`HASH_MEMO`] entries.
    static LOAD_EPOCH: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Re-entrancy depth: generators load modules of their own.
    static LOAD_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// `path -> (epoch, hash)` for generator-cache validation.
    static HASH_MEMO: std::cell::RefCell<HashMap<String, (u64, Option<String>)>> =
        std::cell::RefCell::new(HashMap::new());
}

thread_local! {
    /// Parsed modules by content hash — see the memo in `visit`.
    static PARSE_CACHE: std::cell::RefCell<HashMap<String, Program>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Run a generator-call import target (RFC-0021) and return
/// `(synthesized module key, Some(source) | None-if-already-synthesized)`.
///
/// Flow: prove the arguments are compile-time constants → compute the
/// synthesized module key (which dedups identical calls and separates distinct
/// arguments) → find the exported `gen fn` in an already-loaded module → load +
/// check it as a runnable program → consult the content-addressed cache (a hit
/// skips interpretation) → on a miss, run the generator in the mediated sandbox,
/// then cache the result keyed by `sha256(generator sources ++ args ++ inputs)`.
#[allow(clippy::too_many_arguments)]
fn run_generator(
    importer: &str,
    importer_is_root: bool,
    name: &str,
    args: &[Expr],
    line: usize,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
    modules: &[Module],
    states: &HashMap<String, bool>,
    identities: &mut HashMap<String, String>,
    _root_key: &str,
) -> Result<(String, Option<String>), Vec<Diagnostic>> {
    let err = |msg: String| -> Vec<Diagnostic> {
        let mut d = Diagnostic::error(line, 0, "load", msg);
        if !importer_is_root {
            d.file = Some(importer.to_string());
        }
        vec![d]
    };

    // 1. Arguments must be compile-time constants (RFC-0021).
    let empty = HashMap::new();
    let mut consts = Vec::with_capacity(args.len());
    for a in args {
        match crate::consteval::eval(a, &empty) {
            Some(c) => consts.push(c),
            None => {
                return Err(err(format!(
                    "generator import `{name}(..)` needs compile-time-constant arguments (v1: \
                     string / integer / boolean literals)"
                )))
            }
        }
    }
    let arg_repr = consts
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(", ");

    // A generator imported BY a generated module (a nested generator, e.g. an
    // `i18n(..)` import inside a `.vyx` script that `components(..)` synthesized)
    // resolves its path arguments against the REAL importing file's directory, not
    // the synthetic banner key — as `resolve_spec` unwraps `generated_importer`.
    let path_importer = generated_importer(importer).unwrap_or(importer);
    let importer_dir = dir_of(path_importer).to_string();
    let join_dir = |s: &str| -> String {
        normalize(&if importer_dir.is_empty() {
            s.to_string()
        } else {
            format!("{importer_dir}/{s}")
        })
    };

    // 2. Identity (RFC-0040 §1): a generator import's module is keyed by its
    //    RESOLVED inputs — the generator name, each string path argument REBASED
    //    onto the importer's directory, and every non-string argument verbatim.
    //    Two imports whose paths resolve identically (however spelled — `./strings`
    //    from the root vs a rebased `./widgets/../strings` from a `.vyx` widget)
    //    share ONE synthesized module: one instance, shared state, no top-level
    //    collision. The banner below anchors that module's own relative imports and
    //    visibility at the FIRST importer to trigger it; the generated source is a
    //    pure function of these resolved inputs, so a later importer reusing it is
    //    byte-identical.
    let mut ident = format!("{name}\u{0}");
    for c in &consts {
        match c {
            crate::consteval::ConstVal::Str(s) => {
                ident.push_str(&join_dir(s));
            }
            other => ident.push_str(&other.to_string()),
        }
        ident.push('\u{0}');
    }
    if let Some(existing) = identities.get(&ident) {
        return Ok((existing.clone(), None));
    }

    // The synthesized module's key IS its diagnostic banner. It carries the raw
    // spelling + importer for readable `emit-gen` output; identity above governs
    // dedup. An exact repeat still short-circuits on the banner.
    let gen_key = format!("generated by {name}({arg_repr}) at {importer}");
    if states.contains_key(&gen_key) {
        return Ok((gen_key, None));
    }
    identities.insert(ident, gen_key.clone());

    // 3. The generator must be an exported `gen fn` in a module this file loaded.
    let gen_mod_key = modules
        .iter()
        .find(|m| {
            m.program
                .functions
                .iter()
                .any(|f| f.name == name && f.is_gen && f.exported)
        })
        .map(|m| m.key.clone())
        .ok_or_else(|| {
            err(format!(
                "`{name}` is not an imported `gen fn` — a generator import target must be an \
                 exported `gen fn` in a module this file imports"
            ))
        })?;
    let gen_fn = modules
        .iter()
        .flat_map(|m| &m.program.functions)
        .find(|f| f.name == name && f.is_gen)
        .expect("generator found above");
    if gen_fn.params.len() != consts.len() {
        return Err(err(format!(
            "generator `{name}` takes {} argument(s), got {}",
            gen_fn.params.len(),
            consts.len()
        )));
    }

    // 4. The generator's own source, for the cache key. Loading and CHECKING it
    //    is deferred to the cache-miss path below — on a hit nothing here is
    //    used, and it was the most expensive step in a warm keystroke.
    let gen_source = resolver.read(&gen_mod_key).map_err(|e| {
        err(format!(
            "cannot re-read generator module `{gen_mod_key}`: {e}"
        ))
    })?;

    // 5. Content-addressed cache key: generator sources ++ args ++ inputs read.
    // Each constant string path argument becomes an allowed input root. A path
    // that names a module (no extension) also admits its `.vyrn` file, so
    // `moduleInterface("./contract")` may read `contract.vyrn`.
    let mut allowed: Vec<String> = Vec::new();
    for c in &consts {
        if let crate::consteval::ConstVal::Str(s) = c {
            allowed.push(join_dir(s));
            if !s.ends_with(".vyrn") && !s.ends_with(".json") {
                allowed.push(join_dir(&format!("{s}.vyrn")));
            }
        }
    }
    let sources_hash = generator_cache_key(&gen_mod_key, name, &arg_repr, &allowed);
    let no_cache = std::env::var("VYRN_NO_GEN_CACHE").is_ok();

    // 5a. Cache hit: every recorded input still hashes as it did ⇒ reuse output.
    if !no_cache {
        if let Some(cached) = resolver.gen_cache_get(&sources_hash) {
            if let Some((inputs, output)) = parse_cache_entry(&cached) {
                if inputs
                    .iter()
                    .all(|(path, hash)| current_input_hash(resolver, path) == Some(hash.clone()))
                {
                    return Ok((gen_key, Some(output)));
                }
            }
        }
    }

    // 5b. Cache MISS. Only now load and check the generator as a runnable
    //     program (its own comptime-purity is enforced by the check). Sound to
    //     skip on a hit: an entry is only written after a successful run, which
    //     already passed this same check, and the generator's sources are part
    //     of the cache key — so any edit to it misses and re-checks here.
    let (loaded, _, _, gen_graph) = load_with_origins(&gen_source, &gen_mod_key, opts, resolver);
    let mut gen_program = loaded?;
    // The same check+synthesize a root gets (`crate::check_and_synthesize`): a
    // generator is a runnable program, and RFC-0076 compiles it to wasm, so a
    // builtin whose implementation is synthesized has to be synthesized here too.
    let gdiags = crate::check_and_synthesize(&mut gen_program);
    if !gdiags.is_empty() {
        return Err(gdiags);
    }

    // 4b. Contract provenance (RFC-0071). A generator is re-loaded as its OWN
    //     root, so a contract declared *in* the generator (the normal case —
    //     `std/ui` declares `Page` and `std/ui`'s generator checks against it)
    //     would carry `module: None` and every diagnostic would lose the "which
    //     library demanded this?" half of its message. Restamp each contract with
    //     the IMPORT SPECIFIER of its declaring module (`std/ui`, `./contract`)
    //     rather than a resolved absolute path, since that is what the reader can
    //     actually type. Done after checking so ordinary diagnostics are
    //     untouched, and only for the generator's private copy of the program.
    let std_root = opts.std_root.as_deref();
    let gen_dir = dir_of(&gen_mod_key).to_string();
    for c in &mut gen_program.contracts {
        let key = c.module.clone().unwrap_or_else(|| gen_mod_key.clone());
        c.module = Some(import_specifier(&gen_dir, &key, std_root));
    }

    // The generator's own transitive sources, hashed. Needed twice: the cache
    // entry below records them as inputs (which is what makes editing the
    // generator miss), and the wasm engine keys its compiled artifact on them.
    // Hashing is memoized and these files are re-read on the caching path
    // regardless, so hoisting it above the run costs nothing.
    //
    // A generated module's key is a banner no resolver can read. If the closure
    // contains one there is no describable fingerprint at all — no cache entry
    // (an unverifiable input is worse than a miss) and no artifact key.
    let mut gen_sources: Vec<(String, String)> = Vec::new();
    let mut describable = true;
    for (key, _, _) in &gen_graph {
        match current_input_hash(resolver, key) {
            Some(h) => gen_sources.push((key.clone(), h)),
            None => {
                describable = false;
                break;
            }
        }
    }
    // `gen_mod_key` and the std root join the hashes because the program handed
    // to the engine is not only its files: the contract restamping above spells
    // each module as an import specifier resolved against them.
    let fingerprint = describable.then(|| {
        let mut fp = format!("{gen_mod_key}\u{0}{}\u{0}", std_root.unwrap_or(""));
        for (k, h) in &gen_sources {
            fp.push_str(k);
            fp.push('\u{0}');
            fp.push_str(h);
            fp.push('\u{0}');
        }
        fp
    });

    // 5b. Cache miss: run the generator in the mediated sandbox.
    let out = crate::interp::generate(
        &gen_program,
        name,
        &consts,
        crate::interp::GenInputs {
            resolver,
            opts,
            importer_dir,
            allowed,
            fuel: GEN_FUEL_OVERRIDE.with(|c| c.get()).unwrap_or(GEN_FUEL),
            max_output: GEN_MAX_OUTPUT_OVERRIDE
                .with(|c| c.get())
                .unwrap_or(GEN_MAX_OUTPUT),
            sources_fingerprint: fingerprint,
        },
    )
    .map_err(|trap| err(format!("generator `{name}({arg_repr})` failed: {trap}")))?;
    bump_gen_runs();

    // 6. Cache the output keyed by its recorded inputs, for the next load / the
    //    LSP's per-keystroke re-analysis.
    if !no_cache {
        let mut inputs: Vec<(String, String)> = out
            .reads
            .iter()
            .map(|(p, bytes)| (p.clone(), crate::hash::sha256_hex(bytes)))
            .collect();
        // The generator's OWN transitive sources join the recorded inputs. That is
        // what lets the lookup key stay cheap: the entry now carries everything
        // needed to decide whether it is still valid, instead of the key having to
        // encode it (which meant discovering the closure, which meant parsing the
        // whole generator graph, on every keystroke). Hashed above, before the
        // run, because the engine needs the same hashes for its artifact key.
        inputs.extend(gen_sources);
        if describable {
            resolver.gen_cache_put(&sources_hash, &render_cache_entry(&inputs, &out.source));
        }
    }
    Ok((gen_key, Some(out.source)))
}

/// The cache LOOKUP key: `sha256(generator module ++ name ++ args ++ resolved
/// input roots)`. Deliberately does NOT hash the generator's sources.
///
/// It used to. Encoding the generator's code in the key meant discovering its
/// transitive module closure, which meant a full recursive parse-walk of that
/// graph plus a re-read of every module — measured at 37 ms of a 94 ms keystroke,
/// paid on every cache HIT, to compute a key whose only job was to find an entry
/// that then validates itself anyway.
///
/// Validation moved to where it belongs: the entry records the generator's own
/// sources among its inputs, so a hit re-hashes those files and misses if any
/// changed. Same files checked, discovered from the entry instead of rediscovered
/// by parsing. The trade is that two versions of a generator now collide on one
/// key and take turns owning the entry, rather than each keeping their own.
fn generator_cache_key(
    gen_mod_key: &str,
    name: &str,
    arg_repr: &str,
    resolved_inputs: &[String],
) -> String {
    let mut blob: Vec<u8> = Vec::new();
    for part in [gen_mod_key, name, arg_repr] {
        blob.extend_from_slice(part.as_bytes());
        blob.push(0);
    }
    let mut inputs: Vec<&String> = resolved_inputs.iter().collect();
    inputs.sort();
    inputs.dedup();
    for p in inputs {
        blob.extend_from_slice(p.as_bytes());
        blob.push(0);
    }
    crate::hash::sha256_hex(&blob)
}

/// The current hash of a recorded generation input — a file (`resolver.read`) or
/// a directory listing (a `dir/` marker, `resolver.list`). `None` if it can no
/// longer be read (a miss: the input vanished).
/// Memoized for the duration of ONE outermost load. Validating a generator cache
/// hit re-reads and re-hashes every recorded input, and a root that imports seven
/// generators validates the same std modules seven times — 8.6 ms of a 20 ms load.
/// Files cannot change while a load is running, so computing each hash once is
/// the same answer for less work. The epoch is bumped by the OUTERMOST load only
/// (`load_with_origins` at depth 0), because generators re-enter the loader and a
/// nested bump would throw the memo away mid-use.
fn current_input_hash(resolver: &dyn ModuleResolver, path: &str) -> Option<String> {
    let epoch = LOAD_EPOCH.with(|e| e.get());
    if let Some(hit) = HASH_MEMO.with(|m| {
        m.borrow()
            .get(path)
            .filter(|(e, _)| *e == epoch)
            .map(|(_, h)| h.clone())
    }) {
        return hit;
    }
    let out = current_input_hash_uncached(resolver, path);
    HASH_MEMO.with(|m| {
        m.borrow_mut()
            .insert(path.to_string(), (epoch, out.clone()));
    });
    out
}

fn current_input_hash_uncached(resolver: &dyn ModuleResolver, path: &str) -> Option<String> {
    if let Some(dir) = path.strip_suffix('/') {
        let mut names = resolver.list(dir).ok()?;
        names.sort();
        Some(crate::hash::sha256_hex(names.join("\n").as_bytes()))
    } else {
        Some(crate::hash::sha256_hex(
            resolver.read(path).ok()?.as_bytes(),
        ))
    }
}

/// Serialize a cache entry: an input-hash header (`N` then `path⇥hash` lines)
/// followed verbatim by the generated source.
fn render_cache_entry(inputs: &[(String, String)], output: &str) -> String {
    let mut s = format!("{}\n", inputs.len());
    for (p, h) in inputs {
        s.push_str(&format!("{p}\t{h}\n"));
    }
    s.push_str(output);
    s
}

/// Inverse of [`render_cache_entry`].
fn parse_cache_entry(text: &str) -> Option<(Vec<(String, String)>, String)> {
    let first_nl = text.find('\n')?;
    let n: usize = text[..first_nl].trim().parse().ok()?;
    let mut idx = first_nl + 1;
    let mut inputs = Vec::with_capacity(n);
    for _ in 0..n {
        let nl = text[idx..].find('\n')? + idx;
        let (p, h) = text[idx..nl].split_once('\t')?;
        inputs.push((p.to_string(), h.to_string()));
        idx = nl + 1;
    }
    Some((inputs, text[idx..].to_string()))
}

/// Whether a type decl is one of the parser-injected builtins (`Value`,
/// `Template`, …). They are injected into EVERY parsed file; the linker keeps
/// only the root's copies.
fn is_injected(t: &TypeDecl) -> bool {
    t.line == 0
}

/// Resolve import aliasing (RFC-0022) into the flat namespace *before* the
/// register/visibility/merge machinery, which is deliberately alias-unaware.
///
/// For each `import { X as Y } from M`:
///   * the alias `Y` is checked for collisions in the importing module (against
///     its own top-level decls and its other imports — everything keys on `Y`);
///   * references to `Y` are rewritten to the decl they name;
///   * **co-naming** (the importing module *also* defines a decl called `X` —
///     the RPC stub pattern) frees the name by renaming `M`'s decl `X` to a
///     fresh unique symbol program-wide (its definition, `M`'s internal uses,
///     and every real-name importer), so the local stub keeps `X`.
///
/// Afterwards every import is a bare import of a real, globally-unique decl name,
/// and no reference mentions an alias — so the rest of `link` is untouched. The
/// unlinked root AST the LSP indexes is a separate parse and keeps its aliases.
fn resolve_aliases(modules: &mut [Module], errors: &mut Vec<Diagnostic>, root_key: &str) {
    // Top-level decl names per module, and the union of all decl names (to mint
    // collision-free fresh symbols for co-naming renames).
    let mut module_decls: HashMap<String, HashSet<String>> = HashMap::new();
    // `all_names` exists only so `mint` can pick a collision-free `__fromN`, and
    // most programs never trigger a rename. Building it eagerly cost one extra
    // `String` allocation per declaration on every load — ~4,800 of them on a
    // 762 KB project, for a set usually read zero times. Filled on first use.
    let mut all_names: HashSet<String> = HashSet::new();
    for m in modules.iter() {
        let set = module_decls.entry(m.key.clone()).or_default();
        let mut add = |n: &str| {
            set.insert(n.to_string());
        };
        for t in &m.program.type_decls {
            add(&t.name);
        }
        for f in &m.program.functions {
            add(&f.name);
        }
        for p in &m.program.protocols {
            add(&p.name);
        }
        for c in &m.program.contracts {
            add(&c.name);
        }
        for g in &m.program.globals {
            add(&g.name);
        }
    }

    // Exported top-level decl names per module — the surface a namespace import
    // (RFC-0027) can reach (`ns.member` reaches EXPORTED decls only). Also a
    // program-wide count of how many modules declare each name, so a namespaced
    // module's export is renamed to a fresh symbol only when keeping its name
    // would collide in the flat namespace.
    let mut module_exports: HashMap<String, HashSet<String>> = HashMap::new();
    // Variant names of a module's EXPORTED enums — lets the namespace resolver
    // tell `ns.Enum.Variant(payload)` construction (a variant call) apart from
    // `someFn(ns.Type, ..)` (a type-name argument), which parse identically.
    let mut module_variants: HashMap<String, HashSet<String>> = HashMap::new();
    let mut name_module_count: HashMap<String, usize> = HashMap::new();
    for m in modules.iter() {
        let variants = module_variants.entry(m.key.clone()).or_default();
        for t in &m.program.type_decls {
            if t.line != 0 && t.exported {
                if let Type::Enum(vs) = &t.base {
                    for v in vs {
                        variants.insert(v.name.clone());
                    }
                }
            }
        }
        let set = module_exports.entry(m.key.clone()).or_default();
        let mut ex = |n: &str, exported: bool| {
            if exported {
                set.insert(n.to_string());
            }
        };
        for t in &m.program.type_decls {
            if t.line != 0 {
                ex(&t.name, t.exported);
            }
        }
        for f in &m.program.functions {
            ex(&f.name, f.exported);
        }
        for p in &m.program.protocols {
            ex(&p.name, p.exported);
        }
        for c in &m.program.contracts {
            ex(&c.name, c.exported);
        }
        // Globals are never `export`ed (module state is module-private,
        // RFC-0029 — `export let` does not exist), so they are not
        // namespace-reachable; cross-module access goes through accessor fns.
        for n in module_decls.get(&m.key).into_iter().flatten() {
            *name_module_count.entry(n.clone()).or_insert(0) += 1;
        }
    }

    // Namespace bindings (RFC-0027): module key -> [(ns name, target module)].
    // Validated here for collisions before any reference reinterpretation.
    let mut ns_bindings: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for m in modules.iter() {
        let mine = module_decls.get(&m.key).cloned().unwrap_or_default();
        let import_locals: HashSet<String> = m
            .program
            .imports
            .iter()
            .flat_map(|imp| imp.names.iter())
            .map(|n| n.local().to_string())
            .collect();
        let mut seen_ns: HashSet<String> = HashSet::new();
        let binds = ns_bindings.entry(m.key.clone()).or_default();
        for (imp, target) in m.program.imports.iter().zip(&m.import_targets) {
            let Some(ns) = &imp.namespace else { continue };
            let mut ok = true;
            if !seen_ns.insert(ns.clone()) {
                errors.push(with_file(
                    Diagnostic::error(
                        imp.line,
                        0,
                        "load",
                        format!("namespace `{ns}` is bound twice in this module"),
                    ),
                    m,
                    root_key,
                ));
                ok = false;
            }
            if mine.contains(ns) || import_locals.contains(ns) {
                errors.push(with_file(
                    Diagnostic::error(
                        imp.line,
                        0,
                        "load",
                        format!(
                            "namespace `{ns}` collides with a top-level declaration or import \
                             of the same name in this module"
                        ),
                    ),
                    m,
                    root_key,
                ));
                ok = false;
            }
            if ok {
                binds.push((ns.clone(), target.clone()));
            }
        }
    }

    // (target module, original) -> fresh symbol, for co-naming renames.
    let mut foreign_renames: HashMap<(String, String), String> = HashMap::new();
    // Fill `all_names` on the first mint — see the note at its declaration.
    let ensure_all_names = |all: &mut HashSet<String>, decls: &HashMap<String, HashSet<String>>| {
        if all.is_empty() {
            for names in decls.values() {
                all.extend(names.iter().cloned());
            }
        }
    };
    let mint = |original: &str, all: &mut HashSet<String>| -> String {
        let mut n = 0usize;
        loop {
            let cand = format!("{original}__from{n}");
            if !all.contains(&cand) {
                all.insert(cand.clone());
                return cand;
            }
            n += 1;
        }
    };

    // RFC-0078 M2b: an INJECTED module's every declaration is renamed to its
    // reserved spelling, unconditionally rather than on collision. Two things fall
    // out of that, and they are the reason the injection is safe at all:
    //
    //   * `link`'s program-wide uniqueness check cannot fire. A user's own
    //     `fn emit` would otherwise be "declared by both `main.vyrn` and
    //     `std/json.vyrn`" — an error naming a module they never imported and
    //     cannot remove.
    //   * the desugar's call cannot be captured. RFC-0022 resolves co-naming by
    //     renaming the FOREIGN decl, so a bare `emit` in generated code would have
    //     resolved to the user's function; `json$emit` resolves to one thing.
    //
    // Variants are renamed too, and they had to be: a program whose own enum has a
    // `JStr` variant is rejected today the moment `std/json` is in the link
    // ("function `JStr` is defined in `std/json.vyrn` but not imported here"),
    // which injection would turn into an error about a module the user never
    // mentioned.
    // `(module key, prefix)` for every injected module, and the variant renames of
    // each — a variant is not an import name, so pass 2 would not otherwise reach
    // it. Keyed per module so a hand importer of one runtime module keeps whatever
    // another's variant names mean to it.
    let injected: Vec<(String, &'static str)> = modules
        .iter()
        .filter_map(|m| m.injected.map(|p| (m.key.clone(), p)))
        .collect();
    let mut injected_variants: HashMap<String, HashMap<String, String>> = HashMap::new();
    for (key, prefix) in &injected {
        let m = modules
            .iter()
            .find(|m| &m.key == key)
            .expect("injected module");
        let vars = injected_variants.entry(key.clone()).or_default();
        let mut names: Vec<String> = Vec::new();
        for t in &m.program.type_decls {
            if t.line == 0 {
                continue; // parser-injected builtins are in every module
            }
            names.push(t.name.clone());
            if let Type::Enum(vs) = &t.base {
                for v in vs {
                    vars.insert(v.name.clone(), format!("{prefix}{}", v.name));
                    names.push(v.name.clone());
                }
            }
        }
        for f in &m.program.functions {
            names.push(f.name.clone());
        }
        for p in &m.program.protocols {
            names.push(p.name.clone());
        }
        for c in &m.program.contracts {
            names.push(c.name.clone());
        }
        for g in &m.program.globals {
            names.push(g.name.clone());
        }
        // No `all_names` bookkeeping: `mint` only ever produces `x__fromN`, which
        // has no `$` in it, so a reserved spelling is unreachable from there —
        // and touching `all_names` here would defeat its lazy fill.
        for n in names {
            foreign_renames.insert((key.clone(), n.clone()), format!("{prefix}{n}"));
        }
        // A flattened impl method follows its TYPE's rename rather than taking
        // the prefix in front of the mangling. The parser turns `impl P for T`
        // into a function called `P__T__m` (`parser.rs`), and the checker
        // resolves a call to it by mangling the type key it sees — which here is
        // the RENAMED `json$Json`. So the definition has to be
        // `Copy__json$Json__copy`; `json$Copy__Json__copy`, which the loop above
        // would mint, is a name nothing looks up. Overwrites that entry, so it
        // runs after it.
        for im in &m.program.impls {
            let Some(k) = crate::types::type_key(&im.ty) else {
                continue;
            };
            for me in &im.methods {
                let old = crate::types::impl_method_name(&im.protocol, &k, &me.name);
                let new =
                    crate::types::impl_method_name(&im.protocol, &format!("{prefix}{k}"), &me.name);
                foreign_renames.insert((key.clone(), old), new);
            }
        }
    }

    // Pass 1: alias collision checks + decide co-naming renames.
    for m in modules.iter() {
        let mine = module_decls.get(&m.key).cloned().unwrap_or_default();
        // local name -> (target module, original name) of the import that bound it.
        let mut locals_seen: HashMap<String, (String, String)> = HashMap::new();
        for (imp, target) in m.program.imports.iter().zip(&m.import_targets) {
            for n in &imp.names {
                let local = n.local().to_string();
                // The alias (or bare name) must not clash with another import's
                // local name, nor — when it differs from the original — with a
                // top-level decl of this module.
                let here = (target.clone(), n.original.clone());
                if let Some(prev) = locals_seen.insert(local.clone(), here.clone()) {
                    // The SAME name from two different modules is not really a
                    // double binding — it is those two modules sharing a
                    // top-level name, which `link` reports once for the pair,
                    // with the namespace fix attached. Saying "imported twice"
                    // here as well would bill one mistake twice. A repeat from
                    // the one module, or two different names aliased to one
                    // local, has no such owner and still errors here.
                    let one_name_two_modules = prev.0 != here.0 && prev.1 == here.1;
                    if !one_name_two_modules {
                        errors.push(with_file(
                            Diagnostic::error(
                                imp.line,
                                0,
                                "load",
                                format!("`{local}` is imported twice into this module"),
                            ),
                            m,
                            root_key,
                        ));
                    }
                }
                if n.alias.is_some() && mine.contains(&local) {
                    errors.push(with_file(
                        Diagnostic::error(
                            imp.line,
                            0,
                            "load",
                            format!(
                                "import alias `{local}` clashes with a top-level declaration of \
                                 the same name in this module"
                            ),
                        ),
                        m,
                        root_key,
                    ));
                }
            }
        }
        // Co-naming: an aliased import whose ORIGINAL name is also defined locally.
        for (imp, target) in m.program.imports.iter().zip(&m.import_targets) {
            for n in &imp.names {
                if n.alias.is_some() && mine.contains(&n.original) {
                    let key = (target.clone(), n.original.clone());
                    if !foreign_renames.contains_key(&key) {
                        ensure_all_names(&mut all_names, &module_decls);
                        let s = mint(&n.original, &mut all_names);
                        foreign_renames.insert(key, s);
                    }
                }
            }
        }
        // An aliased import HIDES the original name: it may not be used directly
        // (unless the module also defines or bare-imports it). This must be caught
        // before the reference rewrite fuses alias and original into one name.
        let bare_imported: HashSet<&str> = m
            .program
            .imports
            .iter()
            .flat_map(|imp| imp.names.iter())
            .filter(|n| n.alias.is_none())
            .map(|n| n.original.as_str())
            .collect();
        let refs = program_ref_names(&m.program);
        for imp in &m.program.imports {
            for n in &imp.names {
                if let Some(_alias) = &n.alias {
                    let orig = &n.original;
                    if !mine.contains(orig)
                        && !bare_imported.contains(orig.as_str())
                        && refs.contains(orig)
                    {
                        errors.push(with_file(
                            Diagnostic::error(
                                imp.line,
                                0,
                                "load",
                                format!(
                                    "`{orig}` is not in scope — it was imported as `{}`; use \
                                     that name (or import `{orig}` too)",
                                    n.local()
                                ),
                            ),
                            m,
                            root_key,
                        ));
                    }
                }
            }
        }
    }

    // Namespace renames (RFC-0027): a namespaced module keeps its exports OUT of
    // the flat namespace, so an export whose name is also declared elsewhere is
    // renamed to a fresh program-wide symbol (the same `member__fromN` mechanics
    // co-naming uses). `ns.member` and any selective importer both resolve to
    // that symbol; a name unique to its module keeps it (no churn). This is what
    // lets two namespaced modules export the same name and coexist.
    let namespaced_targets: HashSet<String> = ns_bindings
        .values()
        .flatten()
        .map(|(_, t)| t.clone())
        .collect();
    // Deterministic order at BOTH levels. The inner names were sorted from the
    // start; the targets were not, and `namespaced_targets` is a `HashSet`, so
    // which module got `__from0` and which got `__from1` varied per run for the
    // same input. Two consecutive loads of an unchanged program produced
    // different linked programs — `encodeProps__from0` naming `Array<Paste>` in
    // one and `Paste` in the next.
    let mut namespaced_targets: Vec<String> = namespaced_targets.into_iter().collect();
    namespaced_targets.sort();
    for target in &namespaced_targets {
        let exports = module_exports.get(target).cloned().unwrap_or_default();
        let mut names: Vec<&String> = exports.iter().collect();
        names.sort();
        for name in names {
            if name_module_count.get(name).copied().unwrap_or(0) >= 2 {
                ensure_all_names(&mut all_names, &module_decls);
                foreign_renames
                    .entry((target.clone(), name.clone()))
                    .or_insert_with(|| mint(name, &mut all_names));
            }
        }
    }

    // Name-privacy (RFC-0046 §3): a NON-EXPORTED top-level decl is invisible
    // outside its module, so it must never force a consumer to rename. When such
    // a decl's name also appears in another module, auto-rename it to a fresh
    // program-wide symbol (the same `member__fromN` machinery co-naming uses) —
    // always safe, since nothing can import a non-exported name by name, and its
    // module's own references follow (pass 3's `rename_decl_in_module`). Without
    // this, `std/time`'s private `pad2` collided with a consumer's local `pad2`
    // ("private names aren't private to name resolution"). Deterministic order so
    // the minted suffixes are stable.
    let mut priv_targets: Vec<&Module> = modules.iter().collect();
    priv_targets.sort_by(|a, b| a.key.cmp(&b.key));
    for m in priv_targets {
        let exported = module_exports.get(&m.key).cloned().unwrap_or_default();
        // A name the module itself imports is NOT eligible: a private decl whose
        // name is also brought into scope by an import is a genuine local clash
        // (import vs. declaration) the user must resolve — auto-renaming would
        // silently hide it. Renaming stays limited to names invisible outside
        // their module AND not shadowing an in-scope import here.
        let imported: HashSet<String> = m
            .program
            .imports
            .iter()
            .flat_map(|imp| imp.names.iter())
            .map(|n| n.local().to_string())
            .collect();
        // Non-exported top-level decl names (skip parser-injected line-0 types,
        // which are the same in every module and must never be renamed). Globals
        // are never exported (RFC-0029), so they are always candidates.
        let mut privates: Vec<String> = Vec::new();
        for t in &m.program.type_decls {
            if t.line != 0 && !exported.contains(&t.name) {
                privates.push(t.name.clone());
            }
        }
        for f in &m.program.functions {
            if !exported.contains(&f.name) {
                privates.push(f.name.clone());
            }
        }
        for p in &m.program.protocols {
            if !exported.contains(&p.name) {
                privates.push(p.name.clone());
            }
        }
        for c in &m.program.contracts {
            if !exported.contains(&c.name) {
                privates.push(c.name.clone());
            }
        }
        for g in &m.program.globals {
            privates.push(g.name.clone());
        }
        privates.sort();
        privates.dedup();
        for name in privates {
            if imported.contains(&name) {
                continue;
            }
            if name_module_count.get(&name).copied().unwrap_or(0) >= 2 {
                ensure_all_names(&mut all_names, &module_decls);
                foreign_renames
                    .entry((m.key.clone(), name.clone()))
                    .or_insert_with(|| mint(&name, &mut all_names));
            }
        }
    }

    // Pass 2: per-module reference-rewrite maps (alias/local -> resolved decl).
    let mut rewrites: HashMap<String, HashMap<String, String>> = HashMap::new();
    for m in modules.iter() {
        for (imp, target) in m.program.imports.iter().zip(&m.import_targets) {
            for n in &imp.names {
                let resolved = foreign_renames
                    .get(&(target.clone(), n.original.clone()))
                    .cloned()
                    .unwrap_or_else(|| n.original.clone());
                if n.alias.is_some() {
                    // The alias resolves to the decl (renamed or original).
                    rewrites
                        .entry(m.key.clone())
                        .or_default()
                        .insert(n.local().to_string(), resolved);
                } else if resolved != n.original {
                    // A bare (real-name) importer of a co-named decl follows the rename.
                    rewrites
                        .entry(m.key.clone())
                        .or_default()
                        .insert(n.original.clone(), resolved);
                }
            }
            // RFC-0078 M2b: a HAND importer of the injected module follows its
            // variant renames too. Importing an enum brings its variants, and those
            // are references rather than import names, so nothing above reaches
            // them. Applied per importing module (not program-wide) so a module
            // that does NOT import `std/json` keeps whatever `JObj` means to it.
            if !imp.names.is_empty() {
                if let Some(vars) = injected_variants.get(target) {
                    rewrites
                        .entry(m.key.clone())
                        .or_default()
                        .extend(vars.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            }
        }
    }

    // Pass 3: apply the foreign-decl renames (definition + owning module refs).
    // The renamed module's OWN namespace bindings guard its `ns.member(..)` call
    // sugar from the plain-name rewrite (pass 5 owns those references).
    for ((target, original), s) in &foreign_renames {
        if let Some(tm) = modules.iter_mut().find(|m| &m.key == target) {
            let ns_names: HashSet<String> = ns_bindings
                .get(&tm.key)
                .into_iter()
                .flatten()
                .map(|(n, _)| n.clone())
                .collect();
            rename_decl_in_module(&mut tm.program, original, s, &ns_names);
        }
    }

    // Pass 3b (RFC-0078 M2b): the injected module's enum VARIANT names in the
    // declarations themselves. Pass 3 rewrote every reference to them (a
    // constructor call and a `match` pattern both go through the rename map); the
    // variant list lives in the decl's `Type::Enum` base, which no reference
    // rewrite touches.
    for (key, _) in &injected {
        let Some(vars) = injected_variants.get(key) else {
            continue;
        };
        if let Some(tm) = modules.iter_mut().find(|m| &m.key == key) {
            for t in &mut tm.program.type_decls {
                if t.line == 0 {
                    continue;
                }
                if let Type::Enum(vs) = &mut t.base {
                    for v in vs {
                        if let Some(r) = vars.get(&v.name) {
                            v.name = r.clone();
                        }
                    }
                }
            }
        }
    }

    // Pass 4: apply per-module reference rewrites, and normalize each import to a
    // bare import of the resolved decl name so register/visibility stay unaware.
    for m in modules.iter_mut() {
        if let Some(map) = rewrites.get(&m.key) {
            let ns_names: HashSet<String> = ns_bindings
                .get(&m.key)
                .into_iter()
                .flatten()
                .map(|(n, _)| n.clone())
                .collect();
            rewrite_module_refs(&mut m.program, map, &ns_names);
        }
        for (imp, target) in m.program.imports.iter_mut().zip(&m.import_targets) {
            for n in &mut imp.names {
                let resolved = foreign_renames
                    .get(&(target.clone(), n.original.clone()))
                    .cloned()
                    .unwrap_or_else(|| n.original.clone());
                n.original = resolved;
                n.alias = None;
            }
        }
    }

    // Pass 5 (RFC-0027): reinterpret `ns.member` uses in each namespaced module
    // into the resolved program-wide symbol. Runs after the alias/co-naming
    // rewrites so the two never interfere (alias rewriting touches plain names;
    // this touches `ns.`-headed member access, which alias rewriting leaves
    // alone). Local bindings shadow namespaces — the walk is scope-aware.
    for m in modules.iter_mut() {
        let binds: HashMap<String, String> = match ns_bindings.get(&m.key) {
            Some(b) if !b.is_empty() => b.iter().cloned().collect(),
            _ => continue,
        };
        let mut nr = NsResolver {
            ns: binds,
            foreign_renames: &foreign_renames,
            module_exports: &module_exports,
            module_variants: &module_variants,
            module_key: m.key.clone(),
            root_key: root_key.to_string(),
            errors,
        };
        nr.resolve_program(&mut m.program);
    }
}

/// Reinterprets namespace-qualified references (`ns.member`, RFC-0027) inside one
/// importing module into the resolved program-wide decl symbols. A namespace is a
/// compile-time name, not a value: any surviving bare use of it is an error.
struct NsResolver<'a> {
    /// The module's in-scope namespaces: `ns` name -> target module key.
    ns: HashMap<String, String>,
    foreign_renames: &'a HashMap<(String, String), String>,
    /// Exported decl names (originals) per module — the namespace-reachable surface.
    module_exports: &'a HashMap<String, HashSet<String>>,
    /// Exported-enum variant names per module (disambiguates variant construction
    /// from type-name arguments).
    module_variants: &'a HashMap<String, HashSet<String>>,
    module_key: String,
    root_key: String,
    errors: &'a mut Vec<Diagnostic>,
}

impl NsResolver<'_> {
    fn err(&mut self, line: usize, msg: String) {
        let mut d = Diagnostic::error(line, 0, "load", msg);
        if self.module_key != self.root_key {
            d.file = Some(self.module_key.clone());
        }
        self.errors.push(d);
    }

    /// The program-wide symbol a namespace member resolves to (honoring any
    /// collision rename), or an error if the target does not EXPORT it.
    fn resolve_member(&mut self, ns: &str, member: &str, line: usize) -> Option<String> {
        let target = self.ns.get(ns).cloned()?;
        let exported = self
            .module_exports
            .get(&target)
            .is_some_and(|s| s.contains(member));
        if !exported {
            self.err(
                line,
                format!(
                    "namespace `{ns}` (module `{target}`) has no exported member `{member}` — \
                     namespaces reach exported declarations only, one level deep"
                ),
            );
            return None;
        }
        Some(
            self.foreign_renames
                .get(&(target, member.to_string()))
                .cloned()
                .unwrap_or_else(|| member.to_string()),
        )
    }

    fn resolve_program(&mut self, p: &mut Program) {
        for f in &mut p.functions {
            let mut locals: HashSet<String> = f.params.iter().map(|pm| pm.name.clone()).collect();
            self.walk_type_positions_fn(f, &locals.clone());
            self.walk_block(&mut f.body, &mut locals);
        }
        for im in &mut p.impls {
            self.rewrite_type(&mut im.ty);
            for m in &mut im.methods {
                let mut locals: HashSet<String> =
                    m.params.iter().map(|pm| pm.name.clone()).collect();
                self.walk_type_positions_fn(m, &locals.clone());
                self.walk_block(&mut m.body, &mut locals);
            }
        }
        for t in &mut p.type_decls {
            if t.line == 0 {
                continue;
            }
            self.rewrite_type(&mut t.base);
            if let Some(pred) = &mut t.predicate {
                let mut locals: HashSet<String> = std::iter::once("value".to_string()).collect();
                self.walk_expr(pred, &mut locals);
            }
        }
        for g in &mut p.globals {
            if let Some(ty) = &mut g.ty {
                self.rewrite_type(ty);
            }
            let mut locals = HashSet::new();
            self.walk_expr(&mut g.init, &mut locals);
        }
        for t in &mut p.tests {
            let mut locals = HashSet::new();
            self.walk_block(&mut t.body, &mut locals);
        }
        for b in &mut p.benches {
            let mut locals = HashSet::new();
            self.walk_block(&mut b.body, &mut locals);
        }
    }

    /// Rewrite namespace-qualified types in a function's signature (params, return,
    /// bounds are plain protocol names handled via bounds map below).
    fn walk_type_positions_fn(&mut self, f: &mut Function, _locals: &HashSet<String>) {
        for pm in &mut f.params {
            self.rewrite_type(&mut pm.ty);
        }
        self.rewrite_type(&mut f.ret);
        for bounds in f.type_bounds.values_mut() {
            for b in bounds.iter_mut() {
                // `<T: ns.Show>` — a bound is a bare protocol name; the parser
                // never produces a dotted bound, but a namespaced protocol bound
                // is written `ns.Show` and lands as one dotted string here only if
                // the type parser routed it through `Type::Named`. Bounds are
                // plain strings, so a dotted bound would already have failed to
                // parse; nothing to do beyond the (rare) dotted spelling.
                if let Some((ns, member)) = b.split_once('.') {
                    let line = f.line;
                    if let Some(sym) = self.resolve_member(ns, member, line) {
                        *b = sym;
                    }
                }
            }
        }
    }

    /// Rewrite a namespace-qualified named/applied type (`ns.User`, `ns.Box<T>`)
    /// into its resolved decl name, recursing through the whole type tree.
    fn rewrite_type(&mut self, ty: &mut Type) {
        match ty {
            Type::Named(n) => {
                if let Some((ns, member)) = n.clone().split_once('.') {
                    if self.ns.contains_key(ns) {
                        if let Some(sym) = self.resolve_member(ns, member, 0) {
                            *n = sym;
                        }
                    }
                }
            }
            Type::App(n, args) => {
                if let Some((ns, member)) = n.clone().split_once('.') {
                    if self.ns.contains_key(ns) {
                        if let Some(sym) = self.resolve_member(ns, member, 0) {
                            *n = sym;
                        }
                    }
                }
                for a in args {
                    self.rewrite_type(a);
                }
            }
            Type::Option(a)
            | Type::Array(a)
            | Type::Task(a)
            | Type::Stream(a)
            | Type::Partial(a)
            | Type::ArrayN(a, _)
            | Type::SmallArray(a, _)
            | Type::Omit(a, _)
            | Type::Pick(a, _) => self.rewrite_type(a),
            Type::Result(a, b) | Type::Merge(a, b) => {
                self.rewrite_type(a);
                self.rewrite_type(b);
            }
            Type::Record(fs) => {
                for f in fs {
                    self.rewrite_type(&mut f.ty);
                }
            }
            Type::Enum(vs) => {
                for v in vs {
                    for pl in &mut v.payload {
                        self.rewrite_type(pl);
                    }
                }
            }
            Type::Fn(params, ret) => {
                for pt in params {
                    self.rewrite_type(pt);
                }
                self.rewrite_type(ret);
            }
            Type::Map(k, v) => {
                self.rewrite_type(k);
                self.rewrite_type(v);
            }
            _ => {}
        }
    }

    /// Whether `ns` is an in-scope namespace at this use (not shadowed by a local).
    fn is_ns(&self, ns: &str, locals: &HashSet<String>) -> bool {
        self.ns.contains_key(ns) && !locals.contains(ns)
    }

    fn walk_block(&mut self, b: &mut Block, locals: &mut HashSet<String>) {
        for s in &mut b.stmts {
            self.walk_stmt(s, locals);
        }
    }

    fn walk_stmt(&mut self, s: &mut Stmt, locals: &mut HashSet<String>) {
        match s {
            Stmt::Let {
                name, value, ty, ..
            } => {
                if let Some(t) = ty {
                    self.rewrite_type(t);
                }
                self.walk_expr(value, locals);
                // The binding is in scope for subsequent statements (and shadows a
                // like-named namespace from here on).
                locals.insert(name.clone());
            }
            Stmt::Assign { value, .. } | Stmt::SetField { value, .. } => {
                self.walk_expr(value, locals)
            }
            Stmt::IndexSet { index, value, .. } => {
                self.walk_expr(index, locals);
                self.walk_expr(value, locals);
            }
            Stmt::Return { value: Some(e), .. } => self.walk_expr(e, locals),
            Stmt::Return { value: None, .. } => {}
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.walk_expr(cond, locals);
                let mut inner = locals.clone();
                self.walk_block(then_block, &mut inner);
                if let Some(eb) = else_block {
                    let mut inner2 = locals.clone();
                    self.walk_block(eb, &mut inner2);
                }
            }
            Stmt::IfLet {
                scrutinee,
                then_block,
                else_block,
                pattern,
                ..
            } => {
                self.walk_expr(scrutinee, locals);
                let mut inner = locals.clone();
                for b in crate::movecheck::pattern_bindings(pattern) {
                    inner.insert(b.to_string());
                }
                self.walk_block(then_block, &mut inner);
                if let Some(eb) = else_block {
                    let mut inner2 = locals.clone();
                    self.walk_block(eb, &mut inner2);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.walk_expr(cond, locals);
                let mut inner = locals.clone();
                self.walk_block(body, &mut inner);
            }
            Stmt::ForIn {
                var, iter, body, ..
            } => {
                self.walk_expr(iter, locals);
                let mut inner = locals.clone();
                inner.insert(var.clone());
                self.walk_block(body, &mut inner);
            }
            Stmt::Drop { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::Expr(e) => self.walk_expr(e, locals),
            Stmt::Region { body, .. } => {
                let mut inner = locals.clone();
                self.walk_block(body, &mut inner);
            }
        }
    }

    fn walk_expr(&mut self, e: &mut Expr, locals: &HashSet<String>) {
        match e {
            // `ns.fn(args)` and `ns.Enum.Variant(payload)` both arrive as method
            // sugar — the receiver is the first argument.
            Expr::Call { name, args, line } => {
                let l = *line;
                // `ns.member(rest)` — first arg is the bare namespace.
                if let Some(Expr::Var { name: head, .. }) = args.first() {
                    if self.is_ns(head, locals) {
                        let head = head.clone();
                        if let Some(sym) = self.resolve_member(&head, name, l) {
                            *name = sym;
                        }
                        args.remove(0);
                        for a in args.iter_mut() {
                            self.walk_expr(a, locals);
                        }
                        return;
                    }
                }
                // `ns.Enum.Variant(payload)` — first arg is `ns.Enum` field access
                // AND the call name is a variant of that namespaced module's enums.
                // Otherwise this is `someFn(ns.Type, ..)` (a type-name argument),
                // which parses identically — fall through and let the `Field` arm
                // rewrite `ns.Type`.
                if let Some(Expr::Field { expr: inner, .. }) = args.first() {
                    if let Expr::Var { name: head, .. } = inner.as_ref() {
                        let is_variant_call = self.is_ns(head, locals)
                            && self
                                .ns
                                .get(head)
                                .and_then(|t| self.module_variants.get(t))
                                .is_some_and(|vs| vs.contains(name));
                        if is_variant_call {
                            // The variant name is global (variants are not renamed);
                            // drop the qualifier receiver and keep the call name.
                            args.remove(0);
                            for a in args.iter_mut() {
                                self.walk_expr(a, locals);
                            }
                            return;
                        }
                    }
                }
                for a in args.iter_mut() {
                    self.walk_expr(a, locals);
                }
            }
            Expr::Spawn { args, .. } | Expr::TryConstruct { args, .. } => {
                for a in args.iter_mut() {
                    self.walk_expr(a, locals);
                }
            }
            Expr::StructLit { name, fields, line } => {
                // `ns.Type { .. }` — the parser encoded the qualifier as `ns.Type`.
                if let Some((ns, member)) = name.clone().split_once('.') {
                    if self.is_ns(ns, locals) {
                        if let Some(sym) = self.resolve_member(ns, member, *line) {
                            *name = sym;
                        }
                    } else {
                        let (ns, line) = (ns.to_string(), *line);
                        self.err(line, format!("`{ns}` is not an in-scope namespace"));
                    }
                }
                for (_, v) in fields.iter_mut() {
                    self.walk_expr(v, locals);
                }
            }
            Expr::Field { expr, field, line } => {
                let l = *line;
                // `ns.member` (type-name value / function value / nullary access).
                if let Expr::Var { name: head, .. } = expr.as_ref() {
                    if self.is_ns(head, locals) {
                        let head = head.clone();
                        if let Some(sym) = self.resolve_member(&head, field, l) {
                            *e = Expr::Var { name: sym, line: l };
                        }
                        return;
                    }
                }
                // `ns.Enum.Variant` (nullary variant) — `ns.Enum` is the inner field.
                if let Expr::Field {
                    expr: inner,
                    field: enum_name,
                    ..
                } = expr.as_ref()
                {
                    if let Expr::Var { name: head, .. } = inner.as_ref() {
                        if self.is_ns(head, locals) {
                            let (head, enum_name, variant) =
                                (head.clone(), enum_name.clone(), field.clone());
                            let is_variant = self
                                .ns
                                .get(&head)
                                .and_then(|t| self.module_variants.get(t))
                                .is_some_and(|vs| vs.contains(&variant));
                            if is_variant {
                                let _ = self.resolve_member(&head, &enum_name, l);
                                *e = Expr::Var {
                                    name: variant,
                                    line: l,
                                };
                            } else {
                                self.err(
                                    l,
                                    format!(
                                        "`{head}.{enum_name}.{variant}` is not a namespaced enum \
                                         variant (namespaces are one level deep)"
                                    ),
                                );
                            }
                            return;
                        }
                    }
                }
                self.walk_expr(expr, locals);
            }
            Expr::Var { name, line } => {
                if self.is_ns(name, locals) {
                    let (name, line) = (name.clone(), *line);
                    self.err(line, format!("namespace `{name}` is not a value"));
                }
            }
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } => self.walk_expr(expr, locals),
            Expr::Consume { place, .. } => self.walk_expr(place, locals),
            Expr::Binary { lhs, rhs, .. } => {
                self.walk_expr(lhs, locals);
                self.walk_expr(rhs, locals);
            }
            Expr::Match {
                scrutinee,
                arms,
                line,
            } => {
                let l = *line;
                self.walk_expr(scrutinee, locals);
                for arm in arms.iter_mut() {
                    let mut inner = locals.clone();
                    match &mut arm.pattern {
                        Pattern::Variant(v, binds) => {
                            // `ns.Enum.Variant` pattern — reduce the dotted path to
                            // the bare variant (variants are global; the enum need
                            // only be an exported member of the namespace).
                            if let Some(idx) = v.find('.') {
                                let ns = v[..idx].to_string();
                                let rest = &v[idx + 1..];
                                let variant = rest.rsplit('.').next().unwrap_or(rest).to_string();
                                let enum_name = rest.split('.').next().unwrap_or(rest).to_string();
                                if self.ns.contains_key(&ns) {
                                    let _ = self.resolve_member(&ns, &enum_name, l);
                                    *v = variant;
                                }
                            }
                            for b in binds.iter() {
                                inner.insert(b.clone());
                            }
                        }
                        Pattern::Some(b)
                        | Pattern::Ok(b)
                        | Pattern::Err(b)
                        | Pattern::Success(b)
                        | Pattern::Failure(b) => {
                            inner.insert(b.clone());
                        }
                        Pattern::None => {}
                    }
                    self.walk_expr(&mut arm.body, &mut inner);
                }
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.walk_expr(cond, locals);
                self.walk_expr(then_branch, locals);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb, locals);
                }
            }
            Expr::ArrayLit { elems, .. } => {
                for e2 in elems.iter_mut() {
                    self.walk_expr(e2, locals);
                }
            }
            Expr::MapLit { entries, .. } => {
                for (k, v) in entries.iter_mut() {
                    self.walk_expr(k, locals);
                    self.walk_expr(v, locals);
                }
            }
            Expr::Lambda { params, body, .. } => {
                let mut inner = locals.clone();
                for p in params.iter() {
                    inner.insert(p.clone());
                }
                match body {
                    LambdaBody::Expr(e2) => self.walk_expr(e2, &inner),
                    LambdaBody::Block(b2) => self.walk_block(b2, &mut inner),
                }
            }
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
        }
    }
}

fn link(mut modules: Vec<Module>, root_key: &str) -> Result<Program, Vec<Diagnostic>> {
    let mut errors: Vec<Diagnostic> = Vec::new();
    // RFC-0022: fold import aliases into the flat namespace up front.
    resolve_aliases(&mut modules, &mut errors, root_key);

    // ---- indexes over all modules ----------------------------------------
    // top-level name -> (module key, exported)
    let mut owner: HashMap<String, (String, bool)> = HashMap::new();
    // enum variant name -> owning enum's type name
    let mut variant_enum: HashMap<String, String> = HashMap::new();
    // protocol method name -> protocol name
    let mut method_protocol: HashMap<String, String> = HashMap::new();

    // Flat-namespace collisions, as `(name, first owner, second owner)`. They are
    // COLLECTED rather than reported: one pair of modules sharing five names is
    // one problem with five symptoms, and the decl line they carry belongs to a
    // module the user may never have opened. `clash_diagnostics` turns the whole
    // batch into one diagnostic per module pair, at an import site in a real file.
    let mut clashes: Vec<(String, String, String)> = Vec::new();

    let mut register =
        |name: &str, module: &str, exported: bool, clashes: &mut Vec<(String, String, String)>| {
            // A reserved name never enters the flat namespace, and the reason is
            // not tidiness. `owner` is what decides whether a use is a foreign
            // reference, so registering one made every use of the BUILTIN inside
            // a linked `std/` module look like an unimported import: a single
            // user `fn at` produced 53 diagnostics, all pointing into
            // `std/num.vyrn`, none at the declaration, none saying "reserved".
            //
            // No diagnostic here on purpose. `check`'s own RESERVED guard already
            // reports this once, at the declaration, with the right wording — it
            // was simply never reached, because the loader failed the program
            // first. Skipping is what lets it be reached.
            if crate::checker::RESERVED.contains(&name) {
                return;
            }
            if let Some((prev, _)) = owner.get(name) {
                if prev != module {
                    clashes.push((name.to_string(), prev.clone(), module.to_string()));
                }
                return;
            }
            owner.insert(name.to_string(), (module.to_string(), exported));
        };

    for m in &modules {
        for t in &m.program.type_decls {
            if is_injected(t) {
                continue;
            }
            register(&t.name, &m.key, t.exported, &mut clashes);
            if let Type::Enum(vs) = &t.base {
                for v in vs {
                    variant_enum.insert(v.name.clone(), t.name.clone());
                }
            }
        }
        for f in &m.program.functions {
            // Impl-flattened methods carry mangled names (`P__Key__m`) that
            // cannot collide with user identifiers; register them anyway so
            // duplicate impls across modules collide loudly here.
            register(&f.name, &m.key, f.exported, &mut clashes);
        }
        for p in &m.program.protocols {
            register(&p.name, &m.key, p.exported, &mut clashes);
            for sig in &p.methods {
                method_protocol.insert(sig.name.clone(), p.name.clone());
            }
        }
        // Contracts (RFC-0071) join the same top-level namespace as protocols:
        // a contract name is what `contractOf(Name)` resolves, so it must be
        // program-wide unique and obey the ordinary export/import visibility.
        for c in &m.program.contracts {
            register(&c.name, &m.key, c.exported, &mut clashes);
        }
        // Module-state bindings (RFC-0013) join the top-level namespace: a
        // global may not share a name with any other top-level declaration.
        for g in &m.program.globals {
            register(&g.name, &m.key, false, &mut clashes);
        }
    }
    errors.extend(clash_diagnostics(&clashes, &modules, root_key));
    // `owner` kept only the FIRST module of every collision, so from here on it
    // answers "where does this live?" with half the truth. The checks below must
    // not repeat that half-truth as its own error: `map` IS defined in
    // `std/stream`, it just lost the flat namespace to `std/arrays`, and telling
    // the user to look in `std/arrays` sends them somewhere the fix is not.
    let clashed: HashSet<&str> = clashes.iter().map(|(n, _, _)| n.as_str()).collect();

    // ---- per-module import + visibility checks ---------------------------
    for m in &modules {
        let mut visible: HashSet<String> = HashSet::new(); // foreign names imported here
        for (imp, target) in m.program.imports.iter().zip(&m.import_targets) {
            // A namespace import (`import * as ns`, RFC-0027) makes every EXPORTED
            // decl of the target reachable via `ns.member` — the same surface a
            // selective import could reach. The `ns.member` uses were already
            // reinterpreted into these decls' symbols, so grant them visibility.
            if imp.namespace.is_some() {
                for (name, (def_module, exported)) in &owner {
                    if def_module == target && *exported {
                        visible.insert(name.clone());
                    }
                }
            }
            for imp_name in &imp.names {
                // Aliases were folded into the flat namespace by `resolve_aliases`
                // (RFC-0022): every import is now a bare import of a real decl name.
                let name = &imp_name.original;
                match owner.get(name) {
                    Some((def_module, exported)) if def_module == target => {
                        if !exported {
                            errors.push(with_file(
                                Diagnostic::error(
                                    imp.line,
                                    0,
                                    "load",
                                    format!(
                                        "`{name}` exists in `{target}` but is not exported — \
                                         add `export` to its declaration"
                                    ),
                                ),
                                m,
                                root_key,
                            ));
                        }
                        // Importing an enum also brings its variants, and a
                        // protocol its methods — the visibility check below
                        // resolves those through this name.
                        visible.insert(name.clone());
                    }
                    Some((def_module, _)) if !clashed.contains(name.as_str()) => {
                        errors.push(with_file(
                            Diagnostic::error(
                                imp.line,
                                0,
                                "load",
                                format!(
                                    "`{name}` is not defined in `{target}` (it lives in \
                                     `{def_module}`)"
                                ),
                            ),
                            m,
                            root_key,
                        ));
                    }
                    // A clashed name: `clash_diagnostics` already reported the
                    // pair. Grant visibility anyway so the reference check below
                    // does not follow up with "defined in X but not imported
                    // here" about the module that merely won the name.
                    Some(_) => {
                        visible.insert(name.clone());
                    }
                    None => {
                        errors.push(with_file(
                            Diagnostic::error(
                                imp.line,
                                0,
                                "load",
                                format!("`{target}` does not define `{name}`"),
                            ),
                            m,
                            root_key,
                        ));
                    }
                }
            }
        }

        // Visibility: every foreign name this module references must have been
        // imported. Names defined nowhere are left for the checker (better
        // messages there). Enum variants map to their enum; protocol methods
        // map to their protocol.
        let own: HashSet<&str> = owner
            .iter()
            .filter(|(_, (module, _))| module == &m.key)
            .map(|(n, _)| n.as_str())
            .collect();
        // A generated module (RFC-0021) may call back into the module that
        // imported it — the callback convention (e.g. an RPC dispatcher invoking
        // the user's plain `onGetUser` handler). Names owned by that importer are
        // visible without an explicit import; generated code is unhygienic source
        // by design, and the importer can never `import` the generated module's
        // own re-exports in reverse, so this is the only way the two connect.
        let gen_importer: Option<String> = generated_importer(&m.key).map(normalize);
        let check_name = |name: &str, line: usize, what: &str, errors: &mut Vec<Diagnostic>| {
            // Resolve constructors/methods to their owning declaration.
            let decl_name = variant_enum
                .get(name)
                .or_else(|| method_protocol.get(name))
                .map(|s| s.as_str())
                .unwrap_or(name);
            if own.contains(decl_name) || visible.contains(decl_name) {
                return;
            }
            if let Some((def_module, _)) = owner.get(decl_name) {
                if def_module != &m.key {
                    if gen_importer.as_deref() == Some(def_module.as_str()) {
                        return;
                    }
                    errors.push(with_file(
                        Diagnostic::error(
                            line,
                            0,
                            "load",
                            format!(
                                "{what} `{name}` is defined in `{def_module}` but not \
                                 imported here — add it to an `import {{ .. }} from` list"
                            ),
                        ),
                        m,
                        root_key,
                    ));
                }
            }
        };

        for f in &m.program.functions {
            // Scope-aware: a name bound by a local (param, `let`, loop/lambda var,
            // match bind) shadows a like-named foreign export and is never a
            // cross-module reference at that use site.
            for c in fn_body_ref_names(f) {
                check_name(&c.0, c.1, "function", &mut errors);
            }
            for p in &f.params {
                for n in type_names(&p.ty) {
                    check_name(&n, f.line, "type", &mut errors);
                }
            }
            for n in type_names(&f.ret) {
                check_name(&n, f.line, "type", &mut errors);
            }
            for bounds in f.type_bounds.values() {
                for b in bounds {
                    check_name(b, f.line, "protocol", &mut errors);
                }
            }
        }
        for t in &m.program.type_decls {
            if is_injected(t) {
                continue;
            }
            for n in type_names(&t.base) {
                check_name(&n, t.line, "type", &mut errors);
            }
        }
        for imp in &m.program.impls {
            check_name(&imp.protocol, imp.line, "protocol", &mut errors);
            for n in type_names(&imp.ty) {
                check_name(&n, imp.line, "type", &mut errors);
            }
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    // ---- merge ------------------------------------------------------------
    // Root last so its injected builtins/log config win; imported modules'
    // injected decls are dropped.
    let mut merged: Option<Program> = None;
    let mut extra_types = Vec::new();
    let mut extra_fns = Vec::new();
    let mut extra_protocols = Vec::new();
    let mut extra_contracts = Vec::new();
    let mut extra_impls = Vec::new();
    let mut extra_tests = Vec::new();
    let mut extra_benches = Vec::new();
    // Any module may reach here with globals (RFC-0029). Every module's state
    // joins the linked program and initializes before `main` in LINKER ORDER —
    // post-order over the import graph, dependencies first (see below).
    let mut extra_globals = Vec::new();
    for m in modules {
        if m.key == root_key {
            merged = Some(m.program);
        } else {
            let p = m.program;
            extra_types.extend(p.type_decls.into_iter().filter(|t| !is_injected(t)));
            extra_fns.extend(p.functions);
            extra_protocols.extend(p.protocols);
            extra_contracts.extend(p.contracts);
            extra_impls.extend(p.impls);
            extra_globals.extend(p.globals);
            // Imported tests keep their `module` tag: they type-check but do not
            // run under `vyrn test <root>` (RFC-0015).
            extra_tests.extend(p.tests);
            // Imported benches likewise (RFC-0055).
            extra_benches.extend(p.benches);
        }
    }
    let mut program = merged.expect("root module was loaded");
    program.type_decls.extend(extra_types);
    program.functions.extend(extra_fns);
    program.protocols.extend(extra_protocols);
    program.contracts.extend(extra_contracts);
    program.impls.extend(extra_impls);
    // Init order (RFC-0029): dependencies first, then the root's own state.
    // `modules` is built depth-first with each module pushed AFTER its imports
    // and the root last, so `extra_globals` is already in post-order over the
    // import graph (a diamond's shared dep appears once, at its first visit).
    // Appending the root's globals last places them at their post-order slot.
    // An only-root program has empty `extra_globals`, so ordering — and thus
    // every existing corpus program's output — is byte-identical to before.
    let mut ordered = extra_globals;
    ordered.append(&mut program.globals);
    program.globals = ordered;
    program.tests.extend(extra_tests);
    program.benches.extend(extra_benches);
    program.imports.clear(); // consumed
    Ok(program)
}

/// Attach the module's file to a diagnostic unless it is the root.
fn with_file(mut d: Diagnostic, m: &Module, root_key: &str) -> Diagnostic {
    if m.key != root_key {
        d.file = Some(m.key.clone());
    }
    d
}

/// The import of `target` a diagnostic should point at, with the module that
/// wrote it — preferring one that names a name in `names`, since that is the line
/// the user has to edit.
fn import_site<'a>(
    modules: &'a [Module],
    target: &str,
    names: &[&str],
) -> Option<(&'a Module, &'a ImportDecl)> {
    let mut fallback = None;
    for m in modules {
        for (imp, t) in m.program.imports.iter().zip(&m.import_targets) {
            if t != target {
                continue;
            }
            if imp
                .names
                .iter()
                .any(|n| names.contains(&n.original.as_str()))
            {
                return Some((m, imp));
            }
            fallback.get_or_insert((m, imp));
        }
    }
    fallback
}

/// A plausible namespace binding for `spec`: its last path segment, minus the
/// extension and anything that is not an identifier character.
fn ns_suggestion(spec: &str) -> String {
    let tail = spec.rsplit(['/', '\\', ':']).next().unwrap_or(spec);
    let stem = tail.strip_suffix(".vyrn").unwrap_or(tail);
    let n: String = stem
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if n.is_empty() || n.starts_with(|c: char| c.is_ascii_digit()) {
        "ns".to_string()
    } else {
        n
    }
}

/// Turn the raw flat-namespace collisions into ONE diagnostic per pair of
/// modules, reported at an import of one of them.
///
/// Every collision `link` finds has the same single cause — two linked modules
/// declaring the same top-level name — but it used to surface once per NAME, at
/// the foreign declaration's line, with no file attached. Importing `map` from
/// both `std/arrays` and `std/stream` therefore produced an error about `filter`,
/// which the user never wrote, pointing at line 69 of a six-line program. Both
/// facts belong to one problem: the pair collides, these are the names, and the
/// fix is a namespace import — which is why they are grouped and re-located here.
fn clash_diagnostics(
    clashes: &[(String, String, String)],
    modules: &[Module],
    root_key: &str,
) -> Vec<Diagnostic> {
    let mut pairs: BTreeMap<(&str, &str), BTreeSet<&str>> = BTreeMap::new();
    for (name, first, second) in clashes {
        pairs
            .entry((first.as_str(), second.as_str()))
            .or_default()
            .insert(name.as_str());
    }
    let mut out = Vec::new();
    for ((first, second), names) in pairs {
        let mut names: Vec<&str> = names.into_iter().collect();
        // Prefer an import of the SECOND module: it is the one whose names lost,
        // and dropping it is the smaller edit. The root is imported by nobody, so
        // fall back to the first when the root is the second owner.
        let (m, imp) = match import_site(modules, second, &names)
            .or_else(|| import_site(modules, first, &names))
        {
            Some(site) => site,
            // Unreachable in a real link (a module is here because it was
            // imported), but the diagnostic must not be lost if it ever is.
            None => {
                out.push(Diagnostic::error(
                    0,
                    0,
                    "load",
                    format!(
                        "`{}` is declared by both `{first}` and `{second}`",
                        names[0]
                    ),
                ));
                continue;
            }
        };
        // Lead with a name the user actually wrote at this line, if any — the
        // rest are collateral and belong in the note. Alphabetical order is fine
        // for those; it is not fine for the headline, which is how `filter` came
        // to front an error about an `import { map }`.
        if let Some(i) = names
            .iter()
            .position(|n| imp.names.iter().any(|x| x.original == *n))
        {
            names.swap(0, i);
            names[1..].sort();
        }
        let spec = match &imp.source {
            ImportSource::Path(p) => Some(p.as_str()),
            ImportSource::Generator { .. } => None,
        };
        let line = imp.line;
        let mut d = Diagnostic::error(
            line,
            0,
            "load",
            format!(
                "`{}` is declared by both `{first}` and `{second}` — a top-level name is \
                 program-wide, so two linked modules cannot share one",
                names[0]
            ),
        );
        let fix = match spec {
            Some(s) => format!(
                "import one of them as a namespace instead — `import * as {ns} from \"{s}\"` \
                 reaches its exports as `{ns}.{}` and keeps them out of the flat namespace",
                names[0],
                ns = ns_suggestion(s)
            ),
            None => "import one of them as a namespace (`import * as ns from ..`) instead — a \
                     namespace keeps its exports out of the flat namespace"
                .to_string(),
        };
        let rest = &names[1..];
        d.note = Some(if rest.is_empty() {
            fix
        } else {
            let list: Vec<String> = rest.iter().map(|n| format!("`{n}`")).collect();
            format!(
                "{fix}; {} collide{} the same way",
                list.join(", "),
                if rest.len() == 1 { "s" } else { "" }
            )
        });
        out.push(with_file(d, m, root_key));
    }
    out
}

/// Every (callee/constructor name, line) referenced in a block — calls, spawns,
/// struct literals, fallible constructions, and bare variant constructors.
///
/// `ns` is the enclosing module's in-scope namespace bindings (RFC-0027), and a
/// call whose receiver is one of them is NOT a bare reference: `bt.routes()`
/// parses as the method sugar `routes(bt)`, so without this the member name of
/// every namespace call reads as a top-level name the module used directly. The
/// same guard [`rewrite_expr`] applies before renaming a callee, for the same
/// reason — a namespace member belongs to its receiver's module, and nothing in
/// this module's flat namespace answers for it. A local that shadows a namespace
/// name (legal, and what `NsResolver`'s scope-aware walk honors) is not modelled
/// here: this walk knows no scopes, so such a call's member name goes unrecorded
/// rather than misrecorded — the direction that reports less, not wrongly.
fn fn_body_names(b: &Block, ns: &HashSet<String>) -> Vec<(String, usize)> {
    /// The walk's accumulator, carrying the namespace set the `Call` arm needs.
    /// A field rather than a parameter on every nested function: the recursion
    /// already threads one `&mut` everywhere, so this rides it.
    struct Sink<'a> {
        out: Vec<(String, usize)>,
        ns: &'a HashSet<String>,
    }
    impl Sink<'_> {
        fn push(&mut self, n: (String, usize)) {
            self.out.push(n);
        }
    }
    let mut out = Sink {
        out: Vec::new(),
        ns,
    };
    fn stmt(s: &Stmt, out: &mut Sink) {
        match s {
            Stmt::Let {
                value, line, ty, ..
            } => {
                if let Some(t) = ty {
                    for n in type_names(t) {
                        out.push((n, *line));
                    }
                }
                expr(value, *line, out)
            }
            Stmt::Assign { value, line, .. } | Stmt::SetField { value, line, .. } => {
                expr(value, *line, out)
            }
            Stmt::IndexSet {
                index, value, line, ..
            } => {
                expr(index, *line, out);
                expr(value, *line, out)
            }
            Stmt::Return {
                value: Some(e),
                line,
            } => expr(e, *line, out),
            Stmt::Return { value: None, .. } => {}
            Stmt::If {
                cond,
                then_block,
                else_block,
                line,
            } => {
                expr(cond, *line, out);
                block(then_block, out);
                if let Some(eb) = else_block {
                    block(eb, out);
                }
            }
            Stmt::IfLet {
                scrutinee,
                then_block,
                else_block,
                line,
                ..
            } => {
                expr(scrutinee, *line, out);
                block(then_block, out);
                if let Some(eb) = else_block {
                    block(eb, out);
                }
            }
            Stmt::While { cond, body, line } => {
                expr(cond, *line, out);
                block(body, out);
            }
            Stmt::ForIn {
                iter, body, line, ..
            } => {
                expr(iter, *line, out);
                block(body, out);
            }
            Stmt::Drop { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::Expr(e) => expr(e, 0, out),
            Stmt::Region { body, .. } => block(body, out),
        }
    }
    fn block(b: &Block, out: &mut Sink) {
        for s in &b.stmts {
            stmt(s, out);
        }
    }
    fn expr(e: &Expr, line: usize, out: &mut Sink) {
        match e {
            Expr::Call { name, args, line } => {
                // `spawn` takes a bare identifier, so only a `Call` can carry a
                // namespace receiver — the same split `rewrite_expr` makes.
                let ns_receiver =
                    matches!(args.first(), Some(Expr::Var { name: h, .. }) if out.ns.contains(h));
                if !ns_receiver {
                    out.push((name.clone(), *line));
                }
                for a in args {
                    expr(a, *line, out);
                }
            }
            Expr::Spawn { name, args, line } => {
                out.push((name.clone(), *line));
                for a in args {
                    expr(a, *line, out);
                }
            }
            Expr::StructLit { name, fields, line } => {
                out.push((name.clone(), *line));
                for (_, v) in fields {
                    expr(v, *line, out);
                }
            }
            Expr::TryConstruct { name, args, line } => {
                out.push((name.clone(), *line));
                for a in args {
                    expr(a, *line, out);
                }
            }
            // A bare PascalCase variable may be a nullary variant constructor;
            // the visibility check resolves it via the variant map (plain
            // variables never appear there).
            Expr::Var { name, line } => out.push((name.clone(), *line)),
            Expr::Unary { expr: e2, .. } | Expr::Try { expr: e2, .. } => expr(e2, line, out),
            Expr::Field { expr: e2, .. } => expr(e2, line, out),
            Expr::Consume { place, .. } => expr(place, line, out),
            Expr::Binary { lhs, rhs, line, .. } => {
                expr(lhs, *line, out);
                expr(rhs, *line, out);
            }
            Expr::Match {
                scrutinee,
                arms,
                line,
            } => {
                expr(scrutinee, *line, out);
                for arm in arms {
                    if let Pattern::Variant(v, _) = &arm.pattern {
                        out.push((v.clone(), *line));
                    }
                    expr(&arm.body, *line, out);
                }
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                line,
            } => {
                expr(cond, *line, out);
                expr(then_branch, *line, out);
                if let Some(eb) = else_branch {
                    expr(eb, *line, out);
                }
            }
            Expr::ArrayLit { elems, line } => {
                for e2 in elems {
                    expr(e2, *line, out);
                }
            }
            Expr::MapLit { entries, line } => {
                for (k, v) in entries {
                    expr(k, *line, out);
                    expr(v, *line, out);
                }
            }
            // A lambda body (RFC-0023) references names too — walk it so a call
            // or constructor used only inside a lambda is still visibility-checked.
            Expr::Lambda { body, line, .. } => match body {
                LambdaBody::Expr(e2) => expr(e2, *line, out),
                LambdaBody::Block(b2) => block(b2, out),
            },
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
        }
    }
    block(b, &mut out);
    out.out
}

/// Scope-aware reference scan for the link-time visibility check: every name a
/// function references that could name a program-level declaration, MINUS any
/// name bound by a local in scope — params, `let`, `for`/lambda variables, and
/// match binds. A local shadows a like-named foreign export, so at that use site
/// it is never a cross-module reference: the flat namespace binds locals before
/// imports (RFC-0027, one level below imports). Type-position names (`let x: T`
/// annotations, and the caller's param/return/bound types) are always kept — a
/// value local never shadows a type. Unlike [`fn_body_names`], this seeds the
/// scope with the function's params, so a param that shadows a foreign export is
/// not mistaken for an un-imported reference.
fn fn_body_ref_names(f: &Function) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut locals: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
    scope_block(&f.body, &mut locals, &mut out);
    out
}

fn scope_block(b: &Block, locals: &mut HashSet<String>, out: &mut Vec<(String, usize)>) {
    for s in &b.stmts {
        scope_stmt(s, locals, out);
    }
}

fn scope_stmt(s: &Stmt, locals: &mut HashSet<String>, out: &mut Vec<(String, usize)>) {
    match s {
        Stmt::Let {
            name,
            value,
            ty,
            line,
            ..
        } => {
            if let Some(t) = ty {
                for n in type_names(t) {
                    out.push((n, *line));
                }
            }
            scope_expr(value, *line, locals, out);
            // In scope for subsequent statements (and shadows a like-named export
            // from here on).
            locals.insert(name.clone());
        }
        Stmt::Assign { value, line, .. } | Stmt::SetField { value, line, .. } => {
            scope_expr(value, *line, locals, out)
        }
        Stmt::IndexSet {
            index, value, line, ..
        } => {
            scope_expr(index, *line, locals, out);
            scope_expr(value, *line, locals, out);
        }
        Stmt::Return {
            value: Some(e),
            line,
        } => scope_expr(e, *line, locals, out),
        Stmt::Return { value: None, .. } => {}
        Stmt::If {
            cond,
            then_block,
            else_block,
            line,
        } => {
            scope_expr(cond, *line, locals, out);
            let mut inner = locals.clone();
            scope_block(then_block, &mut inner, out);
            if let Some(eb) = else_block {
                let mut inner2 = locals.clone();
                scope_block(eb, &mut inner2, out);
            }
        }
        Stmt::IfLet {
            scrutinee,
            then_block,
            else_block,
            pattern,
            line,
        } => {
            scope_expr(scrutinee, *line, locals, out);
            let mut inner = locals.clone();
            for b in crate::movecheck::pattern_bindings(pattern) {
                inner.insert(b.to_string());
            }
            scope_block(then_block, &mut inner, out);
            if let Some(eb) = else_block {
                let mut inner2 = locals.clone();
                scope_block(eb, &mut inner2, out);
            }
        }
        Stmt::While { cond, body, line } => {
            scope_expr(cond, *line, locals, out);
            let mut inner = locals.clone();
            scope_block(body, &mut inner, out);
        }
        Stmt::ForIn {
            var,
            iter,
            body,
            line,
            ..
        } => {
            scope_expr(iter, *line, locals, out);
            let mut inner = locals.clone();
            inner.insert(var.clone());
            scope_block(body, &mut inner, out);
        }
        Stmt::Drop { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Expr(e) => scope_expr(e, 0, locals, out),
        Stmt::Region { body, .. } => {
            let mut inner = locals.clone();
            scope_block(body, &mut inner, out);
        }
    }
}

fn scope_expr(e: &Expr, line: usize, locals: &HashSet<String>, out: &mut Vec<(String, usize)>) {
    match e {
        Expr::Call { name, args, line } | Expr::Spawn { name, args, line } => {
            if !locals.contains(name) {
                out.push((name.clone(), *line));
            }
            for a in args {
                scope_expr(a, *line, locals, out);
            }
        }
        Expr::StructLit { name, fields, line } => {
            if !locals.contains(name) {
                out.push((name.clone(), *line));
            }
            for (_, v) in fields {
                scope_expr(v, *line, locals, out);
            }
        }
        Expr::TryConstruct { name, args, line } => {
            if !locals.contains(name) {
                out.push((name.clone(), *line));
            }
            for a in args {
                scope_expr(a, *line, locals, out);
            }
        }
        Expr::Var { name, line } => {
            if !locals.contains(name) {
                out.push((name.clone(), *line));
            }
        }
        Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
            scope_expr(expr, line, locals, out)
        }
        Expr::Consume { place, .. } => scope_expr(place, line, locals, out),
        Expr::Binary { lhs, rhs, line, .. } => {
            scope_expr(lhs, *line, locals, out);
            scope_expr(rhs, *line, locals, out);
        }
        Expr::Match {
            scrutinee,
            arms,
            line,
        } => {
            scope_expr(scrutinee, *line, locals, out);
            for arm in arms {
                let mut inner = locals.clone();
                match &arm.pattern {
                    Pattern::Variant(v, binds) => {
                        // The variant constructor is a reference; its binds are new locals.
                        if !inner.contains(v) {
                            out.push((v.clone(), *line));
                        }
                        for b in binds {
                            inner.insert(b.clone());
                        }
                    }
                    Pattern::Some(b)
                    | Pattern::Ok(b)
                    | Pattern::Err(b)
                    | Pattern::Success(b)
                    | Pattern::Failure(b) => {
                        inner.insert(b.clone());
                    }
                    Pattern::None => {}
                }
                scope_expr(&arm.body, *line, &inner, out);
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            line,
        } => {
            scope_expr(cond, *line, locals, out);
            scope_expr(then_branch, *line, locals, out);
            if let Some(eb) = else_branch {
                scope_expr(eb, *line, locals, out);
            }
        }
        Expr::ArrayLit { elems, line } => {
            for e2 in elems {
                scope_expr(e2, *line, locals, out);
            }
        }
        Expr::MapLit { entries, line } => {
            for (k, v) in entries {
                scope_expr(k, *line, locals, out);
                scope_expr(v, *line, locals, out);
            }
        }
        Expr::Lambda { params, body, line } => {
            let mut inner = locals.clone();
            for p in params {
                inner.insert(p.clone());
            }
            match body {
                LambdaBody::Expr(e2) => scope_expr(e2, *line, &inner, out),
                LambdaBody::Block(b2) => scope_block(b2, &mut inner, out),
            }
        }
        Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
    }
}

/// Every named/applied type mentioned anywhere inside `ty`.
fn type_names(ty: &Type) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(t: &Type, out: &mut Vec<String>) {
        match t {
            Type::Named(n) => out.push(n.clone()),
            Type::App(n, args) => {
                out.push(n.clone());
                for a in args {
                    walk(a, out);
                }
            }
            Type::Option(a)
            | Type::Array(a)
            | Type::Task(a)
            | Type::Stream(a)
            | Type::Partial(a)
            | Type::ArrayN(a, _)
            | Type::SmallArray(a, _) => walk(a, out),
            Type::Result(a, b) | Type::Merge(a, b) => {
                walk(a, out);
                walk(b, out);
            }
            Type::Omit(a, _) | Type::Pick(a, _) => walk(a, out),
            Type::Record(fs) => {
                for f in fs {
                    walk(&f.ty, out);
                }
            }
            Type::Enum(vs) => {
                for v in vs {
                    for p in &v.payload {
                        walk(p, out);
                    }
                }
            }
            // Stored function values (RFC-0037) and Maps carry decl references
            // in their component types too (RFC-0040 §2 exposed this).
            Type::Fn(params, ret) => {
                for p in params {
                    walk(p, out);
                }
                walk(ret, out);
            }
            Type::Map(k, v) => {
                walk(k, out);
                walk(v, out);
            }
            _ => {}
        }
    }
    walk(ty, &mut out);
    out
}

// ---- alias reference rewriting (RFC-0022) ---------------------------------
//
// Import aliasing (`import { X as Y } from M`) is resolved by rewriting, in the
// importing module's linked AST, every *reference* to the local name `Y` into
// the actual decl name it stands for. The rewrite runs on the merged program's
// per-module copies before flattening, so the checker/interp/codegen — which
// resolve by decl name in one flat namespace — never learn aliases exist. The
// unlinked root AST that the LSP indexes is untouched, so hover still sees `Y`.

/// A name→name substitution for references (`map.get(n)` or `n` unchanged).
fn ren<'a>(map: &'a HashMap<String, String>, n: &'a str) -> String {
    map.get(n).cloned().unwrap_or_else(|| n.to_string())
}

/// Rewrite every referenced type name in `ty` through `map`.
fn rewrite_type(ty: &mut Type, map: &HashMap<String, String>) {
    match ty {
        Type::Named(n) => *n = ren(map, n),
        Type::App(n, args) => {
            *n = ren(map, n);
            for a in args {
                rewrite_type(a, map);
            }
        }
        Type::Option(a)
        | Type::Array(a)
        | Type::Task(a)
        | Type::Stream(a)
        | Type::Partial(a)
        | Type::ArrayN(a, _)
        | Type::SmallArray(a, _)
        | Type::Omit(a, _)
        | Type::Pick(a, _) => rewrite_type(a, map),
        Type::Result(a, b) | Type::Merge(a, b) => {
            rewrite_type(a, map);
            rewrite_type(b, map);
        }
        Type::Record(fs) => {
            for f in fs {
                rewrite_type(&mut f.ty, map);
            }
        }
        Type::Enum(vs) => {
            for v in vs {
                for p in &mut v.payload {
                    rewrite_type(p, map);
                }
            }
        }
        // Stored function values (RFC-0037) and Map values reference decl names
        // too — a generated module's `fn(Validation<T>)` callback type or
        // `Map<String, fn(..)>` pending map must follow a co-naming/namespace
        // rename of `T` like every other position (RFC-0040 §2 exposed this).
        Type::Fn(params, ret) => {
            for p in params {
                rewrite_type(p, map);
            }
            rewrite_type(ret, map);
        }
        Type::Map(k, v) => {
            rewrite_type(k, map);
            rewrite_type(v, map);
        }
        _ => {}
    }
}

/// Rewrite every referenced name in `e` (call/spawn/struct-lit/try-construct
/// callees, bare variables, and match-variant constructors) through `map`.
///
/// `ns` holds the module's namespace-binding names (RFC-0027): a method-sugar
/// call whose receiver is a bare namespace (`ns.member(..)` arrives as
/// `Call { member, args: [Var(ns), ..] }`) is a NAMESPACE member reference that
/// pass 5 (the `NsResolver`) owns — the plain-name rewrite must leave its call
/// name alone, or a co-naming rename of a like-named local decl would corrupt
/// `ns.member` into `ns.renamed` before pass 5 can resolve it (RFC-0031 found
/// this via a thin contract delegating `store.getItem(..)` while a generated
/// module co-named `getItem`).
/// Rewrite every reference to a declaration name in `p` through `map`.
///
/// The plain-name half of the machinery above, exposed for [`crate::jsonenc`]:
/// generated encoder source spells the injected module's reserved names as
/// `VyrnRt_` placeholders (a `$` is unlexable, which is the point of it), and this
/// is the pass that folds them back. One rewriter, so a generated program and an
/// imported one resolve names by the same code.
pub(crate) fn rewrite_names(p: &mut Program, map: &HashMap<String, String>) {
    rewrite_module_refs(p, map, &HashSet::new());
}

fn rewrite_expr(e: &mut Expr, map: &HashMap<String, String>, ns: &HashSet<String>) {
    match e {
        Expr::Call { name, args, .. } => {
            let ns_receiver =
                matches!(args.first(), Some(Expr::Var { name: h, .. }) if ns.contains(h));
            if !ns_receiver {
                *name = ren(map, name);
            }
            for a in args {
                rewrite_expr(a, map, ns);
            }
        }
        Expr::Spawn { name, args, .. } | Expr::TryConstruct { name, args, .. } => {
            *name = ren(map, name);
            for a in args {
                rewrite_expr(a, map, ns);
            }
        }
        Expr::StructLit { name, fields, .. } => {
            *name = ren(map, name);
            for (_, v) in fields {
                rewrite_expr(v, map, ns);
            }
        }
        Expr::Var { name, .. } => *name = ren(map, name),
        Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
            rewrite_expr(expr, map, ns)
        }
        Expr::Consume { place, .. } => rewrite_expr(place, map, ns),
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_expr(lhs, map, ns);
            rewrite_expr(rhs, map, ns);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            rewrite_expr(scrutinee, map, ns);
            for arm in arms {
                if let Pattern::Variant(v, _) = &mut arm.pattern {
                    *v = ren(map, v);
                }
                rewrite_expr(&mut arm.body, map, ns);
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            rewrite_expr(cond, map, ns);
            rewrite_expr(then_branch, map, ns);
            if let Some(eb) = else_branch {
                rewrite_expr(eb, map, ns);
            }
        }
        Expr::ArrayLit { elems, .. } => {
            for e2 in elems {
                rewrite_expr(e2, map, ns);
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                rewrite_expr(k, map, ns);
                rewrite_expr(v, map, ns);
            }
        }
        // A lambda body (RFC-0023): rewrite referenced names inside it (its own
        // untyped params are locals, never in `map`, so blanket rewriting is safe).
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e2) => rewrite_expr(e2, map, ns),
            LambdaBody::Block(b2) => rewrite_block(b2, map, ns),
        },
        Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
    }
}

fn rewrite_block(b: &mut Block, map: &HashMap<String, String>, ns: &HashSet<String>) {
    for s in &mut b.stmts {
        rewrite_stmt(s, map, ns);
    }
}

fn rewrite_stmt(s: &mut Stmt, map: &HashMap<String, String>, ns: &HashSet<String>) {
    match s {
        Stmt::Let { value, ty, .. } => {
            if let Some(t) = ty {
                rewrite_type(t, map);
            }
            rewrite_expr(value, map, ns);
        }
        // The assignment TARGET is a reference too, not a declaration: module
        // state (RFC-0029) is a top-level decl, so a rename must reach `g = v`
        // exactly as it reaches the `g` reads (`Expr::Var` below). Missing these
        // left the write side naming a decl that no longer exists ("assignment
        // to unknown variable `filter`" once std/arrays' `filter` forced the
        // name-privacy rename of a same-named global).
        Stmt::Assign { name, value, .. } | Stmt::SetField { name, value, .. } => {
            *name = ren(map, name);
            rewrite_expr(value, map, ns);
        }
        Stmt::IndexSet {
            name, index, value, ..
        } => {
            *name = ren(map, name);
            rewrite_expr(index, map, ns);
            rewrite_expr(value, map, ns);
        }
        Stmt::Return { value: Some(e), .. } => rewrite_expr(e, map, ns),
        Stmt::Return { value: None, .. } => {}
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            rewrite_expr(cond, map, ns);
            rewrite_block(then_block, map, ns);
            if let Some(eb) = else_block {
                rewrite_block(eb, map, ns);
            }
        }
        Stmt::IfLet {
            scrutinee,
            then_block,
            else_block,
            ..
        } => {
            rewrite_expr(scrutinee, map, ns);
            rewrite_block(then_block, map, ns);
            if let Some(eb) = else_block {
                rewrite_block(eb, map, ns);
            }
        }
        Stmt::While { cond, body, .. } => {
            rewrite_expr(cond, map, ns);
            rewrite_block(body, map, ns);
        }
        Stmt::ForIn { iter, body, .. } => {
            rewrite_expr(iter, map, ns);
            rewrite_block(body, map, ns);
        }
        // `drop g` names a binding the same way — same rule as the target above.
        Stmt::Drop { name, .. } => *name = ren(map, name),
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Expr(e) => rewrite_expr(e, map, ns),
        Stmt::Region { body, .. } => rewrite_block(body, map, ns),
    }
}

/// Rewrite one function's signature types and body references through `map`.
fn rewrite_function(f: &mut Function, map: &HashMap<String, String>, ns: &HashSet<String>) {
    for p in &mut f.params {
        rewrite_type(&mut p.ty, map);
    }
    rewrite_type(&mut f.ret, map);
    // A `<T: P>` bound naming an aliased protocol resolves through `map` too.
    for bounds in f.type_bounds.values_mut() {
        for b in bounds.iter_mut() {
            *b = ren(map, b);
        }
    }
    rewrite_block(&mut f.body, map, ns);
}

/// Rewrite every *reference* (types, calls, variables, bounds) in one module's
/// program through `map`. Declaration names are left alone — a separate step
/// renames a decl when a foreign name must be freed for a co-named local stub.
/// `ns` is the module's namespace-binding names (see [`rewrite_expr`]).
fn rewrite_module_refs(p: &mut Program, map: &HashMap<String, String>, ns: &HashSet<String>) {
    if map.is_empty() {
        return;
    }
    for f in &mut p.functions {
        rewrite_function(f, map, ns);
    }
    for im in &mut p.impls {
        im.protocol = ren(map, &im.protocol);
        rewrite_type(&mut im.ty, map);
        for m in &mut im.methods {
            rewrite_function(m, map, ns);
        }
    }
    for t in &mut p.type_decls {
        rewrite_type(&mut t.base, map);
        if let Some(pred) = &mut t.predicate {
            rewrite_expr(pred, map, ns);
        }
    }
    for g in &mut p.globals {
        if let Some(t) = &mut g.ty {
            rewrite_type(t, map);
        }
        rewrite_expr(&mut g.init, map, ns);
    }
    for pr in &mut p.protocols {
        for m in &mut pr.methods {
            for t in &mut m.params {
                rewrite_type(t, map);
            }
            rewrite_type(&mut m.ret, map);
        }
    }
    // A contract member's types name declarations too (RFC-0071), so an alias
    // or a co-naming rename must reach them like any other signature.
    for c in &mut p.contracts {
        for m in &mut c.members {
            match &mut m.kind {
                crate::ast::ContractMemberKind::Value { ty, default } => {
                    rewrite_type(ty, map);
                    if let Some(d) = default {
                        rewrite_expr(d, map, ns);
                    }
                }
                crate::ast::ContractMemberKind::Fn {
                    params,
                    ret,
                    default,
                    ..
                } => {
                    for t in params {
                        rewrite_type(t, map);
                    }
                    rewrite_type(ret, map);
                    if let Some(d) = default {
                        rewrite_expr(d, map, ns);
                    }
                }
            }
        }
    }
    for t in &mut p.tests {
        rewrite_block(&mut t.body, map, ns);
    }
    for b in &mut p.benches {
        rewrite_block(&mut b.body, map, ns);
    }
}

/// Every reference name (types and expression callees/variables/variants) used
/// anywhere in a module's declarations — for the RFC-0022 check that an aliased
/// import's original name is not also used directly.
fn program_ref_names(p: &Program) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    // The module's own namespace bindings, read off its imports rather than
    // passed in: both callers hold only a `Program`, and `ns.member` is spelled
    // against the namespaces this file declares.
    let ns: HashSet<String> = p
        .imports
        .iter()
        .filter_map(|i| i.namespace.clone())
        .collect();
    let add_block = |b: &Block, out: &mut HashSet<String>| {
        for (n, _) in fn_body_names(b, &ns) {
            out.insert(n);
        }
    };
    let add_type = |t: &Type, out: &mut HashSet<String>| {
        for n in type_names(t) {
            out.insert(n);
        }
    };
    for f in &p.functions {
        for pm in &f.params {
            add_type(&pm.ty, &mut out);
        }
        add_type(&f.ret, &mut out);
        add_block(&f.body, &mut out);
    }
    for im in &p.impls {
        out.insert(im.protocol.clone());
        add_type(&im.ty, &mut out);
        for m in &im.methods {
            for pm in &m.params {
                add_type(&pm.ty, &mut out);
            }
            add_type(&m.ret, &mut out);
            add_block(&m.body, &mut out);
        }
    }
    for t in &p.type_decls {
        add_type(&t.base, &mut out);
    }
    for g in &p.globals {
        if let Some(t) = &g.ty {
            add_type(t, &mut out);
        }
    }
    for t in &p.tests {
        add_block(&t.body, &mut out);
    }
    for b in &p.benches {
        add_block(&b.body, &mut out);
    }
    out
}

/// Rename a top-level *declaration* (its defining name) from `from` to `to` in
/// module `p`, and rewrite that module's own references to it. Used to free a
/// foreign name so a co-naming importer's stub can take it (RFC-0022).
fn rename_decl_in_module(p: &mut Program, from: &str, to: &str, ns: &HashSet<String>) {
    for t in &mut p.type_decls {
        if t.name == from {
            t.name = to.to_string();
        }
    }
    for f in &mut p.functions {
        if f.name == from {
            f.name = to.to_string();
        }
    }
    for pr in &mut p.protocols {
        if pr.name == from {
            pr.name = to.to_string();
        }
    }
    for c in &mut p.contracts {
        if c.name == from {
            c.name = to.to_string();
        }
    }
    for g in &mut p.globals {
        if g.name == from {
            g.name = to.to_string();
        }
    }
    let map: HashMap<String, String> =
        std::iter::once((from.to_string(), to.to_string())).collect();
    rewrite_module_refs(p, &map, ns);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`RT_MODULES`] writes each route's reserved name out in full so
    /// [`routed_builtin`] can return a `&'static str` without composing one on a
    /// path every call expression takes. That redundancy has to be checked, or a
    /// row could name a spelling the decl rename never produces — a call to a
    /// function nobody defines, which every engine would then refuse at the worst
    /// possible moment. Also: no builtin may be claimed by two modules.
    #[test]
    fn every_route_is_spelled_with_its_modules_prefix() {
        let mut seen: Vec<&str> = Vec::new();
        for rt in RT_MODULES {
            assert!(rt.prefix.ends_with('$'), "`{}` must end in `$`", rt.prefix);
            for (builtin, reserved) in rt.routes {
                assert_eq!(
                    *reserved,
                    format!("{}{}", rt.prefix, reserved.trim_start_matches(rt.prefix)),
                    "`{builtin}` names `{reserved}`, which is not `{}`-prefixed",
                    rt.prefix
                );
                assert!(
                    reserved.starts_with(rt.prefix),
                    "`{reserved}` vs `{}`",
                    rt.prefix
                );
                assert!(!seen.contains(builtin), "`{builtin}` is routed twice");
                seen.push(builtin);
                assert_eq!(routed_builtin(builtin), Some(*reserved));
            }
            for b in rt.desugared {
                assert!(
                    routed_builtin(b).is_none(),
                    "`{b}` is a desugar, not a route"
                );
            }
        }
        // A route is matched on the CALL NAME, before any type is known, so it
        // only means the builtin while no declaration may carry the name. This
        // is the check `movecheck::every_view_and_sink_name_is_reserved` makes
        // for its list and `parser::every_method_builtin_is_reserved_or_shadowable`
        // makes for its own — the same hazard, a third pass. A route spelled
        // with the method-form `@` prefix is unspellable and needs no guard.
        for rt in RT_MODULES {
            for (builtin, _) in rt.routes {
                assert!(
                    builtin.starts_with('@') || crate::checker::RESERVED.contains(builtin),
                    "`{builtin}` is routed to a std function but is not reserved, so a \
                     user declaration of that name would be silently unreachable"
                );
            }
        }
        assert!(routed_builtin("print").is_none());
        assert!(
            routed_builtin("lineAt").is_none(),
            "`lineAt` keeps its interpreter cache"
        );
        // RFC-0079 M3: `slice` was the one refusal on this list that a language
        // change could retire, and it did. The row is inverted rather than deleted
        // — the reason it moved is the milestone.
        assert_eq!(
            routed_builtin("slice"),
            Some("strpred$sliceV"),
            "`slice` returns its failure now; RFC-0079 M3 routed it"
        );
    }

    /// RFC-0081 M2: [`F64_STR`] is a name two backends emit a call to, so the
    /// module it names has to be in the table and its prefix has to be the one it
    /// is spelled with. Neither backend can check that for itself — a mismatch is
    /// a link error in a program that formats a float, which is most of them.
    #[test]
    fn the_float_formatter_is_std_nums() {
        let num = RT_MODULES
            .iter()
            .find(|rt| rt.spec == "std/num")
            .expect("std/num is linked");
        assert!(
            F64_STR.starts_with(num.prefix),
            "`{F64_STR}` vs `{}`",
            num.prefix
        );
        // Both spellings that reach it, and neither is a route: the float case is
        // one case of a type-directed builtin.
        assert!(num.desugared.contains(&"@str") && num.desugared.contains(&"print"));
        assert!(routed_builtin(F64_STR).is_none());
    }

    pub(super) fn map(entries: &[(&str, &str)]) -> MapResolver {
        MapResolver(
            entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    pub(super) fn opts() -> LoadOptions {
        LoadOptions {
            std_root: Some("std".into()),
            ..Default::default()
        }
    }

    /// Every runtime module a builtin can inject (RFC-0078), plus the modules those
    /// import. Added to every `run_multi` resolver rather than per test: injection is
    /// conditional on the mention, so a program that never says `fromJson` links none
    /// of them, and a test that does say it should not have to know which files that
    /// implies.
    const RT_FILES: &[(&str, &str)] = &[
        ("std/json.vyrn", include_str!("../../../std/json.vyrn")),
        (
            "std/jsonread.vyrn",
            include_str!("../../../std/jsonread.vyrn"),
        ),
        (
            "std/jsondec.vyrn",
            include_str!("../../../std/jsondec.vyrn"),
        ),
        ("std/num.vyrn", include_str!("../../../std/num.vyrn")),
        ("std/codecs.vyrn", include_str!("../../../std/codecs.vyrn")),
        ("std/text.vyrn", include_str!("../../../std/text.vyrn")),
        (
            "std/strpred.vyrn",
            include_str!("../../../std/strpred.vyrn"),
        ),
    ];

    fn run_multi(root: &str, files: &[(&str, &str)]) -> Result<i64, String> {
        let files: Vec<(&str, &str)> = files
            .iter()
            .copied()
            .chain(RT_FILES.iter().copied())
            .collect();
        let files = &files[..];
        let mut program = load(root, "main.vyrn", &opts(), &map(files)).map_err(|ds| {
            ds.iter().map(|d| d.render()).collect::<Vec<_>>().join(
                "
",
            )
        })?;
        // `check_and_synthesize` rather than a bare check: since RFC-0078 M2b/M3 a
        // linked program is not runnable until the JSON builtins' generated Vyrn is
        // in it, and `loader::load` deliberately stops at the link. A test that ran
        // the bare check would fail at the call site with "no decoder", which is a
        // true statement about a program nobody finished assembling.
        let diags = crate::check_and_synthesize(&mut program);
        if let Some(d) = diags.first() {
            return Err(d.render());
        }
        crate::interp::run(&program)
    }

    fn load_err(root: &str, files: &[(&str, &str)]) -> String {
        match load(root, "main.vyrn", &opts(), &map(files)) {
            Ok(_) => panic!("expected a load error"),
            Err(ds) => ds
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    #[test]
    fn imports_functions_and_types_across_modules() {
        let lib = "export fn double(x: Int64) -> Int64 { return x * 2 } \
                   export type Age = Int64 where value >= 18 \
                   fn hidden() -> Int64 { return 0 }";
        let root = "import { double, Age } from \"./lib\" \
                    fn main() -> Int64 { let a: Age = 21 return double(a) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 42);
    }

    #[test]
    fn import_alias_resolves_to_the_original_decl() {
        // RFC-0022: `getUser as fetchUser` — the alias is the local name and
        // resolves to the original function/type in the flat namespace.
        let lib = "export fn getUser(id: Int64) -> Int64 { return id * 10 } \
                   export type Age = Int64 where value >= 0";
        let root = "import { getUser as fetchUser, Age as Years } from \"./lib\" \
                    fn main() -> Int64 { let y: Years = 3 return fetchUser(y) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 30);
    }

    #[test]
    fn import_alias_hides_the_original_name() {
        // The original name is not brought into scope by an aliased import.
        let lib = "export fn getUser(id: Int64) -> Int64 { return id }";
        let root = "import { getUser as fetchUser } from \"./lib\" \
                    fn main() -> Int64 { return getUser(1) }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("getUser"), "{e}");
    }

    #[test]
    fn import_alias_clashing_with_a_local_decl_is_an_error() {
        let lib = "export fn getUser(id: Int64) -> Int64 { return id }";
        let root = "import { getUser as fetchUser } from \"./lib\" \
                    fn fetchUser() -> Int64 { return 0 } \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("clashes with a top-level declaration"), "{e}");
    }

    #[test]
    fn import_alias_lets_a_stub_share_the_real_name() {
        // The co-naming (RPC stub) pattern: the importing module defines its own
        // `getUser`, importing the real one under an alias it forwards to.
        let lib = "export fn getUser(id: Int64) -> Int64 { return id * 100 }";
        let root = "import { getUser as getUserReal } from \"./lib\" \
                    fn getUser(id: Int64) -> Int64 { return getUserReal(id) + 1 } \
                    fn main() -> Int64 { return getUser(2) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 201);
    }

    #[test]
    fn aliased_enum_import_brings_variants_under_own_names() {
        // Importing an enum under an alias still brings its variants by their
        // own (unaliased) names (RFC-0022).
        let lib = "export type Color = | Red | Green | Blue";
        let root = "import { Color as Hue } from \"./lib\" \
                    fn pick(h: Hue) -> Int64 { return match h { Red => 1, Green => 2, Blue => 3 } } \
                    fn main() -> Int64 { let c: Hue = Green return pick(c) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 2);
    }

    #[test]
    fn validated_type_auto_validates_across_modules() {
        let lib = "export type Age = Int64 where value >= 18";
        let root = "import { Age } from \"./lib\" \
                    fn mk(n: Int64) -> Age { return n } \
                    fn main() -> Int64 { let a = mk(5) return 0 }";
        let e = run_multi(root, &[("lib.vyrn", lib)]).unwrap_err();
        assert!(e.contains("validation failed for `Age`"), "{e}");
    }

    #[test]
    fn importing_a_private_name_is_an_error() {
        let lib = "fn secret() -> Int64 { return 1 }";
        let root = "import { secret } from \"./lib\" \
                    fn main() -> Int64 { return secret() }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("not exported"), "{e}");
    }

    #[test]
    fn importing_a_missing_name_is_an_error() {
        let root = "import { nope } from \"./lib\" \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[("lib.vyrn", "export fn f() -> Int64 { return 1 }")]);
        assert!(e.contains("does not define `nope`"), "{e}");
    }

    #[test]
    fn using_a_foreign_name_without_importing_it_is_an_error() {
        // `helper` exists (exported, even) in lib, but main never imported it.
        let lib = "export fn helper() -> Int64 { return 1 } \
                   export fn wanted() -> Int64 { return 2 }";
        let root = "import { wanted } from \"./lib\" \
                    fn main() -> Int64 { return wanted() + helper() }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("not imported here"), "{e}");
    }

    #[test]
    fn std_result_and_option_imports_are_validated_noops() {
        // RFC-0062: importing the ambient builtins by name from `std/result` /
        // `std/option` is a no-op — no file is loaded, the names keep resolving
        // to the builtins, and the program runs exactly as it would ambiently.
        let root = "import { Result, Ok, Err } from \"std/result\" \
                    import { Option, Some, None } from \"std/option\" \
                    fn find(x: Int64) -> Result<Int64, String> { \
                        if x > 0 { return Ok(x) } return Err(\"neg\") } \
                    fn opt(x: Bool) -> Option<Int64> { \
                        if x { return Some(7) } return None } \
                    fn main() -> Int64 { \
                        let r = match find(5) { Ok(v) => v, Err(e) => 0 } \
                        let o = match opt(true) { Some(n) => n, None => 0 } \
                        return r + o }";
        assert_eq!(run_multi(root, &[]).unwrap(), 12);
    }

    #[test]
    fn std_result_ambient_use_without_the_import_still_works() {
        // The import is opt-in style, not a requirement: the same program runs
        // without importing the names (they were always ambient).
        let root = "fn find(x: Int64) -> Result<Int64, String> { \
                        if x > 0 { return Ok(x) } return Err(\"neg\") } \
                    fn main() -> Int64 { return match find(5) { Ok(v) => v, Err(e) => 0 } }";
        assert_eq!(run_multi(root, &[]).unwrap(), 5);
    }

    #[test]
    fn std_result_unknown_export_is_rejected() {
        let root = "import { Result, Foo } from \"std/result\" \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[]);
        assert!(e.contains("std/result has no export `Foo`"), "{e}");
    }

    #[test]
    fn std_option_rejects_a_result_only_export() {
        // Each module's export list is fixed and distinct — `Result` is not an
        // export of `std/option`.
        let root = "import { Option, Result } from \"std/option\" \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[]);
        assert!(e.contains("std/option has no export `Result`"), "{e}");
    }

    #[test]
    fn std_result_namespace_import_is_rejected() {
        // `import * as r from "std/result"` would create a second spelling
        // (`r.Ok`) for a builtin — rejected.
        let root = "import * as r from \"std/result\" \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[]);
        assert!(e.contains("cannot be imported as a namespace"), "{e}");
    }

    #[test]
    fn import_cycles_are_errors() {
        let a = "import { b } from \"./b\" export fn a() -> Int64 { return 1 }";
        let b = "import { a } from \"./a\" export fn b() -> Int64 { return 2 }";
        let root = "import { a } from \"./a\" fn main() -> Int64 { return a() }";
        let e = load_err(root, &[("a.vyrn", a), ("b.vyrn", b)]);
        assert!(e.contains("import cycle"), "{e}");
    }

    #[test]
    fn cross_module_name_collisions_are_errors() {
        let a = "export fn f() -> Int64 { return 1 }";
        let b = "export fn f() -> Int64 { return 2 }";
        let root = "import { f } from \"./a\" \
                    import { f } from \"./b\" \
                    fn main() -> Int64 { return f() }";
        let e = load_err(root, &[("a.vyrn", a), ("b.vyrn", b)]);
        assert!(
            e.contains("`f` is declared by both `a.vyrn` and `b.vyrn`"),
            "{e}"
        );
    }

    #[test]
    fn a_module_pair_collision_is_one_error_that_names_the_fix() {
        // Two modules sharing top-level names is ONE mistake. It used to be
        // reported once per shared name — including names the user never wrote —
        // at the foreign declaration's line, against the root file, and then a
        // fourth time as "`f` is not defined in `b.vyrn`", which is false.
        let a = "export fn f() -> Int64 { return 1 } \
                 export fn g() -> Int64 { return 1 }";
        let b = "export fn f() -> Int64 { return 2 } \
                 export fn g() -> Int64 { return 2 }";
        let root = "import { f } from \"./a\" \n\
                    import { f } from \"./b\" \n\
                    fn main() -> Int64 { return f() }";
        let ds = match load(
            root,
            "main.vyrn",
            &opts(),
            &map(&[("a.vyrn", a), ("b.vyrn", b)]),
        ) {
            Ok(_) => panic!("expected a load error"),
            Err(ds) => ds,
        };
        let all = ds
            .iter()
            .map(|d| format!("{:?} {} {}", d.file, d.line, d.message))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(ds.len(), 1, "{all}");
        let d = &ds[0];
        // The user's own file, at the import they wrote — never a line borrowed
        // from the module the name collided with.
        assert_eq!(d.file, None, "{all}");
        assert_eq!(d.line, 2, "{all}");
        assert!(d.message.contains("`f` is declared by both"), "{all}");
        let note = d.note.as_deref().unwrap_or("");
        assert!(note.contains("import * as b from \"./b\""), "{note}");
        // `g` collides too, but the user never wrote it: a note, not an error.
        assert!(note.contains("`g` collides the same way"), "{note}");
        assert!(!all.contains("is not defined in"), "{all}");
        assert!(!all.contains("imported twice"), "{all}");
    }

    #[test]
    fn the_same_module_imported_twice_still_says_so() {
        // The suppression above is only for the same name from DIFFERENT modules
        // (which the pair collision covers). A genuine double binding still errors.
        let a = "export fn f() -> Int64 { return 1 } export fn g() -> Int64 { return 2 }";
        let root = "import { f } from \"./a\" \
                    import { f } from \"./a\" \
                    fn main() -> Int64 { return f() }";
        let e = load_err(root, &[("a.vyrn", a)]);
        assert!(e.contains("`f` is imported twice"), "{e}");
        let root = "import { f as x } from \"./a\" \
                    import { g as x } from \"./a\" \
                    fn main() -> Int64 { return x() }";
        let e = load_err(root, &[("a.vyrn", a)]);
        assert!(e.contains("`x` is imported twice"), "{e}");
    }

    #[test]
    fn a_collision_the_user_did_not_import_is_still_one_error() {
        // Neither name is imported from both modules, so nothing is "imported
        // twice" — but the flat namespace still cannot hold two `g`s, and the
        // user has to hear about it exactly once, with the fix.
        let a = "export fn f() -> Int64 { return 1 } \
                 export fn g() -> Int64 { return 1 }";
        let b = "export fn h() -> Int64 { return 2 } \
                 export fn g() -> Int64 { return 2 }";
        let root = "import { f } from \"./a\" \n\
                    import { h } from \"./b\" \n\
                    fn main() -> Int64 { return f() + h() }";
        let ds = match load(
            root,
            "main.vyrn",
            &opts(),
            &map(&[("a.vyrn", a), ("b.vyrn", b)]),
        ) {
            Ok(_) => panic!("expected a load error"),
            Err(ds) => ds,
        };
        assert_eq!(ds.len(), 1, "{ds:?}");
        assert!(ds[0].message.contains("`g` is declared by both"), "{ds:?}");
    }

    #[test]
    fn a_namespace_call_is_not_a_bare_use_of_an_aliased_original() {
        // An aliased import hides the original name, and using it directly is an
        // error. `bt.routes()` is not that use: it is a namespace call to another
        // module entirely, which the method sugar parses as `routes(bt)` — and the
        // check counted that member name as a bare reference. The advice it gave
        // (`use pageRoutes`) would have produced `bt.pageRoutes()`, which names
        // nothing.
        let a = "export fn route() -> Int64 { return 1 } \
                 export fn routes() -> Int64 { return 2 }";
        let b = "export fn routes() -> Int64 { return 3 }";
        let root = "import { route, routes as pageRoutes } from \"./a\" \
                    import * as bt from \"./b\" \
                    fn main() -> Int64 { return route() + pageRoutes() + bt.routes() }";
        assert_eq!(run_multi(root, &[("a.vyrn", a), ("b.vyrn", b)]), Ok(6));
    }

    #[test]
    fn a_real_bare_use_of_an_aliased_original_is_still_reported() {
        // The other direction: the namespace call must not SATISFY the check
        // either. `routes()` here is the hidden name, written bare, and it is
        // still an error however many namespace calls share its spelling.
        let a = "export fn routes() -> Int64 { return 2 }";
        let b = "export fn routes() -> Int64 { return 3 }";
        let root = "import { routes as pageRoutes } from \"./a\" \
                    import * as bt from \"./b\" \
                    fn main() -> Int64 { return routes() + bt.routes() }";
        let e = load_err(root, &[("a.vyrn", a), ("b.vyrn", b)]);
        assert!(
            e.contains("`routes` is not in scope — it was imported as `pageRoutes`"),
            "{e}"
        );
        // One cause, one error: the collision diagnostics next door do not pile on.
        assert!(!e.contains("is declared by both"), "{e}");
    }

    #[test]
    fn a_namespace_import_resolves_the_collision() {
        // The fix the diagnostic names has to actually work.
        let a = "export fn f() -> Int64 { return 1 }";
        let b = "export fn f() -> Int64 { return 2 }";
        let root = "import { f } from \"./a\" \
                    import * as b from \"./b\" \
                    fn main() -> Int64 { return f() + b.f() }";
        assert_eq!(run_multi(root, &[("a.vyrn", a), ("b.vyrn", b)]).unwrap(), 3);
    }

    #[test]
    fn importing_an_enum_brings_its_variants() {
        let lib = "export type Shape = | Circle(Int64) | Dot \
                   export fn area(s: Shape) -> Int64 { \
                       return match s { Circle(r) => 3 * r * r, Dot => 0 } }";
        let root = "import { Shape, area } from \"./lib\" \
                    fn main() -> Int64 { return area(Circle(2)) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 12);
    }

    #[test]
    fn importing_a_protocol_brings_its_methods() {
        let lib = "export protocol Loud { fn shout(self) -> Int64 } \
                   impl Loud for Int64 { fn shout(self) -> Int64 { return self * 10 } }";
        let root = "import { Loud } from \"./lib\" \
                    fn main() -> Int64 { return 4.shout() }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 40);
    }

    #[test]
    fn std_prefix_resolves_against_the_std_root() {
        let m = "export fn twice(x: Int64) -> Int64 { return x + x }";
        let root = "import { twice } from \"std/math\" \
                    fn main() -> Int64 { return twice(21) }";
        assert_eq!(run_multi(root, &[("std/math.vyrn", m)]).unwrap(), 42);
    }

    #[test]
    fn transitive_imports_load_once() {
        // Both a and b import shared; the diamond loads it once (no collision
        // with itself).
        let shared = "export fn one() -> Int64 { return 1 }";
        let a = "import { one } from \"./shared\" export fn a() -> Int64 { return one() + 10 }";
        let b = "import { one } from \"./shared\" export fn b() -> Int64 { return one() + 20 }";
        let root = "import { a } from \"./a\" \
                    import { b } from \"./b\" \
                    fn main() -> Int64 { return a() + b() }";
        assert_eq!(
            run_multi(
                root,
                &[("shared.vyrn", shared), ("a.vyrn", a), ("b.vyrn", b)]
            )
            .unwrap(),
            32
        );
    }

    #[test]
    fn non_root_logging_config_is_an_error() {
        let lib = "logging { level: trace } export fn f() -> Int64 { return 1 }";
        let root = "import { f } from \"./lib\" fn main() -> Int64 { return f() }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(
            e.contains("only the root module may configure `logging"),
            "{e}"
        );
    }

    #[test]
    fn non_root_module_state_is_legal_via_accessors() {
        // RFC-0029: a top-level `let` is legal in any module; cross-module
        // access goes through exported accessor functions. The imported module
        // owns `count`; the root reads it through `f`.
        let lib = "let mut count = 7 export fn f() -> Int64 { return count }";
        let root = "import { f } from \"./lib\" fn main() -> Int64 { return f() }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 7);
    }

    #[test]
    fn diamond_imports_share_one_state_instance() {
        // RFC-0029: `left` and `right` both import the same `store`; the loader
        // resolves them to ONE module identity, so both mutate the single shared
        // `count`. The root observes 2 — a single instance across the diamond.
        let store = "let mut count: Int64 = 0 \
                     export fn tally() -> Int64 { return count } \
                     export fn bump() { count = count + 1 }";
        let left = "import { bump } from \"./store\" export fn l() { bump() }";
        let right = "import { bump } from \"./store\" export fn r() { bump() }";
        let root = "import { tally } from \"./store\" \
                    import { l } from \"./left\" import { r } from \"./right\" \
                    fn main() -> Int64 { l() r() return tally() }";
        assert_eq!(
            run_multi(
                root,
                &[
                    ("store.vyrn", store),
                    ("left.vyrn", left),
                    ("right.vyrn", right)
                ]
            )
            .unwrap(),
            2
        );
    }

    #[test]
    fn initializer_may_read_imported_module_state() {
        // RFC-0029: an initializer may call an imported accessor — the imported
        // module initializes first (post-order), so its state is already set.
        let store = "let seed: Int64 = 41 export fn seedVal() -> Int64 { return seed }";
        let root = "import { seedVal } from \"./store\" \
                    let snapshot: Int64 = seedVal() + 1 \
                    fn main() -> Int64 { return snapshot }";
        assert_eq!(run_multi(root, &[("store.vyrn", store)]).unwrap(), 42);
    }

    #[test]
    fn spawning_a_cross_module_stateful_fn_is_refused() {
        // RFC-0029 keeps RFC-0013's spawn isolation module-agnostic: a function
        // reaching ANY module's state (here the imported store's) is not
        // spawn-safe, so spawning it is refused.
        let store = "let mut count: Int64 = 0 \
                     export fn bump() -> Int64 { count = count + 1 return count }";
        let root = "import { bump } from \"./store\" \
                    fn worker() -> Int64 { return bump() } \
                    fn main() -> Int64 { let h = spawn worker() return h.join() }";
        let e = run_multi(root, &[("store.vyrn", store)]).unwrap_err();
        assert!(
            e.contains("is not allowed") && e.contains("isolated"),
            "{e}"
        );
    }

    #[test]
    fn two_modules_with_a_private_same_named_helper_link_cleanly() {
        // RFC-0046 §3: a non-exported decl is invisible outside its module, so
        // two modules may each carry a private `helper` without colliding — the
        // linker auto-renames the non-exported decls.
        let a = "fn helper() -> Int64 { return 1 } \
                 export fn aVal() -> Int64 { return helper() }";
        let b = "fn helper() -> Int64 { return 2 } \
                 export fn bVal() -> Int64 { return helper() }";
        let root = "import { aVal } from \"./a\" \
                    import { bVal } from \"./b\" \
                    fn main() -> Int64 { return aVal() + bVal() }";
        assert_eq!(run_multi(root, &[("a.vyrn", a), ("b.vyrn", b)]).unwrap(), 3);
    }

    #[test]
    fn local_may_shadow_a_private_std_internal_name() {
        // RFC-0046 §3 (the vlog `pad2` bug): `std/time`'s private `pad2` forced a
        // consumer to rename its own `pad2`. A non-exported foreign name no longer
        // consumes the consumer's namespace — the local `pad2` compiles unchanged,
        // and each module's `pad2` resolves to its own.
        let lib = "fn pad2(n: Int64) -> Int64 { return n } \
                   export fn tick() -> Int64 { return pad2(7) }";
        let root = "import { tick } from \"./lib\" \
                    fn pad2(n: Int64) -> Int64 { return n + 100 } \
                    fn main() -> Int64 { return tick() + pad2(0) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 7 + 100);
    }

    #[test]
    fn module_state_assignment_survives_a_never_imported_foreign_namesake() {
        // The shelf `filter` bug: `arrays` exports `filter`, `ui` imports only
        // `includes` from it, so `arrays::filter` is LINKED but never imported
        // here — enough to force the name-privacy rename of this module's
        // same-named state. The rename has to reach the assignment TARGETS, not
        // just the reads, or the write side names a decl that no longer exists.
        let arrays = "export fn filter() -> Int64 { return 99 } \
                      export fn includes() -> Int64 { return 1 }";
        let ui = "import { includes } from \"./arrays\" \
                  export fn tag() -> Int64 { return includes() }";
        let root = "import { tag } from \"./ui\" \
                    let mut filter: Int64 = 0 \
                    let mut includes: Array<Int64> = [0] \
                    fn main() -> Int64 { \
                        filter = 7 \
                        includes[0] = 2 \
                        return filter + includes[0] + tag() \
                    }";
        assert_eq!(
            run_multi(root, &[("arrays.vyrn", arrays), ("ui.vyrn", ui)]).unwrap(),
            7 + 2 + 1
        );
    }

    #[test]
    fn global_name_collides_with_a_function() {
        // A global may not share a name with any other top-level declaration.
        let lib = "export fn tally() -> Int64 { return 1 }";
        let root = "import { tally } from \"./lib\" \
                    let tally = 0 \
                    fn main() -> Int64 { return tally }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("`tally` is declared by both"), "{e}");
    }

    // ---- flat-namespace local shadowing (dogfood BUG 2) ------------------
    // A local/param/loop/lambda/match binding whose name equals ANOTHER linked
    // module's export must never be mis-resolved as an un-imported foreign
    // reference. The visibility scan is scope-aware; locals bind before imports.

    #[test]
    fn local_let_shadows_a_foreign_export_of_the_same_name() {
        // The shelf shape: module `ui` has a local `t`; module `strings` exports
        // `t`. Both are linked (root imports from each). `ui`'s local `t` is NOT
        // a reference to `strings`'s `t`.
        let strings = "export fn t() -> Int64 { return 99 } \
                       export fn label() -> Int64 { return 1 }";
        let ui = "import { label } from \"./strings\" \
                  export fn render() -> Int64 { let t = 5 return t + label() }";
        let root = "import { render } from \"./ui\" \
                    import { t } from \"./strings\" \
                    fn main() -> Int64 { return render() + t() }";
        assert_eq!(
            run_multi(root, &[("strings.vyrn", strings), ("ui.vyrn", ui)]).unwrap(),
            5 + 1 + 99
        );
    }

    #[test]
    fn param_shadows_a_foreign_global_of_the_same_name() {
        // The shelf `loc` shape: a generated/library fn's PARAM `loc` shadows the
        // root's module-state global `loc`.
        let lib = "export fn greet(loc: Int64) -> Int64 { return loc + 1 }";
        let root = "import { greet } from \"./lib\" \
                    let mut loc = 10 \
                    fn main() -> Int64 { return greet(loc) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 11);
    }

    #[test]
    fn for_loop_var_shadows_a_foreign_export() {
        // The `std/ui` loop-var `t` shape.
        let strings = "export fn t() -> Int64 { return 7 }";
        let lib = "export fn total(xs: Array<Int64>) -> Int64 { \
                       let mut sum = 0 for t in xs { sum = sum + t } return sum }";
        let root = "import { total } from \"./lib\" \
                    import { t } from \"./strings\" \
                    fn main() -> Int64 { let xs: Array<Int64> = [1, 2, 3] return total(xs) + t() }";
        assert_eq!(
            run_multi(root, &[("strings.vyrn", strings), ("lib.vyrn", lib)]).unwrap(),
            6 + 7
        );
    }

    #[test]
    fn match_bind_shadows_a_foreign_export() {
        let lib = "export fn why() -> Int64 { return 100 }";
        let root = "import { why } from \"./lib\" \
                    fn pick(x: Result<Int64, Int64>) -> Int64 { \
                        return match x { Ok(why) => why, Err(e) => e } } \
                    fn main() -> Int64 { return pick(Ok(3)) + why() }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 3 + 100);
    }

    #[test]
    fn a_genuinely_unimported_foreign_name_still_errors() {
        // Guard against over-fixing: a bare use that is NOT shadowed by any local
        // must still be flagged.
        let lib = "export fn helper() -> Int64 { return 1 } \
                   export fn wanted() -> Int64 { return 2 }";
        let root = "import { wanted } from \"./lib\" \
                    fn main() -> Int64 { return wanted() + helper() }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("not imported here"), "{e}");
    }

    #[test]
    fn a_local_shadow_does_not_hide_a_later_genuine_reference() {
        // `t` is a local only inside the `for`; a use of `t` OUTSIDE that scope is
        // still a genuine foreign reference — and here `lib` never imported it, so
        // it must error even though a same-named local exists elsewhere in the fn.
        let strings = "export fn t() -> Int64 { return 7 }";
        let lib = "export fn f(xs: Array<Int64>) -> Int64 { \
                       let mut s = 0 for t in xs { s = s + t } return s + t() }";
        let root = "import { f } from \"./lib\" \
                    import { t } from \"./strings\" \
                    fn main() -> Int64 { let xs: Array<Int64> = [1] return f(xs) + t() }";
        let e = load_err(root, &[("strings.vyrn", strings), ("lib.vyrn", lib)]);
        assert!(e.contains("not imported here"), "{e}");
    }

    #[test]
    fn namespaced_module_local_shadows_another_modules_export() {
        // Interaction with RFC-0027: a namespaced module `ui` has a local `t`
        // while `strings` (also linked) exports `t`.
        let strings = "export fn t() -> Int64 { return 40 }";
        let ui = "export fn render() -> Int64 { let t = 2 return t }";
        let root = "import * as ui from \"./ui\" \
                    import { t } from \"./strings\" \
                    fn main() -> Int64 { return ui.render() + t() }";
        assert_eq!(
            run_multi(root, &[("strings.vyrn", strings), ("ui.vyrn", ui)]).unwrap(),
            2 + 40
        );
    }

    #[test]
    fn co_named_stub_with_a_local_shadowing_another_export() {
        // Interaction with RFC-0022 co-naming AND local shadowing at once: the
        // root stubs `getUser` (co-naming) and also has a local `t` shadowing
        // `strings`'s exported `t`.
        let lib = "export fn getUser(id: Int64) -> Int64 { return id * 100 }";
        let strings = "export fn t() -> Int64 { return 5 }";
        let root = "import { getUser as getUserReal } from \"./lib\" \
                    import { t } from \"./strings\" \
                    fn getUser(id: Int64) -> Int64 { let t = 1 return getUserReal(id) + t } \
                    fn main() -> Int64 { return getUser(2) + t() }";
        assert_eq!(
            run_multi(root, &[("lib.vyrn", lib), ("strings.vyrn", strings)]).unwrap(),
            201 + 5
        );
    }

    #[test]
    fn generated_module_param_shadows_a_foreign_export() {
        // The `.vyx` `cls` shape: a generator-synthesized module has a fn whose
        // PARAM `cls` shadows `std/html`'s exported `cls`, both linked together.
        let html = "export fn cls(s: String) -> String { return s.copy() }";
        let gen = "export gen fn widgets(dir: String) -> String { \
                       return \"export fn item(cls: String) -> String { return cls.copy() }\" }";
        let root = "import { cls } from \"./html\" \
                    import { widgets } from \"./gen\" \
                    import { item } from widgets(\"./w\") \
                    fn main() -> Int64 { let a = cls(\"x\") let b = item(\"y\") return 0 }";
        // Links html (exports `cls`) + the synthesized module (param `cls`) with
        // no false \"cls not imported\" error.
        assert_eq!(
            run_multi(root, &[("html.vyrn", html), ("gen.vyrn", gen)]).unwrap(),
            0
        );
    }

    // ---- RFC-0027: namespaced imports ------------------------------------

    #[test]
    fn namespace_calls_and_type_positions() {
        let api = "export type User = { id: Int64 } \
                   export fn getUser(id: Int64) -> User { return User { id: id } }";
        let root = "import * as api from \"./api\" \
                    fn main() -> Int64 { \
                        let u: api.User = api.getUser(7) \
                        return u.id }";
        assert_eq!(run_multi(root, &[("api.vyrn", api)]).unwrap(), 7);
    }

    #[test]
    fn namespace_record_construction() {
        let api = "export type Req = { id: Int64 } \
                   export fn take(r: Req) -> Int64 { return r.id }";
        let root = "import * as api from \"./api\" \
                    fn main() -> Int64 { return api.take(api.Req { id: 41 }) + 1 }";
        assert_eq!(run_multi(root, &[("api.vyrn", api)]).unwrap(), 42);
    }

    #[test]
    fn namespace_enum_variant_construction_and_match() {
        let lib = "export type Color = | Red | Green | Blue";
        let root = "import * as c from \"./lib\" \
                    fn rank(x: c.Color) -> Int64 { \
                        return match x { c.Color.Red => 1, c.Color.Green => 2, c.Color.Blue => 3 } } \
                    fn main() -> Int64 { return rank(c.Color.Green) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 2);
    }

    #[test]
    fn namespace_enum_variant_with_payload() {
        let lib = "export type Shape = | Circle(Int64) | Dot \
                   export fn area(s: Shape) -> Int64 { return match s { Circle(r) => r * r, Dot => 0 } }";
        let root = "import * as g from \"./lib\" \
                    fn main() -> Int64 { return g.area(g.Shape.Circle(6)) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 36);
    }

    #[test]
    fn two_namespaced_modules_share_an_export_name() {
        // The whole point: two modules both export `render`, coexisting under
        // distinct namespaces without a flat-namespace collision.
        let a = "export fn render() -> Int64 { return 1 }";
        let b = "export fn render() -> Int64 { return 20 }";
        let root = "import * as a from \"./a\" \
                    import * as b from \"./b\" \
                    fn main() -> Int64 { return a.render() + b.render() }";
        assert_eq!(
            run_multi(root, &[("a.vyrn", a), ("b.vyrn", b)]).unwrap(),
            21
        );
    }

    #[test]
    fn namespace_composes_with_selective_import() {
        // A module may both selectively import and namespace the same module;
        // they resolve to the same decls.
        let api = "export fn getUser(id: Int64) -> Int64 { return id * 10 }";
        let root = "import { getUser } from \"./api\" \
                    import * as api from \"./api\" \
                    fn main() -> Int64 { return getUser(2) + api.getUser(3) }";
        assert_eq!(run_multi(root, &[("api.vyrn", api)]).unwrap(), 50);
    }

    #[test]
    fn namespace_type_name_argument() {
        // `fromJson(ns.User, s)` / `jsonSchema(ns.User)` — type-name arguments.
        let api = "export type User = { id: Int64, name: String }";
        let root = "import * as api from \"./api\" \
                    fn main() -> Int64 { \
                        return match fromJson(api.User, \"{\\\"id\\\":5,\\\"name\\\":\\\"a\\\"}\") { \
                            Valid(u) => u.id, Invalid(iss) => 0 } }";
        assert_eq!(run_multi(root, &[("api.vyrn", api)]).unwrap(), 5);
    }

    #[test]
    fn local_binding_shadows_a_namespace() {
        // A local `api` shadows the namespace; `api.field` is then field access on
        // the local record, not a qualified reference.
        let api = "export type T = { field: Int64 } export fn mk() -> T { return T { field: 9 } }";
        let root = "import * as api from \"./api\" \
                    fn main() -> Int64 { \
                        let rec = api.mk() \
                        let api = rec \
                        return api.field }";
        assert_eq!(run_multi(root, &[("api.vyrn", api)]).unwrap(), 9);
    }

    #[test]
    fn namespace_used_as_a_value_is_an_error() {
        let api = "export fn f() -> Int64 { return 1 }";
        let root = "import * as api from \"./api\" \
                    fn main() -> Int64 { let x = api return 0 }";
        let e = load_err(root, &[("api.vyrn", api)]);
        assert!(e.contains("namespace `api` is not a value"), "{e}");
    }

    #[test]
    fn namespace_member_must_be_exported() {
        let api = "fn secret() -> Int64 { return 1 } export fn ok() -> Int64 { return 2 }";
        let root = "import * as api from \"./api\" \
                    fn main() -> Int64 { return api.secret() }";
        let e = load_err(root, &[("api.vyrn", api)]);
        assert!(e.contains("no exported member `secret`"), "{e}");
    }

    #[test]
    fn namespaces_are_one_level_deep() {
        // `./a` namespaces `./b`; a root namespace of `./a` cannot reach `b.thing`.
        let b = "export fn thing() -> Int64 { return 7 }";
        let a = "import * as b from \"./b\" export fn viaA() -> Int64 { return b.thing() }";
        let root = "import * as a from \"./a\" \
                    fn main() -> Int64 { return a.b.thing() }";
        let e = load_err(root, &[("a.vyrn", a), ("b.vyrn", b)]);
        assert!(e.contains("no exported member `b`"), "{e}");
    }

    #[test]
    fn namespace_name_colliding_with_a_decl_is_an_error() {
        let api = "export fn f() -> Int64 { return 1 }";
        let root = "import * as api from \"./api\" \
                    fn api() -> Int64 { return 0 } \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[("api.vyrn", api)]);
        assert!(e.contains("collides with a top-level declaration"), "{e}");
    }

    #[test]
    fn duplicate_namespace_name_is_an_error() {
        let a = "export fn f() -> Int64 { return 1 }";
        let b = "export fn g() -> Int64 { return 2 }";
        let root = "import * as x from \"./a\" \
                    import * as x from \"./b\" \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[("a.vyrn", a), ("b.vyrn", b)]);
        assert!(e.contains("bound twice"), "{e}");
    }
}

#[cfg(test)]
mod remote_tests {
    use super::tests::{map, opts};
    use super::*;

    fn load_err_at(root: &str, files: &[(&str, &str)]) -> String {
        match load(root, "main.vyrn", &opts(), &map(files)) {
            Ok(_) => panic!("expected a load error"),
            Err(ds) => ds
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    #[test]
    fn remote_specifiers_are_their_own_keys() {
        // A MapResolver keyed by the remote key stands in for the network —
        // exactly what the CLI's cache does.
        let lib = "export fn pad(n: Int64) -> Int64 { return n + 1 }";
        let root = "import { pad } from \"github:acme/strings@v1/src/pad\" \
                    fn main() -> Int64 { return pad(41) }";
        let program = load(
            root,
            "main.vyrn",
            &opts(),
            &map(&[("github:acme/strings@v1/src/pad.vyrn", lib)]),
        )
        .unwrap();
        assert_eq!(crate::interp::run(&program).unwrap(), 42);
    }

    #[test]
    fn relative_imports_inside_a_remote_stay_in_its_base() {
        let a = "import { b } from \"./b\" export fn a() -> Int64 { return b() }";
        let b = "export fn b() -> Int64 { return 7 }";
        let root = "import { a } from \"github:acme/x@abc/src/a\" \
                    fn main() -> Int64 { return a() }";
        let program = load(
            root,
            "main.vyrn",
            &opts(),
            &map(&[
                ("github:acme/x@abc/src/a.vyrn", a),
                ("github:acme/x@abc/src/b.vyrn", b),
            ]),
        )
        .unwrap();
        assert_eq!(crate::interp::run(&program).unwrap(), 7);
    }

    #[test]
    fn remote_relative_escapes_are_rejected() {
        let a = "import { x } from \"../../../etc/passwd\" \
                 export fn a() -> Int64 { return 0 }";
        let root = "import { a } from \"github:acme/x@abc/src/a\" \
                    fn main() -> Int64 { return a() }";
        let e = load_err_at(root, &[("github:acme/x@abc/src/a.vyrn", a)]);
        assert!(e.contains("escapes its remote module's base"), "{e}");
    }

    #[test]
    fn bare_specifiers_inside_remote_modules_are_rejected() {
        let a = "import { x } from \"money\" export fn a() -> Int64 { return 0 }";
        let root = "import { a } from \"gist:demko/abc123/a\" \
                    fn main() -> Int64 { return a() }";
        let mut o = opts();
        o.aliases.insert("money".into(), "./money".into());
        let e = match load(
            root,
            "main.vyrn",
            &o,
            &map(&[("gist:demko/abc123/a.vyrn", a)]),
        ) {
            Ok(_) => panic!("expected error"),
            Err(ds) => ds[0].message.clone(),
        };
        assert!(e.contains("cannot resolve import `money`"), "{e}");
    }

    #[test]
    fn http_imports_are_rejected() {
        let root = "import { x } from \"http://x.dev/y\" fn main() -> Int64 { return 0 }";
        let e = load_err_at(root, &[]);
        assert!(e.contains("insecure `http:`"), "{e}");
    }
}

#[cfg(test)]
mod gen_tests {
    use super::tests::opts;
    use super::*;
    use std::cell::RefCell;

    /// A resolver over an in-memory map that ALSO persists the generator cache in
    /// memory — so a second load in the same test observes cache hits.
    struct CachingResolver {
        files: HashMap<String, String>,
        cache: RefCell<HashMap<String, String>>,
    }
    impl CachingResolver {
        fn new(entries: &[(&str, &str)]) -> CachingResolver {
            CachingResolver {
                files: entries
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                cache: RefCell::new(HashMap::new()),
            }
        }
    }
    impl ModuleResolver for CachingResolver {
        fn read(&self, resolved: &str) -> Result<String, String> {
            self.files
                .get(resolved)
                .cloned()
                .ok_or_else(|| format!("not found: {resolved}"))
        }
        fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
            let prefix = format!("{}/", resolved.trim_end_matches('/'));
            let mut names: std::collections::BTreeSet<String> = Default::default();
            let mut any = false;
            for k in self.files.keys() {
                if let Some(rest) = k.strip_prefix(&prefix) {
                    any = true;
                    if let Some(seg) = rest.split('/').next() {
                        if !seg.is_empty() {
                            names.insert(seg.to_string());
                        }
                    }
                }
            }
            if any {
                Ok(names.into_iter().collect())
            } else {
                Err(format!("cannot list `{resolved}`"))
            }
        }
        fn gen_cache_get(&self, key: &str) -> Option<String> {
            self.cache.borrow().get(key).cloned()
        }
        fn gen_cache_put(&self, key: &str, value: &str) {
            self.cache
                .borrow_mut()
                .insert(key.to_string(), value.to_string());
        }
    }

    fn run_with(root: &str, r: &dyn ModuleResolver) -> Result<i64, String> {
        let program = load(root, "main.vyrn", &opts(), r)
            .map_err(|ds| ds.iter().map(|d| d.render()).collect::<Vec<_>>().join("\n"))?;
        let diags = crate::checker::check_accum(&program);
        if let Some(d) = diags.first() {
            return Err(d.render());
        }
        crate::interp::run(&program)
    }

    fn map(entries: &[(&str, &str)]) -> MapResolver {
        MapResolver(
            entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }
    fn run(root: &str, files: &[(&str, &str)]) -> Result<i64, String> {
        run_with(root, &map(files))
    }
    fn gen_err(root: &str, files: &[(&str, &str)]) -> String {
        match load(root, "main.vyrn", &opts(), &map(files)) {
            Ok(p) => match crate::checker::check_accum(&p).first() {
                Some(d) => d.message.clone(),
                None => panic!("expected an error, load+check succeeded"),
            },
            Err(ds) => ds
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    #[test]
    fn generator_output_links_and_runs() {
        let gen = "export gen fn mk(dir: String) -> String { \
                       return \"export fn magic() -> Int64 { return 42 }\" }";
        let root = "import { mk } from \"./gen\" \
                    import { magic } from mk(\"./data\") \
                    fn main() -> Int64 { return magic() }";
        assert_eq!(run(root, &[("gen.vyrn", gen)]).unwrap(), 42);
    }

    #[test]
    fn generator_reads_a_scoped_file() {
        // The generator reads a data file (mediated) and emits it as a constant.
        let gen = "export gen fn consts(dir: String) -> String { \
                       return match readFile(\"./data/n.txt\") { \
                           Ok(s) => \"export fn n() -> String { return \\\"\" + s + \"\\\" }\", \
                           Err(e) => e } }";
        let root = "import { consts } from \"./gen\" \
                    import { n } from consts(\"./data\") \
                    fn main() -> Int64 { print(n()) return 0 }";
        let files = &[("gen.vyrn", gen), ("data/n.txt", "hello")];
        // Links + runs (the emitted `n` returns the file content).
        assert_eq!(run(root, files).unwrap(), 0);
    }

    #[test]
    fn generator_readfile_escape_is_rejected() {
        let gen = "export gen fn g(dir: String) -> String { \
                       return match readFile(\"./secret.txt\") { Ok(s) => s, Err(e) => e } }";
        let root = "import { g } from \"./gen\" \
                    import { x } from g(\"./data\") \
                    fn main() -> Int64 { return 0 }";
        let e = gen_err(root, &[("gen.vyrn", gen), ("secret.txt", "top secret")]);
        assert!(e.contains("escapes its declared inputs"), "{e}");
    }

    #[test]
    fn generator_listdir_is_scoped_and_works() {
        // Emit a function returning the number of files under the data dir.
        let gen = "export gen fn count(dir: String) -> String { \
                       return match listDir(dir) { \
                           Ok(names) => \"export fn n() -> Int64 { return \" + names.length.toString() + \" }\", \
                           Err(e) => e } }";
        let root = "import { count } from \"./gen\" \
                    import { n } from count(\"./data\") \
                    fn main() -> Int64 { return n() }";
        let files = &[
            ("gen.vyrn", gen),
            ("data/a.txt", "1"),
            ("data/b.txt", "2"),
            ("data/c.txt", "3"),
        ];
        assert_eq!(run(root, files).unwrap(), 3);
    }

    #[test]
    fn distinct_args_make_distinct_modules_same_args_dedup() {
        // Two calls with different args ⇒ two modules with different names.
        let gen = "export gen fn mk(tag: String) -> String { \
                       return \"export fn tag\" + tag + \"() -> Int64 { return \" + tag + \" }\" }";
        let root = "import { mk } from \"./gen\" \
                    import { tag1 } from mk(\"1\") \
                    import { tag2 } from mk(\"2\") \
                    fn main() -> Int64 { return tag1() + tag2() }";
        assert_eq!(run(root, &[("gen.vyrn", gen)]).unwrap(), 3);
    }

    #[test]
    fn same_resolved_path_different_spellings_share_one_stateful_module() {
        // RFC-0040 §1: two importers call the same generator with path args that
        // RESOLVE identically but are spelled differently (`./data` vs the rebased
        // `./x/../data`). They must synthesize ONE module — so its module state (`n`)
        // exists once and both importers mutate the SAME instance. Without the
        // resolved-inputs identity, two modules each define `n`/`bump` and collide.
        let gen = "export gen fn mk(dir: String) -> String { \
                       return \"let mut n: Int64 = 0\\n\
                                export fn bump() -> Int64 { n = n + 1\\nreturn n }\\n\" }";
        let a = "import { mk } from \"./gen\" \
                 import { bump } from mk(\"./data\") \
                 export fn a() -> Int64 { return bump() }";
        let b = "import { mk } from \"./gen\" \
                 import { bump } from mk(\"./x/../data\") \
                 export fn b() -> Int64 { return bump() }";
        let root = "import { a } from \"./a\" \
                    import { b } from \"./b\" \
                    fn main() -> Int64 { return a() + b() }";
        // One shared `n`: a() = 1, b() = 2, sum = 3.
        assert_eq!(
            run(root, &[("gen.vyrn", gen), ("a.vyrn", a), ("b.vyrn", b)]).unwrap(),
            3,
        );
    }

    #[test]
    fn different_resolved_paths_stay_distinct_modules() {
        // The flip side of §1: two calls that resolve to DIFFERENT targets are
        // still two modules. Each emits `bump`, so the flat namespace collides —
        // proof the identity did not over-merge distinct targets.
        let gen = "export gen fn mk(dir: String) -> String { \
                       return \"export fn bump() -> Int64 { return 1 }\\n\" }";
        let a = "import { mk } from \"./gen\" \
                 import { bump } from mk(\"./data\") \
                 export fn a() -> Int64 { return bump() }";
        let b = "import { mk } from \"./gen\" \
                 import { bump } from mk(\"./other\") \
                 export fn b() -> Int64 { return bump() }";
        let root = "import { a } from \"./a\" \
                    import { b } from \"./b\" \
                    fn main() -> Int64 { return a() + b() }";
        let e = gen_err(root, &[("gen.vyrn", gen), ("a.vyrn", a), ("b.vyrn", b)]);
        assert!(e.contains("`bump` is declared by both"), "{e}");
    }

    #[test]
    fn generator_trap_becomes_a_load_diagnostic() {
        let gen = "export gen fn bad(x: Int64) -> String { \
                       let q = 1 / x \
                       return \"\" }";
        let root = "import { bad } from \"./gen\" \
                    import { z } from bad(0) \
                    fn main() -> Int64 { return 0 }";
        let e = gen_err(root, &[("gen.vyrn", gen)]);
        assert!(e.contains("generator `bad") && e.contains("failed"), "{e}");
    }

    #[test]
    fn generated_name_collision_is_a_load_error() {
        let gen = "export gen fn mk(d: String) -> String { \
                       return \"export fn dup() -> Int64 { return 1 }\" }";
        // The root already defines `dup`, so the generated `dup` collides.
        let root = "import { mk } from \"./gen\" \
                    import { dup } from mk(\"./x\") \
                    fn dup() -> Int64 { return 2 } \
                    fn main() -> Int64 { return dup() }";
        let e = gen_err(root, &[("gen.vyrn", gen)]);
        assert!(e.contains("`dup` is declared by both"), "{e}");
    }

    #[test]
    fn non_constant_generator_argument_is_rejected() {
        let gen = "export gen fn mk(d: String) -> String { return \"\" }";
        let root = "import { mk } from \"./gen\" \
                    import { x } from mk(readLine()) \
                    fn main() -> Int64 { return 0 }";
        let e = gen_err(root, &[("gen.vyrn", gen)]);
        assert!(e.contains("compile-time-constant"), "{e}");
    }

    #[test]
    fn missing_generator_is_a_clear_error() {
        let root = "import { x } from nope(\"./d\") fn main() -> Int64 { return 0 }";
        let e = gen_err(root, &[]);
        assert!(e.contains("not an imported `gen fn`"), "{e}");
    }

    #[test]
    fn module_interface_reflects_exported_surface() {
        // The generator emits a doc string listing the contract's exported fns.
        let contract = "export type Id = Int64 where value >= 1 \
                        export fn ping(id: Id) -> String { return \"pong\" }";
        let gen = "export gen fn doc(path: String) -> String { \
                       let iface = moduleInterface(path) \
                       let mut body = \"export fn names() -> String { return \\\"\" \
                       for f in iface.functions { body = body + f.name + \";\" } \
                       body = body + \"\\\" }\" \
                       return body }";
        let root = "import { doc } from \"./gen\" \
                    import { names } from doc(\"./contract\") \
                    fn main() -> Int64 { print(names()) return 0 }";
        let files = &[("gen.vyrn", gen), ("contract.vyrn", contract)];
        // Runs; `names()` returns "ping;" (the one exported fn).
        assert_eq!(run(root, files).unwrap(), 0);
    }

    #[test]
    fn module_interface_closure_reaches_imported_types() {
        // RFC-0031: the contract NAMES only `Req` in its signature and declares no
        // types of its own; `Req`/`Book`/`Id` live in `wire`. `moduleInterface`
        // must reach the whole closure, so the generator counting `iface.types`
        // sees all three.
        let wire = "export type Id = Int64 where value >= 1 \
                    export type Book = { id: Id } \
                    export type Req = { book: Book }";
        let contract = "import { Req } from \"./wire\" \
                        export fn make(r: Req) -> Req { return r }";
        let gen = "export gen fn cnt(path: String) -> String { \
                       let iface = moduleInterface(path) \
                       let mut n = 0 \
                       for t in iface.types { n = n + 1 } \
                       return \"export fn n() -> Int64 { return \" + \"\\{n}\" + \" }\\n\" }";
        let root = "import { cnt } from \"./gen\" \
                    import { n } from cnt(\"./contract\") \
                    fn main() -> Int64 { return n() }";
        assert_eq!(
            run(
                root,
                &[
                    ("gen.vyrn", gen),
                    ("contract.vyrn", contract),
                    ("wire.vyrn", wire)
                ]
            )
            .unwrap(),
            3,
            "closure = Req + Book + Id"
        );
    }

    #[test]
    fn closure_type_file_edit_invalidates_the_cache_unrelated_edit_hits() {
        // RFC-0031 cache soundness: a closure type's defining FILE (`wire.vyrn`)
        // is never a generator ARGUMENT (the arg is `./contract`), yet editing it
        // must miss the cache. It joins the recorded inputs through the reflection
        // read, so the content hash changes on edit; an unrelated file does not.
        let wire = "export type T = { a: Int64 } export fn seed(t: T) -> T { return t }";
        let contract = "import { T } from \"./wire\" export fn f(x: T) -> T { return x }";
        // The generator's output embeds the closure's field spelling, so a real
        // edit to `wire.vyrn` produces FRESH output, not just a re-run.
        let gen = "export gen fn refl(path: String) -> String { \
                       let iface = moduleInterface(path) \
                       let mut src = \"\" \
                       for t in iface.types { src = src + t.source } \
                       return \"export fn shape() -> Int64 { return \" + \"\\{src.byteLength}\" + \" }\\n\" }";
        let root = "import { refl } from \"./gen\" \
                    import { shape } from refl(\"./contract\") \
                    fn main() -> Int64 { return shape() }";
        let mut r = CachingResolver::new(&[
            ("gen.vyrn", gen),
            ("contract.vyrn", contract),
            ("wire.vyrn", wire),
            ("noise.vyrn", "export fn unused() -> Int64 { return 0 }"),
        ]);

        let before = gen_run_count();
        run_with(root, &r).unwrap();
        assert_eq!(gen_run_count(), before + 1, "cold: one run");
        run_with(root, &r).unwrap();
        assert_eq!(gen_run_count(), before + 1, "warm: cache hit, no re-run");

        // An unrelated edit (a file the closure never reads) still hits.
        r.files.insert(
            "noise.vyrn".to_string(),
            "export fn unused() -> Int64 { return 1 }".to_string(),
        );
        run_with(root, &r).unwrap();
        assert_eq!(
            gen_run_count(),
            before + 1,
            "unrelated edit: still a cache hit"
        );

        // Editing the foreign closure type's file misses → re-run + fresh output.
        r.files.insert(
            "wire.vyrn".to_string(),
            "export type T = { a: Int64, b: Int64 } export fn seed(t: T) -> T { return t }"
                .to_string(),
        );
        run_with(root, &r).unwrap();
        assert_eq!(gen_run_count(), before + 2, "closure type edited: re-run");
    }

    /// The generator's OWN sources — its module and everything that module
    /// imports — must invalidate its cache entry.
    ///
    /// They used to be hashed into the lookup key, which meant discovering the
    /// closure (a full parse-walk) on every hit just to find the entry. They are
    /// now recorded among the entry's inputs and re-hashed on lookup instead, so
    /// this is the test that the move kept the guarantee: edit the generator, or
    /// anything it imports, and the next load must RE-RUN rather than serve a
    /// stale expansion.
    #[test]
    fn editing_the_generator_or_its_imports_invalidates_the_cache() {
        let helper = r#"export fn tag() -> String { return "one" }"#;
        let gen = r#"import { tag } from "./helper"
export gen fn emit(x: String) -> String { return "export fn shape() -> String { return \"" + tag() + "\" }" }"#;
        let root = r#"import { emit } from "./gen"
import { shape } from emit("./seed")
fn main() -> Int64 { return shape().byteLength }"#;
        let mut r = CachingResolver::new(&[
            ("gen.vyrn", gen),
            ("helper.vyrn", helper),
            ("seed.vyrn", "export fn seed() -> Int64 { return 0 }"),
        ]);

        let before = gen_run_count();
        assert_eq!(run_with(root, &r).unwrap(), 3, "cold: `one`");
        assert_eq!(gen_run_count(), before + 1, "cold: one run");
        assert_eq!(run_with(root, &r).unwrap(), 3);
        assert_eq!(gen_run_count(), before + 1, "warm: cache hit, no re-run");

        // Edit a module the GENERATOR imports — never an argument, never read by
        // the sandbox, reachable only through the generator's own module graph.
        r.files.insert(
            "helper.vyrn".to_string(),
            r#"export fn tag() -> String { return "three" }"#.to_string(),
        );
        assert_eq!(
            run_with(root, &r).unwrap(),
            5,
            "generator's import edited: fresh output, not the stale `one`"
        );
        assert_eq!(
            gen_run_count(),
            before + 2,
            "generator's import edited: re-run"
        );

        // Edit the generator module itself.
        r.files.insert(
            "gen.vyrn".to_string(),
            gen.replace("tag()", "tag() + \"!\""),
        );
        assert_eq!(
            run_with(root, &r).unwrap(),
            6,
            "generator edited: fresh output (`three` + `!`)"
        );
        assert_eq!(gen_run_count(), before + 3, "generator edited: re-run");
    }

    #[test]
    fn co_naming_rename_leaves_namespace_member_calls_alone() {
        // RFC-0031 found this: `mid` delegates `store.get()` via a namespace
        // (RFC-0027) while ANOTHER module co-names `get` (aliased import + a local
        // stub of the same name, RFC-0022). The co-naming rename frees `mid`'s
        // `get` for the stub — but must NOT rewrite `store.get()` (method-sugar
        // call name) into `store.get__from0`; that member belongs to the
        // namespace resolver.
        let store = "let mut n = 41 \
                     export fn fetch() -> Int64 { n = n + 1 return n }";
        let mid = "import * as store from \"./store\" \
                   export fn fetch() -> Int64 { return store.fetch() }";
        let root = "import { fetch as fetch__real } from \"./mid\" \
                    fn fetch() -> Int64 { return fetch__real() } \
                    fn main() -> Int64 { return fetch() }";
        assert_eq!(
            run(root, &[("store.vyrn", store), ("mid.vyrn", mid)]).unwrap(),
            42
        );
    }

    #[test]
    fn closure_name_collision_is_a_load_diagnostic_naming_both_modules() {
        // RFC-0031: if the closure would hold two DISTINCT `T` decls (one per
        // module), reflection fails with a load diagnostic naming BOTH modules —
        // a wire format with two `T`s has no honest JSON spelling.
        let wire_a = "export type T = { a: Int64 } export type A = { t: T }";
        let wire_b = "export type T = { b: Int64 } export type B = { t: T }";
        let contract = "import { A } from \"./wireA\" \
                        import { B } from \"./wireB\" \
                        export fn f(a: A) -> B { return B { t: T { b: 0 } } }";
        let gen = "export gen fn cnt(path: String) -> String { \
                       let iface = moduleInterface(path) \
                       return \"export fn z() -> Int64 { return 0 }\\n\" }";
        let root = "import { cnt } from \"./gen\" \
                    import { z } from cnt(\"./contract\") \
                    fn main() -> Int64 { return z() }";
        let e = gen_err(
            root,
            &[
                ("gen.vyrn", gen),
                ("contract.vyrn", contract),
                ("wireA.vyrn", wire_a),
                ("wireB.vyrn", wire_b),
            ],
        );
        assert!(
            e.contains("wireA.vyrn") && e.contains("wireB.vyrn"),
            "names both modules: {e}"
        );
        assert!(e.contains('T'), "names the colliding type: {e}");
    }

    #[test]
    fn generated_module_imports_a_sibling() {
        // A synthesized module (its key is a banner, not a path) must resolve its
        // own relative imports against the real importer's directory (RFC-0021 —
        // the first `moduleInterface` consumer, RPC, needs this).
        let contract = "export fn calc() -> Int64 { return 21 }";
        let gen = "export gen fn wrap(path: String) -> String { \
                       let iface = moduleInterface(path) \
                       return \"import { calc } from \\\"\" + path + \"\\\"\\n\" \
                            + \"export fn go() -> Int64 { return calc() + calc() }\\n\" }";
        let root = "import { wrap } from \"./gen\" \
                    import { go } from wrap(\"./contract\") \
                    fn main() -> Int64 { return go() }";
        assert_eq!(
            run(root, &[("gen.vyrn", gen), ("contract.vyrn", contract)]).unwrap(),
            42
        );
    }

    #[test]
    fn nested_generator_resolves_paths_against_the_real_importer() {
        // RFC-0029 wave: a generator imported BY a generated module (a nested
        // generator — e.g. `i18n(..)` inside a `.vyx` script that `components(..)`
        // synthesized) must resolve its path arguments against the REAL importing
        // file's directory, not the synthetic banner key. `outer` emits a module
        // that imports `inner("./sub/data")`; `inner` reflects that module — which
        // it can only read if the path resolves to `sub/data.vyrn`.
        let gen = "export gen fn inner(path: String) -> String { \
                       let iface = moduleInterface(path) \
                       let mut n = 0 \
                       for f in iface.functions { n = n + 1 } \
                       return \"export fn cnt() -> Int64 { return \" + \"\\{n}\" + \" }\\n\" } \
                   export gen fn outer(dummy: String) -> String { \
                       return \"import { inner } from \\\"./gen\\\"\\n\" \
                            + \"import { cnt } from inner(\\\"./sub/data\\\")\\n\" \
                            + \"export fn go() -> Int64 { return cnt() }\\n\" }";
        let data = "export fn a() -> Int64 { return 1 } export fn b() -> Int64 { return 2 }";
        let root = "import { outer } from \"./gen\" \
                    import { go } from outer(\"x\") \
                    fn main() -> Int64 { return go() }";
        // `sub/data` has two exported functions, so `cnt()` — hence `go()` — is 2.
        assert_eq!(
            run(root, &[("gen.vyrn", gen), ("sub/data.vyrn", data)]).unwrap(),
            2
        );
    }

    #[test]
    fn generated_module_may_declare_module_state() {
        // Module state is legal in a generated module (RFC-0021's carve-out, now
        // the general RFC-0029 rule — see `non_root_module_state_is_legal_via_accessors`).
        // The generated `currentLocale`-style global initializes before `main` and
        // persists across handler calls made from the root (the setLocale/locale + t() shape).
        let gen = "export gen fn mk(tag: String) -> String { \
                       return \"let mut cur = 10\\n\" \
                            + \"export fn bump() { cur = cur + 1 }\\n\" \
                            + \"export fn peek() -> Int64 { return cur }\\n\" }";
        let root = "import { mk } from \"./gen\" \
                    import { bump, peek } from mk(\"x\") \
                    fn main() -> Int64 { bump() bump() return peek() }";
        // 10 (init) + 1 + 1 = 12; state persists across the two `bump()` calls.
        assert_eq!(run(root, &[("gen.vyrn", gen)]).unwrap(), 12);
    }

    #[test]
    fn generated_module_calls_back_into_its_importer() {
        // The RPC dispatcher pattern: a generated module invokes a plain function
        // defined in the module that imported it (the callback convention). Names
        // owned by the importer are visible to generated code without an import.
        let gen = "export gen fn cb(tag: String) -> String { \
                       return \"export fn dispatch() -> Int64 { return onEvent() + 1 }\\n\" }";
        let root = "import { cb } from \"./gen\" \
                    import { dispatch } from cb(\"x\") \
                    fn onEvent() -> Int64 { return 41 } \
                    fn main() -> Int64 { return dispatch() }";
        assert_eq!(run(root, &[("gen.vyrn", gen)]).unwrap(), 42);
    }

    #[test]
    fn two_generators_same_args_do_not_share_a_cache_entry() {
        // One module may export several `gen fn`s; distinct generators over the
        // same arguments must not collide in the content-addressed cache (the
        // cache key includes the generator name).
        let gen = "export gen fn a(p: String) -> String { \
                       return \"export fn which() -> Int64 { return 1 }\" } \
                   export gen fn b(p: String) -> String { \
                       return \"export fn which() -> Int64 { return 2 }\" }";
        let root_a = "import { a } from \"./gen\" \
                      import { which } from a(\"./x\") \
                      fn main() -> Int64 { return which() }";
        let root_b = "import { b } from \"./gen\" \
                      import { which } from b(\"./x\") \
                      fn main() -> Int64 { return which() }";
        let r = CachingResolver::new(&[("gen.vyrn", gen)]);
        assert_eq!(run_with(root_a, &r).unwrap(), 1, "generator `a` output");
        assert_eq!(
            run_with(root_b, &r).unwrap(),
            2,
            "generator `b` must not reuse `a`'s cache"
        );
    }

    #[test]
    fn cache_hit_skips_the_second_run_and_input_change_invalidates() {
        let gen = "export gen fn consts(dir: String) -> String { \
                       return match readFile(\"./data/n.txt\") { \
                           Ok(s) => \"export fn n() -> String { return \\\"\" + s + \"\\\" }\", \
                           Err(e) => e } }";
        let root = "import { consts } from \"./gen\" \
                    import { n } from consts(\"./data\") \
                    fn main() -> Int64 { return 0 }";
        let mut r = CachingResolver::new(&[("gen.vyrn", gen), ("data/n.txt", "one")]);

        let before = gen_run_count();
        run_with(root, &r).unwrap();
        assert_eq!(gen_run_count(), before + 1, "cold: one run");
        run_with(root, &r).unwrap();
        assert_eq!(gen_run_count(), before + 1, "warm: cache hit, no re-run");

        // Change the input file — the recorded input hash no longer matches.
        r.files.insert("data/n.txt".to_string(), "two".to_string());
        run_with(root, &r).unwrap();
        assert_eq!(gen_run_count(), before + 2, "input changed: re-run");
    }

    #[test]
    fn generator_over_step_budget_fails_loudly() {
        super::GEN_FUEL_OVERRIDE.with(|c| c.set(Some(500)));
        let gen = "export gen fn spin(n: Int64) -> String { \
                       let mut i = 0 \
                       while i < 1000000000 { i = i + 1 } \
                       return \"\" }";
        let root = "import { spin } from \"./gen\" \
                    import { z } from spin(1) \
                    fn main() -> Int64 { return 0 }";
        let e = gen_err(root, &[("gen.vyrn", gen)]);
        super::GEN_FUEL_OVERRIDE.with(|c| c.set(None));
        assert!(e.contains("exceeded its step budget"), "{e}");
    }

    #[test]
    fn generator_over_output_cap_fails_loudly() {
        super::GEN_MAX_OUTPUT_OVERRIDE.with(|c| c.set(Some(5)));
        let gen = "export gen fn big(d: String) -> String { \
                       return \"this is far more than five bytes\" }";
        let root = "import { big } from \"./gen\" \
                    import { z } from big(\"./d\") \
                    fn main() -> Int64 { return 0 }";
        let e = gen_err(root, &[("gen.vyrn", gen)]);
        super::GEN_MAX_OUTPUT_OVERRIDE.with(|c| c.set(None));
        assert!(e.contains("over the") && e.contains("cap"), "{e}");
    }

    #[test]
    fn generator_purity_violation_is_reported() {
        // A `gen fn` that writes a file fails the comptime-purity check.
        let gen = "export gen fn bad(d: String) -> String { \
                       let w = writeFile(\"x\", \"y\") return \"\" }";
        let root = "import { bad } from \"./gen\" \
                    import { z } from bad(\"./d\") \
                    fn main() -> Int64 { return 0 }";
        let e = gen_err(root, &[("gen.vyrn", gen)]);
        assert!(e.contains("not comptime-pure"), "{e}");
    }

    #[test]
    fn same_relative_arg_in_different_dirs_does_not_collide_in_the_cache() {
        // dogfood BUG 1: two modules in DIFFERENT directories both call the same
        // generator with the SAME relative arg (`consts("./data")`), but each
        // `./data` resolves to a different file. The content-addressed cache must
        // NOT serve the first importer's output to the second — the key now folds
        // in the RESOLVED inputs, so the two never share an entry.
        let gen = "export gen fn consts(dir: String) -> String { \
                       return match readFile(dir + \"/n.txt\") { \
                           Ok(s) => \"export fn val() -> Int64 { return \" + s + \" }\", \
                           Err(e) => e } }";
        let a = "import { consts } from \"../gen\" \
                 import * as g from consts(\"./data\") \
                 export fn na() -> Int64 { return g.val() }";
        let b = "import { consts } from \"../gen\" \
                 import * as g from consts(\"./data\") \
                 export fn nb() -> Int64 { return g.val() }";
        let root = "import { na } from \"./a/client\" \
                    import { nb } from \"./b/client\" \
                    fn main() -> Int64 { return na() * 10 + nb() }";
        let r = CachingResolver::new(&[
            ("gen.vyrn", gen),
            ("a/client.vyrn", a),
            ("a/data/n.txt", "1"),
            ("b/client.vyrn", b),
            ("b/data/n.txt", "2"),
        ]);
        // Warm cache from `a`'s generation must not leak into `b`'s: 1*10 + 2 = 12
        // (a pre-fix collision served `b` the value `1`, giving 11).
        assert_eq!(run_with(root, &r).unwrap(), 12);
    }

    #[test]
    fn identical_importer_and_arg_still_hits_the_cache() {
        // The other half of BUG 1's fix: same importer + same arg must STILL be a
        // cache hit on re-load (no needless re-run). Two loads of the same root;
        // the generation runs once, then the warm cache short-circuits it.
        let gen = "export gen fn consts(dir: String) -> String { \
                       return match readFile(dir + \"/n.txt\") { \
                           Ok(s) => \"export fn val() -> Int64 { return \" + s + \" }\", \
                           Err(e) => e } }";
        let client = "import { consts } from \"../gen\" \
                      import { val } from consts(\"./data\") \
                      export fn na() -> Int64 { return val() }";
        let root = "import { na } from \"./a/client\" fn main() -> Int64 { return na() }";
        let r = CachingResolver::new(&[
            ("gen.vyrn", gen),
            ("a/client.vyrn", client),
            ("a/data/n.txt", "7"),
        ]);
        let before = gen_run_count();
        assert_eq!(run_with(root, &r).unwrap(), 7);
        assert_eq!(gen_run_count(), before + 1, "cold: one run");
        assert_eq!(run_with(root, &r).unwrap(), 7);
        assert_eq!(gen_run_count(), before + 1, "warm: cache hit, no re-run");
    }
}
