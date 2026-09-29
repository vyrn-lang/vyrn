//! Generation machinery: the splice rule, the
//! `Code` piece list and its renderer, the `lex` token literal, the sandbox's
//! path rule, `moduleInterface`'s reflection, and the seam the driver installs
//! the generation engine through. Nothing here walks a tree: each function
//! takes a resolver, a path or a source, and returns an `Expr` or a `String`.

use crate::ast::{Expr, Id, Program};
use std::collections::HashMap;

/// One piece of a `Code` fragment: plain rendered text (from a
/// quote skeleton, a splice or `raw`), or a region from `rawAt` that `render`
/// wraps in `//@origin path:line:col` ... `//@origin end`.
#[derive(Debug, Clone, PartialEq)]
pub enum CodePiece {
    /// Verbatim source text with no origin.
    Text(String),
    /// A region derived from input at `path:line:col`, bracketed with the
    /// `//@origin` directives so a diagnostic inside it maps back.
    Origin {
        path: String,
        line: i64,
        col: i64,
        text: String,
    },
}

/// Renders a `Code` piece list to source text (`render`). Each origin
/// directive sits on its own line, because it governs the lines that follow.
pub fn render_code(pieces: &[CodePiece]) -> String {
    let mut out = String::new();
    let ensure_nl = |out: &mut String| {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
    };
    for p in pieces {
        match p {
            CodePiece::Text(t) => out.push_str(t),
            CodePiece::Origin {
                path,
                line,
                col,
                text,
            } => {
                ensure_nl(&mut out);
                out.push_str(&format!("//@origin {path}:{line}:{col}\n"));
                out.push_str(text);
                ensure_nl(&mut out);
                out.push_str("//@origin end\n");
            }
        }
    }
    out
}

/// A value in a code-quote hole, as the splice rule reads it.
///
/// The six cases are the six `vyrn_codegen::TAG_*` values, because a compiled
/// generator names them across the wall: a seventh on either side would be a
/// tag the other cannot handle.
#[derive(Debug, Clone, PartialEq)]
pub enum Spliced {
    Str(String),
    Code(Vec<CodePiece>),
    Bool(bool),
    /// Any integer width, sign-extended to 64 bits; `signed` picks the decimal
    /// rendering.
    Int {
        v: i64,
        signed: bool,
    },
    F64(f64),
    F32(f32),
}

impl Spliced {
    /// A short kind name for splice diagnostics, in the vocabulary
    /// [`no_splice_rule`] uses.
    fn kind(&self) -> &'static str {
        match self {
            Spliced::Int { .. } | Spliced::F64(_) | Spliced::F32(_) => "a number",
            Spliced::Bool(_) => "a Bool",
            Spliced::Str(_) => "a String",
            Spliced::Code(_) => "Code",
        }
    }
}

/// Returns the refusal for a value the splice rule has no case for, in context
/// `ctx`. Public because a value with no [`Spliced`] case (an array, a map) is
/// refused by the engine before conversion, in the same words.
pub fn no_splice_rule(kind: &str, ctx: i64) -> String {
    if ctx == 0 {
        format!("cannot splice {kind} into a code quote (expected String, number, Bool, or Code)")
    } else {
        format!("cannot splice {kind} in identifier position (only String or Code)")
    }
}

/// Applies the splice rule to a value in a hole of context `ctx` (`0`
/// expression, `1` identifier fragment, `2` standalone identifier or type).
/// A `String` is data, never code: an escaped string literal in expression
/// position, a validated identifier elsewhere.
///
/// The wasm generation engine applies it host-side, so the
/// escaping, identifier validation and float formatting have one
/// implementation.
pub fn gen_code_splice(val: &Spliced, ctx: i64) -> Result<Vec<CodePiece>, String> {
    // A `Code` value is already valid code and splices verbatim everywhere.
    if let Spliced::Code(pieces) = val {
        return Ok(pieces.clone());
    }
    let text = |s: String| Ok(vec![CodePiece::Text(s)]);
    match ctx {
        0 => match val {
            Spliced::Str(s) => text(escape_string_literal(s)),
            Spliced::Int { v, signed } => text(if *signed {
                v.to_string()
            } else {
                (*v as u64).to_string()
            }),
            // `NaN` and `inf` have no Vyrn literal, so a non-finite value fails here,
            // not later as a module that cannot parse.
            Spliced::F64(f) if f.is_finite() => text(splice_float(format!("{f}"))),
            Spliced::F32(f) if f.is_finite() => text(splice_float(format!("{f}"))),
            Spliced::F64(f) => Err(format!(
                "cannot splice non-finite float {f} into a code quote"
            )),
            Spliced::F32(f) => Err(format!(
                "cannot splice non-finite float {f} into a code quote"
            )),
            Spliced::Bool(b) => text(b.to_string()),
            Spliced::Code(_) => unreachable!("answered above"),
        },
        // Identifier fragment: non-empty `[A-Za-z0-9_]+`. It merges with adjacent
        // word characters, so a leading digit is fine.
        1 => match val {
            Spliced::Str(s) => {
                if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    text(s.to_string())
                } else {
                    Err(format!(
                        "cannot splice {s:?} as an identifier fragment: not `[A-Za-z0-9_]+`"
                    ))
                }
            }
            other => Err(no_splice_rule(other.kind(), ctx)),
        },
        // Standalone identifier or type: a valid non-keyword identifier.
        _ => match val {
            Spliced::Str(s) => {
                if is_bare_identifier(s) {
                    text(s.to_string())
                } else {
                    Err(format!(
                        "cannot splice {s:?} as an identifier: not a valid non-keyword identifier"
                    ))
                }
            }
            other => Err(no_splice_rule(other.kind(), ctx)),
        },
    }
}

/// Returns shortest-roundtrip float digits as plain decimal text. `Display`
/// never uses an exponent, which the lexer cannot read, and an integral value
/// gets `.0` so it lexes as a float.
fn splice_float(digits: String) -> String {
    if digits.contains('.') {
        digits
    } else {
        format!("{digits}.0")
    }
}

/// Escapes a `String` into a Vyrn string literal, quotes included, with the
/// escapes the lexer decodes (`\n \t \r \" \\`).
fn escape_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            // `{` is an ordinary character: a backslash before it is already doubled,
            // so it cannot open a hole.
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Returns whether `s` is one non-keyword Vyrn identifier, decided by the
/// lexer.
fn is_bare_identifier(s: &str) -> bool {
    match crate::lexer::lex(s) {
        Ok(toks) => {
            matches!(toks.first().map(|t| &t.tok), Some(crate::lexer::Tok::Ident(n)) if n == s)
                && toks.len() == 2 // the identifier, then EOF
        }
        Err(_) => false,
    }
}

/// Returns the `lex` token list as an `Array<Token>` record literal, which the
/// wasm generation engine encodes for the guest.
pub fn gen_lex_tokens_lit(source: &str) -> Expr {
    Expr::ArrayLit {
        id: Id::NEW,
        elems: lexed(source)
            .into_iter()
            .map(|(kind, text, line, col)| Expr::StructLit {
                id: Id::NEW,
                name: "Token".to_string(),
                fields: vec![
                    ("kind".to_string(), Expr::Str(kind, Id::NEW)),
                    ("text".to_string(), Expr::Str(text, Id::NEW)),
                    ("line".to_string(), Expr::Int(line, Id::NEW)),
                    ("col".to_string(), Expr::Int(col, Id::NEW)),
                ],
                line: 0,
            })
            .collect(),
        line: 0,
    }
}

/// Runs the lexer over `source` as `(kind, text, line, col)` rows: the one
/// place the `lex` builtin's token list is decided.
pub(crate) fn lexed(source: &str) -> Vec<(String, String, i64, i64)> {
    match crate::lexer::lex(source) {
        Ok(toks) => toks
            .iter()
            .filter(|t| !matches!(t.tok, crate::lexer::Tok::Eof))
            .map(|t| {
                let (kind, text) = crate::lexer::token_name_and_text(&t.tok);
                (kind, text, t.line as i64, t.col as i64)
            })
            .collect(),
        Err(d) => {
            // Unlexable input is one `error` token at the diagnostic's position.
            vec![("error".to_string(), d.message, d.line as i64, d.col as i64)]
        }
    }
}

/// One generation read: the resolved key (a file, or a directory with a
/// trailing `/`) and its content, `None` when the read failed. A failure is
/// recorded too: "0 examples" stops being right when `examples/` appears.
pub type GenRead = (String, Option<Vec<u8>>);

/// A generator's result: the module source, and the inputs it read,
/// which the loader folds into the cache key.
pub struct GenOutput {
    pub source: String,
    pub reads: Vec<GenRead>,
}

/// Everything a generation run needs from the loader.
pub struct GenInputs<'a> {
    pub resolver: &'a dyn crate::loader::ModuleResolver,
    /// The load options (std root, manifest aliases), so `moduleInterface` can
    /// link the reflected module's imports.
    pub opts: &'a crate::loader::LoadOptions,
    /// The importing module's directory: the base for relative paths.
    pub importer_dir: String,
    /// Resolved path prefixes the generator may read under: its constant path
    /// arguments. Empty means no filesystem access.
    pub allowed: Vec<String>,
    /// The constant path arguments that name a manifest dependency, with the
    /// module key the import map resolves each to, so a read reaches
    /// the pinned bytes. Each key is also in `allowed`, so an alias is a second
    /// spelling of a declared root, not a new root.
    pub aliased: Vec<(String, String)>,
    /// Step budget and output-size cap.
    pub fuel: u64,
    pub max_output: usize,
    /// A fingerprint of the generator's module closure (its keys and content
    /// hashes), or `None` when the closure holds a generated module no resolver
    /// can re-read. An engine that caches compiled generators keys on it instead
    /// of hashing the whole program.
    pub sources_fingerprint: Option<String>,
    /// The `TypeArg` literal a derived-code generator's one parameter
    /// receives; `None` for an import target.
    pub type_arg: Option<Expr>,
}

/// Resolves a mediated path argument against the importer's directory and
/// checks that it stays under one of the generator's declared input roots.
/// Returns the resolver key, or the scoping trap message.
///
/// An argument in `aliased` resolves to the key the loader gave
/// that manifest dependency: a lock-pinned remote key or a path under the
/// manifest's directory, which path arithmetic cannot reach. The input-root
/// check still decides on the resolved key. The wasm generation engine
/// mediates its host imports with this same rule.
pub fn gen_scoped_path(
    importer_dir: &str,
    allowed: &[String],
    aliased: &[(String, String)],
    arg: &str,
) -> Result<String, String> {
    let resolved = match aliased.iter().find(|(spelling, _)| spelling == arg) {
        Some((_, key)) => key.clone(),
        None => {
            let joined = if importer_dir.is_empty() {
                arg.to_string()
            } else {
                format!("{importer_dir}/{arg}")
            };
            crate::loader::normalize(&joined)
        }
    };
    let ok = allowed
        .iter()
        .any(|root| resolved == *root || resolved.starts_with(&format!("{root}/")));
    if !ok {
        return Err(format!(
            "generator read `{arg}` escapes its declared inputs ({}) — a generator may only \
             read under its constant path arguments",
            allowed.join(", ")
        ));
    }
    Ok(resolved)
}

/// Serves `moduleInterface(path)`: reads the module, links it to
/// follow its type closure, and builds its `ModuleInterface`
/// literal. Every module the link touched is appended to `reads`, so editing
/// a closure type's defining file misses the generator cache. The wasm
/// generation engine serves its `moduleInterface` import from here.
pub fn gen_module_interface_lit(
    resolver: &dyn crate::loader::ModuleResolver,
    opts: &crate::loader::LoadOptions,
    importer_dir: &str,
    allowed: &[String],
    aliased: &[(String, String)],
    reads: &mut Vec<GenRead>,
    path: &str,
) -> Result<Expr, String> {
    // Resolve like a module specifier (`.vyrn` appended), scoped like
    // `readFile`. A manifest dependency keeps its spelling: its target already
    // carries the extension.
    let spec = if path.ends_with(".vyrn")
        || path.ends_with(".json")
        || aliased.iter().any(|(spelling, _)| spelling == path)
    {
        path.to_string()
    } else {
        format!("{path}.vyrn")
    };
    let resolved = gen_scoped_path(importer_dir, allowed, aliased, &spec)?;
    let source = resolver.read(&resolved).map_err(|e| {
        format!(
            "moduleInterface {}: {e}",
            crate::trap::io_at("readerr", &path)
        )
    })?;
    reads.push((resolved.clone(), Some(source.clone().into_bytes())));

    // Link the module so the closure walk sees types declared in its imports.
    // A recording resolver adds every file the link reads to the cache inputs.
    let rec = crate::loader::RecordingResolver::new(resolver);
    let program = crate::loader::load(&source, &resolved, opts, &rec).map_err(|diags| {
        let d = diags.first();
        let where_ = d
            .and_then(|d| d.file.clone())
            .map(|f| format!(" ({f})"))
            .unwrap_or_default();
        let msg = d
            .map(|d| d.message.clone())
            .unwrap_or_else(|| "load failed".to_string());
        format!("moduleInterface `{path}`{where_}: {msg}")
    })?;
    // The link's reads, kept as text for the origin index: the AST has no name
    // columns, so the lexer supplies them (see `Origins`).
    let mut origin_src: Vec<(Option<String>, String, String)> =
        vec![(None, resolved.clone(), source.clone())];
    for (p, s) in rec.into_reads() {
        // The root module was recorded above.
        if p != resolved {
            origin_src.push((Some(p.clone()), p.clone(), s.clone()));
            reads.push((p, Some(s.into_bytes())));
        }
    }

    // One import specifier per declaring module, so a generator can
    // import a closure type from the module that declares it. The reflected
    // module's own types keep the generator's argument spelling.
    let mut specifiers: HashMap<Option<String>, String> = HashMap::new();
    specifiers.insert(None, path.to_string());
    for t in &program.type_decls {
        if let Some(key) = &t.module {
            specifiers.entry(Some(key.clone())).or_insert_with(|| {
                crate::loader::import_specifier(importer_dir, key, opts.std_root.as_deref())
            });
        }
    }
    let origins = crate::schema_reflect::Origins::new(
        origin_src
            .iter()
            .map(|(k, f, s)| (k.clone(), f.as_str(), s.as_str())),
    );
    Ok(crate::schema_reflect::module_interface_lit(
        &program,
        &specifiers,
        &origins,
    ))
}

/// The generation engine. Compiling a generator to wasm needs
/// codegen, which only the driver has, so the driver installs the engine
/// here. `None` means the engine declined the generator.
pub type GenEngine = dyn Fn(
        &Program,
        &str,
        &[crate::consteval::ConstVal],
        &GenInputs<'_>,
    ) -> Option<Result<GenOutput, String>>
    + Send
    + Sync;

static GEN_ENGINE: std::sync::OnceLock<Box<GenEngine>> = std::sync::OnceLock::new();

/// Identifies the running compiler build: the crate version, then the
/// executable's size and mtime. A persisted generator output or artifact is
/// reused only by the same build.
pub fn compiler_identity() -> String {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let m = std::env::current_exe().and_then(std::fs::metadata);
        let exe = match m {
            Ok(m) => format!(
                "{}:{:?}",
                m.len(),
                m.modified().ok().and_then(|t| t
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_nanos()))
            ),
            // A build that cannot be identified must not reuse outputs across
            // processes.
            Err(_) => format!("unknown-{}", std::process::id()),
        };
        format!("{}:{exe}", env!("CARGO_PKG_VERSION"))
    })
    .clone()
}

/// Installs the generation engine. The driver calls it once, before any load;
/// a second call is ignored.
pub fn set_gen_engine(engine: Box<GenEngine>) {
    let _ = GEN_ENGINE.set(engine);
}

/// Runs `fn_name` in `program` as a generation target under the
/// sandbox in `inputs`, with the compile-time constants `args`. Returns the
/// module source and the recorded reads, or a trap message.
pub fn generate(
    program: &Program,
    fn_name: &str,
    args: &[crate::consteval::ConstVal],
    inputs: GenInputs<'_>,
) -> Result<GenOutput, String> {
    // Without an installed engine no `gen fn` can run, and the error says so.
    let Some(engine) = GEN_ENGINE.get() else {
        return Err(format!(
            "cannot run the generator `{fn_name}`: no generation engine is installed"
        ));
    };
    engine(program, fn_name, args, &inputs).unwrap_or_else(|| {
        Err(format!(
            "cannot run the generator `{fn_name}`: the installed generation engine declined it"
        ))
    })
}

/// The name `derive(g, x)` calls: the entry generator `g` writes for the type
/// of `x`, after [`derive`] prefixes every function `g` wrote.
pub fn derived_name(g: &str, ty: &crate::ast::Type) -> String {
    format!("derive${g}$t{}", crate::types::struct_key(ty))
}

thread_local! {
    /// Each generator run's parsed output, keyed by the generator, its program
    /// and its `TypeArg`, so the editor's re-check of an unchanged program does
    /// not run the generator again.
    static DERIVED: std::cell::RefCell<HashMap<String, Vec<crate::ast::Function>>> =
        std::cell::RefCell::new(HashMap::new());
    /// The generators running, outermost first. A generator's own program may
    /// call `derive` (a `std/ui` generator reaches `toJson`), but not reach
    /// itself.
    static DERIVING: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Runs the generator of every `derive(g, x)` site on the types the checker
/// gave `x`, and returns the functions they wrote, renamed under `derive$g$`.
///
/// `sites` is `(generator, type)` in source order. Each generator runs once per
/// program, over all of its types as one `TypeArg`: a run per type would write a
/// shared subtype's function twice. Its program is `program` cut down to the
/// functions the generator reaches, checked as a generator's own program is.
/// The output is not checked here; the caller checks the program it joins.
pub fn derive(
    program: &Program,
    sites: &[(String, crate::ast::Type)],
) -> Result<Vec<crate::ast::Function>, String> {
    if sites.is_empty() {
        return Ok(Vec::new());
    }
    let types = crate::types::decl_map(program);
    let mut gens: Vec<&str> = Vec::new();
    for (g, _) in sites {
        if !gens.contains(&g.as_str()) {
            gens.push(g);
        }
    }
    let mut out = Vec::new();
    for g in gens {
        let roots: Vec<crate::ast::Type> = sites
            .iter()
            .filter(|(s, _)| s == g)
            .map(|(_, t)| t.clone())
            .collect();
        let (arg, placed) = crate::schema_reflect::type_arg_lit(&roots, &types);
        if let Some(i) = placed.iter().position(Option::is_none) {
            return Err(format!(
                "`derive({g}, ..)` cannot reflect `{}`: it has no wire form or no source spelling",
                roots[i]
            ));
        }
        let gen_program = generator_program(program, g);
        let fingerprint = crate::hash::sha256_hex(canonical(&gen_program).as_bytes());
        let key = crate::hash::sha256_hex(format!("{g}\u{0}{fingerprint}\u{0}{arg:?}").as_bytes());
        let cached = DERIVED.with(|d| d.borrow().get(&key).cloned());
        let fns = match cached {
            Some(fns) => fns,
            None => {
                if DERIVING.with(|d| d.borrow().iter().any(|r| r == g)) {
                    return Err(format!("generator `{g}` reaches `derive({g}, ..)`"));
                }
                DERIVING.with(|d| d.borrow_mut().push(g.to_string()));
                let fns = run_derive(gen_program, g, arg, fingerprint);
                DERIVING.with(|d| d.borrow_mut().pop());
                let fns = fns?;
                DERIVED.with(|d| d.borrow_mut().insert(key, fns.clone()));
                fns
            }
        };
        for ty in &roots {
            let entry = derived_name(g, ty);
            if !fns.iter().any(|f| f.name == entry) {
                return Err(format!(
                    "generator `{g}` wrote no entry for `{ty}`: each root node's `name` must be a function"
                ));
            }
        }
        out.extend(fns);
    }
    Ok(out)
}

/// `program` with the functions and module state `g` does not reach, the tests
/// and the benches removed: what a generator's own load would link.
fn generator_program(program: &Program, g: &str) -> Program {
    let by_name: HashMap<&str, &crate::ast::Function> = program
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect();
    let state: HashMap<&str, &Expr> = program
        .globals
        .iter()
        .map(|s| (s.name.as_str(), &s.init))
        .collect();
    let mut keep: std::collections::HashSet<String> = std::collections::HashSet::new();
    // The injected runtime modules (`$` names) stay whole: the emitter calls
    // into `std/runtime` where no source does. The impls and contracts stay
    // whole, so their flattened methods and what their defaults call stay too.
    let mut work: Vec<String> = vec![g.to_string()];
    work.extend(
        program
            .functions
            .iter()
            .filter(|f| f.name.contains('$') || crate::types::impl_method_member(&f.name).is_some())
            .map(|f| f.name.clone()),
    );
    for d in program
        .contracts
        .iter()
        .flat_map(|c| &c.members)
        .filter_map(|m| m.default())
    {
        work.extend(crate::checker::expr_refs(d));
    }
    for m in program
        .impls
        .iter()
        .flat_map(|i| i.methods.iter().chain(&i.places))
    {
        work.extend(crate::checker::fn_refs(m));
    }
    while let Some(n) = work.pop() {
        if let Some(f) = by_name.get(n.as_str()) {
            if keep.insert(n) {
                work.extend(crate::checker::fn_refs(f));
            }
        } else if let Some(init) = state.get(n.as_str()) {
            if keep.insert(n) {
                work.extend(crate::checker::expr_refs(init));
            }
        }
    }
    let mut p = program.clone();
    p.functions.retain(|f| keep.contains(&f.name));
    p.globals.retain(|s| keep.contains(&s.name));
    p.tests.clear();
    p.benches.clear();
    p
}

/// Returns a text that names `p` alike in every process: its `Debug`, with the
/// AST's three hash containers (`surface_shadows` and each function's and
/// impl's `type_bounds`) moved out and sorted, because a hash container's
/// `Debug` order differs per process.
fn canonical(p: &Program) -> String {
    use std::collections::{BTreeMap, BTreeSet};
    let mut c = p.clone();
    let shadows: BTreeSet<_> = c.surface_shadows.drain().collect();
    let mut bounds: Vec<BTreeMap<String, Vec<String>>> = Vec::new();
    for i in &mut c.impls {
        bounds.push(i.type_bounds.drain().collect());
    }
    let methods = c
        .impls
        .iter_mut()
        .flat_map(|i| i.methods.iter_mut().chain(i.places.iter_mut()));
    for f in c.functions.iter_mut().chain(methods) {
        bounds.push(f.type_bounds.drain().collect());
    }
    format!("{c:?}\u{0}{shadows:?}\u{0}{bounds:?}")
}

/// Checks and runs generator `g` on `arg`, parses what it wrote, and renames
/// each function it defines to `derive$g$<name>`. `fingerprint` names the
/// generator's program, so the engine keeps its compiled module across
/// processes.
fn run_derive(
    mut gen_program: Program,
    g: &str,
    arg: Expr,
    fingerprint: String,
) -> Result<Vec<crate::ast::Function>, String> {
    let diags = crate::floor::aside(|| {
        crate::movecheck::comptime(|| crate::check_and_synthesize(&mut gen_program))
    });
    if let Some(d) = diags.first() {
        return Err(format!("generator `{g}` does not check: {}", d.render()));
    }
    let resolver = crate::loader::MapResolver(HashMap::new());
    let opts = crate::loader::LoadOptions::default();
    let _ = crate::own::typed_refusals();
    let src = crate::movecheck::comptime(|| {
        generate(
            &gen_program,
            g,
            &[],
            GenInputs {
                resolver: &resolver,
                opts: &opts,
                importer_dir: String::new(),
                allowed: Vec::new(),
                aliased: Vec::new(),
                fuel: crate::loader::GEN_FUEL,
                max_output: crate::loader::GEN_MAX_OUTPUT,
                sources_fingerprint: Some(fingerprint),
                type_arg: Some(arg),
            },
        )
    })
    .map_err(|trap| match crate::own::typed_refusals().first() {
        Some(d) => format!("generator `{g}` does not check: {}", d.render()),
        None => trap,
    })?
    .source;
    let tokens = crate::lexer::lex(&src).map_err(|d| {
        format!(
            "generator `{g}` wrote text that does not lex: {}\n{src}",
            d.message
        )
    })?;
    let (mut written, errors) = crate::parser::parse_accum(tokens);
    if let Some(d) = errors.first() {
        return Err(format!(
            "generator `{g}` wrote text that does not parse: {}\n{src}",
            d.message
        ));
    }
    let mut map: HashMap<String, String> = written
        .functions
        .iter()
        .map(|f| (f.name.clone(), format!("derive${g}${}", f.name)))
        .collect();
    let ph = crate::schema_reflect::PH;
    for (i, _) in src.match_indices(ph) {
        let name: String = src[i..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let real = format!("{}{}", crate::loader::RT_PREFIX, &name[ph.len()..]);
        map.insert(name, real);
    }
    // The banner a diagnostic in the written code names as its file.
    let banner = format!("generated by derive({g}, ..)");
    for f in &mut written.functions {
        f.name = map[&f.name].clone();
        f.module = Some(banner.clone());
    }
    crate::loader::rewrite_names(&mut written, &map);
    Ok(written.functions)
}
