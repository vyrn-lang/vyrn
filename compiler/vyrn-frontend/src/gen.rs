//! Generation machinery (RFC-0021, RFC-0054, RFC-0076): the splice rule, the
//! `Code` piece list and its renderer, the `lex` token literal, the sandbox's
//! path rule, `moduleInterface`'s reflection, and the seam an alternative
//! generation engine is installed through.
//!
//! It lived in `interp.rs` because that is the file it was written in. Nothing
//! here walks a tree: these take a resolver, a path and a source, and answer
//! with an `Expr` or a `String`. RFC-0125 §3 M5's tenth slice read the thirteen
//! references `vyrn-genwasm` and the loader make into the interpreter and found
//! twelve of them were this module in the wrong file; the thirteenth was
//! `gen_code_splice`, which took a `Val`. It takes [`Spliced`] now — the six
//! cases the rule actually reads, which are the six `vyrn_codegen::TAG_*`
//! enumerates — so the interpreter converts once at the one place it splices
//! and the compiled generation engine names no interpreter type at all.
//!
//! One edge remains: [`generate`] falls through to the tree-walker when no
//! engine is installed. That is the edge the deletion removes rather than moves.

use crate::ast::{Expr, Program};
use std::collections::HashMap;

/// One piece of a [`Val::Code`] fragment (RFC-0054). Either plain rendered text
/// (from a quote skeleton, a splice, or `raw`) or an origin-carrying region (from
/// `rawAt`) that `render` wraps in `//@origin path:line:col` … `//@origin end`.
#[derive(Debug, Clone, PartialEq)]
pub enum CodePiece {
    /// Verbatim rendered source text (no origin attribution).
    Text(String),
    /// A region derived from input at `path:line:col` — `render` brackets it with
    /// the RFC-0033 origin directives so a diagnostic inside it maps back.
    Origin {
        path: String,
        line: i64,
        col: i64,
        text: String,
    },
}

/// Render a `Code` piece list to final source text (RFC-0054 `render`). Origin
/// pieces are bracketed by `//@origin` directives, each on its own line (the
/// directive governs the lines that follow — RFC-0033), so a check/parse error
/// inside the region maps back to its recorded `path:line:col`.
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

/// A value in a code-quote hole, as the splice rule reads it (RFC-0054).
///
/// Six cases, and they are the six `vyrn_codegen::TAG_*` enumerates, because a
/// compiled generator has to name them across the wall. That is not a
/// coincidence to be maintained: the rule reads exactly these and nothing else,
/// so a seventh here would be a tag the emitter cannot send and a seventh there
/// would be a value this rule cannot splice.
///
/// It replaced `interp::Val` in RFC-0125 §3 M5's thirteenth slice. A `Val` is
/// the tree-walker's value — twenty variants, `Rc`, closures, a region depth —
/// and `vyrn-genwasm` was building one out of a tag and one word for no other
/// purpose than to hand it here.
#[derive(Debug, Clone, PartialEq)]
pub enum Spliced {
    Str(String),
    Code(Vec<CodePiece>),
    Bool(bool),
    /// Any integer width, sign-extended to 64 bits by the time it arrives;
    /// `signed` decides which decimal it renders as.
    Int {
        v: i64,
        signed: bool,
    },
    F64(f64),
    F32(f32),
}

impl Spliced {
    /// A short kind name, for splice diagnostics. The wordings are the
    /// interpreter's, so a value with no case here (an Array, a Map) is named by
    /// [`no_splice_rule`] with the same vocabulary.
    fn kind(&self) -> &'static str {
        match self {
            Spliced::Int { .. } | Spliced::F64(_) | Spliced::F32(_) => "a number",
            Spliced::Bool(_) => "a Bool",
            Spliced::Str(_) => "a String",
            Spliced::Code(_) => "Code",
        }
    }
}

/// The refusal for a value the splice rule has no case for, in context `ctx`.
///
/// Public because the CONVERSION is on the interpreter's side — a `Val` that is
/// an Array never becomes a [`Spliced`] — and the wording has to be the same
/// sentence either way.
pub fn no_splice_rule(kind: &str, ctx: i64) -> String {
    if ctx == 0 {
        format!("cannot splice {kind} into a code quote (expected String, number, Bool, or Code)")
    } else {
        format!("cannot splice {kind} in identifier position (only String or Code)")
    }
}

/// Apply the RFC-0054 splice rule for a value in a hole of grammatical context
/// `ctx` (`0` expression, `1` identifier fragment, `2` standalone identifier /
/// type), yielding the code pieces to splice. A `String` is DATA, never code:
/// in expression position it becomes an escaped string *literal*; in an
/// identifier position it is a validated bare-identifier fragment (there is
/// deliberately no way to splice a `String` as code).
///
/// Public, and a free function, because the wasm generation engine (RFC-0076)
/// lowers `Code` to a handle into a host-side arena and applies the splice rule
/// host-side. It must be THIS rule — the escaping, the identifier validation and
/// the shortest-roundtrip float formatting are not things a second
/// implementation would reproduce, they are things it would eventually disagree
/// with.
pub fn gen_code_splice(val: &Spliced, ctx: i64) -> Result<Vec<CodePiece>, String> {
    // A `Code` value splices verbatim in every context (already-validated code).
    if let Spliced::Code(pieces) = val {
        return Ok(pieces.clone());
    }
    let text = |s: String| Ok(vec![CodePiece::Text(s)]);
    match ctx {
        // Expression position.
        0 => match val {
            Spliced::Str(s) => text(escape_string_literal(s)),
            Spliced::Int { v, signed } => text(if *signed {
                v.to_string()
            } else {
                (*v as u64).to_string()
            }),
            // `NaN`/`inf` do not lex as Vyrn numbers (there is no literal for
            // either), so a computed non-finite value fails here, at the
            // boundary — not downstream as a module that cannot parse.
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
        // Identifier fragment: `[A-Za-z0-9_]+`, non-empty (it merges with adjacent
        // word characters, so a leading digit is fine).
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
        // Standalone identifier / type position: a valid, non-keyword identifier.
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

/// Shortest-roundtrip float digits as PLAIN decimal text. Rust's `Display`
/// formatting never uses an exponent (`Debug` switches to scientific notation
/// for magnitudes ≥ 1e16 or < 1e-4, which the Vyrn lexer cannot read), and an
/// integral value gets `.0` appended so the text lexes as a float literal
/// (`digits '.' digits`) rather than an integer.
fn splice_float(digits: String) -> String {
    if digits.contains('.') {
        digits
    } else {
        format!("{digits}.0")
    }
}

/// Escape a `String` value into a Vyrn source string literal, quotes included —
/// the mechanism by which a spliced String is data, never code (RFC-0054). Uses
/// the same escapes the lexer decodes (`\n \t \r \" \\` and `\{` so an emitted
/// literal cannot itself open an interpolation).
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
            // A literal `{` that follows a `\` in the OUTPUT would open a hole; the
            // backslash is already doubled above, so a lone `{` is safe. Emit it
            // verbatim (braces are ordinary characters in Vyrn strings).
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Whether `s` is a single, non-keyword Vyrn identifier — decided by the real
/// lexer, so `fn`/`if`/… (keywords) and `a b`/`1x`/… (non-identifiers) are all
/// rejected in one place (RFC-0054 identifier splice).
fn is_bare_identifier(s: &str) -> bool {
    match crate::lexer::lex(s) {
        Ok(toks) => {
            matches!(toks.first().map(|t| &t.tok), Some(crate::lexer::Tok::Ident(n)) if n == s)
                && toks.len() == 2 // the ident plus EOF
        }
        Err(_) => false,
    }
}

/// The same token list as an `Array<Token>` record *literal* — what the wasm
/// generation engine (RFC-0076 M3b) encodes for the guest.
///
/// Both this and `interp`'s `lex_tokens` read one [`lexed`], so the two engines cannot
/// disagree about which tokens `lex` yields, only about how the value is built.
pub fn gen_lex_tokens_lit(source: &str) -> Expr {
    Expr::ArrayLit {
        elems: lexed(source)
            .into_iter()
            .map(|(kind, text, line, col)| Expr::StructLit {
                name: "Token".to_string(),
                fields: vec![
                    ("kind".to_string(), Expr::Str(kind)),
                    ("text".to_string(), Expr::Str(text)),
                    ("line".to_string(), Expr::Int(line)),
                    ("col".to_string(), Expr::Int(col)),
                ],
                line: 0,
            })
            .collect(),
        line: 0,
    }
}

/// The compiler's real lexer over `source`, as `(kind, text, line, col)` rows —
/// the one place the `lex` builtin's token list is decided.
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
            // Non-fatal: attribute the unlexable input as one `error` token at the
            // diagnostic's position.
            vec![("error".to_string(), d.message, d.line as i64, d.col as i64)]
        }
    }
}

/// One thing a generation read: the resolved key (a file, or a directory with a
/// trailing `/`) and what it held — `None` when the read or the listing FAILED.
///
/// A failure is an observation too. A generator that finds no `examples/`
/// directory emits "0 examples", and that answer stops being right the moment
/// the directory appears. Recording only successes made the entry unable to
/// notice, and the build stayed green with the wrong output.
pub type GenRead = (String, Option<Vec<u8>>);

/// The result of running a generator (RFC-0021): the synthesized module source
/// plus the inputs the generation read, which the loader folds into the
/// content-addressed cache entry.
pub struct GenOutput {
    pub source: String,
    pub reads: Vec<GenRead>,
}

/// Everything a generation run needs from the loader (RFC-0021). Bundled so the
/// [`generate`] signature stays legible.
pub struct GenInputs<'a> {
    pub resolver: &'a dyn crate::loader::ModuleResolver,
    /// The load options (std root + manifest aliases) — needed so `moduleInterface`
    /// can link the reflected module's imports to follow the type closure (RFC-0031).
    pub opts: &'a crate::loader::LoadOptions,
    /// The importing module's directory — the base for relative-path resolution.
    pub importer_dir: String,
    /// Resolved path prefixes the generator may read under (its constant path
    /// args). Empty ⇒ no filesystem access is permitted.
    pub allowed: Vec<String>,
    /// RFC-0107 M2: those constant path arguments that name a manifest
    /// DEPENDENCY, paired with the module key the import map resolves them to.
    /// A mediated read spelling one of them reaches the pinned bytes instead of a
    /// path that exists on no disk.
    ///
    /// This does not widen the sandbox. Every pair comes from the generator's own
    /// constant arguments and its resolved key is one of `allowed`, so the
    /// input-root rule decides exactly as before — an alias is a second SPELLING
    /// of a declared root, not a new root.
    pub aliased: Vec<(String, String)>,
    /// Step budget and output-size cap (guardrails).
    pub fuel: u64,
    pub max_output: usize,
    /// A fingerprint of the generator's own module closure — its keys and the
    /// content hashes the loader hashes them by anyway — or `None` when the
    /// closure contains something no resolver can re-read (a generated module),
    /// so no honest fingerprint exists.
    ///
    /// The interpreter ignores it. It is here for an engine that CACHES a
    /// compiled artifact (RFC-0076 M5): keying on this instead of on the whole
    /// program's `Debug` output turns a 1.1–1.9 ms hash of 4,536 lines into a
    /// string compare, and the loader already computed every part of it.
    pub sources_fingerprint: Option<String>,
}

/// Resolve a mediated path argument against the importer's directory, then
/// enforce that it stays under one of the generator's declared input roots (its
/// constant path args). Returns the resolved resolver key, or the scoping trap
/// message.
///
/// `aliased` is the import-map step (RFC-0107 M2): an argument that names a
/// manifest dependency resolves to the key the LOADER resolved it to, the same
/// key a module specifier of that name would reach. Path arithmetic cannot do
/// this — a dependency's target is a lock-pinned remote key or a path rooted at
/// the MANIFEST's directory, neither of which is `importer_dir/arg` — which is
/// why a `gen fn` could not read a pinned collection file before.
///
/// The input-root rule below still decides, on the resolved key: the loader
/// derives `aliased` from the generator's own constant arguments and puts each
/// resolved key into `allowed`, so an alias adds a SPELLING of a declared root
/// and never a root.
///
/// Public, and a free function, because the wasm generation engine (RFC-0076)
/// mediates its host imports with exactly this rule. Two implementations of a
/// sandbox boundary is one too many.
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

/// `moduleInterface(path)` (RFC-0021): read the referenced module through the
/// resolver, link it to follow its reachable type closure (RFC-0031), and build
/// the `ModuleInterface` record literal for its exported surface. Every module
/// the link touched is appended to `reads`, which is what makes editing a
/// closure type's DEFINING file miss the generator cache even though its path
/// was never a generator argument.
///
/// Public, and a free function, for the same reason [`gen_scoped_path`] is: the
/// wasm generation engine (RFC-0076 M3b) serves its `moduleInterface` import
/// from here. Reflection is compiler machinery — a second implementation would
/// be a second answer, and a second set of recorded reads is a stale cache hit.
pub fn gen_module_interface_lit(
    resolver: &dyn crate::loader::ModuleResolver,
    opts: &crate::loader::LoadOptions,
    importer_dir: &str,
    allowed: &[String],
    aliased: &[(String, String)],
    reads: &mut Vec<GenRead>,
    path: &str,
) -> Result<Expr, String> {
    // Resolve like a module specifier (`.vyrn` appended), scoped like readFile.
    // A manifest dependency is left ALONE: the import map answers for the whole
    // spelling, and its target already carries the extension.
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

    // Follow the reflected module's imports to build the reachable type closure
    // (RFC-0031): link it into one program so a type declared in an imported
    // module is still visible to the closure walk. Every module the link reads is
    // recorded through a proxy resolver, so a closure type's defining FILE joins
    // the generator's cache inputs — editing `types.vyrn` must miss the cache
    // even though its path was never a generator argument.
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
    // Every module the link read, kept as text for the origin index below — the
    // AST has a declaration's line but not its name column, so the columns come
    // back out of the lexer (RFC-0073 M1). These are the same reads, so a module
    // reflected is a module indexed, by construction.
    let mut origin_src: Vec<(Option<String>, String, String)> =
        vec![(None, resolved.clone(), source.clone())];
    for (p, s) in rec.into_reads() {
        // The root module was already recorded above; skip the duplicate.
        if p != resolved {
            origin_src.push((Some(p.clone()), p.clone(), s.clone()));
            reads.push((p, Some(s.into_bytes())));
        }
    }

    // Import specifier per declaring module, so a generator that must SHARE a
    // closure type's identity (rpcServer/rpcInProcess) can import it from the
    // module that declares it (RFC-0031). The reflected module's own types
    // (`module == None`) keep the generator's own argument spelling; a foreign
    // type gets a specifier relative to the real importing file's directory.
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

/// An alternative engine for running a generator (RFC-0076).
///
/// The frontend defines this seam and nothing more: compiling a generator to
/// wasm needs codegen and clang, which only the driver has, so the driver
/// installs an engine and the frontend stays free of external dependencies.
///
/// Returning `None` means "not served" — not an error. Every generator the wasm
/// path cannot yet handle falls through to the interpreter, which stays the
/// reference the alternative is checked against.
pub type GenEngine = dyn Fn(
        &Program,
        &str,
        &[crate::consteval::ConstVal],
        &GenInputs<'_>,
    ) -> Option<Result<GenOutput, String>>
    + Send
    + Sync;

static GEN_ENGINE: std::sync::OnceLock<Box<GenEngine>> = std::sync::OnceLock::new();

/// Install the alternative generation engine. Called once, by the driver, before
/// any load. A second call is ignored rather than racing.
pub fn set_gen_engine(engine: Box<GenEngine>) {
    let _ = GEN_ENGINE.set(engine);
}

/// Run `fn_name` in `program` as a **generation target** (RFC-0021): under the
/// capability-mediated sandbox in `inputs`, with `args` (compile-time constants)
/// as its arguments. Returns the returned `String` (the synthesized module
/// source) plus the recorded input reads, or a trap message.
///
/// Runs on the big-stack interpreter thread like [`run`]. The generator is
/// ordinary Vyrn code — the ONLY differences from a normal call are the mediated
/// `readFile`/`listDir`/`moduleInterface` and the step/size guardrails.
pub fn generate(
    program: &Program,
    fn_name: &str,
    args: &[crate::consteval::ConstVal],
    inputs: GenInputs<'_>,
) -> Result<GenOutput, String> {
    // RFC-0076: an installed engine gets first refusal; `None` falls through.
    if let Some(engine) = GEN_ENGINE.get() {
        if let Some(out) = engine(program, fn_name, args, &inputs) {
            return out;
        }
    }
    crate::interp::generate_interpreted(program, fn_name, args, inputs)
}
