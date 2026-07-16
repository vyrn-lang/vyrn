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
//!     or against the std root for `std/...`; `.vela` is appended when the
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
}

/// A resolver over an in-memory map — used by tests and always available.
pub struct MapResolver(pub HashMap<String, String>);

impl ModuleResolver for MapResolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        self.0.get(resolved).cloned().ok_or_else(|| format!("module not found: {resolved}"))
    }
}

/// Lexically normalize a slash-separated path: resolve `.` and `..`, collapse
/// duplicate separators. Purely textual (works for in-memory resolvers too).
fn normalize(path: &str) -> String {
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
    let with_ext = |p: String| {
        if p.ends_with(".vela") || p.ends_with(".json") {
            p
        } else {
            format!("{p}.vela")
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
    // Remote specifiers are their own keys; the resolver (vela-cli) turns them
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
         a remote specifier (github:/gist:/https:), or declare it in vela.json's \
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
/// pair reachable from the root — powers `velac deps`.
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
        // type declarations from it (RFC-0010 M2) instead of parsing Vela.
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
        if !is_root {
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
            // but `velac test <root>` runs only the root's (`None`-module) tests.
            for t in &mut program.tests {
                t.module = Some(key.to_string());
            }
        }

        // Resolve and load imports depth-first.
        let mut import_targets = Vec::new();
        for imp in &program.imports {
            let target = resolve_spec(&imp.path, key, opts).map_err(|e| {
                let mut d = Diagnostic::error(imp.line, 0, "load", e);
                if !is_root {
                    d.file = Some(key.to_string());
                }
                vec![d]
            })?;
            visit(&target, None, opts, resolver, modules, states, stack, root_key)?;
            import_targets.push(target);
        }

        stack.pop();
        states.insert(key.to_string(), true);
        modules.push(Module { key: key.to_string(), program, import_targets });
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
    for m in modules {
        if m.key == root_key {
            merged = Some(m.program);
        } else {
            let p = m.program;
            extra_types.extend(p.type_decls.into_iter().filter(|t| !is_injected(t)));
            extra_fns.extend(p.functions);
            extra_protocols.extend(p.protocols);
            extra_impls.extend(p.impls);
            // Imported tests keep their `module` tag: they type-check but do not
            // run under `velac test <root>` (RFC-0015).
            extra_tests.extend(p.tests);
        }
    }
    let mut program = merged.expect("root module was loaded");
    program.type_decls.extend(extra_types);
    program.functions.extend(extra_fns);
    program.protocols.extend(extra_protocols);
    program.impls.extend(extra_impls);
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
        let program = load(root, "main.vela", &opts(), &map(files))
            .map_err(|ds| ds.iter().map(|d| d.render()).collect::<Vec<_>>().join("\n"))?;
        let diags = crate::checker::check_accum(&program);
        if let Some(d) = diags.first() {
            return Err(d.render());
        }
        crate::interp::run(&program)
    }

    fn load_err(root: &str, files: &[(&str, &str)]) -> String {
        match load(root, "main.vela", &opts(), &map(files)) {
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
        assert_eq!(run_multi(root, &[("lib.vela", lib)]).unwrap(), 42);
    }

    #[test]
    fn rpc_procedure_is_importable_and_dispatches_in_process_across_modules() {
        // A `rpc fn` is implicitly exported (RFC-0019), so another module can
        // import it by name and dispatch it in-process with `rpc()` — the id
        // (deterministic, from 1) is `main`'s return value.
        let lib = "export type Req = { id: Int64 } \
                   rpc fn getUser(req: Req) -> Req { return req }";
        let root = "import { getUser, Req } from \"./lib\" \
                    export extern fn onRpc(id: Int64, status: Int64, body: String) { print(\"cb\") } \
                    fn main() -> Int64 { return rpc(getUser, Req { id: 9 }) }";
        assert_eq!(run_multi(root, &[("lib.vela", lib)]).unwrap(), 1);
    }

    #[test]
    fn validated_type_auto_validates_across_modules() {
        let lib = "export type Age = Int64 where value >= 18";
        let root = "import { Age } from \"./lib\" \
                    fn mk(n: Int64) -> Age { return n } \
                    fn main() -> Int64 { let a = mk(5) return 0 }";
        let e = run_multi(root, &[("lib.vela", lib)]).unwrap_err();
        assert!(e.contains("validation failed for `Age`"), "{e}");
    }

    #[test]
    fn importing_a_private_name_is_an_error() {
        let lib = "fn secret() -> Int64 { return 1 }";
        let root = "import { secret } from \"./lib\" \
                    fn main() -> Int64 { return secret() }";
        let e = load_err(root, &[("lib.vela", lib)]);
        assert!(e.contains("not exported"), "{e}");
    }

    #[test]
    fn importing_a_missing_name_is_an_error() {
        let root = "import { nope } from \"./lib\" \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[("lib.vela", "export fn f() -> Int64 { return 1 }")]);
        assert!(e.contains("does not define `nope`"), "{e}");
    }

    #[test]
    fn using_a_foreign_name_without_importing_it_is_an_error() {
        // `helper` exists (exported, even) in lib, but main never imported it.
        let lib = "export fn helper() -> Int64 { return 1 } \
                   export fn wanted() -> Int64 { return 2 }";
        let root = "import { wanted } from \"./lib\" \
                    fn main() -> Int64 { return wanted() + helper() }";
        let e = load_err(root, &[("lib.vela", lib)]);
        assert!(e.contains("not imported here"), "{e}");
    }

    #[test]
    fn import_cycles_are_errors() {
        let a = "import { b } from \"./b\" export fn a() -> Int64 { return 1 }";
        let b = "import { a } from \"./a\" export fn b() -> Int64 { return 2 }";
        let root = "import { a } from \"./a\" fn main() -> Int64 { return a() }";
        let e = load_err(root, &[("a.vela", a), ("b.vela", b)]);
        assert!(e.contains("import cycle"), "{e}");
    }

    #[test]
    fn cross_module_name_collisions_are_errors() {
        let a = "export fn f() -> Int64 { return 1 }";
        let b = "export fn f() -> Int64 { return 2 }";
        let root = "import { f } from \"./a\" \
                    import { f } from \"./b\" \
                    fn main() -> Int64 { return f() }";
        let e = load_err(root, &[("a.vela", a), ("b.vela", b)]);
        assert!(e.contains("defined in both"), "{e}");
    }

    #[test]
    fn importing_an_enum_brings_its_variants() {
        let lib = "export type Shape = | Circle(Int64) | Dot \
                   export fn area(s: Shape) -> Int64 { \
                       return match s { Circle(r) => 3 * r * r, Dot => 0 } }";
        let root = "import { Shape, area } from \"./lib\" \
                    fn main() -> Int64 { return area(Circle(2)) }";
        assert_eq!(run_multi(root, &[("lib.vela", lib)]).unwrap(), 12);
    }

    #[test]
    fn importing_a_protocol_brings_its_methods() {
        let lib = "export protocol Loud { fn shout(self) -> Int64 } \
                   impl Loud for Int64 { fn shout(self) -> Int64 { return self * 10 } }";
        let root = "import { Loud } from \"./lib\" \
                    fn main() -> Int64 { return 4.shout() }";
        assert_eq!(run_multi(root, &[("lib.vela", lib)]).unwrap(), 40);
    }

    #[test]
    fn std_prefix_resolves_against_the_std_root() {
        let m = "export fn twice(x: Int64) -> Int64 { return x + x }";
        let root = "import { twice } from \"std/math\" \
                    fn main() -> Int64 { return twice(21) }";
        assert_eq!(run_multi(root, &[("std/math.vela", m)]).unwrap(), 42);
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
            run_multi(root, &[("shared.vela", shared), ("a.vela", a), ("b.vela", b)]).unwrap(),
            32
        );
    }

    #[test]
    fn non_root_logging_config_is_an_error() {
        let lib = "logging { level: trace } export fn f() -> Int64 { return 1 }";
        let root = "import { f } from \"./lib\" fn main() -> Int64 { return f() }";
        let e = load_err(root, &[("lib.vela", lib)]);
        assert!(e.contains("only the root module may configure `logging"), "{e}");
    }

    #[test]
    fn non_root_module_state_is_an_error() {
        // RFC-0013: a top-level `let` may only appear in the root module.
        let lib = "let mut count = 0 export fn f() -> Int64 { return count }";
        let root = "import { f } from \"./lib\" fn main() -> Int64 { return f() }";
        let e = load_err(root, &[("lib.vela", lib)]);
        assert!(e.contains("module state is root-only"), "{e}");
    }

    #[test]
    fn global_name_collides_with_a_function() {
        // A global may not share a name with any other top-level declaration.
        let lib = "export fn tally() -> Int64 { return 1 }";
        let root = "import { tally } from \"./lib\" \
                    let tally = 0 \
                    fn main() -> Int64 { return tally }";
        let e = load_err(root, &[("lib.vela", lib)]);
        assert!(e.contains("must be unique"), "{e}");
    }
}

#[cfg(test)]
mod remote_tests {
    use super::tests::{map, opts};
    use super::*;

    fn load_err_at(root: &str, files: &[(&str, &str)]) -> String {
        match load(root, "main.vela", &opts(), &map(files)) {
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
            "main.vela",
            &opts(),
            &map(&[("github:acme/strings@v1/src/pad.vela", lib)]),
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
            "main.vela",
            &opts(),
            &map(&[
                ("github:acme/x@abc/src/a.vela", a),
                ("github:acme/x@abc/src/b.vela", b),
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
        let e = load_err_at(root, &[("github:acme/x@abc/src/a.vela", a)]);
        assert!(e.contains("escapes its remote module's base"), "{e}");
    }

    #[test]
    fn bare_specifiers_inside_remote_modules_are_rejected() {
        let a = "import { x } from \"money\" export fn a() -> Int64 { return 0 }";
        let root = "import { a } from \"gist:demko/abc123/a\" \
                    fn main() -> Int64 { return a() }";
        let mut o = opts();
        o.aliases.insert("money".into(), "./money".into());
        let e = match load(root, "main.vela", &o, &map(&[("gist:demko/abc123/a.vela", a)])) {
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
