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

use std::collections::{HashMap, HashSet};

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
        self.0.get(resolved).cloned().ok_or_else(|| format!("module not found: {resolved}"))
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
fn generated_importer(key: &str) -> Option<&str> {
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
    let Some(base) = remote_base(key) else { return key.to_string() };
    let rest = &key[base.len()..];
    let rest = rest.trim_start_matches('/');
    format!("{base}/{}", normalize(rest))
}

/// Resolve an import specifier written inside `importer` to a module key.
fn resolve_spec(spec: &str, importer: &str, opts: &LoadOptions) -> Result<String, String> {
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
        let root = opts.std_root.as_deref().ok_or_else(|| {
            "std library not available (no std root configured)".to_string()
        })?;
        return Ok(normalize(&with_ext(format!("{root}/{rest}"))));
    }
    if spec.starts_with("http://") {
        return Err(format!("insecure `http:` import `{spec}` — use https"));
    }
    // Remote specifiers are their own keys; the resolver (vyrn-cli) turns them
    // into content via the lockfile/cache/network.
    if is_remote(spec) {
        let key = normalize_remote(&with_ext(spec.to_string()));
        remote_base(&key)
            .ok_or_else(|| format!("malformed remote specifier `{spec}`"))?;
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
        let joined =
            if base.is_empty() { spec.to_string() } else { format!("{base}/{spec}") };
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
    let (modules, _) = load_modules(root_source, root_path, opts, resolver)?;
    Ok(modules.into_iter().filter_map(|m| m.gen_source.map(|s| (m.key, s))).collect())
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
    let (modules, root_key) = load_modules(root_source, root_path, opts, resolver)?;
    link(modules, &root_key)
}

/// The module dependency graph: every (module key, resolved import targets)
/// pair reachable from the root — powers `vyrn deps`.
pub fn module_graph(
    root_source: &str,
    root_path: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Result<Vec<(String, Vec<String>)>, Vec<Diagnostic>> {
    let (modules, _) = load_modules(root_source, root_path, opts, resolver)?;
    Ok(modules.into_iter().map(|m| (m.key, m.import_targets)).collect())
}

fn load_modules(
    root_source: &str,
    root_path: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Result<(Vec<Module>, String), Vec<Diagnostic>> {
    let root_key = normalize(root_path);
    let mut modules: Vec<Module> = Vec::new();
    let mut states: HashMap<String, bool> = HashMap::new(); // false = loading
    let mut stack: Vec<String> = Vec::new();

    fn visit(
        key: &str,
        source: Option<&str>,
        opts: &LoadOptions,
        resolver: &dyn ModuleResolver,
        modules: &mut Vec<Module>,
        states: &mut HashMap<String, bool>,
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
                vec![Diagnostic::error(0, 0, "load", format!("cannot load `{key}`: {e}"))]
            })?,
        };
        let is_root = key == root_key;

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
                    impls: Vec::new(),
                    globals: Vec::new(),
                    tests: Vec::new(),
                    log_level: DEFAULT_LOG_LEVEL,
                    log_sink: LogSink::Stderr,
                },
                import_targets: Vec::new(),
                gen_source: None,
            });
            stack.pop();
            states.insert(key.to_string(), true);
            return Ok(());
        }
        let tokens = lexer::lex(&text).map_err(|mut d| {
            if !is_root {
                d.file = Some(key.to_string());
            }
            vec![d]
        })?;
        let (mut program, errors) = parser::parse_accum(tokens);
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

        // Module state is root-only (RFC-0013): an imported library stays
        // stateless, the same discipline as root-only `logging`. A top-level
        // `let` in a non-root module is a load error.
        //
        // Carve-out (RFC-0021/RFC-0020 M2): a module SYNTHESIZED BY A GENERATOR
        // may declare module state. Its key is the generator banner, and it is
        // instantiated on behalf of the root (identical calls dedup), so its
        // globals are as singular as the root's own — the i18n generator owns a
        // `currentLocale` this way. A hand-written imported module still errors.
        let is_generated = key.starts_with("generated by ");
        if !is_root && !is_generated {
            if let Some(g) = program.globals.first() {
                return Err(vec![Diagnostic::error(
                    g.line,
                    0,
                    "load",
                    format!(
                        "`{key}`: module state is root-only — a top-level `let` may only appear \
                         in the root module (imported modules stay stateless)"
                    ),
                )]);
            }
        }

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
            // Tag tests with their module too (RFC-0015): they still type-check,
            // but `vyrn test <root>` runs only the root's (`None`-module) tests.
            for t in &mut program.tests {
                t.module = Some(key.to_string());
            }
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
                visit(&target, None, opts, resolver, modules, states, stack, root_key)?;
                import_targets[i] = Some(target);
            }
        }
        // Generator-call imports (RFC-0021): run each generator now that its
        // module is loaded, synthesize the module source, and visit it. Identical
        // calls dedup on `gen_key` (already-loaded ⇒ no source, no re-run).
        for (i, imp) in program.imports.iter().enumerate() {
            if let ImportSource::Generator { name, args, line } = &imp.source {
                let (gen_key, gen_source) = run_generator(
                    key, is_root, name, args, *line, opts, resolver, modules, states, root_key,
                )?;
                if let Some(src) = gen_source {
                    visit(&gen_key, Some(&src), opts, resolver, modules, states, stack, root_key)?;
                }
                import_targets[i] = Some(gen_key);
            }
        }
        let import_targets: Vec<String> =
            import_targets.into_iter().map(|t| t.expect("every import resolved")).collect();

        stack.pop();
        states.insert(key.to_string(), true);
        // A module synthesized by a generator (RFC-0021) keeps its source text
        // (its key is the generator banner) so `vyrn emit-gen` can print it.
        let gen_source = key.starts_with("generated by ").then(|| text.clone());
        modules.push(Module { key: key.to_string(), program, import_targets, gen_source });
        Ok(())
    }

    visit(
        &root_key,
        Some(root_source),
        opts,
        resolver,
        &mut modules,
        &mut states,
        &mut stack,
        &root_key,
    )?;

    Ok((modules, root_key))
}

/// Guardrails (RFC-0021): a generator's step budget and output-size cap.
const GEN_FUEL: u64 = 20_000_000;
const GEN_MAX_OUTPUT: usize = 4 * 1024 * 1024;

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
    let arg_repr = consts.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ");

    // 2. The synthesized module's key IS its diagnostic banner. It omits the
    //    line, so two identical calls dedup; different arguments ⇒ different key.
    let gen_key = format!("generated by {name}({arg_repr}) at {importer}");
    if states.contains_key(&gen_key) {
        return Ok((gen_key, None));
    }

    // 3. The generator must be an exported `gen fn` in a module this file loaded.
    let gen_mod_key = modules
        .iter()
        .find(|m| m.program.functions.iter().any(|f| f.name == name && f.is_gen && f.exported))
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

    // 4. Load + check the generator module as a runnable program (its own
    //    comptime-purity is enforced by the check).
    let gen_source = resolver
        .read(&gen_mod_key)
        .map_err(|e| err(format!("cannot re-read generator module `{gen_mod_key}`: {e}")))?;
    let gen_program = load(&gen_source, &gen_mod_key, opts, resolver)?;
    let mut gdiags = crate::checker::check_accum(&gen_program);
    if gdiags.is_empty() {
        gdiags.extend(crate::movecheck::check_accum(&gen_program));
    }
    if !gdiags.is_empty() {
        return Err(gdiags);
    }

    // 5. Content-addressed cache key: generator sources ++ args ++ inputs read.
    let importer_dir = dir_of(importer).to_string();
    let join_dir = |s: &str| -> String {
        normalize(&if importer_dir.is_empty() { s.to_string() } else { format!("{importer_dir}/{s}") })
    };
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
    let sources_hash =
        generator_sources_hash(&gen_source, &gen_mod_key, opts, resolver, name, &arg_repr)?;
    let no_cache = std::env::var("VYRN_NO_GEN_CACHE").is_ok();

    // 5a. Cache hit: every recorded input still hashes as it did ⇒ reuse output.
    if !no_cache {
        if let Some(cached) = resolver.gen_cache_get(&sources_hash) {
            if let Some((inputs, output)) = parse_cache_entry(&cached) {
                if inputs.iter().all(|(path, hash)| current_input_hash(resolver, path) == Some(hash.clone())) {
                    return Ok((gen_key, Some(output)));
                }
            }
        }
    }

    // 5b. Cache miss: run the generator in the mediated sandbox.
    let out = crate::interp::generate(
        &gen_program,
        name,
        &consts,
        crate::interp::GenInputs {
            resolver,
            importer_dir,
            allowed,
            fuel: GEN_FUEL_OVERRIDE.with(|c| c.get()).unwrap_or(GEN_FUEL),
            max_output: GEN_MAX_OUTPUT_OVERRIDE.with(|c| c.get()).unwrap_or(GEN_MAX_OUTPUT),
        },
    )
    .map_err(|trap| err(format!("generator `{name}({arg_repr})` failed: {trap}")))?;
    bump_gen_runs();

    // 6. Cache the output keyed by its recorded inputs, for the next load / the
    //    LSP's per-keystroke re-analysis.
    if !no_cache {
        let inputs: Vec<(String, String)> = out
            .reads
            .iter()
            .map(|(p, bytes)| (p.clone(), crate::hash::sha256_hex(bytes)))
            .collect();
        resolver.gen_cache_put(&sources_hash, &render_cache_entry(&inputs, &out.source));
    }
    Ok((gen_key, Some(out.source)))
}

/// `sha256(sorted generator module sources ++ args)` — the stable part of the
/// cache key (the generator's code + its call). The variable part (which input
/// files it reads) is verified separately at hit time.
fn generator_sources_hash(
    gen_source: &str,
    gen_mod_key: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
    name: &str,
    arg_repr: &str,
) -> Result<String, Vec<Diagnostic>> {
    let graph = module_graph(gen_source, gen_mod_key, opts, resolver)?;
    let mut keys: Vec<String> = graph.into_iter().map(|(k, _)| k).collect();
    keys.sort();
    let mut blob: Vec<u8> = Vec::new();
    for k in &keys {
        let src = resolver.read(k).map_err(|e| {
            vec![Diagnostic::error(0, 0, "load", format!("cannot read `{k}`: {e}"))]
        })?;
        blob.extend_from_slice(k.as_bytes());
        blob.push(0);
        blob.extend_from_slice(src.as_bytes());
        blob.push(0);
    }
    // The generator's own name — one module may export several `gen fn`s, and
    // distinct generators over the same arguments must not share a cache entry.
    blob.extend_from_slice(name.as_bytes());
    blob.push(0);
    blob.extend_from_slice(arg_repr.as_bytes());
    Ok(crate::hash::sha256_hex(&blob))
}

/// The current hash of a recorded generation input — a file (`resolver.read`) or
/// a directory listing (a `dir/` marker, `resolver.list`). `None` if it can no
/// longer be read (a miss: the input vanished).
fn current_input_hash(resolver: &dyn ModuleResolver, path: &str) -> Option<String> {
    if let Some(dir) = path.strip_suffix('/') {
        let mut names = resolver.list(dir).ok()?;
        names.sort();
        Some(crate::hash::sha256_hex(names.join("\n").as_bytes()))
    } else {
        Some(crate::hash::sha256_hex(resolver.read(path).ok()?.as_bytes()))
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
    let mut all_names: HashSet<String> = HashSet::new();
    for m in modules.iter() {
        let set = module_decls.entry(m.key.clone()).or_default();
        let mut add = |n: &str| {
            set.insert(n.to_string());
            all_names.insert(n.to_string());
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
        // Globals are never `export`ed (module state is root-only), so they are
        // not namespace-reachable.
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

    // Pass 1: alias collision checks + decide co-naming renames.
    for m in modules.iter() {
        let mine = module_decls.get(&m.key).cloned().unwrap_or_default();
        let mut locals_seen: HashSet<String> = HashSet::new();
        for imp in &m.program.imports {
            for n in &imp.names {
                let local = n.local().to_string();
                // The alias (or bare name) must not clash with another import's
                // local name, nor — when it differs from the original — with a
                // top-level decl of this module.
                if !locals_seen.insert(local.clone()) {
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
    let namespaced_targets: HashSet<String> =
        ns_bindings.values().flatten().map(|(_, t)| t.clone()).collect();
    for target in &namespaced_targets {
        let exports = module_exports.get(target).cloned().unwrap_or_default();
        // Deterministic order so the minted suffixes are stable across runs.
        let mut names: Vec<&String> = exports.iter().collect();
        names.sort();
        for name in names {
            if name_module_count.get(name).copied().unwrap_or(0) >= 2 {
                foreign_renames
                    .entry((target.clone(), name.clone()))
                    .or_insert_with(|| mint(name, &mut all_names));
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
                    rewrites.entry(m.key.clone()).or_default().insert(n.local().to_string(), resolved);
                } else if resolved != n.original {
                    // A bare (real-name) importer of a co-named decl follows the rename.
                    rewrites.entry(m.key.clone()).or_default().insert(n.original.clone(), resolved);
                }
            }
        }
    }

    // Pass 3: apply the foreign-decl renames (definition + owning module refs).
    for ((target, original), s) in &foreign_renames {
        if let Some(tm) = modules.iter_mut().find(|m| &m.key == target) {
            rename_decl_in_module(&mut tm.program, original, s);
        }
    }

    // Pass 4: apply per-module reference rewrites, and normalize each import to a
    // bare import of the resolved decl name so register/visibility stay unaware.
    for m in modules.iter_mut() {
        if let Some(map) = rewrites.get(&m.key) {
            rewrite_module_refs(&mut m.program, map);
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
        let exported = self.module_exports.get(&target).is_some_and(|s| s.contains(member));
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
            let mut locals: HashSet<String> =
                f.params.iter().map(|pm| pm.name.clone()).collect();
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
            Type::Option(a) | Type::Ref(a) | Type::Array(a) | Type::Task(a) | Type::Partial(a)
            | Type::ArrayN(a, _) | Type::Omit(a, _) | Type::Pick(a, _) => self.rewrite_type(a),
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
            Stmt::Let { name, value, ty, .. } => {
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
            Stmt::If { cond, then_block, else_block, .. } => {
                self.walk_expr(cond, locals);
                let mut inner = locals.clone();
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
            Stmt::ForIn { var, iter, body, .. } => {
                self.walk_expr(iter, locals);
                let mut inner = locals.clone();
                inner.insert(var.clone());
                self.walk_block(body, &mut inner);
            }
            Stmt::Drop { .. } => {}
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
                if let Expr::Field { expr: inner, field: enum_name, .. } = expr.as_ref() {
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
                                *e = Expr::Var { name: variant, line: l };
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
            Expr::Binary { lhs, rhs, .. } => {
                self.walk_expr(lhs, locals);
                self.walk_expr(rhs, locals);
            }
            Expr::Match { scrutinee, arms, line } => {
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
                                let variant =
                                    rest.rsplit('.').next().unwrap_or(rest).to_string();
                                let enum_name =
                                    rest.split('.').next().unwrap_or(rest).to_string();
                                if self.ns.contains_key(&ns) {
                                    let _ = self.resolve_member(&ns, &enum_name, l);
                                    *v = variant;
                                }
                            }
                            for b in binds.iter() {
                                inner.insert(b.clone());
                            }
                        }
                        Pattern::Some(b) | Pattern::Ok(b) | Pattern::Err(b) => {
                            inner.insert(b.clone());
                        }
                        Pattern::None => {}
                    }
                    self.walk_expr(&mut arm.body, &mut inner);
                }
            }
            Expr::ArrayLit { elems, .. } => {
                for e2 in elems.iter_mut() {
                    self.walk_expr(e2, locals);
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
            Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
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

    let mut register =
        |name: &str, module: &str, exported: bool, line: usize, errors: &mut Vec<Diagnostic>| {
            if let Some((prev, _)) = owner.get(name) {
                if prev != module {
                    errors.push(Diagnostic::error(
                        line,
                        0,
                        "load",
                        format!(
                            "`{name}` is defined in both `{prev}` and `{module}` — top-level \
                             names must be unique across the program"
                        ),
                    ));
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
            register(&t.name, &m.key, t.exported, t.line, &mut errors);
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
            register(&f.name, &m.key, f.exported, f.line, &mut errors);
        }
        for p in &m.program.protocols {
            register(&p.name, &m.key, p.exported, p.line, &mut errors);
            for sig in &p.methods {
                method_protocol.insert(sig.name.clone(), p.name.clone());
            }
        }
        // Module-state bindings (RFC-0013) join the top-level namespace: a
        // global may not share a name with any other top-level declaration.
        for g in &m.program.globals {
            register(&g.name, &m.key, false, g.line, &mut errors);
        }
    }

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
                    Some((def_module, _)) => {
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
            for c in fn_body_names(&f.body) {
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
    let mut extra_impls = Vec::new();
    let mut extra_tests = Vec::new();
    // Only generated modules reach here with globals (the root-only check above
    // rejects hand-written imported globals at load time). A generated module is
    // instantiated on behalf of the root, so its module state joins the program
    // and initializes before `main`, after the root's own globals.
    let mut extra_globals = Vec::new();
    for m in modules {
        if m.key == root_key {
            merged = Some(m.program);
        } else {
            let p = m.program;
            extra_types.extend(p.type_decls.into_iter().filter(|t| !is_injected(t)));
            extra_fns.extend(p.functions);
            extra_protocols.extend(p.protocols);
            extra_impls.extend(p.impls);
            extra_globals.extend(p.globals);
            // Imported tests keep their `module` tag: they type-check but do not
            // run under `vyrn test <root>` (RFC-0015).
            extra_tests.extend(p.tests);
        }
    }
    let mut program = merged.expect("root module was loaded");
    program.type_decls.extend(extra_types);
    program.functions.extend(extra_fns);
    program.protocols.extend(extra_protocols);
    program.impls.extend(extra_impls);
    program.globals.extend(extra_globals);
    program.tests.extend(extra_tests);
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

/// Every (callee/constructor name, line) referenced in a block — calls, spawns,
/// struct literals, fallible constructions, and bare variant constructors.
fn fn_body_names(b: &Block) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    fn stmt(s: &Stmt, out: &mut Vec<(String, usize)>) {
        match s {
            Stmt::Let { value, line, ty, .. } => {
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
            Stmt::IndexSet { index, value, line, .. } => {
                expr(index, *line, out);
                expr(value, *line, out)
            }
            Stmt::Return { value: Some(e), line } => expr(e, *line, out),
            Stmt::Return { value: None, .. } => {}
            Stmt::If { cond, then_block, else_block, line } => {
                expr(cond, *line, out);
                block(then_block, out);
                if let Some(eb) = else_block {
                    block(eb, out);
                }
            }
            Stmt::While { cond, body, line } => {
                expr(cond, *line, out);
                block(body, out);
            }
            Stmt::ForIn { iter, body, line, .. } => {
                expr(iter, *line, out);
                block(body, out);
            }
            Stmt::Drop { .. } => {}
            Stmt::Expr(e) => expr(e, 0, out),
            Stmt::Region { body, .. } => block(body, out),
        }
    }
    fn block(b: &Block, out: &mut Vec<(String, usize)>) {
        for s in &b.stmts {
            stmt(s, out);
        }
    }
    fn expr(e: &Expr, line: usize, out: &mut Vec<(String, usize)>) {
        match e {
            Expr::Call { name, args, line } | Expr::Spawn { name, args, line } => {
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
            Expr::Binary { lhs, rhs, line, .. } => {
                expr(lhs, *line, out);
                expr(rhs, *line, out);
            }
            Expr::Match { scrutinee, arms, line } => {
                expr(scrutinee, *line, out);
                for arm in arms {
                    if let Pattern::Variant(v, _) = &arm.pattern {
                        out.push((v.clone(), *line));
                    }
                    expr(&arm.body, *line, out);
                }
            }
            Expr::ArrayLit { elems, line } => {
                for e2 in elems {
                    expr(e2, *line, out);
                }
            }
            // A lambda body (RFC-0023) references names too — walk it so a call
            // or constructor used only inside a lambda is still visibility-checked.
            Expr::Lambda { body, line, .. } => match body {
                LambdaBody::Expr(e2) => expr(e2, *line, out),
                LambdaBody::Block(b2) => block(b2, out),
            },
            Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
        }
    }
    block(b, &mut out);
    out
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
            Type::Option(a) | Type::Ref(a) | Type::Array(a) | Type::Task(a)
            | Type::Partial(a) | Type::ArrayN(a, _) => walk(a, out),
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
        Type::Option(a) | Type::Ref(a) | Type::Array(a) | Type::Task(a) | Type::Partial(a)
        | Type::ArrayN(a, _) | Type::Omit(a, _) | Type::Pick(a, _) => rewrite_type(a, map),
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
        _ => {}
    }
}

/// Rewrite every referenced name in `e` (call/spawn/struct-lit/try-construct
/// callees, bare variables, and match-variant constructors) through `map`.
fn rewrite_expr(e: &mut Expr, map: &HashMap<String, String>) {
    match e {
        Expr::Call { name, args, .. }
        | Expr::Spawn { name, args, .. }
        | Expr::TryConstruct { name, args, .. } => {
            *name = ren(map, name);
            for a in args {
                rewrite_expr(a, map);
            }
        }
        Expr::StructLit { name, fields, .. } => {
            *name = ren(map, name);
            for (_, v) in fields {
                rewrite_expr(v, map);
            }
        }
        Expr::Var { name, .. } => *name = ren(map, name),
        Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
            rewrite_expr(expr, map)
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_expr(lhs, map);
            rewrite_expr(rhs, map);
        }
        Expr::Match { scrutinee, arms, .. } => {
            rewrite_expr(scrutinee, map);
            for arm in arms {
                if let Pattern::Variant(v, _) = &mut arm.pattern {
                    *v = ren(map, v);
                }
                rewrite_expr(&mut arm.body, map);
            }
        }
        Expr::ArrayLit { elems, .. } => {
            for e2 in elems {
                rewrite_expr(e2, map);
            }
        }
        // A lambda body (RFC-0023): rewrite referenced names inside it (its own
        // untyped params are locals, never in `map`, so blanket rewriting is safe).
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e2) => rewrite_expr(e2, map),
            LambdaBody::Block(b2) => rewrite_block(b2, map),
        },
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
    }
}

fn rewrite_block(b: &mut Block, map: &HashMap<String, String>) {
    for s in &mut b.stmts {
        rewrite_stmt(s, map);
    }
}

fn rewrite_stmt(s: &mut Stmt, map: &HashMap<String, String>) {
    match s {
        Stmt::Let { value, ty, .. } => {
            if let Some(t) = ty {
                rewrite_type(t, map);
            }
            rewrite_expr(value, map);
        }
        Stmt::Assign { value, .. } | Stmt::SetField { value, .. } => rewrite_expr(value, map),
        Stmt::IndexSet { index, value, .. } => {
            rewrite_expr(index, map);
            rewrite_expr(value, map);
        }
        Stmt::Return { value: Some(e), .. } => rewrite_expr(e, map),
        Stmt::Return { value: None, .. } => {}
        Stmt::If { cond, then_block, else_block, .. } => {
            rewrite_expr(cond, map);
            rewrite_block(then_block, map);
            if let Some(eb) = else_block {
                rewrite_block(eb, map);
            }
        }
        Stmt::While { cond, body, .. } => {
            rewrite_expr(cond, map);
            rewrite_block(body, map);
        }
        Stmt::ForIn { iter, body, .. } => {
            rewrite_expr(iter, map);
            rewrite_block(body, map);
        }
        Stmt::Drop { .. } => {}
        Stmt::Expr(e) => rewrite_expr(e, map),
        Stmt::Region { body, .. } => rewrite_block(body, map),
    }
}

/// Rewrite one function's signature types and body references through `map`.
fn rewrite_function(f: &mut Function, map: &HashMap<String, String>) {
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
    rewrite_block(&mut f.body, map);
}

/// Rewrite every *reference* (types, calls, variables, bounds) in one module's
/// program through `map`. Declaration names are left alone — a separate step
/// renames a decl when a foreign name must be freed for a co-named local stub.
fn rewrite_module_refs(p: &mut Program, map: &HashMap<String, String>) {
    if map.is_empty() {
        return;
    }
    for f in &mut p.functions {
        rewrite_function(f, map);
    }
    for im in &mut p.impls {
        im.protocol = ren(map, &im.protocol);
        rewrite_type(&mut im.ty, map);
        for m in &mut im.methods {
            rewrite_function(m, map);
        }
    }
    for t in &mut p.type_decls {
        rewrite_type(&mut t.base, map);
        if let Some(pred) = &mut t.predicate {
            rewrite_expr(pred, map);
        }
    }
    for g in &mut p.globals {
        if let Some(t) = &mut g.ty {
            rewrite_type(t, map);
        }
        rewrite_expr(&mut g.init, map);
    }
    for pr in &mut p.protocols {
        for m in &mut pr.methods {
            for t in &mut m.params {
                rewrite_type(t, map);
            }
            rewrite_type(&mut m.ret, map);
        }
    }
    for t in &mut p.tests {
        rewrite_block(&mut t.body, map);
    }
}

/// Every reference name (types and expression callees/variables/variants) used
/// anywhere in a module's declarations — for the RFC-0022 check that an aliased
/// import's original name is not also used directly.
fn program_ref_names(p: &Program) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    let add_block = |b: &Block, out: &mut HashSet<String>| {
        for (n, _) in fn_body_names(b) {
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
    out
}

/// Rename a top-level *declaration* (its defining name) from `from` to `to` in
/// module `p`, and rewrite that module's own references to it. Used to free a
/// foreign name so a co-naming importer's stub can take it (RFC-0022).
fn rename_decl_in_module(p: &mut Program, from: &str, to: &str) {
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
    for g in &mut p.globals {
        if g.name == from {
            g.name = to.to_string();
        }
    }
    let map: HashMap<String, String> = std::iter::once((from.to_string(), to.to_string())).collect();
    rewrite_module_refs(p, &map);
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn map(entries: &[(&str, &str)]) -> MapResolver {
        MapResolver(entries.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    pub(super) fn opts() -> LoadOptions {
        LoadOptions { std_root: Some("std".into()), ..Default::default() }
    }

    fn run_multi(root: &str, files: &[(&str, &str)]) -> Result<i64, String> {
        let program = load(root, "main.vyrn", &opts(), &map(files))
            .map_err(|ds| ds.iter().map(|d| d.render()).collect::<Vec<_>>().join("\n"))?;
        let diags = crate::checker::check_accum(&program);
        if let Some(d) = diags.first() {
            return Err(d.render());
        }
        crate::interp::run(&program)
    }

    fn load_err(root: &str, files: &[(&str, &str)]) -> String {
        match load(root, "main.vyrn", &opts(), &map(files)) {
            Ok(_) => panic!("expected a load error"),
            Err(ds) => ds.iter().map(|d| d.message.clone()).collect::<Vec<_>>().join("\n"),
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
        assert!(e.contains("defined in both"), "{e}");
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
            run_multi(root, &[("shared.vyrn", shared), ("a.vyrn", a), ("b.vyrn", b)]).unwrap(),
            32
        );
    }

    #[test]
    fn non_root_logging_config_is_an_error() {
        let lib = "logging { level: trace } export fn f() -> Int64 { return 1 }";
        let root = "import { f } from \"./lib\" fn main() -> Int64 { return f() }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("only the root module may configure `logging"), "{e}");
    }

    #[test]
    fn non_root_module_state_is_an_error() {
        // RFC-0013: a top-level `let` may only appear in the root module.
        let lib = "let mut count = 0 export fn f() -> Int64 { return count }";
        let root = "import { f } from \"./lib\" fn main() -> Int64 { return f() }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("module state is root-only"), "{e}");
    }

    #[test]
    fn global_name_collides_with_a_function() {
        // A global may not share a name with any other top-level declaration.
        let lib = "export fn tally() -> Int64 { return 1 }";
        let root = "import { tally } from \"./lib\" \
                    let tally = 0 \
                    fn main() -> Int64 { return tally }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("must be unique"), "{e}");
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
        assert_eq!(run_multi(root, &[("a.vyrn", a), ("b.vyrn", b)]).unwrap(), 21);
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
            Err(ds) => ds.iter().map(|d| d.message.clone()).collect::<Vec<_>>().join("\n"),
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
        let e = match load(root, "main.vyrn", &o, &map(&[("gist:demko/abc123/a.vyrn", a)])) {
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
                files: entries.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                cache: RefCell::new(HashMap::new()),
            }
        }
    }
    impl ModuleResolver for CachingResolver {
        fn read(&self, resolved: &str) -> Result<String, String> {
            self.files.get(resolved).cloned().ok_or_else(|| format!("not found: {resolved}"))
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
            self.cache.borrow_mut().insert(key.to_string(), value.to_string());
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
        MapResolver(entries.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
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
            Err(ds) => ds.iter().map(|d| d.message.clone()).collect::<Vec<_>>().join("\n"),
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
        assert!(e.contains("defined in both") || e.contains("unique"), "{e}");
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
    fn generated_module_may_declare_module_state() {
        // RFC-0021/RFC-0020 M2 carve-out: a module SYNTHESIZED BY A GENERATOR may
        // own module state (a hand-written imported module still cannot — see
        // `non_root_module_state_is_an_error`). The generated `currentLocale`-style
        // global initializes before `main` and persists across handler calls made
        // from the root module (the setLocale/locale + t() shape).
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
        assert_eq!(run_with(root_b, &r).unwrap(), 2, "generator `b` must not reuse `a`'s cache");
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
}
