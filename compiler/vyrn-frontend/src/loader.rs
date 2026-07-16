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

fn link(modules: Vec<Module>, root_key: &str) -> Result<Program, Vec<Diagnostic>> {
    let mut errors: Vec<Diagnostic> = Vec::new();

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
            for name in &imp.names {
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
