//! Module loader and linker. Lexes and parses each file, resolves
//! imports recursively, and links everything into one [`Program`], so the passes
//! after it never see modules. All I/O goes through [`ModuleResolver`]. The
//! loader refuses import cycles, an imported name that is missing or not
//! exported, a top-level name defined twice, a foreign name used without an
//! import (importing an enum or protocol brings its variants or methods), a
//! `logging` block outside the root, and two impls of one `(protocol, type)`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::{lexer, parser};

/// Provides module source text for a resolved specifier: a normalized,
/// slash-separated path (see [`resolve_spec`]).
pub trait ModuleResolver {
    fn read(&self, resolved: &str) -> Result<String, String>;
    /// Lists the bare entry names directly under the directory `resolved`,
    /// without `.` and `..`. Only generation-time `listDir` calls it.
    /// The default is unsupported.
    fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
        Err(crate::trap::io_at("listerr", resolved))
    }
    /// Lists like `list`, but a directory's name ends in `/`. A walker
    /// needs the kind because `list`'s error cannot tell "not a directory" from
    /// "unreadable". The default is unsupported.
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        Err(crate::trap::io_at("listerr", resolved))
    }
    /// Returns a cached generator output by its content-address key.
    /// The default is a permanent miss.
    fn gen_cache_get(&self, _key: &str) -> Option<String> {
        None
    }
    /// Stores a generator output under its content-address key. The default is a
    /// no-op. A failure is swallowed: the cache is never a correctness dependency.
    fn gen_cache_put(&self, _key: &str, _value: &str) {}
}

thread_local! {
    /// Generator bodies run (cache misses) on this thread. Thread-local, so each
    /// parallel test sees its own count.
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
    /// Test overrides of the generator budgets; `None` means the default.
    static GEN_FUEL_OVERRIDE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    static GEN_MAX_OUTPUT_OVERRIDE: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// Lowers the two generator budgets on this thread, so a test reaches one fast.
/// `None` restores the default.
///
/// Public for `tests/loader_run.rs`: running a `gen fn` needs the driver's engine.
pub fn set_gen_budgets_for_test(fuel: Option<u64>, max_output: Option<usize>) {
    GEN_FUEL_OVERRIDE.with(|c| c.set(fuel));
    GEN_MAX_OUTPUT_OVERRIDE.with(|c| c.set(max_output));
}

/// A resolver over an in-memory map, for tests.
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
            return Err(crate::trap::io_at("listerr", resolved));
        }
        Ok(names.into_iter().collect())
    }
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        // A segment with more path after it is a directory; an exact key is a
        // file. If both hold (keys `a/b` and `a/b/c`), the directory wins,
        // because a walker acts on it.
        let prefix = format!("{}/", resolved.trim_end_matches('/'));
        let mut dirs: std::collections::BTreeSet<String> = Default::default();
        let mut files: std::collections::BTreeSet<String> = Default::default();
        let mut any_under = false;
        for key in self.0.keys() {
            if let Some(rest) = key.strip_prefix(&prefix) {
                any_under = true;
                match rest.split_once('/') {
                    Some((seg, _)) if !seg.is_empty() => {
                        dirs.insert(seg.to_string());
                    }
                    None if !rest.is_empty() => {
                        files.insert(rest.to_string());
                    }
                    _ => {}
                }
            }
        }
        if !any_under {
            return Err(crate::trap::io_at("listerr", resolved));
        }
        let mut out: Vec<String> = Vec::new();
        for d in &dirs {
            out.push(format!("{d}/"));
        }
        for f in files {
            if !dirs.contains(&f) {
                out.push(f);
            }
        }
        Ok(out)
    }
}

/// A resolver over the filesystem.
///
/// The driver and every suite share this one copy, so no suite loads a corpus
/// the driver would refuse. The listings sort, so a generator that walks a
/// directory produces the same module on two machines. An entry
/// whose type cannot be read counts as a file; the walk then reports the real
/// error at that entry.
pub struct DiskResolver;

impl ModuleResolver for DiskResolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        std::fs::read_to_string(resolved).map_err(|e| e.to_string())
    }
    fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
        let mut names = read_dir_names(resolved, false)?;
        names.sort();
        Ok(names)
    }
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        let mut names = read_dir_names(resolved, true)?;
        names.sort();
        Ok(names)
    }
    fn gen_cache_get(&self, key: &str) -> Option<String> {
        crate::manifest::gen_cache_get(key)
    }
    fn gen_cache_put(&self, key: &str, value: &str) {
        crate::manifest::gen_cache_put(key, value)
    }
}

/// The entry names directly under `dir`, unsorted; with `kinds`, a directory's
/// name carries a trailing `/`. The error is the project's own
/// `listerr` wording, never the operating system's.
fn read_dir_names(dir: &str, kinds: bool) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir).map_err(|_| crate::trap::io_at("listerr", dir))?;
    Ok(entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if kinds && e.file_type().is_ok_and(|t| t.is_dir()) {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect())
}

/// Forwards every call to an inner resolver and records each successful `read`
/// as `(resolved key, content)`. `moduleInterface` links a module's
/// type closure, and every module that link reads joins the generator's cache
/// inputs.
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
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        self.inner.list_kinds(resolved)
    }
    fn gen_cache_get(&self, key: &str) -> Option<String> {
        self.inner.gen_cache_get(key)
    }
    fn gen_cache_put(&self, key: &str, value: &str) {
        self.inner.gen_cache_put(key, value)
    }
}

/// Normalizes a slash-separated path lexically: resolves `.` and `..` and
/// collapses duplicate separators.
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

/// Returns the import specifier for the module at resolved key `key`, written
/// from a file in `importer_dir`. A `std/` module keeps its `std/`
/// specifier and a remote module its key; a local module becomes a relative path
/// without `.vyrn`. It inverts [`resolve_spec`] for these cases.
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

/// The file name a `panic` in this module reports.
///
/// Not the module key: a key can be absolute, and a shipped wasm module would
/// carry the build machine's directory layout. The root module gets its base
/// name; every other module gets the specifier an import would spell, plus
/// `.vyrn`. Two machines building one program bake the same bytes.
fn site_file(key: &str, root_key: &str, std_root: Option<&str>) -> String {
    if key == root_key {
        return key.rsplit('/').next().unwrap_or(key).to_string();
    }
    // A generated module's key is a banner ending in its importer's path.
    // Name the generator and the file it was generated for.
    if let Some(importer) = generated_importer(key) {
        // Spell the control-character separators as ` at ` and drop the
        // trailing one the suffix strip leaves.
        let head = key
            .strip_suffix(importer)
            .unwrap_or(key)
            .trim_end()
            .trim_end_matches(GEN_SEP);
        return format!(
            "{} {}",
            readable(head),
            site_file(importer, root_key, std_root)
        );
    }
    let spec = import_specifier(dir_of(root_key), key, std_root);
    format!("{}.vyrn", spec.strip_prefix("./").unwrap_or(&spec))
}

/// Rewrites every `panic(msg)` in `program` to [`PANIC_AT`]`(msg, "file:line")`,
/// in every body [`crate::project::walk_program`] visits.
fn stamp_panic_sites(program: &mut Program, file: &str) {
    let mut stamp = |e: &mut Expr| {
        if let Expr::Call {
            dot: _,
            name,
            args,
            line,
            type_args: _,
            id: _,
        } = e
        {
            if name == "panic" && args.len() == 1 {
                *name = PANIC_AT.to_string();
                args.push(Expr::Str(format!("{file}:{line}"), Id::NEW));
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

/// The separator in a generated module's banner key. A control character cannot
/// occur in a path, so the banner splits whatever the importer's directory is.
pub(crate) const GEN_SEP: &str = "\u{1f}";

/// `key` as a reader sees it: every [`GEN_SEP`] spelled ` at `, so no diagnostic prints the
/// control character (#589).
pub(crate) fn readable(key: &str) -> String {
    key.replace(GEN_SEP, " at ")
}

/// Returns the importer file a generated module's banner key names
/// (`generated by <fn>(<args>)` + [`GEN_SEP`] + `<importer>`), or
/// `None` for any other key. A generated module's relative imports, visibility
/// and audience resolve against this file.
///
/// A nested banner's importer is itself a banner; the loop unwraps to a real
/// file. A banner without the separator splits on the last `" at "`, because
/// cached generations outlive the compiler that wrote them.
pub fn generated_importer(key: &str) -> Option<&str> {
    let mut rest = key.strip_prefix("generated by ")?;
    loop {
        let next = if let Some(i) = rest.find(GEN_SEP) {
            &rest[i + GEN_SEP.len()..]
        } else if let Some(i) = rest.rfind(" at ") {
            &rest[i + 4..]
        } else {
            return None;
        };
        match next.strip_prefix("generated by ") {
            Some(inner) => rest = inner,
            None => return Some(next),
        }
    }
}

/// Whether a specifier/key is remote (`github:`, `gist:`, `https:`).
pub fn is_remote(spec: &str) -> bool {
    spec.starts_with("github:") || spec.starts_with("gist:") || spec.starts_with("https://")
}

/// The immutable base of a remote key (`github:o/r@ref`, `gist:u/id[@rev]`,
/// or `https://host`). Relative imports inside a remote module stay under it,
/// so a remote file never reads your disk or climbs out of its pinned tree.
fn remote_base(key: &str) -> Option<String> {
    if let Some(rest) = key.strip_prefix("github:") {
        let at = rest.find('@')?;
        let slash = rest[at + 1..].find('/')?;
        return Some(format!("github:{}", &rest[..at + 1 + slash]));
    }
    if let Some(rest) = key.strip_prefix("gist:") {
        // gist:user/id[@rev]/file: the base is user/id[@rev].
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

/// Returns the fixed export list of a std module that only names ambient
/// builtins (`std/result`, `std/option`), or `None` for any other
/// specifier. Importing one checks the names and binds nothing. The rows live in
/// `symbols::BUILTIN_TYPES_AND_CTORS`.
pub fn builtin_alias_exports(spec: &str) -> Option<Vec<&'static str>> {
    let names: Vec<&'static str> = crate::symbols::BUILTIN_TYPES_AND_CTORS
        .iter()
        .filter(|(_, m, _, _)| *m == spec)
        .map(|(n, _, _, _)| *n)
        .collect();
    (!names.is_empty()).then_some(names)
}

/// Resolves an import specifier written inside `importer` to a module key.
///
/// The editor reuses it for go-to-definition on an import path. The
/// key is a local slash path for a relative or `std/` specifier (with `.vyrn`
/// appended when it has no extension), or a remote key. It reads no file.
pub fn resolve_spec(spec: &str, importer: &str, opts: &LoadOptions) -> Result<String, String> {
    // A generated module resolves its imports against the file that triggered
    // it, named in its banner key.
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
    // A remote specifier is its own key; the CLI's resolver fetches it through
    // the lockfile, the cache or the network.
    if is_remote(spec) {
        let key = normalize_remote(&with_ext(spec.to_string()));
        remote_base(&key).ok_or_else(|| format!("malformed remote specifier `{spec}`"))?;
        return Ok(key);
    }
    if spec.starts_with("./") || spec.starts_with("../") {
        // Inside a remote module, a relative import stays under the pinned base:
        // never onto the local disk, never above the anchor.
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
    // A bare specifier resolves through the manifest's dependency map; the target
    // is itself a specifier, rooted at the manifest's directory. A remote module
    // has no manifest, so its bare specifiers are errors.
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

/// Options for a load: the std root and the manifest's dependency aliases.
/// `aliases` maps a bare specifier (`"pad"`) to a real one; a relative target
/// resolves against `alias_base`, not the importing file.
#[derive(Default)]
pub struct LoadOptions {
    pub std_root: Option<String>,
    pub aliases: std::collections::HashMap<String, String>,
    /// The manifest's directory, slash-separated; empty means the current one.
    pub alias_base: String,
    /// The project's audience vocabulary, or `None` when `vyrn.json`
    /// has no `audience` key. `None` makes every module universal.
    pub audience: Option<crate::audience::AudienceMap>,
    /// The artifacts the manifest declares, or `None`. The floor
    /// ([`crate::floor`]) runs only when the root is one artifact's entry point.
    pub artifacts: Option<crate::artifacts::ArtifactMap>,
}

/// Returns the objection, if any, to `importer` importing `imported`.
/// `None` when the project declares no `audience` key.
///
/// The message names both files, because the objection is about the edge. The
/// note cites the `vyrn.json` key that decided it.
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
    let d = Diagnostic::error(
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
    Some(d.with_note(format!(
        "audience `{}` is declared by vyrn.json:{} — {}; the importer's own audience comes from {}",
        to.audience,
        to.audience.key(),
        audience::remedy(to.audience, importer, imported, map),
        from.because()
    )))
}

/// Refuses an import of `std/mem` from anywhere but `std/runtime`, and of
/// `std/runtime` from anywhere.
///
/// The compiler states this audience, not `vyrn.json`, so no manifest widens it
/// and it runs whether or not the project opts into audiences. The diagnostic
/// keeps the audience wording. Identity is by resolved path, so a
/// project file named `std/runtime.vyrn` gets no primitives.
fn runtime_fence(
    importer: &str,
    imported: &str,
    line: usize,
    opts: &LoadOptions,
) -> Option<Diagnostic> {
    // A key and a std spec can spell one file two ways (`vyrn check
    // std/runtime.vyrn` is relative to the shell, the std root absolute), so
    // identity falls back to the real path.
    let is = |key: &str, spec: &str| {
        let Ok(k) = resolve_spec(spec, importer, opts) else {
            return false;
        };
        key == k
            || matches!(
                (crate::manifest::real_path(key), crate::manifest::real_path(&k)),
                (Some(a), Some(b)) if a == b
            )
    };
    let fenced = if is(imported, MEM_SPEC) {
        if is(importer, RUNTIME_SPEC) {
            return None;
        }
        MEM_SPEC
    } else if is(imported, RUNTIME_SPEC) {
        RUNTIME_SPEC
    } else {
        return None;
    };
    let shown = match &opts.audience {
        Some(map) => crate::audience::display_path(importer, map),
        None => importer.to_string(),
    };
    let d = Diagnostic::error(
        line,
        0,
        "audience",
        format!("`{shown}` cannot import `{fenced}`, whose audience is the runtime"),
    );
    Some(d.with_note(format!(
        "audience `{RUNTIME_SPEC}` is declared by the compiler, not by \
         vyrn.json; the safe surface over `std/mem` is what `{RUNTIME_SPEC}` exports, \
         and the compiler links that into every program"
    )))
}

/// One parsed module awaiting linking.
struct Module {
    key: String,
    program: Program,
    /// The resolved key each import points at, in `program.imports` order.
    import_targets: Vec<String>,
    /// The generated source text, for `vyrn emit-gen`; `None` for a
    /// module read from disk.
    gen_source: Option<String>,
    /// `Some(prefix)` when a builtin injected this module and nothing imported
    /// it. Its declarations are renamed to that reserved prefix (see
    /// [`RT_PREFIX`]), so they neither collide with nor are captured by a user's.
    injected: Option<&'static str>,
}

/// The state one load walks: the modules entered, their loading state, the
/// generated-module identities, the origin maps, the warnings and the
/// cycle stack.
struct Work {
    modules: Vec<Module>,
    /// `false` = loading, `true` = loaded.
    states: HashMap<String, bool>,
    /// A generator import's resolved-inputs key, mapped to the banner of the first
    /// module synthesized for it. Two imports whose path arguments
    /// resolve alike, however spelled, share one module and its state.
    identities: HashMap<String, String>,
    origins: crate::origin::OriginMaps,
    /// Warnings of a load that succeeds. They travel beside the
    /// program and never change an exit code or its output.
    warnings: Vec<Diagnostic>,
    /// The modules on the path to the one being entered, for the cycle report.
    stack: Vec<String>,
}

/// The prefix every declaration of an injected runtime module is renamed to.
/// `$` is not an identifier character, so no source can spell these names: a
/// desugar calling `json$emit` is never captured by a program's `fn emit`, and
/// `link`'s uniqueness check never reports a module the user did not import.
pub const RT_PREFIX: &str = "json$";

/// The module the `toJson` desugar links: `std/json`'s value tree and writer.
pub const RT_JSON_SPEC: &str = "std/json";

/// The `std/json` generator that writes `toJson`'s encoders, under its reserved
/// name. `toJson(x)` is `derive(jsonEncoders, x)`.
pub const JSON_ENCODERS: &str = "json$jsonEncoders";

/// A Vyrn module a builtin's implementation lives in, and the reserved prefix
/// its declarations are renamed to.
pub struct RtModule {
    /// The import spec, resolved against the std root like any other.
    pub spec: &'static str,
    /// The prefix every declaration of the module is renamed to (see
    /// [`RT_PREFIX`]).
    pub prefix: &'static str,
    /// Builtins whose mention links the module but whose lowering stays a
    /// compiler part: `toJson` needs its argument's static type, so only the
    /// serializer lives in the module.
    pub desugared: &'static [&'static str],
    /// Linked into every program, mentioned or not. [`RUNTIME_SPEC`] is, because
    /// the wasm emitter calls it from lowerings no builtin names (a `String`
    /// comparison calls `strCmp`). `std/text` is, because the runtime's `intStr`
    /// mentions `stringFromBytes` after the mention scan has run.
    ///
    /// A second scan pass would inject `std/text` for every program too, and
    /// would change the load order and so every emitted index. The direct
    /// backend's sweep drops an unreached function's code and data.
    pub always: bool,
}

/// The module the compiler carries as its runtime. It is the one
/// member of `std/mem`'s audience, and nothing may import it.
pub const RUNTIME_SPEC: &str = "std/runtime";

/// The raw-memory primitives. Their audience is
/// `{ std/runtime }`, declared here and not in any `vyrn.json`, so no manifest
/// widens it. [`runtime_fence`] is the check.
pub const MEM_SPEC: &str = "std/mem";

/// Whether `spec` is behind the runtime fence. Listings for readers (`vyrn doc
/// --std`, the site's reference) leave these modules out; one predicate keeps
/// the fence and the listings in agreement.
pub fn is_fenced(spec: &str) -> bool {
    spec == MEM_SPEC || spec == RUNTIME_SPEC
}

/// The reserved prefix of every `std/mem` declaration. The emitter lowers a call
/// to one of them to one instruction, not a `call`.
pub const MEM_PREFIX: &str = "mem$";

/// The reserved prefix of every `std/runtime` declaration.
pub const RUNTIME_PREFIX: &str = "runtime$";

/// Whether this build is audited: `VYRN_LEAK_CHECK` set to anything but `0` in
/// the compiler's environment.
///
/// It selects `std/runtime`'s accounting allocator, the `audit` calls, the
/// module-state teardown and the lowering's `<teardown>` root. It is a build
/// flag, so `malloc` has no branch and an unaudited build stays byte-identical
/// under `VYRN_WASM_MANIFEST=check`. The emitter and the lowering both read it
/// here. A generator host is never audited: its exit code is a protocol with
/// the compiler, and a residue report would fail the build.
pub fn audit_build() -> bool {
    std::env::var_os("VYRN_LEAK_CHECK").is_some_and(|v| !v.is_empty() && v != "0")
        && !crate::checker::gen_host()
}

/// Whether `name` is one of `std/runtime`'s audit hooks, which a build emits
/// only when [`audit_build`] says it is audited.
pub fn audit_hook(name: &str) -> bool {
    name.strip_prefix(RUNTIME_PREFIX)
        .is_some_and(|n| n.starts_with("audit"))
}

/// Every runtime module, in load order.
pub const RT_MODULES: &[RtModule] = &[
    // `fromJson` links this too: its decoders walk the `Json` tree declared
    // here. Desugared, because both builtins need the argument's static type.
    RtModule {
        spec: RT_JSON_SPEC,
        prefix: RT_PREFIX,
        desugared: &["toJson", "fromJson"],
        always: false,
    },
    // `fromJson`'s untyped half: the reader, the `Issue` vocabulary, the
    // path arithmetic and the scalar decoders. `jsondec` generates the typed half
    // per target type, and it calls in here.
    RtModule {
        spec: "std/jsondec",
        prefix: "jsondec$",
        desugared: &["fromJson"],
        always: false,
    },
    // `stringFromBytes` is a desugar: only its check ([`STRING_FAULT`]) lives
    // here, and the backend builds the `String`, which needs `std/mem`.
    //
    // `always`, because the runtime's `intStr` builds its digits with
    // `stringFromBytes`, the only route from bytes to a `String`, and the runtime
    // enters after the mention scan. See [`RtModule::always`].
    RtModule {
        spec: "std/text",
        prefix: "text$",
        desugared: &["stringFromBytes"],
        always: true,
    },
    // The float formatter. Desugared, because `@str` is type-directed and only
    // its float case is a call. `print` formats a float without `@str`, so it
    // links the module too. Nearly every program links it; the direct backend's
    // sweep (`Module::sweep`) drops it from a program that formats no float.
    RtModule {
        spec: "std/num",
        prefix: "num$",
        // `assertEq` renders a mismatched float the way `@str` does.
        desugared: &["@str", "print", "assertEq"],
        always: false,
    },
    // The runtime, in every program. `std/mem` enters as `std/runtime`'s import;
    // its row only names its prefix.
    RtModule {
        spec: RUNTIME_SPEC,
        prefix: RUNTIME_PREFIX,
        desugared: &[],
        always: true,
    },
    RtModule {
        spec: MEM_SPEC,
        prefix: MEM_PREFIX,
        desugared: &[],
        always: false,
    },
];

impl RtModule {
    /// Builtins that are one of the module's exported functions:
    /// `(builtin, reserved spelling of the function)`, the rows whose
    /// [`crate::prelude::Builtin::route`] carries the module's prefix. The
    /// call is the whole implementation.
    pub fn routes(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        crate::prelude::builtins()
            .iter()
            .filter_map(|b| Some((b.name, b.route?)))
            .filter(|(_, f)| f.starts_with(self.prefix))
    }
}

/// The reserved spelling of `std/num`'s float formatter, reached from `@str` and
/// `print`. `the_float_formatter_is_std_nums` checks it against the table.
///
/// Not a route: a route renames a whole builtin, and only the float case of
/// `@str` is a call.
pub const F64_STR: &str = "num$f64Str";

/// The reserved spelling of `std/text`'s byte check, the one statement of what a
/// `String` may hold, reached from `stringFromBytes`. It answers 0 for bytes that
/// can be a `String`, 1 for an embedded NUL and 2 for bytes that are not UTF-8;
/// [`crate::trap::io`]'s `bnul` and `butf8` word 1 and 2.
///
/// Not a route: the build of the `String` allocates and stays in the backend,
/// behind `std/mem`'s fence. `the_string_check_is_std_texts` checks the spelling.
pub const STRING_FAULT: &str = "text$stringFault";

/// Returns the reserved spelling a routed builtin's call becomes, or `None` for
/// a name no runtime module implements. A generator host takes the row's
/// generation twin, which reads the resource through the loader's resolver
/// (`vyrn_gen.read`) rather than WASI.
pub fn routed_builtin(name: &str) -> Option<&'static str> {
    let b = crate::prelude::builtin(name)?;
    b.gen_route
        .filter(|_| crate::checker::gen_host())
        .or(b.route)
}

/// Returns the function a builtin call calls where its argument's type or name
/// names the callee, and the arguments it hands on; `None` for any other call.
/// `ty_of` answers an argument's static type.
///
/// `contractOf(C)` calls the entry `vyrn-genwasm` appends for `C` and hands on
/// nothing. `toJson(x)` calls what [`JSON_ENCODERS`] wrote for `x`'s type, and
/// `fromJson<T>(s)` the decoder of `T`, which [`crate::check_and_synthesize`]
/// appends. The builder and the emitter treat it as a call only where the
/// program declares it.
pub fn routed_callee<'e>(
    name: &str,
    type_args: &[Type],
    args: &'e [Expr],
    ty_of: impl FnOnce(&Expr) -> Option<Type>,
) -> Option<(String, &'e [Expr])> {
    match (name, type_args, args) {
        ("contractOf", _, [Expr::Var { name: c, .. }]) => {
            Some((crate::checker::gen_entry_contract_of(c), &[]))
        }
        ("toJson", _, [a]) => Some((crate::gen::derived_name(JSON_ENCODERS, &ty_of(a)?), args)),
        ("derive", _, [Expr::Var { name: g, .. }, a]) => {
            Some((crate::gen::derived_name(g, &ty_of(a)?), &args[1..]))
        }
        ("fromJson", [t], [_]) => Some((crate::jsondec::top_name(t), args)),
        _ => None,
    }
}

/// Returns the generated source of every generator module reachable from the
/// root, as `(banner, source)` pairs in load order, for
/// `vyrn emit-gen`. Runs the whole load, cache included, and discards the link.
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

/// Loads `root_source` (read from `root_path`) and every module it imports
/// transitively, and links them into one [`Program`].
///
/// # Errors
///
/// Returns every diagnostic found so far; each carries its file in
/// [`Diagnostic::file`].
pub fn load(
    root_source: &str,
    root_path: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Result<Program, Vec<Diagnostic>> {
    load_with_origins(root_source, root_path, opts, resolver).0
}

/// The warnings of a load that succeeds, in module-entry order.
pub type Warnings = Vec<Diagnostic>;

/// Like [`load`], and also returns the origin maps of every generated
/// module reachable from the root, the load's warnings, and its module graph.
///
/// The maps come back whether or not the load succeeds: they are a line-scan of
/// each generated text, so a `.vyx` whose template fails to lex still maps its
/// lines. The returned diagnostics are already remapped. A failed load
/// returns no warnings. The graph is the one [`module_graph_with_sources`]
/// derives; the symbol indexer needs it for `import * as ns`, and rebuilding it
/// there would run a second whole load on every keystroke.
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
    // A fresh epoch for the outermost load only; see `current_input_hash`.
    let depth = LOAD_DEPTH.with(|d| {
        d.set(d.get() + 1);
        d.get()
    });
    if depth == 1 {
        LOAD_EPOCH.with(|e| e.set(e.get().wrapping_add(1)));
        MODULE_HASHES.with(|m| m.borrow_mut().clear());
    }
    if depth > GEN_DEPTH_MAX {
        LOAD_DEPTH.with(|d| d.set(d.get() - 1));
        // Each nested generator load gets a fresh module-state map, so the
        // import-cycle check never sees a generator that mints a growing
        // argument (`g(x + "1")` from `g(x)`). The depth bound turns that stack
        // overflow into a named error.
        return (
            Err(vec![Diagnostic::error(
                0,
                0,
                "load",
                format!(
                    "generator imports nest more than {GEN_DEPTH_MAX} deep — a generator \
                     likely imports itself with a growing argument"
                ),
            )]),
            crate::origin::OriginMaps::default(),
            Vec::new(),
            Vec::new(),
        );
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
    let read_parse = crate::prof::phase("load: read+parse+resolve");
    let loaded = load_modules(root_source, root_path, opts, resolver);
    drop(read_parse);
    match loaded {
        Err((diags, origins)) => (Err(diags), origins, Vec::new(), Vec::new()),
        Ok((modules, root_key, origins, warnings)) => {
            let _p = crate::prof::phase("load: link");
            let graph = graph_of(&modules);
            (link(modules, &root_key), origins, warnings, graph)
        }
    }
}

/// `(module key, resolved import targets, synthesized source)` per loaded module.
pub type ModuleGraph = Vec<(String, Vec<String>, Option<String>)>;

/// `module key -> content hash` for the modules the last outermost load visited.
/// The kernel's judgment memo keys a body on it ([`crate::movecheck::Judgments`]).
/// Valid until the next load begins.
pub fn last_module_hashes() -> HashMap<String, String> {
    MODULE_HASHES.with(|m| m.borrow().clone())
}

/// The floor's [`crate::floor::Graph`] for a linked load: every module the
/// artifact contains, including a generator's output and the runtime
/// modules a desugar injects, which no resolver can read.
fn floor_graph(modules: &mut [Module]) -> crate::floor::Graph {
    modules
        .iter_mut()
        .map(|m| {
            (
                m.key.clone(),
                m.import_targets.clone(),
                crate::floor::carried(&mut m.program),
            )
        })
        .collect()
}

/// The floor's graph and the load's root key, for `vyrn why --capability`.
///
/// It is the graph the check walks ([`floor_graph`]), so the report sees a
/// capability only a generated module carries (a client stub's `vyrnRpcCall`
/// `extern`). The caller arms the refusing policies: `vyrn why` clears
/// `opts.audience` and `opts.artifacts` to get the graph instead of the
/// objection.
pub fn capability_graph(
    root_source: &str,
    root_path: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Result<(crate::floor::Graph, String), Vec<Diagnostic>> {
    let (mut modules, root_key, _, _) =
        load_modules(root_source, root_path, opts, resolver).map_err(|(d, _)| d)?;
    Ok((floor_graph(&mut modules), root_key))
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

/// Returns every `(module key, resolved import targets)` pair reachable from the
/// root, for `vyrn deps`.
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

/// Like [`module_graph`], and each entry also carries a generated module's source,
/// `None` for a file. The symbol indexer lists the exports of an
/// `import * as ns from gen(..)` namespace from it, since no resolver can read a
/// banner key.
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

/// Loads every module reachable from the root, and returns them with the root
/// key, the origin maps and the warnings.
///
/// A generated module's map is built when the module is entered, from its text
/// alone, so a module that fails to lex or parse still has one. The
/// error path remaps through [`crate::origin::OriginMaps::remap`]. An error on a
/// line no directive governs keeps its generated location and the note.
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
    // A deferred floor decision belongs to one load. Nobody must check the
    // program a load returns, so the next outermost load drops it.
    if LOAD_DEPTH.with(|d| d.get()) <= 1 {
        crate::floor::forget();
    }
    let mut w = Work {
        modules: Vec::new(),
        states: HashMap::new(),
        identities: HashMap::new(),
        origins: crate::origin::OriginMaps::new(),
        warnings: Vec::new(),
        stack: Vec::new(),
    };

    fn visit(
        key: &str,
        source: Option<&str>,
        opts: &LoadOptions,
        resolver: &dyn ModuleResolver,
        w: &mut Work,
        root_key: &str,
    ) -> Result<(), Vec<Diagnostic>> {
        match w.states.get(key) {
            Some(true) => return Ok(()), // already loaded
            Some(false) => {
                let cycle: Vec<&str> = w.stack.iter().map(|s| s.as_str()).collect();
                return Err(vec![Diagnostic::error(
                    0,
                    0,
                    "load",
                    format!("import cycle: {} -> {key}", cycle.join(" -> ")),
                )]);
            }
            None => {}
        }
        w.states.insert(key.to_string(), false);
        w.stack.push(key.to_string());

        let _read = crate::prof::phase("read");
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
        drop(_read);
        crate::prof::read_lines(text.lines().count());
        let is_root = key == root_key;

        // Register a generated module's `//@origin` table before it is lexed:
        // the table is a line-scan, so a module that never parses
        // still maps its lex and parse errors onto its input file.
        if key.starts_with("generated by ") {
            let importer = generated_importer(key).unwrap_or(key);
            // Both scans honour a `//@origin` or `//@diag` only where the lexer
            // says a comment begins, so a string literal a generator copied from
            // its input is data, not a control line.
            // The project that bounds a directive is the manifest's directory, or
            // without a manifest the entry file's directory. A bound at the
            // filesystem root made the refusal depend on the project's depth.
            let project = if opts.alias_base.is_empty() {
                dir_of(root_key)
            } else {
                opts.alias_base.as_str()
            };
            let ctx = crate::origin::Context::new(&text, dir_of(importer), project);
            w.origins.add_module(key, &text, &ctx);
            // The same line-scan lifts `//@diag` directives into diagnostics at
            // the generator's severity. A page is generated twice and a
            // generator can be re-entered, so de-duplicate on file, line and
            // message, never on the banner.
            let mut errors: Vec<Diagnostic> = Vec::new();
            for d in crate::origin::diagnostics(key, &text, &ctx) {
                let seen = |list: &[Diagnostic]| {
                    list.iter().any(|w: &Diagnostic| {
                        w.file == d.file && w.line == d.line && w.message == d.message
                    })
                };
                match d.severity {
                    // A generator's error fails the load, reported here so it
                    // keeps the anchor the generator gave.
                    crate::diagnostics::Severity::Error => {
                        if !seen(&errors) {
                            errors.push(d);
                        }
                    }
                    crate::diagnostics::Severity::Warning => {
                        if !seen(&w.warnings) {
                            w.warnings.push(d);
                        }
                    }
                }
            }
            if !errors.is_empty() {
                return Err(errors);
            }
        }

        // A `.json` module is a JSON Schema document: synthesize validated type
        // declarations from it instead of parsing Vyrn. It imports
        // nothing.
        if key.ends_with(".json") {
            let decls = crate::schema::synthesize(&text, None, key)
                .map_err(|e| vec![Diagnostic::error(0, 0, "load", e)])?;
            w.modules.push(Module {
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
                    surface_shadows: std::collections::HashSet::new(),
                    log_sink: LogSink::Stderr,
                    nodes: 0,
                },
                import_targets: Vec::new(),
                gen_source: None,
                injected: None,
            });
            w.stack.pop();
            w.states.insert(key.to_string(), true);
            return Ok(());
        }
        // Lex and parse, memoized on the module's text: a keystroke changes one
        // module, and the text is the whole input to the parse. The per-module
        // attribution below depends on `key`, so it runs after the cache: one
        // text loaded under two keys gives two modules from one parse. Only
        // successes are cached.
        let mut program = {
            // Non-cryptographic: this key never leaves the process.
            let hash = {
                let mut h: u64 = 0xcbf29ce484222325;
                for b in text.as_bytes() {
                    h ^= *b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                format!("{h:x}:{}", text.len())
            };
            MODULE_HASHES.with(|m| m.borrow_mut().insert(key.to_string(), hash.clone()));
            if let Some(hit) = {
                let _p = crate::prof::phase("parse (cache hit)");
                PARSE_CACHE.with(|c| c.borrow().get(&hash).cloned())
            } {
                hit
            } else {
                let _p = crate::prof::phase("parse");
                let tokens = lexer::lex(&text).map_err(|d| vec![in_module(d, key, root_key)])?;
                let (parsed, errors) = parser::parse_accum(tokens);
                if !errors.is_empty() {
                    return Err(errors
                        .into_iter()
                        .map(|d| in_module(d, key, root_key))
                        .collect());
                }
                PARSE_CACHE.with(|c| {
                    let mut c = c.borrow_mut();
                    // ponytail: a keystroke leaves the old text's entry behind.
                    // Cleared at 512 entries; an LRU if it ever matters.
                    if c.len() > 512 {
                        c.clear();
                    }
                    c.insert(hash, parsed.clone());
                });
                parsed
            }
        };

        // Only the root configures logging. A default is indistinguishable from
        // unset, and both behave the same.
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

        // Module state is legal in any module: module-private, one
        // instance per process, initialized in linker order, never exported.

        // Attribute decls to this module. The root stays `None`.
        if !is_root {
            for slot in decl_modules_mut(&mut program) {
                *slot = Some(key.to_string());
            }
        }

        // Stamp every `panic` with its file and line here: the parser
        // knows only the line, and every later stage clones bodies (a projection
        // into its access site, a generic per instantiation).
        //
        // After the parse cache: two files with identical text share one parse,
        // and must not share one file name.
        //
        // Not the runtime module: its `panic` is a fixed `trap.rs` wording
        // (`malloc`'s `out of memory`), printed without a site.
        let site = site_file(key, root_key, opts.std_root.as_deref());
        if site != format!("{RUNTIME_SPEC}.vyrn") {
            stamp_panic_sites(&mut program, &site);
        }

        // `std/result` and `std/option` are validated no-op imports.
        // Recognize the specifier before file resolution, so the builtins are
        // never shadowed; check the names against the fixed list; refuse
        // `import * as`, since `r.Ok` would be a second spelling; then drop the
        // import.
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
            let load_err =
                |msg: String| -> Vec<Diagnostic> { vec![load_error(key, root_key, line, msg)] };
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

        // Resolve and load path imports depth-first. Generator imports run in a
        // second pass, once every path-imported module, the generator's own
        // included, is loaded.
        let mut import_targets: Vec<Option<String>> = vec![None; program.imports.len()];
        for (i, imp) in program.imports.iter().enumerate() {
            if let ImportSource::Path(path) = &imp.source {
                let target = resolve_spec(path, key, opts)
                    .map_err(|e| vec![load_error(key, root_key, imp.line, e)])?;
                // An import may not widen audience. Checked before the
                // target is visited, so the first illegal edge is the one reported.
                if let Some(d) = audience_objection(key, &target, imp.line, opts)
                    .or_else(|| runtime_fence(key, &target, imp.line, opts))
                {
                    return Err(vec![in_module(d, key, root_key)]);
                }
                visit(&target, None, opts, resolver, w, root_key)?;
                import_targets[i] = Some(target);
            }
        }
        // Generator imports: run each generator, synthesize the
        // module, and visit it. Calls whose path arguments resolve alike share
        // one module; an exact repeat dedups on `gen_key` and does
        // not re-run.
        for (i, imp) in program.imports.iter().enumerate() {
            if let ImportSource::Generator { name, args, line } = &imp.source {
                let (gen_key, gen_source) = run_generator(
                    key,
                    name,
                    args,
                    *line,
                    opts,
                    resolver,
                    &w.modules,
                    &w.states,
                    &mut w.identities,
                    root_key,
                )?;
                // A generator import is an import, and the audience rule decides
                // it too. The generated module's audience is its input file's, or
                // the mounting root's when the input declares none, so a `.vyx`
                // under `server/` mounted from the client root widens here.
                if let Some(d) = audience_objection(key, &gen_key, *line, opts) {
                    return Err(vec![in_module(d, key, root_key)]);
                }
                if let Some(src) = gen_source {
                    visit(&gen_key, Some(&src), opts, resolver, w, root_key)?;
                }
                import_targets[i] = Some(gen_key);
            }
        }
        let import_targets: Vec<String> = import_targets
            .into_iter()
            .map(|t| t.expect("every import resolved"))
            .collect();

        w.stack.pop();
        w.states.insert(key.to_string(), true);
        // A generated module keeps its source text for `vyrn emit-gen`.
        let gen_source = key.starts_with("generated by ").then(|| text.clone());
        w.modules.push(Module {
            key: key.to_string(),
            program,
            import_targets,
            gen_source,
            injected: None,
        });
        Ok(())
    }

    if let Err(diags) = visit(
        &root_key,
        Some(root_source),
        opts,
        resolver,
        &mut w,
        &root_key,
    ) {
        return Err(failed(diags, w.origins));
    }

    // The injected imports: a program that mentions a builtin in `RT_MODULES`
    // links that builtin's module although no import names it. Only on a
    // mention, so a program does not carry every runtime module.
    // `program_ref_names` is the scan `resolve_aliases` uses; module-scope `let`
    // initializers are outside it, and may not call user code anyway.
    let mentioned: HashSet<String> = w
        .modules
        .iter()
        .flat_map(|m| program_ref_names(&m.program))
        .collect();
    for rt in RT_MODULES {
        let wanted = rt.always
            || rt
                .desugared
                .iter()
                .copied()
                .chain(rt.routes().map(|(b, _)| b))
                .any(|b| mentioned.contains(b));
        // A missing std root is not an error here: whoever needs the runtime
        // refuses at the call.
        let Ok(target) = resolve_spec(rt.spec, &root_key, opts) else {
            continue;
        };
        // `wanted` gates the fetch, never the marking below: a module the
        // program imports by hand is program-global too, and must take the
        // reserved spellings, or a hand-imported `std/json`'s `JStr` sits beside
        // a consumer's own `JStr`.
        if !wanted && !w.states.contains_key(&target) {
            continue;
        }
        // A spec can resolve against a root without that file, and `@str` and
        // `print` bring nearly every program here. Skip what cannot be read, so
        // a partial std tree (an in-memory or editor resolver) still loads a
        // program that formats no float. A present but broken module still fails.
        if !w.states.contains_key(&target) && resolver.read(&target).is_err() {
            continue;
        }
        if !w.states.contains_key(&target) {
            if let Err(diags) = visit(&target, None, opts, resolver, &mut w, &root_key) {
                return Err(failed(diags, w.origins));
            }
        }
        // Marked after the visit, whether or not this load performed it: a hand
        // import takes the reserved spellings too, and `resolve_aliases` rewrites
        // its references with everything else.
        if let Some(m) = w.modules.iter_mut().find(|m| m.key == target) {
            m.injected = Some(rt.prefix);
        }
    }

    // The floor. Last, so it walks everything the artifact links,
    // injected runtime modules included. It is a whole-artifact rule, because
    // no single import edge knows what the program needs.
    if let Some(map) = &opts.artifacts {
        let graph = floor_graph(&mut w.modules);
        // A row a judgment answers cannot be decided here: the judgment reads the
        // named core, which needs the checker's types. That decision is held and
        // made after the check; every other row is refused here.
        match crate::floor::objected(&graph, &root_key, map) {
            // A nested generator load is not the artifact; only the outermost
            // load may hold a decision for the check that follows it.
            Some(c) if crate::floor::is_judged(&c) && LOAD_DEPTH.with(|d| d.get()) == 1 => {
                crate::floor::defer(graph, root_key.clone(), map.clone(), w.origins.clone());
            }
            _ => {
                if let Some(mut d) = crate::floor::objection(&graph, &root_key, map) {
                    if d.file.as_deref() == Some(root_key.as_str()) {
                        d.file = None;
                    }
                    return Err(failed(vec![d], w.origins));
                }
            }
        }
    }

    Ok((w.modules, root_key, w.origins, w.warnings))
}

/// A load's failure: every diagnostic remapped onto the input file a generator
/// synthesized it from. An ungoverned line keeps its generated
/// location.
fn failed(
    mut diags: Vec<Diagnostic>,
    origins: crate::origin::OriginMaps,
) -> (Vec<Diagnostic>, crate::origin::OriginMaps) {
    if !origins.is_empty() {
        for d in &mut diags {
            origins.remap(d);
        }
    }
    (diags, origins)
}

/// A generator's step budget and output-size cap.
pub(crate) const GEN_FUEL: u64 = 20_000_000;
pub(crate) const GEN_MAX_OUTPUT: usize = 4 * 1024 * 1024;

thread_local! {
    /// `module key -> content hash` for the load in progress, so the checker can
    /// tell which modules are byte-identical to last time.
    static MODULE_HASHES: std::cell::RefCell<HashMap<String, String>> =
        std::cell::RefCell::new(HashMap::new());
}

/// How deep nested generator loads may go. Far past any honest pipeline, and low
/// enough that the refusal is a diagnostic instead of a stack overflow.
const GEN_DEPTH_MAX: u32 = 32;

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
    /// Parsed modules by content hash; see the memo in `visit`.
    static PARSE_CACHE: std::cell::RefCell<HashMap<String, Program>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Runs a generator-call import target and returns the generated
/// module's key, with its source, or `None` when it is already synthesized.
///
/// The arguments must be constants. Identical calls share one key. On a cache
/// miss the generator is loaded, checked and run in the mediated sandbox, and
/// the output is cached under its recorded inputs.
#[allow(clippy::too_many_arguments)]
fn run_generator(
    importer: &str,
    name: &str,
    args: &[Expr],
    line: usize,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
    modules: &[Module],
    states: &HashMap<String, bool>,
    identities: &mut HashMap<String, String>,
    root_key: &str,
) -> Result<(String, Option<String>), Vec<Diagnostic>> {
    let err = |msg: String| -> Vec<Diagnostic> { vec![load_error(importer, root_key, line, msg)] };

    // Arguments must be compile-time constants.
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

    // A generator imported by a generated module resolves its path arguments
    // against the real importing file, not the banner key, as `resolve_spec` does.
    let path_importer = generated_importer(importer).unwrap_or(importer);
    let importer_dir = dir_of(path_importer).to_string();
    let join_dir = |s: &str| -> String {
        normalize(&if importer_dir.is_empty() {
            s.to_string()
        } else {
            format!("{importer_dir}/{s}")
        })
    };

    // Identity: the generator name, each string path argument rebased
    // onto the importer's directory, and every other argument verbatim. Two
    // imports whose paths resolve alike share one module and its state. The
    // banner anchors that module's imports at the first importer; the output is
    // a pure function of these inputs, so a later importer gets the same bytes.
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

    // The module's key is its diagnostic banner: the raw spelling and importer,
    // for readable `emit-gen` output. An exact repeat short-circuits on it.
    let gen_key = format!("generated by {name}({arg_repr}){GEN_SEP}{importer}");
    if states.contains_key(&gen_key) {
        return Ok((gen_key, None));
    }
    identities.insert(ident, gen_key.clone());

    // The generator must be an exported `gen fn` in a module this file loaded.
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

    // The generator's own source, for the cache key. Loading and checking it
    // waits for a cache miss: it is the most expensive step of a warm keystroke.
    let gen_source = resolver.read(&gen_mod_key).map_err(|e| {
        err(format!(
            "cannot re-read generator module `{gen_mod_key}`: {e}"
        ))
    })?;

    // Each constant string path argument is an allowed input root; a path with
    // no extension also admits its `.vyrn` file. A path that names a manifest
    // dependency also admits the key the import map resolves it to,
    // and the pair goes to the sandbox, which cannot resolve aliases. The
    // resolved key joins the cache key, so re-pinning the dependency misses.
    let mut allowed: Vec<String> = Vec::new();
    let mut aliased: Vec<(String, String)> = Vec::new();
    for c in &consts {
        if let crate::consteval::ConstVal::Str(s) = c {
            allowed.push(join_dir(s));
            if !s.ends_with(".vyrn") && !s.ends_with(".json") {
                allowed.push(join_dir(&format!("{s}.vyrn")));
            }
            // An alias `resolve_spec` refuses adds no pair, so the read fails as a
            // plain path. The module import of that alias already reports the
            // manifest's fault.
            if opts.aliases.contains_key(s.as_str()) {
                if let Ok(key) = resolve_spec(s, importer, opts) {
                    allowed.push(key.clone());
                    aliased.push((s.clone(), key));
                }
            }
        }
    }
    let sources_hash = generator_cache_key(
        &crate::gen::compiler_identity(),
        &gen_mod_key,
        name,
        &arg_repr,
        &allowed,
    );
    let no_cache = std::env::var("VYRN_NO_GEN_CACHE").is_ok();

    // Cache hit: the entry is one this compiler wrote for this key, it records
    // the generator's own module, and every recorded input still hashes the
    // same. An input recorded as absent must still be absent.
    if !no_cache {
        if let Some(cached) = resolver.gen_cache_get(&sources_hash) {
            if let Some((inputs, output)) = read_cache_entry(&sources_hash, &cached) {
                // `inputs` comes from the entry and so agrees with itself: an
                // empty or forged list passes `all`. The call site decides first:
                // an entry that does not record `gen_mod_key` is not this
                // generation.
                let records_generator = inputs.iter().any(|(path, _)| path == &gen_mod_key);
                if records_generator
                    && inputs.iter().all(|(path, hash)| {
                        current_input_hash(resolver, path).unwrap_or_else(|| ABSENT.to_string())
                            == *hash
                    })
                {
                    return Ok((gen_key, Some(output)));
                }
            }
        }
    }

    // Cache miss: load and check the generator as a runnable program. Skipping
    // this on a hit is sound: an entry is written only after a run that passed
    // this check, and an edit to the generator's sources misses.
    let (loaded, _, _, gen_graph) = load_with_origins(&gen_source, &gen_mod_key, opts, resolver);
    let mut gen_program = loaded?;
    // A generator is a runnable program compiled to wasm, so it gets
    // the check and synthesis a root gets.
    let gdiags = crate::movecheck::comptime(|| crate::check_and_synthesize(&mut gen_program));
    if !gdiags.is_empty() {
        return Err(gdiags);
    }

    // Contract provenance. A generator is re-loaded as its own root,
    // so a contract declared in it would carry `module: None` and its diagnostics
    // would not say which library demanded it. Restamp each contract with its
    // module's import specifier (`std/ui`, `./contract`), which a reader can
    // type. Only the generator's private copy changes, after its check.
    let std_root = opts.std_root.as_deref();
    let gen_dir = dir_of(&gen_mod_key).to_string();
    for c in &mut gen_program.contracts {
        let key = c.module.clone().unwrap_or_else(|| gen_mod_key.clone());
        c.module = Some(import_specifier(&gen_dir, &key, std_root));
    }

    // The generator's transitive sources, hashed: the cache entry records them
    // as inputs, and the wasm engine keys its compiled artifact on them.
    //
    // A generated module's banner key has no readable file. A closure holding
    // one has no fingerprint: no cache entry, since an unverifiable input is
    // worse than a miss, and no artifact key.
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
    // The contract restamping above resolves specifiers against `gen_mod_key`
    // and the std root, so both join the fingerprint.
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

    // Run the generator in the mediated sandbox, as comptime like the check
    // above: the kernel and the lowering's lint are about a program a tool holds.
    let _ = crate::own::typed_refusals();
    let out = crate::movecheck::comptime(|| {
        crate::gen::generate(
            &gen_program,
            name,
            &consts,
            crate::gen::GenInputs {
                resolver,
                opts,
                importer_dir,
                allowed,
                aliased,
                fuel: GEN_FUEL_OVERRIDE.with(|c| c.get()).unwrap_or(GEN_FUEL),
                max_output: GEN_MAX_OUTPUT_OVERRIDE
                    .with(|c| c.get())
                    .unwrap_or(GEN_MAX_OUTPUT),
                sources_fingerprint: fingerprint,
                type_arg: None,
            },
        )
    })
    .map_err(|trap| {
        // The checker alone judged the generator above. A rule the typed
        // judgment states reaches it through the engine's compile, which
        // refuses the program.
        let typed = crate::own::typed_refusals();
        if typed.is_empty() {
            err(format!("generator `{name}({arg_repr})` failed: {trap}"))
        } else {
            typed
        }
    })?;
    bump_gen_runs();

    // Cache the output under its recorded inputs, for the next load and the
    // LSP's per-keystroke re-analysis.
    if !no_cache {
        let mut inputs: Vec<(String, String)> = out
            .reads
            .iter()
            .map(|(p, bytes)| {
                let h = match bytes {
                    Some(b) => crate::hash::sha256_hex(b),
                    None => ABSENT.to_string(),
                };
                (p.clone(), h)
            })
            .collect();
        // The generator's own transitive sources join the recorded inputs, so
        // the entry carries its own validity and the lookup key stays cheap.
        inputs.extend(gen_sources);
        // Recorded unconditionally: a hit is validated against the generator
        // module the call site named.
        if !inputs.iter().any(|(p, _)| p == &gen_mod_key) {
            if let Some(h) = current_input_hash(resolver, &gen_mod_key) {
                inputs.push((gen_mod_key.clone(), h));
            }
        }
        if describable {
            resolver.gen_cache_put(
                &sources_hash,
                &render_cache_entry(&sources_hash, &inputs, &out.source),
            );
        }
    }
    Ok((gen_key, Some(out.source)))
}

/// The cache lookup key: `sha256(compiler identity ++ generator module ++ name
/// ++ args ++ resolved input roots)`. The identity is
/// [`crate::gen::compiler_identity`], because recorded inputs cannot tell two
/// compilers' outputs apart.
///
/// The key omits the generator's sources: finding them means parsing the whole
/// generator graph on every hit. The entry records them among its inputs, so a
/// hit re-hashes them and misses if any changed. Two versions of a generator
/// share one key and take turns owning the entry.
fn generator_cache_key(
    identity: &str,
    gen_mod_key: &str,
    name: &str,
    arg_repr: &str,
    resolved_inputs: &[String],
) -> String {
    let mut blob: Vec<u8> = Vec::new();
    for part in [identity, gen_mod_key, name, arg_repr] {
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

/// The current hash of a recorded generation input: a file (`resolver.read`) or
/// a directory listing (a `dir/` marker, `resolver.list`). `None` when it cannot
/// be read; validation reads that as [`ABSENT`].
///
/// Memoized for one outermost load, because a root that imports several
/// generators validates the same std modules once each, and files do not change
/// during a load. Only the outermost load bumps the epoch: generators re-enter
/// the loader, and a nested bump would drop the memo mid-use.
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

/// The recorded hash of an input that was absent when the generator looked. Not
/// a sha256, so it never equals the hash of any content.
const ABSENT: &str = "absent";

/// The cache entry format tag, bumped when an entry's meaning changes.
/// [`read_cache_entry`] rejects any other tag as a miss, so the generator re-runs
/// and overwrites the entry.
const CACHE_ENTRY_TAG: &str = "v3";

/// Formats this compiler wrote before [`CACHE_ENTRY_TAG`]. Such an entry is
/// stale, not foreign, so it is ignored without a warning.
const SUPERSEDED_ENTRY_TAGS: &[&str] = &["v1", "v2"];

/// Serializes a cache entry: `v3 <tag> <N>`, then `path<TAB>hash` lines, then the
/// generated source verbatim. The tag authenticates everything after it, and
/// [`entry_tag`] binds the lookup key into it.
fn render_cache_entry(key: &str, inputs: &[(String, String)], output: &str) -> String {
    let mut body = format!("{}\n", inputs.len());
    for (p, h) in inputs {
        body.push_str(&format!("{p}\t{h}\n"));
    }
    body.push_str(output);
    format!("{CACHE_ENTRY_TAG} {} {body}", entry_tag(key, &body))
}

/// Inverse of [`render_cache_entry`], for an entry this compiler wrote.
///
/// The generator cache holds compiler input: a hit is linked as a module and
/// never re-runs the generator. A generated module has no trusted hash to check
/// against (as `vyrn.lock` is for a remote module), so each entry carries a tag
/// under a per-user key kept outside the cache directory
/// ([`gen_cache_secret`]). An entry this user's compiler did not write is
/// refused: a restored CI cache, a shared `~/.vyrn/cache/gen`, a redirected
/// `VYRN_GEN_CACHE_DIR`, an entry moved between keys. A process running as this
/// user can still read the key.
///
/// A superseded format is a silent miss. Anything else that fails, including a
/// `v3` entry with a bad tag, is a miss with a warning.
fn read_cache_entry(key: &str, text: &str) -> Option<(Vec<(String, String)>, String)> {
    let Some(first_nl) = text.find('\n') else {
        warn_foreign_entry(key);
        return None;
    };
    let header = &text[..first_nl];
    // Every format ends its header with the input count, so read it before the
    // format. A generation always reads the generator's own module, so an entry
    // that records nothing describes no generation, authentic or not: an empty
    // list satisfies `all` vacuously.
    let count = header.rsplit(' ').next().unwrap_or("");
    if count.parse::<usize>() == Ok(0) {
        warn_foreign_entry(key);
        return None;
    }
    let Some(rest) = header
        .strip_prefix(CACHE_ENTRY_TAG)
        .and_then(|r| r.strip_prefix(' '))
    else {
        // `v1` and `v2` are this compiler's own earlier formats.
        if !SUPERSEDED_ENTRY_TAGS
            .iter()
            .any(|t| header.starts_with(&format!("{t} ")))
        {
            warn_foreign_entry(key);
        }
        return None;
    };
    let Some((tag, count)) = rest.split_once(' ') else {
        warn_foreign_entry(key);
        return None;
    };
    let Ok(n) = count.parse::<usize>() else {
        warn_foreign_entry(key);
        return None;
    };
    // `count` is the tail of the header line, so the body starts where it does.
    let body = &text[first_nl - count.len()..];
    if entry_tag(key, body) != tag {
        warn_foreign_entry(key);
        return None;
    }
    let mut idx = first_nl + 1;
    // No `with_capacity(n)`: `n` comes from the file, and a truncated write can
    // make it `usize::MAX`, which aborts on the allocation.
    let mut inputs = Vec::new();
    for _ in 0..n {
        let nl = text[idx..].find('\n')? + idx;
        let (p, h) = text[idx..nl].split_once('\t')?;
        inputs.push((p.to_string(), h.to_string()));
        idx = nl + 1;
    }
    Some((inputs, text[idx..].to_string()))
}

thread_local! {
    /// Keys already reported by [`warn_foreign_entry`]. The LSP validates the
    /// same entry on every keystroke, so each key warns once.
    static WARNED_ENTRIES: std::cell::RefCell<HashSet<String>> =
        std::cell::RefCell::new(HashSet::new());
}

fn warn_foreign_entry(key: &str) {
    let first = WARNED_ENTRIES.with(|w| w.borrow_mut().insert(key.to_string()));
    if !first {
        return;
    }
    eprintln!("warning: ignoring generator cache entry `{key}`: this compiler did not write it");
    eprintln!(
        "  note: the generator ran instead, so this build is correct — but something \
         other than `vyrn` is writing to the generator cache (`VYRN_GEN_CACHE_DIR`, \
         else `~/.vyrn/cache/gen`)"
    );
}

fn entry_tag(key: &str, body: &str) -> String {
    gen_cache_tag(key, body.as_bytes())
}

/// Authenticates `body` under `key`: `H(secret || H(secret || key || body))`.
///
/// The key is inside the tag, so an entry cannot move to another lookup key.
/// Nested rather than prefixed, because SHA-256 extends and the outer hash is
/// over a fixed-length digest. Public because `vyrn-genwasm` writes a cranelift
/// artifact beside the entries and maps it in as native code; one secret
/// authenticates both.
pub fn gen_cache_tag(key: &str, body: &[u8]) -> String {
    let secret = gen_cache_secret();
    let mut inner = Vec::with_capacity(secret.len() + key.len() + body.len() + 2);
    inner.extend_from_slice(secret);
    inner.push(0);
    inner.extend_from_slice(key.as_bytes());
    inner.push(0);
    inner.extend_from_slice(body);
    let inner = crate::hash::sha256_hex(&inner);
    let mut outer = Vec::with_capacity(secret.len() + inner.len() + 1);
    outer.extend_from_slice(secret);
    outer.push(0);
    outer.extend_from_slice(inner.as_bytes());
    crate::hash::sha256_hex(&outer)
}

static GEN_CACHE_SECRET: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();

/// The per-user key that tells entries this compiler wrote from files something
/// else left in the cache directory. Read from `~/.vyrn/gen-cache.key`, created
/// on first use.
///
/// It sits outside `~/.vyrn/cache`, and `VYRN_GEN_CACHE_DIR` does not move it,
/// so a copied or redirected cache never carries the key. Without a home
/// directory or a writable file the process keeps its own key, and the next
/// process misses on every entry.
fn gen_cache_secret() -> &'static [u8] {
    GEN_CACHE_SECRET
        .get_or_init(|| {
            let path = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .ok()
                .map(|home| {
                    std::path::Path::new(&home)
                        .join(".vyrn")
                        .join("gen-cache.key")
                });
            let Some(path) = path else {
                return fresh_secret();
            };
            if let Some(k) = read_secret(&path) {
                return k;
            }
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            // `create_new`, so two compilers starting together agree on one key.
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            if let Ok(mut f) = opts.open(&path) {
                use std::io::Write;
                let _ = f.write_all(&fresh_secret());
                let _ = f.flush();
            }
            read_secret(&path).unwrap_or_else(fresh_secret)
        })
        .as_slice()
}

/// The key file's bytes, if it holds a whole key. A short read is a file caught
/// mid-creation by another process; this run keeps its own key and misses.
fn read_secret(path: &std::path::Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    (bytes.len() >= 32).then_some(bytes)
}

/// A fresh key. `RandomState` is seeded from the operating system's randomness;
/// the process id and clock join it so two keys minted in one process differ.
fn fresh_secret() -> Vec<u8> {
    use std::hash::{BuildHasher, Hasher};
    let state = std::collections::hash_map::RandomState::new();
    let mut seed: Vec<u8> = Vec::new();
    for i in 0..4u64 {
        let mut h = state.build_hasher();
        h.write_u64(i);
        seed.extend_from_slice(&h.finish().to_le_bytes());
    }
    seed.extend_from_slice(&std::process::id().to_le_bytes());
    if let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        seed.extend_from_slice(&d.as_nanos().to_le_bytes());
    }
    crate::hash::sha256_hex(&seed).into_bytes()
}

/// Whether a type decl is one of the parser-injected builtins (`Value`,
/// `Template`, and others). Every parsed file has them; the linker keeps only
/// the root's copies.
fn is_injected(t: &TypeDecl) -> bool {
    t.line == 0
}

/// Which of a module's declaration lists a row came from. A shared `extern fn`
/// name is skipped only as a function.
#[derive(Clone, Copy, PartialEq)]
enum DeclKind {
    Type,
    Fn,
    Protocol,
    Contract,
    Global,
}

/// One top-level declaration of a module.
struct Decl<'a> {
    name: &'a str,
    kind: DeclKind,
    /// Always `false` for a global: module state is module-private,
    /// and another module reaches it through an accessor function.
    exported: bool,
    /// A parser-injected builtin type ([`is_injected`]), present in every file.
    injected: bool,
    /// An `extern fn`: a host-ABI contract under its source spelling,
    /// so no rule here may rename it.
    is_extern: bool,
}

/// Every top-level declaration a module states, types first and globals last.
/// Every reader in this file asks this one function, so a new declaration form
/// is added once.
fn decls(p: &Program) -> impl Iterator<Item = Decl<'_>> {
    fn d(name: &str, kind: DeclKind, exported: bool) -> Decl<'_> {
        Decl {
            name,
            kind,
            exported,
            injected: false,
            is_extern: false,
        }
    }
    p.type_decls
        .iter()
        .map(|t| Decl {
            injected: is_injected(t),
            ..d(&t.name, DeclKind::Type, t.exported)
        })
        .chain(p.functions.iter().map(|f| Decl {
            is_extern: f.is_extern,
            ..d(&f.name, DeclKind::Fn, f.exported)
        }))
        .chain(
            p.protocols
                .iter()
                .map(|pr| d(&pr.name, DeclKind::Protocol, pr.exported)),
        )
        .chain(
            p.contracts
                .iter()
                .map(|c| d(&c.name, DeclKind::Contract, c.exported)),
        )
        .chain(
            p.globals
                .iter()
                .map(|g| d(&g.name, DeclKind::Global, false)),
        )
}

/// The owning-module slot of everything a module holds, tests and benches
/// included. A contract's names the library a diagnostic blames, a
/// global's carries the same-module initializer rule, and a test's
/// or bench's lets `vyrn test <root>` run the root's alone. Separate from
/// [`decls`] because the borrow is unique.
fn decl_modules_mut(p: &mut Program) -> impl Iterator<Item = &mut Option<String>> {
    (p.type_decls.iter_mut().map(|t| &mut t.module))
        .chain(p.functions.iter_mut().map(|f| &mut f.module))
        .chain(p.protocols.iter_mut().map(|pr| &mut pr.module))
        .chain(p.contracts.iter_mut().map(|c| &mut c.module))
        .chain(p.globals.iter_mut().map(|g| &mut g.module))
        .chain(p.tests.iter_mut().map(|t| &mut t.module))
        .chain(p.benches.iter_mut().map(|b| &mut b.module))
}

/// Resolves import aliases into the flat namespace, before the
/// alias-unaware registration, visibility and merge steps.
///
/// For each `import { X as Y } from M`: `Y` must not collide with the importing
/// module's decls or other imports; references to `Y` become the decl they
/// name; and if the importing module also defines `X` (the RPC stub pattern),
/// `M`'s `X` is renamed to a fresh symbol program-wide, so the local keeps `X`.
///
/// Afterwards every import is a bare import of a unique decl name. The LSP
/// indexes a separate parse of the root, which keeps its aliases.
fn resolve_aliases(modules: &mut [Module], errors: &mut Vec<Diagnostic>, root_key: &str) {
    // Top-level decl names per module.
    let mut module_decls: HashMap<String, HashSet<String>> = HashMap::new();
    // `all_names` only lets `rename_apart` mint a collision-free `__fromN`, and
    // most programs never rename, so it fills on first use.
    let mut all_names: HashSet<String> = HashSet::new();
    for m in modules.iter() {
        module_decls
            .entry(m.key.clone())
            .or_default()
            .extend(decls(&m.program).map(|d| d.name.to_string()));
    }

    // Exported decl names per module: what a namespace import reaches.
    // `name_module_count` counts the modules declaring each name, so a
    // namespaced export is renamed only when its name would collide.
    let mut module_exports: HashMap<String, HashSet<String>> = HashMap::new();
    // Variant names of a module's exported enums, to tell `ns.Enum.Variant(x)`
    // apart from `someFn(ns.Type, ..)`, which parse the same.
    let mut module_variants: HashMap<String, HashSet<String>> = HashMap::new();
    let mut name_module_count: HashMap<String, usize> = HashMap::new();
    for m in modules.iter() {
        let variants = module_variants.entry(m.key.clone()).or_default();
        for t in &m.program.type_decls {
            if t.line != 0 && t.exported {
                if let Some(vs) = crate::types::declared_variants(&t.base) {
                    for v in vs {
                        variants.insert(v.name.clone());
                    }
                }
            }
        }
        module_exports.entry(m.key.clone()).or_default().extend(
            decls(&m.program)
                .filter(|d| d.exported && !d.injected)
                .map(|d| d.name.to_string()),
        );
        for n in module_decls.get(&m.key).into_iter().flatten() {
            *name_module_count.entry(n.clone()).or_insert(0) += 1;
        }
    }

    // Namespace bindings: module key -> [(ns name, target module)],
    // checked for collisions before any reference is reinterpreted.
    let mut ns_bindings: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for m in modules.iter() {
        let mine = module_decls.get(&m.key).cloned().unwrap_or_default();
        let import_locals = import_locals(&m.program);
        let mut seen_ns: HashSet<String> = HashSet::new();
        let binds = ns_bindings.entry(m.key.clone()).or_default();
        for (imp, target) in m.program.imports.iter().zip(&m.import_targets) {
            let Some(ns) = &imp.namespace else { continue };
            let mut ok = true;
            if !seen_ns.insert(ns.clone()) {
                errors.push(load_error(
                    &m.key,
                    root_key,
                    imp.line,
                    format!("namespace `{ns}` is bound twice in this module"),
                ));
                ok = false;
            }
            if mine.contains(ns) || import_locals.contains(ns) {
                errors.push(load_error(
                    &m.key,
                    root_key,
                    imp.line,
                    format!(
                        "namespace `{ns}` collides with a top-level declaration or import \
                             of the same name in this module"
                    ),
                ));
                ok = false;
            }
            if ok {
                binds.push((ns.clone(), target.clone()));
            }
        }
    }

    // (target module, original) -> fresh symbol, for every rename apart.
    let mut foreign_renames: HashMap<(String, String), String> = HashMap::new();
    // Rename `name`, as `target` declares it, apart from every other name in the
    // program, once, whichever rule asks: co-naming, a namespaced
    // export or name privacy.
    let mut rename_apart =
        |renames: &mut HashMap<(String, String), String>, target: &str, name: &str| {
            let key = (target.to_string(), name.to_string());
            if renames.contains_key(&key) {
                return;
            }
            if all_names.is_empty() {
                for names in module_decls.values() {
                    all_names.extend(names.iter().cloned());
                }
            }
            let mut n = 0usize;
            let fresh = loop {
                let cand = format!("{name}__from{n}");
                if all_names.insert(cand.clone()) {
                    break cand;
                }
                n += 1;
            };
            renames.insert(key, fresh);
        };

    // Every declaration of an injected module takes its reserved spelling,
    // unconditionally. So `link`'s uniqueness check never names a module the
    // user did not import, and a desugar's call (`json$emit`) is never captured
    // by a user's `emit`. Variants are renamed too, or a user enum with a `JStr`
    // variant would clash with `std/json`.
    // A variant is not an import name, so the variant renames are kept per
    // module; a hand importer of one runtime module keeps what another's
    // variant names mean to it.
    let injected: Vec<(String, &'static str)> = modules
        .iter()
        .filter_map(|m| m.injected.map(|p| (m.key.clone(), p)))
        .collect();
    // module key -> (resolved enum spelling -> its variants' renames). Per enum,
    // so pass 2 extends only an importer that imports that enum.
    let mut injected_variants: HashMap<String, HashMap<String, HashMap<String, String>>> =
        HashMap::new();
    for (key, prefix) in &injected {
        let m = modules
            .iter()
            .find(|m| &m.key == key)
            .expect("injected module");
        let by_enum = injected_variants.entry(key.clone()).or_default();
        // Parser-injected builtins are in every module and keep their spelling.
        let mut names: Vec<String> = decls(&m.program)
            .filter(|d| !d.injected)
            .map(|d| d.name.to_string())
            .collect();
        for t in &m.program.type_decls {
            if t.line == 0 {
                continue;
            }
            if let Some(vs) = crate::types::declared_variants(&t.base) {
                let vars = by_enum.entry(format!("{prefix}{}", t.name)).or_default();
                for v in vs {
                    vars.insert(v.name.clone(), format!("{prefix}{}", v.name));
                    names.push(v.name.clone());
                }
            }
        }
        // No `all_names` entry: `rename_apart` mints only `x__fromN`, which has
        // no `$`, and touching `all_names` here would defeat its lazy fill.
        for n in names {
            foreign_renames.insert((key.clone(), n.clone()), format!("{prefix}{n}"));
        }
        // An impl method follows its type's rename: the parser flattens
        // `impl P for T` to `P__T__m`, and the checker mangles the renamed key,
        // so the name is `Copy__json$Json__copy`, not `json$Copy__Json__copy`.
        // Overwrites the entry the loop above wrote, so it runs after it.
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

    // Protocol method names across every module. A method call dispatches to an
    // impl before a free function, so an argument-bearing call to one of these
    // names is method sugar, not a direct use of an aliased import's original.
    let method_surface: HashSet<String> = modules
        .iter()
        .flat_map(|m| m.program.protocols.iter())
        .flat_map(|p| p.methods.iter().map(|sig| sig.name.clone()))
        .collect();

    // Pass 1: alias collision checks, and the co-naming renames.
    for m in modules.iter() {
        let mine = module_decls.get(&m.key).cloned().unwrap_or_default();
        // local name -> (target module, original name) of the import that bound it.
        let mut locals_seen: HashMap<String, (String, String)> = HashMap::new();
        for (imp, target) in m.program.imports.iter().zip(&m.import_targets) {
            for n in &imp.names {
                let local = n.local().to_string();
                // The local name must not clash with another import's, nor, as an
                // alias, with a top-level decl of this module.
                let here = (target.clone(), n.original.clone());
                if let Some(prev) = locals_seen.insert(local.clone(), here.clone()) {
                    // One name from two modules is those modules sharing a
                    // top-level name, which `link` reports once with the namespace
                    // fix. A repeat from one module, or two names aliased to one
                    // local, errors here.
                    let one_name_two_modules = prev.0 != here.0 && prev.1 == here.1;
                    if !one_name_two_modules {
                        errors.push(load_error(
                            &m.key,
                            root_key,
                            imp.line,
                            format!("`{local}` is imported twice into this module"),
                        ));
                    }
                }
                if n.alias.is_some() && mine.contains(&local) {
                    errors.push(load_error(
                        &m.key,
                        root_key,
                        imp.line,
                        format!(
                            "import alias `{local}` clashes with a top-level declaration of \
                                 the same name in this module"
                        ),
                    ));
                }
            }
        }
        // Co-naming: an aliased import whose original name is also defined here.
        for (imp, target) in m.program.imports.iter().zip(&m.import_targets) {
            for n in &imp.names {
                if n.alias.is_some() && mine.contains(&n.original) {
                    rename_apart(&mut foreign_renames, target, &n.original);
                }
            }
        }
        // An aliased import hides the original name unless the module also
        // defines or bare-imports it. Caught before the reference rewrite fuses
        // alias and original.
        let bare_imported: HashSet<&str> = m
            .program
            .imports
            .iter()
            .flat_map(|imp| imp.names.iter())
            .filter(|n| n.alias.is_none())
            .map(|n| n.original.as_str())
            .collect();
        let (refs, ambiguous_only) = program_ref_kinds(&m.program, true);
        for imp in &m.program.imports {
            for n in &imp.names {
                if let Some(_alias) = &n.alias {
                    let orig = &n.original;
                    if !mine.contains(orig)
                        && !bare_imported.contains(orig.as_str())
                        && refs.contains(orig)
                        // An argument-bearing call to a protocol method name may
                        // be `widget.render()`, which dispatches to impls before
                        // any free function, so it is not a direct use.
                        && !(ambiguous_only.contains(orig) && method_surface.contains(orig))
                    {
                        errors.push(load_error(
                            &m.key,
                            root_key,
                            imp.line,
                            format!(
                                "`{orig}` is not in scope — it was imported as `{}`; use \
                                     that name (or import `{orig}` too)",
                                n.local()
                            ),
                        ));
                    }
                }
            }
        }
    }

    // Namespace renames: a namespaced module's exports stay out of the
    // flat namespace, so an export whose name another module also declares is
    // renamed to a fresh symbol. `ns.member` and a selective importer both
    // resolve to it; a unique name keeps its spelling.
    let namespaced_targets: HashSet<String> = ns_bindings
        .values()
        .flatten()
        .map(|(_, t)| t.clone())
        .collect();
    // Sorted at both levels: `namespaced_targets` is a `HashSet`, and the order
    // decides which module gets `__from0`, so the linked program must not
    // depend on it.
    let mut namespaced_targets: Vec<String> = namespaced_targets.into_iter().collect();
    namespaced_targets.sort();
    for target in &namespaced_targets {
        let exports = module_exports.get(target).cloned().unwrap_or_default();
        let mut names: Vec<&String> = exports.iter().collect();
        names.sort();
        for name in names {
            if name_module_count.get(name).copied().unwrap_or(0) >= 2 {
                rename_apart(&mut foreign_renames, target, name);
            }
        }
    }

    // Name privacy: a non-exported decl is invisible outside its
    // module, so it never forces a consumer to rename. If its name also appears
    // in another module, it is renamed to a fresh symbol; nothing can import it
    // by name, and its module's references follow in pass 3. Sorted, so the
    // minted suffixes are stable.
    let mut priv_targets: Vec<&Module> = modules.iter().collect();
    priv_targets.sort_by(|a, b| a.key.cmp(&b.key));
    for m in priv_targets {
        let exported = module_exports.get(&m.key).cloned().unwrap_or_default();
        // A private decl whose name an import also brings into scope is a real
        // clash the user must resolve; renaming it would hide the clash.
        let imported = import_locals(&m.program);
        // Non-exported decl names. A parser-injected type is the same everywhere
        // and never renamed. An `extern fn` is a host-ABI contract emitted under
        // its source spelling, even when several modules restate it (std/rpc's
        // client stubs do). A global is never exported.
        let mut privates: Vec<String> = decls(&m.program)
            .filter(|d| {
                !d.injected
                    && !d.is_extern
                    && (d.kind == DeclKind::Global || !exported.contains(d.name))
            })
            .map(|d| d.name.to_string())
            .collect();
        privates.sort();
        privates.dedup();
        for name in privates {
            if imported.contains(&name) {
                continue;
            }
            // The root's `main` is the entry point every backend reaches by
            // that spelling, so it never renames, even when an imported module
            // declares a `main` too. A non-root `main` renames.
            if m.key == root_key && name == "main" {
                continue;
            }
            // A protocol method name is not a free-function name: `x.m(..)`
            // parses like `m(x, ..)`, and the checker dispatches it to the impls
            // and refuses the declaration. Renaming would hide that refusal and
            // rewrite the module's `w.fetch()` into `fetch__from0(w)`.
            if method_surface.contains(&name) {
                continue;
            }
            if name_module_count.get(&name).copied().unwrap_or(0) >= 2 {
                rename_apart(&mut foreign_renames, &m.key, &name);
            }
        }
    }

    // Pass 2: per-module reference-rewrite maps (alias or local -> resolved decl).
    let mut rewrites: HashMap<String, HashMap<String, String>> = HashMap::new();
    for m in modules.iter() {
        for (imp, target) in m.program.imports.iter().zip(&m.import_targets) {
            for n in &imp.names {
                let resolved = resolved_name(&foreign_renames, target, &n.original);
                if n.alias.is_some() {
                    // The alias resolves to the decl, renamed or not.
                    rewrites
                        .entry(m.key.clone())
                        .or_default()
                        .insert(n.local().to_string(), resolved);
                } else if resolved != n.original {
                    // A bare importer of a co-named decl follows the rename.
                    rewrites
                        .entry(m.key.clone())
                        .or_default()
                        .insert(n.original.clone(), resolved);
                }
            }
            // A hand importer of an injected module follows its variant renames
            // too: importing an enum brings its variants, which are references,
            // not import names. Only when it imports that enum itself, so a
            // consumer's own `JStr` variant is not rewritten to `json$JStr`.
            if !imp.names.is_empty() {
                if let Some(by_enum) = injected_variants.get(target) {
                    for n in &imp.names {
                        let resolved = resolved_name(&foreign_renames, target, &n.original);
                        if let Some(vars) = by_enum.get(&resolved) {
                            rewrites
                                .entry(m.key.clone())
                                .or_default()
                                .extend(vars.iter().map(|(k, v)| (k.clone(), v.clone())));
                        }
                    }
                }
            }
        }
    }

    // Pass 3: apply the foreign-decl renames to the definition and its module's
    // references. The module's own namespace bindings keep its `ns.member(..)`
    // sugar out of the plain-name rewrite; pass 5 owns those.
    let mut renames_by_module: HashMap<&str, HashMap<String, String>> = HashMap::new();
    for ((target, original), s) in &foreign_renames {
        renames_by_module
            .entry(target.as_str())
            .or_default()
            .insert(original.clone(), s.clone());
    }
    for tm in modules.iter_mut() {
        let Some(map) = renames_by_module.get(tm.key.as_str()) else {
            continue;
        };
        let ns_names = ns_names_of(&ns_bindings, &tm.key);
        rename_decls_in_module(&mut tm.program, map, &ns_names);
    }

    // Pass 3b: the injected module's enum variant names in the declarations.
    // Pass 3 rewrote every reference; the variant list in the decl's
    // `Type::Enum` base is not a reference.
    for (key, _) in &injected {
        let Some(vars) = injected_variants.get(key) else {
            continue;
        };
        if let Some(tm) = modules.iter_mut().find(|m| &m.key == key) {
            for t in &mut tm.program.type_decls {
                if t.line == 0 || crate::types::is_sum_alias(&t.base) {
                    continue;
                }
                if let Type::Enum(vs) = &mut t.base {
                    let Some(by_enum) = vars.get(&t.name) else {
                        continue;
                    };
                    for v in vs {
                        if let Some(r) = by_enum.get(&v.name) {
                            v.name = r.clone();
                        }
                    }
                }
            }
        }
    }

    // Pass 4: apply the per-module reference rewrites, and turn each import into
    // a bare import of the resolved decl, so registration and visibility need
    // no alias logic.
    for m in modules.iter_mut() {
        if let Some(map) = rewrites.get(&m.key) {
            let ns_names = ns_names_of(&ns_bindings, &m.key);
            // The module's own variants guard the rewrite (see
            // [`Renamer::variants`]): an alias local or injected spelling that
            // collides with one must not fold the constructor sites.
            let variants = own_variant_names(&m.program);
            rewrite_module_refs(&mut m.program, map, &ns_names, &variants);
        }
        for (imp, target) in m.program.imports.iter_mut().zip(&m.import_targets) {
            for n in &mut imp.names {
                let resolved = resolved_name(&foreign_renames, target, &n.original);
                n.original = resolved;
                n.alias = None;
            }
        }
    }

    // Pass 5: resolve `ns.member` uses in each namespaced module to
    // the program-wide symbol. After the alias rewrites, which touch only plain
    // names. The walk is scope-aware: a local shadows a namespace.
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

/// Hands every part of a type to a visitor, outermost first: the one descent
/// the compiler makes over a `Type`.
///
/// [`type_names`], [`rewrite_type`], [`NsResolver::rewrite_type`] and
/// `parser::mark_member_type_params` read it. The hook gets the node, not the
/// head name, because `mark_member_type_params` replaces a node;
/// [`type_heads`] and [`type_heads_mut`] give the head name. A macro, because
/// the readers need both a shared and a unique borrow.
macro_rules! type_head_descent {
    ($name:ident $(, $mut_:tt)?) => {
        pub(crate) fn $name(ty: &$($mut_)? Type, f: &mut impl FnMut(&$($mut_)? Type)) {
            f(ty);
            match ty {
                Type::App(_, args) => {
                    for a in args {
                        $name(a, f);
                    }
                }
                Type::Array(a)
                | Type::Stream(a)
                | Type::Partial(a)
                | Type::ArrayN(a, _)
                | Type::SmallArray(a, _)
                | Type::Omit(a, _)
                | Type::Pick(a, _) => $name(a, f),
                Type::Merge(a, b) => {
                    $name(a, f);
                    $name(b, f);
                }
                Type::Record(fs) => {
                    for fl in fs {
                        $name(&$($mut_)? fl.ty, f);
                    }
                }
                Type::Enum(vs) => {
                    for v in vs {
                        for p in &$($mut_)? v.payload {
                            $name(p, f);
                        }
                    }
                }
                // Stored function values and maps carry decl
                // references in their component types too.
                Type::Fn(params, ret) => {
                    for p in params {
                        $name(p, f);
                    }
                    $name(ret, f);
                }
                Type::Map(k, v) => {
                    $name(k, f);
                    $name(v, f);
                }
                _ => {}
            }
        }
    };
}

type_head_descent!(type_nodes);
type_head_descent!(type_nodes_mut, mut);

/// The same descent, with the hook on a type's head name. `Named` and `App` are
/// the two constructors that carry one.
fn type_heads(ty: &Type, f: &mut impl FnMut(&String)) {
    type_nodes(ty, &mut |t| {
        if let Type::Named(n) | Type::App(n, _) = t {
            f(n)
        }
    });
}

fn type_heads_mut(ty: &mut Type, f: &mut impl FnMut(&mut String)) {
    type_nodes_mut(ty, &mut |t| {
        if let Type::Named(n) | Type::App(n, _) = t {
            f(n)
        }
    });
}

// The scope-aware descent over a body is `ast::body_scope_descent!`.

crate::body_scope_descent!(BodyVisit, body_block, body_stmt, body_expr);
crate::body_scope_descent!(
    BodyVisitMut,
    body_block_mut,
    body_stmt_mut,
    body_expr_mut,
    mut
);

/// Resolves namespace-qualified references (`ns.member`) in one
/// importing module to program-wide decl symbols. A namespace is a compile-time
/// name, not a value: a bare use of it is an error.
struct NsResolver<'a> {
    /// The module's in-scope namespaces: `ns` name -> target module key.
    ns: HashMap<String, String>,
    foreign_renames: &'a HashMap<(String, String), String>,
    /// Exported decl names (originals) per module: the namespace-reachable surface.
    module_exports: &'a HashMap<String, HashSet<String>>,
    /// Exported-enum variant names per module, to tell variant construction from
    /// a type-name argument.
    module_variants: &'a HashMap<String, HashSet<String>>,
    module_key: String,
    root_key: String,
    errors: &'a mut Vec<Diagnostic>,
}

impl NsResolver<'_> {
    fn err(&mut self, line: usize, msg: String) {
        self.errors
            .push(load_error(&self.module_key, &self.root_key, line, msg));
    }

    /// The program-wide symbol a namespace member resolves to, after any
    /// collision rename, or an error if the target does not export it.
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
        Some(resolved_name(self.foreign_renames, &target, member))
    }

    fn resolve_program(&mut self, p: &mut Program) {
        for f in &mut p.functions {
            let mut locals: HashSet<String> = f.params.iter().map(|pm| pm.name.clone()).collect();
            self.walk_type_positions_fn(f, &locals.clone());
            body_block_mut(&mut f.body, &mut locals, self);
        }
        for im in &mut p.impls {
            self.rewrite_type(&mut im.ty);
            for m in &mut im.methods {
                let mut locals: HashSet<String> =
                    m.params.iter().map(|pm| pm.name.clone()).collect();
                self.walk_type_positions_fn(m, &locals.clone());
                body_block_mut(&mut m.body, &mut locals, self);
            }
            // A projection is an ordinary body the loader never flattened, so
            // its `ns.member` uses resolve like any other.
            for pl in &mut im.places {
                let mut locals: HashSet<String> =
                    pl.params.iter().map(|pm| pm.name.clone()).collect();
                self.walk_type_positions_fn(pl, &locals.clone());
                body_block_mut(&mut pl.body, &mut locals, self);
            }
        }
        for t in &mut p.type_decls {
            if t.line == 0 {
                continue;
            }
            self.rewrite_type(&mut t.base);
            if let Some(pred) = &mut t.predicate {
                let locals: HashSet<String> = std::iter::once("value".to_string()).collect();
                body_expr_mut(pred, &locals, self);
            }
        }
        for g in &mut p.globals {
            if let Some(ty) = &mut g.ty {
                self.rewrite_type(ty);
            }
            let locals = HashSet::new();
            body_expr_mut(&mut g.init, &locals, self);
        }
        for t in &mut p.tests {
            let mut locals = HashSet::new();
            body_block_mut(&mut t.body, &mut locals, self);
        }
        for b in &mut p.benches {
            let mut locals = HashSet::new();
            body_block_mut(&mut b.body, &mut locals, self);
        }
    }

    /// Rewrites namespace-qualified types in a function's signature: parameters,
    /// return type and bounds.
    fn walk_type_positions_fn(&mut self, f: &mut Function, _locals: &HashSet<String>) {
        for pm in &mut f.params {
            self.rewrite_type(&mut pm.ty);
        }
        self.rewrite_type(&mut f.ret);
        for bounds in f.type_bounds.values_mut() {
            for b in bounds.iter_mut() {
                // A bound is a plain string; a dotted `ns.Show` resolves like a
                // type.
                if let Some((ns, member)) = b.split_once('.') {
                    let line = f.line;
                    if let Some(sym) = self.resolve_member(ns, member, line) {
                        *b = sym;
                    }
                }
            }
        }
    }

    /// Rewrites every namespace-qualified head (`ns.User`, `ns.Box<T>`) in a type
    /// to its resolved decl name.
    fn rewrite_type(&mut self, ty: &mut Type) {
        let mut visit = |n: &mut String| {
            let head = n.clone();
            if let Some((ns, member)) = head.split_once('.') {
                if self.ns.contains_key(ns) {
                    if let Some(sym) = self.resolve_member(ns, member, 0) {
                        *n = sym;
                    }
                }
            }
        };
        type_heads_mut(ty, &mut visit);
    }

    /// Whether `ns` is an in-scope namespace here, not shadowed by a local.
    fn is_ns(&self, ns: &str, locals: &HashSet<String>) -> bool {
        self.ns.contains_key(ns) && !locals.contains(ns)
    }
}

/// The namespace pass at each site of [`body_scope_descent`]: it deletes the
/// namespace receiver.
impl BodyVisitMut for NsResolver<'_> {
    fn stmt(&mut self, s: &mut Stmt, _locals: &HashSet<String>) {
        if let Stmt::Let { ty: Some(t), .. } = s {
            self.rewrite_type(t);
        }
    }

    fn expr(&mut self, e: &mut Expr, locals: &HashSet<String>) -> bool {
        match e {
            // `ns.fn(args)` and `ns.Enum.Variant(payload)` both arrive as method
            // sugar with the namespace as the first argument, which goes.
            Expr::Call {
                dot,
                name,
                args,
                type_args,
                line,
                id: _,
            } => {
                let l = *line;
                // A type argument resolves like any other type spelling:
                // `fromJson<shapes.Point>(s)`.
                for t in type_args.iter_mut() {
                    self.rewrite_type(t);
                }
                // `ns.member(rest)`: the first argument is the bare namespace.
                if let Some(Expr::Var { name: head, .. }) = args.first() {
                    if self.is_ns(head, locals) {
                        let head = head.clone();
                        if let Some(sym) = self.resolve_member(&head, name, l) {
                            *name = sym;
                        }
                        args.remove(0);
                        *dot = false;
                        return true;
                    }
                }
                // `ns.Enum.Variant(payload)`: the first argument is `ns.Enum` and
                // the call name is a variant of that module's enums. Otherwise
                // it is `someFn(ns.Type, ..)`, which parses the same, and the
                // `Field` arm rewrites `ns.Type`.
                if let Some(Expr::Field { expr: inner, .. }) = args.first() {
                    if let Expr::Var { name: head, .. } = inner.as_ref() {
                        let is_variant_call = self.is_ns(head, locals)
                            && self
                                .ns
                                .get(head)
                                .and_then(|t| self.module_variants.get(t))
                                .is_some_and(|vs| vs.contains(name));
                        if is_variant_call {
                            // Variants are not renamed; drop the qualifier and
                            // keep the call name.
                            args.remove(0);
                            *dot = false;
                        }
                    }
                }
            }
            Expr::TryConstruct { name, line, .. } | Expr::StructLit { name, line, .. } => {
                // `ns.Type?(..)` and `ns.Type { .. }`: the parser folds the
                // qualifier into the name.
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
            }
            Expr::Field {
                expr,
                field,
                line,
                id: _,
            } => {
                let l = *line;
                // `ns.member`: a type name, a function value or a nullary access.
                if let Expr::Var { name: head, .. } = expr.as_ref() {
                    if self.is_ns(head, locals) {
                        let head = head.clone();
                        if let Some(sym) = self.resolve_member(&head, field, l) {
                            *e = Expr::Var {
                                id: Id::NEW,
                                name: sym,
                                line: l,
                            };
                        }
                        return false;
                    }
                }
                // `ns.Enum.Variant`, a nullary variant: `ns.Enum` is the inner field.
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
                                    id: Id::NEW,
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
                            return false;
                        }
                    }
                }
            }
            Expr::Var { name, line, id: _ } => {
                if self.is_ns(name, locals) {
                    let (name, line) = (name.clone(), *line);
                    self.err(line, format!("namespace `{name}` is not a value"));
                }
            }
            _ => {}
        }
        true
    }

    fn arm_pattern(&mut self, p: &mut Pattern, line: usize, _locals: &HashSet<String>) {
        // An `ns.Enum.Variant` pattern becomes the bare variant: variants are
        // global, and the enum need only be an exported member of the namespace.
        if let Pattern::Variant(v, _) = p {
            if let Some(idx) = v.find('.') {
                let ns = v[..idx].to_string();
                let rest = &v[idx + 1..];
                let variant = rest.rsplit('.').next().unwrap_or(rest).to_string();
                let enum_name = rest.split('.').next().unwrap_or(rest).to_string();
                if self.ns.contains_key(&ns) {
                    let _ = self.resolve_member(&ns, &enum_name, line);
                    *v = variant;
                }
            }
        }
    }
}

fn link(mut modules: Vec<Module>, root_key: &str) -> Result<Program, Vec<Diagnostic>> {
    let mut errors: Vec<Diagnostic> = Vec::new();
    // Fold import aliases into the flat namespace first.
    let alias_span = crate::prof::phase("link: resolve_aliases");
    resolve_aliases(&mut modules, &mut errors, root_key);
    drop(alias_span);
    let index_span = crate::prof::phase("link: index");

    // top-level name -> (module key, exported)
    let mut owner: HashMap<String, (String, bool)> = HashMap::new();
    // enum variant name -> every enum declaring it, as (type, module); protocol
    // method name -> every protocol declaring it. Lists, because two modules may
    // each declare a `render` method or a `None` variant, and a single owner
    // made the answer depend on import order.
    let mut variant_enum: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut method_protocol: HashMap<String, Vec<(String, String)>> = HashMap::new();

    // Flat-namespace collisions, as `(name, first owner, second owner)`.
    // `clash_diagnostics` turns them into one diagnostic per module pair, at an
    // import site in a real file.
    let mut clashes: Vec<(String, String, String)> = Vec::new();

    // Names whose every declaration is a non-exported `extern fn`, in two or
    // more modules: one host-ABI contract restated per module (std/rpc plants
    // `extern fn vyrnRpcCall` in every client stub). Renaming would sever the
    // ABI, so they neither clash nor take part in the foreign-reference check,
    // and the merge keeps one copy.
    let mut extern_totals: HashMap<String, (usize, usize)> = HashMap::new();
    for m in &modules {
        for f in &m.program.functions {
            if !f.exported {
                let e = extern_totals.entry(f.name.clone()).or_default();
                e.0 += 1;
                if f.is_extern {
                    e.1 += 1;
                }
            }
        }
    }
    let shared_externs: HashSet<String> = extern_totals
        .into_iter()
        .filter(|(_, (total, ext))| *ext == *total && *total >= 2)
        .map(|(name, _)| name)
        .collect();

    let mut register =
        |name: &str, module: &str, exported: bool, clashes: &mut Vec<(String, String, String)>| {
            // A reserved name never enters the flat namespace. `owner` decides
            // whether a use is a foreign reference, so registering one made every
            // use of the builtin in a linked `std/` module look unimported. The
            // checker's `RESERVED` guard reports the declaration, once.
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

    // Every declaration form joins one top-level namespace: a contract name is
    // what `contractOf(Name)` resolves, and a module-state binding
    // shares no name with another declaration. Flattened impl methods
    // (`P__Key__m`) cannot collide with a user identifier; they register so
    // duplicate impls across modules collide here.
    for m in &modules {
        for d in decls(&m.program) {
            if d.injected || (d.kind == DeclKind::Fn && shared_externs.contains(d.name)) {
                continue;
            }
            register(d.name, &m.key, d.exported, &mut clashes);
        }
        for t in &m.program.type_decls {
            if is_injected(t) {
                continue;
            }
            if let Some(vs) = crate::types::declared_variants(&t.base) {
                for v in vs {
                    variant_enum
                        .entry(v.name.clone())
                        .or_default()
                        .push((t.name.clone(), m.key.clone()));
                }
            }
        }
        for p in &m.program.protocols {
            for sig in &p.methods {
                method_protocol
                    .entry(sig.name.clone())
                    .or_default()
                    .push((p.name.clone(), m.key.clone()));
            }
        }
    }
    errors.extend(clash_diagnostics(&clashes, &modules, root_key));
    // `owner` keeps only the first module of each collision. The checks below
    // must not report that half as its own error: `map` is defined in
    // `std/stream` even when it lost the flat namespace to `std/arrays`.
    let clashed: HashSet<&str> = clashes.iter().map(|(n, _, _)| n.as_str()).collect();
    drop(index_span);
    let visible_span = crate::prof::phase("link: visibility");

    // Per-module import and visibility checks. `surface_shadows` gathers the
    // shadowing fact for the checker (see `ast::Program::surface_shadows`).
    let mut surface_shadows: HashSet<(Option<String>, String)> = HashSet::new();
    for m in &modules {
        let mut visible: HashSet<String> = HashSet::new(); // foreign names imported here
        for (imp, target) in m.program.imports.iter().zip(&m.import_targets) {
            // A namespace import reaches every exported decl of the
            // target, and its `ns.member` uses already name those symbols, so
            // they are visible.
            if imp.namespace.is_some() {
                for (name, (def_module, exported)) in &owner {
                    if def_module == target && *exported {
                        visible.insert(name.clone());
                    }
                }
            }
            for imp_name in &imp.names {
                // `resolve_aliases` made every import a bare import of a real
                // decl name.
                let name = &imp_name.original;
                match owner.get(name) {
                    Some((def_module, exported)) if def_module == target => {
                        if !exported {
                            errors.push(load_error(
                                &m.key,
                                root_key,
                                imp.line,
                                format!(
                                    "`{name}` exists in `{target}` but is not exported — \
                                         add `export` to its declaration"
                                ),
                            ));
                        }
                        // Importing an enum brings its variants, and a protocol its
                        // methods; the check below resolves them through this name.
                        visible.insert(name.clone());
                    }
                    Some((def_module, _)) if !clashed.contains(name.as_str()) => {
                        errors.push(load_error(
                            &m.key,
                            root_key,
                            imp.line,
                            format!(
                                "`{name}` is not defined in `{target}` (it lives in \
                                     `{def_module}`)"
                            ),
                        ));
                    }
                    // A clashed name: `clash_diagnostics` reported the pair. It
                    // stays visible, so the check below does not blame the module
                    // that merely won the name.
                    Some(_) => {
                        visible.insert(name.clone());
                    }
                    None => {
                        errors.push(load_error(
                            &m.key,
                            root_key,
                            imp.line,
                            format!("`{target}` does not define `{name}`"),
                        ));
                    }
                }
            }
        }

        // Visibility: every foreign name this module references must be imported.
        // A name defined nowhere is the checker's to report. Enum variants map to
        // their enum, protocol methods to their protocol.
        let own: HashSet<&str> = owner
            .iter()
            .filter(|(_, (module, _))| module == &m.key)
            .map(|(n, _)| n.as_str())
            .collect();
        // A generated module may call back into its importer (an RPC
        // dispatcher calling the user's `onGetUser`). The importer's names are
        // visible without an import, because the importer cannot import the
        // generated module's names in reverse.
        let gen_importer: Option<String> = generated_importer(&m.key).map(normalize);
        // Every module this file imports anything from is present for the
        // candidate maps: the checker resolves same-named candidates by
        // receiver type.
        let imported_modules: HashSet<&str> = visible
            .iter()
            .filter_map(|d| owner.get(d.as_str()).map(|(md, _)| md.as_str()))
            .collect();
        // This module's own `extern fn`s. A shared one never entered
        // the flat namespace, so without this set a stub calling its own
        // `vyrnRpcCall` would read as unimported.
        let my_externs: HashSet<&str> = m
            .program
            .functions
            .iter()
            .filter(|f| f.is_extern && !f.exported)
            .map(|f| f.name.as_str())
            .collect();
        // A module shadows a surface builtin when it sees a
        // declaration of that name, its own or imported. Only this loop knows
        // both: `imports` never reach the checker.
        for b in crate::ast::SURFACE_BUILTINS {
            if own.contains(b) || visible.contains(b) {
                let home = if m.key == root_key {
                    None
                } else {
                    Some(m.key.clone())
                };
                surface_shadows.insert((home, b.to_string()));
            }
        }
        let check_name = |name: &str, line: usize, what: &str, errors: &mut Vec<Diagnostic>| {
            // Resolve constructors and methods to their owning declarations.
            // Same-named variants or methods in different modules are ordinary,
            // so every candidate is tried before the use is called foreign.
            let mut candidates: Vec<&(String, String)> = Vec::new();
            // A private `extern fn` of this module resolves to its own copy.
            if my_externs.contains(name) {
                return;
            }
            if let Some(vs) = variant_enum.get(name) {
                candidates.extend(vs);
            }
            if let Some(ps) = method_protocol.get(name) {
                candidates.extend(ps);
            }
            let in_scope = |decl: &str| own.contains(decl) || visible.contains(decl);
            // A declaration this module owns or imports resolves the use,
            // whatever a variant or method elsewhere is called: a user
            // variant `Match` does not hide `std/regex`'s own `Match`.
            if in_scope(name) {
                return;
            }
            // A surface builtin this module has not declared or
            // imported means the builtin, whatever another module declared.
            if crate::ast::is_surface_builtin(name) {
                return;
            }
            if candidates.is_empty() {
                // A plain reference to a top-level decl.
                if let Some((def_module, _)) = owner.get(name) {
                    if def_module != &m.key {
                        if gen_importer.as_deref() == Some(def_module.as_str()) {
                            return;
                        }
                        errors.push(load_error(
                            &m.key,
                            root_key,
                            line,
                            format!(
                                "{what} `{name}` is defined in `{def_module}` but not \
                                     imported here — add it to an `import {{ .. }} from` list"
                            ),
                        ));
                    }
                }
                return;
            }
            // Any candidate this module can see resolves the use: the checker
            // picks between same-named declarations by type.
            if candidates.iter().any(|(decl, def_module)| {
                in_scope(decl)
                    || def_module == &m.key
                    || gen_importer.as_deref() == Some(def_module.as_str())
                    || imported_modules.contains(def_module.as_str())
            }) {
                return;
            }
            // No candidate is imported. Several candidates are all listed, since
            // naming one would be a guess.
            let mut modules: Vec<&str> = candidates.iter().map(|(_, md)| md.as_str()).collect();
            modules.sort_unstable();
            modules.dedup();
            let list = modules.join("`, `");
            errors.push(load_error(
                &m.key,
                root_key,
                line,
                format!(
                    "{what} `{name}` is defined in `{list}` but not imported here — add \
                         it to an `import {{ .. }} from` list"
                ),
            ));
        };

        for f in &m.program.functions {
            // Scope-aware: a local (param, `let`, loop or lambda variable, match
            // bind) shadows a like-named foreign export.
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
            // A projection's body is checked like a function's.
            for pl in &imp.places {
                for c in fn_body_ref_names(pl) {
                    check_name(&c.0, c.1, "function", &mut errors);
                }
                for p in &pl.params {
                    for n in type_names(&p.ty) {
                        check_name(&n, pl.line, "type", &mut errors);
                    }
                }
                for n in type_names(&pl.ret) {
                    check_name(&n, pl.line, "type", &mut errors);
                }
            }
        }
    }

    drop(visible_span);
    if !errors.is_empty() {
        return Err(errors);
    }

    let _merge = crate::prof::phase("link: merge");
    // Merge. The root goes last so its injected builtins and log config win;
    // imported modules' injected decls are dropped.
    let mut merged: Option<Program> = None;
    let mut extra_types = Vec::new();
    let mut extra_fns = Vec::new();
    let mut extra_protocols = Vec::new();
    let mut extra_contracts = Vec::new();
    let mut extra_impls = Vec::new();
    let mut extra_tests = Vec::new();
    let mut extra_benches = Vec::new();
    // Every module's state joins the linked program and initializes before
    // `main` in linker order: dependencies first.
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
            // Imported tests and benches keep their `module` tag: they type-check
            // but do not run under `vyrn test <root>`.
            extra_tests.extend(p.tests);
            extra_benches.extend(p.benches);
        }
    }
    let mut program = merged.expect("root module was loaded");
    program.type_decls.extend(extra_types);
    // Which modules can see a declaration of a surface builtin. The
    // checker cannot compute it, because `imports` are consumed here.
    program.surface_shadows = surface_shadows;
    // One declaration per shared-extern name: the copies are identical, and a
    // second wasm import is waste. The root's copy, or the first imported, wins.
    let mut seen_externs: HashSet<String> = program
        .functions
        .iter()
        .filter(|f| f.is_extern && !f.exported)
        .map(|f| f.name.clone())
        .collect();
    program.functions.extend(
        extra_fns
            .into_iter()
            .filter(|f| !(f.is_extern && !f.exported && !seen_externs.insert(f.name.clone()))),
    );
    program.protocols.extend(extra_protocols);
    program.contracts.extend(extra_contracts);
    program.impls.extend(extra_impls);
    // Init order: `modules` pushes each module after its imports and
    // the root last, so `extra_globals` is already post-order over the import
    // graph, and the root's globals go last.
    let mut ordered = extra_globals;
    ordered.append(&mut program.globals);
    program.globals = ordered;
    program.tests.extend(extra_tests);
    program.benches.extend(extra_benches);
    program.imports.clear(); // consumed
    program.number();
    Ok(program)
}

/// Attaches `key` to a diagnostic as its file, unless `key` is the root: a root
/// diagnostic renders without one, as a single-file program's does. Every load
/// diagnostic learns its file here.
fn in_module(mut d: Diagnostic, key: &str, root_key: &str) -> Diagnostic {
    if key != root_key {
        d.file = Some(key.to_string());
    }
    d
}

/// A load error at `line` of `key`, located by [`in_module`].
fn load_error(key: &str, root_key: &str, line: usize, msg: String) -> Diagnostic {
    in_module(Diagnostic::error(line, 0, "load", msg), key, root_key)
}

/// The import of `target` a diagnostic should point at, with its module.
/// Prefers an import that names one of `names`, the line the user must edit.
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

/// Turns the flat-namespace collisions into one diagnostic per module pair, at an
/// import of one of them.
///
/// A pair of modules sharing several names is one problem, and the fix is a
/// namespace import. So the names are grouped, and the diagnostic sits at an
/// import site in a real file, not at a foreign declaration's line.
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
        // Prefer an import of the second module: its names lost, and dropping it
        // is the smaller edit. The root is imported by nobody, so fall back to
        // the first.
        let (m, imp) = match import_site(modules, second, &names)
            .or_else(|| import_site(modules, first, &names))
        {
            Some(site) => site,
            // A linked module was imported, so a site exists; the diagnostic
            // survives even if it does not.
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
        // Lead with a name the user wrote at this line; the rest go to the
        // note, sorted.
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
        let d = Diagnostic::error(
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
        let note = if rest.is_empty() {
            fix
        } else {
            let list: Vec<String> = rest.iter().map(|n| format!("`{n}`")).collect();
            format!(
                "{fix}; {} collide{} the same way",
                list.join(", "),
                if rest.len() == 1 { "s" } else { "" }
            )
        };
        out.push(in_module(d.with_note(note), &m.key, root_key));
    }
    out
}

/// Every name a function references that could name a program-level
/// declaration, with its line, minus names bound by a local in scope. A local
/// shadows a like-named foreign export. Type-position names are always kept,
/// since a value local never shadows a type. The scope starts with the
/// function's params.
fn fn_body_ref_names(f: &Function) -> Vec<(String, usize)> {
    let mut v = RefNames { out: Vec::new() };
    let mut locals: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
    body_block(&f.body, &mut locals, &mut v);
    v.out
}

/// The four variant names of the built-in sums. They are patterns
/// but name no declaration, so a walk that collects free names skips them.
fn is_sum_arm(name: &str) -> bool {
    matches!(name, "Some" | "None" | "Ok" | "Err")
}

/// The reference collector at each site of [`body_scope_descent`].
struct RefNames {
    out: Vec<(String, usize)>,
}

impl BodyVisit<'_> for RefNames {
    fn stmt(&mut self, s: &Stmt, _locals: &HashSet<String>) {
        // A `let x: T` annotation is a reference: a value local never shadows a
        // type.
        if let Stmt::Let {
            ty: Some(t), line, ..
        } = s
        {
            for n in type_names(t) {
                self.out.push((n, *line));
            }
        }
    }

    fn expr(&mut self, e: &Expr, locals: &HashSet<String>) -> bool {
        match e {
            // Method sugar `ns.f(x)` parses as callee `f` with the namespace as
            // first argument. When the receiver names one of the module's
            // namespaces, record the dotted spelling, not a bare `f`.
            Expr::Call {
                dot: _,
                name,
                args,
                line,
                type_args: _,
                id: _,
            } => {
                let mut sugar = false;
                if let Some(Expr::Var { name: recv, .. }) = args.first() {
                    sugar = !locals.contains(recv) && SCOPE_NS.with(|s| s.borrow().contains(recv));
                }
                if sugar {
                    if let Some(Expr::Var { name: recv, .. }) = args.first() {
                        self.out.push((format!("{recv}.{name}"), *line));
                    }
                } else if !locals.contains(name) {
                    self.out.push((name.clone(), *line));
                    // `f(x)` is also how method sugar `x.f()` arrives. For
                    // `program_ref_kinds`, count it, so a name seen only here is
                    // told apart from one also used as a variable, a type or a
                    // zero-argument call, none of which can be method dispatch.
                    if !args.is_empty() {
                        SCOPE_AMB.with(|a| {
                            if let Some(amb) = a.borrow_mut().as_mut() {
                                *amb.entry(name.clone()).or_default() += 1;
                            }
                        });
                    }
                }
            }
            Expr::StructLit { name, line, .. }
            | Expr::TryConstruct { name, line, .. }
            | Expr::Var { name, line, id: _ } => {
                if !locals.contains(name) {
                    self.out.push((name.clone(), *line));
                }
            }
            _ => {}
        }
        true
    }

    fn arm_pattern(&mut self, p: &Pattern, line: usize, locals: &HashSet<String>) {
        // The variant constructor is a reference; its binds are new locals. The
        // four built-in sum names have no declaration.
        if let Pattern::Variant(v, _) = p {
            if !locals.contains(v) && !is_sum_arm(v) {
                self.out.push((v.clone(), line));
            }
        }
    }
}

/// Every named or applied type inside `ty`, in order.
fn type_names(ty: &Type) -> Vec<String> {
    let mut out = Vec::new();
    type_heads(ty, &mut |n| out.push(n.clone()));
    out
}

// Alias reference rewriting: every reference to an alias `Y` becomes
// the decl name it stands for, in each module's copy before flattening, so the
// passes after the link never see aliases. The LSP indexes the unlinked root,
// so hover still sees `Y`.

/// The program-wide symbol `original` names in module `target` once every
/// rename apart is decided; `original` itself when no rule renamed it.
fn resolved_name(
    renames: &HashMap<(String, String), String>,
    target: &str,
    original: &str,
) -> String {
    renames
        .get(&(target.to_string(), original.to_string()))
        .cloned()
        .unwrap_or_else(|| original.to_string())
}

/// Every name a module's imports bring into scope: the alias where one is
/// written, the declaration's own name otherwise.
fn import_locals(p: &Program) -> HashSet<String> {
    p.imports
        .iter()
        .flat_map(|imp| imp.names.iter())
        .map(|n| n.local().to_string())
        .collect()
}

/// The namespace names module `key` binds; passes 3 and 4 guard their
/// rewrites with them.
fn ns_names_of(binds: &HashMap<String, Vec<(String, String)>>, key: &str) -> HashSet<String> {
    binds
        .get(key)
        .into_iter()
        .flatten()
        .map(|(n, _)| n.clone())
        .collect()
}

fn ren<'a>(map: &'a HashMap<String, String>, n: &'a str) -> String {
    map.get(n).cloned().unwrap_or_else(|| n.to_string())
}

/// Every enum variant name `p` declares itself.
fn own_variant_names(p: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    for t in &p.type_decls {
        if let Some(vs) = crate::types::declared_variants(&t.base) {
            out.extend(vs.iter().map(|v| v.name.clone()));
        }
    }
    out
}

/// Rewrites every referenced type name in `ty` through `map`.
fn rewrite_type(ty: &mut Type, map: &HashMap<String, String>) {
    type_heads_mut(ty, &mut |n| *n = ren(map, n));
}

/// Rewrites every reference to a declaration name in `p` through `map`.
///
/// Generated source spells the injected module's reserved names as `VyrnRt_`
/// placeholders, because a `$` does not lex, and this pass folds them back.
pub(crate) fn rewrite_names(p: &mut Program, map: &HashMap<String, String>) {
    rewrite_module_refs(p, map, &HashSet::new(), &HashSet::new());
}

/// The reference renamer at each site of [`body_scope_descent`].
struct Renamer<'a> {
    map: &'a HashMap<String, String>,
    /// The module's namespace-binding names. A call whose receiver is
    /// a bare namespace (`ns.member(..)`) is a namespace member reference that
    /// pass 5 owns, so its call name is left alone; renaming it would turn
    /// `ns.member` into `ns.renamed` before pass 5 resolves it.
    ns: &'a HashSet<String>,
    /// The enum variant names this module declares. A call `V(x)` or pattern
    /// `V(..)` with one of these constructs the module's own enum and is not a
    /// reference to a same-spelled global or protocol.
    variants: &'a HashSet<String>,
}

impl BodyVisitMut for Renamer<'_> {
    fn stmt(&mut self, s: &mut Stmt, locals: &HashSet<String>) {
        match s {
            Stmt::Let { ty: Some(t), .. } => rewrite_type(t, self.map),
            // An assignment target is a reference too: module state
            // is a top-level decl, so a rename reaches `g = v` as it reaches reads
            // of `g`. A local of that name is not the decl. `drop g` names a
            // binding the same way.
            Stmt::Assign { name, .. }
            | Stmt::SetField { name, .. }
            | Stmt::IndexSet { name, .. }
            | Stmt::Drop { name, .. } => {
                if !locals.contains(name) {
                    *name = ren(self.map, name);
                }
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr, locals: &HashSet<String>) -> bool {
        match e {
            Expr::Call {
                name,
                args,
                type_args,
                ..
            } => {
                // A type argument is renamed too: `fromJson<Pt>(s)` names a type
                // this module may have renamed for privacy.
                for t in type_args.iter_mut() {
                    rewrite_type(t, self.map);
                }
                let ns_receiver =
                    matches!(args.first(), Some(Expr::Var { name: h, .. }) if self.ns.contains(h));
                let ctor = self.variants.contains(name.as_str());
                if !ns_receiver && !locals.contains(name) && !ctor {
                    *name = ren(self.map, name);
                }
            }
            Expr::TryConstruct { name, .. }
            | Expr::StructLit { name, .. }
            | Expr::Var { name, .. } => {
                if !locals.contains(name) {
                    *name = ren(self.map, name);
                }
            }
            _ => {}
        }
        true
    }

    fn arm_pattern(&mut self, p: &mut Pattern, _line: usize, _locals: &HashSet<String>) {
        // A `match` arm always constructs; it is never a declaration reference.
        self.rename_variant(p);
    }
}

impl Renamer<'_> {
    fn rename_variant(&self, p: &mut Pattern) {
        if let Pattern::Variant(v, _) = p {
            if !self.variants.contains(v.as_str()) {
                *v = ren(self.map, v);
            }
        }
    }
}

/// Rewrites one function's signature types and body references through `map`.
/// The params seed the local set, so a param or `let` that shadows a renamed
/// decl keeps naming the local.
fn rewrite_function(f: &mut Function, rn: &mut Renamer) {
    for p in &mut f.params {
        rewrite_type(&mut p.ty, rn.map);
    }
    rewrite_type(&mut f.ret, rn.map);
    // A `<T: P>` bound naming an aliased protocol resolves through `map` too.
    for bounds in f.type_bounds.values_mut() {
        for b in bounds.iter_mut() {
            *b = ren(rn.map, b);
        }
    }
    let mut locals: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
    body_block_mut(&mut f.body, &mut locals, rn);
}

/// Rewrites every reference (types, calls, variables, bounds) in one module
/// through `map`. Declaration names stay; [`rename_decls_in_module`] renames
/// those. `ns` is the module's namespace-binding names (see [`Renamer`]).
fn rewrite_module_refs(
    p: &mut Program,
    map: &HashMap<String, String>,
    ns: &HashSet<String>,
    variants: &HashSet<String>,
) {
    if map.is_empty() {
        return;
    }
    let rn = &mut Renamer { map, ns, variants };
    for f in &mut p.functions {
        rewrite_function(f, rn);
    }
    for im in &mut p.impls {
        im.protocol = ren(map, &im.protocol);
        rewrite_type(&mut im.ty, map);
        for m in &mut im.methods {
            rewrite_function(m, rn);
        }
        // A `place` projection is never flattened into `Program::functions`, so
        // its body needs its own walk.
        for pl in &mut im.places {
            rewrite_function(pl, rn);
        }
    }
    for t in &mut p.type_decls {
        rewrite_type(&mut t.base, map);
        if let Some(pred) = &mut t.predicate {
            // A refinement predicate has no locals of its own.
            body_expr_mut(pred, &HashSet::new(), rn);
        }
    }
    for g in &mut p.globals {
        if let Some(t) = &mut g.ty {
            rewrite_type(t, map);
        }
        // A global initializer runs at module-state init: no locals in scope.
        body_expr_mut(&mut g.init, &HashSet::new(), rn);
    }
    for pr in &mut p.protocols {
        for m in &mut pr.methods {
            for t in &mut m.params {
                rewrite_type(t, map);
            }
            rewrite_type(&mut m.ret, map);
        }
    }
    // A contract member's types name declarations too.
    for c in &mut p.contracts {
        for m in &mut c.members {
            match &mut m.kind {
                crate::ast::ContractMemberKind::Value { ty, default } => {
                    rewrite_type(ty, map);
                    if let Some(d) = default {
                        body_expr_mut(d, &HashSet::new(), rn);
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
                        body_expr_mut(d, &HashSet::new(), rn);
                    }
                }
            }
        }
    }
    for t in &mut p.tests {
        body_block_mut(&mut t.body, &mut HashSet::new(), rn);
    }
    for b in &mut p.benches {
        body_block_mut(&mut b.body, &mut HashSet::new(), rn);
    }
}

// Every reference name (types, callees, variables, variants) in a module's
// declarations, for the check that an aliased import's original is not
// used directly. Bodies are scanned scope-aware, so a local does not count as a
// reference; type positions always count.
// `//` and not `///`: a doc comment on `thread_local!` documents nothing and
// rustc warns.
thread_local! {
    /// The namespace bindings of the module `program_ref_names` is walking, so
    /// the walk tells method sugar (`ns.f(x)`, recorded qualified) from a flat
    /// call of the same spelling (recorded bare).
    static SCOPE_NS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// While [`program_ref_kinds`] walks: each argument-bearing call callee with
    /// its occurrence count. `f(x)` may be method sugar for `x.f()`, so it alone
    /// cannot prove a flat use of `f`. `None` while other walkers run.
    static SCOPE_AMB: RefCell<Option<HashMap<String, usize>>> = const { RefCell::new(None) };
}

fn program_ref_names(p: &Program) -> HashSet<String> {
    program_ref_kinds(p, false).0
}

/// [`program_ref_names`], plus the names whose every occurrence is an
/// argument-bearing call callee (only with `split_ambiguous`): those may be
/// method dispatch, and nothing else about them proves otherwise.
fn program_ref_kinds(p: &Program, split_ambiguous: bool) -> (HashSet<String>, HashSet<String>) {
    let mut out: HashSet<String> = HashSet::new();
    let mut totals: HashMap<String, usize> = HashMap::new();
    // The walker reads this module's namespaces through `SCOPE_NS`.
    SCOPE_NS.with(|s| {
        *s.borrow_mut() = p
            .imports
            .iter()
            .filter_map(|i| i.namespace.clone())
            .collect()
    });
    SCOPE_AMB.with(|s| {
        *s.borrow_mut() = split_ambiguous.then(HashMap::new);
    });
    fn add_scoped_block<I: Iterator<Item = String>>(
        b: &Block,
        params: I,
        out: &mut HashSet<String>,
        totals: &mut HashMap<String, usize>,
    ) {
        let mut locals: HashSet<String> = params.collect();
        let mut v = RefNames { out: Vec::new() };
        body_block(b, &mut locals, &mut v);
        for (n, _) in v.out {
            *totals.entry(n.clone()).or_default() += 1;
            out.insert(n);
        }
    }
    let add_type = |t: &Type, out: &mut HashSet<String>, totals: &mut HashMap<String, usize>| {
        for n in type_names(t) {
            *totals.entry(n.clone()).or_default() += 1;
            out.insert(n);
        }
    };
    for f in &p.functions {
        for pm in &f.params {
            add_type(&pm.ty, &mut out, &mut totals);
        }
        add_type(&f.ret, &mut out, &mut totals);
        add_scoped_block(
            &f.body,
            f.params.iter().map(|p| p.name.clone()),
            &mut out,
            &mut totals,
        );
    }
    for im in &p.impls {
        *totals.entry(im.protocol.clone()).or_default() += 1;
        out.insert(im.protocol.clone());
        add_type(&im.ty, &mut out, &mut totals);
        for m in &im.methods {
            for pm in &m.params {
                add_type(&pm.ty, &mut out, &mut totals);
            }
            add_type(&m.ret, &mut out, &mut totals);
            add_scoped_block(
                &m.body,
                m.params.iter().map(|p| p.name.clone()),
                &mut out,
                &mut totals,
            );
        }
        // A `place` projection is never flattened, so its references need their
        // own walk.
        for pl in &im.places {
            for pm in &pl.params {
                add_type(&pm.ty, &mut out, &mut totals);
            }
            add_type(&pl.ret, &mut out, &mut totals);
            add_scoped_block(
                &pl.body,
                pl.params.iter().map(|p| p.name.clone()),
                &mut out,
                &mut totals,
            );
        }
    }
    for t in &p.type_decls {
        add_type(&t.base, &mut out, &mut totals);
    }
    for g in &p.globals {
        if let Some(t) = &g.ty {
            add_type(t, &mut out, &mut totals);
        }
    }
    for t in &p.tests {
        add_scoped_block(&t.body, std::iter::empty(), &mut out, &mut totals);
    }
    for b in &p.benches {
        add_scoped_block(&b.body, std::iter::empty(), &mut out, &mut totals);
    }
    // Clear the thread-locals the walker read.
    SCOPE_NS.with(|s| s.borrow_mut().clear());
    let amb = SCOPE_AMB.with(|s| s.borrow_mut().take().unwrap_or_default());
    // Ambiguity-only names stay in `out`: the hidden-original check
    // must still fire for a name that is not a protocol method. The one caller
    // with the method surface applies the narrower skip itself.
    let mut ambiguous_only = HashSet::new();
    if split_ambiguous {
        for (n, c) in amb {
            if totals.get(&n).copied() == Some(c) {
                ambiguous_only.insert(n);
            }
        }
    }
    (out, ambiguous_only)
}

/// Applies every rename in `map` to module `p`: its top-level declarations and
/// its own references to them. It frees a foreign name for a co-naming
/// importer's stub and gives an injected runtime module its reserved
/// spellings.
///
/// One walk per module, not per rename: `std/runtime` is in every program, and a
/// walk per declaration is quadratic in it. One walk gives the same answer
/// because renames cannot chain: a reserved spelling holds a `$` and a minted
/// one a `__fromN`, and no declared name holds either.
fn rename_decls_in_module(p: &mut Program, map: &HashMap<String, String>, ns: &HashSet<String>) {
    for t in &mut p.type_decls {
        t.name = ren(map, &t.name);
    }
    for f in &mut p.functions {
        f.name = ren(map, &f.name);
    }
    for pr in &mut p.protocols {
        pr.name = ren(map, &pr.name);
    }
    for c in &mut p.contracts {
        c.name = ren(map, &c.name);
    }
    for g in &mut p.globals {
        g.name = ren(map, &g.name);
    }
    // Construction sites follow the map: a variant guard here never changed a
    // site.
    rewrite_module_refs(p, map, ns, &HashSet::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests of this module's internals only. A test that runs a linked program
    // needs the driver's engine, and a dev-dependency on `vyrn-cli` would build a
    // second copy of this crate, so those tests live in `tests/loader_run.rs`.
    fn map(entries: &[(&str, &str)]) -> MapResolver {
        MapResolver(
            entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn opts() -> LoadOptions {
        LoadOptions {
            std_root: Some("std".into()),
            ..Default::default()
        }
    }

    #[test]
    fn a_generator_chain_nesting_past_the_cap_is_a_diagnostic_not_an_abort() {
        // A nested generator load gets a fresh module-state map, so a chain that
        // mints growing arguments never trips the cycle check. The nesting
        // counter refuses it.
        LOAD_DEPTH.with(|d| d.set(GEN_DEPTH_MAX + 1));
        let (r, _, _, _) = load_with_origins(
            "fn main() -> Int64 { return 0 }",
            "main.vyrn",
            &opts(),
            &map(&[]),
        );
        LOAD_DEPTH.with(|d| d.set(0));
        let e = r.unwrap_err();
        assert!(
            e[0].message.contains("nest more than 32 deep"),
            "{}",
            e[0].message
        );
    }

    #[test]
    fn a_cache_entry_from_an_older_format_misses_instead_of_being_misread() {
        // A `v1` entry cannot say whether a missing file has since appeared, and
        // a `v2` entry is not authenticated. Both miss.
        let key = "k";
        let inputs = [("data/a.txt".to_string(), "deadbeef".to_string())];
        let v3 = render_cache_entry(key, &inputs, "export fn n() -> Int64 { return 1 }");
        assert!(read_cache_entry(key, &v3).is_some());
        for older in [
            "v2 1\ndata/a.txt\tdeadbeef\nx",
            "v1\ndata/a.txt\tdeadbeef\nx",
        ] {
            assert!(
                read_cache_entry(key, older).is_none(),
                "an entry in an older format must not parse: {older:?}"
            );
        }
    }

    /// A hit is linked without re-running the generator, so an entry written by
    /// anything but this compiler must not be read back.
    #[test]
    fn a_cache_entry_this_compiler_did_not_write_is_refused() {
        let key = "k";
        let inputs = [("data/a.txt".to_string(), "deadbeef".to_string())];
        let output = "export fn n() -> Int64 { return 1 }";
        let honest = render_cache_entry(key, &inputs, output);
        assert!(read_cache_entry(key, &honest).is_some(), "the control");

        // An entry declaring zero inputs satisfies `all` vacuously.
        let vacuous = format!(
            "{CACHE_ENTRY_TAG} {} 0\nexport fn n() -> Int64 {{ return 999 }}",
            {
                let body = "0\nexport fn n() -> Int64 { return 999 }";
                entry_tag(key, body)
            }
        );
        assert!(
            read_cache_entry(key, &vacuous).is_none(),
            "an entry recording no inputs describes no generation"
        );

        // The output swapped under an otherwise honest record.
        let swapped = honest.replace("return 1", "return 999");
        assert!(
            read_cache_entry(key, &swapped).is_none(),
            "the tag covers the generated source"
        );

        // The recorded inputs rewritten to files that happen to match.
        let relabelled = honest.replace("data/a.txt", "data/z.txt");
        assert!(
            read_cache_entry(key, &relabelled).is_none(),
            "the tag covers the recorded inputs"
        );

        // A valid entry moved to another lookup key.
        assert!(
            read_cache_entry("other-key", &honest).is_none(),
            "the tag covers the lookup key"
        );

        // A file in no format at all.
        for junk in ["", "\n", "v3\n", "v3 x y\n", "not an entry at all\n"] {
            assert!(read_cache_entry(key, junk).is_none(), "junk: {junk:?}");
        }
    }

    /// An input count off the first line of the file must not size a `Vec`: a
    /// truncated write can make it `usize::MAX`.
    #[test]
    fn an_impossible_input_count_is_a_miss_not_an_abort() {
        let key = "k";
        let body = format!("{}\ndata/a.txt\tdeadbeef\nout", u64::MAX);
        let entry = format!("{CACHE_ENTRY_TAG} {} {body}", entry_tag(key, &body));
        assert!(read_cache_entry(key, &entry).is_none());
    }

    #[test]
    fn two_compilers_do_not_share_a_generator_output() {
        let key = |id: &str| generator_cache_key(id, "m.vyrn", "g", "x", &[]);
        assert_ne!(key("0.1.0:1:2"), key("0.1.0:1:3"));
    }
}
