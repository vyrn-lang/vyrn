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

use std::cell::RefCell;
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
        Err(crate::trap::io_at("listerr", resolved))
    }
    /// The same listing with each entry's KIND (RFC-0119): a directory's name
    /// carries a trailing `/`, a file's does not. One convention instead of a
    /// record type, and unambiguous because no entry NAME can contain a slash.
    /// This exists because `list`'s error cannot tell "not a directory" from
    /// "unreadable" — the project single-sources its I/O error strings and will
    /// not parse operating-system wording — so a caller that needs to descend
    /// listed a directory once to test it and again to walk it. Default:
    /// unsupported, exactly as `list`.
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        Err(crate::trap::io_at("listerr", resolved))
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
            return Err(crate::trap::io_at("listerr", resolved));
        }
        Ok(names.into_iter().collect())
    }
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        // A segment with more path after it is a directory of the virtual tree;
        // an exact key is a file. Both can hold at once (a key `a/b` beside a
        // key `a/b/c` names `b` twice) — the map cannot happen on a real
        // filesystem, and the directory reading wins because it is the one a
        // walker acts on.
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
        // The separators are control characters in the key; spell them back as
        // ` at ` for the reader, and drop the trailing one the suffix strip leaves.
        let head = key
            .strip_suffix(importer)
            .unwrap_or(key)
            .trim_end()
            .trim_end_matches(GEN_SEP)
            .replace(GEN_SEP, " at ");
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

/// The one separator inside a generated module's banner key that cannot occur
/// in a path (a control character does not survive a filename), so the banner
/// splits cleanly no matter what the importer's directory is spelled like.
pub(crate) const GEN_SEP: &str = "\u{1f}";

/// If `key` is a generated module's banner (`generated by <fn>(<args>)` +
/// [`GEN_SEP`] + `<importer>`, RFC-0021), the real importer file it was
/// synthesized for; otherwise `None`. A generated module has no path of its
/// own, so its relative/bare imports — and its visibility into the surrounding
/// program — resolve against this real importer, not the banner text.
///
/// The separator is a control character because the importer is a PATH, and a
/// path may contain anything spellable — ` at ` included, which the previous
/// space-separated banner split on and so truncated every importer whose
/// directory happened to contain it. A nested banner's importer is itself a
/// banner; the unwrapping repeats until a real file falls out.
///
/// Banners written before the separator existed are still parsed the old way
/// (last `" at "`): cached generations outlive the compiler that wrote them.
///
/// Public since RFC-0072: a generated module's AUDIENCE is the audience of the
/// file it was synthesized for, so [`crate::audience`] asks the same question.
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
    /// RFC-0103 M2: the artifacts this project declares, or `None` when the
    /// manifest declares none. The floor ([`crate::floor`]) runs when the root
    /// being loaded IS one artifact's entry point, and never otherwise — the
    /// same absolute opt-in `audience` has, one step narrower: a file inside a
    /// project that declares artifacts is not itself checked unless it is one.
    pub artifacts: Option<crate::artifacts::ArtifactMap>,
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
        audience::remedy(to.audience, importer, imported, map),
        from.because()
    ));
    Some(d)
}

/// The fence of PLAN-0125-runtime §3: `std/mem` may be imported by
/// `std/runtime` and by nothing else, and `std/runtime` by nothing at all.
///
/// The audience is a constant here beside the standard-library table rather
/// than a key `vyrn.json` reads, so no manifest widens it. It runs whether or
/// not the project declared an `audience` key, which is what separates it from
/// [`audience_objection`]: that one is a fence a project opts into, this one is
/// the compiler's own. The diagnostic keeps the audience shape (RFC-0072
/// §Enforcement) so a reader meets one wording for "you may not import this".
///
/// Identity is by resolved path, the same resolution every import takes. A
/// file that merely calls itself `std/runtime.vyrn` inside a project resolves
/// to its own path, not to the std root's, and gets no primitives.
fn runtime_fence(
    importer: &str,
    imported: &str,
    line: usize,
    opts: &LoadOptions,
) -> Option<Diagnostic> {
    // A key and a std spec can spell one file two ways — `vyrn check
    // std/runtime.vyrn` names the root relative to the shell and the std root
    // absolutely — so identity falls back to the real path. Computed only once
    // an import has resolved to one of the two fenced modules.
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
    let mut d = Diagnostic::error(
        line,
        0,
        "audience",
        format!("`{shown}` cannot import `{fenced}`, whose audience is the runtime"),
    );
    d.note = Some(format!(
        "audience `{RUNTIME_SPEC}` is declared by the compiler (RFC-0125 §2.4), not by \
         vyrn.json; the safe surface over `std/mem` is what `{RUNTIME_SPEC}` exports, \
         and the compiler links that into every program"
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
    /// Linked into every program, mentioned or not. Two modules are:
    /// [`RUNTIME_SPEC`], because the wasm emitter calls its functions from
    /// lowerings no builtin name announces (a `String` comparison is a call to
    /// `strCmp`), so no mention scan could gate it (PLAN-0125-runtime §3.2
    /// step 4); and `std/text`, because the runtime's own `intStr` mentions
    /// `stringFromBytes` and enters the load inside this loop, after the scan
    /// (RFC-0125 §3 M6, the third judgment's fifth slice).
    pub always: bool,
}

/// The module the compiler carries as its runtime (RFC-0125 §2.4). It is the
/// one member of `std/mem`'s audience, and nothing may import it.
pub const RUNTIME_SPEC: &str = "std/runtime";

/// The raw-memory primitives (PLAN-0125-runtime §2.1). Its audience is
/// `{ std/runtime }`, declared here rather than by any `vyrn.json`, which is
/// what makes the fence a floor: no manifest key widens it. [`runtime_fence`]
/// is the check.
pub const MEM_SPEC: &str = "std/mem";

/// The two modules behind the fence. A reader outside the compiler cannot
/// import either, so a list written for readers — `vyrn doc --std`, the site's
/// reference shelf — leaves them out. One predicate, so the fence and the
/// listing cannot disagree (RFC-0125 §2.4).
pub fn is_fenced(spec: &str) -> bool {
    spec == MEM_SPEC || spec == RUNTIME_SPEC
}

/// The reserved prefix of every `std/mem` declaration. The emitter matches on
/// it to lower a call to one instruction rather than to a `call`.
pub const MEM_PREFIX: &str = "mem$";

/// The reserved prefix of every `std/runtime` declaration.
pub const RUNTIME_PREFIX: &str = "runtime$";

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
        always: false,
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
        always: false,
    },
    // RFC-0078 M4b(2)/M4c: `@charCount` — the census's one builtin with no
    // justification for being one. It is spelled with the `@` because that is what
    // the parser produces: `s.charCount()` is method-only, so the AST call name is
    // `@charCount` and that is the string every engine looks up. A method spelling
    // has no free form, so it could not become an import and it stays routed.
    //
    // `chars` was the other route here and RFC-0094 M2 took it: it has a free
    // spelling, so `import { chars } from "std/text"` is the whole of what routing
    // was doing for it. `lineAt`/`colAt` never routed at all, and M2 left them
    // alone — see the M4c note in RFC-0078 and the doc on `lineAtV`: the
    // interpreter memoizes a line-start table that a Vyrn library cannot, worth
    // 122 ms of a 291 ms `std/vyx` page compile.
    //
    // RFC-0125 §3 M6 (the third judgment's fifth slice) added `stringFromBytes`
    // as a DESUGAR rather than a route: only the CHECK half moved here
    // ([`STRING_FAULT`]), and each engine still builds the `String` itself,
    // which needs the primitives `std/mem` fences.
    //
    // `always`, and the reason is [`RUNTIME_SPEC`]'s own `intStr`: it makes its
    // digits into a `String` with `stringFromBytes`, because that builtin is the
    // ONLY route from bytes to a `String` a Vyrn body has. So the check is in
    // every program's closure whatever the mention scan says, and the scan could
    // not see it anyway — it reads the modules loaded before this loop, and the
    // runtime enters inside it. The price is the direct backend's to sweep: a
    // program that formats no integer and makes no `String` from bytes reaches
    // neither function and carries neither.
    RtModule {
        spec: "std/text",
        prefix: "text$",
        desugared: &["stringFromBytes"],
        routes: &[("@charCount", "text$charCountV")],
        always: true,
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
        // `assertEq` renders a mismatched float the way `@str` does (RFC-0125 §3
        // M5), so a test that compares floats and prints nothing still links
        // the formatter.
        desugared: &["@str", "print", "assertEq"],
        routes: &[],
        always: false,
    },
    // RFC-0125 §2.4 / PLAN-0125-runtime §3.2: the runtime module, in every
    // program. `std/mem` is not listed here because nothing injects it: it
    // enters as `std/runtime`'s import and is marked with its prefix below.
    RtModule {
        spec: RUNTIME_SPEC,
        prefix: RUNTIME_PREFIX,
        desugared: &[],
        routes: &[],
        always: true,
    },
    RtModule {
        spec: MEM_SPEC,
        prefix: MEM_PREFIX,
        desugared: &[],
        routes: &[],
        always: false,
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

/// The reserved spelling of `std/text`'s byte check — the ONE statement of what a
/// `String` may hold (RFC-0125 §3 M6, the third judgment's fifth slice), reached
/// from `stringFromBytes` in all three engines. It answers 0 for bytes that can be
/// a `String`, 1 for an embedded NUL and 2 for bytes that are not UTF-8; the
/// wording for 1 and 2 is [`crate::trap::io`]'s `bnul` and `butf8`, as it always
/// was.
///
/// Not a `routes` row, for the reason [`F64_STR`] is not one: a route is a
/// whole-builtin rename and this is one HALF of one. The BUILD stays per engine
/// because it allocates, and allocation is what `std/mem`'s fence is around.
/// `the_string_check_is_std_texts` is what keeps the prefix and the name honest.
pub const STRING_FAULT: &str = "text$stringFault";

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
///
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
    if depth > GEN_DEPTH_MAX {
        LOAD_DEPTH.with(|d| d.set(d.get() - 1));
        // Each nested generator load gets a FRESH module-state map, so the
        // ordinary import-cycle check never sees a self-propagating chain — a
        // generator that mints a growing argument (`g(x + "1")` from `g(x)`)
        // recursed until the stack died, an abort with no diagnostic. The
        // counter above already tracks the nesting; comparing it turns the
        // abort into a named cycle error.
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

/// The floor's [`crate::floor::Graph`] for a linked load — every module the
/// artifact contains, INCLUDING the ones no resolver could read: a generator's
/// output (RFC-0021) and the runtime modules a builtin's desugar injects
/// (RFC-0078).
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

/// The floor's graph and the load's root key — what `vyrn why --capability`
/// reports over.
///
/// The same function the check walks ([`floor_graph`]), so the report cannot
/// under-report a capability that a GENERATED module carries: an rpc or connect
/// client stub declares the `vyrnRpcCall` `extern` that no author wrote and no
/// reading of the project's own files can find (RFC-0103 M4 finding 2).
///
/// The two policies that refuse over this graph are the caller's to arm. A
/// report has to be able to answer for the tree you are asking about, which is
/// usually the one that was just refused, so `vyrn why` clears `opts.audience`
/// and `opts.artifacts` and gets the graph instead of the objection.
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
    // A deferred floor decision belongs to ONE load (RFC-0125 M6, fourth
    // slice). Nobody is obliged to check the program a load returns, so the
    // next outermost load drops whatever the last one held.
    if LOAD_DEPTH.with(|d| d.get()) <= 1 {
        crate::floor::forget();
    }
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
        // `generated_importer` unwraps nested banners to the real on-disk file
        // in one step.
        if key.starts_with("generated by ") {
            let importer = generated_importer(key).unwrap_or(key);
            // One reading of the generated text's lexical structure serves both
            // scans: a `//@origin` or `//@diag` is honoured only where the LEXER
            // says a comment begins, so text a generator copied through from an
            // input file — a string literal in a `.vyx` — is data, not a control
            // line (RFC-0054's `lex()` exists for this class of problem).
            let ctx = crate::origin::Context::new(&text, dir_of(importer), &opts.alias_base);
            origins.add_module(key, &text, &ctx);
            // RFC-0071 M2b, RFC-0099: the same line-scan lifts `//@diag`
            // directives into diagnostics at the severity the generator chose. A
            // page is generated twice (server + client bundle), and a generator
            // may be re-entered, so the same report arrives more than once for
            // one authored line — de-duplicate on what the user sees (file,
            // line, message), never on the banner.
            let mut errors: Vec<Diagnostic> = Vec::new();
            for d in crate::origin::diagnostics(key, &text, &ctx) {
                let seen = |list: &[Diagnostic]| {
                    list.iter().any(|w: &Diagnostic| {
                        w.file == d.file && w.line == d.line && w.message == d.message
                    })
                };
                match d.severity {
                    // An error a generator reports fails the load, like every
                    // other error: it travels in the `Err` arm, not beside the
                    // program. Reporting it here rather than at the import site
                    // keeps the anchor the generator gave.
                    crate::diagnostics::Severity::Error => {
                        if !seen(&errors) {
                            errors.push(d);
                        }
                    }
                    crate::diagnostics::Severity::Warning => {
                        if !seen(warnings) {
                            warnings.push(d);
                        }
                    }
                }
            }
            if !errors.is_empty() {
                return Err(errors);
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
                    surface_shadows: std::collections::HashSet::new(),
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
        //
        // Not the runtime module (RFC-0125 §2.4): a `panic` there is one of
        // `trap.rs`'s fixed wordings — `malloc`'s `out of memory` — and every
        // engine prints that line without a site, byte for byte.
        let site = site_file(key, root_key, opts.std_root.as_deref());
        if site != format!("{RUNTIME_SPEC}.vyrn") {
            stamp_panic_sites(&mut program, &site);
        }

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
                if let Some(mut d) = audience_objection(key, &target, imp.line, opts)
                    .or_else(|| runtime_fence(key, &target, imp.line, opts))
                {
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
                // RFC-0072 M1: a generator import is an IMPORT, and the same rule
                // decides it. The generated module's audience is its input file's
                // when the input declares one and the mounting root's when it does
                // not (M5), so this edge is the one a `.vyx` widens by living under
                // `server/` and being mounted from the client root — the SSR half of
                // a universal page inherits its caller and cannot widen against it.
                if let Some(mut d) = audience_objection(key, &gen_key, *line, opts) {
                    if !is_root {
                        d.file = Some(key.to_string());
                    }
                    return Err(vec![d]);
                }
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
        let wanted = rt.always
            || rt
                .desugared
                .iter()
                .chain(rt.routes.iter().map(|(b, _)| b))
                .any(|b| mentioned.contains(*b));
        // A missing std root is not an error HERE: the diagnostic for a program
        // that needs the runtime and cannot find it belongs to whoever needs it,
        // not to a scan. Each engine refuses loudly at the call instead.
        let Ok(target) = resolve_spec(rt.spec, &root_key, opts) else {
            continue;
        };
        // `wanted` gates the FETCH (a builtin's desugar pulled this module in),
        // never the MARKING below: a module the program imports BY HAND is
        // linked all the same, and its declarations are program-global — the
        // reserved spellings apply to it whether it arrived by mention or by
        // import. Marking only mention-linked modules left a hand-imported
        // `std/json` with bare `JStr` beside a consumer's own `JStr`, two
        // enums one variant name apart.
        if !wanted && !states.contains_key(&target) {
            continue;
        }
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

    // RFC-0103 M2: the floor. Last, so the closure it walks is everything the
    // artifact links — the modules the source imports AND the runtime modules a
    // builtin's desugar just injected. It is a whole-artifact rule rather than a
    // per-edge one (audience's shape), because the question is what the program
    // NEEDS, and no single import edge knows that.
    if let Some(map) = &opts.artifacts {
        let graph = floor_graph(&mut modules);
        // RFC-0125 M6, fourth slice: a row a judgment answers cannot be decided
        // here. The judgment reads the named core, which is built from the
        // checker's types, and nothing in this load is checked yet. So the
        // decision is HELD and made after the check, in the one place the CLI
        // reaches with a checked program; every other row is refused where it
        // always was, in the order it always was.
        match crate::floor::objected(&graph, &root_key, map) {
            // A nested generator load (RFC-0021) is not the artifact; only the
            // outermost load may hold a decision for the check that follows it.
            Some(c) if crate::floor::is_judged(&c) && LOAD_DEPTH.with(|d| d.get()) == 1 => {
                crate::floor::defer(graph, root_key.clone(), map.clone(), origins.clone());
            }
            _ => {
                if let Some(mut d) = crate::floor::objection(&graph, &root_key, map) {
                    if d.file.as_deref() == Some(root_key.as_str()) {
                        d.file = None;
                    }
                    if !origins.is_empty() {
                        origins.remap(&mut d);
                    }
                    return Err((vec![d], origins));
                }
            }
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

/// How deep nested generator loads may go before the load is refused. Far past
/// any honest pipeline (a `.vyx` widget generating a `.vyx` generating …), and
/// low enough that the refusal is a diagnostic instead of a dead stack.
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
    let gen_key = format!("generated by {name}({arg_repr}){GEN_SEP}{importer}");
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
    // A path argument that names a manifest DEPENDENCY also admits the key the
    // import map resolves it to (RFC-0107 M2), so a generator can read a
    // lock-pinned collection file. The pair goes to the sandbox as the
    // import-map step `gen_scoped_path` has no way to perform, and the resolved
    // key joins `allowed` so the input-root rule keeps deciding — and so it joins
    // the cache key below, which is what makes RE-PINNING the dependency a miss.
    // Without that the entry's recorded input would be the OLD pinned key, which
    // still hashes as it did, and the stale output would be served.
    let mut allowed: Vec<String> = Vec::new();
    let mut aliased: Vec<(String, String)> = Vec::new();
    for c in &consts {
        if let crate::consteval::ConstVal::Str(s) = c {
            allowed.push(join_dir(s));
            if !s.ends_with(".vyrn") && !s.ends_with(".json") {
                allowed.push(join_dir(&format!("{s}.vyrn")));
            }
            // A declared alias whose target `resolve_spec` refuses (a manifest
            // that maps it to something that is not a specifier) contributes no
            // pair, so the read stays the path it was and fails as one. The
            // manifest's own fault is already loud where a module IMPORT of that
            // alias reports it, and inventing a second wording here would be a
            // second answer about one broken key.
            if opts.aliases.contains_key(s.as_str()) {
                if let Ok(key) = resolve_spec(s, importer, opts) {
                    allowed.push(key.clone());
                    aliased.push((s.clone(), key));
                }
            }
        }
    }
    let sources_hash = generator_cache_key(&gen_mod_key, name, &arg_repr, &allowed);
    let no_cache = std::env::var("VYRN_NO_GEN_CACHE").is_ok();

    // 5a. Cache hit: the entry is one this compiler wrote for THIS key, it
    //     records the generator's own module, and every recorded input still
    //     hashes as it did. An input recorded as ABSENT must still be absent — a
    //     file or directory that appeared since the run is a change like any
    //     other.
    if !no_cache {
        if let Some(cached) = resolver.gen_cache_get(&sources_hash) {
            if let Some((inputs, output)) = read_cache_entry(&sources_hash, &cached) {
                // `inputs` comes out of the entry, so on its own it can only
                // ever agree with itself: a list of length zero passes `all`
                // vacuously, and a list an attacker wrote passes it by
                // construction. The call site's own facts decide first — a
                // generation reads the generator's module, so an entry that does
                // not record `gen_mod_key` is not a record of this generation
                // whatever else it claims.
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
            aliased,
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
            .map(|(p, bytes)| {
                let h = match bytes {
                    Some(b) => crate::hash::sha256_hex(b),
                    None => ABSENT.to_string(),
                };
                (p.clone(), h)
            })
            .collect();
        // The generator's OWN transitive sources join the recorded inputs. That is
        // what lets the lookup key stay cheap: the entry now carries everything
        // needed to decide whether it is still valid, instead of the key having to
        // encode it (which meant discovering the closure, which meant parsing the
        // whole generator graph, on every keystroke). Hashed above, before the
        // run, because the engine needs the same hashes for its artifact key.
        inputs.extend(gen_sources);
        // Recorded unconditionally, because validation requires it: the reader
        // asks for the generator module the CALL SITE named, which is the one
        // fact about a hit that does not come out of the entry.
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
/// a directory listing (a `dir/` marker, `resolver.list`). `None` if it cannot
/// be read now; validation reads that as [`ABSENT`], which matches an input the
/// generator also found absent and mismatches every other recorded hash.
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

/// The recorded hash of an input that was NOT there when the generator looked.
/// Not a sha256, so it can never equal the hash of any content: absent↔present
/// always disagrees.
const ABSENT: &str = "absent";

/// The cache entry format tag. Bumped when the meaning of an entry changes,
/// because an entry written by an older compiler cannot be re-read under the new
/// meaning. `v1` recorded only the inputs that succeeded, so it could not tell
/// whether a missing file had since appeared; `v2` recorded absent ones too, and
/// was believed on sight; `v3` carries the tag below. [`read_cache_entry`]
/// rejects anything else, and rejecting is a miss: the generator re-runs and
/// overwrites the entry in place, so an older entry is ignored cleanly rather
/// than misread under the newer rules.
const CACHE_ENTRY_TAG: &str = "v3";

/// Formats this compiler used to write. An entry in one of them is stale, not
/// suspicious, so it is ignored without a word — which is the difference between
/// a format bump and a file nobody here wrote.
const SUPERSEDED_ENTRY_TAGS: &[&str] = &["v1", "v2"];

/// Serialize a cache entry: `v3 <tag> <N>`, then `path⇥hash` lines, then the
/// generated source verbatim. The tag authenticates everything after it,
/// [`entry_tag`] included the lookup key.
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
/// The generator cache holds compiler INPUT — its entries are linked into the
/// program as a synthesized module, and a hit never re-runs the generator, so a
/// file dropped in that directory is permanent. Every other cache in the design
/// is content-addressed against a hash from somewhere trusted: the blob cache
/// re-hashes each remote module against the sha256 in `vyrn.lock`, which lives
/// in the project and is reviewed like source. A generated module has no such
/// anchor — its content is whatever the generator produces from inputs that
/// change — so this cache authenticates its entries instead, with a per-user key
/// that lives OUTSIDE the cache directory ([`gen_cache_secret`]).
///
/// What that buys: an entry written by anything other than this user's compiler
/// is refused — a cache directory restored from a CI artifact, a shipped or
/// shared `~/.vyrn/cache/gen`, a `VYRN_GEN_CACHE_DIR` pointed at a tree someone
/// else filled, an entry moved between lookup keys. What it does not buy:
/// nothing stops a process already running as this user, which can read the key
/// like it can read the source it would rather edit. A cache cannot be made more
/// trustworthy than the account that owns it.
///
/// Two rejections, deliberately different. A format this compiler used to write
/// is an old entry, and a silent miss: the generator re-runs and replaces it,
/// which is what a format bump means. Anything else in that directory — a `v3`
/// entry that fails its tag, a file in no format at all — is something else
/// writing here, and it says so.
fn read_cache_entry(key: &str, text: &str) -> Option<(Vec<(String, String)>, String)> {
    let Some(first_nl) = text.find('\n') else {
        warn_foreign_entry(key);
        return None;
    };
    let header = &text[..first_nl];
    // Every format this compiler has written ends its header with the input
    // count, so the count is read before the format is judged. A generation
    // always reads at least the generator's own module, so an entry recording
    // NOTHING describes no generation — in any format, authentic or not. That
    // rule comes first because "the artifact's own list decides whether the
    // artifact is valid" is the defect, independent of any hashing: a list of
    // length zero satisfies `all` by saying nothing.
    let count = header.rsplit(' ').next().unwrap_or("");
    if count.parse::<usize>() == Ok(0) {
        warn_foreign_entry(key);
        return None;
    }
    let Some(rest) = header
        .strip_prefix(CACHE_ENTRY_TAG)
        .and_then(|r| r.strip_prefix(' '))
    else {
        // `v1`/`v2` are this compiler's own earlier formats.
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
    // No `with_capacity(n)`: `n` came off the first line of a file, and a
    // truncated write is enough to make it `usize::MAX`, which aborts on the
    // allocation before a single claimed line is read. Growing the vector as the
    // lines actually arrive costs nothing and cannot be told a size.
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
    /// same entry on every keystroke, and one line per keystroke is a log, not
    /// a warning.
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

/// Authenticate `body` under `key`: `H(secret ‖ H(secret ‖ key ‖ body))`.
///
/// The key is inside the tag, so an artifact cannot be moved to another lookup
/// key — a valid generation of one module is not a valid generation of a
/// different one. Nested rather than prefixed because SHA-256 extends, and the
/// outer hash is over a digest of fixed length.
///
/// Public because the generator cache directory holds a SECOND kind of file: the
/// cranelift artifact `vyrn-genwasm` writes beside the entries, which it maps in
/// as native code. That file needs the same answer to the same question, and one
/// secret answers both.
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
/// It sits beside `~/.vyrn/cache`, not inside it, and `VYRN_GEN_CACHE_DIR` does
/// not move it: an archived, shared or redirected cache directory therefore
/// carries entries but never the key that would make them believable.
///
/// When there is no home directory to read, or the file cannot be created, the
/// process keeps a key of its own: entries it writes are then unreadable by the
/// next process, which is a cache that misses rather than a cache that is
/// believed.
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
            // `create_new` so two compilers starting together agree on one key
            // instead of overwriting each other's.
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

/// The key file's bytes, if it holds a whole one. A short read is a file caught
/// mid-creation by another process; this run keeps its own key and misses.
fn read_secret(path: &std::path::Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    (bytes.len() >= 32).then_some(bytes)
}

/// A fresh key. `RandomState` is seeded from the operating system's randomness
/// once per thread — the same source `HashMap` relies on not to be predictable —
/// and the process id and clock join it so that two keys minted in one process
/// still differ.
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
    // module key -> (RESOLVED enum type spelling -> its variants' renames).
    // Nested per ENUM so pass 2 can gate the extension on the importer
    // actually importing that enum, instead of spraying every variant
    // spelling over every hand importer of any decl of the module.
    let mut injected_variants: HashMap<String, HashMap<String, HashMap<String, String>>> =
        HashMap::new();
    for (key, prefix) in &injected {
        let m = modules
            .iter()
            .find(|m| &m.key == key)
            .expect("injected module");
        let by_enum = injected_variants.entry(key.clone()).or_default();
        let mut names: Vec<String> = Vec::new();
        for t in &m.program.type_decls {
            if t.line == 0 {
                continue; // parser-injected builtins are in every module
            }
            names.push(t.name.clone());
            if let Type::Enum(vs) = &t.base {
                let vars = by_enum.entry(format!("{prefix}{}", t.name)).or_default();
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

    // Protocol-method surface names across every linked module. Method calls
    // dispatch to impls BEFORE free functions, so when one of these names is
    // also an aliased import's original, an argument-bearing call can never
    // reach the imported decl — it is method sugar (`widget.render()`), not a
    // forbidden direct use of the original.
    let method_surface: HashSet<String> = modules
        .iter()
        .flat_map(|m| m.program.protocols.iter())
        .flat_map(|p| p.methods.iter().map(|sig| sig.name.clone()))
        .collect();

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
        let (refs, ambiguous_only) = program_ref_kinds(&m.program, true);
        for imp in &m.program.imports {
            for n in &imp.names {
                if let Some(_alias) = &n.alias {
                    let orig = &n.original;
                    if !mine.contains(orig)
                        && !bare_imported.contains(orig.as_str())
                        && refs.contains(orig)
                        // Method-sugar ambiguity (RFC-0022 vs. dispatch):
                        // when the original's name is a protocol-method
                        // surface name and the ONLY evidence is an
                        // argument-bearing call, the use may be
                        // `widget.render()` — which dispatches to impls
                        // before any free function and can never reach the
                        // imported decl. Rejecting it billed a legal method
                        // call as a forbidden direct use.
                        && !(ambiguous_only.contains(orig) && method_surface.contains(orig))
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
            // An `extern fn` is a host-ABI contract, not a namespace member:
            // the backends emit the import under the SOURCE spelling and the
            // JS host supplies it by that exact name (`extern:
            // { vyrnRpcCall: .. }`). Renaming it severs the contract, so an
            // extern is never a privacy-rename candidate even when several
            // modules restate the same one (std/rpc's client stubs do).
            if !f.is_extern && !exported.contains(&f.name) {
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
            // The ROOT's `main` is the program's entry point, and every engine
            // reaches it by that spelling. It is not exported — nothing can
            // import it — so the rule above treated it as a private name like
            // any other and minted a fresh symbol for it the moment a SECOND
            // module declared one. Every `examples/` file is a program, so
            // importing one from a program left the whole build with no entry
            // at all: `call to unknown function \`main\``, naming no file and no
            // line. A non-root `main` is unreachable code and still renames,
            // which is what clears the collision.
            if m.key == root_key && name == "main" {
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
            // them. Gated on importing THE ENUM ITSELF (its resolved spelling):
            // extending every importer of any decl blanket-rewrote a consumer's
            // own same-spelled variant (`JStr`) into `json$JStr` — corruption of
            // a perfectly legal private enum. A module that does NOT import the
            // enum keeps whatever `JStr` means to it.
            if !imp.names.is_empty() {
                if let Some(by_enum) = injected_variants.get(target) {
                    for n in &imp.names {
                        let resolved = foreign_renames
                            .get(&(target.clone(), n.original.clone()))
                            .cloned()
                            .unwrap_or_else(|| n.original.clone());
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
            // This module's own variants guard the rewrite (see
            // [`RW_VARIANTS`]): an alias local or injected spelling that
            // collides with one must not fold the constructor sites.
            let variants = own_variant_names(&m.program);
            rewrite_module_refs(&mut m.program, map, &ns_names, &variants);
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
            // `ns.member` uses inside a place projection resolve like any
            // other reference (RFC-0091 M2: a projection is an ordinary body
            // the loader just never flattened).
            for pl in &mut im.places {
                let mut locals: HashSet<String> =
                    pl.params.iter().map(|pm| pm.name.clone()).collect();
                self.walk_type_positions_fn(pl, &locals.clone());
                self.walk_block(&mut pl.body, &mut locals);
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
            Expr::Spawn { args, .. } => {
                for a in args.iter_mut() {
                    self.walk_expr(a, locals);
                }
            }
            Expr::TryConstruct { name, args, line } => {
                // `ns.Type?(..)` — the parser folds the qualifier into the name,
                // exactly as it does for a struct literal's `ns.Type { .. }`.
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
                        Pattern::None | Pattern::Other => {}
                    }
                    match &mut arm.body {
                        ArmBody::Expr(e) => self.walk_expr(e, &mut inner),
                        ArmBody::Block(b) => self.walk_block(b, &mut inner),
                    }
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
    // enum variant name -> EVERY enum declaring it, as (type, module); protocol
    // method name -> EVERY protocol declaring it, same shape. Lists, not single
    // owners: two linked modules may each declare a `render` method or a `None`
    // variant, and last-writer-wins made the later-loaded module own the name —
    // a legitimate call resolving to the EARLIER module's declaration was then
    // rejected as "not imported here", purely because of import order.
    let mut variant_enum: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut method_protocol: HashMap<String, Vec<(String, String)>> = HashMap::new();

    // Flat-namespace collisions, as `(name, first owner, second owner)`. They are
    // COLLECTED rather than reported: one pair of modules sharing five names is
    // one problem with five symptoms, and the decl line they carry belongs to a
    // module the user may never have opened. `clash_diagnostics` turns the whole
    // batch into one diagnostic per module pair, at an import site in a real file.
    let mut clashes: Vec<(String, String, String)> = Vec::new();

    // Names EVERY declaration of which is a non-exported `extern fn`, across
    // two or more modules. Those duplicates are one host-ABI contract restated
    // per module — std/rpc's generators plant `extern fn vyrnRpcCall` in every
    // client stub — not a flat-namespace collision: renaming them away would
    // sever the ABI, so instead they neither clash nor take part in the
    // foreign-reference check, and the merge keeps a single copy.
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
                    variant_enum
                        .entry(v.name.clone())
                        .or_default()
                        .push((t.name.clone(), m.key.clone()));
                }
            }
        }
        for f in &m.program.functions {
            // Impl-flattened methods carry mangled names (`P__Key__m`) that
            // cannot collide with user identifiers; register them anyway so
            // duplicate impls across modules collide loudly here.
            if shared_externs.contains(&f.name) {
                continue;
            }
            register(&f.name, &m.key, f.exported, &mut clashes);
        }
        for p in &m.program.protocols {
            register(&p.name, &m.key, p.exported, &mut clashes);
            for sig in &p.methods {
                method_protocol
                    .entry(sig.name.clone())
                    .or_default()
                    .push((p.name.clone(), m.key.clone()));
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
    // RFC-0054's shadowing fact, gathered as the loop goes and handed to the
    // checker below. See `ast::Program::surface_shadows`.
    let mut surface_shadows: HashSet<(Option<String>, String)> = HashSet::new();
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
        // Modules this file imports ANYTHING from count as present for the
        // candidate maps: importing a module's type brings its protocol's
        // method surface into the checker's reach, and the checker — which
        // resolves by receiver type — picks between same-named candidates.
        let imported_modules: HashSet<&str> = visible
            .iter()
            .filter_map(|d| owner.get(d.as_str()).map(|(md, _)| md.as_str()))
            .collect();
        // This module's own `extern fn` declarations (RFC-0012). A shared one
        // (`shared_externs`) never entered the flat namespace, so without this
        // set a stub calling its own `vyrnRpcCall` would read as an un-imported
        // foreign reference. An extern declared here is a local host call by
        // definition.
        let my_externs: HashSet<&str> = m
            .program
            .functions
            .iter()
            .filter(|f| f.is_extern && !f.exported)
            .map(|f| f.name.as_str())
            .collect();
        // RFC-0054, recorded for the checker. A module shadows a surface builtin
        // when it can SEE a declaration of that name — its own, or one it
        // imported — and only this loop knows both halves: `imports` are consumed
        // here and never reach the checker.
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
            // Resolve constructors/methods to their OWNING declarations. Either
            // map can hold several candidates — same-named protocol methods or
            // enum variants in different linked modules are ordinary until a
            // use must pick one — so every candidate is tried before the use is
            // called foreign.
            let mut candidates: Vec<&(String, String)> = Vec::new();
            // A private `extern fn` of THIS module resolves to its own copy,
            // whatever the flat namespace decided about its name.
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
            // RFC-0054's four surface builtins. A module that has NOT declared
            // or imported one MEANS THE BUILTIN, and what some other module of
            // the program called its functions is none of this module's
            // business. Without this, a `fn raw` anywhere made every other
            // module's `raw(..)` read as an un-imported foreign reference —
            // including `std/vyx`'s, which is where it was found.
            if crate::ast::is_surface_builtin(name) && !in_scope(name) {
                return;
            }
            if candidates.is_empty() {
                // A plain reference to a top-level decl.
                if in_scope(name) {
                    return;
                }
                if let Some((def_module, _)) = owner.get(name) {
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
                return;
            }
            // Any candidate this module can already see resolves the use: the
            // type-directed checker picks between same-named declarations, and
            // the loader's flat maps cannot.
            if candidates.iter().any(|(decl, def_module)| {
                in_scope(decl)
                    || def_module == &m.key
                    || gen_importer.as_deref() == Some(def_module.as_str())
                    || imported_modules.contains(def_module.as_str())
            }) {
                return;
            }
            // No candidate is imported. One keeps the singular wording; several
            // are all listed, because "it lives in X" would be a guess about
            // which one this call means.
            let mut modules: Vec<&str> = candidates.iter().map(|(_, md)| md.as_str()).collect();
            modules.sort_unstable();
            modules.dedup();
            let list = modules.join("`, `");
            errors.push(with_file(
                Diagnostic::error(
                    line,
                    0,
                    "load",
                    format!(
                        "{what} `{name}` is defined in `{list}` but not imported here — add \
                         it to an `import {{ .. }} from` list"
                    ),
                ),
                m,
                root_key,
            ));
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
            // A projection's body is checked like any function's: a foreign
            // reference inside it must be imported too.
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
    // RFC-0054: which modules can see a declaration of a surface builtin. The
    // checker needs this and cannot compute it — `imports` are consumed here.
    program.surface_shadows = surface_shadows;
    // One host-ABI declaration per shared-extern name (`shared_externs`): the
    // modules carried identical copies, and emitting the wasm import twice
    // would be pure waste. The root's copy (already in `program.functions`)
    // or the first import order wins; they are interchangeable by definition.
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

/// Scope-aware reference scan for the link-time visibility check: every name a
/// function references that could name a program-level declaration, MINUS any
/// name bound by a local in scope — params, `let`, `for`/lambda variables, and
/// match binds. A local shadows a like-named foreign export, so at that use site
/// it is never a cross-module reference: the flat namespace binds locals before
/// imports (RFC-0027, one level below imports). Type-position names (`let x: T`
/// annotations, and the caller's param/return/bound types) are always kept — a
/// value local never shadows a type. This seeds the scope with the function's
/// params, so a param that shadows a foreign export is
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
        Stmt::Expr(e) => scope_expr(e, e.line(), locals, out),
        Stmt::Region { body, .. } => {
            let mut inner = locals.clone();
            scope_block(body, &mut inner, out);
        }
    }
}

fn scope_expr(e: &Expr, line: usize, locals: &HashSet<String>, out: &mut Vec<(String, usize)>) {
    match e {
        Expr::Call { name, args, line } | Expr::Spawn { name, args, line } => {
            // Method sugar `ns.f(x)` parses as callee `f` with the namespace as
            // its first argument. When that receiver names one of the module's
            // namespaces, the use is QUALIFIED — another module's member — and
            // is recorded under its dotted spelling, never as a bare `f` that
            // would read as this module's flat name. A flat call with a like-
            // shaped argument keeps the bare spelling.
            let mut sugar = false;
            if let Some(Expr::Var { name: recv, .. }) = args.first() {
                sugar = !locals.contains(recv) && SCOPE_NS.with(|s| s.borrow().contains(recv));
            }
            if sugar {
                if let Some(Expr::Var { name: recv, .. }) = args.first() {
                    out.push((format!("{recv}.{name}"), *line));
                }
            } else if !locals.contains(name) {
                out.push((name.clone(), *line));
                // `f(x)` is also exactly what method sugar `x.f()` arrives
                // as. When the caller asked for it (`program_ref_kinds`),
                // count this occurrence so a name seen ONLY here can be told
                // apart from one that also appears as a variable, a type, or
                // a zero-argument call — those cannot be method dispatch.
                if !args.is_empty() {
                    SCOPE_AMB.with(|a| {
                        if let Some(amb) = a.borrow_mut().as_mut() {
                            *amb.entry(name.clone()).or_default() += 1;
                        }
                    });
                }
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
                    Pattern::None | Pattern::Other => {}
                }
                match &arm.body {
                    ArmBody::Expr(e) => scope_expr(e, *line, &inner, out),
                    ArmBody::Block(b) => {
                        let mut binner = inner.clone();
                        scope_block(b, &mut binner, out);
                    }
                }
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

thread_local! {
    /// The enum variant names the module a [`rewrite_module_refs`] call is
    /// currently walking declares itself. A call spelled `V(x)` or a `match`
    /// pattern `V(..)` whose `V` is one of these CONSTRUCTS the module's own
    /// enum — it is not a reference to a same-spelled declaration, and
    /// renaming it there corrupted variant constructions inside the renamed
    /// module (a global/protocol may share a variant's spelling; a fn or type
    /// cannot). Carried in a thread-local so the recursive rewrite family
    /// keeps its signatures; set by [`rewrite_module_refs`], read by
    /// [`rewrite_expr`].
    static RW_VARIANTS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Every enum variant name `p` declares itself.
fn own_variant_names(p: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    for t in &p.type_decls {
        if let Type::Enum(vs) = &t.base {
            out.extend(vs.iter().map(|v| v.name.clone()));
        }
    }
    out
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
    rewrite_module_refs(p, map, &HashSet::new(), &HashSet::new());
}

fn rewrite_expr(
    e: &mut Expr,
    map: &HashMap<String, String>,
    ns: &HashSet<String>,
    locals: &HashSet<String>,
) {
    match e {
        Expr::Call { name, args, .. } => {
            let ns_receiver =
                matches!(args.first(), Some(Expr::Var { name: h, .. }) if ns.contains(h));
            let shadowed = locals.contains(name);
            // A constructor of THIS module's own enum (see [`RW_VARIANTS`])
            // is reached by this spelling, not by any declaration's name.
            let ctor = RW_VARIANTS.with(|v| v.borrow().contains(name.as_str()));
            if !ns_receiver && !shadowed && !ctor {
                *name = ren(map, name);
            }
            for a in args {
                rewrite_expr(a, map, ns, locals);
            }
        }
        Expr::Spawn { name, args, .. } | Expr::TryConstruct { name, args, .. } => {
            if !locals.contains(name) {
                *name = ren(map, name);
            }
            for a in args {
                rewrite_expr(a, map, ns, locals);
            }
        }
        Expr::StructLit { name, fields, .. } => {
            if !locals.contains(name) {
                *name = ren(map, name);
            }
            for (_, v) in fields {
                rewrite_expr(v, map, ns, locals);
            }
        }
        Expr::Var { name, .. } => {
            if !locals.contains(name) {
                *name = ren(map, name);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
            rewrite_expr(expr, map, ns, locals)
        }
        Expr::Consume { place, .. } => rewrite_expr(place, map, ns, locals),
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_expr(lhs, map, ns, locals);
            rewrite_expr(rhs, map, ns, locals);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            rewrite_expr(scrutinee, map, ns, locals);
            for arm in arms {
                let mut inner = locals.clone();
                if let Pattern::Variant(v, binds) = &mut arm.pattern {
                    // A `match` arm always constructs — never a declaration
                    // reference (see [`RW_VARIANTS`]).
                    let ctor = RW_VARIANTS.with(|w| w.borrow().contains(v.as_str()));
                    if !ctor {
                        *v = ren(map, v);
                    }
                    for b in binds {
                        inner.insert(b.clone());
                    }
                }
                match &mut arm.body {
                    ArmBody::Expr(e) => rewrite_expr(e, map, ns, &inner),
                    ArmBody::Block(b) => {
                        let mut binner = inner.clone();
                        rewrite_block(b, map, ns, &mut binner);
                    }
                }
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            rewrite_expr(cond, map, ns, locals);
            rewrite_expr(then_branch, map, ns, locals);
            if let Some(eb) = else_branch {
                rewrite_expr(eb, map, ns, locals);
            }
        }
        Expr::ArrayLit { elems, .. } => {
            for e2 in elems {
                rewrite_expr(e2, map, ns, locals);
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                rewrite_expr(k, map, ns, locals);
                rewrite_expr(v, map, ns, locals);
            }
        }
        // A lambda body (RFC-0023): rewrite referenced names inside it. Its
        // params are new locals — they shadow a renamed decl exactly like a
        // `let` does, so they join the scope rather than trusting that no map
        // key ever spells them.
        Expr::Lambda { params, body, .. } => {
            let mut inner = locals.clone();
            for p in params {
                inner.insert(p.clone());
            }
            match body {
                LambdaBody::Expr(e2) => rewrite_expr(e2, map, ns, &inner),
                LambdaBody::Block(b2) => rewrite_block(b2, map, ns, &mut inner),
            }
        }
        Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
    }
}

fn rewrite_block(
    b: &mut Block,
    map: &HashMap<String, String>,
    ns: &HashSet<String>,
    locals: &mut HashSet<String>,
) {
    for s in &mut b.stmts {
        rewrite_stmt(s, map, ns, locals);
    }
}

fn rewrite_stmt(
    s: &mut Stmt,
    map: &HashMap<String, String>,
    ns: &HashSet<String>,
    locals: &mut HashSet<String>,
) {
    match s {
        Stmt::Let {
            name, value, ty, ..
        } => {
            if let Some(t) = ty {
                rewrite_type(t, map);
            }
            rewrite_expr(value, map, ns, locals);
            // In scope for everything after it — a local shadows a renamed
            // decl exactly as it shadows the original.
            locals.insert(name.clone());
        }
        // The assignment TARGET is a reference too, not a declaration: module
        // state (RFC-0029) is a top-level decl, so a rename must reach `g = v`
        // exactly as it reaches the `g` reads (`Expr::Var` below). Missing these
        // left the write side naming a decl that no longer exists ("assignment
        // to unknown variable `filter`" once std/arrays' `filter` forced the
        // name-privacy rename of a same-named global). A LOCAL of the same name
        // is not that decl, though: `let flag = ..; flag = x` writes the local,
        // and rewriting the target rebound the write to the global.
        Stmt::Assign { name, value, .. } | Stmt::SetField { name, value, .. } => {
            if !locals.contains(name) {
                *name = ren(map, name);
            }
            rewrite_expr(value, map, ns, locals);
        }
        Stmt::IndexSet {
            name, index, value, ..
        } => {
            if !locals.contains(name) {
                *name = ren(map, name);
            }
            rewrite_expr(index, map, ns, locals);
            rewrite_expr(value, map, ns, locals);
        }
        Stmt::Return { value: Some(e), .. } => rewrite_expr(e, map, ns, locals),
        Stmt::Return { value: None, .. } => {}
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            rewrite_expr(cond, map, ns, locals);
            let mut inner = locals.clone();
            rewrite_block(then_block, map, ns, &mut inner);
            if let Some(eb) = else_block {
                let mut inner2 = locals.clone();
                rewrite_block(eb, map, ns, &mut inner2);
            }
        }
        Stmt::IfLet {
            pattern,
            scrutinee,
            then_block,
            else_block,
            ..
        } => {
            rewrite_expr(scrutinee, map, ns, locals);
            // The variant NAME follows the same rename a `match` arm's does —
            // this walk missed it until RFC-0121 made `if let` over an
            // imported enum's variants a written shape (the corpus never had
            // one; `match` always renamed).
            if let Pattern::Variant(v, _) = &mut *pattern {
                let ctor = RW_VARIANTS.with(|w| w.borrow().contains(v.as_str()));
                if !ctor {
                    *v = ren(map, v);
                }
            }
            let mut inner = locals.clone();
            for b in crate::movecheck::pattern_bindings(pattern) {
                inner.insert(b.to_string());
            }
            rewrite_block(then_block, map, ns, &mut inner);
            if let Some(eb) = else_block {
                let mut inner2 = locals.clone();
                rewrite_block(eb, map, ns, &mut inner2);
            }
        }
        Stmt::While { cond, body, .. } => {
            rewrite_expr(cond, map, ns, locals);
            let mut inner = locals.clone();
            rewrite_block(body, map, ns, &mut inner);
        }
        Stmt::ForIn {
            var, iter, body, ..
        } => {
            rewrite_expr(iter, map, ns, locals);
            let mut inner = locals.clone();
            inner.insert(var.clone());
            rewrite_block(body, map, ns, &mut inner);
        }
        // `drop g` names a binding the same way — same rule as the target above.
        Stmt::Drop { name, .. } => {
            if !locals.contains(name) {
                *name = ren(map, name);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Expr(e) => rewrite_expr(e, map, ns, locals),
        Stmt::Region { body, .. } => {
            let mut inner = locals.clone();
            rewrite_block(body, map, ns, &mut inner);
        }
    }
}

/// Rewrite one function's signature types and body references through `map`.
///
/// The body is walked scope-aware: the params seed the local set, so a param or
/// a `let` that shadows a renamed decl keeps naming the local.
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
    let mut locals: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
    rewrite_block(&mut f.body, map, ns, &mut locals);
}

/// Rewrite every *reference* (types, calls, variables, bounds) in one module's
/// program through `map`. Declaration names are left alone — a separate step
/// renames a decl when a foreign name must be freed for a co-named local stub.
/// `ns` is the module's namespace-binding names (see [`rewrite_expr`]).
fn rewrite_module_refs(
    p: &mut Program,
    map: &HashMap<String, String>,
    ns: &HashSet<String>,
    variants: &HashSet<String>,
) {
    if map.is_empty() {
        return;
    }
    // The variant guard rides a thread-local so the recursive rewrite family
    // keeps its signatures (see [`RW_VARIANTS`]).
    RW_VARIANTS.with(|v| *v.borrow_mut() = variants.clone());
    for f in &mut p.functions {
        rewrite_function(f, map, ns);
    }
    for im in &mut p.impls {
        im.protocol = ren(map, &im.protocol);
        rewrite_type(&mut im.ty, map);
        for m in &mut im.methods {
            rewrite_function(m, map, ns);
        }
        // A `place` projection is never flattened into `Program::functions`
        // (RFC-0091 M2), so without this walk a rename never reached its
        // body: `place nth(..) { yield self.vals[clamp(i)] }` kept calling
        // `clamp` after clamp was renamed, and an alias folding skipped the
        // projection's prologue entirely.
        for pl in &mut im.places {
            rewrite_function(pl, map, ns);
        }
    }
    for t in &mut p.type_decls {
        rewrite_type(&mut t.base, map);
        if let Some(pred) = &mut t.predicate {
            // A refinement predicate has no locals of its own.
            rewrite_expr(pred, map, ns, &HashSet::new());
        }
    }
    for g in &mut p.globals {
        if let Some(t) = &mut g.ty {
            rewrite_type(t, map);
        }
        // A global initializer runs at module-state init: no locals in scope.
        rewrite_expr(&mut g.init, map, ns, &HashSet::new());
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
                        rewrite_expr(d, map, ns, &HashSet::new());
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
                        rewrite_expr(d, map, ns, &HashSet::new());
                    }
                }
            }
        }
    }
    for t in &mut p.tests {
        rewrite_block(&mut t.body, map, ns, &mut HashSet::new());
    }
    for b in &mut p.benches {
        rewrite_block(&mut b.body, map, ns, &mut HashSet::new());
    }
    RW_VARIANTS.with(|v| {
        v.borrow_mut().clear();
    });
}

// Every reference name (types and expression callees/variables/variants) used
// anywhere in a module's declarations — for the RFC-0022 check that an aliased
// import's original name is not also used directly.
//
// Bodies are scanned scope-aware (the same walk `fn_body_ref_names` uses): a
// name bound by a local — a param, a `let`, a loop or lambda variable — is not
// a reference to a like-named foreign decl, so it must not satisfy this check.
// Type positions are always kept: a value local never shadows a type.
// (`//` and not `///`: a doc comment on a `thread_local!` invocation documents
// nothing and rustc warns.)
thread_local! {
    /// The namespace bindings of the module `program_ref_names` is currently
    /// walking, so [`scope_expr`] can tell method sugar (`ns.f(x)`, recorded
    /// qualified) from a flat call of the same spelling (recorded bare).
    static SCOPE_NS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// While [`program_ref_kinds`] walks, every name recorded as an
    /// argument-bearing call callee lands here with its occurrence count. A
    /// call spelled `f(x)` may be protocol-method sugar (`x.f()` arrives as
    /// exactly that call), so such evidence alone cannot prove a flat foreign
    /// use of `f`. `None` while other walkers run.
    static SCOPE_AMB: RefCell<Option<HashMap<String, usize>>> = const { RefCell::new(None) };
}

fn program_ref_names(p: &Program) -> HashSet<String> {
    program_ref_kinds(p, false).0
}

/// [`program_ref_names`] plus the set of names whose EVERY occurrence was an
/// argument-bearing call callee. With `split_ambiguous` the walk also counts
/// occurrences per name, so a caller can tell "only ever a plausible method
/// dispatch" from "also a variable, a type, or a zero-argument call".
fn program_ref_kinds(p: &Program, split_ambiguous: bool) -> (HashSet<String>, HashSet<String>) {
    let mut out: HashSet<String> = HashSet::new();
    let mut totals: HashMap<String, usize> = HashMap::new();
    // The walker tells method sugar (`ns.f(x)`) from a flat `f(x)` by whether
    // the receiver names one of THIS module's namespaces, so the set travels
    // through the same thread-local the walker reads.
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
        let mut refs = Vec::new();
        scope_block(b, &mut locals, &mut refs);
        for (n, _) in refs {
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
        // A `place` projection is never flattened into `Program::functions`
        // (RFC-0091 M2), so without this walk its references hid from the
        // RFC-0022 check — and from every rename below.
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
    // Method sugar recorded the qualified spelling under its namespace; a
    // flat use of the same spelling stays bare. The walker knew which was
    // which through `SCOPE_NS`, set above and cleared here.
    SCOPE_NS.with(|s| s.borrow_mut().clear());
    let amb = SCOPE_AMB.with(|s| s.borrow_mut().take().unwrap_or_default());
    // Ambiguity-only names STAY in `out`: removing them here would silence
    // every consumer, including the RFC-0022 hidden-original check that must
    // still fire when the name is not a protocol-method surface spelling. The
    // set travels alongside so the one caller with the method-surface context
    // can apply the narrower skip itself.
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
    let own_variants = own_variant_names(p);
    // When the spelling being renamed IS one of this module's own enum
    // variants — an injected module's reserved-spelling pass renaming `JStr`
    // to `json$JStr` — the variant positions are the TARGETS of the rename
    // and must follow it. The guard exists for the other direction: a
    // non-variant decl (a private global, a protocol) sharing a variant's
    // spelling, whose rename must leave the constructions alone.
    let protected = if own_variants.contains(from) {
        HashSet::new()
    } else {
        own_variants
    };
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
    rewrite_module_refs(p, &map, ns, &protected);
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
        // RFC-0094 M2: a routed builtin with a FREE spelling needs no route — an
        // import does the same work and costs the compiler nothing. Eleven rows
        // left this table; `@charCount` is what remains, because a method-only
        // name has no spelling an import can bring into scope.
        for gone in crate::checker::MOVED_TO_STD {
            assert!(
                routed_builtin(gone.0).is_none(),
                "`{}` moved to `{}`; a route for it would shadow the import",
                gone.0,
                gone.1
            );
        }
        let routes: Vec<&str> = RT_MODULES
            .iter()
            .flat_map(|rt| rt.routes)
            .map(|(b, _)| *b)
            .collect();
        assert_eq!(routes, vec!["@charCount"]);
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

    /// RFC-0125 §3 M6 (the third judgment's fifth slice): [`STRING_FAULT`] is the
    /// same shape and needs the same guard — three engines emit a call to it, and
    /// a mismatch between the name and the module's prefix is a link error in
    /// every program that makes a `String` from bytes.
    #[test]
    fn the_string_check_is_std_texts() {
        let text = RT_MODULES
            .iter()
            .find(|rt| rt.spec == "std/text")
            .expect("std/text is linked");
        assert!(
            STRING_FAULT.starts_with(text.prefix),
            "`{STRING_FAULT}` vs `{}`",
            text.prefix
        );
        // The mention that links the module, and it is a desugar rather than a
        // route: only the check moved, the build stayed with each engine.
        assert!(text.desugared.contains(&"stringFromBytes"));
        assert!(routed_builtin("stringFromBytes").is_none());
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
        ("std/hash.vyrn", include_str!("../../../std/hash.vyrn")),
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
    fn a_program_that_imports_a_program_keeps_its_entry_point() {
        // Every file in `examples/` is a program, so importing one — the website
        // imports `examples/herofield.vyrn` to hash what it prints — put a
        // SECOND `main` in the link. `main` is not exported, so the name-privacy
        // rename above minted a fresh symbol for both of them, and the program
        // was left with no `main` at all: `call to unknown function \`main\``,
        // naming no file and no line. The root's entry keeps its spelling; the
        // imported one, which nothing can reach, is the one that renames.
        let lib = "export fn libValue() -> Int64 { return 5 } \
                   fn main() -> Int64 { return 99 }";
        let root = "import { libValue } from \"./lib\" \
                    fn main() -> Int64 { return libValue() }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 5);
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

    #[test]
    fn a_local_shadow_survives_a_name_privacy_rename() {
        // The rewrite of a module's OWN references after a name-privacy rename
        // (RFC-0046 §3) was scope-unaware: a local `let flag` kept its name while
        // every READ and WRITE of `flag` after it was rewritten to the renamed
        // GLOBAL — in plain statements and inside lambda bodies alike.
        let lib = "let mut flag = 9 \
                   export fn flip() -> Int64 { let flag = 1 return flag } \
                   export fn lam() -> Int64 { let g: fn(Int64) -> Int64 = flag -> flag + 1 return g(10) } \
                   export fn peek() -> Int64 { return flag }";
        let root = "import { flip, lam, peek } from \"./lib\" \
                    let mut flag = 7 \
                    fn main() -> Int64 { let f = flip() return f + lam() + peek() + flag }";
        // flip reads its LOCAL (1), lam's parameter is untouched by the global's
        // rename (11), peek reads lib's own state and main reads root's (9 + 7).
        assert_eq!(
            run_multi(root, &[("lib.vyrn", lib)]).unwrap(),
            1 + 11 + 9 + 7
        );
    }

    #[test]
    fn a_shadowed_use_of_an_aliased_original_is_not_reported() {
        // The hidden-original check (RFC-0022) scanned references unscoped: a
        // LOCAL named like an aliased import's original satisfied it and failed
        // the load though nothing referenced the hidden foreign name.
        let ui = "export fn render() -> Int64 { return 1 }";
        let root = "import { render as draw } from \"./ui\" \
                    fn helper() -> Int64 { let render = 2 return render } \
                    fn main() -> Int64 { return helper() }";
        assert_eq!(run_multi(root, &[("ui.vyrn", ui)]).unwrap(), 2);
    }

    // KNOWN LIMITATION (documented, not forgotten): with two same-named methods
    // on two linked protocols, the loader accepts and the checker picks by the
    // receiver's impl table, but the lowered symbol still mangles from a
    // last-writer protocol name — `Q__A__area` vs the registered `P__A__area`.
    // Closing it needs one protocol choice carried into symbol mangling.
    #[test]
    #[ignore = "same-named methods on two linked protocols: checker picks P, symbol mangling still says Q"]
    fn a_shared_method_name_resolves_to_the_imported_protocol() {
        // Two linked protocols may declare the same method name. Last-writer-wins
        // made the LATER-loaded module own `render`, so a call resolving to the
        // EARLIER (imported) protocol was rejected as unimported — purely because
        // of import order. The loader accepts when the receiver's own module is
        // imported; the checker picks between the candidates by impl table.
        let a = "export protocol P { fn area(self) -> Int64 } \
                 export type A = { v: Int64 } \
                 impl P for A { fn area(self) -> Int64 { return self.v } }";
        let b = "export protocol Q { fn area(self) -> Int64 } \
                 export type B = { v: Int64 } \
                 impl Q for B { fn area(self) -> Int64 { return self.v + 1 } }";
        let root = "import { A } from \"./a\" \
                    import { B } from \"./b\" \
                    fn main() -> Int64 { let x = A { v: 4 } return x.area() }";
        assert_eq!(run_multi(root, &[("a.vyrn", a), ("b.vyrn", b)]).unwrap(), 4);
    }

    #[test]
    fn an_unimported_shared_method_name_lists_every_candidate_module() {
        // The refusal keeps its shape when the receiver's own module is NOT
        // imported — reached through a third module that links the protocols
        // and provides the impl, so every candidate module is named instead of
        // guessing one.
        let a = "export protocol P { fn area(self) -> Int64 }";
        let b = "export protocol Q { fn area(self) -> Int64 }";
        let c = "import { P } from \"./a\" \
                 import { Q } from \"./b\" \
                 export type C = { v: Int64 } \
                 impl P for C { fn area(self) -> Int64 { return self.v } }";
        let root = "import { C } from \"./c\" \
                    fn main() -> Int64 { let x = C { v: 1 } return x.area() }";
        let e = load_err(root, &[("a.vyrn", a), ("b.vyrn", b), ("c.vyrn", c)]);
        assert!(e.contains("`area` is defined in"), "{e}");
        assert!(e.contains("a.vyrn") && e.contains("b.vyrn"), "{e}");
    }

    #[test]
    fn generated_importer_survives_an_at_in_the_path() {
        // The banner used to split on the last " at ", so an importer whose path
        // contained " at " was truncated mid-directory and everything derived
        // from it — relative imports, audience, panic sites — resolved wrong.
        assert_eq!(
            generated_importer("generated by mk(\"./w\")\u{1f}N:/work/at acme/app/main.vyrn"),
            Some("N:/work/at acme/app/main.vyrn")
        );
        // A nested banner unwraps to the real on-disk file.
        assert_eq!(
            generated_importer(
                "generated by components(\"./w\")\u{1f}generated by i18n(\"./s\")\u{1f}proj/site.vyrn"
            ),
            Some("proj/site.vyrn")
        );
        // Banners written before the separator existed still parse the old way —
        // including its blind spot, which only such legacy keys can reach.
        assert_eq!(
            generated_importer("generated by mk(\"./w\") at N:/work at acme/main.vyrn"),
            Some("acme/main.vyrn")
        );
        assert_eq!(generated_importer("main.vyrn"), None);
    }

    #[test]
    fn a_generator_chain_nesting_past_the_cap_is_a_diagnostic_not_an_abort() {
        // Each nested generator load gets a fresh module-state map, so a chain
        // that mints growing arguments never trips the cycle check and used to
        // recurse until the stack died. The nesting counter now refuses first.
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
    // ---- round-two regressions --------------------------------------------

    #[test]
    fn bare_expression_statement_diagnoses_at_its_own_line() {
        // A side-effect statement's references used to be seeded with line 0,
        // so an un-imported foreign call reported at the top of the module
        // instead of at the call site.
        let lib = "export fn other() -> Int64 { return 1 }";
        let root = "import { helper } from \"./lib\"\n\
                    fn main() -> Int64 {\n\
                        helper()\n\
                        other()\n\
                        return 0\n\
                    }";
        let ds = load(root, "main.vyrn", &opts(), &map(&[("lib.vyrn", lib)]))
            .expect_err("expected a load error");
        let hit = ds
            .iter()
            .find(|d| d.message.contains("`other`") && d.message.contains("not imported"));
        let d = hit.expect("the un-imported reference must be diagnosed");
        assert_eq!(d.line, 4, "diagnostic must sit on the call site: {d:?}");
    }

    #[test]
    fn shared_private_externs_keep_their_host_abi_spelling() {
        // Two stub modules restating the same private `extern fn` are one
        // host-ABI contract, not a collision: neither may be renamed (the
        // backends emit the import under the SOURCE spelling and the JS host
        // supplies it by that name), the merged program keeps a single copy,
        // and each stub calls its own without an import.
        let rpc_a = "extern fn vyrnRpcCall(x: Int64) -> Int64 \
                     export fn pingA(x: Int64) -> Int64 { return vyrnRpcCall(x) }";
        let rpc_b = "extern fn vyrnRpcCall(x: Int64) -> Int64 \
                     export fn pingB(x: Int64) -> Int64 { return vyrnRpcCall(x) }";
        let root = "import { pingA } from \"./rpc_a\" \
                    import { pingB } from \"./rpc_b\" \
                    fn main() -> Int64 { return 0 }";
        let program = load(
            root,
            "main.vyrn",
            &opts(),
            &map(&[("rpc_a.vyrn", rpc_a), ("rpc_b.vyrn", rpc_b)]),
        )
        .unwrap();
        let externs: Vec<&str> = program
            .functions
            .iter()
            .filter(|f| f.is_extern)
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(externs, vec!["vyrnRpcCall"], "one copy, source spelling");
    }

    #[test]
    fn aliased_import_does_not_reject_a_protocol_method_call() {
        // `widget.render()` arrives as the call `render(widget)` — exactly the
        // shape of a forbidden direct use of an aliased import's original.
        // When `render` is also a protocol-method surface name, that call
        // dispatches to impls BEFORE any free function and can never reach the
        // imported decl, so the hidden-original check must not fire.
        let ui = "export fn render(w: Int64) -> Int64 { return w }";
        let gfx = "export protocol P { fn render(self) -> Int64 } \
                   export type G = { v: Int64 } \
                   impl P for G { fn render(self) -> Int64 { return self.v } }";
        let root = "import { render as draw } from \"./ui\" \
                    import { G } from \"./gfx\" \
                    fn main() -> Int64 { let g: G = G { v: 3 } return g.render() }";
        let loaded = load(
            root,
            "main.vyrn",
            &opts(),
            &map(&[("ui.vyrn", ui), ("gfx.vyrn", gfx)]),
        );
        assert!(
            loaded.is_ok(),
            "a method-sugar call must not read as a direct use: {:?}",
            loaded
                .err()
                .map(|ds| ds.iter().map(|d| d.message.clone()).collect::<Vec<_>>())
        );
    }

    #[test]
    fn hand_import_of_a_non_enum_decl_keeps_own_variant_spellings() {
        // Importing ANY decl of an injected module used to spray ALL of its
        // variant renames over the importer — corrupting a legal private enum
        // whose variant happens to share a spelling (`JStr`). The renames ride
        // on importing THE ENUM itself; importing only `emit` leaves the
        // consumer's own `JStr` alone.
        let root = "import { emit } from \"std/json\" \
                    type T = | JStr(Int64) | JEnd \
                    fn main() -> Int64 { \
                        let t: T = JStr(41) \
                        return match t { JStr(n) => n, JEnd => 0 } \
                    }";
        assert_eq!(run_multi(root, RT_FILES).unwrap(), 41);
    }

    #[test]
    fn importing_the_injected_enum_still_folds_its_variants() {
        // The other half of the gate: an importer of the enum itself follows
        // the variant renames, so its own same-spelled variant is NOT created
        // and the folded constructor runs std/json's code.
        let root = "import { Json, emit } from \"std/json\" \
                    fn main() -> Int64 { \
                        let j: Json = JStr(\"hi\") \
                        return if emit(j).byteLength == 4 { 1 } else { 0 } \
                    }";
        assert_eq!(run_multi(root, RT_FILES).unwrap(), 1);
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
                Err(crate::trap::io_at("listerr", resolved))
            }
        }
        fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
            let prefix = format!("{}/", resolved.trim_end_matches('/'));
            let mut names: std::collections::BTreeSet<String> = Default::default();
            let mut any = false;
            for k in self.files.keys() {
                if let Some(rest) = k.strip_prefix(&prefix) {
                    any = true;
                    match rest.split_once('/') {
                        Some((seg, _)) if !seg.is_empty() => {
                            names.insert(format!("{seg}/"));
                        }
                        None if !rest.is_empty() => {
                            names.insert(rest.to_string());
                        }
                        _ => {}
                    }
                }
            }
            if any {
                Ok(names.into_iter().collect())
            } else {
                Err(crate::trap::io_at("listerr", resolved))
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

    /// A generator that COUNTS the entries of a listed directory: the site's
    /// `repo.vyrn` in miniature. `-1` when the directory is not there at all.
    const COUNTER: &str = "export gen fn count(dir: String) -> String { \
                               return match listDir(dir) { \
                                   Ok(names) => \"export fn n() -> Int64 { return \" \
                                       + names.length.toString() + \" }\", \
                                   Err(e) => \"export fn n() -> Int64 { return 0 - 1 }\", \
                               } }";
    const COUNTER_ROOT: &str = "import { count } from \"./gen\" \
                                import { n } from count(\"./data\") \
                                fn main() -> Int64 { return n() }";

    #[test]
    fn a_file_added_to_a_listed_directory_re_runs_the_generator() {
        let mut r = CachingResolver::new(&[("gen.vyrn", COUNTER), ("data/a.txt", "1")]);
        let before = gen_run_count();
        assert_eq!(run_with(COUNTER_ROOT, &r).unwrap(), 1);
        assert_eq!(gen_run_count(), before + 1, "cold: one run");

        r.files.insert("data/b.txt".to_string(), "2".to_string());
        assert_eq!(
            run_with(COUNTER_ROOT, &r).unwrap(),
            2,
            "counts the new file"
        );
        assert_eq!(gen_run_count(), before + 2, "listing changed: re-run");
    }

    #[test]
    fn a_file_removed_from_a_listed_directory_re_runs_the_generator() {
        let mut r = CachingResolver::new(&[
            ("gen.vyrn", COUNTER),
            ("data/a.txt", "1"),
            ("data/b.txt", "2"),
        ]);
        let before = gen_run_count();
        assert_eq!(run_with(COUNTER_ROOT, &r).unwrap(), 2);
        assert_eq!(gen_run_count(), before + 1, "cold: one run");

        r.files.remove("data/b.txt");
        assert_eq!(
            run_with(COUNTER_ROOT, &r).unwrap(),
            1,
            "counts what is left"
        );
        assert_eq!(gen_run_count(), before + 2, "listing changed: re-run");
    }

    #[test]
    fn a_directory_that_appears_re_runs_the_generator() {
        // The site bug: the first build found no `examples/`, published "0
        // examples", and kept publishing it. A listing that FAILED is an input
        // too — the directory being absent is what the generator saw.
        let mut r = CachingResolver::new(&[("gen.vyrn", COUNTER)]);
        let before = gen_run_count();
        assert_eq!(run_with(COUNTER_ROOT, &r).unwrap(), -1, "no directory yet");
        assert_eq!(gen_run_count(), before + 1, "cold: one run");

        r.files.insert("data/a.txt".to_string(), "1".to_string());
        assert_eq!(
            run_with(COUNTER_ROOT, &r).unwrap(),
            1,
            "the directory appeared: the cached `-1` is stale"
        );
        assert_eq!(gen_run_count(), before + 2, "directory appeared: re-run");
    }

    #[test]
    fn an_unrelated_file_does_not_invalidate_a_listing() {
        // The over-invalidation direction. An entry records what the generation
        // actually read — this listing and nothing else — so a file elsewhere in
        // the tree leaves the cache hit intact. A cache that never hits would
        // undo RFC-0076's keystroke budget.
        let mut r = CachingResolver::new(&[("gen.vyrn", COUNTER), ("data/a.txt", "1")]);
        let before = gen_run_count();
        assert_eq!(run_with(COUNTER_ROOT, &r).unwrap(), 1);
        assert_eq!(gen_run_count(), before + 1, "cold: one run");

        r.files
            .insert("elsewhere/z.txt".to_string(), "irrelevant".to_string());
        r.files
            .insert("data.txt".to_string(), "a near miss".to_string());
        assert_eq!(run_with(COUNTER_ROOT, &r).unwrap(), 1);
        assert_eq!(
            gen_run_count(),
            before + 1,
            "unrelated files: the cache must still hit"
        );
    }

    #[test]
    fn a_cache_entry_from_an_older_format_misses_instead_of_being_misread() {
        // A `v1` entry recorded only the inputs that succeeded, so it cannot say
        // whether a missing file has since appeared; a `v2` entry was believed on
        // sight. Reject both: re-run, and the run overwrites the entry in place.
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

    /// A cache entry decides what the compiler LINKS, and a hit never re-runs the
    /// generator, so an entry is only as trustworthy as whoever could have
    /// written it. Every one of these was written by something that is not this
    /// compiler, and none of them may be read back.
    #[test]
    fn a_cache_entry_this_compiler_did_not_write_is_refused() {
        let key = "k";
        let inputs = [("data/a.txt".to_string(), "deadbeef".to_string())];
        let output = "export fn n() -> Int64 { return 1 }";
        let honest = render_cache_entry(key, &inputs, output);
        assert!(read_cache_entry(key, &honest).is_some(), "the control");

        // The reproduction: an entry declaring ZERO inputs, whose recorded list
        // therefore satisfies `all` by saying nothing.
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

        // A valid entry moved to a different lookup key: a real generation of one
        // module is not a generation of another.
        assert!(
            read_cache_entry("other-key", &honest).is_none(),
            "the tag covers the lookup key"
        );

        // A file in no format at all.
        for junk in ["", "\n", "v3\n", "v3 x y\n", "not an entry at all\n"] {
            assert!(read_cache_entry(key, junk).is_none(), "junk: {junk:?}");
        }
    }

    /// The input count used to size a `Vec` before a single claimed line was
    /// read, so a count off the first line of a file aborted the process on the
    /// allocation. A truncated write is enough to produce one.
    #[test]
    fn an_impossible_input_count_is_a_miss_not_an_abort() {
        let key = "k";
        let body = format!("{}\ndata/a.txt\tdeadbeef\nout", u64::MAX);
        let entry = format!("{CACHE_ENTRY_TAG} {} {body}", entry_tag(key, &body));
        assert!(read_cache_entry(key, &entry).is_none());
    }

    /// End to end through the loader: a poisoned entry sitting at the right key
    /// does not reach the program, and the generator runs instead.
    #[test]
    fn a_poisoned_entry_does_not_reach_the_program() {
        let r = CachingResolver::new(&[("gen.vyrn", COUNTER), ("data/a.txt", "1")]);
        let before = gen_run_count();
        assert_eq!(run_with(COUNTER_ROOT, &r).unwrap(), 1);
        assert_eq!(gen_run_count(), before + 1, "cold: one run");
        let keys: Vec<String> = r.cache.borrow().keys().cloned().collect();
        assert_eq!(keys.len(), 1, "one generation, one entry");

        // What an attacker with write access to the cache directory writes.
        r.gen_cache_put(
            &keys[0],
            "v2 0\nexport fn count() -> Int64 { return 999 }\n",
        );
        assert_eq!(
            run_with(COUNTER_ROOT, &r).unwrap(),
            1,
            "the generator's own answer, not the entry's"
        );
        assert_eq!(gen_run_count(), before + 2, "the refused entry re-ran it");
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
